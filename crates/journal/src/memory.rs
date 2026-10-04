//! The in-memory journal: one contiguous buffer of [frames](crate::frame).

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::frame::{Corruption, Frames};
use types::{Command, Seq};

// TODO(w09): implement — see the week guide on tepy.dev/hft.

/// A sequenced, append-only journal of [`Command`]s held in one contiguous
/// `Vec<u8>` of [frames](crate::frame).
///
/// The journal is the sequencer: [`append`](Journal::append) assigns each
/// command the next sequence number. It holds the frames with sequence
/// numbers `first_seq() .. next_seq()`; a fresh journal starts at
/// [`FIRST_SEQ`](crate::FIRST_SEQ), a journal *tail* (everything after a snapshot, week 18)
/// starts later.
///
/// Appending is O(1) amortized and allocation-free once the buffer has
/// grown to its working size (or was sized with
/// [`with_capacity`](Journal::with_capacity)).
#[derive(Clone, Debug)]
pub struct Journal {
    // TODO(w09): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl Journal {
    /// An empty journal whose first command gets [`FIRST_SEQ`](crate::FIRST_SEQ). No
    /// allocation until the first append.
    pub fn new() -> Journal {
        todo!("w09: new")
    }

    /// An empty journal with room for `bytes` bytes of frames, so appends
    /// do not reallocate until that much has been written.
    pub fn with_capacity(bytes: usize) -> Journal {
        todo!("w09: with_capacity")
    }

    /// An empty journal whose first command gets `first_seq` — e.g. the
    /// tail that follows a snapshot taken at `first_seq - 1`.
    pub fn starting_at(first_seq: Seq) -> Journal {
        todo!("w09: starting_at")
    }

    /// Rebuilds a journal from raw bytes whose first frame should carry
    /// `first_seq` (e.g. read back after a crash). Keeps the longest valid
    /// prefix and drops everything from the first damaged frame on; the
    /// damage, if any, is returned so the caller can tell a torn last write
    /// ([`Corruption::is_torn_tail`], expected after a crash) from real
    /// corruption. Appends continue after the last good frame.
    /// O(bytes); reuses `bytes`' allocation.
    pub fn recover(bytes: Vec<u8>, first_seq: Seq) -> (Journal, Option<Corruption>) {
        todo!("w09: recover")
    }

    /// Appends `cmd` as one frame and returns the sequence number it was
    /// assigned. O(1) amortized: encodes into a stack buffer, checksums
    /// header and payload, and copies one frame into the journal.
    #[inline]
    pub fn append(&mut self, cmd: &Command) -> Seq {
        todo!("w09: append")
    }

    /// Sequence number of the first frame held (equal to
    /// [`next_seq`](Journal::next_seq) when empty).
    pub fn first_seq(&self) -> Seq {
        todo!("w09: first_seq")
    }

    /// Sequence number the next [`append`](Journal::append) will assign.
    pub fn next_seq(&self) -> Seq {
        todo!("w09: next_seq")
    }

    /// Sequence number of the last frame, if any.
    pub fn last_seq(&self) -> Option<Seq> {
        todo!("w09: last_seq")
    }

    /// Number of frames held. O(1).
    pub fn len(&self) -> u64 {
        todo!("w09: len")
    }

    /// True if no frames are held.
    pub fn is_empty(&self) -> bool {
        todo!("w09: is_empty")
    }

    /// Size of all frames in bytes.
    pub fn byte_len(&self) -> usize {
        todo!("w09: byte_len")
    }

    /// The raw frames — what a file-backed journal writes to disk.
    pub fn as_bytes(&self) -> &[u8] {
        todo!("w09: as_bytes")
    }

    /// Gives up the raw frames without copying.
    pub fn into_bytes(self) -> Vec<u8> {
        todo!("w09: into_bytes")
    }

    /// Every frame, in order. See [`Frames`].
    pub fn iter(&self) -> Frames<'_> {
        todo!("w09: iter")
    }

    /// Frames with sequence number `>= from`, in order. O(bytes before
    /// `from`) to get started — there is no index (a sparse seq → offset
    /// index is a natural week-18 addition).
    pub fn iter_from(&self, from: Seq) -> Frames<'_> {
        todo!("w09: iter_from")
    }

    /// Drops every frame but keeps the allocation. Sequence numbers are
    /// never reused: the next append still gets
    /// [`next_seq`](Journal::next_seq), which becomes the new first seq.
    /// O(1).
    pub fn clear(&mut self) {
        todo!("w09: clear")
    }
}

impl Default for Journal {
    fn default() -> Self {
        Journal::new()
    }
}

use crate::frame::FrameError;
// TODO(w20): implement — see the week guide on tepy.dev/hft.

#[allow(unused_variables)]
impl Journal {
    /// Appends one complete frame received from elsewhere — a primary's
    /// replication stream (week 20) — after checking that `frame` is exactly
    /// one well-formed frame, that its CRC matches, that its payload decodes
    /// and that it carries [`next_seq`](Journal::next_seq). Returns that
    /// sequence number.
    ///
    /// A refused frame changes nothing; the error says whether it was
    /// damaged, a retransmission ([`FrameError::Duplicate`]) or early
    /// ([`FrameError::Gap`]). O(frame length).
    pub fn append_frame(&mut self, frame: &[u8]) -> Result<Seq, FrameError> {
        todo!("w20: append_frame")
    }
}
