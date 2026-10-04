//! Week 29 — a dense price ladder for L2 depth, scanned with SIMD.
//!
//! [`MapDepth`](crate::MapDepth) answers "quantity at price p" with a walk
//! through tree nodes. A ladder answers it with one subtraction and one
//! load: each side is an array of quantities indexed by price over a
//! window of `len` consecutive prices around the touch.
//!
//! ```text
//! prices      lo  lo+1  …                 …  hi-1  hi        (hi = lo + len - 1)
//! asks[i]     i = price - lo       →  index 0 is the lowest (best) ask price
//! bids[i]     i = hi - price       →  index 0 is the highest (best) bid price
//! ```
//!
//! Both arrays grow *away from the touch* with the index, so "walk the book
//! from the best level outward" is a forward scan over contiguous memory
//! for either side — exactly the shape the kernels in [`crate::simd`]
//! vectorize: depth over N levels is a sum, the next level after the best
//! is removed is a first-non-zero search, the sweep price of an order is a
//! two-phase cumulative search.
//!
//! * **Best level** is cached as an index; removing it rescans forward
//!   from there (usually a few slots, vectorized when the book is sparse).
//! * **Prices outside the window** go to a sparse ordered map per side, so
//!   any price works and every answer equals `MapDepth`'s.
//! * **Re-centring**: when a level outside the window becomes the best of
//!   its side (the market moved), the window is re-centred on the touch —
//!   O(window + levels), a cold path. Applying a snapshot centres it too.
//!
//! Every price is a slot: with an instrument tick of 5, four in five slots
//! stay empty (the scans skip them cheaply). Memory: `2 × len × 8` bytes.

use crate::simd::Kernels;
use crate::{DepthBook, L2Snapshot, Level};
use types::{Price, Qty, Side};

// @student-begin w29
use std::collections::BTreeMap;

const NONE: usize = usize::MAX;

#[inline]
fn si(side: Side) -> usize {
    match side {
        Side::Buy => 0,
        Side::Sell => 1,
    }
}

/// `a` is a better price than `b` for `side`.
#[inline]
fn better(side: Side, a: Price, b: Price) -> bool {
    match side {
        Side::Buy => a > b,
        Side::Sell => a < b,
    }
}

fn midpoint(a: Price, b: Price) -> Price {
    Price(((a.0 as i128 + b.0 as i128) / 2) as i64)
}
// @student-end

/// Default window: 4,096 prices per side (32 KiB each — L1/L2 resident).
pub const DEFAULT_WINDOW: usize = 4096;

/// [`DepthBook`] as dense quantity arrays around the touch, with a sparse
/// fallback outside the window (see the module docs). `get`/`set`/`add`/
/// `sub` are O(1) inside the window (plus a forward scan when the best
/// level empties) and O(log n) outside it; `best` is O(1) plus a map peek.
// @student w29
#[derive(Clone)]
pub struct LadderDepth {
    /// Lowest price in the window; meaningless until `anchored`.
    lo: i64,
    len: usize,
    anchored: bool,
    /// Quantity per price, best first: `[bids, asks]` (see module docs).
    qty: [Vec<u64>; 2],
    /// Index of each side's best level in the window, or `NONE`.
    best: [usize; 2],
    /// Non-empty levels in the window, per side.
    count: [usize; 2],
    /// Levels outside the window, per side.
    overflow: [BTreeMap<Price, Qty>; 2],
    recenters: u64,
    kernels: &'static Kernels,
}

impl Default for LadderDepth {
    fn default() -> Self {
        LadderDepth::new(DEFAULT_WINDOW)
    }
}

// @student w29
impl std::fmt::Debug for LadderDepth {
    /// The window, the best few levels and the counts — not the arrays.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LadderDepth")
            .field("window", &self.window())
            .field("bids", &self.top(Side::Buy, 5))
            .field("asks", &self.top(Side::Sell, 5))
            .field(
                "levels",
                &(self.level_count(Side::Buy), self.level_count(Side::Sell)),
            )
            .field("overflow", &self.overflow_levels())
            .field("kernels", &self.kernels.name())
            .finish()
    }
}

// @student w29
impl LadderDepth {
    /// An empty ladder of `window` prices per side using the fastest
    /// kernels this CPU supports. The window is placed around the first
    /// level that arrives. Allocates `2 × window × 8` bytes.
    ///
    /// # Panics
    /// If `window` is 0.
    pub fn new(window: usize) -> LadderDepth {
        LadderDepth::with_kernels(window, Kernels::detect())
    }

    /// Like [`new`](LadderDepth::new) with explicit kernels (e.g.
    /// [`Kernels::scalar`] as the baseline in benchmarks).
    pub fn with_kernels(window: usize, kernels: &'static Kernels) -> LadderDepth {
        assert!(window > 0, "ladder window must not be empty");
        LadderDepth {
            lo: 0,
            len: window,
            anchored: false,
            qty: [vec![0; window], vec![0; window]],
            best: [NONE; 2],
            count: [0; 2],
            overflow: [BTreeMap::new(), BTreeMap::new()],
            recenters: 0,
            kernels,
        }
    }

    /// The kernels this ladder calls.
    pub fn kernels(&self) -> &'static Kernels {
        self.kernels
    }

    /// The prices the dense window covers (`lo..=hi`), once placed.
    pub fn window(&self) -> Option<(Price, Price)> {
        self.anchored.then(|| (Price(self.lo), Price(self.hi())))
    }

    /// Levels currently kept outside the window (both sides).
    pub fn overflow_levels(&self) -> usize {
        self.overflow[0].len() + self.overflow[1].len()
    }

    /// How many times the window was placed or moved.
    pub fn recenters(&self) -> u64 {
        self.recenters
    }

    /// Moves the window so `mid` sits in its middle, migrating every level
    /// to the arrays or the overflow maps. O(window + levels · log levels);
    /// allocates temporarily.
    pub fn recenter(&mut self, mid: Price) {
        let mut levels =
            Vec::with_capacity(self.level_count(Side::Buy) + self.level_count(Side::Sell));
        for side in [Side::Buy, Side::Sell] {
            levels.extend(self.top(side, usize::MAX).into_iter().map(|l| (side, l)));
        }
        self.clear();
        self.place(mid);
        for (side, l) in levels {
            self.insert(side, l.price, l.qty.0);
        }
        self.recenters += 1;
    }

    /// Total quantity within `ticks` prices of the best level (the best
    /// included): the depth a consumer sees "N ticks deep". Zero if the
    /// side is empty. Vectorized sum over the window plus a map range
    /// outside it.
    pub fn depth_qty(&self, side: Side, ticks: u64) -> Qty {
        let Some(best) = self.best(side) else {
            return Qty::ZERO;
        };
        if ticks == 0 {
            return Qty::ZERO;
        }
        let span = (ticks - 1) as i128;
        let far = match side {
            Side::Buy => best.price.0 as i128 - span,
            Side::Sell => best.price.0 as i128 + span,
        };
        let far = far.clamp(i64::MIN as i128, i64::MAX as i128) as i64;
        self.range_qty(side, best.price, Price(far))
    }

    /// Total quantity at `price` or better (what an order with limit
    /// `price` on the other side could take). Vectorized like
    /// [`depth_qty`](LadderDepth::depth_qty).
    pub fn qty_at_or_better(&self, side: Side, price: Price) -> Qty {
        match self.best(side) {
            Some(best) if !better(side, price, best.price) => {
                self.range_qty(side, best.price, price)
            }
            _ => Qty::ZERO,
        }
    }

    /// The worst price an order taking `qty` from this side reaches: the
    /// level where the cumulative quantity from the best level first
    /// covers `qty`. `None` if `qty` is zero or the side holds less.
    /// Vectorized two-phase search over the window.
    pub fn sweep_price(&self, side: Side, qty: Qty) -> Option<Price> {
        if qty.is_zero() {
            return None;
        }
        let need = qty.0 as u128;
        let mut cum = 0u128;
        let (better_ovf, worse_ovf) = self.overflow_split(side);
        for l in better_ovf {
            cum += l.qty.0 as u128;
            if cum >= need {
                return Some(l.price);
            }
        }
        let s = si(side);
        if self.best[s] != NONE {
            let from = self.best[s];
            let window = &self.qty[s][from..];
            let left = (need - cum) as u64;
            match self.kernels.levels_to_fill(window, left) {
                Some(k) => return Some(self.price_at(side, from + k - 1)),
                // The window holds less than `left` (< 2^64): exact sum.
                None => cum += self.kernels.sum(window) as u128,
            }
        }
        for l in worse_ovf {
            cum += l.qty.0 as u128;
            if cum >= need {
                return Some(l.price);
            }
        }
        None
    }

    /// The first level from the best outward holding at least `min_qty`.
    /// Vectorized search over the window.
    pub fn first_level_at_least(&self, side: Side, min_qty: Qty) -> Option<Level> {
        let min = min_qty.0.max(1);
        let (better_ovf, worse_ovf) = self.overflow_split(side);
        if let Some(l) = better_ovf.into_iter().find(|l| l.qty.0 >= min) {
            return Some(l);
        }
        let s = si(side);
        if self.best[s] != NONE {
            let from = self.best[s];
            if let Some(k) = self.kernels.first_at_least(&self.qty[s][from..], min) {
                return Some(self.level_at(side, from + k));
            }
        }
        worse_ovf.into_iter().find(|l| l.qty.0 >= min)
    }

    /// The first non-empty level at `price` or worse (for bids: at or
    /// below `price`; for asks: at or above). Vectorized search over the
    /// window.
    pub fn next_level_from(&self, side: Side, price: Price) -> Option<Level> {
        let s = si(side);
        let reaches = |p: Price| !better(side, p, price);
        let (better_ovf, worse_ovf) = self.overflow_split(side);
        if let Some(l) = better_ovf.into_iter().find(|l| reaches(l.price)) {
            return Some(l);
        }
        if self.best[s] != NONE {
            // First window index at `price` or worse.
            let start = match self.index(side, price) {
                Some(i) => i,
                None if self.anchored && better(side, price, Price(self.edge(side))) => 0,
                None => self.len,
            };
            let from = start.max(self.best[s]);
            if from < self.len {
                if let Some(k) = self.kernels.first_nonzero(&self.qty[s][from..]) {
                    return Some(self.level_at(side, from + k));
                }
            }
        }
        worse_ovf.into_iter().find(|l| reaches(l.price))
    }
}

// @student-begin w29
impl LadderDepth {
    #[inline]
    fn hi(&self) -> i64 {
        self.lo + (self.len as i64 - 1)
    }

    /// The window's best-end price for `side` (index 0).
    #[inline]
    fn edge(&self, side: Side) -> i64 {
        match side {
            Side::Buy => self.hi(),
            Side::Sell => self.lo,
        }
    }

    /// Window index of `price` on `side`, if inside the window.
    #[inline]
    fn index(&self, side: Side, price: Price) -> Option<usize> {
        if !self.anchored {
            return None;
        }
        let off = price.0.checked_sub(self.lo)?;
        if off < 0 || off >= self.len as i64 {
            return None;
        }
        Some(match side {
            Side::Sell => off as usize,
            Side::Buy => self.len - 1 - off as usize,
        })
    }

    #[inline]
    fn price_at(&self, side: Side, i: usize) -> Price {
        Price(match side {
            Side::Sell => self.lo + i as i64,
            Side::Buy => self.hi() - i as i64,
        })
    }

    #[inline]
    fn level_at(&self, side: Side, i: usize) -> Level {
        Level {
            price: self.price_at(side, i),
            qty: Qty(self.qty[si(side)][i]),
        }
    }

    /// Places an empty window around `mid` (clamped so `hi` cannot
    /// overflow).
    fn place(&mut self, mid: Price) {
        let half = (self.len / 2) as i64;
        self.lo = mid
            .0
            .saturating_sub(half)
            .min(i64::MAX - (self.len as i64 - 1));
        self.anchored = true;
    }

    /// Empties both sides (keeps the window position). O(window).
    fn clear(&mut self) {
        for s in 0..2 {
            self.qty[s].fill(0);
            self.overflow[s].clear();
        }
        self.best = [NONE; 2];
        self.count = [0; 2];
    }

    /// Sets a window slot, maintaining the count and the best index.
    #[inline]
    fn set_slot(&mut self, side: Side, i: usize, q: u64) {
        let s = si(side);
        let old = std::mem::replace(&mut self.qty[s][i], q);
        if old == 0 && q != 0 {
            self.count[s] += 1;
            if self.best[s] == NONE || i < self.best[s] {
                self.best[s] = i;
            }
        } else if old != 0 && q == 0 {
            self.count[s] -= 1;
            if i == self.best[s] {
                self.best[s] = if self.count[s] == 0 {
                    NONE
                } else {
                    // Levels left are all further out: the scan finds one.
                    i + 1
                        + self
                            .kernels
                            .first_nonzero(&self.qty[s][i + 1..])
                            .unwrap_or(0)
                };
            }
        }
    }

    /// Sets a level without considering a re-centre.
    fn insert(&mut self, side: Side, price: Price, q: u64) {
        match self.index(side, price) {
            Some(i) => self.set_slot(side, i, q),
            None if q == 0 => {
                self.overflow[si(side)].remove(&price);
            }
            None => {
                self.overflow[si(side)].insert(price, Qty(q));
            }
        }
    }

    /// Overflow levels better than the window and worse than it, each best
    /// first. Allocates (cold: the overflow is normally empty or far away).
    fn overflow_split(&self, side: Side) -> (Vec<Level>, Vec<Level>) {
        let map = &self.overflow[si(side)];
        if map.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let level = |(&price, &qty): (&Price, &Qty)| Level { price, qty };
        if !self.anchored {
            return (Vec::new(), self.top_overflow(side));
        }
        let (lo, hi) = (Price(self.lo), Price(self.hi()));
        match side {
            Side::Buy => (
                map.range(hi..)
                    .rev()
                    .filter(|(p, _)| **p > hi)
                    .map(level)
                    .collect(),
                map.range(..lo).rev().map(level).collect(),
            ),
            Side::Sell => (
                map.range(..lo).map(level).collect(),
                map.range(hi..)
                    .filter(|(p, _)| **p > hi)
                    .map(level)
                    .collect(),
            ),
        }
    }

    fn top_overflow(&self, side: Side) -> Vec<Level> {
        let level = |(&price, &qty): (&Price, &Qty)| Level { price, qty };
        let map = &self.overflow[si(side)];
        match side {
            Side::Buy => map.iter().rev().map(level).collect(),
            Side::Sell => map.iter().map(level).collect(),
        }
    }

    /// Sum of the levels between `near` (the better end) and `far`,
    /// inclusive.
    fn range_qty(&self, side: Side, near: Price, far: Price) -> Qty {
        let (a, b) = if near <= far {
            (near, far)
        } else {
            (far, near)
        };
        let s = si(side);
        let mut total = self.overflow[s]
            .range(a..=b)
            .fold(0u64, |t, (_, q)| t.wrapping_add(q.0));
        if self.anchored {
            let (lo, hi) = (a.0.max(self.lo), b.0.min(self.hi()));
            if lo <= hi {
                let (i, j) = match side {
                    Side::Sell => ((lo - self.lo) as usize, (hi - self.lo) as usize),
                    Side::Buy => ((self.hi() - hi) as usize, (self.hi() - lo) as usize),
                };
                total = total.wrapping_add(self.kernels.sum(&self.qty[s][i..=j]));
            }
        }
        Qty(total)
    }

    /// After a level outside the window was added: if it is now the best
    /// of its side, the market has left the window — re-centre on the
    /// touch, provided both sides' best levels then fit in the middle half.
    #[cold]
    fn maybe_recenter(&mut self, side: Side, price: Price) {
        if self.best(side).map(|l| l.price) != Some(price) {
            return;
        }
        let mid = match self.best(side.opposite()) {
            Some(o) => {
                if o.price.0.abs_diff(price.0) > (self.len / 2) as u64 {
                    return;
                }
                midpoint(price, o.price)
            }
            None => price,
        };
        self.recenter(mid);
    }
}
// @student-end

// @student w29
impl DepthBook for LadderDepth {
    #[inline]
    fn add(&mut self, side: Side, price: Price, qty: Qty) -> Qty {
        let total = self.get(side, price) + qty;
        self.set(side, price, total);
        total
    }

    #[inline]
    fn sub(&mut self, side: Side, price: Price, qty: Qty) -> Qty {
        let old = self.get(side, price);
        if old.is_zero() {
            return Qty::ZERO;
        }
        let left = old.saturating_sub(qty);
        self.set(side, price, left);
        left
    }

    #[inline]
    fn set(&mut self, side: Side, price: Price, qty: Qty) {
        if let Some(i) = self.index(side, price) {
            return self.set_slot(side, i, qty.0);
        }
        if qty.is_zero() {
            self.overflow[si(side)].remove(&price);
        } else if !self.anchored || self.count == [0, 0] {
            // Nothing in the window to migrate: put the window here.
            self.recenter(price);
            self.insert(side, price, qty.0);
        } else {
            self.overflow[si(side)].insert(price, qty);
            self.maybe_recenter(side, price);
        }
    }

    #[inline]
    fn get(&self, side: Side, price: Price) -> Qty {
        match self.index(side, price) {
            Some(i) => Qty(self.qty[si(side)][i]),
            None => self.overflow[si(side)]
                .get(&price)
                .copied()
                .unwrap_or_default(),
        }
    }

    #[inline]
    fn best(&self, side: Side) -> Option<Level> {
        let s = si(side);
        let window = (self.best[s] != NONE).then(|| self.level_at(side, self.best[s]));
        let map = &self.overflow[s];
        let outside = match side {
            Side::Buy => map.last_key_value(),
            Side::Sell => map.first_key_value(),
        }
        .map(|(&price, &qty)| Level { price, qty });
        match (window, outside) {
            (Some(w), Some(o)) if better(side, o.price, w.price) => Some(o),
            (Some(w), _) => Some(w),
            (None, o) => o,
        }
    }

    fn level_count(&self, side: Side) -> usize {
        self.count[si(side)] + self.overflow[si(side)].len()
    }

    fn top(&self, side: Side, n: usize) -> Vec<Level> {
        let mut out = Vec::with_capacity(n.min(self.level_count(side)));
        let (better_ovf, worse_ovf) = self.overflow_split(side);
        out.extend(better_ovf.into_iter().take(n));
        let s = si(side);
        let mut i = self.best[s];
        while i != NONE && i < self.len && out.len() < n {
            out.push(self.level_at(side, i));
            i = match self.kernels.first_nonzero(&self.qty[s][i + 1..]) {
                Some(k) => i + 1 + k,
                None => NONE,
            };
        }
        let room = n - out.len();
        out.extend(worse_ovf.into_iter().take(room));
        out
    }

    fn load(&mut self, snap: &L2Snapshot) {
        self.clear();
        let mid = match (snap.bids.first(), snap.asks.first()) {
            (Some(b), Some(a)) => midpoint(b.price, a.price),
            (Some(b), None) => b.price,
            (None, Some(a)) => a.price,
            (None, None) => return,
        };
        self.place(mid);
        for (side, levels) in [(Side::Buy, &snap.bids), (Side::Sell, &snap.asks)] {
            for l in levels {
                self.insert(side, l.price, l.qty.0);
            }
        }
    }
}
