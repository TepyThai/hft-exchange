//! Week 13: the TCP gateway — Tokio sessions in front of a dedicated engine
//! thread.
//!
//! ```text
//!            Tokio runtime (io_threads workers)                engine thread (std::thread)
//!  socket ─▶ reader task: FrameDecoder → Session ─┐
//!  socket ─▶ reader task ...                      ├─▶ sync::mpsc (w05) ─▶ Sequencer ─┐
//!  socket ─▶ reader task ...                     ─┘                     (engines,    │
//!                                                                        router, risk)│
//!  socket ◀─ writer task ◀─ per-session bounded queue ◀── batch outbox ◀─────────────┘
//! ```
//!
//! * **Readers** own the read half, the [`wire::FrameDecoder`] and the
//!   [`Session`]. Logon, commands and the final disconnect go to the engine
//!   thread through one bounded MPSC queue; when it is full the reader
//!   yields to the runtime (back-pressure), it never blocks a worker.
//! * **The engine thread** busy-polls the queue (see [`EngineIdle`]) and
//!   runs the [`Sequencer`]. It owns the session registry (session id →
//!   outbound queue) and pushes reports with a non-blocking `try_send`: a
//!   session whose queue is full is a slow consumer and is disconnected —
//!   the engine never waits for a socket.
//! * **Writers** own the write half, drain everything queued for their
//!   session and write it with one `write` (or `writev`, week 31).
//!
//! **Shutdown** (in this order, so no accepted order loses its reports):
//! stop accepting and reading, wait for every reader to exit (each tells
//! the engine thread its session closed), let the engine thread drain its
//! queue and exit — which drops every outbound queue — then let the writers
//! flush what is left and close their sockets with a FIN.

use crate::hooks::Hooks;
use crate::net::SocketTuning;
#[allow(unused_imports)] // used by the implementation the learner writes
use crate::sequencer::Sequencer;
#[allow(unused_imports)] // used by the implementation the learner writes
use crate::session::{Action, Auth, CloseReason, Session, SessionId, SessionState};
use engine::EngineConfig;
use std::net::SocketAddr;
use std::time::Duration;
use types::InstrumentSpec;

#[allow(unused_imports)] // used by the implementation the learner writes
use {
    std::collections::HashMap,
    std::sync::atomic::{AtomicBool, AtomicU64, Ordering},
    std::sync::{Arc, Mutex},
    std::time::Instant,
    sync::{mpsc, CancelToken},
    tokio::io::{AsyncReadExt, AsyncWriteExt},
    tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf},
    tokio::net::{TcpListener, TcpStream},
    tokio::sync::{mpsc as tokio_mpsc, Notify},
    tokio::task::JoinSet,
    types::{AccountId, Command},
    wire::{encode_frame, FrameDecoder, Message, FRAME_HEADER_LEN, MAX_MSG_LEN},
};

/// How the engine thread waits when its inbound queue is empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineIdle {
    /// Spin forever (a `pause` hint per poll, `yield_now` every 1024
    /// polls). Lowest latency; burns a core. Production and benchmarks.
    BusyPoll,
    /// Spin for `spin` empty polls, then sleep `sleep` between polls until
    /// work arrives. An idle gateway costs almost no CPU; the first order
    /// after a quiet period waits up to `sleep` (plus timer slack).
    Backoff {
        /// Empty polls before sleeping (~1 ms per 20 000 on a modern core).
        spin: u32,
        /// Sleep between polls once idle.
        sleep: Duration,
    },
}

impl Default for EngineIdle {
    /// `Backoff { spin: 20_000, sleep: 50µs }`.
    fn default() -> Self {
        EngineIdle::Backoff {
            spin: 20_000,
            sleep: Duration::from_micros(50),
        }
    }
}

/// Everything the gateway needs to start. `GatewayConfig::default()` listens
/// on an ephemeral loopback port with one instrument (id 1, tick 1, lot 1).
#[derive(Clone, Debug)]
pub struct GatewayConfig {
    /// Address to listen on (`127.0.0.1:0` picks a free port; see
    /// [`Gateway::local_addr`]).
    pub listen: SocketAddr,
    /// One engine per instrument.
    pub instruments: Vec<EngineConfig>,
    /// Accepted logons.
    pub auth: Auth,
    /// Tokio worker threads for the sessions. Leave a core free for the
    /// engine thread.
    pub io_threads: usize,
    /// Capacity of the sessions → engine queue (rounded up to a power of
    /// two).
    pub inbound_capacity: usize,
    /// Reports queued per session before it counts as a slow consumer.
    pub outbound_capacity: usize,
    /// Bytes per session read buffer.
    pub read_buffer: usize,
    /// A connection that has not logged on by then is closed.
    pub logon_timeout: Duration,
    /// Cancel a session's resting orders when it disconnects (sequenced
    /// through the engine like any cancel).
    pub cancel_on_disconnect: bool,
    /// Engine thread wait strategy.
    pub engine_idle: EngineIdle,
    /// Pin the engine thread to this CPU (uses week 30's
    /// `sync::pin_current_thread`).
    pub engine_cpu: Option<usize>,
    /// Week 31 socket tuning for every accepted session socket. `None`:
    /// only `TCP_NODELAY`.
    pub tuning: Option<SocketTuning>,
    /// Week 31: write reports with one vectored write of per-report frames
    /// instead of one coalesced buffer.
    pub vectored_writes: bool,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        GatewayConfig {
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
            instruments: vec![EngineConfig::new(InstrumentSpec::default())],
            auth: Auth::Open,
            io_threads: 2,
            inbound_capacity: 4096,
            outbound_capacity: 4096,
            read_buffer: 16 * 1024,
            logon_timeout: Duration::from_secs(5),
            cancel_on_disconnect: true,
            engine_idle: EngineIdle::default(),
            engine_cpu: None,
            tuning: None,
            vectored_writes: false,
        }
    }
}

/// Counters since start (each a relaxed atomic; a snapshot is not atomic
/// across fields).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GatewayStats {
    /// Connections accepted.
    pub connections: u64,
    /// Sessions that logged on.
    pub logons: u64,
    /// Connections closed (for any reason).
    pub closed: u64,
    /// Frames read from clients.
    pub frames_in: u64,
    /// Commands handed to the engine thread.
    pub commands_in: u64,
    /// Commands applied to the engines (gateway and pre-trade rejects
    /// excluded).
    pub sequenced: u64,
    /// Reports queued to sessions (LogonAck included).
    pub reports_out: u64,
    /// Sessions disconnected as slow consumers.
    pub slow_consumers: u64,
    /// Sessions closed for a protocol violation (bad frame, malformed
    /// message, no logon, bad credentials).
    pub protocol_errors: u64,
}

/// What [`Gateway::shutdown`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShutdownReport {
    /// From the call to the end of the last join.
    pub elapsed: Duration,
    /// Sessions that were open when shutdown began.
    pub sessions_closed: usize,
    /// Commands the engines applied over the gateway's life.
    pub sequenced: u64,
    /// The engine thread drained its queue and exited in time.
    pub engine_drained: bool,
    /// The engine thread panicked.
    pub engine_panicked: bool,
    /// Writer tasks that flushed and exited in time.
    pub writers_flushed: usize,
    /// Writer tasks still running at the deadline (aborted).
    pub writers_missed: usize,
}

impl ShutdownReport {
    /// True if everything drained and exited within the deadline.
    pub fn is_clean(&self) -> bool {
        self.engine_drained && !self.engine_panicked && self.writers_missed == 0
    }
}

/// A running gateway: a Tokio runtime with the sessions, and the engine
/// thread. Start it with [`Gateway::start`]; stop it with
/// [`Gateway::shutdown`] (dropping it shuts down with a 5 s deadline).
///
/// `start`, `shutdown` and `drop` block; call them from ordinary threads,
/// not from inside an async runtime.
// @student w13
pub struct Gateway {
    runtime: Option<tokio::runtime::Runtime>,
    local_addr: SocketAddr,
    ctx: Arc<Ctx>,
    acceptor: Option<tokio::task::JoinHandle<JoinSet<()>>>,
    engine: Option<std::thread::JoinHandle<u64>>,
    engine_token: CancelToken,
}

// @student w13
impl Gateway {
    /// Starts a gateway with the default [`Hooks`] (no pre-trade checks, no
    /// pipeline stage).
    pub fn start(config: GatewayConfig) -> std::io::Result<Gateway> {
        Gateway::start_with(config, Hooks::default())
    }

    /// Binds the listener, starts the engine thread and the runtime, and
    /// returns once the gateway accepts connections. Fails if the address
    /// cannot be bound or a thread cannot be created.
    pub fn start_with(config: GatewayConfig, hooks: Hooks) -> std::io::Result<Gateway> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(config.io_threads.max(1))
            .thread_name("gw-io")
            .enable_io()
            .enable_time()
            .build()?;
        let listener = runtime.block_on(bind(&config))?;
        let local_addr = listener.local_addr()?;

        let (inbound, rx) = mpsc::channel::<ToEngine>(config.inbound_capacity);
        let ctx = Arc::new(Ctx {
            auth: config.auth.clone(),
            logon_timeout: config.logon_timeout,
            read_buffer: config.read_buffer.max(64),
            outbound_capacity: config.outbound_capacity.max(1),
            tuning: config.tuning,
            vectored: config.vectored_writes,
            epoch: Instant::now(),
            next_session: AtomicU64::new(1),
            sessions: Mutex::new(HashMap::new()),
            stop: Stop::new(),
            engine_done: AtomicBool::new(false),
            inbound,
            stats: Counters::default(),
        });

        let sequencer = Sequencer::new(&config.instruments, hooks, config.cancel_on_disconnect);
        let engine_token = CancelToken::new();
        let engine = {
            let ctx = Arc::clone(&ctx);
            let token = engine_token.clone();
            let idle = config.engine_idle;
            let cpu = config.engine_cpu;
            std::thread::Builder::new()
                .name("gw-engine".into())
                .spawn(move || {
                    if let Some(cpu) = cpu {
                        if let Err(e) = sync::pin_current_thread(cpu) {
                            eprintln!("gateway: cannot pin the engine thread to cpu {cpu}: {e}");
                        }
                    }
                    engine_loop(rx, sequencer, &ctx, &token, idle)
                })?
        };

        let acceptor = runtime.spawn(acceptor(listener, Arc::clone(&ctx)));
        Ok(Gateway {
            runtime: Some(runtime),
            local_addr,
            ctx,
            acceptor: Some(acceptor),
            engine: Some(engine),
            engine_token,
        })
    }

    /// The address the gateway listens on.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// A snapshot of the gateway's counters.
    pub fn stats(&self) -> GatewayStats {
        self.ctx.stats.snapshot()
    }

    /// Stops the gateway: stop accepting and reading, drain the engine
    /// queue, flush every session's reports, close the sockets (see the
    /// module docs for why in this order). Gives up waiting at `timeout`.
    pub fn shutdown(mut self, timeout: Duration) -> ShutdownReport {
        self.stop(timeout)
    }
}

// @student w13
impl Drop for Gateway {
    fn drop(&mut self) {
        if self.runtime.is_some() {
            self.stop(Duration::from_secs(5));
        }
    }
}

// @student-begin w13
/// Bytes of one encoded report frame, at most.
const MAX_FRAME: usize = FRAME_HEADER_LEN + MAX_MSG_LEN;
/// Reports a writer takes from its queue per write.
const WRITE_BATCH: usize = 64;
/// Inbound messages the engine thread handles before flushing reports.
const ENGINE_BATCH: usize = 256;

/// Sessions → engine thread.
enum ToEngine {
    Open {
        session: SessionId,
        account: AccountId,
        out: tokio_mpsc::Sender<Message>,
        stop: Stop,
    },
    Command {
        session: SessionId,
        rx_ns: u64,
        cmd: Command,
    },
    Closed {
        session: SessionId,
    },
}

/// Shared by the acceptor, the sessions and the engine thread.
struct Ctx {
    auth: Auth,
    logon_timeout: Duration,
    read_buffer: usize,
    outbound_capacity: usize,
    tuning: Option<SocketTuning>,
    vectored: bool,
    epoch: Instant,
    next_session: AtomicU64,
    /// Live connections' stop signals (touched at connect, disconnect and
    /// shutdown only).
    sessions: Mutex<HashMap<SessionId, Stop>>,
    /// The acceptor's stop signal; session tokens are its children.
    stop: Stop,
    /// Set when the engine thread exits (normally or by panic), so readers
    /// stop waiting for queue space.
    engine_done: AtomicBool,
    inbound: mpsc::Producer<ToEngine>,
    stats: Counters,
}

impl Ctx {
    /// Nanoseconds since the gateway started (monotonic).
    fn now_ns(&self) -> u64 {
        self.epoch.elapsed().as_nanos() as u64
    }
}

#[derive(Default)]
struct Counters {
    connections: AtomicU64,
    logons: AtomicU64,
    closed: AtomicU64,
    frames_in: AtomicU64,
    commands_in: AtomicU64,
    sequenced: AtomicU64,
    reports_out: AtomicU64,
    slow_consumers: AtomicU64,
    protocol_errors: AtomicU64,
}

impl Counters {
    fn add(c: &AtomicU64, n: u64) {
        c.fetch_add(n, Ordering::Relaxed);
    }

    fn snapshot(&self) -> GatewayStats {
        let r = |c: &AtomicU64| c.load(Ordering::Relaxed);
        GatewayStats {
            connections: r(&self.connections),
            logons: r(&self.logons),
            closed: r(&self.closed),
            frames_in: r(&self.frames_in),
            commands_in: r(&self.commands_in),
            sequenced: r(&self.sequenced),
            reports_out: r(&self.reports_out),
            slow_consumers: r(&self.slow_consumers),
            protocol_errors: r(&self.protocol_errors),
        }
    }
}

/// A cancellation signal async tasks can wait on: a `CancelToken` (the
/// state, checkable from any thread) plus one `Notify` per waiting task
/// (the wakeup). `notify_one` stores a permit if its task is not waiting
/// yet, so no wakeup is lost.
#[derive(Clone)]
struct Stop {
    token: CancelToken,
    /// `[READER, WRITER]` (the acceptor uses `READER`).
    notify: Arc<[Notify; 2]>,
    reason: Arc<Mutex<Option<CloseReason>>>,
}

const READER: usize = 0;
const WRITER: usize = 1;

impl Stop {
    fn new() -> Stop {
        Stop::from_token(CancelToken::new())
    }

    fn from_token(token: CancelToken) -> Stop {
        Stop {
            token,
            notify: Arc::new([Notify::new(), Notify::new()]),
            reason: Arc::new(Mutex::new(None)),
        }
    }

    /// A signal cancelled with this one (born cancelled if this one is).
    fn child(&self) -> Stop {
        Stop::from_token(self.token.child())
    }

    fn cancel(&self, reason: CloseReason) {
        self.reason
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert(reason);
        self.token.cancel();
        for n in self.notify.iter() {
            n.notify_one();
        }
    }

    fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Why it was cancelled (a parent's cancel counts as `Shutdown`).
    fn reason(&self) -> CloseReason {
        self.reason
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .unwrap_or(CloseReason::Shutdown)
    }

    /// Completes once cancelled; `who` is `READER` or `WRITER` (one task
    /// each).
    async fn cancelled(&self, who: usize) {
        while !self.token.is_cancelled() {
            self.notify[who].notified().await;
        }
    }
}

async fn bind(config: &GatewayConfig) -> std::io::Result<TcpListener> {
    match &config.tuning {
        Some(t) => {
            let socket = if config.listen.is_ipv4() {
                tokio::net::TcpSocket::new_v4()?
            } else {
                tokio::net::TcpSocket::new_v6()?
            };
            socket.set_reuseaddr(true)?;
            t.apply_to_socket(&socket)?;
            socket.bind(config.listen)?;
            socket.listen(1024)
        }
        None => TcpListener::bind(config.listen).await,
    }
}

/// Accepts connections until stopped, then waits for every reader to exit
/// and hands the writers back to `shutdown`.
async fn acceptor(listener: TcpListener, ctx: Arc<Ctx>) -> JoinSet<()> {
    let mut readers = JoinSet::new();
    let mut writers = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            _ = ctx.stop.cancelled(READER) => break,
            Some(_) = readers.join_next(), if !readers.is_empty() => {}
            Some(_) = writers.join_next(), if !writers.is_empty() => {}
            accepted = listener.accept() => match accepted {
                Ok((stream, _peer)) => spawn_session(stream, &ctx, &mut readers, &mut writers),
                // Out of fds or a connection reset before accept: back off
                // briefly instead of spinning on the error.
                Err(_) => tokio::time::sleep(Duration::from_millis(5)).await,
            },
        }
    }
    drop(listener);
    while readers.join_next().await.is_some() {}
    writers
}

fn spawn_session(
    stream: TcpStream,
    ctx: &Arc<Ctx>,
    readers: &mut JoinSet<()>,
    writers: &mut JoinSet<()>,
) {
    Counters::add(&ctx.stats.connections, 1);
    let id = SessionId(ctx.next_session.fetch_add(1, Ordering::Relaxed));
    let tuned = match &ctx.tuning {
        Some(t) => t.apply(&stream),
        None => stream.set_nodelay(true),
    };
    if tuned.is_err() {
        Counters::add(&ctx.stats.closed, 1);
        return;
    }
    let stop = ctx.stop.child();
    ctx.sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, stop.clone());
    let (rd, wr) = stream.into_split();
    let (tx, rx) = tokio_mpsc::channel(ctx.outbound_capacity);
    writers.spawn(writer(wr, rx, stop.clone(), ctx.vectored));
    readers.spawn(reader(rd, tx, id, Arc::clone(ctx), stop));
}

/// Pushes into the engine queue, yielding to the runtime while it is full.
/// False if the engine thread is gone.
async fn to_engine(ctx: &Ctx, mut msg: ToEngine) -> bool {
    loop {
        match ctx.inbound.try_push(msg) {
            Ok(()) => return true,
            Err(back) => {
                if ctx.engine_done.load(Ordering::Acquire) {
                    return false;
                }
                msg = back;
                tokio::task::yield_now().await;
            }
        }
    }
}

async fn reader(
    mut rd: OwnedReadHalf,
    out: tokio_mpsc::Sender<Message>,
    id: SessionId,
    ctx: Arc<Ctx>,
    stop: Stop,
) {
    let mut session = Session::new(id);
    let mut decoder = FrameDecoder::default();
    let mut buf = vec![0u8; ctx.read_buffer];
    // Handed to the engine thread at logon (it then owns the only sender).
    let mut out = Some(out);
    let logon_deadline = tokio::time::sleep(ctx.logon_timeout);
    tokio::pin!(logon_deadline);

    let reason = 'conn: loop {
        if stop.is_cancelled() {
            break stop.reason();
        }
        let n = tokio::select! {
            biased;
            read = rd.read(&mut buf) => match read {
                Ok(0) => break CloseReason::PeerClosed,
                Ok(n) => n,
                Err(e) => break CloseReason::Io(e.kind()),
            },
            _ = stop.cancelled(READER) => break stop.reason(),
            _ = &mut logon_deadline, if session.state() == SessionState::AwaitingLogon => {
                break CloseReason::LogonTimeout
            }
        };
        let rx_ns = ctx.now_ns();
        let mut input = &buf[..n];
        loop {
            let action = match decoder.next_frame(&mut input) {
                Ok(Some(frame)) => {
                    Counters::add(&ctx.stats.frames_in, 1);
                    session.on_frame(frame, &ctx.auth)
                }
                Ok(None) => break,
                Err(e) => Action::Disconnect(CloseReason::BadFrame(e)),
            };
            let sent = match action {
                Action::Logon(logon) => {
                    let Some(out) = out.take() else {
                        break 'conn CloseReason::DuplicateLogon;
                    };
                    Counters::add(&ctx.stats.logons, 1);
                    let msg = ToEngine::Open {
                        session: id,
                        account: logon.account,
                        out,
                        stop: stop.clone(),
                    };
                    to_engine(&ctx, msg).await
                }
                Action::Forward(cmd) => {
                    Counters::add(&ctx.stats.commands_in, 1);
                    to_engine(
                        &ctx,
                        ToEngine::Command {
                            session: id,
                            rx_ns,
                            cmd,
                        },
                    )
                    .await
                }
                Action::Disconnect(reason) => break 'conn reason,
                Action::Ignore => true,
            };
            if !sent {
                break 'conn CloseReason::Shutdown;
            }
        }
    };

    if matches!(
        reason,
        CloseReason::NotLoggedOn
            | CloseReason::DuplicateLogon
            | CloseReason::BadCredentials
            | CloseReason::Malformed(_)
            | CloseReason::BadFrame(_)
            | CloseReason::LogonTimeout
    ) {
        Counters::add(&ctx.stats.protocol_errors, 1);
    }
    session.close(reason);
    match out {
        // Never logged on: dropping the only sender ends the writer.
        Some(out) => drop(out),
        // The engine thread holds the sender; it drops it after delivering
        // everything this session's commands produced.
        None => {
            to_engine(&ctx, ToEngine::Closed { session: id }).await;
        }
    }
    ctx.sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&id);
    Counters::add(&ctx.stats.closed, 1);
}

/// Drains the session's queue to its socket. Flushes everything queued
/// before closing — unless the session was kicked as a slow consumer: then
/// nobody is reading, and waiting for socket space would only hold the
/// task, so it gives up at once.
async fn writer(
    mut wr: OwnedWriteHalf,
    mut rx: tokio_mpsc::Receiver<Message>,
    stop: Stop,
    vectored: bool,
) {
    let mut batch: Vec<Message> = Vec::with_capacity(WRITE_BATCH);
    let mut buf: Vec<u8> = Vec::with_capacity(WRITE_BATCH * MAX_FRAME);
    // Week 31's vectored path, created only when configured.
    let mut frames: Option<crate::net::FrameBatch> = None;
    let mut draining = false;
    loop {
        let n = tokio::select! {
            biased;
            n = rx.recv_many(&mut batch, WRITE_BATCH) => n,
            _ = stop.cancelled(WRITER), if !draining => {
                if stop.reason() == CloseReason::SlowConsumer {
                    return;
                }
                draining = true;
                continue;
            }
        };
        if n == 0 {
            break;
        }
        let written = {
            let write = write_batch(&mut wr, &batch, &mut buf, &mut frames, vectored);
            tokio::pin!(write);
            loop {
                tokio::select! {
                    biased;
                    r = &mut write => break r,
                    _ = stop.cancelled(WRITER), if !draining => {
                        if stop.reason() == CloseReason::SlowConsumer {
                            return;
                        }
                        draining = true;
                    }
                }
            }
        };
        batch.clear();
        if let Err(e) = written {
            stop.cancel(CloseReason::Io(e.kind()));
            return;
        }
    }
    // Every sender is gone and the queue is empty: send FIN.
    let _ = wr.shutdown().await;
}

/// Encodes a batch of reports and writes it with one `write` (coalesced)
/// or one `writev` (week 31).
async fn write_batch(
    wr: &mut OwnedWriteHalf,
    batch: &[Message],
    buf: &mut Vec<u8>,
    frames: &mut Option<crate::net::FrameBatch>,
    vectored: bool,
) -> std::io::Result<()> {
    if vectored {
        let frames = frames.get_or_insert_with(|| crate::net::FrameBatch::new(WRITE_BATCH));
        frames.clear();
        for msg in batch {
            frames.push(msg);
        }
        frames.write_to(wr).await.map(drop)
    } else {
        buf.clear();
        for msg in batch {
            let start = buf.len();
            buf.resize(start + MAX_FRAME, 0);
            let n = encode_frame(msg, &mut buf[start..]);
            buf.truncate(start + n);
        }
        wr.write_all(buf).await
    }
}

/// Session registry entry on the engine thread.
struct Link {
    out: tokio_mpsc::Sender<Message>,
    stop: Stop,
    /// Kicked or closed: drop after this batch's reports.
    dead: bool,
}

/// The engine thread: drain the inbound queue in batches, run the
/// sequencer, release each batch's reports. Exits once cancelled and the
/// queue is empty. Returns the number of commands sequenced.
fn engine_loop(
    mut rx: mpsc::Consumer<ToEngine>,
    mut seq: Sequencer,
    ctx: &Ctx,
    token: &CancelToken,
    idle: EngineIdle,
) -> u64 {
    struct Done<'a>(&'a AtomicBool);
    impl Drop for Done<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let _done = Done(&ctx.engine_done);

    let mut links: HashMap<SessionId, Link> = HashMap::new();
    let mut outbox: Vec<(SessionId, Message)> = Vec::with_capacity(4 * ENGINE_BATCH);
    let mut closing: Vec<SessionId> = Vec::new();
    let mut empty_polls: u32 = 0;
    loop {
        let mut handled = 0;
        while handled < ENGINE_BATCH {
            let Some(msg) = rx.try_pop() else { break };
            handled += 1;
            let mut deliver = |s: SessionId, m: Message| outbox.push((s, m));
            match msg {
                ToEngine::Open {
                    session,
                    account,
                    out,
                    stop,
                } => {
                    links.insert(
                        session,
                        Link {
                            out,
                            stop,
                            dead: false,
                        },
                    );
                    seq.open(session, account, &mut deliver);
                }
                ToEngine::Command {
                    session,
                    rx_ns,
                    cmd,
                } => {
                    seq.command(session, rx_ns, &cmd, &mut deliver);
                }
                ToEngine::Closed { session } => {
                    seq.close(session, &mut deliver);
                    closing.push(session);
                }
            }
        }
        if handled > 0 {
            seq.flush();
            // Published before the reports: a client that has a report
            // never sees a count that misses its command.
            ctx.stats
                .sequenced
                .store(seq.sequenced(), Ordering::Relaxed);
            dispatch(&mut outbox, &mut links, &mut closing, ctx);
            empty_polls = 0;
            continue;
        }
        if token.is_cancelled() && rx.is_empty() {
            break;
        }
        empty_polls = empty_polls.saturating_add(1);
        match idle {
            EngineIdle::BusyPoll => {
                if empty_polls.is_multiple_of(1024) {
                    std::thread::yield_now();
                } else {
                    std::hint::spin_loop();
                }
            }
            EngineIdle::Backoff { spin, sleep } => {
                if empty_polls < spin {
                    std::hint::spin_loop();
                } else {
                    std::thread::sleep(sleep);
                }
            }
        }
    }
    // Dropping the links drops every outbound sender: writers flush and
    // close.
    drop(links);
    seq.sequenced()
}

/// Releases a batch's reports to the sessions' queues, never waiting: a
/// full queue marks a slow consumer, which is disconnected.
fn dispatch(
    outbox: &mut Vec<(SessionId, Message)>,
    links: &mut HashMap<SessionId, Link>,
    closing: &mut Vec<SessionId>,
    ctx: &Ctx,
) {
    let mut sent = 0;
    let mut any_dead = false;
    for (session, msg) in outbox.drain(..) {
        let Some(link) = links.get_mut(&session) else {
            continue;
        };
        if link.dead {
            continue;
        }
        match link.out.try_send(msg) {
            Ok(()) => sent += 1,
            Err(tokio_mpsc::error::TrySendError::Full(_)) => {
                Counters::add(&ctx.stats.slow_consumers, 1);
                link.stop.cancel(CloseReason::SlowConsumer);
                link.dead = true;
                any_dead = true;
            }
            Err(tokio_mpsc::error::TrySendError::Closed(_)) => {
                link.dead = true;
                any_dead = true;
            }
        }
    }
    Counters::add(&ctx.stats.reports_out, sent);
    for s in closing.drain(..) {
        links.remove(&s);
    }
    if any_dead {
        links.retain(|_, l| !l.dead);
    }
}

impl Gateway {
    fn stop(&mut self, timeout: Duration) -> ShutdownReport {
        let start = Instant::now();
        let deadline = start + timeout;
        let left = || deadline.saturating_duration_since(Instant::now());
        let Some(runtime) = self.runtime.take() else {
            return ShutdownReport::default();
        };
        let mut report = ShutdownReport::default();

        // 1. Stop accepting and reading.
        self.ctx.stop.cancel(CloseReason::Shutdown);
        {
            let sessions = self.ctx.sessions.lock().unwrap_or_else(|e| e.into_inner());
            report.sessions_closed = sessions.len();
            for stop in sessions.values() {
                stop.cancel(CloseReason::Shutdown);
            }
        }
        // 2. Every reader has exited (and told the engine thread).
        let mut writers = match self.acceptor.take() {
            Some(handle) => runtime
                .block_on(async { tokio::time::timeout(left(), handle).await })
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default(),
            None => JoinSet::new(),
        };
        // 3. The engine drains its queue, then drops every outbound queue.
        self.engine_token.cancel();
        if let Some(engine) = self.engine.take() {
            while !engine.is_finished() && !left().is_zero() {
                std::thread::sleep(Duration::from_millis(1));
            }
            if engine.is_finished() {
                match engine.join() {
                    Ok(sequenced) => {
                        report.engine_drained = true;
                        report.sequenced = sequenced;
                    }
                    Err(_) => report.engine_panicked = true,
                }
            }
        }
        if !report.engine_drained {
            report.sequenced = self.ctx.stats.sequenced.load(Ordering::Relaxed);
        }
        // 4. Writers flush what is queued and close with FIN.
        let total = writers.len();
        let flushed = runtime.block_on(async {
            let mut done = 0;
            let _ = tokio::time::timeout(left(), async {
                while writers.join_next().await.is_some() {
                    done += 1;
                }
            })
            .await;
            writers.abort_all();
            done
        });
        report.writers_flushed = flushed;
        report.writers_missed = total - flushed;
        runtime.shutdown_timeout(left());
        report.elapsed = start.elapsed();
        report
    }
}
// @student-end
