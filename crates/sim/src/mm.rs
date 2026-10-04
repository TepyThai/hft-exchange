//! Week 42 — a market-making strategy.
//!
//! [`MarketMaker`] keeps one bid and one ask on the book around its
//! estimate of fair value, earns the spread from uninformed flow, and
//! manages the risk that comes with it — inventory:
//!
//! ```text
//! fair        = microprice of the market's top of book (excluding own quotes)
//! reservation = fair − skew · position · tick           (long → quote lower)
//! bid         = floor(reservation − half_spread · tick)  (on the tick grid)
//! ask         = ceil (reservation + half_spread · tick)
//! bid ≤ market ask − tick,  ask ≥ market bid + tick       (never take)
//! bid size    = min(quote_qty, max_inventory − position)  (0 → no bid)
//! ask size    = min(quote_qty, max_inventory + position)  (0 → no ask)
//! ```
//!
//! Skewing both quotes against the inventory makes the side that reduces it
//! more attractive and the side that grows it less, so the position mean-
//! reverts. The size rule makes the limit hard: [`decide`](MarketMaker::decide)
//! further caps each new quote by the strategy's other orders still open on
//! that side (e.g. a quote whose cancel is in flight and may yet fill), so
//! `position + open bids ≤ max` and `position − open asks ≥ −max` hold after
//! every decision, and fills only move quantity from open to position:
//! whatever fills, `|position| ≤ max_inventory`, even with order latency.
//!
//! Each decision compares the wanted quotes with the working ones and sends
//! the least it can: nothing if a quote is within `requote_ticks` of its
//! target, a `Reduce` (keeps queue priority) if only the size must shrink,
//! otherwise cancel + new. All cancels go out before any new order, so a
//! new quote never trades against the strategy's own stale one. Fills
//! arrive as [`Delivery`]s and change the position, which the very next
//! decision uses.
//!
//! P&L is cash-based ([`Inventory`]): `cash + position × mark`. Marked at
//! the true fair value of a simulation it is the strategy's *edge*:
//! `Σ sells (p − F)·q + Σ buys (F − p)·q`.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::micro::TopOfBook;
use crate::scenario::{Agent, AgentCtx, Delivery, Inventory, Outbox};
pub use crate::scenario::{Quote, Quotes};
use marketdata::BookBuilder;
use types::{OrderId, Price, Qty, Side};
// Used only by the implementation (unused in the learner template).
#[allow(unused_imports)]
use {marketdata::Level, types::Event};

/// Parameters of a [`MarketMaker`]. Distances are in ticks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MmConfig {
    /// Size of each quote.
    pub quote_qty: Qty,
    /// Distance of each quote from the reservation price, in ticks
    /// (fractional values are fine: 0.5 quotes at the touch around a
    /// one-tick market).
    pub half_spread_ticks: f64,
    /// Reservation price shift per lot of inventory, in ticks.
    pub skew_ticks_per_lot: f64,
    /// Hard limit on `|position|`, in lots.
    pub max_inventory: i64,
    /// Replace a quote only when its target moved at least this many ticks
    /// (1 = on any change of price).
    pub requote_ticks: i64,
    /// The instrument's tick size.
    pub tick: i64,
}

impl Default for MmConfig {
    fn default() -> Self {
        MmConfig {
            quote_qty: Qty(5),
            half_spread_ticks: 1.0,
            skew_ticks_per_lot: 0.1,
            max_inventory: 20,
            requote_ticks: 1,
            tick: 1,
        }
    }
}

/// A quote the strategy has sent and believes is on the book.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WorkingQuote {
    /// Its order id.
    pub id: OrderId,
    /// Its limit price.
    pub price: Price,
    /// Its open quantity, as far as events have reported.
    pub remaining: Qty,
}

/// The market maker; see the module docs. Single-threaded; every method is
/// O(1) and allocation-free (it tracks a handful of its own orders) except
/// [`market_top`](MarketMaker::market_top) in the rare case it must look
/// past the top level.
///
/// It keeps every order of its own that may still be on the book — sent,
/// resting, or being cancelled — until an event says it is gone. Two
/// things depend on that when commands take time to arrive (the w44
/// backtester's latency):
///
/// * the market top excludes only orders known to rest (a quote still in
///   flight is not on the book yet; one being cancelled still is);
/// * quote sizes count every open order on the side, so a quote whose
///   cancel is still in flight (and may yet fill) cannot push the position
///   past the limit.
#[derive(Clone, Debug)]
pub struct MarketMaker {
    // TODO(w42): design this type's fields.
    _todo: (),
}

// TODO(w42): implement — see the week guide on tepy.dev/hft.

#[allow(unused_variables)]
impl MarketMaker {
    /// A flat market maker with no quotes.
    pub fn new(cfg: MmConfig) -> MarketMaker {
        todo!("w42: new")
    }

    /// Its parameters.
    pub fn config(&self) -> &MmConfig {
        todo!("w42: config")
    }

    /// Changes the inventory limit (e.g. a risk desk tightening it); the
    /// next decision shrinks the working quotes to fit.
    pub fn set_max_inventory(&mut self, max: i64) {
        todo!("w42: set_max_inventory")
    }

    /// Position, cash and fill counters, from its own fills.
    pub fn inventory(&self) -> &Inventory {
        todo!("w42: inventory")
    }

    /// The working quote on `side`, if any (sent and not being cancelled;
    /// it may still be in flight).
    pub fn working(&self, side: Side) -> Option<WorkingQuote> {
        todo!("w42: working")
    }

    /// Decisions made so far.
    pub fn decisions(&self) -> u64 {
        todo!("w42: decisions")
    }

    /// Fair value estimate: the microprice of `top`. `None` unless
    /// two-sided.
    pub fn fair_value(&self, top: &TopOfBook) -> Option<f64> {
        todo!("w42: fair_value")
    }

    /// The quotes wanted against market `top` at `position` (see the module
    /// docs). Pure; O(1). No quotes unless `top` is two-sided.
    pub fn target(&self, top: &TopOfBook, position: i64) -> Quotes {
        todo!("w42: target")
    }

    /// One decision against market `top`: brings the working quotes to
    /// [`target`](MarketMaker::target) at the current position with the
    /// fewest commands (none, reduce, or cancel then post-only new), all
    /// cancels first, sizes capped by the orders still open on each side.
    /// O(own orders) — a handful; no allocation once `out` has grown.
    pub fn decide(&mut self, top: &TopOfBook, out: &mut Outbox) {
        todo!("w42: decide")
    }

    /// Applies an execution report about one of its orders: fills move the
    /// position and cash; cancels, expiries and rejections retire the
    /// order. O(own orders).
    pub fn on_delivery(&mut self, d: &Delivery) {
        todo!("w42: on_delivery")
    }

    /// The market's top of book as seen in `book` minus this strategy's
    /// own resting orders (quoting around a fair value that includes your
    /// own quotes would chase your own tail). O(log levels); allocates
    /// only when its own orders are alone at the top.
    pub fn market_top(&self, book: &BookBuilder) -> TopOfBook {
        todo!("w42: market_top")
    }
}

// TODO(w42): implement — see the week guide on tepy.dev/hft.

#[allow(unused_variables)]
impl Agent for MarketMaker {
    fn name(&self) -> &str {
        todo!("w42: name")
    }

    fn act(&mut self, ctx: &mut AgentCtx<'_>) {
        todo!("w42: act")
    }

    fn on_event(&mut self, delivery: &Delivery) {
        todo!("w42: on_event")
    }
}
