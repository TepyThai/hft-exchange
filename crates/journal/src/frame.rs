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

// @student-begin w09
use crate::crc::Crc32;

/// Validates the frame starting at `pos`; on success returns it and the
/// offset of the next frame.
#[inline]
fn read_frame(bytes: &[u8], pos: usize, expected: Seq) -> Result<(Frame<'_>, usize), Corruption> {
    let rest = &bytes[pos..];
    let fail = |kind| Err(Corruption { offset: pos, kind });
    if rest.len() < FRAME_HEADER_LEN {
        return fail(CorruptionKind::Truncated {
            needed: FRAME_HEADER_LEN,
            available: rest.len(),
        });
    }
    let len = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]);
    if len > MAX_PAYLOAD_LEN {
        return fail(CorruptionKind::BadLength { len });
    }
    let total = FRAME_HEADER_LEN + len as usize;
    if rest.len() < total {
        return fail(CorruptionKind::Truncated {
            needed: total,
            available: rest.len(),
        });
    }
    let mut seq = [0u8; 8];
    seq.copy_from_slice(&rest[4..12]);
    let seq = Seq(u64::from_le_bytes(seq));
    let stored = u32::from_le_bytes([rest[12], rest[13], rest[14], rest[15]]);
    let payload = &rest[FRAME_HEADER_LEN..total];
    let mut crc = Crc32::new();
    crc.update(&rest[..12]);
    crc.update(payload);
    let computed = crc.finish();
    if computed != stored {
        return fail(CorruptionKind::BadCrc { stored, computed });
    }
    if seq != expected {
        return fail(CorruptionKind::SeqMismatch {
            expected,
            found: seq,
        });
    }
    Ok((
        Frame {
            seq,
            offset: pos,
            payload,
        },
        pos + total,
    ))
}
// @student-end

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
// @student w09
#[derive(Clone, Debug)]
pub struct Frames<'a> {
    bytes: &'a [u8],
    /// Offset of the next frame to read.
    pos: usize,
    /// Sequence number the next frame must carry.
    expected: Seq,
    /// Frames below this sequence number are verified but not yielded.
    from: Seq,
    /// Set after yielding a corruption error.
    stopped: bool,
}

// @student w09
impl<'a> Frames<'a> {
    /// Iterates `bytes`, whose first frame must carry `first_seq`. O(1).
    pub fn new(bytes: &'a [u8], first_seq: Seq) -> Frames<'a> {
        Frames {
            bytes,
            pos: 0,
            expected: first_seq,
            from: first_seq,
            stopped: false,
        }
    }

    /// Skips (but still verifies) frames whose sequence number is below
    /// `from`. Skipping is O(bytes skipped): frames are variable length, so
    /// finding a sequence number means walking the headers before it.
    pub fn skip_to(self, from: Seq) -> Frames<'a> {
        Frames { from, ..self }
    }

    /// Byte offset of the next unread frame. After the iterator has
    /// finished this is the length of the valid prefix of the bytes.
    pub fn offset(&self) -> usize {
        self.pos
    }

    /// Sequence number the next frame must carry; after finishing, one past
    /// the last good frame.
    pub fn next_seq(&self) -> Seq {
        self.expected
    }
}

// @student w09
impl<'a> Iterator for Frames<'a> {
    type Item = Result<Frame<'a>, Corruption>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.stopped || self.pos == self.bytes.len() {
                return None;
            }
            match read_frame(self.bytes, self.pos, self.expected) {
                Ok((frame, next)) => {
                    self.pos = next;
                    self.expected = Seq(self.expected.0 + 1);
                    if frame.seq >= self.from {
                        return Some(Ok(frame));
                    }
                }
                Err(c) => {
                    self.stopped = true;
                    return Some(Err(c));
                }
            }
        }
    }
}

impl std::iter::FusedIterator for Frames<'_> {}
