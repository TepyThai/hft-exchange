//! Week 31: socket tuning and vectored writes.
//!
//! **Socket options.** [`SocketTuning`] is the gateway's deliberate list:
//!
//! * `TCP_NODELAY` — always. Nagle's algorithm holds a small write while
//!   earlier data is unacknowledged; with the peer's delayed ACK that is a
//!   40 ms stall on Linux. We do our own batching (one write per batch of
//!   reports), so Nagle only ever adds delay.
//! * `SO_SNDBUF` / `SO_RCVBUF` — modest. A socket buffer is a queue: a slow
//!   session should be detected by the gateway's bounded outbound queue
//!   (and disconnected), not hidden for milliseconds in a 4 MB kernel
//!   buffer. Linux doubles the value you set (bookkeeping overhead), caps
//!   it at `net.core.{w,r}mem_max`, and setting it turns off autotuning.
//! * `SO_KEEPALIVE` with a short idle time — a backstop for half-open
//!   connections when a client vanishes without a FIN. Application
//!   heartbeats remain the real liveness check.
//! * `SO_BUSY_POLL` — optional (raising it needs `CAP_NET_ADMIN`).
//! * `SO_LINGER` — deliberately **not** set: a non-zero linger makes
//!   `close` block the thread, and zero linger turns every close into a
//!   RST that discards unsent reports. Graceful close is `shutdown` (FIN).
//!
//! Options are applied to every accepted socket explicitly — never assumed
//! inherited from the listener — and can be read back with
//! [`socket_options`]. std and Tokio expose `TCP_NODELAY` and, on a
//! not-yet-connected [`tokio::net::TcpSocket`], the buffer sizes; for an
//! accepted socket the rest needs `setsockopt(2)`, declared below through a
//! few lines of FFI (no `libc` crate).
//!
//! **Vectored writes.** [`write_all_vectored`] writes many buffers with as
//! few `writev(2)` calls as possible and handles partial writes: a call may
//! accept fewer bytes than offered, ending in the middle of a buffer, and
//! the next call must resume exactly there. [`FrameBatch`] encodes a batch
//! of reports into one frame per slot and writes them all with one call.

use std::io::{self, IoSlice, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::time::Duration;
#[allow(unused_imports)] // used by the implementation the learner writes
use tokio::io::{AsyncWrite, AsyncWriteExt};
#[allow(unused_imports)] // used by the implementation the learner writes
use wire::{encode_frame, Message, FRAME_HEADER_LEN, MAX_MSG_LEN};

/// Socket options for latency-sensitive TCP sessions (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SocketTuning {
    /// Set `TCP_NODELAY` (disable Nagle).
    pub nodelay: bool,
    /// Requested `SO_RCVBUF` in bytes (the kernel doubles it). `None`:
    /// kernel default with autotuning.
    pub recv_buffer: Option<u32>,
    /// Requested `SO_SNDBUF` in bytes (the kernel doubles it).
    pub send_buffer: Option<u32>,
    /// Enable `SO_KEEPALIVE` and start probing after this much idle time
    /// (`TCP_KEEPIDLE`; whole seconds, at least 1).
    pub keepalive_idle: Option<Duration>,
    /// `SO_BUSY_POLL` in microseconds (Linux; needs `CAP_NET_ADMIN`).
    pub busy_poll_us: Option<u32>,
}

impl SocketTuning {
    /// The gateway's choice: no Nagle, 64 KiB send / 128 KiB receive
    /// requested (128 / 256 KiB effective on Linux), keepalive after 30 s.
    pub const fn latency() -> SocketTuning {
        SocketTuning {
            nodelay: true,
            recv_buffer: Some(128 * 1024),
            send_buffer: Some(64 * 1024),
            keepalive_idle: Some(Duration::from_secs(30)),
            busy_poll_us: None,
        }
    }
}

impl Default for SocketTuning {
    fn default() -> Self {
        SocketTuning::latency()
    }
}

/// Socket options as the kernel reports them ([`socket_options`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SocketOptions {
    /// `TCP_NODELAY`.
    pub nodelay: bool,
    /// `SO_RCVBUF` (bytes, as reported: on Linux twice what was set).
    pub recv_buffer: u32,
    /// `SO_SNDBUF` (bytes, as reported).
    pub send_buffer: u32,
    /// `SO_KEEPALIVE`.
    pub keepalive: bool,
    /// `TCP_KEEPIDLE` (`TCP_KEEPALIVE` on macOS).
    pub keepalive_idle: Duration,
    /// `SO_LINGER` (`None`: off).
    pub linger: Option<Duration>,
}

// @student w31
impl SocketTuning {
    /// Applies the options to a socket that is not yet connected or
    /// listening, through Tokio's API (a listener's accepted sockets
    /// inherit the buffer sizes on Linux, but [`apply`](Self::apply) sets
    /// them again anyway). A few syscalls; call once per socket.
    pub fn apply_to_socket(&self, socket: &tokio::net::TcpSocket) -> io::Result<()> {
        socket.set_nodelay(self.nodelay)?;
        if let Some(n) = self.recv_buffer {
            socket.set_recv_buffer_size(n)?;
        }
        if let Some(n) = self.send_buffer {
            socket.set_send_buffer_size(n)?;
        }
        socket.set_keepalive(self.keepalive_idle.is_some())?;
        #[cfg(unix)]
        {
            if let Some(idle) = self.keepalive_idle {
                sys::set_int(
                    socket.as_raw_fd(),
                    sys::IPPROTO_TCP,
                    sys::TCP_KEEPIDLE,
                    keepidle_secs(idle),
                )?;
            }
            if let Some(us) = self.busy_poll_us {
                sys::set_busy_poll(socket.as_raw_fd(), us)?;
            }
        }
        Ok(())
    }

    /// Applies every option to a connected socket (accepted, or a client's)
    /// with `setsockopt(2)`. One syscall per option; call once per
    /// connection. On platforms without the FFI declarations (not Linux or
    /// macOS) only `TCP_NODELAY` is set and other options are an
    /// `Unsupported` error.
    #[cfg(unix)]
    pub fn apply<S: AsRawFd>(&self, socket: &S) -> io::Result<()> {
        let fd = socket.as_raw_fd();
        sys::set_int(fd, sys::IPPROTO_TCP, sys::TCP_NODELAY, self.nodelay as i32)?;
        if let Some(n) = self.recv_buffer {
            sys::set_int(fd, sys::SOL_SOCKET, sys::SO_RCVBUF, clamp_buffer(n))?;
        }
        if let Some(n) = self.send_buffer {
            sys::set_int(fd, sys::SOL_SOCKET, sys::SO_SNDBUF, clamp_buffer(n))?;
        }
        sys::set_int(
            fd,
            sys::SOL_SOCKET,
            sys::SO_KEEPALIVE,
            self.keepalive_idle.is_some() as i32,
        )?;
        if let Some(idle) = self.keepalive_idle {
            sys::set_int(fd, sys::IPPROTO_TCP, sys::TCP_KEEPIDLE, keepidle_secs(idle))?;
        }
        if let Some(us) = self.busy_poll_us {
            sys::set_busy_poll(fd, us)?;
        }
        Ok(())
    }

    /// Without the socket FFI (not unix): sets `TCP_NODELAY` only.
    #[cfg(not(unix))]
    pub fn apply(&self, socket: &tokio::net::TcpStream) -> io::Result<()> {
        socket.set_nodelay(self.nodelay)
    }
}

/// Reads back the options [`SocketTuning`] manages (plus `SO_LINGER`).
/// One syscall per option.
// @student w31
#[cfg(unix)]
pub fn socket_options<S: AsRawFd>(socket: &S) -> io::Result<SocketOptions> {
    let fd = socket.as_raw_fd();
    Ok(SocketOptions {
        nodelay: sys::get_int(fd, sys::IPPROTO_TCP, sys::TCP_NODELAY)? != 0,
        recv_buffer: sys::get_int(fd, sys::SOL_SOCKET, sys::SO_RCVBUF)? as u32,
        send_buffer: sys::get_int(fd, sys::SOL_SOCKET, sys::SO_SNDBUF)? as u32,
        keepalive: sys::get_int(fd, sys::SOL_SOCKET, sys::SO_KEEPALIVE)? != 0,
        keepalive_idle: Duration::from_secs(
            sys::get_int(fd, sys::IPPROTO_TCP, sys::TCP_KEEPIDLE)?.max(0) as u64,
        ),
        linger: sys::get_linger(fd)?,
    })
}

/// Writes every byte of `bufs` with as few `write_vectored` calls as
/// possible — one per partial write, never one per buffer — and returns the
/// total written.
///
/// * A call may accept fewer bytes than offered, stopping in the middle of
///   a slice: the next call resumes exactly there (`bufs` is advanced in
///   place, so its contents are unspecified afterwards).
/// * Empty slices are skipped; `write_vectored` is never called with
///   nothing to write.
/// * `Interrupted` is retried; `Ok(0)` while bytes remain is a `WriteZero`
///   error (a writer that accepts nothing would otherwise spin forever).
/// * Any other error is returned, and how much was written before it is
///   not reported: use this with blocking (or async) writers. On a
///   non-blocking socket, `WouldBlock` mid-batch means keeping your own
///   offset into the pending bytes, as the week-32 backends do.
///
/// O(slices) per call; no allocation.
// @student w31
pub fn write_all_vectored<W: Write + ?Sized>(
    w: &mut W,
    mut bufs: &mut [IoSlice<'_>],
) -> io::Result<usize> {
    IoSlice::advance_slices(&mut bufs, 0);
    let mut total = 0;
    while !bufs.is_empty() {
        match w.write_vectored(bufs) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "write_vectored accepted 0 bytes",
                ))
            }
            Ok(n) => {
                total += n;
                IoSlice::advance_slices(&mut bufs, n);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}

/// [`write_all_vectored`] for a Tokio writer: the same partial-write
/// handling, waiting (not spinning) while the socket is not writable.
// @student w31
pub async fn write_all_vectored_async<W: AsyncWrite + Unpin + ?Sized>(
    w: &mut W,
    mut bufs: &mut [IoSlice<'_>],
) -> io::Result<usize> {
    IoSlice::advance_slices(&mut bufs, 0);
    let mut total = 0;
    while !bufs.is_empty() {
        match w.write_vectored(bufs).await {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "write_vectored accepted 0 bytes",
                ))
            }
            Ok(n) => {
                total += n;
                IoSlice::advance_slices(&mut bufs, n);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}

/// Most slices passed to one `writev` (Linux allows `IOV_MAX` = 1024; a
/// batch of reports is far smaller).
pub const MAX_IOV: usize = 64;

/// Bytes of one encoded frame, at most.
pub const MAX_FRAME: usize = FRAME_HEADER_LEN + MAX_MSG_LEN;

/// A batch of outbound messages, each encoded as one frame in its own
/// fixed-size slot, written with one vectored write per [`MAX_IOV`] frames.
///
/// The slots are allocated once (`capacity × 42` bytes); pushing encodes in
/// place and never allocates. The alternative — copying every frame into
/// one contiguous buffer and issuing one `write` — is the gateway's default
/// and costs about the same for messages this small; `writev` pays off when
/// payloads are large or already live in separate buffers.
// @student w31
#[derive(Debug)]
pub struct FrameBatch {
    slots: Vec<[u8; MAX_FRAME]>,
    lens: Vec<u8>,
}

// @student w31
impl FrameBatch {
    /// A batch holding up to `capacity` frames. Allocates the slots.
    pub fn new(capacity: usize) -> FrameBatch {
        FrameBatch {
            slots: vec![[0; MAX_FRAME]; capacity],
            lens: Vec::with_capacity(capacity),
        }
    }

    /// Encodes `msg` as the next frame; false (and nothing changes) if the
    /// batch is full. O(1).
    pub fn push(&mut self, msg: &Message) -> bool {
        let i = self.lens.len();
        let Some(slot) = self.slots.get_mut(i) else {
            return false;
        };
        let n = encode_frame(msg, slot);
        self.lens.push(n as u8);
        true
    }

    /// Frames in the batch.
    pub fn len(&self) -> usize {
        self.lens.len()
    }

    /// True if the batch holds no frame.
    pub fn is_empty(&self) -> bool {
        self.lens.is_empty()
    }

    /// Total encoded bytes.
    pub fn bytes(&self) -> usize {
        self.lens.iter().map(|&n| n as usize).sum()
    }

    /// The `i`-th encoded frame.
    pub fn frame(&self, i: usize) -> &[u8] {
        &self.slots[i][..self.lens[i] as usize]
    }

    /// Empties the batch (keeps the slots).
    pub fn clear(&mut self) {
        self.lens.clear();
    }

    /// Writes every frame to a blocking writer: one `write_vectored` per
    /// [`MAX_IOV`] frames plus one per partial write. Returns the bytes
    /// written.
    pub fn write_blocking<W: Write + ?Sized>(&self, w: &mut W) -> io::Result<usize> {
        let mut total = 0;
        for start in (0..self.len()).step_by(MAX_IOV) {
            let mut iov = [IoSlice::new(&[]); MAX_IOV];
            let end = (start + MAX_IOV).min(self.len());
            for (k, i) in (start..end).enumerate() {
                iov[k] = IoSlice::new(self.frame(i));
            }
            total += write_all_vectored(w, &mut iov[..end - start])?;
        }
        Ok(total)
    }

    /// Writes every frame to a Tokio writer (see
    /// [`write_blocking`](Self::write_blocking)).
    pub async fn write_to<W: AsyncWrite + Unpin + ?Sized>(&self, w: &mut W) -> io::Result<usize> {
        let mut total = 0;
        for start in (0..self.len()).step_by(MAX_IOV) {
            let mut iov = [IoSlice::new(&[]); MAX_IOV];
            let end = (start + MAX_IOV).min(self.len());
            for (k, i) in (start..end).enumerate() {
                iov[k] = IoSlice::new(self.frame(i));
            }
            total += write_all_vectored_async(w, &mut iov[..end - start]).await?;
        }
        Ok(total)
    }
}

// @student-begin w31
/// Whole seconds for `TCP_KEEPIDLE` (at least 1).
fn keepidle_secs(idle: Duration) -> i32 {
    idle.as_secs().clamp(1, i32::MAX as u64) as i32
}

/// `setsockopt` takes an `int`; larger requests are capped by the kernel
/// anyway.
fn clamp_buffer(n: u32) -> i32 {
    n.min(i32::MAX as u32) as i32
}

/// The socket syscalls std does not expose, declared by hand (no `libc`).
#[cfg(unix)]
mod sys {
    use std::ffi::c_void;
    use std::io;
    use std::os::fd::RawFd;
    use std::time::Duration;

    type SockLen = u32;

    extern "C" {
        /// `int setsockopt(int fd, int level, int name, const void *value, socklen_t len);`
        fn setsockopt(fd: i32, level: i32, name: i32, value: *const c_void, len: SockLen) -> i32;
        /// `int getsockopt(int fd, int level, int name, void *value, socklen_t *len);`
        fn getsockopt(fd: i32, level: i32, name: i32, value: *mut c_void, len: *mut SockLen)
            -> i32;
    }

    // Option numbers. Linux's are the asm-generic values (x86-64, arm64,
    // riscv); macOS uses BSD numbering.
    #[cfg(target_os = "linux")]
    mod consts {
        pub const SOL_SOCKET: i32 = 1;
        pub const SO_SNDBUF: i32 = 7;
        pub const SO_RCVBUF: i32 = 8;
        pub const SO_KEEPALIVE: i32 = 9;
        pub const SO_LINGER: i32 = 13;
        pub const SO_BUSY_POLL: i32 = 46;
        pub const IPPROTO_TCP: i32 = 6;
        pub const TCP_NODELAY: i32 = 1;
        pub const TCP_KEEPIDLE: i32 = 4;
    }
    #[cfg(not(target_os = "linux"))]
    mod consts {
        pub const SOL_SOCKET: i32 = 0xffff;
        pub const SO_SNDBUF: i32 = 0x1001;
        pub const SO_RCVBUF: i32 = 0x1002;
        pub const SO_KEEPALIVE: i32 = 0x0008;
        pub const SO_LINGER: i32 = 0x0080;
        pub const SO_BUSY_POLL: i32 = -1;
        pub const IPPROTO_TCP: i32 = 6;
        pub const TCP_NODELAY: i32 = 0x01;
        /// `TCP_KEEPALIVE` on macOS/BSD: idle seconds before probing.
        pub const TCP_KEEPIDLE: i32 = 0x10;
    }
    pub use consts::*;

    /// `struct linger { int l_onoff; int l_linger; }`
    #[repr(C)]
    #[derive(Default)]
    struct Linger {
        onoff: i32,
        secs: i32,
    }

    pub fn set_int(fd: RawFd, level: i32, name: i32, value: i32) -> io::Result<()> {
        // SAFETY: `value` is a live, initialized `int` on our stack for the
        // duration of the call and we pass its exact size; the kernel only
        // reads it. An invalid `fd` or option is reported through the
        // return value, not undefined behaviour.
        let rc = unsafe {
            setsockopt(
                fd,
                level,
                name,
                (&value as *const i32).cast(),
                std::mem::size_of::<i32>() as SockLen,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn get_int(fd: RawFd, level: i32, name: i32) -> io::Result<i32> {
        let mut value: i32 = 0;
        let mut len = std::mem::size_of::<i32>() as SockLen;
        // SAFETY: `value` and `len` are valid for writes and `len` holds the
        // buffer's exact size, so the kernel writes at most 4 bytes into
        // `value` and updates `len`.
        let rc = unsafe { getsockopt(fd, level, name, (&mut value as *mut i32).cast(), &mut len) };
        if rc == 0 {
            Ok(value)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn get_linger(fd: RawFd) -> io::Result<Option<Duration>> {
        let mut value = Linger::default();
        let mut len = std::mem::size_of::<Linger>() as SockLen;
        // SAFETY: `value` is a `#[repr(C)] struct linger` valid for writes
        // and `len` is its exact size.
        let rc = unsafe {
            getsockopt(
                fd,
                SOL_SOCKET,
                SO_LINGER,
                (&mut value as *mut Linger).cast(),
                &mut len,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((value.onoff != 0).then(|| Duration::from_secs(value.secs.max(0) as u64)))
    }

    pub fn set_busy_poll(fd: RawFd, us: u32) -> io::Result<()> {
        if SO_BUSY_POLL < 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "SO_BUSY_POLL is Linux-only",
            ));
        }
        set_int(fd, SOL_SOCKET, SO_BUSY_POLL, us.min(i32::MAX as u32) as i32)
    }
}
// @student-end
