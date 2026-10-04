//! Sparse sequence-number → byte-offset index (week 18).
//!
//! Frames are variable length, so finding sequence number `s` in raw
//! journal bytes means walking every header before it — O(journal) for a
//! read that wants only the tail (recovery after a snapshot, a standby
//! catching up). Remembering the offset of every `stride`-th frame bounds
//! that walk to `stride - 1` frames, for 8 bytes of memory per `stride`
//! frames (1 MB per 128M commands at the default stride of 1024).
//!
//! Sequence numbers are dense, so the entry for `s` is found by arithmetic
//! (`(s - first) / stride`), not by search.

use types::Seq;

/// Byte offsets of every `stride`-th frame of a journal, starting with its
/// first frame.
///
/// ```
/// use journal::SeqIndex;
/// use types::Seq;
/// let mut ix = SeqIndex::new(Seq(1), 4);
/// for (k, seq) in (1..=10).enumerate() {
///     ix.record(Seq(seq), 100 * k as u64); // frame `seq` starts at byte 100·k
/// }
/// assert_eq!(ix.len(), 3); // seqs 1, 5 and 9
/// assert_eq!(ix.lookup(Seq(7)), Some((Seq(5), 400)));
/// assert_eq!(ix.lookup(Seq(10)), Some((Seq(9), 800)));
/// assert_eq!(ix.lookup(Seq(0)), None);
/// ```
// @student w18
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeqIndex {
    /// Sequence number of the journal's first frame (entry 0).
    first: Seq,
    stride: u64,
    /// `offsets[k]` = offset of frame `first + k * stride`.
    offsets: Vec<u64>,
}

// @student w18
impl SeqIndex {
    /// An empty index for a journal whose first frame carries `first`,
    /// keeping one entry every `stride` frames. No allocation until the
    /// first entry.
    ///
    /// # Panics
    /// If `stride` is 0.
    pub fn new(first: Seq, stride: u64) -> SeqIndex {
        assert!(stride > 0, "index stride must be positive");
        SeqIndex {
            first,
            stride,
            offsets: Vec::new(),
        }
    }

    /// Notes that frame `seq` starts at byte `offset`. Call it for every
    /// frame, in order; only every `stride`-th is kept. O(1) amortized.
    #[inline]
    pub fn record(&mut self, seq: Seq, offset: u64) {
        let Some(k) = seq.0.checked_sub(self.first.0) else {
            return;
        };
        if k % self.stride == 0 {
            debug_assert_eq!(
                k / self.stride,
                self.offsets.len() as u64,
                "frames out of order"
            );
            self.offsets.push(offset);
        }
    }

    /// The indexed frame at or before `seq`: its sequence number and byte
    /// offset, the place to start walking frames to reach `seq`. `None` if
    /// `seq` precedes the first frame or nothing is indexed yet. Past the
    /// last entry this returns the last entry. O(1).
    #[inline]
    pub fn lookup(&self, seq: Seq) -> Option<(Seq, u64)> {
        let k = seq.0.checked_sub(self.first.0)? / self.stride;
        let k = k.min(self.offsets.len().checked_sub(1)? as u64);
        Some((
            Seq(self.first.0 + k * self.stride),
            self.offsets[k as usize],
        ))
    }

    /// Forgets the entries of frames `>= from` (after the journal was cut
    /// back to just before `from`). O(1).
    pub fn truncate(&mut self, from: Seq) {
        let keep = from.0.saturating_sub(self.first.0).div_ceil(self.stride);
        self.offsets.truncate(keep as usize);
    }

    /// Sequence number of entry 0.
    pub fn first(&self) -> Seq {
        self.first
    }

    /// Frames between entries.
    pub fn stride(&self) -> u64 {
        self.stride
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    /// True if nothing is indexed.
    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }
}
