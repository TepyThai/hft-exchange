//! Limit order book: price levels, FIFO queues, O(1) cancel; slab and ladder variants.
//!
//! Curriculum: Weeks 6, 27, 28 (see https://tepy.dev/hft).
//!
//! The crate has two layers:
//!
//! * the **contract** — [`OrderBook`], [`RestingOrder`], [`LevelInfo`] and
//!   [`BookError`]. The matching engine is written against this trait only,
//!   so the w06 [`Book`], the w27 slab book and the w28 ladder book are
//!   interchangeable (`engine::Engine<B: OrderBook>`).
//! * the **implementations**:
//!   * w06 [`Book`]: a sorted price index per side, an intrusive FIFO per
//!     level and an order-id index (see its docs for why; the price index
//!     was a `BTreeMap` until profiling in w26);
//!   * w27 [`SlabBook`]: the same structure on preallocated generational
//!     [`Slab`]s — zero allocations within its capacity, and stale-proof
//!     [`SlabKey`] handles to resting orders;
//!   * w28 [`LadderBook`]: a dense array of levels over a window of prices
//!     around the touch (O(1) level lookup, bitmap scan for the next best),
//!     with a sparse map for far prices.
//!
//! A book holds one instrument and knows nothing about matching rules,
//! validation or events: it stores resting orders in price-time priority and
//! offers the primitives the engine matches with ([`OrderBook::front`] and
//! [`OrderBook::fill_front`]).

mod book;
mod hash;
mod ladder;
mod slab;
mod slab_book;

pub use book::Book;
pub use hash::{IdBuildHasher, IdHasher, IdMap};
pub use ladder::LadderBook;
pub use slab::{Slab, SlabKey};
pub use slab_book::SlabBook;

use types::{OrderId, Price, Qty, RejectReason, Side};

/// A resting order as a book stores it: identity, side, price and the
/// quantity still open (never zero while it rests).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RestingOrder {
    pub id: OrderId,
    pub side: Side,
    pub price: Price,
    /// Remaining (open) quantity.
    pub qty: Qty,
}

impl RestingOrder {
    /// Shorthand for tests and examples: `RestingOrder::new(1, Side::Buy, 100, 5)`.
    pub fn new(id: u64, side: Side, price: i64, qty: u64) -> RestingOrder {
        RestingOrder {
            id: OrderId(id),
            side,
            price: Price(price),
            qty: Qty(qty),
        }
    }
}

/// Aggregate view of one price level: what L2 market data publishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LevelInfo {
    pub price: Price,
    /// Sum of the remaining quantity of every order at this price (> 0).
    pub qty: Qty,
    /// Number of orders at this price (> 0).
    pub orders: u32,
}

/// Why a book operation was refused. The book enforces only its own
/// invariants (unique ids, positive quantities, reductions only); trading
/// rules are the engine's job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BookError {
    /// An order with this id is already resting.
    DuplicateId,
    /// No resting order has this id.
    UnknownOrder,
    /// Quantity zero (adds) or reduce-to-zero (use cancel instead).
    ZeroQty,
    /// Reduce to a quantity not lower than the current remaining quantity.
    InvalidReduce,
}

impl From<BookError> for RejectReason {
    fn from(e: BookError) -> RejectReason {
        match e {
            BookError::DuplicateId => RejectReason::DuplicateOrderId,
            BookError::UnknownOrder => RejectReason::UnknownOrder,
            BookError::ZeroQty => RejectReason::ZeroQuantity,
            BookError::InvalidReduce => RejectReason::InvalidReduce,
        }
    }
}

impl std::fmt::Display for BookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            BookError::DuplicateId => "an order with this id is already resting",
            BookError::UnknownOrder => "no resting order with this id",
            BookError::ZeroQty => "quantity must be positive",
            BookError::InvalidReduce => "reduce must lower the remaining quantity",
        };
        f.write_str(s)
    }
}

impl std::error::Error for BookError {}

/// A single-instrument limit order book in price-time priority.
///
/// **Priority.** On each side, better prices come first (highest bid,
/// lowest ask); within a price level, orders are kept in arrival order
/// (FIFO). Reducing an order keeps its place; nothing else reorders a level.
///
/// **Invariants every implementation keeps:** every resting order has
/// `qty > 0`; every level holds at least one order and its [`LevelInfo`]
/// equals the sum of its orders; order ids are unique among resting orders.
/// A book does *not* prevent crossing (a bid at or above the best ask): it
/// stores what it is given — not crossing is the matching engine's job.
///
/// **Visitors.** Iteration is callback-based (`f` returns `true` to keep
/// going, `false` to stop) so the trait stays object-safe — the w37 engine
/// compares `Engine<Book>` with `Engine<Box<dyn OrderBook>>` — and no
/// iteration allocates.
///
/// **Costs** quoted per method are those of the reference [`Book`]; other
/// implementations document theirs. The w06 requirement is O(1) for
/// cancel / reduce / get by id and O(log L) or better for everything else
/// (L = price levels on one side).
pub trait OrderBook {
    /// Rests `order` at the back of its price level's queue.
    ///
    /// Errors: [`BookError::ZeroQty`], [`BookError::DuplicateId`] (book
    /// unchanged). `Book`: O(1) at an existing level, O(log L) when it
    /// creates a level (L = levels on that side).
    fn add(&mut self, order: RestingOrder) -> Result<(), BookError>;

    /// Removes a resting order and returns it with its remaining quantity.
    ///
    /// Errors: [`BookError::UnknownOrder`]. `Book`: O(1); O(log L) if it
    /// empties its level.
    fn cancel(&mut self, id: OrderId) -> Result<RestingOrder, BookError>;

    /// Lowers the remaining quantity of a resting order to `qty`, keeping its
    /// time priority, and returns the updated order.
    ///
    /// Errors (checked in this order): [`BookError::UnknownOrder`],
    /// [`BookError::ZeroQty`] (use cancel), [`BookError::InvalidReduce`]
    /// (`qty >= remaining`). `Book`: O(1).
    fn reduce(&mut self, id: OrderId, qty: Qty) -> Result<RestingOrder, BookError>;

    /// The resting order with this id, if any. `Book`: O(1).
    fn get(&self, id: OrderId) -> Option<RestingOrder>;

    /// Best (highest bid / lowest ask) level on `side`. `Book`: O(1).
    fn best(&self, side: Side) -> Option<LevelInfo>;

    /// The level at exactly `price` on `side`. `Book`: O(log L).
    fn level(&self, side: Side, price: Price) -> Option<LevelInfo>;

    /// The order with the highest priority on `side`: the oldest order at the
    /// best price — the next maker a taker on the other side trades with.
    /// `Book`: O(1).
    fn front(&self, side: Side) -> Option<RestingOrder>;

    /// Fills `qty` of the [`front`](OrderBook::front) order on `side` and
    /// returns its remaining quantity. A fully filled order is removed, and
    /// a level left empty is removed with it, so the next `front` is the next
    /// maker in priority.
    ///
    /// # Panics
    /// If `side` is empty or `qty` is not in `1..=front.qty`. `Book`: O(1);
    /// O(log L) when it empties the best level.
    fn fill_front(&mut self, side: Side, qty: Qty) -> Qty;

    /// Calls `f` for each level on `side`, best price first, until `f`
    /// returns `false`. `Book`: O(log L + levels visited).
    fn visit_levels(&self, side: Side, f: &mut dyn FnMut(LevelInfo) -> bool);

    /// Calls `f` for each order at `price` on `side` in time priority
    /// (oldest first) until `f` returns `false`. Visits nothing if the level
    /// does not exist. `Book`: O(log L + orders visited).
    fn visit_level(&self, side: Side, price: Price, f: &mut dyn FnMut(RestingOrder) -> bool);

    /// Calls `f` for every order on `side` in full priority order (best
    /// price first, FIFO within a price) until `f` returns `false`. Used for
    /// state hashing, snapshots and invariant checks.
    ///
    /// The default walks [`visit_levels`](OrderBook::visit_levels) and
    /// [`visit_level`](OrderBook::visit_level); implementations override it
    /// with a direct walk. O(orders visited).
    fn visit_orders(&self, side: Side, f: &mut dyn FnMut(RestingOrder) -> bool) {
        let mut go = true;
        self.visit_levels(side, &mut |level| {
            self.visit_level(side, level.price, &mut |o| {
                go = f(o);
                go
            });
            go
        });
    }

    /// Number of resting orders (both sides). O(1).
    fn len(&self) -> usize;

    /// Number of price levels on `side`. O(1).
    fn level_count(&self, side: Side) -> usize;

    /// Removes every order, keeping allocated capacity.
    fn clear(&mut self);

    // ---------------------------------------------------------------------
    // Provided conveniences (built on the methods above).

    /// True if no order rests on either side.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// True if an order with this id is resting.
    fn contains(&self, id: OrderId) -> bool {
        self.get(id).is_some()
    }

    /// Best bid level (highest buy price).
    fn best_bid(&self) -> Option<LevelInfo> {
        self.best(Side::Buy)
    }

    /// Best ask level (lowest sell price).
    fn best_ask(&self) -> Option<LevelInfo> {
        self.best(Side::Sell)
    }

    /// `best ask - best bid` in ticks, when both sides have orders.
    fn spread(&self) -> Option<i64> {
        Some(self.best_ask()?.price.0 - self.best_bid()?.price.0)
    }

    /// True if the best bid is at or above the best ask. The engine never
    /// leaves a book crossed; this is for invariant checks.
    fn is_crossed(&self) -> bool {
        matches!((self.best_bid(), self.best_ask()), (Some(b), Some(a)) if b.price >= a.price)
    }

    /// Clears `out` and fills it with up to `n` levels of `side`, best
    /// first (top-N depth, L2). Allocates only if `out` must grow.
    fn depth(&self, side: Side, n: usize, out: &mut Vec<LevelInfo>) {
        out.clear();
        if n == 0 {
            return;
        }
        self.visit_levels(side, &mut |level| {
            out.push(level);
            out.len() < n
        });
    }

    /// Clears `out` and fills it with the orders at `price` on `side`, in
    /// time priority (L3 view of one level).
    fn level_orders(&self, side: Side, price: Price, out: &mut Vec<RestingOrder>) {
        out.clear();
        self.visit_level(side, price, &mut |o| {
            out.push(o);
            true
        });
    }
}

/// Boxed books are books too, so `Engine<Box<dyn OrderBook>>` works (w37
/// compares it with the statically dispatched engine).
impl<B: OrderBook + ?Sized> OrderBook for Box<B> {
    fn add(&mut self, order: RestingOrder) -> Result<(), BookError> {
        (**self).add(order)
    }
    fn cancel(&mut self, id: OrderId) -> Result<RestingOrder, BookError> {
        (**self).cancel(id)
    }
    fn reduce(&mut self, id: OrderId, qty: Qty) -> Result<RestingOrder, BookError> {
        (**self).reduce(id, qty)
    }
    fn get(&self, id: OrderId) -> Option<RestingOrder> {
        (**self).get(id)
    }
    fn best(&self, side: Side) -> Option<LevelInfo> {
        (**self).best(side)
    }
    fn level(&self, side: Side, price: Price) -> Option<LevelInfo> {
        (**self).level(side, price)
    }
    fn front(&self, side: Side) -> Option<RestingOrder> {
        (**self).front(side)
    }
    fn fill_front(&mut self, side: Side, qty: Qty) -> Qty {
        (**self).fill_front(side, qty)
    }
    fn visit_levels(&self, side: Side, f: &mut dyn FnMut(LevelInfo) -> bool) {
        (**self).visit_levels(side, f)
    }
    fn visit_level(&self, side: Side, price: Price, f: &mut dyn FnMut(RestingOrder) -> bool) {
        (**self).visit_level(side, price, f)
    }
    fn visit_orders(&self, side: Side, f: &mut dyn FnMut(RestingOrder) -> bool) {
        (**self).visit_orders(side, f)
    }
    fn len(&self) -> usize {
        (**self).len()
    }
    fn level_count(&self, side: Side) -> usize {
        (**self).level_count(side)
    }
    fn clear(&mut self) {
        (**self).clear()
    }
    fn contains(&self, id: OrderId) -> bool {
        (**self).contains(id)
    }
}
