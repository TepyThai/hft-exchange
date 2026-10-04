//! Aggregated depth by price for both sides of one book — the state the
//! publisher maintains and the builder rebuilds.
//!
//! [`DepthBook`] is the interface; [`L2Publisher`](crate::L2Publisher) and
//! [`BookBuilder`](crate::BookBuilder) are generic over it. Two
//! implementations ship:
//!
//! * [`MapDepth`] (week 10, the default): one ordered map per side. Simple,
//!   any price, O(log levels) per operation.
//! * [`LadderDepth`](crate::LadderDepth) (week 29): dense quantity arrays
//!   indexed by price around the touch, scanned with SIMD kernels.
//!
//! Both must give identical answers for every sequence of operations; the
//! week 29 tests check that differentially.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::{L2Snapshot, Level};
use std::fmt;
use types::{Price, Qty, Seq, Side};

/// Price → aggregate resting quantity, for both sides of one book.
///
/// Levels with zero quantity do not exist: setting a level to zero removes
/// it, and queries never return one.
pub trait DepthBook: Clone + fmt::Debug {
    /// Adds `qty` to a level (creating it) and returns its new total.
    fn add(&mut self, side: Side, price: Price, qty: Qty) -> Qty;

    /// Removes up to `qty` from a level (saturating at zero) and returns
    /// its new total; a level that reaches zero is removed. Returns
    /// [`Qty::ZERO`] and changes nothing if the level does not exist.
    fn sub(&mut self, side: Side, price: Price, qty: Qty) -> Qty;

    /// Sets a level's total (zero removes it).
    fn set(&mut self, side: Side, price: Price, qty: Qty);

    /// Total quantity at one level (zero if absent).
    fn get(&self, side: Side, price: Price) -> Qty;

    /// The best level of a side: highest bid, lowest ask.
    fn best(&self, side: Side) -> Option<Level>;

    /// Number of non-empty levels on one side.
    fn level_count(&self, side: Side) -> usize;

    /// The best `n` levels of one side, best first. Allocates.
    fn top(&self, side: Side, n: usize) -> Vec<Level>;

    /// Replaces the whole book with a snapshot's levels (levels with zero
    /// quantity are skipped).
    fn load(&mut self, snap: &L2Snapshot);

    /// The best `n` levels per side as a snapshot stamped `seq`. Allocates.
    fn snapshot(&self, seq: Seq, n: usize) -> L2Snapshot {
        L2Snapshot {
            seq,
            bids: self.top(Side::Buy, n),
            asks: self.top(Side::Sell, n),
        }
    }
}

/// [`DepthBook`] as one ordered map per side: every operation is
/// O(log levels), any price is fine, and levels with zero quantity are
/// never stored. The default depth of the publisher and the builder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MapDepth {
    // TODO(w10): design this type's fields.
    _todo: (),
}

// TODO(w10): implement — see the week guide on tepy.dev/hft.

#[allow(unused_variables)]
impl DepthBook for MapDepth {
    #[inline]
    fn add(&mut self, side: Side, price: Price, qty: Qty) -> Qty {
        todo!("w10: add")
    }

    #[inline]
    fn sub(&mut self, side: Side, price: Price, qty: Qty) -> Qty {
        todo!("w10: sub")
    }

    #[inline]
    fn set(&mut self, side: Side, price: Price, qty: Qty) {
        todo!("w10: set")
    }

    fn get(&self, side: Side, price: Price) -> Qty {
        todo!("w10: get")
    }

    fn best(&self, side: Side) -> Option<Level> {
        todo!("w10: best")
    }

    fn level_count(&self, side: Side) -> usize {
        todo!("w10: level_count")
    }

    fn top(&self, side: Side, n: usize) -> Vec<Level> {
        todo!("w10: top")
    }

    fn load(&mut self, snap: &L2Snapshot) {
        todo!("w10: load")
    }
}
