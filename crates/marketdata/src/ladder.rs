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

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::simd::Kernels;
use crate::{DepthBook, L2Snapshot, Level};
use types::{Price, Qty, Side};

// TODO(w29): implement — see the week guide on tepy.dev/hft.

/// Default window: 4,096 prices per side (32 KiB each — L1/L2 resident).
pub const DEFAULT_WINDOW: usize = 4096;

/// [`DepthBook`] as dense quantity arrays around the touch, with a sparse
/// fallback outside the window (see the module docs). `get`/`set`/`add`/
/// `sub` are O(1) inside the window (plus a forward scan when the best
/// level empties) and O(log n) outside it; `best` is O(1) plus a map peek.
#[derive(Clone)]
pub struct LadderDepth {
    // TODO(w29): design this type's fields.
    _todo: (),
}

impl Default for LadderDepth {
    fn default() -> Self {
        LadderDepth::new(DEFAULT_WINDOW)
    }
}

#[allow(unused_variables)]
impl std::fmt::Debug for LadderDepth {
    /// The window, the best few levels and the counts — not the arrays.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!("w29: fmt")
    }
}

#[allow(unused_variables)]
impl LadderDepth {
    /// An empty ladder of `window` prices per side using the fastest
    /// kernels this CPU supports. The window is placed around the first
    /// level that arrives. Allocates `2 × window × 8` bytes.
    ///
    /// # Panics
    /// If `window` is 0.
    pub fn new(window: usize) -> LadderDepth {
        todo!("w29: new")
    }

    /// Like [`new`](LadderDepth::new) with explicit kernels (e.g.
    /// [`Kernels::scalar`] as the baseline in benchmarks).
    pub fn with_kernels(window: usize, kernels: &'static Kernels) -> LadderDepth {
        todo!("w29: with_kernels")
    }

    /// The kernels this ladder calls.
    pub fn kernels(&self) -> &'static Kernels {
        todo!("w29: kernels")
    }

    /// The prices the dense window covers (`lo..=hi`), once placed.
    pub fn window(&self) -> Option<(Price, Price)> {
        todo!("w29: window")
    }

    /// Levels currently kept outside the window (both sides).
    pub fn overflow_levels(&self) -> usize {
        todo!("w29: overflow_levels")
    }

    /// How many times the window was placed or moved.
    pub fn recenters(&self) -> u64 {
        todo!("w29: recenters")
    }

    /// Moves the window so `mid` sits in its middle, migrating every level
    /// to the arrays or the overflow maps. O(window + levels · log levels);
    /// allocates temporarily.
    pub fn recenter(&mut self, mid: Price) {
        todo!("w29: recenter")
    }

    /// Total quantity within `ticks` prices of the best level (the best
    /// included): the depth a consumer sees "N ticks deep". Zero if the
    /// side is empty. Vectorized sum over the window plus a map range
    /// outside it.
    pub fn depth_qty(&self, side: Side, ticks: u64) -> Qty {
        todo!("w29: depth_qty")
    }

    /// Total quantity at `price` or better (what an order with limit
    /// `price` on the other side could take). Vectorized like
    /// [`depth_qty`](LadderDepth::depth_qty).
    pub fn qty_at_or_better(&self, side: Side, price: Price) -> Qty {
        todo!("w29: qty_at_or_better")
    }

    /// The worst price an order taking `qty` from this side reaches: the
    /// level where the cumulative quantity from the best level first
    /// covers `qty`. `None` if `qty` is zero or the side holds less.
    /// Vectorized two-phase search over the window.
    pub fn sweep_price(&self, side: Side, qty: Qty) -> Option<Price> {
        todo!("w29: sweep_price")
    }

    /// The first level from the best outward holding at least `min_qty`.
    /// Vectorized search over the window.
    pub fn first_level_at_least(&self, side: Side, min_qty: Qty) -> Option<Level> {
        todo!("w29: first_level_at_least")
    }

    /// The first non-empty level at `price` or worse (for bids: at or
    /// below `price`; for asks: at or above). Vectorized search over the
    /// window.
    pub fn next_level_from(&self, side: Side, price: Price) -> Option<Level> {
        todo!("w29: next_level_from")
    }
}

// TODO(w29): implement — see the week guide on tepy.dev/hft.

#[allow(unused_variables)]
impl DepthBook for LadderDepth {
    #[inline]
    fn add(&mut self, side: Side, price: Price, qty: Qty) -> Qty {
        todo!("w29: add")
    }

    #[inline]
    fn sub(&mut self, side: Side, price: Price, qty: Qty) -> Qty {
        todo!("w29: sub")
    }

    #[inline]
    fn set(&mut self, side: Side, price: Price, qty: Qty) {
        todo!("w29: set")
    }

    #[inline]
    fn get(&self, side: Side, price: Price) -> Qty {
        todo!("w29: get")
    }

    #[inline]
    fn best(&self, side: Side) -> Option<Level> {
        todo!("w29: best")
    }

    fn level_count(&self, side: Side) -> usize {
        todo!("w29: level_count")
    }

    fn top(&self, side: Side, n: usize) -> Vec<Level> {
        todo!("w29: top")
    }

    fn load(&mut self, snap: &L2Snapshot) {
        todo!("w29: load")
    }
}
