//! Week 16 — conflation: the latest state per level for a slow consumer.
//!
//! An [`L2Update`] *sets* a level, so for a consumer that cannot keep up,
//! only the newest update of each level matters: queue one entry per
//! `(side, price)` and overwrite it in place. After the consumer drains the
//! queue its book equals the full-stream book as of the highest sequence
//! number it has seen; in between it may show combinations of levels that
//! never coexisted, so a consumer only trusts its book at drain points.
//!
//! Pending levels leave in the order they **first** became dirty, so a
//! level that changes constantly cannot starve the others; once popped, a
//! level that changes again goes to the back. Deletes (qty 0) are delivered
//! like any other value. Memory is bounded by the number of distinct
//! levels, not the number of updates.
//!
//! Trades are events, not state, and must never be conflated.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::L2Update;

// TODO(w16): implement — see the week guide on tepy.dev/hft.

/// A per-consumer conflating queue of L2 levels (see the module docs).
#[derive(Clone, Debug, Default)]
pub struct Conflator {
    // TODO(w16): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl Conflator {
    /// An empty queue.
    pub fn new() -> Conflator {
        todo!("w16: new")
    }

    /// Records an update. If its level is already pending, the pending
    /// value is replaced in place (its position does not change);
    /// otherwise the level joins the back. O(1) expected; allocates only
    /// when a new level needs room.
    pub fn push(&mut self, u: L2Update) {
        todo!("w16: push")
    }

    /// The pending level that became dirty earliest, carrying its latest
    /// quantity and sequence number. O(1) expected.
    pub fn pop(&mut self) -> Option<L2Update> {
        todo!("w16: pop")
    }

    /// Number of distinct pending levels.
    pub fn len(&self) -> usize {
        todo!("w16: len")
    }

    /// True if nothing is pending.
    pub fn is_empty(&self) -> bool {
        todo!("w16: is_empty")
    }

    /// Drops everything pending (after a resync from a snapshot). Keeps the
    /// allocations.
    pub fn clear(&mut self) {
        todo!("w16: clear")
    }
}
