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
// @student w42
#[derive(Clone, Debug)]
pub struct MarketMaker {
    cfg: MmConfig,
    inventory: Inventory,
    /// Every order of ours that may be on the book.
    orders: Vec<OwnOrder>,
    /// The current quote per side (not being cancelled).
    bid: Option<OrderId>,
    ask: Option<OrderId>,
    decisions: u64,
}

// @student-begin w42
/// One of our orders that may still be on the book.
#[derive(Clone, Copy, Debug)]
struct OwnOrder {
    id: OrderId,
    side: Side,
    price: Price,
    remaining: Qty,
    /// A `Rested` event (or a fill as maker) confirmed it is on the book.
    rested: bool,
}
// @student-end

// @student w42
impl MarketMaker {
    /// A flat market maker with no quotes.
    pub fn new(cfg: MmConfig) -> MarketMaker {
        MarketMaker {
            cfg,
            inventory: Inventory::default(),
            orders: Vec::with_capacity(8),
            bid: None,
            ask: None,
            decisions: 0,
        }
    }

    /// Its parameters.
    pub fn config(&self) -> &MmConfig {
        &self.cfg
    }

    /// Changes the inventory limit (e.g. a risk desk tightening it); the
    /// next decision shrinks the working quotes to fit.
    pub fn set_max_inventory(&mut self, max: i64) {
        self.cfg.max_inventory = max;
    }

    /// Position, cash and fill counters, from its own fills.
    pub fn inventory(&self) -> &Inventory {
        &self.inventory
    }

    /// The working quote on `side`, if any (sent and not being cancelled;
    /// it may still be in flight).
    pub fn working(&self, side: Side) -> Option<WorkingQuote> {
        let id = match side {
            Side::Buy => self.bid,
            Side::Sell => self.ask,
        }?;
        let o = self.orders.iter().find(|o| o.id == id)?;
        Some(WorkingQuote {
            id,
            price: o.price,
            remaining: o.remaining,
        })
    }

    /// Decisions made so far.
    pub fn decisions(&self) -> u64 {
        self.decisions
    }

    /// Fair value estimate: the microprice of `top`. `None` unless
    /// two-sided.
    pub fn fair_value(&self, top: &TopOfBook) -> Option<f64> {
        top.microprice()
    }

    /// The quotes wanted against market `top` at `position` (see the module
    /// docs). Pure; O(1). No quotes unless `top` is two-sided.
    pub fn target(&self, top: &TopOfBook, position: i64) -> Quotes {
        let (Some(fair), Some(mbid), Some(mask)) = (self.fair_value(top), top.bid, top.ask) else {
            return Quotes::default();
        };
        let c = &self.cfg;
        let tick = c.tick as f64;
        let reservation = fair - c.skew_ticks_per_lot * position as f64 * tick;
        let half = c.half_spread_ticks * tick;
        // The epsilon keeps a value a hair under a tick boundary (float
        // rounding of an exact grid price) on the right side of it.
        let bid = ((reservation - half) / tick + 1e-9).floor() as i64 * c.tick;
        let ask = ((reservation + half) / tick - 1e-9).ceil() as i64 * c.tick;
        let bid = bid.min(mask.price.0 - c.tick).max(c.tick);
        let ask = ask.max(mbid.price.0 + c.tick);
        let size = |room: i64| (room > 0).then(|| Qty(c.quote_qty.0.min(room as u64)));
        Quotes {
            bid: size(c.max_inventory - position).map(|qty| Quote {
                price: Price(bid),
                qty,
            }),
            ask: size(c.max_inventory + position).map(|qty| Quote {
                price: Price(ask),
                qty,
            }),
        }
    }

    /// One decision against market `top`: brings the working quotes to
    /// [`target`](MarketMaker::target) at the current position with the
    /// fewest commands (none, reduce, or cancel then post-only new), all
    /// cancels first, sizes capped by the orders still open on each side.
    /// O(own orders) — a handful; no allocation once `out` has grown.
    pub fn decide(&mut self, top: &TopOfBook, out: &mut Outbox) {
        self.decisions += 1;
        let want = self.target(top, self.inventory.position);
        for side in [Side::Buy, Side::Sell] {
            self.keep_or_cancel(side, want.side(side), out);
        }
        for side in [Side::Buy, Side::Sell] {
            self.place(side, want.side(side), out);
        }
    }

    /// Applies an execution report about one of its orders: fills move the
    /// position and cash; cancels, expiries and rejections retire the
    /// order. O(own orders).
    pub fn on_delivery(&mut self, d: &Delivery) {
        if let Some(leg) = d.fill() {
            self.inventory.apply(leg.side, leg.price, leg.qty);
        }
        let Some(i) = self.orders.iter().position(|o| o.id == d.order) else {
            return;
        };
        let o = &mut self.orders[i];
        let gone = match d.event {
            Event::Trade { qty, .. } => {
                o.rested |= d.fill().is_some_and(|leg| leg.maker);
                o.remaining = o.remaining.saturating_sub(qty);
                o.remaining.is_zero()
            }
            Event::Rested { qty, .. } => {
                o.rested = true;
                o.remaining = qty;
                false
            }
            Event::Reduced { remaining, .. } => {
                o.remaining = remaining;
                false
            }
            Event::Cancelled { .. } | Event::Expired { .. } | Event::Rejected { .. } => true,
            Event::Accepted { .. } => false,
        };
        if gone {
            let id = self.orders.swap_remove(i).id;
            if self.bid == Some(id) {
                self.bid = None;
            }
            if self.ask == Some(id) {
                self.ask = None;
            }
        }
    }

    /// The market's top of book as seen in `book` minus this strategy's
    /// own resting orders (quoting around a fair value that includes your
    /// own quotes would chase your own tail). O(log levels); allocates
    /// only when its own orders are alone at the top.
    pub fn market_top(&self, book: &BookBuilder) -> TopOfBook {
        TopOfBook {
            bid: self.without_own(book, Side::Buy),
            ask: self.without_own(book, Side::Sell),
        }
    }
}

// @student-begin w42
impl MarketMaker {
    /// Our resting quantity at (`side`, `price`).
    fn own_at(&self, side: Side, price: Price) -> Qty {
        let mut q = Qty::ZERO;
        for o in &self.orders {
            if o.rested && o.side == side && o.price == price {
                q += o.remaining;
            }
        }
        q
    }

    /// Our open quantity on `side` (every state), except order `except`.
    fn open_qty(&self, side: Side, except: Option<OrderId>) -> u64 {
        self.orders
            .iter()
            .filter(|o| o.side == side && Some(o.id) != except)
            .map(|o| o.remaining.0)
            .sum()
    }

    /// How much more `side` may have open: `max − position` for bids,
    /// `max + position` for asks.
    fn room(&self, side: Side) -> i64 {
        let (max, pos) = (self.cfg.max_inventory, self.inventory.position);
        match side {
            Side::Buy => max - pos,
            Side::Sell => max + pos,
        }
    }

    fn working_id(&mut self, side: Side) -> &mut Option<OrderId> {
        match side {
            Side::Buy => &mut self.bid,
            Side::Sell => &mut self.ask,
        }
    }

    /// Keeps, reduces or cancels the working quote on `side`.
    fn keep_or_cancel(&mut self, side: Side, want: Option<Quote>, out: &mut Outbox) {
        let Some(id) = *self.working_id(side) else {
            return;
        };
        let Some(i) = self.orders.iter().position(|o| o.id == id) else {
            return;
        };
        let w = self.orders[i];
        let threshold = self.cfg.requote_ticks.max(1) * self.cfg.tick;
        let allowed = (self.room(side) - self.open_qty(side, Some(id)) as i64).max(0) as u64;
        match want {
            Some(q) if (q.price.0 - w.price.0).abs() < threshold && allowed > 0 => {
                let allowed = Qty(q.qty.0.min(allowed));
                if w.remaining > allowed {
                    out.reduce(id, allowed);
                    self.orders[i].remaining = allowed;
                }
            }
            _ => {
                // Stays in `orders` (it may still fill) until confirmed gone.
                out.cancel(id);
                *self.working_id(side) = None;
            }
        }
    }

    /// Places a new quote on `side` if none is working.
    fn place(&mut self, side: Side, want: Option<Quote>, out: &mut Outbox) {
        let (None, Some(q)) = (*self.working_id(side), want) else {
            return;
        };
        let room = self.room(side) - self.open_qty(side, None) as i64;
        let qty = q.qty.0.min(room.max(0) as u64);
        if qty == 0 {
            return;
        }
        let id = out.post_only(side, q.price, Qty(qty));
        self.orders.push(OwnOrder {
            id,
            side,
            price: q.price,
            remaining: Qty(qty),
            rested: false,
        });
        *self.working_id(side) = Some(id);
    }

    /// The market's best level on `side` without our own resting orders.
    fn without_own(&self, book: &BookBuilder, side: Side) -> Option<Level> {
        let best = match side {
            Side::Buy => book.best_bid(),
            Side::Sell => book.best_ask(),
        }?;
        let mine = self.own_at(side, best.price);
        if mine.is_zero() {
            return Some(best);
        }
        if best.qty > mine {
            return Some(Level {
                price: best.price,
                qty: best.qty - mine,
            });
        }
        // Only our orders at the top: the market is further down.
        book.depth(side, self.orders.len() + 2)
            .into_iter()
            .skip(1)
            .find_map(|l| {
                let mine = self.own_at(side, l.price);
                (l.qty > mine).then(|| Level {
                    price: l.price,
                    qty: l.qty - mine,
                })
            })
    }
}
// @student-end

// @student w42
impl Agent for MarketMaker {
    fn name(&self) -> &str {
        "market-maker"
    }

    fn act(&mut self, ctx: &mut AgentCtx<'_>) {
        let top = self.market_top(ctx.book);
        self.decide(&top, ctx.out);
    }

    fn on_event(&mut self, delivery: &Delivery) {
        self.on_delivery(delivery);
    }
}
