//! `epoll(7)` through a handful of hand-declared syscalls (std has no
//! epoll, and the workspace takes no `libc` dependency).

use super::driver::{Poller, Ready, Source, Waker};
#[allow(unused_imports)] // used by the implementation the learner writes
use super::driver::{WakeHandle, WAKE};
use std::io;
#[allow(unused_imports)] // used by the implementation the learner writes
use std::io::Read;
#[allow(unused_imports)] // used by the implementation the learner writes
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
#[allow(unused_imports)] // used by the implementation the learner writes
use std::sync::Arc;
use std::time::Duration;

/// Readiness from `epoll`, edge-triggered (`EPOLLET`): a socket is
/// reported when it becomes readable or writable, once, and the driver
/// keeps track until an operation returns `WouldBlock`. One `epoll_wait`
/// returns up to 256 ready sockets — that is the readiness model's batch.
// @student w32
#[derive(Debug)]
pub struct EpollPoller {
    epfd: OwnedFd,
    buf: Vec<EpollEvent>,
    waker: Option<(Arc<UnixStream>, UnixStream)>,
}

// @student-begin w32
/// `struct epoll_event` — packed on x86-64 (a historical ABI quirk: 12
/// bytes there, 16 elsewhere).
#[derive(Clone, Copy, Debug, Default)]
#[cfg_attr(target_arch = "x86_64", repr(C, packed))]
#[cfg_attr(not(target_arch = "x86_64"), repr(C))]
struct EpollEvent {
    events: u32,
    data: u64,
}

const EPOLL_CLOEXEC: i32 = 0o2_000_000;
const EPOLL_CTL_ADD: i32 = 1;
const EPOLL_CTL_DEL: i32 = 2;
const EPOLLIN: u32 = 0x001;
const EPOLLOUT: u32 = 0x004;
const EPOLLERR: u32 = 0x008;
const EPOLLHUP: u32 = 0x010;
const EPOLLRDHUP: u32 = 0x2000;
const EPOLLET: u32 = 1 << 31;
const EINTR: i32 = 4;

extern "C" {
    /// `int epoll_create1(int flags);`
    fn epoll_create1(flags: i32) -> i32;
    /// `int epoll_ctl(int epfd, int op, int fd, struct epoll_event *event);`
    fn epoll_ctl(epfd: i32, op: i32, fd: i32, event: *mut EpollEvent) -> i32;
    /// `int epoll_wait(int epfd, struct epoll_event *events, int maxevents, int timeout);`
    fn epoll_wait(epfd: i32, events: *mut EpollEvent, maxevents: i32, timeout: i32) -> i32;
}

fn ctl(epfd: i32, op: i32, fd: i32, token: u64) -> io::Result<()> {
    let mut ev = EpollEvent {
        events: EPOLLIN | EPOLLOUT | EPOLLRDHUP | EPOLLET,
        data: token,
    };
    // SAFETY: `ev` is a valid, initialized `epoll_event` that lives across
    // the call (the kernel only reads it; for DEL it is ignored). Bad file
    // descriptors are reported through the return value.
    let rc = unsafe { epoll_ctl(epfd, op, fd, &mut ev) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Milliseconds for `epoll_wait`, rounded up so a short timeout still
/// waits; `-1` waits forever.
fn timeout_ms(timeout: Option<Duration>) -> i32 {
    match timeout {
        None => -1,
        Some(d) => d.as_nanos().div_ceil(1_000_000).min(i32::MAX as u128) as i32,
    }
}
// @student-end

// @student w32
impl EpollPoller {
    /// A new epoll instance (one `epoll_create1`).
    pub fn new() -> io::Result<EpollPoller> {
        // SAFETY: plain syscall with a flags argument; no pointers.
        let fd = unsafe { epoll_create1(EPOLL_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a freshly created descriptor nobody else owns;
        // `OwnedFd` closes it exactly once on drop.
        let epfd = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok(EpollPoller {
            epfd,
            buf: vec![EpollEvent::default(); 256],
            waker: None,
        })
    }
}

// @student w32
impl Poller for EpollPoller {
    fn add(&mut self, source: Source, token: u64) -> io::Result<()> {
        ctl(self.epfd.as_raw_fd(), EPOLL_CTL_ADD, source, token)
    }

    fn remove(&mut self, source: Source, token: u64) -> io::Result<()> {
        ctl(self.epfd.as_raw_fd(), EPOLL_CTL_DEL, source, token)
    }

    fn wait(&mut self, events: &mut Vec<Ready>, timeout: Option<Duration>) -> io::Result<u64> {
        // SAFETY: `buf` is a live array of `buf.len()` initialized
        // `epoll_event`s and we pass exactly that length, so the kernel
        // writes only within it.
        let n = unsafe {
            epoll_wait(
                self.epfd.as_raw_fd(),
                self.buf.as_mut_ptr(),
                self.buf.len() as i32,
                timeout_ms(timeout),
            )
        };
        if n < 0 {
            let e = io::Error::last_os_error();
            return if e.raw_os_error() == Some(EINTR) {
                Ok(1)
            } else {
                Err(e)
            };
        }
        let mut syscalls = 1;
        for ev in &self.buf[..n as usize] {
            // Copy the (possibly unaligned) fields out of the packed struct.
            let (bits, token) = (ev.events, ev.data);
            if token == WAKE {
                if let Some((_, rx)) = &mut self.waker {
                    let mut sink = [0u8; 64];
                    syscalls += 1;
                    while matches!(rx.read(&mut sink), Ok(n) if n > 0) {
                        syscalls += 1;
                    }
                }
                continue;
            }
            let err = bits & (EPOLLERR | EPOLLHUP) != 0;
            events.push(Ready {
                token,
                readable: err || bits & (EPOLLIN | EPOLLRDHUP) != 0,
                writable: err || bits & EPOLLOUT != 0,
            });
        }
        Ok(syscalls)
    }

    fn waker(&mut self) -> io::Result<Waker> {
        if let Some((tx, _)) = &self.waker {
            return Ok(Waker(WakeHandle::Socket(Arc::clone(tx))));
        }
        let (tx, rx) = UnixStream::pair()?;
        tx.set_nonblocking(true)?;
        rx.set_nonblocking(true)?;
        ctl(self.epfd.as_raw_fd(), EPOLL_CTL_ADD, rx.as_raw_fd(), WAKE)?;
        let tx = Arc::new(tx);
        self.waker = Some((Arc::clone(&tx), rx));
        Ok(Waker(WakeHandle::Socket(tx)))
    }
}
