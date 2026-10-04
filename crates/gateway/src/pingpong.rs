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
// @student w31
#[derive(Debug)]
pub struct EchoServer {
    addr: SocketAddr,
    handle: JoinHandle<io::Result<u64>>,
}

// @student w31
impl EchoServer {
    /// Listens on an ephemeral loopback port and serves the first
    /// connection from a plain thread (pinned to `cpu` if given), applying
    /// `tuning` to it.
    pub fn spawn(
        mode: RecvMode,
        tuning: SocketTuning,
        cpu: Option<usize>,
    ) -> io::Result<EchoServer> {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?;
        let addr = listener.local_addr()?;
        let handle = std::thread::Builder::new()
            .name("echo".into())
            .spawn(move || {
                pin(cpu);
                let (stream, _) = listener.accept()?;
                echo_std(stream, mode, tuning)
            })?;
        Ok(EchoServer { addr, handle })
    }

    /// Like [`spawn`](Self::spawn), but the connection is served by a
    /// current-thread Tokio runtime (`read().await` / `write_all().await`).
    pub fn spawn_tokio(tuning: SocketTuning, cpu: Option<usize>) -> io::Result<EchoServer> {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let handle = std::thread::Builder::new()
            .name("echo-tokio".into())
            .spawn(move || {
                pin(cpu);
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .build()?;
                rt.block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(listener)?;
                    let (stream, _) = listener.accept().await?;
                    echo_tokio(stream, tuning).await
                })
            })?;
        Ok(EchoServer { addr, handle })
    }

    /// The address to connect to.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Waits for the server to finish (it stops when the client closes the
    /// connection) and returns how many frames it echoed.
    pub fn join(self) -> io::Result<u64> {
        self.handle
            .join()
            .map_err(|_| io::Error::other("echo server panicked"))?
    }
}

/// The client side of the ping-pong.
// @student w31
#[derive(Debug)]
pub struct PingClient {
    stream: TcpStream,
    mode: RecvMode,
    decoder: FrameDecoder,
    rbuf: Vec<u8>,
    wbuf: Vec<u8>,
}

// @student w31
impl PingClient {
    /// Connects, applies `tuning` and sets the receive mode.
    pub fn connect(
        addr: SocketAddr,
        mode: RecvMode,
        tuning: SocketTuning,
    ) -> io::Result<PingClient> {
        let stream = TcpStream::connect(addr)?;
        configure(&stream, mode, tuning)?;
        Ok(PingClient {
            stream,
            mode,
            decoder: FrameDecoder::default(),
            rbuf: vec![0; 64 * 1024],
            wbuf: Vec::with_capacity(FRAME_HEADER_LEN + MAX_PING),
        })
    }

    /// Sends `payload` (1..=[`MAX_PING`] bytes) as one frame with one write
    /// and waits for the echo, which is copied into `echo`. An error if the
    /// server answers with anything but exactly one frame.
    pub fn ping(&mut self, payload: &[u8], echo: &mut Vec<u8>) -> io::Result<()> {
        self.wbuf.clear();
        self.wbuf
            .extend_from_slice(&(payload.len() as u16).to_le_bytes());
        self.wbuf.extend_from_slice(payload);
        write_all(&mut self.stream, &self.wbuf, self.mode)?;
        echo.clear();
        loop {
            let n = read(&mut self.stream, &mut self.rbuf, self.mode)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "server closed",
                ));
            }
            let mut input = &self.rbuf[..n];
            while let Some(frame) = self
                .decoder
                .next_frame(&mut input)
                .map_err(io::Error::other)?
            {
                if !echo.is_empty() || !input.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "more than one frame echoed",
                    ));
                }
                echo.extend_from_slice(frame);
            }
            if !echo.is_empty() {
                return Ok(());
            }
        }
    }
}

// @student-begin w31
fn pin(cpu: Option<usize>) {
    if let Some(cpu) = cpu {
        if let Err(e) = sync::pin_current_thread(cpu) {
            eprintln!("pingpong: cannot pin to cpu {cpu}: {e}");
        }
    }
}

fn configure(stream: &TcpStream, mode: RecvMode, tuning: SocketTuning) -> io::Result<()> {
    #[cfg(unix)]
    tuning.apply(stream)?;
    #[cfg(not(unix))]
    stream.set_nodelay(tuning.nodelay)?;
    stream.set_nonblocking(mode == RecvMode::BusyPoll)
}

/// One `read`; in busy-poll mode spins on `WouldBlock`.
fn read(stream: &mut TcpStream, buf: &mut [u8], mode: RecvMode) -> io::Result<usize> {
    loop {
        match stream.read(buf) {
            Ok(n) => return Ok(n),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock && mode == RecvMode::BusyPoll => {
                std::hint::spin_loop();
            }
            Err(e) => return Err(e),
        }
    }
}

/// `write_all` that also spins through `WouldBlock` on a non-blocking
/// socket.
fn write_all(stream: &mut TcpStream, mut bytes: &[u8], mode: RecvMode) -> io::Result<()> {
    while !bytes.is_empty() {
        match stream.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock && mode == RecvMode::BusyPoll => {
                std::hint::spin_loop();
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Appends every complete frame in `input` (header included) to `out`;
/// returns how many.
fn echo_frames(decoder: &mut FrameDecoder, mut input: &[u8], out: &mut Vec<u8>) -> io::Result<u64> {
    let mut n = 0;
    while let Some(frame) = decoder.next_frame(&mut input).map_err(io::Error::other)? {
        out.extend_from_slice(&(frame.len() as u16).to_le_bytes());
        out.extend_from_slice(frame);
        n += 1;
    }
    Ok(n)
}

fn echo_std(mut stream: TcpStream, mode: RecvMode, tuning: SocketTuning) -> io::Result<u64> {
    configure(&stream, mode, tuning)?;
    let mut decoder = FrameDecoder::default();
    let mut rbuf = vec![0u8; 64 * 1024];
    let mut wbuf = Vec::with_capacity(64 * 1024);
    let mut frames = 0;
    loop {
        let n = match read(&mut stream, &mut rbuf, mode) {
            Ok(0) => return Ok(frames),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::ConnectionReset => return Ok(frames),
            Err(e) => return Err(e),
        };
        wbuf.clear();
        frames += echo_frames(&mut decoder, &rbuf[..n], &mut wbuf)?;
        if !wbuf.is_empty() {
            write_all(&mut stream, &wbuf, mode)?;
        }
    }
}

async fn echo_tokio(stream: tokio::net::TcpStream, tuning: SocketTuning) -> io::Result<u64> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[cfg(unix)]
    tuning.apply(&stream)?;
    #[cfg(not(unix))]
    stream.set_nodelay(tuning.nodelay)?;
    let mut stream = stream;
    let mut decoder = FrameDecoder::default();
    let mut rbuf = vec![0u8; 64 * 1024];
    let mut wbuf = Vec::with_capacity(64 * 1024);
    let mut frames = 0;
    loop {
        let n = match stream.read(&mut rbuf).await {
            Ok(0) => return Ok(frames),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::ConnectionReset => return Ok(frames),
            Err(e) => return Err(e),
        };
        wbuf.clear();
        frames += echo_frames(&mut decoder, &rbuf[..n], &mut wbuf)?;
        if !wbuf.is_empty() {
            stream.write_all(&wbuf).await?;
        }
    }
}
// @student-end
