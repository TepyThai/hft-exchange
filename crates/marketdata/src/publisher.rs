//! Engine events → sequenced L2 incrementals and snapshots.
//!
//! # Why an L2 publisher tracks every order
//!
//! L2 is aggregated by price, but the events that change it are about
//! *orders*, and most of them do not say where the order rests:
//!
//! * `Cancelled { id, remaining }` and `Expired { id, remaining }` carry
//!   neither side nor price — only the order's own record knows which
//!   level to reduce.
//! * `Reduced { id, remaining }` carries the order's *new* quantity, not the
//!   change; the level moves by `old - new`, so the old quantity must be
//!   known.
//! * `Expired` is also emitted for IOC / market remainders that never
//!   rested; those must not touch the book at all, and only a lookup tells
//!   the two cases apart.
//! * A `Trade` names the maker and its price, but whether the maker is now
//!   fully filled (and must be forgotten) depends on its remaining
//!   quantity.
//!
//! So the publisher keeps an L3 view — `order id → (side, price,
//! remaining)` — next to the L2 depth it publishes. It is the same
//! information a consumer of a by-order (L3) feed would have to keep.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::{DepthBook, L2Snapshot, L2Update, Level, MapDepth};
use types::{Event, OrderId, Price, Qty, Seq, Side};

// TODO(w10): implement — see the week guide on tepy.dev/hft.

/// Turns one instrument's engine [`Event`]s into an L2 feed.
///
/// Feed it every event the engine emits for the instrument, in order, from
/// the start (it cannot see orders that rested before it was created).
/// Memory is O(resting orders + price levels).
///
/// Generic over the depth representation `D` (week 29), like
/// [`BookBuilder`](crate::BookBuilder); see
/// [`with_depth`](L2Publisher::with_depth).
#[derive(Clone, Debug)]
pub struct L2Publisher<D = MapDepth> {
    // TODO(w10): design this type's fields.
    _todo: std::marker::PhantomData<(D,)>,
}

#[allow(unused_variables)]
impl L2Publisher {
    /// An empty [`MapDepth`] book with no periodic snapshots.
    pub fn new() -> L2Publisher {
        todo!("w10: new")
    }

    /// An empty [`MapDepth`] book that offers a full snapshot through
    /// [`periodic_snapshot`](L2Publisher::periodic_snapshot) every
    /// `updates` updates (0 = never).
    pub fn with_snapshot_interval(updates: u64) -> L2Publisher {
        todo!("w10: with_snapshot_interval")
    }
}

#[allow(unused_variables)]
impl<D: DepthBook> L2Publisher<D> {
    /// An empty book kept in `depth` (cleared first), with a periodic
    /// snapshot every `snapshot_interval` updates (0 = never).
    pub fn with_depth(depth: D, snapshot_interval: u64) -> L2Publisher<D> {
        todo!("w10: with_depth")
    }

    /// The depth the book is kept in.
    pub fn depth_book(&self) -> &D {
        todo!("w10: depth_book")
    }

    /// Applies one engine event and returns the level change it causes, if
    /// any — at most one: `Rested`, `Trade` (the maker's level), `Cancelled`,
    /// `Reduced`, and `Expired` of a resting order each change exactly one
    /// level; `Accepted`, `Rejected` and `Expired` of an order that never
    /// rested change none. Events about unknown orders are ignored.
    ///
    /// O(1) expected for the order lookup plus one [`DepthBook`] operation
    /// for the level (O(log levels) with [`MapDepth`]); allocates only when
    /// a new order or level needs room.
    #[inline]
    pub fn on_event(&mut self, ev: &Event) -> Option<L2Update> {
        todo!("w10: on_event")
    }

    /// Sequence number of the last update emitted (`Seq(0)` before the
    /// first).
    pub fn last_seq(&self) -> Seq {
        todo!("w10: last_seq")
    }

    /// The full book as of [`last_seq`](L2Publisher::last_seq). O(levels),
    /// allocates the two level vectors.
    pub fn snapshot(&self) -> L2Snapshot {
        todo!("w10: snapshot")
    }

    /// The best `depth` levels per side as of
    /// [`last_seq`](L2Publisher::last_seq) — for display. A consumer
    /// cannot rebuild a full book from a truncated snapshot.
    /// O(depth), allocates.
    pub fn snapshot_top(&self, depth: usize) -> L2Snapshot {
        todo!("w10: snapshot_top")
    }

    /// A full snapshot if at least `snapshot_interval` updates have been
    /// emitted since the last one this method returned; `None` otherwise
    /// (or if periodic snapshots are off). Call it after each event.
    pub fn periodic_snapshot(&mut self) -> Option<L2Snapshot> {
        todo!("w10: periodic_snapshot")
    }

    /// Aggregate quantity at one level (zero if absent). O(log levels)
    /// with [`MapDepth`].
    pub fn level_qty(&self, side: Side, price: Price) -> Qty {
        todo!("w10: level_qty")
    }

    /// Highest bid level. O(log levels) with [`MapDepth`].
    pub fn best_bid(&self) -> Option<Level> {
        todo!("w10: best_bid")
    }

    /// Lowest ask level. O(log levels) with [`MapDepth`].
    pub fn best_ask(&self) -> Option<Level> {
        todo!("w10: best_ask")
    }

    /// Number of non-empty levels on one side.
    pub fn level_count(&self, side: Side) -> usize {
        todo!("w10: level_count")
    }

    /// Number of resting orders tracked.
    pub fn order_count(&self) -> usize {
        todo!("w10: order_count")
    }

    /// Remaining quantity of a resting order (the L3 view), if it rests.
    pub fn resting_qty(&self, id: OrderId) -> Option<Qty> {
        todo!("w10: resting_qty")
    }
}

impl Default for L2Publisher {
    fn default() -> Self {
        L2Publisher::new()
    }
}
