//! Load for an echo server: provided infrastructure for the week-32 tests
//! and benchmark (not part of the week's task).

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::thread::JoinHandle;
use types::Rng;
use wire::FrameDecoder;

/// `clients` blocking connections, each keeping up to `window` frames in
/// flight until it has had `frames` frames echoed. Payloads are random
/// (from `seed`), 1..=`max_payload` bytes; every echoed byte is checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EchoLoad {
    /// Connections.
    pub clients: usize,
    /// Frames in flight per connection (1 = ping-pong).
    pub window: usize,
    /// Frames each connection sends.
    pub frames: u64,
    /// Largest payload (at most `wire::MAX_FRAME_LEN`).
    pub max_payload: usize,
    /// Seed for the payloads.
    pub seed: u64,
}

impl EchoLoad {
    /// Total frames across all clients.
    pub fn total(&self) -> u64 {
        self.clients as u64 * self.frames
    }

    /// Starts one thread per client; each connects to `addr`, runs, then
    /// half-closes its connection and returns the frames echoed to it.
    pub fn start(&self, addr: SocketAddr) -> io::Result<Vec<JoinHandle<io::Result<u64>>>> {
        (0..self.clients)
            .map(|i| {
                let load = *self;
                std::thread::Builder::new()
                    .name(format!("echo-client-{i}"))
                    .spawn(move || load.client(addr, i as u64))
            })
            .collect()
    }

    fn client(self, addr: SocketAddr, index: u64) -> io::Result<u64> {
        let mut s = TcpStream::connect(addr)?;
        s.set_nodelay(true)?;
        s.set_read_timeout(Some(std::time::Duration::from_secs(20)))?;
        let mut rng = Rng::new(self.seed ^ (index.wrapping_mul(0x9E37_79B9_7F4A_7C15)));
        let mut expected: VecDeque<u8> = VecDeque::new();
        let mut out = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut decoder = FrameDecoder::default();
        let (mut sent, mut echoed) = (0u64, 0u64);
        while echoed < self.frames {
            out.clear();
            while sent < self.frames && (sent - echoed) < self.window as u64 {
                let len =
                    rng.range(1, self.max_payload.clamp(1, wire::MAX_FRAME_LEN) as u64) as usize;
                out.extend_from_slice(&(len as u16).to_le_bytes());
                for _ in 0..len {
                    out.push(rng.next_u64() as u8);
                }
                sent += 1;
            }
            if !out.is_empty() {
                s.write_all(&out)?;
                expected.extend(out.iter().copied());
            }
            let n = s.read(&mut buf)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "server closed",
                ));
            }
            for &b in &buf[..n] {
                if expected.pop_front() != Some(b) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "echo differs from what was sent",
                    ));
                }
            }
            let mut input = &buf[..n];
            while decoder
                .next_frame(&mut input)
                .map_err(io::Error::other)?
                .is_some()
            {
                echoed += 1;
            }
        }
        s.shutdown(std::net::Shutdown::Write)?;
        Ok(echoed)
    }
}
