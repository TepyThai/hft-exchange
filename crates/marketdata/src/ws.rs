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

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::ring::BroadcastRing;
use crate::{L2Snapshot, L2Update};
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use types::Seq;

// TODO(w16): implement — see the week guide on tepy.dev/hft.

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
#[allow(unused_variables)]
pub fn sha1(data: &[u8]) -> [u8; 20] {
    todo!("w16: sha1")
}

// TODO(w16): implement — see the week guide on tepy.dev/hft.

/// Standard base64 (RFC 4648 §4) with `=` padding. O(len), allocates the
/// result.
#[allow(unused_variables)]
pub fn base64_encode(data: &[u8]) -> String {
    todo!("w16: base64_encode")
}

/// `Sec-WebSocket-Accept` for a client's `Sec-WebSocket-Key`:
/// `base64(SHA-1(key + WS_GUID))`. O(1), allocates.
#[allow(unused_variables)]
pub fn accept_key(client_key: &str) -> String {
    todo!("w16: accept_key")
}

// ---------------------------------------------------------------------------
// Handshake

/// Parses a client's opening handshake: the request line and headers, up
/// to and including the blank line. Header names are case-insensitive;
/// `Connection` may list several tokens. O(len), allocates the result.
#[allow(unused_variables)]
pub fn parse_handshake(request: &[u8]) -> Result<Handshake, HandshakeError> {
    todo!("w16: parse_handshake")
}

/// Server side of the opening handshake on `stream`: reads the request,
/// answers `101 Switching Protocols` (or `400`/`426` and an error). Returns
/// the handshake and any bytes the client sent after it (the start of its
/// first frame). Allocates a small buffer.
#[allow(unused_variables)]
pub fn server_handshake<S: Read + Write>(stream: &mut S) -> Result<(Handshake, Vec<u8>), WsError> {
    todo!("w16: server_handshake")
}

// TODO(w16): implement — see the week guide on tepy.dev/hft.

// ---------------------------------------------------------------------------
// Framing

/// Appends one unfragmented, unmasked (server → client) frame: FIN set,
/// the shortest length encoding (7, 16 or 64 bits, big-endian), then the
/// payload. O(len); allocates only if `out` grows.
#[allow(unused_variables)]
pub fn encode_frame(opcode: Opcode, payload: &[u8], out: &mut Vec<u8>) {
    todo!("w16: encode_frame")
}

/// Decodes the frame at the start of `buf`, masked (client → server) or
/// not. `Ok(None)` if `buf` does not hold a whole frame yet; otherwise the
/// frame, unmasked, and the number of bytes it used. Payloads longer than
/// `max_payload` are an error. O(frame length), allocates the payload.
#[allow(unused_variables)]
pub fn decode_frame(buf: &[u8], max_payload: usize) -> Result<Option<(Frame, usize)>, FrameError> {
    todo!("w16: decode_frame")
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
#[allow(unused_variables)]
pub fn serve_ws_client(
    stream: TcpStream,
    ring: &Arc<BroadcastRing<L2Update>>,
    snapshots: &SnapshotCell,
) -> Result<(), WsError> {
    todo!("w16: serve_ws_client")
}

// TODO(w16): implement — see the week guide on tepy.dev/hft.
