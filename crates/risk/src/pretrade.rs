//! Week 15 — pre-trade risk: per-account limits, exposure tracking and kill
//! switches, checked before an order reaches the matching engine.
//!
//! # Where it sits
//!
//! [`RiskEngine`] is a single-threaded component owned by the thread that
//! sequences orders into the engine (the gateway's engine-side loop):
//!
//! ```text
//! NewOrder ─► risk.check(&o, reference) ─► Ok  ─► risk.on_new(&o) ─► engine
//!                                       └► Err ─► Event::Rejected { reason }
//! engine events ─► risk.on_event(&ev) ─► fills (for positions / PnL, w45)
//! ```
//!
//! `check` takes `&self`, never allocates and never locks: one atomic load,
//! one hash lookup and a handful of integer comparisons. All bookkeeping
//! happens in `on_new` / `on_event`.
//!
//! # Exposure and the projected-position rule
//!
//! For every (account, instrument) the engine keeps an [`Exposure`]: the net
//! filled `position` (long > 0, short < 0) and the total remaining quantity
//! of the account's open orders on each side (`open_buy`, `open_sell`). An
//! order counts as open from `on_new` (so orders in flight to the engine are
//! included) until a `Cancelled`, `Expired`, `Rejected` or final `Trade`
//! event closes it.
//!
//! The position limit assumes the worst case: every open order on the side
//! that *increases* exposure in the order's direction fills, plus the new
//! order. Opposite-side open orders are ignored — they could be cancelled, so
//! they are never credited.
//!
//! ```text
//! buy  qty q:  projected = position + open_buy  + q   reject if projected >  max_position
//! sell qty q:  projected = position − open_sell − q   reject if projected < −max_position
//! ```
//!
//! Consequences: an order that only reduces the position (sell while long,
//! with `position − open_sell − q ≥ −max_position`) always passes, even if
//! the account is over its limit; and an account cannot "pre-load" both
//! sides past the limit, because each side is bounded on its own. The
//! arithmetic is done in `i128`, so no combination of `i64`/`u64` inputs can
//! overflow.
//!
//! # Check order
//!
//! The first failing check decides the reason:
//!
//! 1. global kill switch → `Risk(KillSwitch)`
//! 2. account kill switch → `Risk(KillSwitch)`
//! 3. `qty > max_order_qty` → `Risk(MaxOrderQty)`
//! 4. limit price more than `price_band_ticks` from the reference →
//!    [`RejectReason::PriceBand`] (market orders have no price and skip it)
//! 5. `|price| × qty > max_notional` (market orders are valued at the
//!    reference price) → `Risk(MaxNotional)`
//! 6. projected position beyond `max_position` → `Risk(PositionLimit)`
//!
//! `check` returns a [`RejectReason`] rather than a bare [`RiskReason`]
//! because a price-band violation has its own variant in the shared
//! contract; every other failure is `RejectReason::Risk(_)`, so the gateway
//! can put the result straight into `Event::Rejected`.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use types::{AccountId, Event, InstrumentId, NewOrder, OrderId, Price, Qty, RejectReason, Side};
// Used only by the implementation (unused in the learner template).
#[allow(unused_imports)]
use {
    crate::idmap::IdMap,
    std::sync::atomic::{AtomicBool, Ordering},
    std::sync::Arc,
    types::{OrderKind, RiskReason},
};

/// Pre-trade limits for one account. Position limits apply to each
/// instrument separately.
///
/// Start from [`Limits::UNLIMITED`] and override what you need:
///
/// ```
/// # use risk::Limits; use types::Qty;
/// let l = Limits { max_order_qty: Qty(1_000), price_band_ticks: 50, ..Limits::UNLIMITED };
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Limits {
    /// Largest quantity a single order may have (fat-finger check).
    pub max_order_qty: Qty,
    /// Largest notional of a single order, `|price| × qty` in ticks × lots.
    /// `u128` so that no `i64` price times `u64` quantity can overflow.
    pub max_notional: u128,
    /// Largest absolute projected net position per instrument, in lots
    /// (see the module docs for the projection rule).
    pub max_position: Qty,
    /// Largest allowed distance of a limit price from the reference price,
    /// in ticks. `0` disables the band (as `InstrumentSpec::band_ticks`).
    pub price_band_ticks: u64,
}

impl Limits {
    /// No limits at all: every order passes (unless a kill switch is on).
    pub const UNLIMITED: Limits = Limits {
        max_order_qty: Qty(u64::MAX),
        max_notional: u128::MAX,
        max_position: Qty(u64::MAX),
        price_band_ticks: 0,
    };

    /// Blocks every order with `MaxOrderQty` — a safe default for accounts
    /// that must be configured explicitly before trading.
    pub const BLOCKED: Limits = Limits {
        max_order_qty: Qty(0),
        max_notional: 0,
        max_position: Qty(0),
        price_band_ticks: 0,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Limits::UNLIMITED
    }
}

/// What pre-trade risk knows about one account in one instrument.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Exposure {
    /// Net filled position in lots: long > 0, short < 0.
    pub position: i64,
    /// Total remaining quantity of the account's open buy orders.
    pub open_buy: Qty,
    /// Total remaining quantity of the account's open sell orders.
    pub open_sell: Qty,
}

/// One side of a trade, attributed to a tracked order: what
/// [`RiskEngine::on_event`] returns for every `Event::Trade` leg it knows,
/// and what position keeping (w45) consumes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Fill {
    /// The tracked order that filled.
    pub order: OrderId,
    pub account: AccountId,
    pub instrument: InstrumentId,
    /// The side of *this* order (the maker's side is the taker's opposite).
    pub side: Side,
    /// Trade price (the maker's price).
    pub price: Price,
    pub qty: Qty,
}

/// A global kill switch that any thread can flip.
///
/// Cloning shares the same switch (it is an `Arc<AtomicBool>`), so an
/// operator thread can hold a handle while the engine thread checks it on
/// every order. Engaging blocks new orders only; cancels are never blocked.
/// Every method is one atomic operation — no locks, no allocation (except
/// [`KillSwitch::new`], which allocates the shared flag once).
#[derive(Clone, Debug, Default)]
pub struct KillSwitch {
    // TODO(w15): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl KillSwitch {
    /// A new, disengaged switch.
    pub fn new() -> KillSwitch {
        todo!("w15: new")
    }

    /// Blocks all new orders. Visible to every handle promptly (the flag
    /// guards no other data, so `Relaxed` is enough: there is nothing to
    /// synchronize-with, only a value to observe).
    pub fn engage(&self) {
        todo!("w15: engage")
    }

    /// Allows new orders again.
    pub fn disengage(&self) {
        todo!("w15: disengage")
    }

    /// True while engaged. One relaxed load (~1ns, a plain `mov` on x86).
    #[inline]
    pub fn is_engaged(&self) -> bool {
        todo!("w15: is_engaged")
    }
}

// TODO(w15): implement — see the week guide on tepy.dev/hft.

/// The pre-trade risk engine: limits, kill switches, and the open-order /
/// position bookkeeping the limits are checked against.
///
/// Single-threaded by design (it lives on the engine-side thread); only the
/// global [`KillSwitch`] is shared across threads.
#[derive(Debug)]
pub struct RiskEngine {
    // TODO(w15): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl RiskEngine {
    /// An engine where every account without explicit limits uses
    /// `default_limits` (use [`Limits::BLOCKED`] to require configuration).
    /// Allocates the kill-switch flag; the tables start empty.
    pub fn new(default_limits: Limits) -> RiskEngine {
        todo!("w15: new")
    }

    /// Like [`RiskEngine::new`], but pre-sizes the tables for `pairs`
    /// (account, instrument) pairs and `open_orders` concurrently open
    /// orders, so steady-state `on_new` never reallocates.
    pub fn with_capacity(default_limits: Limits, pairs: usize, open_orders: usize) -> RiskEngine {
        todo!("w15: with_capacity")
    }

    /// Sets `account`'s limits (keeps its kill-switch state). Tighter limits
    /// apply to new orders only; open orders are not touched. O(1) amortized.
    pub fn set_limits(&mut self, account: AccountId, limits: Limits) {
        todo!("w15: set_limits")
    }

    /// The limits that apply to `account` (its own or the default). O(1).
    pub fn limits(&self, account: AccountId) -> Limits {
        todo!("w15: limits")
    }

    /// A handle to the global kill switch, for an operator thread. O(1).
    pub fn global_kill_switch(&self) -> KillSwitch {
        todo!("w15: global_kill_switch")
    }

    /// Engages `account`'s kill switch: its new orders are rejected with
    /// `KillSwitch` until [`RiskEngine::revive_account`]. Open orders stay on
    /// the book — cancel them with [`RiskEngine::open_orders`]. O(1)
    /// amortized.
    pub fn kill_account(&mut self, account: AccountId) {
        todo!("w15: kill_account")
    }

    /// Disengages `account`'s kill switch. O(1).
    pub fn revive_account(&mut self, account: AccountId) {
        todo!("w15: revive_account")
    }

    /// True if new orders from `account` are blocked by a kill switch
    /// (its own or the global one). O(1).
    pub fn is_killed(&self, account: AccountId) -> bool {
        todo!("w15: is_killed")
    }

    /// The pre-trade check: may `order` go to the engine, given the current
    /// `reference` price (last trade, mid or previous close — the caller's
    /// choice)? See the module docs for the order of checks and the
    /// projected-position rule.
    ///
    /// Read-only: it does not record the order (call
    /// [`RiskEngine::on_new`] when you forward it). O(1): one atomic load
    /// and one hash lookup (two for an account's first order in an
    /// instrument); never allocates, never locks.
    #[inline]
    pub fn check(&self, order: &NewOrder, reference: Price) -> Result<(), RejectReason> {
        todo!("w15: check")
    }

    /// Records `order` as open (call it after a successful `check`, when the
    /// order is forwarded to the engine): its quantity counts towards the
    /// account's open quantity on its side from now on, so orders still in
    /// flight to the engine are part of the next check.
    ///
    /// An id that is already open is ignored (the engine will reject the
    /// duplicate). O(1) amortized; allocates only when a table grows past
    /// its capacity (new account/instrument pair, more open orders).
    pub fn on_new(&mut self, order: &NewOrder) {
        todo!("w15: on_new")
    }

    /// Applies an engine event to the exposure of the orders it concerns.
    /// Events for orders this engine never saw in `on_new` are ignored.
    ///
    /// * `Accepted` marks the order acknowledged.
    /// * `Trade` moves the filled quantity from open to position for each
    ///   tracked leg (taker and maker — both may be tracked, even by the
    ///   same account) and returns one [`Fill`] per tracked leg:
    ///   `[taker, maker]`. A fully filled order stops being tracked.
    /// * `Rested` / `Reduced` set the order's remaining quantity.
    /// * `Cancelled` / `Expired` release the remaining quantity.
    /// * `Rejected` releases the order — except a `DuplicateOrderId`
    ///   rejection of an id that is already acknowledged: that reject is for
    ///   a second order reusing a live id, not for the order we track.
    ///
    /// Returns `[None, None]` for every event but `Trade`. O(1): one or two
    /// hash lookups; never allocates.
    pub fn on_event(&mut self, event: &Event) -> [Option<Fill>; 2] {
        todo!("w15: on_event")
    }

    /// Current exposure of `account` in `instrument` (zero if never seen).
    /// O(1).
    pub fn exposure(&self, account: AccountId, instrument: InstrumentId) -> Exposure {
        todo!("w15: exposure")
    }

    /// Overrides the filled position of `account` in `instrument` — for
    /// start-of-day positions or reconciliation. O(1) amortized.
    pub fn set_position(&mut self, account: AccountId, instrument: InstrumentId, position: i64) {
        todo!("w15: set_position")
    }

    /// Ids of `account`'s open orders, ascending — what to cancel after a
    /// kill. O(open orders) and allocates the result: not for the hot path.
    pub fn open_orders(&self, account: AccountId) -> Vec<OrderId> {
        todo!("w15: open_orders")
    }

    /// Number of orders currently tracked as open. O(1).
    pub fn open_order_count(&self) -> usize {
        todo!("w15: open_order_count")
    }
}

// TODO(w15): implement — see the week guide on tepy.dev/hft.
