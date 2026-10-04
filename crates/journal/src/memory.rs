//! The in-memory journal: one contiguous buffer of [frames](crate::frame).

use crate::frame::{Corruption, Frames};
use types::{Command, Seq};

// @student-begin w09
use crate::codec::{encode_command, encoded_len, MAX_COMMAND_LEN};
use crate::crc::Crc32;
use crate::frame::FRAME_HEADER_LEN;
// @student-end

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
// @student w09
#[derive(Clone, Debug)]
pub struct Journal {
    /// Back-to-back frames.
    buf: Vec<u8>,
    /// Sequence number of the first frame in `buf`.
    first_seq: Seq,
    /// Sequence number the next append gets.
    next_seq: Seq,
}

// @student w09
impl Journal {
    /// An empty journal whose first command gets [`FIRST_SEQ`](crate::FIRST_SEQ). No
    /// allocation until the first append.
    pub fn new() -> Journal {
        Journal::starting_at(crate::FIRST_SEQ)
    }

    /// An empty journal with room for `bytes` bytes of frames, so appends
    /// do not reallocate until that much has been written.
    pub fn with_capacity(bytes: usize) -> Journal {
        let mut j = Journal::new();
        j.buf.reserve_exact(bytes);
        j
    }

    /// An empty journal whose first command gets `first_seq` — e.g. the
    /// tail that follows a snapshot taken at `first_seq - 1`.
    pub fn starting_at(first_seq: Seq) -> Journal {
        debug_assert!(first_seq.0 >= 1, "Seq(0) is never assigned");
        Journal {
            buf: Vec::new(),
            first_seq,
            next_seq: first_seq,
        }
    }

    /// Rebuilds a journal from raw bytes whose first frame should carry
    /// `first_seq` (e.g. read back after a crash). Keeps the longest valid
    /// prefix and drops everything from the first damaged frame on; the
    /// damage, if any, is returned so the caller can tell a torn last write
    /// ([`Corruption::is_torn_tail`], expected after a crash) from real
    /// corruption. Appends continue after the last good frame.
    /// O(bytes); reuses `bytes`' allocation.
    pub fn recover(bytes: Vec<u8>, first_seq: Seq) -> (Journal, Option<Corruption>) {
        let mut frames = Frames::new(&bytes, first_seq);
        // The iterator stops after the first error, so this walks the
        // whole valid prefix.
        let damage = frames.by_ref().find_map(Result::err);
        let (valid, next_seq) = (frames.offset(), frames.next_seq());
        let mut buf = bytes;
        buf.truncate(valid);
        (
            Journal {
                buf,
                first_seq,
                next_seq,
            },
            damage,
        )
    }

    /// Appends `cmd` as one frame and returns the sequence number it was
    /// assigned. O(1) amortized: encodes into a stack buffer, checksums
    /// header and payload, and copies one frame into the journal.
    #[inline]
    pub fn append(&mut self, cmd: &Command) -> Seq {
        let seq = self.next_seq;
        let mut frame = [0u8; FRAME_HEADER_LEN + MAX_COMMAND_LEN];
        let (header, body) = frame.split_at_mut(FRAME_HEADER_LEN);
        let body: &mut [u8; MAX_COMMAND_LEN] = body.try_into().expect("sized above");
        let len = encode_command(cmd, body);
        debug_assert_eq!(len, encoded_len(cmd));
        header[0..4].copy_from_slice(&(len as u32).to_le_bytes());
        header[4..12].copy_from_slice(&seq.0.to_le_bytes());
        let mut crc = Crc32::new();
        crc.update(&header[0..12]);
        crc.update(&body[..len]);
        header[12..16].copy_from_slice(&crc.finish().to_le_bytes());
        self.buf.extend_from_slice(&frame[..FRAME_HEADER_LEN + len]);
        self.next_seq = Seq(seq.0 + 1);
        seq
    }

    /// Sequence number of the first frame held (equal to
    /// [`next_seq`](Journal::next_seq) when empty).
    pub fn first_seq(&self) -> Seq {
        self.first_seq
    }

    /// Sequence number the next [`append`](Journal::append) will assign.
    pub fn next_seq(&self) -> Seq {
        self.next_seq
    }

    /// Sequence number of the last frame, if any.
    pub fn last_seq(&self) -> Option<Seq> {
        (self.next_seq > self.first_seq).then(|| Seq(self.next_seq.0 - 1))
    }

    /// Number of frames held. O(1).
    pub fn len(&self) -> u64 {
        self.next_seq.0 - self.first_seq.0
    }

    /// True if no frames are held.
    pub fn is_empty(&self) -> bool {
        self.next_seq == self.first_seq
    }

    /// Size of all frames in bytes.
    pub fn byte_len(&self) -> usize {
        self.buf.len()
    }

    /// The raw frames — what a file-backed journal writes to disk.
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Gives up the raw frames without copying.
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    /// Every frame, in order. See [`Frames`].
    pub fn iter(&self) -> Frames<'_> {
        Frames::new(&self.buf, self.first_seq)
    }

    /// Frames with sequence number `>= from`, in order. O(bytes before
    /// `from`) to get started — there is no index (a sparse seq → offset
    /// index is a natural week-18 addition).
    pub fn iter_from(&self, from: Seq) -> Frames<'_> {
        self.iter().skip_to(from)
    }

    /// Drops every frame but keeps the allocation. Sequence numbers are
    /// never reused: the next append still gets
    /// [`next_seq`](Journal::next_seq), which becomes the new first seq.
    /// O(1).
    pub fn clear(&mut self) {
        self.buf.clear();
        self.first_seq = self.next_seq;
    }
}

impl Default for Journal {
    fn default() -> Self {
        Journal::new()
    }
}
