//! Week 41 — market microstructure: what the book and the trades say about
//! prices, liquidity and the cost of trading.
//!
//! * [`TopOfBook`] — spread, mid, top-of-book **imbalance** and the
//!   **microprice**, from L2 (market data) or the engine.
//! * [`queue_position`] (from the engine's L3 view) and [`QueueTracker`]
//!   (from engine events) — how much quantity is ahead of a resting order
//!   at its price, i.e. what must trade or cancel before it fills.
//! * [`effective_spread`], [`realized_spread`] and [`Markouts`] — what a
//!   fill earned against the mid at the time and at horizons afterwards.
//!
//! # Definitions
//!
//! With best bid `b` (size `B`) and best ask `a` (size `A`):
//!
//! ```text
//! spread     = a − b                       (price units)
//! mid        = (a + b) / 2
//! imbalance  = (B − A) / (B + A)           ∈ [−1, 1]; > 0: more bid size
//! microprice = (b·A + a·B) / (A + B)       = mid + imbalance · spread / 2
//! ```
//!
//! The microprice leans toward the side with *less* size: a thin ask is
//! about to be lifted, so the next trade is more likely up.
//!
//! For a trade at price `p`, with `d = +1` if the taker bought and `−1` if
//! it sold, mid `m₀` just before it and `mₕ` at horizon `h` after it:
//!
//! ```text
//! effective spread = 2·d·(p − m₀)     what the taker paid over the mid (round trip)
//! realized spread  = 2·d·(p − mₕ)     what the liquidity provider kept after the move
//! price impact     = 2·d·(mₕ − m₀)    = effective − realized
//! markout(h)       = d'·(mₕ − p)·q    for one's own fill on side d', qty q
//! ```
//!
//! A market maker's fill has positive markout when the price moved its way
//! afterwards; consistently negative markouts at short horizons mean it is
//! being picked off (adverse selection).
//!
//! Floats are fine here: these are analytics, off the order path. Every
//! value in the tests is a multiple of ½ and therefore exact in `f64`.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use engine::{Engine, OrderBook, RestingOrder};
use marketdata::{BookBuilder, L2Publisher, Level};
use types::{Event, OrderId, Price, Qty, Side};
// Used only by the implementation (unused in the learner template).
#[allow(unused_imports)]
use {risk::IdMap, std::collections::VecDeque};

/// The best bid and best ask of a book (either may be missing).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TopOfBook {
    /// Best (highest) bid: price and total quantity.
    pub bid: Option<Level>,
    /// Best (lowest) ask: price and total quantity.
    pub ask: Option<Level>,
}

impl TopOfBook {
    /// A top of book from `(price, qty)` pairs — shorthand for tests.
    pub fn new(bid: Option<(i64, u64)>, ask: Option<(i64, u64)>) -> TopOfBook {
        let level = |(p, q): (i64, u64)| Level {
            price: Price(p),
            qty: Qty(q),
        };
        TopOfBook {
            bid: bid.map(level),
            ask: ask.map(level),
        }
    }

    /// The top of a book rebuilt from market data. O(log levels).
    pub fn from_builder(book: &BookBuilder) -> TopOfBook {
        TopOfBook {
            bid: book.best_bid(),
            ask: book.best_ask(),
        }
    }

    /// The top of the publisher's book. O(log levels).
    pub fn from_publisher(publisher: &L2Publisher) -> TopOfBook {
        TopOfBook {
            bid: publisher.best_bid(),
            ask: publisher.best_ask(),
        }
    }

    /// The top of the engine's book. O(1) with the w06 book.
    pub fn from_engine<B: OrderBook>(engine: &Engine<B>) -> TopOfBook {
        let level = |l: engine::LevelInfo| Level {
            price: l.price,
            qty: l.qty,
        };
        TopOfBook {
            bid: engine.book().best(Side::Buy).map(level),
            ask: engine.book().best(Side::Sell).map(level),
        }
    }

    /// True if both sides are present.
    pub fn is_two_sided(&self) -> bool {
        self.bid.is_some() && self.ask.is_some()
    }
}

#[allow(unused_variables)]
impl TopOfBook {
    /// `ask − bid` in price units; `None` unless two-sided. O(1).
    pub fn spread(&self) -> Option<i64> {
        todo!("w41: spread")
    }

    /// `(bid + ask) / 2`; `None` unless two-sided. O(1).
    pub fn mid(&self) -> Option<f64> {
        todo!("w41: mid")
    }

    /// `(B − A) / (B + A)` in `[−1, 1]` from the best bid and ask sizes;
    /// `None` unless two-sided. O(1).
    pub fn imbalance(&self) -> Option<f64> {
        todo!("w41: imbalance")
    }

    /// Size-weighted mid `(bid·A + ask·B) / (A + B)`: always within
    /// `[bid, ask]`, closer to the side with less size. `None` unless
    /// two-sided. O(1).
    pub fn microprice(&self) -> Option<f64> {
        todo!("w41: microprice")
    }
}

/// Where a resting order stands in the queue at its price.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct QueuePosition {
    /// Orders ahead of it at the same price (they fill first).
    pub orders_ahead: u32,
    /// Their total remaining quantity: what must trade (or cancel) at this
    /// price before the order fills.
    pub qty_ahead: Qty,
    /// The order's own remaining quantity.
    pub remaining: Qty,
}

/// The queue position of resting order `id`, read from the engine's L3
/// view (the orders at its level in time priority); `None` if it is not
/// resting. O(orders at that level).
#[allow(unused_variables)]
pub fn queue_position<B: OrderBook>(engine: &Engine<B>, id: OrderId) -> Option<QueuePosition> {
    todo!("w41: queue_position")
}

/// Follows one resting order's queue position from engine events.
///
/// Start it from the L3 view of the order's level at any moment (for
/// example right after its `Rested` event, from
/// [`Engine::level_orders`]); from then on it needs only the engine's
/// events: trades against, cancels of and reductions of the orders ahead
/// shrink the queue; orders arriving later queue behind and are ignored.
/// It keeps the ids ahead in a hash map: O(1) per event, O(orders ahead)
/// memory.
#[derive(Clone, Debug)]
pub struct QueueTracker {
    // TODO(w41): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl QueueTracker {
    /// Starts tracking `id` given the orders resting at its level in time
    /// priority (as `Engine::level_orders` returns them); `None` if `id` is
    /// not among them. O(orders ahead).
    pub fn new(id: OrderId, level: &[RestingOrder]) -> Option<QueueTracker> {
        todo!("w41: new")
    }

    /// The tracked order.
    pub fn id(&self) -> OrderId {
        todo!("w41: id")
    }

    /// Applies one engine event (any order; unrelated events are ignored).
    /// O(1).
    pub fn on_event(&mut self, event: &Event) {
        todo!("w41: on_event")
    }

    /// The current queue position; `None` once the order is filled or
    /// cancelled. O(1).
    pub fn position(&self) -> Option<QueuePosition> {
        todo!("w41: position")
    }
}

/// `2·d·(price − mid)` for a trade whose taker was on `taker_side`, against
/// the mid just before it: what the taker paid over the mid, round trip.
/// O(1).
#[allow(unused_variables)]
pub fn effective_spread(taker_side: Side, price: Price, mid_before: f64) -> f64 {
    todo!("w41: effective_spread")
}

/// `2·d·(price − mid_later)`: what the liquidity provider kept once the
/// mid had moved to `mid_later`. O(1).
#[allow(unused_variables)]
pub fn realized_spread(taker_side: Side, price: Price, mid_later: f64) -> f64 {
    todo!("w41: realized_spread")
}

// TODO(w41): implement — see the week guide on tepy.dev/hft.

/// Markouts of a set of fills at one horizon.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MarkoutStats {
    /// Fills resolved at this horizon.
    pub fills: u64,
    /// Their total quantity.
    pub qty: u64,
    /// `Σ d·(m_h − p)·q`, in price units × lots: positive when prices moved
    /// in the fills' favour.
    pub total: f64,
}

impl MarkoutStats {
    /// Average markout per lot; `None` before any fill. O(1).
    pub fn per_unit(&self) -> Option<f64> {
        (self.qty > 0).then(|| self.total / self.qty as f64)
    }
}

/// Markouts of fills at fixed horizons after each fill.
///
/// Time is any non-decreasing `u64` (a command sequence number, a step, or
/// nanoseconds). Feed the mid whenever it may have changed
/// ([`on_mid`](Markouts::on_mid)) and every fill
/// ([`on_fill`](Markouts::on_fill)), in time order. A fill at `t` is
/// marked at horizon `h` with **the mid in effect at `t + h`: the last mid
/// recorded at a time ≤ `t + h`** (or the first mid ever recorded, for a
/// horizon that ended before it). That is known only once a mid with a
/// later time arrives (or at [`finish`](Markouts::finish)), so results
/// trail the clock by one update.
///
/// Horizon 0 uses the mid in effect *at* the fill time, so if you record
/// the post-trade mid at the same `t` as the fill (as the tests do), the
/// horizon-0 markout already includes the fill's own price impact. Record
/// the mid before the fill, or use a small positive horizon, for the
/// pre-trade reference.
///
/// O(horizons) memory per fill still pending; each fill is resolved once
/// per horizon, in O(1).
#[derive(Clone, Debug)]
pub struct Markouts {
    // TODO(w41): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl Markouts {
    /// A tracker for the given horizons (in the same unit as the times).
    pub fn new(horizons: &[u64]) -> Markouts {
        todo!("w41: new")
    }

    /// The horizons, as given.
    pub fn horizons(&self) -> &[u64] {
        todo!("w41: horizons")
    }

    /// Records the mid at time `t` (≥ every earlier time), first resolving
    /// the fills whose horizon ended before `t`. Amortized O(horizons).
    pub fn on_mid(&mut self, t: u64, mid: f64) {
        todo!("w41: on_mid")
    }

    /// Records our fill at time `t`: `qty` lots on `side` at `price`.
    /// O(horizons).
    pub fn on_fill(&mut self, t: u64, side: Side, price: Price, qty: Qty) {
        todo!("w41: on_fill")
    }

    /// Ends the clock at `t`: resolves every fill whose horizon ended at or
    /// before `t` with the current mid. Later fills stay pending.
    pub fn finish(&mut self, t: u64) {
        todo!("w41: finish")
    }

    /// Results per horizon (same order as [`horizons`](Markouts::horizons)),
    /// counting resolved fills only.
    pub fn stats(&self) -> &[MarkoutStats] {
        todo!("w41: stats")
    }

    /// Fill × horizon pairs not resolved yet.
    pub fn pending(&self) -> usize {
        todo!("w41: pending")
    }
}

// TODO(w41): implement — see the week guide on tepy.dev/hft.
