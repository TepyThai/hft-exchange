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
// @student w10
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MapDepth {
    bids: BTreeMap<Price, Qty>,
    asks: BTreeMap<Price, Qty>,
}

// @student-begin w10
use std::collections::BTreeMap;

impl MapDepth {
    #[inline]
    fn side(&self, side: Side) -> &BTreeMap<Price, Qty> {
        match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        }
    }

    #[inline]
    fn side_mut(&mut self, side: Side) -> &mut BTreeMap<Price, Qty> {
        match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        }
    }
}
// @student-end

// @student w10
impl DepthBook for MapDepth {
    #[inline]
    fn add(&mut self, side: Side, price: Price, qty: Qty) -> Qty {
        if qty.is_zero() {
            // Must not create an empty level.
            return self.get(side, price);
        }
        let level = self.side_mut(side).entry(price).or_default();
        *level += qty;
        *level
    }

    #[inline]
    fn sub(&mut self, side: Side, price: Price, qty: Qty) -> Qty {
        let map = self.side_mut(side);
        let Some(level) = map.get_mut(&price) else {
            return Qty::ZERO;
        };
        *level = level.saturating_sub(qty);
        let left = *level;
        if left.is_zero() {
            map.remove(&price);
        }
        left
    }

    #[inline]
    fn set(&mut self, side: Side, price: Price, qty: Qty) {
        let map = self.side_mut(side);
        if qty.is_zero() {
            map.remove(&price);
        } else {
            map.insert(price, qty);
        }
    }

    fn get(&self, side: Side, price: Price) -> Qty {
        self.side(side).get(&price).copied().unwrap_or_default()
    }

    fn best(&self, side: Side) -> Option<Level> {
        let best = match side {
            Side::Buy => self.bids.last_key_value(),
            Side::Sell => self.asks.first_key_value(),
        };
        best.map(|(&price, &qty)| Level { price, qty })
    }

    fn level_count(&self, side: Side) -> usize {
        self.side(side).len()
    }

    fn top(&self, side: Side, n: usize) -> Vec<Level> {
        let level = |(&price, &qty): (&Price, &Qty)| Level { price, qty };
        match side {
            Side::Buy => self.bids.iter().rev().take(n).map(level).collect(),
            Side::Sell => self.asks.iter().take(n).map(level).collect(),
        }
    }

    fn load(&mut self, snap: &L2Snapshot) {
        let levels = |ls: &[Level]| {
            ls.iter()
                .filter(|l| !l.qty.is_zero())
                .map(|l| (l.price, l.qty))
                .collect()
        };
        self.bids = levels(&snap.bids);
        self.asks = levels(&snap.asks);
    }
}
