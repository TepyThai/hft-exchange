//! Week 31: a loopback TCP ping-pong at the gateway's framing layer.
//!
//! A client sends one frame (`u16` length + payload, the week-11 framing)
//! and waits for the server to echo it back; the round trip is the kernel's
//! TCP path twice plus the wakeups on both ends — no engine. Two receive
//! modes:
//!
//! * [`RecvMode::Blocking`] — `read` sleeps until data arrives; every
//!   message pays a thread wakeup on each side.
//! * [`RecvMode::BusyPoll`] — the socket is non-blocking and the thread
//!   spins on `read` (each empty poll is a syscall returning `EAGAIN`):
//!   no sleep, no wakeup, one core burned per side.
//!
//! The server comes in two flavours: a plain thread ([`EchoServer::spawn`])
//! and a single-threaded Tokio runtime ([`EchoServer::spawn_tokio`]), the
//! gateway's I/O model. A ping-pong is closed-loop by nature: it measures
//! per-message latency at low load, not behaviour under load.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::net::SocketTuning;
use std::io;
#[allow(unused_imports)] // used by the implementation the learner writes
use std::io::{Read, Write};
#[allow(unused_imports)] // used by the implementation the learner writes
use std::net::{SocketAddr, TcpListener, TcpStream};
#[allow(unused_imports)] // used by the implementation the learner writes
use std::thread::JoinHandle;
#[allow(unused_imports)] // used by the implementation the learner writes
use wire::{FrameDecoder, FRAME_HEADER_LEN};

/// How a ping-pong endpoint waits for data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecvMode {
    /// Blocking `read`: sleep until data arrives.
    Blocking,
    /// Non-blocking `read` in a spin loop.
    BusyPoll,
}

/// Largest ping payload (the frame decoder's limit).
pub const MAX_PING: usize = wire::MAX_FRAME_LEN;

/// An echo server on its own thread serving one connection: every complete
/// frame it reads is written back (all frames of one read in one write).
#[derive(Debug)]
pub struct EchoServer {
    // TODO(w31): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl EchoServer {
    /// Listens on an ephemeral loopback port and serves the first
    /// connection from a plain thread (pinned to `cpu` if given), applying
    /// `tuning` to it.
    pub fn spawn(
        mode: RecvMode,
        tuning: SocketTuning,
        cpu: Option<usize>,
    ) -> io::Result<EchoServer> {
        todo!("w31: spawn")
    }

    /// Like [`spawn`](Self::spawn), but the connection is served by a
    /// current-thread Tokio runtime (`read().await` / `write_all().await`).
    pub fn spawn_tokio(tuning: SocketTuning, cpu: Option<usize>) -> io::Result<EchoServer> {
        todo!("w31: spawn_tokio")
    }

    /// The address to connect to.
    pub fn addr(&self) -> SocketAddr {
        todo!("w31: addr")
    }

    /// Waits for the server to finish (it stops when the client closes the
    /// connection) and returns how many frames it echoed.
    pub fn join(self) -> io::Result<u64> {
        todo!("w31: join")
    }
}

/// The client side of the ping-pong.
#[derive(Debug)]
pub struct PingClient {
    // TODO(w31): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl PingClient {
    /// Connects, applies `tuning` and sets the receive mode.
    pub fn connect(
        addr: SocketAddr,
        mode: RecvMode,
        tuning: SocketTuning,
    ) -> io::Result<PingClient> {
        todo!("w31: connect")
    }

    /// Sends `payload` (1..=[`MAX_PING`] bytes) as one frame with one write
    /// and waits for the echo, which is copied into `echo`. An error if the
    /// server answers with anything but exactly one frame.
    pub fn ping(&mut self, payload: &[u8], echo: &mut Vec<u8>) -> io::Result<()> {
        todo!("w31: ping")
    }
}

// TODO(w31): implement — see the week guide on tepy.dev/hft.
