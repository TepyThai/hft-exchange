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

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::{DepthBook, L2Snapshot, L2Update, Level, MapDepth};
use types::{Price, Qty, Seq, Side};

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
///
/// Generic over the depth representation `D` (week 29): the default
/// [`MapDepth`] works for any price; [`LadderDepth`](crate::LadderDepth)
/// trades memory for dense, vectorizable arrays (see
/// [`with_depth`](BookBuilder::with_depth)).
#[derive(Clone, Debug)]
pub struct BookBuilder<D = MapDepth> {
    // TODO(w10): design this type's fields.
    _todo: std::marker::PhantomData<(D,)>,
}

#[allow(unused_variables)]
impl BookBuilder {
    /// A builder over [`MapDepth`] that has seen nothing yet (out of sync).
    pub fn new() -> BookBuilder {
        todo!("w10: new")
    }
}

impl Default for BookBuilder {
    fn default() -> Self {
        BookBuilder::new()
    }
}

#[allow(unused_variables)]
impl<D: DepthBook> BookBuilder<D> {
    /// A builder that keeps its book in `depth` (cleared first) and has
    /// seen nothing yet (out of sync).
    pub fn with_depth(depth: D) -> BookBuilder<D> {
        todo!("w10: with_depth")
    }

    /// The depth the book is kept in — for queries beyond this API (the
    /// ladder's vectorized aggregates, for example).
    pub fn depth_book(&self) -> &D {
        todo!("w10: depth_book")
    }

    /// Applies one incremental. In sync and next in sequence: one
    /// [`DepthBook::set`] (O(log levels) for [`MapDepth`]). Otherwise it is buffered (amortized O(1)) or ignored.
    #[inline]
    pub fn apply_update(&mut self, u: &L2Update) -> UpdateOutcome {
        todo!("w10: apply_update")
    }

    /// Applies a full snapshot (see [`SnapshotOutcome`]), then whatever
    /// buffered updates continue from it. O(levels + buffered).
    pub fn apply_snapshot(&mut self, snap: &L2Snapshot) -> SnapshotOutcome {
        todo!("w10: apply_snapshot")
    }

    /// True if the book reflects every update up to
    /// [`last_seq`](BookBuilder::last_seq) and no gap is pending.
    pub fn is_synced(&self) -> bool {
        todo!("w10: is_synced")
    }

    /// Feed sequence number the book reflects (`None` before the first
    /// snapshot).
    pub fn last_seq(&self) -> Option<Seq> {
        todo!("w10: last_seq")
    }

    /// Number of gaps detected so far (each loss of sync counts once).
    pub fn gaps(&self) -> u64 {
        todo!("w10: gaps")
    }

    /// Number of updates buffered while waiting for a snapshot.
    pub fn buffered(&self) -> usize {
        todo!("w10: buffered")
    }

    /// Highest bid level. O(log levels) with [`MapDepth`], O(1) with the
    /// ladder.
    pub fn best_bid(&self) -> Option<Level> {
        todo!("w10: best_bid")
    }

    /// Lowest ask level. O(log levels) with [`MapDepth`], O(1) with the
    /// ladder.
    pub fn best_ask(&self) -> Option<Level> {
        todo!("w10: best_ask")
    }

    /// Aggregate quantity at one level (zero if absent). O(log levels)
    /// with [`MapDepth`], O(1) with the ladder.
    pub fn level_qty(&self, side: Side, price: Price) -> Qty {
        todo!("w10: level_qty")
    }

    /// The best `n` levels of one side, best first. O(n), allocates.
    pub fn depth(&self, side: Side, n: usize) -> Vec<Level> {
        todo!("w10: depth")
    }

    /// Number of non-empty levels on one side.
    pub fn level_count(&self, side: Side) -> usize {
        todo!("w10: level_count")
    }

    /// The whole book as a snapshot at [`last_seq`](BookBuilder::last_seq)
    /// (`Seq(0)` before any snapshot) — comparable with
    /// [`L2Publisher::snapshot`](crate::L2Publisher::snapshot).
    /// O(levels), allocates.
    pub fn snapshot(&self) -> L2Snapshot {
        todo!("w10: snapshot")
    }
}
