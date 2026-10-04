//! Week 47 — the server wiring: an engine thread stepping the simulated
//! market at a fixed rate and publishing L2 data, a WebSocket feed, and a
//! small HTTP server for the page, health, metrics and stats.
//!
//! # Threads and what crosses between them
//!
//! | Thread | Owns | Talks to others through |
//! | --- | --- | --- |
//! | `demo-engine` | `LiveSession`, `L2Publisher`, the ring's writer, its metric handles | the broadcast ring (updates), the `SnapshotCell`, atomics and a small mutex-guarded view for `/stats` |
//! | `demo-ws-accept` | the feed listener, the list of client sockets | spawns one `ws-client` thread per client (`marketdata::serve_ws_client`, reading the ring) |
//! | `demo-http-accept` | the HTTP listener | spawns one short-lived thread per request |
//!
//! The engine never waits on a client: the ring's writer never blocks, a
//! slow client lags, gets conflated batches or a fresh snapshot, and is
//! disconnected if its socket stays full (week 16's policy).
//!
//! # Pacing
//!
//! The engine thread runs on a schedule, not a sleep per command: by time
//! `t` it should have processed `t × rate` commands, so each wake-up runs
//! whatever is due (capped per batch) and then sleeps about a millisecond.
//! If it falls more than a second behind (a stalled VM), it drops the
//! backlog rather than bursting. `rate == 0` runs flat out.
//!
//! # Shutdown
//!
//! [`Demo::shutdown`], `POST /shutdown` or the end of `--duration` set one
//! flag. The engine thread finishes its batch and exits; [`Demo::wait`]
//! then wakes both acceptors (a connection to their own port), closes every
//! feed client's socket so its thread ends, joins the threads and returns
//! the final [`Stats`]. `/health` turns 503 as soon as the flag is set.

use crate::config::Config;
use crate::stats::Stats;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;
use telemetry::Registry;

// @student-begin w47
use crate::client::dial_addr;
use crate::page::render_page;
use crate::stats::{TradePrint, TAPE_LEN};
use marketdata::{
    broadcast, serve_ws_client, BroadcastRing, L2Publisher, L2Update, Level, RingPublisher,
    SnapshotCell,
};
use sim::LiveSession;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;
use telemetry::{Clock, LATENCY_BOUNDS_NS};
use types::Event;
// @student-end

/// Most commands the engine thread runs between two looks at the clock and
/// the stop flag.
pub const MAX_BATCH: u64 = 1_024;

/// Longest the engine sleeps when it is ahead of schedule.
pub const IDLE_SLEEP: Duration = Duration::from_millis(1);

/// HTTP requests served at once; more get 503.
pub const MAX_HTTP_CONNECTIONS: u64 = 64;

/// Read and write timeout of one HTTP connection.
pub const HTTP_IO_TIMEOUT: Duration = Duration::from_secs(2);

/// Largest HTTP request head the server reads.
pub const MAX_REQUEST_HEAD: usize = 8 * 1024;

/// The demo exchange, running. Created by [`Demo::start`]; stop it with
/// [`shutdown`](Demo::shutdown) then [`wait`](Demo::wait), or
/// [`stop`](Demo::stop). Dropping it requests a shutdown but does not
/// wait for the threads.
// @student w47
pub struct Demo {
    shared: Arc<Shared>,
    engine: Option<JoinHandle<()>>,
    ws: Option<JoinHandle<()>>,
    http: Option<JoinHandle<()>>,
}

// @student w47
impl Demo {
    /// Binds both listeners (so the ports are known and in use before it
    /// returns), then starts the engine, feed and HTTP threads. Errors are
    /// bind or spawn failures. Allocates the ring (`ring_capacity × 32`
    /// bytes) and the session.
    pub fn start(config: Config) -> io::Result<Demo> {
        let http_listener = TcpListener::bind(config.http_addr)?;
        let ws_listener = TcpListener::bind(config.ws_addr)?;
        let (tx, ring) = broadcast::<L2Update>(config.ring_capacity);
        let shared = Arc::new(Shared {
            http_addr: http_listener.local_addr()?,
            ws_addr: ws_listener.local_addr()?,
            config,
            started: Instant::now(),
            stop: AtomicBool::new(false),
            commands: AtomicU64::new(0),
            trades: AtomicU64::new(0),
            volume: AtomicU64::new(0),
            feed_seq: AtomicU64::new(0),
            ws_clients: AtomicU64::new(0),
            ws_connections: AtomicU64::new(0),
            ws_rejected: AtomicU64::new(0),
            http_active: AtomicU64::new(0),
            snapshots: SnapshotCell::default(),
            view: Mutex::new(View::default()),
            registry: Registry::new(),
        });
        let mut demo = Demo {
            shared: Arc::clone(&shared),
            engine: None,
            ws: None,
            http: None,
        };
        // On a spawn failure `demo` drops here, which stops what did start.
        let s = Arc::clone(&shared);
        demo.engine = Some(spawn("demo-engine", move || run_engine(&s, tx))?);
        let s = Arc::clone(&shared);
        demo.ws = Some(spawn("demo-ws-accept", move || {
            run_ws(&s, ws_listener, ring)
        })?);
        let s = Arc::clone(&shared);
        demo.http = Some(spawn("demo-http-accept", move || {
            run_http(&s, http_listener)
        })?);
        Ok(demo)
    }

    /// The HTTP server's bound address (the real port when configured
    /// with port 0).
    pub fn http_addr(&self) -> SocketAddr {
        self.shared.http_addr
    }

    /// The WebSocket feed's bound address.
    pub fn ws_addr(&self) -> SocketAddr {
        self.shared.ws_addr
    }

    /// The metrics registry behind `/metrics`.
    pub fn registry(&self) -> &Registry {
        &self.shared.registry
    }

    /// A consistent-enough view of the counters (each value is exact; they
    /// are not read atomically together). Takes the view lock briefly;
    /// allocates the tape.
    pub fn stats(&self) -> Stats {
        self.shared.stats()
    }

    /// Whether a shutdown was requested (by [`shutdown`](Self::shutdown),
    /// `POST /shutdown` or the configured duration).
    pub fn is_stopping(&self) -> bool {
        self.shared.stopping()
    }

    /// Requests a shutdown and returns at once; idempotent.
    pub fn shutdown(&self) {
        self.shared.request_stop();
    }

    /// Blocks until a shutdown is requested, then drains: joins the engine
    /// thread, wakes and joins both acceptors (which close every feed
    /// client), and returns the final stats. Re-raises a panic of the
    /// engine thread after cleaning up.
    pub fn wait(mut self) -> Stats {
        let engine = self.engine.take().map(JoinHandle::join);
        // The engine only returns once the flag is set, unless it panicked.
        self.shared.request_stop();
        wake(self.shared.ws_addr);
        wake(self.shared.http_addr);
        for handle in [self.ws.take(), self.http.take()].into_iter().flatten() {
            let _ = handle.join();
        }
        if let Some(Err(panic)) = engine {
            std::panic::resume_unwind(panic);
        }
        self.shared.stats()
    }

    /// [`shutdown`](Self::shutdown) then [`wait`](Self::wait).
    pub fn stop(self) -> Stats {
        self.shutdown();
        self.wait()
    }
}

// @student w47
impl Drop for Demo {
    fn drop(&mut self) {
        if self.engine.is_some() || self.ws.is_some() || self.http.is_some() {
            self.shared.request_stop();
            wake(self.shared.ws_addr);
            wake(self.shared.http_addr);
        }
    }
}

// @student-begin w47
/// State the threads share. Counters the HTTP side reads are atomics with
/// a single writer; the view for `/stats` is behind a mutex the engine
/// takes at most every [`VIEW_EVERY`].
struct Shared {
    config: Config,
    http_addr: SocketAddr,
    ws_addr: SocketAddr,
    started: Instant,
    stop: AtomicBool,
    commands: AtomicU64,
    trades: AtomicU64,
    volume: AtomicU64,
    feed_seq: AtomicU64,
    ws_clients: AtomicU64,
    ws_connections: AtomicU64,
    ws_rejected: AtomicU64,
    http_active: AtomicU64,
    snapshots: SnapshotCell,
    view: Mutex<View>,
    registry: Registry,
}

/// What `/stats` shows that is not a counter.
#[derive(Default)]
struct View {
    best_bid: Option<Level>,
    best_ask: Option<Level>,
    /// Newest first.
    tape: VecDeque<TradePrint>,
}

/// How often the engine refreshes the `/stats` view.
const VIEW_EVERY: Duration = Duration::from_millis(20);

impl Shared {
    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    fn stats(&self) -> Stats {
        let view = self.view.lock().unwrap_or_else(|p| p.into_inner());
        let feed_seq = self.feed_seq.load(Ordering::Acquire);
        let stopping = self.stopping();
        Stats {
            uptime: self.started.elapsed(),
            ready: feed_seq > 0 && !stopping,
            stopping,
            rate: self.config.rate,
            seed: self.config.seed,
            commands: self.commands.load(Ordering::Relaxed),
            trades: self.trades.load(Ordering::Relaxed),
            volume: self.volume.load(Ordering::Relaxed),
            feed_seq,
            snapshot_seq: self.snapshots.seq().0,
            ws_clients: self.ws_clients.load(Ordering::Relaxed),
            ws_connections: self.ws_connections.load(Ordering::Relaxed),
            ws_rejected: self.ws_rejected.load(Ordering::Relaxed),
            best_bid: view.best_bid,
            best_ask: view.best_ask,
            last_trades: view.tape.iter().copied().collect(),
        }
    }
}

fn spawn<F: FnOnce() + Send + 'static>(name: &str, f: F) -> io::Result<JoinHandle<()>> {
    std::thread::Builder::new().name(name.into()).spawn(f)
}

/// Unblocks a thread waiting in `accept` on `addr` (it then sees the stop
/// flag). Errors only mean the listener is already gone.
fn wake(addr: SocketAddr) {
    let _ = TcpStream::connect_timeout(&dial_addr(addr), Duration::from_secs(1));
}

// ---------------------------------------------------------------------------
// Engine thread

/// Commands due by now on a fixed-rate schedule.
struct Pacer {
    rate: u64,
    epoch: Instant,
    done: u64,
}

impl Pacer {
    fn new(rate: u64) -> Pacer {
        Pacer {
            rate,
            epoch: Instant::now(),
            done: 0,
        }
    }

    /// How many commands to run now (0: sleep a little), at most
    /// [`MAX_BATCH`].
    fn due(&mut self) -> u64 {
        if self.rate == 0 {
            return MAX_BATCH;
        }
        let elapsed = self.epoch.elapsed().as_nanos();
        let target = (elapsed * self.rate as u128 / 1_000_000_000) as u64;
        let behind = target.saturating_sub(self.done);
        if behind > self.rate.max(MAX_BATCH) {
            // More than a second behind: start a new schedule from now.
            self.epoch = Instant::now();
            self.done = 0;
            return 0;
        }
        let n = behind.min(MAX_BATCH);
        self.done += n;
        n
    }
}

fn run_engine(shared: &Shared, mut tx: RingPublisher<L2Update>) {
    let config = &shared.config;
    let registry = &shared.registry;
    let mut commands =
        registry.counter("hft_demo_commands_total", "Commands the engine processed.");
    let mut trades = registry.counter("hft_demo_trades_total", "Trades the engine printed.");
    let mut updates = registry.counter(
        "hft_demo_l2_updates_total",
        "L2 updates published to the feed ring.",
    );
    let mut latency = registry.histogram(
        "hft_demo_publish_latency_ns",
        "Per command: engine, L2 publisher and ring publish, in nanoseconds.",
        LATENCY_BOUNDS_NS,
    );
    let clock = Clock::global();
    let mut session = LiveSession::new(config.seed);
    let mut publisher = L2Publisher::with_snapshot_interval(config.snapshot_interval);
    let mut pacer = Pacer::new(config.rate);
    let mut tape: VecDeque<TradePrint> = VecDeque::with_capacity(TAPE_LEN);
    let (mut n_trades, mut volume) = (0u64, 0u64);
    let mut last_view = Instant::now();

    while !shared.stopping() {
        if config
            .duration
            .is_some_and(|d| shared.started.elapsed() >= d)
        {
            shared.request_stop();
            break;
        }
        let n = pacer.due();
        if n == 0 {
            std::thread::sleep(IDLE_SLEEP);
            continue;
        }
        let mut published = 0;
        let trades_before = n_trades;
        for _ in 0..n {
            let t0 = clock.now();
            session.step();
            for ev in session.last_events() {
                if let Event::Trade {
                    price,
                    qty,
                    taker_side,
                    ..
                } = *ev
                {
                    n_trades += 1;
                    volume += qty.0;
                    if tape.len() == TAPE_LEN {
                        tape.pop_back();
                    }
                    tape.push_front(TradePrint {
                        n: n_trades,
                        price,
                        qty,
                        taker_side,
                    });
                }
                if let Some(update) = publisher.on_event(ev) {
                    tx.publish(update);
                    published += 1;
                }
                if let Some(snapshot) = publisher.periodic_snapshot() {
                    shared.snapshots.store(snapshot);
                }
            }
            latency.record(clock.now().saturating_sub(t0));
        }

        commands.add(n);
        trades.add(n_trades - trades_before);
        updates.add(published);
        shared.commands.fetch_add(n, Ordering::Relaxed);
        shared.trades.store(n_trades, Ordering::Relaxed);
        shared.volume.store(volume, Ordering::Relaxed);
        shared
            .feed_seq
            .store(publisher.last_seq().0, Ordering::Release);
        if last_view.elapsed() >= VIEW_EVERY {
            last_view = Instant::now();
            refresh_view(shared, &publisher, &tape);
        }
    }
    // The final stats show the final book.
    refresh_view(shared, &publisher, &tape);
}

fn refresh_view(shared: &Shared, publisher: &L2Publisher, tape: &VecDeque<TradePrint>) {
    let mut view = shared.view.lock().unwrap_or_else(|p| p.into_inner());
    view.best_bid = publisher.best_bid();
    view.best_ask = publisher.best_ask();
    view.tape.clone_from(tape);
}

// ---------------------------------------------------------------------------
// WebSocket feed

/// Decrements the client gauge when a client thread ends, however it ends.
struct ClientSlot(Arc<Shared>);

impl Drop for ClientSlot {
    fn drop(&mut self) {
        self.0.ws_clients.fetch_sub(1, Ordering::AcqRel);
    }
}

fn run_ws(shared: &Arc<Shared>, listener: TcpListener, ring: Arc<BroadcastRing<L2Update>>) {
    let mut accepted = shared.registry.counter(
        "hft_demo_ws_connections_total",
        "WebSocket feed clients accepted.",
    );
    let mut refused = shared.registry.counter(
        "hft_demo_ws_rejected_total",
        "WebSocket connections refused over the client limit.",
    );
    let mut clients: Vec<(TcpStream, JoinHandle<()>)> = Vec::new();
    for stream in listener.incoming() {
        if shared.stopping() {
            break;
        }
        let Ok(stream) = stream else { continue };
        clients.retain(|(_, thread)| !thread.is_finished());
        if shared.ws_clients.load(Ordering::Acquire) >= shared.config.max_ws_clients as u64 {
            refused.inc();
            shared.ws_rejected.fetch_add(1, Ordering::Relaxed);
            let _ = spawn("ws-refuse", move || {
                refuse(stream, "503 Service Unavailable")
            });
            continue;
        }
        let Ok(socket) = stream.try_clone() else {
            continue;
        };
        shared.ws_clients.fetch_add(1, Ordering::AcqRel);
        let slot = ClientSlot(Arc::clone(shared));
        let ring = Arc::clone(&ring);
        let snapshots = shared.snapshots.clone();
        let thread = spawn("ws-client", move || {
            let _slot = slot;
            // A client that misbehaves or goes away ends only its session.
            let _ = serve_ws_client(stream, &ring, &snapshots);
        });
        // A failed spawn dropped the closure, and with it the slot.
        if let Ok(thread) = thread {
            accepted.inc();
            shared.ws_connections.fetch_add(1, Ordering::Relaxed);
            clients.push((socket, thread));
        }
    }
    // Closing the sockets ends each client thread at its next read or write.
    for (socket, _) in &clients {
        let _ = socket.shutdown(Shutdown::Both);
    }
    for (_, thread) in clients {
        let _ = thread.join();
    }
}

/// Answers a connection with `status` and no body, after reading its
/// request (closing with unread input would reset the connection before
/// the client sees the answer).
fn refuse(mut stream: TcpStream, status: &str) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = read_head(&mut stream);
    let response = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.shutdown(Shutdown::Write);
}

// ---------------------------------------------------------------------------
// HTTP

/// Releases an HTTP connection slot.
struct HttpSlot(Arc<Shared>);

impl Drop for HttpSlot {
    fn drop(&mut self) {
        self.0.http_active.fetch_sub(1, Ordering::AcqRel);
    }
}

fn run_http(shared: &Arc<Shared>, listener: TcpListener) {
    for stream in listener.incoming() {
        if shared.stopping() {
            break;
        }
        let Ok(stream) = stream else { continue };
        if shared.http_active.fetch_add(1, Ordering::AcqRel) >= MAX_HTTP_CONNECTIONS {
            shared.http_active.fetch_sub(1, Ordering::AcqRel);
            let _ = spawn("http-refuse", move || {
                refuse(stream, "503 Service Unavailable")
            });
            continue;
        }
        let slot = HttpSlot(Arc::clone(shared));
        let _ = spawn("http-conn", move || {
            let _ = handle_http(&slot.0, stream);
        });
    }
}

/// A response before it is written.
struct Response {
    status: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
    allow: Option<&'static str>,
}

impl Response {
    fn new(status: &'static str, content_type: &'static str, body: impl Into<Vec<u8>>) -> Response {
        Response {
            status,
            content_type,
            body: body.into(),
            allow: None,
        }
    }

    fn text(status: &'static str, body: &str) -> Response {
        Response::new(status, "text/plain; charset=utf-8", format!("{body}\n"))
    }
}

/// Reads up to the end of the request head. `None` on EOF, timeout or an
/// oversized head.
fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            return Some(String::from_utf8_lossy(&buf[..end]).into_owned());
        }
        if buf.len() > MAX_REQUEST_HEAD {
            return None;
        }
    }
}

fn handle_http(shared: &Shared, mut stream: TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(HTTP_IO_TIMEOUT))?;
    stream.set_write_timeout(Some(HTTP_IO_TIMEOUT))?;
    let response = match read_head(&mut stream) {
        Some(head) => route(shared, &head),
        None => Response::text("400 Bad Request", "bad request"),
    };
    let mut out = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        response.status,
        response.content_type,
        response.body.len()
    );
    if let Some(allow) = response.allow {
        out.push_str(&format!("Allow: {allow}\r\n"));
    }
    out.push_str("\r\n");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(&response.body);
    stream.write_all(&bytes)?;
    stream.shutdown(Shutdown::Write)
}

fn route(shared: &Shared, head: &str) -> Response {
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(target), Some(version)) = (
        request_line.next(),
        request_line.next(),
        request_line.next(),
    ) else {
        return Response::text("400 Bad Request", "bad request line");
    };
    if !version.starts_with("HTTP/1.") {
        return Response::text("505 HTTP Version Not Supported", "HTTP/1.x only");
    }
    let path = target.split('?').next().unwrap_or(target);
    let (label, allowed) = match path {
        "/" | "/health" | "/metrics" | "/stats" => (path, "GET"),
        // Without a token the route does not exist at all.
        "/shutdown" if shared.config.shutdown_token.is_some() => (path, "POST"),
        _ => ("other", ""),
    };
    shared
        .registry
        .counter_with_labels(
            "hft_demo_http_requests_total",
            "HTTP requests, by route.",
            &[("route", label)],
        )
        .inc();
    if allowed.is_empty() {
        return Response::text("404 Not Found", "not found");
    }
    if method != allowed {
        let mut r = Response::text("405 Method Not Allowed", "method not allowed");
        r.allow = Some(allowed);
        return r;
    }
    match path {
        "/" => Response::new(
            "200 OK",
            "text/html; charset=utf-8",
            render_page(
                shared.ws_addr.port(),
                shared.config.public_ws_url.as_deref(),
            ),
        ),
        "/health" => {
            let stats = shared.stats();
            let status = if stats.ready {
                "200 OK"
            } else {
                "503 Service Unavailable"
            };
            Response::new(status, "application/json", stats.health_json())
        }
        "/stats" => Response::new("200 OK", "application/json", shared.stats().to_json()),
        "/metrics" => Response::new(
            "200 OK",
            "text/plain; version=0.0.4; charset=utf-8",
            render_metrics(shared),
        ),
        _ => shutdown_route(shared, lines),
    }
}

/// `POST /shutdown` with `Authorization: Bearer <token>` (only routed when
/// a token is configured): 403 for a wrong or missing one, else 202.
fn shutdown_route<'a>(shared: &Shared, mut headers: impl Iterator<Item = &'a str>) -> Response {
    let token = shared.config.shutdown_token.as_deref().unwrap_or_default();
    let presented = headers.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("authorization")
            .then(|| value.trim().strip_prefix("Bearer "))?
    });
    if !presented.is_some_and(|p| constant_time_eq(p.trim().as_bytes(), token.as_bytes())) {
        return Response::text("403 Forbidden", "forbidden");
    }
    shared.request_stop();
    Response::text("202 Accepted", "shutting down")
}

/// Compares without an early exit on the first differing byte.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The registry's counters and histograms, plus gauges that live in the
/// shared state.
fn render_metrics(shared: &Shared) -> String {
    use std::fmt::Write as _;
    let mut out = shared.registry.render_prometheus();
    let stats = shared.stats();
    let gauges: [(&str, &str, String); 4] = [
        (
            "hft_demo_ws_clients",
            "WebSocket feed clients connected now.",
            stats.ws_clients.to_string(),
        ),
        (
            "hft_demo_feed_seq",
            "Sequence number of the last L2 update published.",
            stats.feed_seq.to_string(),
        ),
        (
            "hft_demo_ready",
            "1 when the feed is publishing and the demo is not stopping.",
            u8::from(stats.ready).to_string(),
        ),
        (
            "hft_demo_uptime_seconds",
            "Seconds since the demo started.",
            format!("{:.3}", stats.uptime.as_secs_f64()),
        ),
    ];
    for (name, help, value) in gauges {
        let _ = write!(
            out,
            "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {value}\n"
        );
    }
    out
}
// @student-end
