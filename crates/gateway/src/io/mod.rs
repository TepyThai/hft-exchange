//! Week 32: batched I/O behind a submission/completion interface.
//!
//! The interface is io_uring's shape even when the backend is not: the
//! caller **pushes** [`Submission`]s (no syscall), calls
//! [`IoBackend::submit_and_wait`] once per loop iteration (one *wakeup*),
//! and **drains** [`Completion`]s tagged with the `user_data` it chose.
//! Buffers move into the backend with the submission and come back with
//! the completion — the kernel owns a buffer while an operation is in
//! flight, so a borrowed `&mut [u8]` could not express it.
//!
//! Two backends implement it:
//!
//! * [`ReadinessBackend`] — the classic readiness loop, run inline on the
//!   caller's thread: try each operation, and when it would block, wait in
//!   `epoll_wait` (Linux, [`EpollPoller`]) or by polling every socket
//!   ([`ScanPoller`], the portable fallback) for the socket to become ready,
//!   then do the `read`/`write` yourself. One syscall per operation plus
//!   one per wakeup. [`EpollBackend`] and [`PollBackend`] are the two
//!   flavours.
//! * [`RingBackend`] — a simulated io_uring with `SQPOLL`: a submission
//!   ring and a completion ring (the week-1/5 SPSC rings) shared with a
//!   "kernel" thread that performs the operations. The caller's thread
//!   makes no syscall at all while the kernel thread is awake and
//!   completions are flowing; under load, completions pile up in the ring
//!   while the caller is busy, so each wakeup drains a batch.
//!
//! [`echo_loop`] is the same session code (frame, count, echo back,
//! coalescing one write per connection per wakeup) running on any backend;
//! the week-32 benchmark compares messages per wakeup and throughput.
//!
//! Semantics shared by both backends:
//!
//! * `Read` completes with the bytes available, at most the buffer's
//!   length (a short read is normal), `0` at end of stream.
//! * `Write` completes only when the whole buffer is written: the backend
//!   resumes partial writes itself.
//! * Operations on one connection complete in submission order (one read
//!   in flight per connection; writes FIFO). Across connections, order
//!   within a wakeup is unspecified — exactly as with io_uring.
//! * Errors are negative `errno` values in [`Completion::result`]
//!   ([`EBADF`], [`ECANCELED`], …), like io_uring's `cqe->res`.

use std::io;
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

mod driver;
mod echo;
mod load;
mod ring;
#[cfg(target_os = "linux")]
mod sys;

pub use driver::{Poller, ReadinessBackend, Ready, ScanPoller, Source, Waker, SCAN_INTERVAL};
pub use echo::{echo_loop, EchoOptions, EchoStats};
pub use load::EchoLoad;
pub use ring::{RingBackend, KERNEL_IDLE_SPINS, USER_SPINS};
#[cfg(target_os = "linux")]
pub use sys::EpollPoller;

/// The readiness backend on `epoll(7)` (Linux).
#[cfg(target_os = "linux")]
pub type EpollBackend = ReadinessBackend<EpollPoller>;

/// The readiness backend on the portable polling fallback.
pub type PollBackend = ReadinessBackend<ScanPoller>;

/// A connection registered with a backend. Ids are never reused within a
/// backend, so a late completion can't be mistaken for a new connection's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConnId(pub u32);

/// An operation for the backend to perform.
#[derive(Debug)]
pub enum Submission {
    /// Accept one connection on the backend's listener; completes with the
    /// new connection's id in `result`.
    Accept {
        /// Returned in the completion.
        user_data: u64,
    },
    /// Read up to `buf.len()` bytes; the completion returns `buf`
    /// truncated to the bytes read.
    Read {
        /// The connection.
        conn: ConnId,
        /// Owned by the backend until the completion.
        buf: Vec<u8>,
        /// Returned in the completion.
        user_data: u64,
    },
    /// Write all of `buf`; the completion returns it for reuse.
    Write {
        /// The connection.
        conn: ConnId,
        /// Owned by the backend until the completion.
        buf: Vec<u8>,
        /// Returned in the completion.
        user_data: u64,
    },
    /// Close the connection. Its pending operations complete first with
    /// [`ECANCELED`]; then this one completes with `0`.
    Close {
        /// The connection.
        conn: ConnId,
        /// Returned in the completion.
        user_data: u64,
    },
}

impl Submission {
    /// The submission's tag.
    pub fn user_data(&self) -> u64 {
        match *self {
            Submission::Accept { user_data }
            | Submission::Read { user_data, .. }
            | Submission::Write { user_data, .. }
            | Submission::Close { user_data, .. } => user_data,
        }
    }
}

/// A finished operation.
#[derive(Debug, PartialEq, Eq)]
pub struct Completion {
    /// The submission's tag.
    pub user_data: u64,
    /// Bytes read or written, the new connection id for `Accept`, `0` for
    /// `Close` — or a negative `errno`.
    pub result: i64,
    /// The submission's buffer (`Read`: holding the bytes read).
    pub buf: Option<Vec<u8>>,
}

impl Completion {
    /// The error, if `result` is negative.
    pub fn error(&self) -> Option<i64> {
        (self.result < 0).then_some(-self.result)
    }
}

/// `-EBADF`: no such connection (never registered, or closed).
pub const EBADF: i64 = 9;
/// `-EIO`: an I/O error with no closer match.
pub const EIO: i64 = 5;
/// `-EBUSY`: a second read submitted while one is in flight.
pub const EBUSY: i64 = 16;
/// `-EINVAL`: `Accept` without a listener, or an empty buffer.
pub const EINVAL: i64 = 22;
/// `-EPIPE`: writing to a connection the peer closed.
pub const EPIPE: i64 = 32;
/// `-ECONNRESET`: the peer reset the connection.
pub const ECONNRESET: i64 = 104;
/// `-ECANCELED`: the connection was closed with the operation pending.
pub const ECANCELED: i64 = 125;

/// The `errno` for an I/O error (its raw OS code when it has one).
pub fn errno_of(e: &io::Error) -> i64 {
    if let Some(code) = e.raw_os_error() {
        return code as i64;
    }
    match e.kind() {
        io::ErrorKind::ConnectionReset => ECONNRESET,
        io::ErrorKind::BrokenPipe | io::ErrorKind::WriteZero => EPIPE,
        io::ErrorKind::InvalidInput => EINVAL,
        _ => EIO,
    }
}

/// Counters a backend keeps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IoStats {
    /// Submissions accepted by `push`.
    pub submitted: u64,
    /// Completions handed out by `drain`.
    pub completed: u64,
    /// `submit_and_wait` calls (the caller's loop iterations).
    pub wakeups: u64,
    /// Syscalls made **on the caller's thread** (readiness backends: every
    /// `epoll_wait`, `read`, `write`, `accept`, including `EAGAIN` probes;
    /// ring backend: the simulated `io_uring_enter`s — waking the kernel
    /// thread or blocking for completions).
    pub syscalls: u64,
    /// Syscalls the ring backend's kernel thread made on the caller's
    /// behalf (in a real io_uring they are kernel work, not syscalls).
    pub kernel_syscalls: u64,
}

/// A submission/completion I/O backend (see the module docs).
pub trait IoBackend {
    /// Adds a connected stream (it is switched to non-blocking mode).
    fn register(&mut self, stream: TcpStream) -> io::Result<ConnId>;

    /// Sets the listener `Accept` submissions use.
    fn listen(&mut self, listener: TcpListener) -> io::Result<()>;

    /// Queues `sub` without a syscall; hands it back if the submission
    /// queue is full (call [`submit_and_wait`](Self::submit_and_wait)
    /// first).
    fn push(&mut self, sub: Submission) -> Result<(), Submission>;

    /// Submits everything queued and waits until at least `min_complete`
    /// completions are ready to drain or `timeout` passes (`None`: no
    /// limit). Returns how many are ready. One wakeup of the caller's loop.
    fn submit_and_wait(
        &mut self,
        min_complete: usize,
        timeout: Option<Duration>,
    ) -> io::Result<usize>;

    /// Moves every ready completion into `out`; returns how many. No
    /// syscall.
    fn drain(&mut self, out: &mut Vec<Completion>) -> usize;

    /// Counters so far.
    fn stats(&self) -> IoStats;

    /// The backend's name for reports.
    fn name(&self) -> &'static str;
}
