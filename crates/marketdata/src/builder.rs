//! Client side: rebuild a book from snapshots + incrementals, detect gaps,
//! recover.
//!
//! ```text
//!            snapshot (seq s)                  update s+1, s+2, …
//!  Recovering ───────────────────────▶ Synced ─────────────────────▶ Synced
//!      ▲  │ updates: buffered                │ update with seq > last+1
//!      │  └─────────────────────             │ (a gap: packets were lost)
//!      └─────────────────────────────────────┘ → buffer it, wait for a
//!                                              snapshot newer than last
//! ```
//!
//! On a snapshot the builder replaces its book, drops buffered updates the
//! snapshot already covers (`seq <= s`) and applies the rest if they
//! continue from `s + 1` without a hole. If they do not, it is still out
//! of sync and keeps waiting for a newer snapshot.
//!
//! Updates are assumed to arrive in sequence order, possibly with losses
//! and duplicates (A/B feed arbitration delivers both). Reordering is week
//! 46's reorder buffer.

use crate::{L2Snapshot, L2Update, Level};
use types::{Price, Qty, Seq, Side};

// @student-begin w10
use crate::depth::Depth;
// @student-end

/// What [`BookBuilder::apply_update`] did with an update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// It was the next in sequence and is now reflected in the book.
    Applied,
    /// The book (or the buffer) already covers this sequence number; it was
    /// ignored.
    Duplicate,
    /// Sequence numbers `expected .. received` are missing. The book is
    /// stale from now on; this update was buffered and a snapshot is needed.
    Gap { expected: Seq, received: Seq },
    /// Waiting for a snapshot; the update was buffered.
    Buffered,
}

/// What [`BookBuilder::apply_snapshot`] did with a snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotOutcome {
    /// The book already reflects this snapshot's sequence number or a later
    /// one; it was ignored.
    Stale,
    /// The book was replaced: `dropped` buffered updates were already
    /// covered by the snapshot, `replayed` newer ones were applied on top.
    /// The builder is in sync.
    Synced { dropped: usize, replayed: usize },
    /// The book was replaced and `replayed` buffered updates applied, but
    /// then `expected` is missing (the next buffered one is `received`).
    /// Still out of sync: a newer snapshot is needed.
    StillGapped {
        dropped: usize,
        replayed: usize,
        expected: Seq,
        received: Seq,
    },
}

/// A client's copy of one book, rebuilt from an L2 feed.
///
/// Starts out of sync: nothing is known until the first snapshot (a client
/// that listens from the very start of the feed can apply
/// `L2Snapshot::default()`, the empty book at `Seq(0)`). While out of
/// sync, the query methods return the last known — stale — book; check
/// [`is_synced`](BookBuilder::is_synced).
// @student w10
#[derive(Clone, Debug, Default)]
pub struct BookBuilder {
    depth: Depth,
    /// Feed sequence number the book reflects; `None` before any snapshot.
    seq: Option<Seq>,
    synced: bool,
    /// Updates received while out of sync, in arrival (= sequence) order.
    buffer: Vec<L2Update>,
    gaps: u64,
}

// @student w10
impl BookBuilder {
    /// A builder that has seen nothing yet (out of sync).
    pub fn new() -> BookBuilder {
        BookBuilder {
            depth: Depth::default(),
            seq: None,
            synced: false,
            buffer: Vec::new(),
            gaps: 0,
        }
    }

    /// Applies one incremental. In sync and next in sequence: O(log
    /// levels). Otherwise it is buffered (amortized O(1)) or ignored.
    #[inline]
    pub fn apply_update(&mut self, u: &L2Update) -> UpdateOutcome {
        if self.synced {
            let last = self.seq.unwrap_or_default();
            if u.seq <= last {
                return UpdateOutcome::Duplicate;
            }
            let expected = Seq(last.0 + 1);
            if u.seq == expected {
                self.depth.set(u.side, u.price, u.qty);
                self.seq = Some(u.seq);
                return UpdateOutcome::Applied;
            }
            self.synced = false;
            self.gaps += 1;
            self.buffer.push(*u);
            return UpdateOutcome::Gap {
                expected,
                received: u.seq,
            };
        }
        let covered = self.seq.is_some_and(|s| u.seq <= s)
            || self.buffer.last().is_some_and(|b| u.seq <= b.seq);
        if covered {
            return UpdateOutcome::Duplicate;
        }
        self.buffer.push(*u);
        UpdateOutcome::Buffered
    }

    /// Applies a full snapshot (see [`SnapshotOutcome`]), then whatever
    /// buffered updates continue from it. O(levels + buffered).
    pub fn apply_snapshot(&mut self, snap: &L2Snapshot) -> SnapshotOutcome {
        if self.seq.is_some_and(|s| snap.seq <= s) {
            return SnapshotOutcome::Stale;
        }
        self.depth.load(snap);
        let mut last = snap.seq;
        let (mut dropped, mut replayed) = (0, 0);
        for (i, u) in self.buffer.iter().enumerate() {
            if u.seq <= last {
                dropped += 1;
            } else if u.seq.0 == last.0 + 1 {
                self.depth.set(u.side, u.price, u.qty);
                last = u.seq;
                replayed += 1;
            } else {
                let (expected, received) = (Seq(last.0 + 1), u.seq);
                self.buffer.drain(..i);
                self.seq = Some(last);
                self.synced = false;
                self.gaps += 1;
                return SnapshotOutcome::StillGapped {
                    dropped,
                    replayed,
                    expected,
                    received,
                };
            }
        }
        self.buffer.clear();
        self.seq = Some(last);
        self.synced = true;
        SnapshotOutcome::Synced { dropped, replayed }
    }

    /// True if the book reflects every update up to
    /// [`last_seq`](BookBuilder::last_seq) and no gap is pending.
    pub fn is_synced(&self) -> bool {
        self.synced
    }

    /// Feed sequence number the book reflects (`None` before the first
    /// snapshot).
    pub fn last_seq(&self) -> Option<Seq> {
        self.seq
    }

    /// Number of gaps detected so far (each loss of sync counts once).
    pub fn gaps(&self) -> u64 {
        self.gaps
    }

    /// Number of updates buffered while waiting for a snapshot.
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }

    /// Highest bid level. O(log levels).
    pub fn best_bid(&self) -> Option<Level> {
        self.depth.best(Side::Buy)
    }

    /// Lowest ask level. O(log levels).
    pub fn best_ask(&self) -> Option<Level> {
        self.depth.best(Side::Sell)
    }

    /// Aggregate quantity at one level (zero if absent). O(log levels).
    pub fn level_qty(&self, side: Side, price: Price) -> Qty {
        self.depth.get(side, price)
    }

    /// The best `n` levels of one side, best first. O(n), allocates.
    pub fn depth(&self, side: Side, n: usize) -> Vec<Level> {
        self.depth.top(side, n)
    }

    /// Number of non-empty levels on one side.
    pub fn level_count(&self, side: Side) -> usize {
        self.depth.level_count(side)
    }

    /// The whole book as a snapshot at [`last_seq`](BookBuilder::last_seq)
    /// (`Seq(0)` before any snapshot) — comparable with
    /// [`L2Publisher::snapshot`](crate::L2Publisher::snapshot).
    /// O(levels), allocates.
    pub fn snapshot(&self) -> L2Snapshot {
        self.depth
            .snapshot(self.seq.unwrap_or_default(), usize::MAX)
    }
}
