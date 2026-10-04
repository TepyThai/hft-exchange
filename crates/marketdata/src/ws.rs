//! Week 16 — a minimal WebSocket feed adapter (RFC 6455), std only.
//!
//! Enough of the protocol to stream the L2 feed to browsers:
//!
//! * the **opening handshake**: an HTTP/1.1 `GET` with `Upgrade:
//!   websocket` and a `Sec-WebSocket-Key`, answered with `101 Switching
//!   Protocols` and `Sec-WebSocket-Accept = base64(SHA-1(key + GUID))`
//!   ([`accept_key`]; SHA-1 and base64 are implemented here);
//! * **framing** (§5.2): a 2–14 byte header — FIN, opcode, a 7-bit length
//!   that escapes to 16 bits (126) or 64 bits (127), and a 4-byte mask
//!   that clients must apply and servers must not;
//! * the **feed server**: [`serve_ws`] accepts connections and streams the
//!   book to each client as a snapshot followed by conflated update
//!   batches, in the JSON format of [`crate::json`].
//!
//! # Serving market data
//!
//! ```no_run
//! use marketdata::{broadcast, serve_ws, L2Publisher, L2Update, SnapshotCell};
//! use std::net::TcpListener;
//!
//! let (mut tx, ring) = broadcast::<L2Update>(1 << 16);
//! let snapshots = SnapshotCell::default();
//! let listener = TcpListener::bind("0.0.0.0:8080").unwrap();
//! let (r, s) = (ring.clone(), snapshots.clone());
//! std::thread::spawn(move || serve_ws(listener, r, s));
//!
//! // The engine thread: publish every update, refresh the snapshot often
//! // enough that a new client finds one within the ring's reach.
//! let mut publisher = L2Publisher::with_snapshot_interval(1_000);
//! # let events: Vec<types::Event> = Vec::new();
//! for ev in &events {
//!     if let Some(u) = publisher.on_event(ev) {
//!         tx.publish(u);
//!     }
//!     if let Some(snap) = publisher.periodic_snapshot() {
//!         snapshots.store(snap);
//!     }
//! }
//! ```
//!
//! # Per-client policy
//!
//! Each client has its own thread and ring [`Reader`](crate::Reader),
//! starting at the oldest value in the ring, and keeps a
//! [`BookBuilder`](crate::BookBuilder) mirror of what the client holds:
//!
//! 1. on connect it sends the latest snapshot from the [`SnapshotCell`];
//! 2. it drains the ring into a [`Conflator`](crate::Conflator) and sends
//!    whatever is pending as **one** batch — a slow client gets fewer,
//!    conflated batches instead of a growing queue;
//! 3. if its reader lags (or the snapshot was older than the ring), its
//!    mirror detects the gap; it waits for a newer snapshot in the cell
//!    and sends that (the client replaces its book);
//! 4. writes that block for more than [`WRITE_TIMEOUT`] disconnect the
//!    client; a Close frame is answered and ends the session.

use crate::ring::BroadcastRing;
use crate::{L2Snapshot, L2Update};
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use types::Seq;

// @student-begin w16
use crate::json::{write_batch_json, write_snapshot_json};
use crate::ring::Recv;
use crate::{BookBuilder, Conflator, SnapshotOutcome};
// @student-end

/// The GUID appended to the client's key before hashing (RFC 6455 §1.3).
pub const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Largest opening handshake the server reads before giving up.
pub const MAX_HANDSHAKE: usize = 8 * 1024;

/// Largest client frame payload the server accepts (clients only send
/// control frames to a feed).
pub const MAX_CLIENT_PAYLOAD: usize = 4 * 1024;

/// A client whose socket accepts nothing for this long is disconnected.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long an idle client thread waits for client input before polling
/// the ring again.
pub const IDLE_WAIT: Duration = Duration::from_millis(1);

/// Ring values a client thread reads per pass before it sends a batch.
pub const MAX_DRAIN: usize = 4096;

/// Frame opcodes (RFC 6455 §5.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Opcode {
    Continuation = 0x0,
    Text = 0x1,
    Binary = 0x2,
    Close = 0x8,
    Ping = 0x9,
    Pong = 0xA,
}

impl Opcode {
    /// The opcode for a header nibble, if it is a defined one.
    pub fn from_u8(v: u8) -> Option<Opcode> {
        Some(match v {
            0x0 => Opcode::Continuation,
            0x1 => Opcode::Text,
            0x2 => Opcode::Binary,
            0x8 => Opcode::Close,
            0x9 => Opcode::Ping,
            0xA => Opcode::Pong,
            _ => return None,
        })
    }

    /// Close, Ping and Pong are control frames: never fragmented, at most
    /// 125 bytes of payload.
    pub fn is_control(self) -> bool {
        (self as u8) & 0x8 != 0
    }
}

/// A decoded frame, payload already unmasked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// Final fragment of a message.
    pub fin: bool,
    pub opcode: Opcode,
    pub payload: Vec<u8>,
}

/// A frame that violates RFC 6455.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// RSV1–3 set without a negotiated extension.
    ReservedBits,
    /// Opcode 3–7 or 0xB–0xF.
    UnknownOpcode(u8),
    /// A control frame that is fragmented or longer than 125 bytes.
    BadControlFrame,
    /// Payload longer than the receiver accepts (or a 64-bit length with
    /// the top bit set).
    TooLong(u64),
}

/// An opening handshake the server refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandshakeError {
    /// Not a parseable HTTP/1.1 request.
    Malformed,
    /// The method is not `GET`.
    NotGet,
    /// Missing `Upgrade: websocket` or `Connection: Upgrade`.
    NotUpgrade,
    /// Missing or invalid `Sec-WebSocket-Key` (must be 16 bytes, base64).
    BadKey,
    /// `Sec-WebSocket-Version` is not 13.
    UnsupportedVersion,
    /// The request exceeded [`MAX_HANDSHAKE`] bytes.
    TooLarge,
}

/// Anything that ends a WebSocket session.
#[derive(Debug)]
pub enum WsError {
    Io(io::Error),
    Handshake(HandshakeError),
    Frame(FrameError),
}

impl fmt::Display for WsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WsError::Io(e) => write!(f, "websocket i/o: {e}"),
            WsError::Handshake(e) => write!(f, "websocket handshake refused: {e:?}"),
            WsError::Frame(e) => write!(f, "websocket protocol error: {e:?}"),
        }
    }
}

impl std::error::Error for WsError {}

impl From<io::Error> for WsError {
    fn from(e: io::Error) -> Self {
        WsError::Io(e)
    }
}

impl From<HandshakeError> for WsError {
    fn from(e: HandshakeError) -> Self {
        WsError::Handshake(e)
    }
}

impl From<FrameError> for WsError {
    fn from(e: FrameError) -> Self {
        WsError::Frame(e)
    }
}

/// What the client asked for in its opening handshake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handshake {
    /// Request target, e.g. `/feed`.
    pub path: String,
    /// The client's `Sec-WebSocket-Key`, trimmed.
    pub key: String,
}

// ---------------------------------------------------------------------------
// SHA-1, base64 and the accept key

/// SHA-1 digest of `data` (FIPS 180-4). Only for the handshake — SHA-1 is
/// broken for anything security-related. O(len), no allocation.
// @student w16
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let mut blocks = data.chunks_exact(64);
    for block in &mut blocks {
        sha1_block(&mut h, block);
    }
    // Padding: 0x80, zeros, then the message length in bits (big-endian)
    // in the last 8 bytes of the final block — one or two blocks.
    let rest = blocks.remainder();
    let mut tail = [0u8; 128];
    tail[..rest.len()].copy_from_slice(rest);
    tail[rest.len()] = 0x80;
    let tail_len = if rest.len() < 56 { 64 } else { 128 };
    let bits = (data.len() as u64).wrapping_mul(8);
    tail[tail_len - 8..tail_len].copy_from_slice(&bits.to_be_bytes());
    for block in tail[..tail_len].chunks_exact(64) {
        sha1_block(&mut h, block);
    }
    let mut out = [0u8; 20];
    for (chunk, word) in out.chunks_exact_mut(4).zip(h) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    out
}

// @student-begin w16
/// One 64-byte block of SHA-1's compression function.
fn sha1_block(h: &mut [u32; 5], block: &[u8]) {
    let mut w = [0u32; 80];
    for (i, word) in block.chunks_exact(4).enumerate() {
        w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }
    let [mut a, mut b, mut c, mut d, mut e] = *h;
    for (i, &wi) in w.iter().enumerate() {
        let (f, k) = match i {
            0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
            20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
            _ => (b ^ c ^ d, 0xCA62_C1D6),
        };
        let t = a
            .rotate_left(5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(wi);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = t;
    }
    for (x, v) in h.iter_mut().zip([a, b, c, d, e]) {
        *x = x.wrapping_add(v);
    }
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
// @student-end

/// Standard base64 (RFC 4648 §4) with `=` padding. O(len), allocates the
/// result.
// @student w16
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(BASE64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `Sec-WebSocket-Accept` for a client's `Sec-WebSocket-Key`:
/// `base64(SHA-1(key + WS_GUID))`. O(1), allocates.
// @student w16
pub fn accept_key(client_key: &str) -> String {
    let mut input = Vec::with_capacity(client_key.len() + WS_GUID.len());
    input.extend_from_slice(client_key.trim().as_bytes());
    input.extend_from_slice(WS_GUID.as_bytes());
    base64_encode(&sha1(&input))
}

// ---------------------------------------------------------------------------
// Handshake

/// Parses a client's opening handshake: the request line and headers, up
/// to and including the blank line. Header names are case-insensitive;
/// `Connection` may list several tokens. O(len), allocates the result.
// @student w16
pub fn parse_handshake(request: &[u8]) -> Result<Handshake, HandshakeError> {
    let text = std::str::from_utf8(request).map_err(|_| HandshakeError::Malformed)?;
    let mut lines = text.split("\r\n");
    let mut request_line = lines.next().unwrap_or("").split(' ');
    let (method, path, version) = (
        request_line.next().unwrap_or(""),
        request_line.next().unwrap_or(""),
        request_line.next().unwrap_or(""),
    );
    if path.is_empty() || version != "HTTP/1.1" || request_line.next().is_some() {
        return Err(HandshakeError::Malformed);
    }
    if method != "GET" {
        return Err(HandshakeError::NotGet);
    }
    let (mut upgrade, mut connection, mut key, mut ws_version) = (false, false, None, None);
    for line in lines {
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').ok_or(HandshakeError::Malformed)?;
        let (name, value) = (name.trim(), value.trim());
        if name.eq_ignore_ascii_case("upgrade") {
            upgrade = value.eq_ignore_ascii_case("websocket");
        } else if name.eq_ignore_ascii_case("connection") {
            connection = value
                .split(',')
                .any(|t| t.trim().eq_ignore_ascii_case("upgrade"));
        } else if name.eq_ignore_ascii_case("sec-websocket-key") {
            key = Some(value);
        } else if name.eq_ignore_ascii_case("sec-websocket-version") {
            ws_version = Some(value);
        }
    }
    if !upgrade || !connection {
        return Err(HandshakeError::NotUpgrade);
    }
    // 16 random bytes in base64: 22 characters plus "==".
    let key = key.ok_or(HandshakeError::BadKey)?;
    let valid = key.len() == 24
        && key.ends_with("==")
        && key[..22]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/');
    if !valid {
        return Err(HandshakeError::BadKey);
    }
    if ws_version != Some("13") {
        return Err(HandshakeError::UnsupportedVersion);
    }
    Ok(Handshake {
        path: path.to_string(),
        key: key.to_string(),
    })
}

/// Server side of the opening handshake on `stream`: reads the request,
/// answers `101 Switching Protocols` (or `400`/`426` and an error). Returns
/// the handshake and any bytes the client sent after it (the start of its
/// first frame). Allocates a small buffer.
// @student w16
pub fn server_handshake<S: Read + Write>(stream: &mut S) -> Result<(Handshake, Vec<u8>), WsError> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let end = loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > MAX_HANDSHAKE {
            refuse(stream, HandshakeError::TooLarge)?;
            return Err(HandshakeError::TooLarge.into());
        }
    };
    match parse_handshake(&buf[..end]) {
        Ok(hs) => {
            let response = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
                accept_key(&hs.key)
            );
            stream.write_all(response.as_bytes())?;
            stream.flush()?;
            Ok((hs, buf[end..].to_vec()))
        }
        Err(e) => {
            refuse(stream, e)?;
            Err(e.into())
        }
    }
}

// @student-begin w16
fn refuse<S: Write>(stream: &mut S, e: HandshakeError) -> io::Result<()> {
    let response = if e == HandshakeError::UnsupportedVersion {
        "HTTP/1.1 426 Upgrade Required\r\nSec-WebSocket-Version: 13\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    } else {
        "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    };
    stream.write_all(response.as_bytes())?;
    stream.flush()
}
// @student-end

// ---------------------------------------------------------------------------
// Framing

/// Appends one unfragmented, unmasked (server → client) frame: FIN set,
/// the shortest length encoding (7, 16 or 64 bits, big-endian), then the
/// payload. O(len); allocates only if `out` grows.
// @student w16
pub fn encode_frame(opcode: Opcode, payload: &[u8], out: &mut Vec<u8>) {
    out.push(0x80 | opcode as u8);
    let len = payload.len();
    if len <= 125 {
        out.push(len as u8);
    } else if len <= u16::MAX as usize {
        out.push(126);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(127);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
    out.extend_from_slice(payload);
}

/// Decodes the frame at the start of `buf`, masked (client → server) or
/// not. `Ok(None)` if `buf` does not hold a whole frame yet; otherwise the
/// frame, unmasked, and the number of bytes it used. Payloads longer than
/// `max_payload` are an error. O(frame length), allocates the payload.
// @student w16
pub fn decode_frame(buf: &[u8], max_payload: usize) -> Result<Option<(Frame, usize)>, FrameError> {
    let [b0, b1, ..] = *buf else {
        return Ok(None);
    };
    if b0 & 0x70 != 0 {
        return Err(FrameError::ReservedBits);
    }
    let fin = b0 & 0x80 != 0;
    let opcode = Opcode::from_u8(b0 & 0x0F).ok_or(FrameError::UnknownOpcode(b0 & 0x0F))?;
    let masked = b1 & 0x80 != 0;
    let (len, mut at) = match b1 & 0x7F {
        126 => {
            let Some(b) = buf.get(2..4) else {
                return Ok(None);
            };
            (u16::from_be_bytes([b[0], b[1]]) as u64, 4)
        }
        127 => {
            let Some(b) = buf.get(2..10) else {
                return Ok(None);
            };
            let mut be = [0u8; 8];
            be.copy_from_slice(b);
            (u64::from_be_bytes(be), 10)
        }
        n => (n as u64, 2),
    };
    if len >> 63 != 0 {
        return Err(FrameError::TooLong(len));
    }
    if opcode.is_control() && (!fin || len > 125) {
        return Err(FrameError::BadControlFrame);
    }
    if len > max_payload as u64 {
        return Err(FrameError::TooLong(len));
    }
    let mut mask = [0u8; 4];
    if masked {
        let Some(m) = buf.get(at..at + 4) else {
            return Ok(None);
        };
        mask.copy_from_slice(m);
        at += 4;
    }
    let end = at + len as usize;
    let Some(body) = buf.get(at..end) else {
        return Ok(None);
    };
    let payload = body
        .iter()
        .enumerate()
        .map(|(i, &b)| b ^ mask[i & 3])
        .collect();
    Ok(Some((
        Frame {
            fin,
            opcode,
            payload,
        },
        end,
    )))
}

// ---------------------------------------------------------------------------
// Feed server

/// The latest full book snapshot, shared between the thread that owns the
/// [`L2Publisher`](crate::L2Publisher) (stores) and the feed server's
/// client threads (load on connect and after a lag). Cloning shares it.
///
/// Store a fresh snapshot at least every `capacity / 2` updates of the
/// ring the server reads, so a client that starts at the ring's oldest
/// value always finds a snapshot it can continue from.
#[derive(Clone, Debug, Default)]
pub struct SnapshotCell {
    inner: Arc<SnapshotInner>,
}

#[derive(Debug, Default)]
struct SnapshotInner {
    seq: AtomicU64,
    snapshot: Mutex<L2Snapshot>,
}

impl SnapshotCell {
    /// A cell holding `snap`.
    pub fn new(snap: L2Snapshot) -> SnapshotCell {
        let cell = SnapshotCell::default();
        cell.store(snap);
        cell
    }

    /// Replaces the snapshot unless the cell already holds a newer one.
    /// O(1) plus dropping the old snapshot; takes a lock.
    pub fn store(&self, snap: L2Snapshot) {
        let mut slot = self.lock();
        if snap.seq >= slot.seq {
            self.inner.seq.store(snap.seq.0, Ordering::Release);
            *slot = snap;
        }
    }

    /// A copy of the current snapshot. O(levels), allocates; takes a lock.
    pub fn load(&self) -> L2Snapshot {
        self.lock().clone()
    }

    /// Sequence number of the current snapshot. One atomic load.
    pub fn seq(&self) -> Seq {
        Seq(self.inner.seq.load(Ordering::Acquire))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, L2Snapshot> {
        // A panic while holding the lock cannot leave a half-written
        // snapshot (assignment is the only mutation), so poison is benign.
        self.inner
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Accepts WebSocket clients on `listener` forever, one thread per client
/// running [`serve_ws_client`]. Returns only if a thread cannot be
/// spawned; failed connections are dropped.
pub fn serve_ws(
    listener: TcpListener,
    ring: Arc<BroadcastRing<L2Update>>,
    snapshots: SnapshotCell,
) -> io::Result<()> {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let (ring, snapshots) = (Arc::clone(&ring), snapshots.clone());
        std::thread::Builder::new()
            .name("ws-client".into())
            .spawn(move || {
                // A client that misbehaves or goes away only ends its own
                // session.
                let _ = serve_ws_client(stream, &ring, &snapshots);
            })?;
    }
    Ok(())
}

/// Serves one client until it closes the connection, sends a Close frame,
/// stops reading for [`WRITE_TIMEOUT`], or violates the protocol: the
/// handshake, then a snapshot, then conflated update batches, re-sending a
/// snapshot after a lag (see the module docs for the policy). Busy-polls
/// the ring, waiting [`IDLE_WAIT`] on the socket when there is nothing to
/// send. `Ok(())` on an orderly close.
// @student w16
pub fn serve_ws_client(
    stream: TcpStream,
    ring: &Arc<BroadcastRing<L2Update>>,
    snapshots: &SnapshotCell,
) -> Result<(), WsError> {
    let mut stream = stream;
    stream.set_nodelay(true)?;
    let (_, mut input) = server_handshake(&mut stream)?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    stream.set_read_timeout(Some(IDLE_WAIT))?;

    // Read from the oldest value the ring still has, then take the
    // snapshot: the snapshot is usually older than the live edge, and the
    // ring bridges the difference.
    let mut reader = ring.subscribe_oldest();
    let mut mirror = BookBuilder::new();
    let mut conflator = Conflator::new();
    let (mut out, mut json, mut batch) = (Vec::new(), String::new(), Vec::new());

    mirror.apply_snapshot(&snapshots.load());
    send_snapshot(&mut stream, &mirror.snapshot(), &mut json, &mut out)?;
    let mut tried_snapshot = mirror.last_seq().unwrap_or_default();

    loop {
        let mut drained = 0;
        while drained < MAX_DRAIN {
            match reader.try_recv() {
                Recv::Item(u) => {
                    drained += 1;
                    if mirror.apply_update(&u) == crate::UpdateOutcome::Applied {
                        conflator.push(u);
                    }
                }
                // The mirror sees the hole on the next item and waits for
                // a snapshot.
                Recv::Lagged(_) => drained += 1,
                Recv::Empty => break,
            }
        }

        if !mirror.is_synced() && snapshots.seq() > tried_snapshot {
            let snap = snapshots.load();
            tried_snapshot = snap.seq;
            if let SnapshotOutcome::Synced { .. } = mirror.apply_snapshot(&snap) {
                conflator.clear();
                send_snapshot(&mut stream, &mirror.snapshot(), &mut json, &mut out)?;
            }
        }

        // Everything pending was applied before any gap, so the batch is
        // consistent as of the mirror's last applied update.
        if !conflator.is_empty() {
            batch.clear();
            while let Some(u) = conflator.pop() {
                batch.push(u);
            }
            let seq = mirror.last_seq().unwrap_or_default();
            json.clear();
            write_batch_json(seq, &batch, &mut json);
            out.clear();
            encode_frame(Opcode::Text, json.as_bytes(), &mut out);
            stream.write_all(&out)?;
        } else if drained == 0 && !poll_client(&mut stream, &mut input, &mut out)? {
            return Ok(());
        }
    }
}

// @student-begin w16
fn send_snapshot(
    stream: &mut TcpStream,
    snap: &L2Snapshot,
    json: &mut String,
    out: &mut Vec<u8>,
) -> io::Result<()> {
    json.clear();
    write_snapshot_json(snap, json);
    out.clear();
    encode_frame(Opcode::Text, json.as_bytes(), out);
    stream.write_all(out)
}

/// Waits up to `IDLE_WAIT` for client input and handles it. `Ok(false)`
/// when the session is over (EOF or a Close frame, answered).
fn poll_client(
    stream: &mut TcpStream,
    input: &mut Vec<u8>,
    out: &mut Vec<u8>,
) -> Result<bool, WsError> {
    let mut chunk = [0u8; 512];
    match stream.read(&mut chunk) {
        Ok(0) => return Ok(false),
        Ok(n) => input.extend_from_slice(&chunk[..n]),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) => {}
        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
        Err(e) => return Err(e.into()),
    }
    while let Some((frame, used)) = decode_frame(input, MAX_CLIENT_PAYLOAD)? {
        input.drain(..used);
        match frame.opcode {
            Opcode::Close => {
                // Echo the status code (if any) and end the session.
                out.clear();
                encode_frame(
                    Opcode::Close,
                    &frame.payload[..frame.payload.len().min(2)],
                    out,
                );
                stream.write_all(out)?;
                return Ok(false);
            }
            Opcode::Ping => {
                out.clear();
                encode_frame(Opcode::Pong, &frame.payload, out);
                stream.write_all(out)?;
            }
            _ => {}
        }
    }
    Ok(true)
}
// @student-end
