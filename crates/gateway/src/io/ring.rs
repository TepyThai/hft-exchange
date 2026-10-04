//! The simulated io_uring backend: SQ and CQ rings shared with a "kernel"
//! thread (the `SQPOLL` model).
//!
//! ```text
//!  caller thread                         kernel thread (Driver + poller)
//!  push ──▶ SQ ring (spsc, w01/w05) ──▶  pop a batch, start every op,
//!                                        poll readiness for armed ops
//!  drain ◀── CQ ring (spsc) ◀──────────  post completions (a batch)
//! ```
//!
//! * **No syscall per operation on the caller's thread.** `push` writes the
//!   ring; the kernel thread does the reads and writes.
//! * **Completions batch per wakeup.** While the caller handles one batch,
//!   the kernel thread keeps completing operations into the CQ; the next
//!   `submit_and_wait` finds them all at once.
//! * **Entering the kernel is rare.** Only two things cost the caller a
//!   (simulated) `io_uring_enter`: waking the kernel thread after it went
//!   to sleep for lack of work (`IORING_SQ_NEED_WAKEUP`), and blocking
//!   because no completion is ready. Both are counted in
//!   [`IoStats::syscalls`].
//! * **The CQ never drops.** If it is full, the kernel thread keeps the
//!   overflow and retries (Linux's `IORING_FEAT_NODROP`).
//!
//! Wake-up protocol (each side publishes its flag, then re-checks the
//! other side's state, with `SeqCst` ordering, so one of them always sees
//! the other): the kernel thread sets `kernel_sleeping` and re-checks the
//! SQ before sleeping; the caller pushes, then checks `kernel_sleeping`.
//! The caller sets `user_waiting` and re-checks the CQ before parking; the
//! kernel thread posts, then checks `user_waiting`.

use super::driver::Poller;
#[allow(unused_imports)] // used by the implementation the learner writes
use super::driver::{ScanPoller, Waker};
use super::{Completion, ConnId, IoBackend, IoStats, Submission};
use std::io;
use std::net::{TcpListener, TcpStream};
#[allow(unused_imports)] // used by the implementation the learner writes
use std::sync::atomic::{fence, AtomicBool, AtomicU64, Ordering};
#[allow(unused_imports)] // used by the implementation the learner writes
use std::sync::{Arc, Mutex};
#[allow(unused_imports)] // used by the implementation the learner writes
use std::time::{Duration, Instant};
#[allow(unused_imports)] // used by the implementation the learner writes
use sync::spsc;

/// Polls the kernel thread spins through, finding no work, before it
/// sleeps (`sq_thread_idle`, in iterations rather than milliseconds).
pub const KERNEL_IDLE_SPINS: u32 = 20_000;
/// Polls the caller spins through, waiting for a completion, before it
/// parks.
pub const USER_SPINS: u32 = 2_000;

/// The simulated io_uring backend (see the module docs).
// @student w32
pub struct RingBackend {
    sq: spsc::Producer<Sqe>,
    cq: spsc::Consumer<Completion>,
    shared: Arc<Shared>,
    waker: Waker,
    kernel: Option<std::thread::JoinHandle<()>>,
    /// Shared with the kernel thread's driver (accepted connections take
    /// ids from it too).
    ids: Arc<std::sync::atomic::AtomicU32>,
    /// A wakeup the caller owes the kernel thread (pushed since it slept).
    pushed: bool,
    stats: IoStats,
}

// @student-begin w32
use super::driver::Driver;

/// A submission-queue entry: an operation, or a control message.
enum Sqe {
    Op(Submission),
    Register(TcpStream, ConnId),
    Listen(TcpListener),
}

struct Shared {
    stop: AtomicBool,
    kernel_sleeping: AtomicBool,
    user_waiting: AtomicBool,
    user: Mutex<Option<std::thread::Thread>>,
    kernel_syscalls: AtomicU64,
}

/// SQ entries the kernel thread takes per pass.
const KERNEL_BATCH: usize = 256;
/// How long the sleeping kernel thread waits before re-checking `stop`.
const KERNEL_SLEEP: Duration = Duration::from_millis(50);

fn kernel_loop<P: Poller>(
    mut driver: Driver<P>,
    mut sq: spsc::Consumer<Sqe>,
    mut cq: spsc::Producer<Completion>,
    shared: Arc<Shared>,
) {
    let mut batch: Vec<Sqe> = Vec::with_capacity(KERNEL_BATCH);
    let mut done: Vec<Completion> = Vec::with_capacity(KERNEL_BATCH);
    let mut overflow: std::collections::VecDeque<Completion> = std::collections::VecDeque::new();
    let mut idle: u32 = 0;
    let mut unparks: u64 = 0;
    while !shared.stop.load(Ordering::Acquire) {
        let mut work = false;
        if sq.pop_batch(&mut batch, KERNEL_BATCH) > 0 {
            work = true;
            for sqe in batch.drain(..) {
                match sqe {
                    Sqe::Op(sub) => driver.start(sub, &mut done),
                    // The caller took the id from the shared allocator.
                    Sqe::Register(stream, id) => {
                        let _ = driver.register_as(stream, id);
                    }
                    Sqe::Listen(listener) => {
                        let _ = driver.listen(listener);
                    }
                }
            }
        }
        if driver.has_pending() {
            // Readiness of armed operations, without blocking.
            if driver.poll(Some(Duration::ZERO), &mut done).is_err() {
                break;
            }
        }
        overflow.extend(done.drain(..));
        let mut posted = false;
        while let Some(c) = overflow.pop_front() {
            match cq.try_push(c) {
                Ok(()) => posted = true,
                Err(c) => {
                    overflow.push_front(c);
                    break;
                }
            }
        }
        if posted {
            work = true;
            fence(Ordering::SeqCst);
            if shared.user_waiting.load(Ordering::SeqCst) {
                if let Some(t) = shared
                    .user
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                {
                    t.unpark();
                }
                unparks += 1;
            }
        }
        shared
            .kernel_syscalls
            .store(driver.syscalls + unparks, Ordering::Relaxed);
        if work || !overflow.is_empty() {
            idle = 0;
            continue;
        }
        idle += 1;
        if idle < KERNEL_IDLE_SPINS {
            std::hint::spin_loop();
            continue;
        }
        // Nothing to do: sleep in the poller until a socket is ready or the
        // caller wakes us.
        shared.kernel_sleeping.store(true, Ordering::SeqCst);
        if sq.is_empty()
            && !shared.stop.load(Ordering::SeqCst)
            && driver.poll(Some(KERNEL_SLEEP), &mut done).is_err()
        {
            break;
        }
        shared.kernel_sleeping.store(false, Ordering::SeqCst);
        idle = 0;
    }
    shared
        .kernel_syscalls
        .store(driver.syscalls + unparks, Ordering::Relaxed);
}
impl RingBackend {
    /// Pushes a control entry, spinning while the SQ is full (setup only).
    fn push_control(&mut self, mut sqe: Sqe) {
        loop {
            match self.sq.try_push(sqe) {
                Ok(()) => break,
                Err(back) => {
                    sqe = back;
                    self.wake_kernel();
                    std::thread::yield_now();
                }
            }
        }
        self.pushed = true;
    }

    /// `io_uring_enter(IORING_ENTER_SQ_WAKEUP)` if the kernel thread sleeps.
    fn wake_kernel(&mut self) {
        fence(Ordering::SeqCst);
        if self.shared.kernel_sleeping.load(Ordering::SeqCst) {
            let _ = self.waker.wake();
            self.stats.syscalls += 1;
        }
    }
}
// @student-end

// @student w32
impl RingBackend {
    /// A ring backend whose kernel thread uses `poller`, with room for
    /// `entries` submissions (rounded up to a power of two) and twice that
    /// many completions (the kernel's default ratio). Spawns the kernel
    /// thread.
    pub fn with_poller<P: Poller>(mut poller: P, entries: usize) -> io::Result<RingBackend> {
        let waker = poller.waker()?;
        let (sq, sq_rx) = spsc::channel::<Sqe>(entries.max(2));
        let (cq_tx, cq) = spsc::channel::<Completion>(2 * entries.max(2));
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            kernel_sleeping: AtomicBool::new(false),
            user_waiting: AtomicBool::new(false),
            user: Mutex::new(None),
            kernel_syscalls: AtomicU64::new(0),
        });
        let driver = Driver::new(poller);
        let ids = driver.ids();
        let kernel = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("ring-kernel".into())
                .spawn(move || kernel_loop(driver, sq_rx, cq_tx, shared))?
        };
        Ok(RingBackend {
            sq,
            cq,
            shared,
            waker,
            kernel: Some(kernel),
            ids,
            pushed: false,
            stats: IoStats::default(),
        })
    }

    /// The ring backend on this platform's best poller: epoll on Linux,
    /// the scan poller elsewhere.
    pub fn new(entries: usize) -> io::Result<RingBackend> {
        #[cfg(target_os = "linux")]
        {
            RingBackend::with_poller(super::EpollPoller::new()?, entries)
        }
        #[cfg(not(target_os = "linux"))]
        {
            RingBackend::with_poller(ScanPoller::new(), entries)
        }
    }

    /// The ring backend with the portable scan poller.
    pub fn scan(entries: usize) -> io::Result<RingBackend> {
        RingBackend::with_poller(ScanPoller::new(), entries)
    }
}

// @student w32
impl IoBackend for RingBackend {
    fn register(&mut self, stream: TcpStream) -> io::Result<ConnId> {
        stream.set_nonblocking(true)?;
        let id = ConnId(self.ids.fetch_add(1, Ordering::Relaxed));
        self.push_control(Sqe::Register(stream, id));
        Ok(id)
    }

    fn listen(&mut self, listener: TcpListener) -> io::Result<()> {
        listener.set_nonblocking(true)?;
        self.push_control(Sqe::Listen(listener));
        Ok(())
    }

    fn push(&mut self, sub: Submission) -> Result<(), Submission> {
        match self.sq.try_push(Sqe::Op(sub)) {
            Ok(()) => {
                self.pushed = true;
                self.stats.submitted += 1;
                Ok(())
            }
            Err(Sqe::Op(sub)) => Err(sub),
            Err(_) => unreachable!("pushed an Op"),
        }
    }

    fn submit_and_wait(
        &mut self,
        min_complete: usize,
        timeout: Option<Duration>,
    ) -> io::Result<usize> {
        self.stats.wakeups += 1;
        if std::mem::take(&mut self.pushed) {
            self.wake_kernel();
        }
        let deadline = timeout.map(|t| Instant::now() + t);
        let mut spins = 0;
        loop {
            let ready = self.cq.len();
            if ready >= min_complete {
                return Ok(ready);
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Ok(ready);
            }
            if spins < USER_SPINS {
                spins += 1;
                std::hint::spin_loop();
                continue;
            }
            // Block for completions: one io_uring_enter(GETEVENTS).
            *self.shared.user.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(std::thread::current());
            self.shared.user_waiting.store(true, Ordering::SeqCst);
            fence(Ordering::SeqCst);
            if self.cq.len() < min_complete {
                self.stats.syscalls += 1;
                // The kernel thread may be asleep with work we owe it.
                self.wake_kernel();
                let left = deadline.map_or(Duration::from_millis(10), |d| {
                    d.saturating_duration_since(Instant::now())
                });
                std::thread::park_timeout(left.min(Duration::from_millis(10)));
            }
            self.shared.user_waiting.store(false, Ordering::SeqCst);
        }
    }

    fn drain(&mut self, out: &mut Vec<Completion>) -> usize {
        let start = out.len();
        while let Some(c) = self.cq.try_pop() {
            out.push(c);
        }
        let n = out.len() - start;
        self.stats.completed += n as u64;
        n
    }

    fn stats(&self) -> IoStats {
        IoStats {
            kernel_syscalls: self.shared.kernel_syscalls.load(Ordering::Relaxed),
            ..self.stats
        }
    }

    fn name(&self) -> &'static str {
        "ring"
    }
}

// @student w32
impl Drop for RingBackend {
    /// Stops and joins the kernel thread (in-flight operations are
    /// abandoned; their sockets close with it).
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        let _ = self.waker.wake();
        if let Some(k) = self.kernel.take() {
            let _ = k.join();
        }
    }
}
