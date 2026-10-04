//! The operation engine both backends share, the readiness pollers, and
//! the inline readiness backend.

#[allow(unused_imports)] // used by the implementation the learner writes
use super::{
    errno_of, Completion, ConnId, IoBackend, IoStats, Submission, EBADF, EBUSY, ECANCELED, EINVAL,
    EPIPE,
};
#[allow(unused_imports)] // used by the implementation the learner writes
use std::collections::VecDeque;
#[allow(unused_imports)] // used by the implementation the learner writes
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
#[allow(unused_imports)] // used by the implementation the learner writes
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
#[allow(unused_imports)] // used by the implementation the learner writes
use std::time::{Duration, Instant};

/// What a poller watches: a raw file descriptor on unix.
#[cfg(unix)]
pub type Source = std::os::fd::RawFd;
/// What a poller watches (nothing to name off unix: the scan poller
/// ignores it).
#[cfg(not(unix))]
pub type Source = ();

/// Readiness reported by a [`Poller`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ready {
    /// The token the source was added with.
    pub token: u64,
    /// Reading may make progress (data, end of stream or an error).
    pub readable: bool,
    /// Writing may make progress.
    pub writable: bool,
}

/// A readiness source: tells the driver which sockets may make progress.
/// Edge-triggered: a socket is reported when it *becomes* ready; the
/// driver remembers readiness until an operation hits `WouldBlock`.
pub trait Poller: Send + 'static {
    /// Watches `source` for read and write readiness under `token`.
    fn add(&mut self, source: Source, token: u64) -> io::Result<()>;

    /// Stops watching `source`.
    fn remove(&mut self, source: Source, token: u64) -> io::Result<()>;

    /// Waits up to `timeout` (`None`: forever; zero: just check) for
    /// readiness or a [`Waker::wake`], appending events to `events`.
    /// Returns the syscalls it made.
    fn wait(&mut self, events: &mut Vec<Ready>, timeout: Option<Duration>) -> io::Result<u64>;

    /// A handle another thread uses to interrupt [`wait`](Self::wait).
    fn waker(&mut self) -> io::Result<Waker>;
}

/// Interrupts a [`Poller::wait`] from another thread.
#[derive(Clone, Debug)]
#[allow(dead_code)] // read by the implementation the learner writes
pub struct Waker(pub(crate) WakeHandle);

#[derive(Clone, Debug)]
#[allow(dead_code)] // built by the implementation the learner writes
pub(crate) enum WakeHandle {
    /// One byte into a socket the poller watches.
    #[cfg(unix)]
    Socket(Arc<std::os::unix::net::UnixStream>),
    /// A flag and a condition variable the scan poller sleeps on.
    Flag(Arc<(Mutex<bool>, Condvar)>),
}

/// How long the scan poller sleeps between scans.
pub const SCAN_INTERVAL: Duration = Duration::from_micros(50);

/// The portable fallback poller: no readiness information at all. Each
/// wait sleeps briefly (or until woken), then reports every source as
/// ready, so the driver simply retries each pending operation — a
/// non-blocking `read`/`write` that usually returns `WouldBlock`. Works
/// everywhere std's non-blocking sockets do; costs a syscall per pending
/// operation per scan.
#[derive(Debug)]
#[allow(dead_code)] // read by the implementation the learner writes
pub struct ScanPoller {
    tokens: Vec<u64>,
    wake: Arc<(Mutex<bool>, Condvar)>,
}

impl ScanPoller {
    /// A poller watching nothing yet.
    pub fn new() -> ScanPoller {
        ScanPoller {
            tokens: Vec::new(),
            wake: Arc::new((Mutex::new(false), Condvar::new())),
        }
    }
}

impl Default for ScanPoller {
    fn default() -> Self {
        ScanPoller::new()
    }
}

// @student w32
impl Poller for ScanPoller {
    fn add(&mut self, _source: Source, token: u64) -> io::Result<()> {
        self.tokens.push(token);
        Ok(())
    }

    fn remove(&mut self, _source: Source, token: u64) -> io::Result<()> {
        self.tokens.retain(|&t| t != token);
        Ok(())
    }

    fn wait(&mut self, events: &mut Vec<Ready>, timeout: Option<Duration>) -> io::Result<u64> {
        let pause = timeout.unwrap_or(SCAN_INTERVAL).min(SCAN_INTERVAL);
        if !pause.is_zero() {
            let (lock, cond) = &*self.wake;
            let mut woken = lock.lock().unwrap_or_else(|e| e.into_inner());
            if !*woken {
                woken = cond
                    .wait_timeout(woken, pause)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
            *woken = false;
        }
        events.extend(self.tokens.iter().map(|&token| Ready {
            token,
            readable: true,
            writable: true,
        }));
        Ok(0)
    }

    fn waker(&mut self) -> io::Result<Waker> {
        Ok(Waker(WakeHandle::Flag(Arc::clone(&self.wake))))
    }
}

// @student w32
impl Waker {
    /// Interrupts the poller's current (or next) wait. One syscall for the
    /// socket waker (a 1-byte write).
    pub fn wake(&self) -> io::Result<()> {
        match &self.0 {
            #[cfg(unix)]
            WakeHandle::Socket(s) => match (&**s).write(&[1]) {
                // A full socket already holds a pending wakeup.
                Ok(_) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
                Err(e) => Err(e),
            },
            WakeHandle::Flag(pair) => {
                let (lock, cond) = &**pair;
                *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
                cond.notify_one();
                Ok(())
            }
        }
    }
}

/// Token the driver uses for the listener.
#[allow(dead_code)] // used by the implementation the learner writes
pub(crate) const LISTENER: u64 = u64::MAX;
/// Token pollers use for their waker.
#[allow(dead_code)] // used by the implementation the learner writes
pub(crate) const WAKE: u64 = u64::MAX - 1;

// @student-begin w32
#[cfg(unix)]
fn source_of<S: std::os::fd::AsRawFd>(s: &S) -> Source {
    s.as_raw_fd()
}

#[cfg(not(unix))]
fn source_of<S>(_: &S) -> Source {}

struct PendingRead {
    buf: Vec<u8>,
    user_data: u64,
}

struct PendingWrite {
    buf: Vec<u8>,
    written: usize,
    user_data: u64,
}

struct Conn {
    stream: TcpStream,
    /// Edge-triggered readiness cache: true until an operation hits
    /// `WouldBlock` (optimistic at registration).
    readable: bool,
    writable: bool,
    read: Option<PendingRead>,
    writes: VecDeque<PendingWrite>,
}

struct Listener {
    listener: TcpListener,
    ready: bool,
    waiting: VecDeque<u64>,
}

/// Performs operations on non-blocking sockets as readiness allows: the
/// "kernel side" of both backends.
pub(crate) struct Driver<P: Poller> {
    poller: P,
    /// Indexed by `ConnId` (a slot stays `None` until its connection
    /// arrives, and after it closes).
    conns: Vec<Option<Conn>>,
    /// Connection id allocator, shared with the caller's thread in the
    /// ring backend so ids it hands out and ids of accepted connections
    /// never collide.
    ids: Arc<AtomicU32>,
    listener: Option<Listener>,
    events: Vec<Ready>,
    /// Operations waiting for readiness (reads, queued writes, accepts):
    /// lets "is anything armed?" be O(1).
    armed: usize,
    /// Syscalls made so far (attempts and waits).
    pub(crate) syscalls: u64,
}

fn done(out: &mut Vec<Completion>, user_data: u64, result: i64, buf: Option<Vec<u8>>) {
    out.push(Completion {
        user_data,
        result,
        buf,
    });
}

impl<P: Poller> Driver<P> {
    pub(crate) fn new(poller: P) -> Driver<P> {
        Driver {
            poller,
            conns: Vec::new(),
            ids: Arc::new(AtomicU32::new(0)),
            listener: None,
            events: Vec::with_capacity(256),
            armed: 0,
            syscalls: 0,
        }
    }

    /// The connection id allocator.
    pub(crate) fn ids(&self) -> Arc<AtomicU32> {
        Arc::clone(&self.ids)
    }

    /// Registers a connection under a fresh id.
    pub(crate) fn register(&mut self, stream: TcpStream) -> io::Result<ConnId> {
        let id = ConnId(self.ids.fetch_add(1, Ordering::Relaxed));
        self.register_as(stream, id)?;
        Ok(id)
    }

    /// Registers a connection under an id already taken from [`ids`](Self::ids).
    pub(crate) fn register_as(&mut self, stream: TcpStream, id: ConnId) -> io::Result<()> {
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        self.poller.add(source_of(&stream), id.0 as u64)?;
        let i = id.0 as usize;
        if self.conns.len() <= i {
            self.conns.resize_with(i + 1, || None);
        }
        self.conns[i] = Some(Conn {
            stream,
            readable: true,
            writable: true,
            read: None,
            writes: VecDeque::new(),
        });
        Ok(())
    }

    pub(crate) fn listen(&mut self, listener: TcpListener) -> io::Result<()> {
        listener.set_nonblocking(true)?;
        self.poller.add(source_of(&listener), LISTENER)?;
        let waiting = self.listener.take().map(|l| l.waiting).unwrap_or_default();
        self.listener = Some(Listener {
            listener,
            ready: true,
            waiting,
        });
        Ok(())
    }

    /// True if an operation is waiting for readiness. O(1).
    pub(crate) fn has_pending(&self) -> bool {
        self.armed > 0
    }

    /// Starts an operation, completing it at once if the socket allows.
    pub(crate) fn start(&mut self, sub: Submission, out: &mut Vec<Completion>) {
        match sub {
            Submission::Accept { user_data } => match self.listener.as_mut() {
                Some(l) => {
                    l.waiting.push_back(user_data);
                    self.armed += 1;
                    self.progress_accept(out);
                }
                None => done(out, user_data, -EINVAL, None),
            },
            Submission::Read {
                conn,
                buf,
                user_data,
            } => {
                let Some(c) = self.conn_mut(conn) else {
                    return done(out, user_data, -EBADF, Some(buf));
                };
                if c.read.is_some() {
                    return done(out, user_data, -EBUSY, Some(buf));
                }
                if buf.is_empty() {
                    return done(out, user_data, -EINVAL, Some(buf));
                }
                c.read = Some(PendingRead { buf, user_data });
                self.armed += 1;
                self.progress(conn, out);
            }
            Submission::Write {
                conn,
                buf,
                user_data,
            } => {
                let Some(c) = self.conn_mut(conn) else {
                    return done(out, user_data, -EBADF, Some(buf));
                };
                c.writes.push_back(PendingWrite {
                    buf,
                    written: 0,
                    user_data,
                });
                self.armed += 1;
                self.progress(conn, out);
            }
            Submission::Close { conn, user_data } => {
                let Some(c) = self.conns.get_mut(conn.0 as usize).and_then(Option::take) else {
                    return done(out, user_data, -EBADF, None);
                };
                if let Some(r) = c.read {
                    self.armed -= 1;
                    done(out, r.user_data, -ECANCELED, Some(r.buf));
                }
                for w in c.writes {
                    self.armed -= 1;
                    done(out, w.user_data, -ECANCELED, Some(w.buf));
                }
                let _ = self.poller.remove(source_of(&c.stream), conn.0 as u64);
                done(out, user_data, 0, None);
            }
        }
    }

    /// Waits up to `timeout` for readiness (or a wakeup) and makes all the
    /// progress it allows.
    pub(crate) fn poll(
        &mut self,
        timeout: Option<Duration>,
        out: &mut Vec<Completion>,
    ) -> io::Result<()> {
        self.events.clear();
        self.syscalls += self.poller.wait(&mut self.events, timeout)?;
        for i in 0..self.events.len() {
            let ev = self.events[i];
            match ev.token {
                LISTENER => {
                    if let Some(l) = self.listener.as_mut() {
                        l.ready = true;
                    }
                    self.progress_accept(out);
                }
                WAKE => {}
                token => {
                    let id = ConnId(token as u32);
                    if let Some(c) = self.conn_mut(id) {
                        c.readable |= ev.readable;
                        c.writable |= ev.writable;
                        self.progress(id, out);
                    }
                }
            }
        }
        Ok(())
    }

    fn conn_mut(&mut self, id: ConnId) -> Option<&mut Conn> {
        self.conns.get_mut(id.0 as usize)?.as_mut()
    }

    fn progress_accept(&mut self, out: &mut Vec<Completion>) {
        loop {
            let Some(l) = self.listener.as_mut() else {
                return;
            };
            if !l.ready || l.waiting.is_empty() {
                return;
            }
            self.syscalls += 1;
            match l.listener.accept() {
                Ok((stream, _)) => {
                    let user_data = l.waiting.pop_front().expect("checked non-empty");
                    self.armed -= 1;
                    match self.register(stream) {
                        Ok(id) => done(out, user_data, id.0 as i64, None),
                        Err(e) => done(out, user_data, -errno_of(&e), None),
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => l.ready = false,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    let user_data = l.waiting.pop_front().expect("checked non-empty");
                    self.armed -= 1;
                    done(out, user_data, -errno_of(&e), None);
                }
            }
        }
    }

    /// Drives a connection's writes (FIFO), then its read, as far as its
    /// cached readiness allows.
    fn progress(&mut self, id: ConnId, out: &mut Vec<Completion>) {
        let mut syscalls = 0;
        let before = out.len();
        let Some(c) = self.conns.get_mut(id.0 as usize).and_then(Option::as_mut) else {
            return;
        };
        while c.writable {
            let Some(w) = c.writes.front_mut() else { break };
            syscalls += 1;
            match c.stream.write(&w.buf[w.written..]) {
                Ok(0) => {
                    let w = c.writes.pop_front().expect("front exists");
                    done(out, w.user_data, -EPIPE, Some(w.buf));
                }
                Ok(n) => {
                    w.written += n;
                    if w.written == w.buf.len() {
                        let w = c.writes.pop_front().expect("front exists");
                        done(out, w.user_data, w.written as i64, Some(w.buf));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => c.writable = false,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    let w = c.writes.pop_front().expect("front exists");
                    done(out, w.user_data, -errno_of(&e), Some(w.buf));
                }
            }
        }
        while c.readable {
            let Some(mut r) = c.read.take() else { break };
            syscalls += 1;
            match c.stream.read(&mut r.buf) {
                Ok(n) => {
                    r.buf.truncate(n);
                    done(out, r.user_data, n as i64, Some(r.buf));
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    c.readable = false;
                    c.read = Some(r);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => c.read = Some(r),
                Err(e) => done(out, r.user_data, -errno_of(&e), Some(r.buf)),
            }
        }
        self.syscalls += syscalls;
        // Every completion made here finished an armed operation.
        self.armed -= out.len() - before;
    }
}
// @student-end

/// The readiness model, run inline on the caller's thread: every
/// operation is a syscall the caller makes, and a wakeup is a wait on the
/// poller. See the [module docs](super).
// @student w32
pub struct ReadinessBackend<P: Poller> {
    driver: Driver<P>,
    queue: VecDeque<Submission>,
    capacity: usize,
    ready: Vec<Completion>,
    stats: IoStats,
    name: &'static str,
}

// @student w32
impl<P: Poller> ReadinessBackend<P> {
    /// A backend on `poller` queueing up to `capacity` submissions between
    /// `submit_and_wait` calls.
    pub fn with_poller(poller: P, capacity: usize, name: &'static str) -> ReadinessBackend<P> {
        ReadinessBackend {
            driver: Driver::new(poller),
            queue: VecDeque::with_capacity(capacity),
            capacity: capacity.max(1),
            ready: Vec::with_capacity(capacity),
            stats: IoStats::default(),
            name,
        }
    }
}

// @student w32
impl ReadinessBackend<ScanPoller> {
    /// The portable polling backend.
    pub fn scan(capacity: usize) -> ReadinessBackend<ScanPoller> {
        ReadinessBackend::with_poller(ScanPoller::new(), capacity, "poll")
    }
}

// @student w32
impl<P: Poller> IoBackend for ReadinessBackend<P> {
    fn register(&mut self, stream: TcpStream) -> io::Result<ConnId> {
        self.driver.register(stream)
    }

    fn listen(&mut self, listener: TcpListener) -> io::Result<()> {
        self.driver.listen(listener)
    }

    fn push(&mut self, sub: Submission) -> Result<(), Submission> {
        if self.queue.len() >= self.capacity {
            return Err(sub);
        }
        self.queue.push_back(sub);
        self.stats.submitted += 1;
        Ok(())
    }

    fn submit_and_wait(
        &mut self,
        min_complete: usize,
        timeout: Option<Duration>,
    ) -> io::Result<usize> {
        self.stats.wakeups += 1;
        let before = self.driver.syscalls;
        while let Some(sub) = self.queue.pop_front() {
            self.driver.start(sub, &mut self.ready);
        }
        let deadline = timeout.map(|t| Instant::now() + t);
        while self.ready.len() < min_complete {
            let left = deadline.map(|d| d.saturating_duration_since(Instant::now()));
            if left.is_some_and(|l| l.is_zero()) {
                break;
            }
            self.driver.poll(left, &mut self.ready)?;
        }
        if min_complete == 0 && self.ready.is_empty() && self.driver.has_pending() {
            self.driver.poll(Some(Duration::ZERO), &mut self.ready)?;
        }
        self.stats.syscalls += self.driver.syscalls - before;
        Ok(self.ready.len())
    }

    fn drain(&mut self, out: &mut Vec<Completion>) -> usize {
        let n = self.ready.len();
        self.stats.completed += n as u64;
        out.append(&mut self.ready);
        n
    }

    fn stats(&self) -> IoStats {
        self.stats
    }

    fn name(&self) -> &'static str {
        self.name
    }
}
