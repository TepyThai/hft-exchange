//! The journal frame format, the verifying [`Frames`] iterator and
//! corruption reports. Shared by every journal backend (in memory now, a
//! file in week 18, a replication stream in week 20).
//!
//! ```text
//! frame = header (FRAME_HEADER_LEN = 16 bytes) ++ payload
//!   off  size  field
//!     0     4  len    u32 LE   payload length in bytes (<= MAX_PAYLOAD_LEN)
//!     4     8  seq    u64 LE   sequence number of this command
//!    12     4  crc    u32 LE   CRC-32/IEEE of bytes [0, 12) ++ payload
//!    16   len  payload         encode_command(cmd) (see `codec`)
//! ```
//!
//! The CRC covers the length and the sequence number as well as the
//! payload, so a flipped bit anywhere in a frame is caught. Frames are
//! packed back to back with no padding; a journal is valid when every
//! frame's CRC matches and sequence numbers increase by exactly one from
//! the journal's first sequence number.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::codec::{decode_command, DecodeError};
use std::fmt;
use types::{Command, Seq};

/// Size of a frame header: `len u32 + seq u64 + crc u32`.
pub const FRAME_HEADER_LEN: usize = 16;

/// Largest payload a well-formed frame may carry. A larger length field can
/// only come from corruption, and is reported as such instead of being
/// mistaken for a frame that runs past the end of the journal.
pub const MAX_PAYLOAD_LEN: u32 = 1 << 16;

/// Sequence number of the first command of a fresh journal. `Seq(0)` is
/// never assigned: it means "before the first command".
pub const FIRST_SEQ: Seq = Seq(1);

/// One verified frame, borrowed from the journal's bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    /// Sequence number stored in the header.
    pub seq: Seq,
    /// Byte offset of the frame's header within the journal.
    pub offset: usize,
    /// The payload (an encoded command).
    pub payload: &'a [u8],
}

impl Frame<'_> {
    /// Total size of the frame in bytes, header included.
    pub fn frame_len(&self) -> usize {
        FRAME_HEADER_LEN + self.payload.len()
    }

    /// Decodes the payload as a [`Command`]. O(1).
    pub fn command(&self) -> Result<Command, DecodeError> {
        decode_command(self.payload)
    }
}

/// Where and how a journal is damaged. Everything before `offset` is a
/// sequence of valid frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Corruption {
    /// Byte offset of the first bad frame (= length of the valid prefix).
    pub offset: usize,
    pub kind: CorruptionKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CorruptionKind {
    /// The bytes end inside a frame: `needed` bytes from `offset` to finish
    /// it, `available` present. The signature of a torn (interrupted) write
    /// of the last frame.
    Truncated { needed: usize, available: usize },
    /// The length field exceeds [`MAX_PAYLOAD_LEN`].
    BadLength { len: u32 },
    /// The stored CRC does not match the frame's contents.
    BadCrc { stored: u32, computed: u32 },
    /// An intact frame with an unexpected sequence number (a gap, a
    /// duplicate, or frames from another journal spliced in).
    SeqMismatch { expected: Seq, found: Seq },
}

impl Corruption {
    /// True if the damage is an incomplete last frame — what a crash in the
    /// middle of an append leaves behind, and safe to truncate away.
    /// Anything else means bytes *inside* the journal changed.
    pub fn is_torn_tail(&self) -> bool {
        matches!(self.kind, CorruptionKind::Truncated { .. })
    }
}

impl fmt::Display for Corruption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "journal corrupt at byte {}: ", self.offset)?;
        match self.kind {
            CorruptionKind::Truncated { needed, available } => {
                write!(f, "truncated frame ({available} of {needed} bytes)")
            }
            CorruptionKind::BadLength { len } => {
                write!(f, "payload length {len} exceeds {MAX_PAYLOAD_LEN}")
            }
            CorruptionKind::BadCrc { stored, computed } => {
                write!(
                    f,
                    "crc mismatch (stored {stored:#010x}, computed {computed:#010x})"
                )
            }
            CorruptionKind::SeqMismatch { expected, found } => {
                write!(f, "expected seq {}, found {}", expected.0, found.0)
            }
        }
    }
}

impl std::error::Error for Corruption {}

// TODO(w09): implement — see the week guide on tepy.dev/hft.

/// Iterator over the frames of raw journal bytes, verifying each one.
///
/// Yields `Ok(frame)` for every valid frame, in order. At the first damaged
/// frame it yields one `Err(Corruption)` and then stops for good, so a
/// consumer naturally stops at the last good frame; [`offset`](Frames::offset)
/// then tells where the valid prefix ends. Each step is O(frame length)
/// (the CRC), with no allocation.
///
/// Use it directly on bytes that did not come from [`Journal::append`](crate::Journal::append) —
/// read back from a file (week 18) or received from a primary (week 20).
#[derive(Clone, Debug)]
pub struct Frames<'a> {
    // TODO(w09): design this type's fields.
    _todo: std::marker::PhantomData<(&'a (),)>,
}

#[allow(unused_variables)]
impl<'a> Frames<'a> {
    /// Iterates `bytes`, whose first frame must carry `first_seq`. O(1).
    pub fn new(bytes: &'a [u8], first_seq: Seq) -> Frames<'a> {
        todo!("w09: new")
    }

    /// Skips (but still verifies) frames whose sequence number is below
    /// `from`. Skipping is O(bytes skipped): frames are variable length, so
    /// finding a sequence number means walking the headers before it.
    pub fn skip_to(self, from: Seq) -> Frames<'a> {
        todo!("w09: skip_to")
    }

    /// Byte offset of the next unread frame. After the iterator has
    /// finished this is the length of the valid prefix of the bytes.
    pub fn offset(&self) -> usize {
        todo!("w09: offset")
    }

    /// Sequence number the next frame must carry; after finishing, one past
    /// the last good frame.
    pub fn next_seq(&self) -> Seq {
        todo!("w09: next_seq")
    }
}

#[allow(unused_variables)]
impl<'a> Iterator for Frames<'a> {
    type Item = Result<Frame<'a>, Corruption>;

    fn next(&mut self) -> Option<Self::Item> {
        todo!("w09: next")
    }
}

impl std::iter::FusedIterator for Frames<'_> {}

/// Why [`Journal::append_frame`](crate::Journal::append_frame) refused a
/// frame received from elsewhere (week 20: a primary's replication stream).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// Not a well-formed frame: truncated, a bad length field or a CRC
    /// mismatch (`offset` is 0, the start of the frame).
    Corrupt(Corruption),
    /// A valid frame followed by `len - frame_len` extra bytes.
    TrailingBytes { frame_len: usize, len: usize },
    /// The frame's sequence number is below the next expected one: it was
    /// already appended (a retransmission).
    Duplicate { expected: Seq, found: Seq },
    /// The frame's sequence number is above the next expected one: frames
    /// `expected .. found` are missing.
    Gap { expected: Seq, found: Seq },
    /// The frame verifies but its payload is not a command (a broken
    /// writer, not a broken link).
    Decode { seq: Seq, error: DecodeError },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::Corrupt(c) => write!(f, "{c}"),
            FrameError::TrailingBytes { frame_len, len } => {
                write!(
                    f,
                    "{len} bytes hold a {frame_len}-byte frame and trailing bytes"
                )
            }
            FrameError::Duplicate { expected, found } => {
                write!(f, "duplicate frame {} (expected {})", found.0, expected.0)
            }
            FrameError::Gap { expected, found } => {
                write!(f, "gap: got frame {}, expected {}", found.0, expected.0)
            }
            FrameError::Decode { seq, error } => write!(f, "frame {}: {error}", seq.0),
        }
    }
}

impl std::error::Error for FrameError {}
