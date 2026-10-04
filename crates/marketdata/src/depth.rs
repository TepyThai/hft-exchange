//! Aggregated depth by price for both sides of one book — the state the
//! publisher maintains and the builder rebuilds.

// @student-begin w10
use crate::{L2Snapshot, Level};
use std::collections::BTreeMap;
use types::{Price, Qty, Seq, Side};

/// Price → aggregate quantity, one ordered map per side. Every operation is
/// O(log levels); levels with zero quantity are never stored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Depth {
    bids: BTreeMap<Price, Qty>,
    asks: BTreeMap<Price, Qty>,
}

impl Depth {
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

    /// Adds `qty` to a level; returns the new aggregate.
    #[inline]
    pub(crate) fn add(&mut self, side: Side, price: Price, qty: Qty) -> Qty {
        let level = self.side_mut(side).entry(price).or_default();
        *level += qty;
        *level
    }

    /// Removes `qty` from a level (saturating); returns the new aggregate,
    /// removing the level when it reaches zero.
    #[inline]
    pub(crate) fn sub(&mut self, side: Side, price: Price, qty: Qty) -> Qty {
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

    /// Sets a level to `qty` (zero removes it).
    #[inline]
    pub(crate) fn set(&mut self, side: Side, price: Price, qty: Qty) {
        let map = self.side_mut(side);
        if qty.is_zero() {
            map.remove(&price);
        } else {
            map.insert(price, qty);
        }
    }

    pub(crate) fn get(&self, side: Side, price: Price) -> Qty {
        self.side(side).get(&price).copied().unwrap_or_default()
    }

    pub(crate) fn best(&self, side: Side) -> Option<Level> {
        let best = match side {
            Side::Buy => self.bids.last_key_value(),
            Side::Sell => self.asks.first_key_value(),
        };
        best.map(|(&price, &qty)| Level { price, qty })
    }

    pub(crate) fn level_count(&self, side: Side) -> usize {
        self.side(side).len()
    }

    /// The best `n` levels of one side, best first.
    pub(crate) fn top(&self, side: Side, n: usize) -> Vec<Level> {
        let level = |(&price, &qty): (&Price, &Qty)| Level { price, qty };
        match side {
            Side::Buy => self.bids.iter().rev().take(n).map(level).collect(),
            Side::Sell => self.asks.iter().take(n).map(level).collect(),
        }
    }

    pub(crate) fn snapshot(&self, seq: Seq, n: usize) -> L2Snapshot {
        L2Snapshot {
            seq,
            bids: self.top(Side::Buy, n),
            asks: self.top(Side::Sell, n),
        }
    }

    /// Replaces the whole book with a snapshot's levels.
    pub(crate) fn load(&mut self, snap: &L2Snapshot) {
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
// @student-end
