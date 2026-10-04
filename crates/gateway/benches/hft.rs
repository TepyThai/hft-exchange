//! Benchmarks for the `gateway` crate. Run with `cargo xtask bench --week N`.

use benchkit::{Histogram, Report, Unit};
use gateway::client::Client;
use gateway::config::Fsync;
use gateway::io::{echo_loop, EchoLoad, EchoOptions, EchoStats, IoBackend, RingBackend};
use gateway::loadgen::{self, LoadConfig};
use gateway::pingpong::{EchoServer, PingClient, RecvMode};
use gateway::session::{Action, Auth, Session, SessionId};
use gateway::SocketTuning;
use gateway::{
    assemble, EngineIdle, ExchangeV2, Gateway, GatewayConfig, JournalBackend, MemJournals,
    RiskGate, V2Config,
};
use risk::{BucketConfig, Limits};
use sim::flow::{FlowConfig, OrderFlow};
use std::time::{Duration, Instant};
use telemetry::{Attribution, Clock};
use types::{AccountId, Event, NewOrder, OrderId, Qty, Side, TimeInForce};
use wire::{encode_frame, FrameDecoder, Logon, Message, FRAME_HEADER_LEN, MAX_MSG_LEN};

const W24_LABEL: &str = "Order in → ack out, loopback, under load";
const W39_LABEL: &str = "Order in → ack, in-process pipeline";

fn main() {
    benchkit::run(&[
        (
            "w13.roundtrip",
            "Client order → ack over loopback TCP",
            bench_roundtrip,
        ),
        (
            "w31.tcp_pingpong",
            "TCP ping-pong, loopback",
            bench_pingpong,
        ),
        ("w24.e2e_latency", W24_LABEL, bench_e2e_latency),
        (
            "w32.batched_io",
            "Messages per wakeup-batched loop",
            bench_batched_io,
        ),
        ("w39.tick_to_ack", W39_LABEL, bench_tick_to_ack),
    ]);
}

/// Latency report in `unit` from a telemetry histogram of nanoseconds
/// (value: p50).
fn latency_report(id: &str, label: &str, unit: Unit, h: &telemetry::Histogram) -> Report {
    let scale = match unit {
        Unit::Us => 1e3,
        _ => 1.0,
    };
    let q = |p: f64| h.percentile(p) as f64 / scale;
    Report {
        id: id.into(),
        label: label.into(),
        unit,
        value: q(0.5),
        p50: Some(q(0.5)),
        p99: Some(q(0.99)),
        p999: Some(q(0.999)),
        max: Some(h.max() as f64 / scale),
        samples: h.count(),
    }
}

/// A directory removed when dropped, also while unwinding from a panic.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(path: std::path::PathBuf) -> TempDir {
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp dir");
        TempDir(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Pre-trade limits that bind like production ones but never reject the
/// benchmark flow; no throttle.
fn bench_gate() -> RiskGate {
    let limits = Limits {
        max_order_qty: Qty(1_000_000),
        max_notional: 1 << 60,
        max_position: Qty(1 << 40),
        ..Limits::UNLIMITED
    };
    RiskGate::new(limits, BucketConfig::UNLIMITED, BucketConfig::UNLIMITED)
}

/// Exchange v2 under a moderate open-loop load over loopback TCP: 2
/// client sessions (busy-polling generator threads) offering 50 000
/// commands/s of the w23-style flow (about two thirds new orders, the rest
/// cancels; some IOC and market orders), 20 000 warm-up then 100 000
/// measured commands. The exchange runs `RiskGate` (throttles off), file
/// journals written every batch without fsync (`Fsync::Never`: durable
/// against a process crash) on tmpfs (`/dev/shm`) where there is one — the
/// write syscalls stay, the device's writeback stalls do not (with fsync
/// on, this would measure the disk: `loadtest --fsync always` does) — a
/// busy-polling engine thread and 2 Tokio workers. Value: client-measured
/// intended send → first response of every measured new order. Prints the
/// load summary and the server's stage attribution of the same orders.
fn bench_e2e_latency() -> Report {
    const RATE: u64 = 50_000;
    let shm = std::path::Path::new("/dev/shm");
    let root = if shm.is_dir() {
        shm.to_path_buf()
    } else {
        std::env::temp_dir()
    };
    let dir = TempDir::new(root.join(format!("hft-bench-w24-{}", std::process::id())));
    let config = V2Config {
        gateway: GatewayConfig {
            engine_idle: EngineIdle::BusyPoll,
            io_threads: 2,
            cancel_on_disconnect: true,
            ..GatewayConfig::default()
        },
        journal: JournalBackend::File(dir.0.join("journal.bin")),
        fsync: Fsync::Never,
        stage_samples: 1 << 18,
        ..V2Config::in_memory(GatewayConfig::default(), MemJournals::new())
    };
    let exchange = ExchangeV2::start(config, bench_gate()).expect("exchange v2 starts");
    let load = LoadConfig {
        warmup: 20_000,
        ..LoadConfig::new(RATE, 100_000)
    };
    let r = loadgen::run(exchange.local_addr(), &load).expect("load run");
    eprintln!("{}", r.summary());
    eprintln!(
        "achieved {:.0} orders/s; busy rejects {}",
        r.achieved_rate(),
        exchange.stats().busy_rejects
    );
    let samples = exchange.stage_log().samples_since(r.measure_start_ns);
    eprint!(
        "server stages, ns:\n{}",
        gateway::exchange::summarize(&samples)
    );
    let report = exchange.shutdown(Duration::from_secs(10));
    drop(dir);
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(r.unanswered, 0, "{}", r.summary());
    latency_report("w24.e2e_latency", W24_LABEL, Unit::Us, &r.e2e)
}

/// Exchange v2's order path in process, one order per batch, no sockets:
/// the session decodes the frame, the engine thread's sequencer runs
/// `RiskGate` (limits on, throttles off), appends to the journal (in
/// memory), matches, updates the L2 publisher, commits (an in-memory
/// "fsync") and publishes to the rings. 200 000 commands of the w23-style
/// flow after 20 000 warm-up ones; every new order is stamped on the TSC
/// clock (Ingress before the decode, Ack at release). Value: Ingress → Ack
/// of the new orders. Prints the attribution table.
fn bench_tick_to_ack() -> Report {
    const WARMUP: u64 = 20_000;
    const COMMANDS: u64 = 200_000;
    let config = V2Config {
        fsync: Fsync::Always,
        stage_samples: COMMANDS as usize,
        ..V2Config::in_memory(GatewayConfig::default(), MemJournals::new())
    };
    let mut x = assemble(&config, bench_gate()).expect("assemble");
    let sid = SessionId(1);
    let mut session = Session::new(sid);
    let auth = Auth::Open;
    let mut frame = vec![0u8; FRAME_HEADER_LEN + MAX_MSG_LEN];
    let mut decoder = FrameDecoder::default();
    let logon = Message::Logon(Logon {
        account: AccountId(1),
        token: 0,
    });
    let n = encode_frame(&logon, &mut frame);
    let mut input = &frame[..n];
    let payload = decoder.next_frame(&mut input).unwrap().unwrap();
    assert!(matches!(session.on_frame(payload, &auth), Action::Logon(_)));
    x.sequencer.open(sid, AccountId(1), &mut |_, _| {});

    let clock = Clock::global();
    let mut flow = OrderFlow::new(FlowConfig::default(), 39);
    let mut reports: Vec<Event> = Vec::with_capacity(64);
    for i in 0..WARMUP + COMMANDS {
        for ev in reports.drain(..) {
            flow.on_event(&ev);
        }
        if i == WARMUP {
            x.log.clear();
        }
        let n = encode_frame(&Message::from(flow.next_command()), &mut frame);
        let rx = clock.now();
        let mut input = &frame[..n];
        let payload = decoder.next_frame(&mut input).unwrap().unwrap();
        let Action::Forward(cmd) = session.on_frame(payload, &auth) else {
            panic!("the session forwards orders")
        };
        x.sequencer.command(sid, rx, &cmd, &mut |_, m| {
            if let Message::Event(e) = m {
                reports.push(e);
            }
        });
        x.sequencer.flush();
    }
    let attribution = Attribution::from_samples(&x.log.samples());
    eprint!("{attribution}");
    latency_report(
        "w39.tick_to_ack",
        W39_LABEL,
        Unit::Ns,
        attribution.end_to_end(),
    )
}

/// One logged-on client sends an IOC order into an empty book and waits
/// for its `Accepted` (timed), then reads the `Expired` that follows
/// (untimed), 20 000 times after 2 000 warm-up round trips. The engine
/// thread busy-polls; one Tokio worker serves the session.
fn bench_roundtrip() -> Report {
    const WARMUP: u64 = 2_000;
    const ROUNDS: u64 = 20_000;
    let gw = Gateway::start(GatewayConfig {
        io_threads: 1,
        engine_idle: EngineIdle::BusyPoll,
        ..GatewayConfig::default()
    })
    .expect("gateway starts");
    let mut client = Client::connect(gw.local_addr()).expect("connect");
    client.logon(1, 0).expect("logon");
    let mut hist = Histogram::new();
    for i in 0..WARMUP + ROUNDS {
        let order = NewOrder::limit(i + 1, Side::Buy, 100, 1).with_tif(TimeInForce::Ioc);
        let t = Instant::now();
        client.send_order(&order).expect("send");
        let ack = client.recv_event().expect("ack");
        let rtt = t.elapsed();
        assert_eq!(ack, Event::Accepted { id: OrderId(i + 1) });
        assert!(matches!(
            client.recv_event().expect("expired"),
            Event::Expired { .. }
        ));
        if i >= WARMUP {
            hist.record(rtt.as_nanos() as u64);
        }
    }
    drop(client);
    gw.shutdown(Duration::from_secs(5));
    hist.report(
        "w13.roundtrip",
        "Client order → ack over loopback TCP",
        Unit::Us,
    )
}

fn pingpong_hist(server: EchoServer, mode: RecvMode, rounds: u64) -> Histogram {
    let mut client =
        PingClient::connect(server.addr(), mode, SocketTuning::latency()).expect("connect");
    let payload = [7u8; 40];
    let mut echo = Vec::new();
    let mut hist = Histogram::new();
    for i in 0..rounds + rounds / 10 {
        let t = Instant::now();
        client.ping(&payload, &mut echo).expect("ping");
        let rtt = t.elapsed();
        if i >= rounds / 10 {
            hist.record(rtt.as_nanos() as u64);
        }
    }
    drop(client);
    server.join().expect("echo server");
    hist
}

/// A 40-byte frame echoed at the framing layer over loopback TCP, 20 000
/// round trips after 2 000 warm-up ones, with the gateway's socket tuning
/// (`TCP_NODELAY`, modest buffers). The reported value is the plain-thread
/// server with blocking receives on both ends (std, faster than the Tokio
/// server on the reference machine); the busy-polling and Tokio variants
/// are printed for comparison.
fn bench_pingpong() -> Report {
    const ROUNDS: u64 = 20_000;
    let t = SocketTuning::latency();
    let reported = pingpong_hist(
        EchoServer::spawn(RecvMode::Blocking, t, None).expect("server"),
        RecvMode::Blocking,
        ROUNDS,
    );
    let others = [
        ("std busy-poll", RecvMode::BusyPoll, false),
        ("tokio server, blocking client", RecvMode::Blocking, true),
    ];
    let us = |h: &Histogram, q: f64| h.quantile(q) as f64 / 1e3;
    eprintln!(
        "std blocking (reported): p50 {:.1} us, p99 {:.1} us, p99.9 {:.1} us",
        us(&reported, 0.5),
        us(&reported, 0.99),
        us(&reported, 0.999)
    );
    for (name, mode, tokio) in others {
        let server = if tokio {
            EchoServer::spawn_tokio(t, None)
        } else {
            EchoServer::spawn(mode, t, None)
        };
        let h = pingpong_hist(server.expect("server"), mode, ROUNDS);
        eprintln!(
            "{name}: p50 {:.1} us, p99 {:.1} us, p99.9 {:.1} us",
            us(&h, 0.5),
            us(&h, 0.99),
            us(&h, 0.999)
        );
    }
    reported.report("w31.tcp_pingpong", "TCP ping-pong, loopback", Unit::Us)
}

fn echo_run(io: &mut dyn IoBackend, load: EchoLoad) -> EchoStats {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let clients = load
        .start(listener.local_addr().expect("addr"))
        .expect("clients");
    let conns: Vec<_> = (0..load.clients)
        .map(|_| {
            io.register(listener.accept().expect("accept").0)
                .expect("register")
        })
        .collect();
    let stats = echo_loop(io, &conns, &EchoOptions::default()).expect("echo loop");
    for c in clients {
        assert_eq!(
            c.join().expect("client thread").expect("client"),
            load.frames
        );
    }
    stats
}

fn readiness_backend() -> Box<dyn IoBackend> {
    #[cfg(target_os = "linux")]
    {
        Box::new(gateway::io::EpollBackend::with_poller(
            gateway::io::EpollPoller::new().expect("epoll"),
            1024,
            "epoll",
        ))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Box::new(gateway::io::PollBackend::scan(1024))
    }
}

/// 8 client connections, each keeping 64 frames (1–32 byte payloads) in
/// flight, echoed by one event loop; the value is the ring backend's
/// messages per second (median of 3 runs). The readiness (epoll) backend
/// and a low-load run (one frame in flight per client) are printed for
/// comparison, with messages per wakeup.
fn bench_batched_io() -> Report {
    let high = EchoLoad {
        clients: 8,
        window: 64,
        frames: 50_000,
        max_payload: 32,
        seed: 32,
    };
    let low = EchoLoad {
        window: 1,
        frames: 5_000,
        ..high
    };
    let mut ring_rates = Vec::new();
    type Make = fn() -> Box<dyn IoBackend>;
    let backends: [(&str, Make); 2] = [
        ("ring", || Box::new(RingBackend::new(1024).expect("ring"))),
        ("epoll", readiness_backend),
    ];
    for (name, make) in backends {
        let mut rates = Vec::new();
        let mut per_wakeup = Vec::new();
        for _ in 0..3 {
            let s = echo_run(make().as_mut(), high);
            rates.push(s.msgs_per_sec());
            per_wakeup.push(s.per_wakeup());
        }
        rates.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let lo = echo_run(make().as_mut(), low);
        eprintln!(
            "{name:>5}: high load {:.2}M msgs/s ({:.1} msgs/wakeup) | low load {:.2}M msgs/s ({:.2} msgs/wakeup, {} caller syscalls for {} msgs)",
            rates[1] / 1e6,
            per_wakeup[1],
            lo.msgs_per_sec() / 1e6,
            lo.per_wakeup(),
            lo.io.syscalls,
            lo.messages
        );
        if name == "ring" {
            ring_rates = rates;
        }
    }
    Report {
        id: "w32.batched_io".into(),
        label: "Messages per wakeup-batched loop".into(),
        unit: Unit::MsgsPerSec,
        value: ring_rates[1],
        p50: None,
        p99: None,
        p999: None,
        max: None,
        samples: ring_rates.len() as u64,
    }
}
