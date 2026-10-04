//! Week 24: an open-loop load generator over TCP — provided infrastructure
//! for the week's load test and benchmark, not part of the task.
//!
//! `sessions` client connections, each its own trader population
//! (`sim::OrderFlow`, the w23-style synthetic flow: passive and aggressive
//! limits, IOC and market orders, cancels of its own resting orders). The
//! commands follow one fixed schedule ([`telemetry::OpenLoop`]): command
//! `i` is due at `start + i / rate`, round-robin over the sessions, whether
//! or not earlier ones were answered. Every new order's latency is measured
//! from its **intended** send time to its first response (`Accepted` or
//! `Rejected`), so a stall is charged to every order that should have gone
//! out during it — no coordinated omission (week 25 makes this the whole
//! story). Busy rejects ([`BUSY`]) are counted and timed on their own.
//!
//! One thread per session, busy-polling: it writes every command that is
//! due (a late thread catches up in one write) and, in between, drains its
//! non-blocking socket. Blocking reads would add the generator's own
//! wake-up latency — on a loaded host, milliseconds — to every sample.
//! Each session therefore needs a core of its own, away from the
//! exchange's; [`LoadResult::send_lag`] shows how far the sends fell
//! behind the schedule (that part of `e2e` is the generator's).

use crate::client::Client;
use crate::server::BUSY;
use sim::flow::{FlowConfig, OrderFlow};
use std::io::{self, Write};
use std::net::SocketAddr;
use std::time::Duration;
use telemetry::{Clock, Histogram, OpenLoop};
use types::{Command, Event};
use wire::{encode_frame, Message, FRAME_HEADER_LEN, MAX_MSG_LEN};

/// One load run.
#[derive(Clone, Copy, Debug)]
pub struct LoadConfig {
    /// Client sessions (each its own connection, account, flow and
    /// busy-polling thread).
    pub sessions: usize,
    /// Offered rate: commands per second, all sessions together.
    pub rate: u64,
    /// Commands sent on schedule before measuring starts.
    pub warmup: u64,
    /// Commands measured (new orders among them are timed).
    pub commands: u64,
    /// The flow's shape. Session `k` trades for account
    /// `first_account + k` with seed `seed + k`; orders go to
    /// `flow.spec.id`.
    pub flow: FlowConfig,
    /// Seed of session 0's flow.
    pub seed: u64,
    /// Account of session 0.
    pub first_account: u32,
    /// Logon token of every session.
    pub token: u64,
    /// How long to wait for the last responses after the last send.
    pub drain_timeout: Duration,
}

impl LoadConfig {
    /// `commands` measured commands at `rate` per second over 2 sessions,
    /// after `commands / 5` warm-up ones; the default flow (instrument 1,
    /// tick 1, lot 1, fair value 10 000), accounts from 1, token 0.
    pub fn new(rate: u64, commands: u64) -> LoadConfig {
        LoadConfig {
            sessions: 2,
            rate,
            warmup: commands / 5,
            commands,
            flow: FlowConfig::default(),
            seed: 24,
            first_account: 1,
            token: 0,
            drain_timeout: Duration::from_secs(10),
        }
    }
}

/// What a load run measured (warm-up excluded).
#[derive(Clone, Debug)]
pub struct LoadResult {
    /// Offered rate (commands per second).
    pub rate: u64,
    /// Sessions.
    pub sessions: usize,
    /// Measured commands sent.
    pub sent: u64,
    /// Measured new orders sent.
    pub orders: u64,
    /// … answered `Accepted`.
    pub accepted: u64,
    /// … answered with a busy reject.
    pub busy: u64,
    /// … rejected otherwise (risk, validation).
    pub rejected: u64,
    /// … never answered before the drain timeout.
    pub unanswered: u64,
    /// Intended send → first response (`Accepted` or a non-busy reject),
    /// ns: the client's end-to-end latency.
    pub e2e: Histogram,
    /// Intended send → busy reject, ns.
    pub busy_latency: Histogram,
    /// Lag of each measured command's write behind its schedule, ns: the
    /// part of `e2e` the generator itself is responsible for (large values
    /// mean it could not keep up, or was descheduled).
    pub send_lag: Histogram,
    /// Intended send time of the first measured command, on
    /// `Clock::global()` — the exchange's stage stamps from this point on
    /// belong to the measured phase.
    pub measure_start_ns: u64,
    /// From `measure_start_ns` to the last response, ns.
    pub elapsed_ns: u64,
}

impl LoadResult {
    /// Responses to measured orders per second.
    pub fn achieved_rate(&self) -> f64 {
        let answered = self.accepted + self.busy + self.rejected;
        if self.elapsed_ns == 0 {
            0.0
        } else {
            answered as f64 * 1e9 / self.elapsed_ns as f64
        }
    }

    /// One line: rate, counts, end-to-end p50/p99/p99.9/max and the send
    /// lag, in µs.
    pub fn summary(&self) -> String {
        let us = |h: &Histogram, q: f64| h.percentile(q) as f64 / 1e3;
        format!(
            "offered {} cmd/s ({} sessions): {} orders, {} accepted, {} busy, {} rejected, \
             {} unanswered | e2e µs p50 {:.1} p99 {:.1} p99.9 {:.1} max {:.1} | send lag µs \
             p99 {:.1} max {:.1}",
            self.rate,
            self.sessions,
            self.orders,
            self.accepted,
            self.busy,
            self.rejected,
            self.unanswered,
            us(&self.e2e, 0.5),
            us(&self.e2e, 0.99),
            us(&self.e2e, 0.999),
            self.e2e.max() as f64 / 1e3,
            us(&self.send_lag, 0.99),
            self.send_lag.max() as f64 / 1e3,
        )
    }
}

/// Runs `cfg` against the exchange at `addr`: the sessions log on, the
/// schedule starts 2 ms later. Returns once every order is answered (or
/// the drain timeout passed); the sessions are then closed. Spawns one
/// thread per session, each spinning on a core until done.
pub fn run(addr: SocketAddr, cfg: &LoadConfig) -> io::Result<LoadResult> {
    let sessions = cfg.sessions.max(1);
    let total = cfg.warmup + cfg.commands;
    let schedule = OpenLoop::at_rate(cfg.rate.max(1), cfg.commands).with_warmup(cfg.warmup);
    let clock = *Clock::global();
    let mut clients = Vec::with_capacity(sessions);
    for k in 0..sessions {
        let mut c = Client::connect(addr)?;
        c.logon(cfg.first_account + k as u32, cfg.token)?;
        c.stream().set_nonblocking(true)?;
        clients.push(c);
    }
    let start = clock.now() + 2_000_000;
    let run = Run {
        schedule,
        start,
        total,
        warmup: cfg.warmup,
        sessions: sessions as u64,
        clock,
        drain_ns: cfg.drain_timeout.as_nanos() as u64,
    };
    let handles = clients
        .into_iter()
        .enumerate()
        .map(|(k, client)| {
            let flow = OrderFlow::new(
                FlowConfig {
                    account: cfg.first_account + k as u32,
                    first_id: 1,
                    ..cfg.flow
                },
                cfg.seed.wrapping_add(k as u64),
            );
            let instrument = cfg.flow.spec.id;
            std::thread::Builder::new()
                .name(format!("load-{k}"))
                .spawn(move || run.session(k as u64, client, flow, instrument))
        })
        .collect::<io::Result<Vec<_>>>()?;

    let measure_start = schedule.intended_start(start, cfg.warmup);
    let mut result = LoadResult {
        rate: cfg.rate,
        sessions,
        sent: 0,
        orders: 0,
        accepted: 0,
        busy: 0,
        rejected: 0,
        unanswered: 0,
        e2e: Histogram::new(),
        busy_latency: Histogram::new(),
        send_lag: Histogram::new(),
        measure_start_ns: measure_start,
        elapsed_ns: 0,
    };
    let mut last = measure_start;
    let mut first_error = None;
    for h in handles {
        match h.join() {
            Ok(Ok(s)) => {
                result.sent += s.sent;
                result.orders += s.orders;
                result.accepted += s.accepted;
                result.busy += s.busy;
                result.rejected += s.rejected;
                result.e2e.merge(&s.e2e);
                result.busy_latency.merge(&s.busy_latency);
                result.send_lag.merge(&s.send_lag);
                last = last.max(s.last_ns);
            }
            Ok(Err(e)) => {
                first_error.get_or_insert(e);
            }
            Err(_) => {
                first_error.get_or_insert(io::Error::other("load session panicked"));
            }
        }
    }
    if let Some(e) = first_error {
        return Err(e);
    }
    result.unanswered = result
        .orders
        .saturating_sub(result.accepted + result.busy + result.rejected);
    result.elapsed_ns = last.saturating_sub(measure_start);
    Ok(result)
}

/// The shared schedule of one run.
#[derive(Clone, Copy)]
struct Run {
    schedule: OpenLoop,
    start: u64,
    total: u64,
    warmup: u64,
    sessions: u64,
    clock: Clock,
    drain_ns: u64,
}

/// One session's measurements.
struct SessionResult {
    sent: u64,
    orders: u64,
    accepted: u64,
    busy: u64,
    rejected: u64,
    e2e: Histogram,
    busy_latency: Histogram,
    send_lag: Histogram,
    last_ns: u64,
}

/// Top bit of an intended time: the order is in the measured phase.
const MEASURED: u64 = 1 << 63;

#[derive(Clone, Copy)]
enum Answer {
    Accepted,
    Busy,
    Rejected,
}

impl Run {
    /// Session `k` sends commands `k, k + n, k + 2n, …` on schedule; its new
    /// orders get client ids 1, 2, 3, … Between sends it drains its socket.
    fn session(
        self,
        k: u64,
        mut client: Client,
        mut flow: OrderFlow,
        instrument: types::InstrumentId,
    ) -> io::Result<SessionResult> {
        let clock = self.clock;
        let mine = (self.total + (self.sessions - 1 - k)) / self.sessions;
        // By client order id: intended send time (| MEASURED) until the
        // first response arrives, then 0.
        let mut intended = vec![0u64; mine as usize + 1];
        let mut writer = client.stream().try_clone()?;
        let mut r = SessionResult {
            sent: 0,
            orders: 0,
            accepted: 0,
            busy: 0,
            rejected: 0,
            e2e: Histogram::new(),
            busy_latency: Histogram::new(),
            send_lag: Histogram::new(),
            last_ns: 0,
        };
        let mut buf = Vec::with_capacity(64 * (FRAME_HEADER_LEN + MAX_MSG_LEN));
        let mut due_measured = Vec::with_capacity(64);
        let (mut next, mut issued, mut answered) = (k, 0u64, 0u64);
        let mut done_at = None;
        loop {
            // 1. Send everything that is due, in one write.
            let now = clock.now();
            if next < self.total && now >= self.schedule.intended_start(self.start, next) {
                buf.clear();
                due_measured.clear();
                let mut batch = 0;
                while next < self.total
                    && self.schedule.intended_start(self.start, next) <= now
                    && batch < 64
                {
                    let due = self.schedule.intended_start(self.start, next);
                    let measured = next >= self.warmup;
                    let mut cmd = flow.next_command();
                    if let Command::New(o) = &mut cmd {
                        o.instrument = instrument;
                        intended[o.id.0 as usize] = due | if measured { MEASURED } else { 0 };
                        issued += 1;
                        r.orders += measured as u64;
                    }
                    if measured {
                        r.sent += 1;
                        due_measured.push(due);
                    }
                    let at = buf.len();
                    buf.resize(at + FRAME_HEADER_LEN + MAX_MSG_LEN, 0);
                    let n = encode_frame(&Message::from(cmd), &mut buf[at..]);
                    buf.truncate(at + n);
                    next += self.sessions;
                    batch += 1;
                }
                let sent_at = clock.now();
                for &d in &due_measured {
                    r.send_lag.record(sent_at.saturating_sub(d));
                }
                write_all_spinning(&mut writer, &buf)?;
            }

            // 2. Drain what has arrived.
            loop {
                let ev = match client.recv() {
                    Ok(Some(Message::Event(ev))) => ev,
                    Ok(Some(_)) => continue,
                    Ok(None) => return Ok(r),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                };
                let now = clock.now();
                let (id, answer) = match ev {
                    Event::Accepted { id } => (id, Answer::Accepted),
                    Event::Rejected { id, reason } if reason == BUSY => (id, Answer::Busy),
                    Event::Rejected { id, .. } => (id, Answer::Rejected),
                    _ => {
                        flow.on_event(&ev);
                        continue;
                    }
                };
                // The first response to a new order resolves it; a reject
                // of an id already answered is a cancel's.
                let Some(slot) = intended.get_mut(id.0 as usize) else {
                    continue;
                };
                let t = std::mem::take(slot);
                if t == 0 {
                    continue;
                }
                answered += 1;
                if t & MEASURED == 0 {
                    continue;
                }
                let latency = now.saturating_sub(t & !MEASURED);
                match answer {
                    Answer::Accepted => {
                        r.accepted += 1;
                        r.e2e.record(latency);
                    }
                    Answer::Rejected => {
                        r.rejected += 1;
                        r.e2e.record(latency);
                    }
                    Answer::Busy => {
                        r.busy += 1;
                        r.busy_latency.record(latency);
                    }
                }
                r.last_ns = r.last_ns.max(now);
            }

            // 3. Done once everything sent is answered (or drain ran out).
            if next >= self.total {
                let t = *done_at.get_or_insert_with(|| clock.now());
                if answered >= issued || clock.now().saturating_sub(t) > self.drain_ns {
                    return Ok(r);
                }
            }
            std::hint::spin_loop();
        }
    }
}

/// `write_all` on a non-blocking socket: spins while the send buffer is
/// full (the exchange is not reading fast enough — that is its latency).
fn write_all_spinning(w: &mut std::net::TcpStream, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        match w.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                std::hint::spin_loop()
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
