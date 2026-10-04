//! `epoll(7)` through a handful of hand-declared syscalls (std has no
//! epoll, and the workspace takes no `libc` dependency).

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
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
#[derive(Debug)]
pub struct EpollPoller {
    // TODO(w32): design this type's fields.
    _todo: (),
}

// TODO(w32): implement — see the week guide on tepy.dev/hft.

#[allow(unused_variables)]
impl EpollPoller {
    /// A new epoll instance (one `epoll_create1`).
    pub fn new() -> io::Result<EpollPoller> {
        todo!("w32: new")
    }
}

#[allow(unused_variables)]
impl Poller for EpollPoller {
    fn add(&mut self, source: Source, token: u64) -> io::Result<()> {
        todo!("w32: add")
    }

    fn remove(&mut self, source: Source, token: u64) -> io::Result<()> {
        todo!("w32: remove")
    }

    fn wait(&mut self, events: &mut Vec<Ready>, timeout: Option<Duration>) -> io::Result<u64> {
        todo!("w32: wait")
    }

    fn waker(&mut self) -> io::Result<Waker> {
        todo!("w32: waker")
    }
}
