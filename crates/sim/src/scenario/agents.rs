//! Built-in simulated traders: market makers, noise takers and momentum
//! takers. Each is deterministic given its random number generator.

use super::agent::{Agent, AgentCtx, Delivery, Quotes};
use types::{Price, Qty};
// Used only by the implementation (unused in the learner template).
#[allow(unused_imports)]
use {
    super::agent::Quote,
    std::collections::VecDeque,
    types::{Event, OrderId, Side},
};

/// How a [`MakerAgent`] quotes. Prices are in price units, distances in
/// ticks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MakerConfig {
    /// The value the maker believes in and quotes around.
    pub fair_value: Price,
    /// Distance of each quote from the (skewed) center, in ticks (≥ 1).
    pub half_spread_ticks: i64,
    /// Size of each quote.
    pub qty: Qty,
    /// The center moves one tick against the inventory per this many lots
    /// held (long → lower quotes, short → higher). `0` disables skew.
    pub lots_per_tick: i64,
    /// The maker never quotes a size that could take `|position|` beyond
    /// this: bid size ≤ `max_position − position`, ask size ≤
    /// `max_position + position`.
    pub max_position: i64,
    /// The instrument's tick size.
    pub tick: i64,
}

impl Default for MakerConfig {
    fn default() -> Self {
        MakerConfig {
            fair_value: Price(10_000),
            half_spread_ticks: 2,
            qty: Qty(10),
            lots_per_tick: 5,
            max_position: 100,
            tick: 1,
        }
    }
}

/// A market maker: keeps one bid and one ask around its fair value,
/// skewed against its inventory.
///
/// On each turn it computes `center = fair − (position / lots_per_tick)`
/// ticks (integer division, toward zero), wants a bid at
/// `center − half_spread` and an ask at `center + half_spread`, sized by
/// [`MakerConfig::max_position`], and replaces a resting quote (cancel +
/// new) when its price differs from the wanted one or it is larger than
/// allowed — all cancels before any new quote, so it never trades with
/// itself. A partially filled quote at the right price is left alone (it
/// keeps its queue position); a fully filled one is replaced next turn.
// @student w23
pub struct MakerAgent {
    cfg: MakerConfig,
    bid: Option<Working>,
    ask: Option<Working>,
}

// @student-begin w23
/// A quote the maker believes is resting.
#[derive(Clone, Copy, Debug)]
struct Working {
    id: OrderId,
    price: Price,
    remaining: Qty,
}

/// Cancels `working` unless it still matches `want` (same price, not
/// larger than allowed).
fn cancel_stale(ctx: &mut AgentCtx<'_>, working: &mut Option<Working>, want: Option<Quote>) {
    if let Some(w) = *working {
        if !want.is_some_and(|q| q.price == w.price && w.remaining <= q.qty) {
            ctx.out.cancel(w.id);
            *working = None;
        }
    }
}

/// Places `want` if no quote is working on that side.
fn place(ctx: &mut AgentCtx<'_>, side: Side, working: &mut Option<Working>, want: Option<Quote>) {
    if let (None, Some(q)) = (*working, want) {
        let id = ctx.out.limit(side, q.price, q.qty);
        *working = Some(Working {
            id,
            price: q.price,
            remaining: q.qty,
        });
    }
}

/// Applies a delivery to the quote it concerns (if any).
fn track(quote: &mut Option<Working>, d: &Delivery) {
    let Some(q) = quote.as_mut() else { return };
    if d.order != q.id {
        return;
    }
    match d.event {
        Event::Trade { qty, .. } => {
            q.remaining = q.remaining.saturating_sub(qty);
            if q.remaining.is_zero() {
                *quote = None;
            }
        }
        Event::Reduced { remaining, .. } => q.remaining = remaining,
        Event::Cancelled { .. } | Event::Expired { .. } | Event::Rejected { .. } => *quote = None,
        Event::Accepted { .. } | Event::Rested { .. } => {}
    }
}
// @student-end

// @student w23
impl MakerAgent {
    /// A maker with no quotes yet.
    pub fn new(cfg: MakerConfig) -> MakerAgent {
        MakerAgent {
            cfg,
            bid: None,
            ask: None,
        }
    }

    /// The bid and ask the maker wants at `position`; `None` for a side it
    /// must not quote. Pure, O(1).
    pub fn quotes(&self, position: i64) -> Quotes {
        let c = &self.cfg;
        let skew = if c.lots_per_tick > 0 {
            position / c.lots_per_tick
        } else {
            0
        };
        let center = c.fair_value.0 - skew * c.tick;
        let quote = |room: i64, price: i64| {
            (room > 0).then(|| Quote {
                price: Price(price),
                qty: Qty(c.qty.0.min(room as u64)),
            })
        };
        Quotes {
            bid: quote(
                c.max_position - position,
                center - c.half_spread_ticks * c.tick,
            ),
            ask: quote(
                c.max_position + position,
                center + c.half_spread_ticks * c.tick,
            ),
        }
    }
}

// @student w23
impl Agent for MakerAgent {
    fn name(&self) -> &str {
        "maker"
    }

    fn act(&mut self, ctx: &mut AgentCtx<'_>) {
        // All cancels first: a new quote must not trade with this maker's
        // own stale quote on the other side.
        let want = self.quotes(ctx.inventory.position);
        cancel_stale(ctx, &mut self.bid, want.bid);
        cancel_stale(ctx, &mut self.ask, want.ask);
        place(ctx, Side::Buy, &mut self.bid, want.bid);
        place(ctx, Side::Sell, &mut self.ask, want.ask);
    }

    fn on_event(&mut self, d: &Delivery) {
        track(&mut self.bid, d);
        track(&mut self.ask, d);
    }
}

/// How a [`NoiseAgent`] trades.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoiseConfig {
    /// Chance of trading on a turn.
    pub p_trade: f64,
    /// Chance that a trade is a buy (0.5: no bias).
    pub p_buy: f64,
    /// Order size is uniform in `1..=max_qty` lots.
    pub max_qty: u64,
    /// `None`: market orders. `Some(k)`: IOC limit orders `k` ticks through
    /// the opposite best price (no order when that side is empty).
    pub slippage_ticks: Option<i64>,
    /// The instrument's tick size.
    pub tick: i64,
}

impl Default for NoiseConfig {
    fn default() -> Self {
        NoiseConfig {
            p_trade: 0.5,
            p_buy: 0.5,
            max_qty: 5,
            slippage_ticks: Some(1),
            tick: 1,
        }
    }
}

/// An uninformed taker: trades at random, on a random side, at a random
/// size, whatever the price. Market makers earn the spread from it.
///
/// Random draws per turn, in this order: trade? (`chance(p_trade)`), then
/// buy? (`chance(p_buy)`), then size (`range(1, max_qty)`).
// @student w23
pub struct NoiseAgent {
    cfg: NoiseConfig,
}

// @student w23
impl NoiseAgent {
    /// A noise taker.
    pub fn new(cfg: NoiseConfig) -> NoiseAgent {
        NoiseAgent { cfg }
    }
}

// @student w23
impl Agent for NoiseAgent {
    fn name(&self) -> &str {
        "noise"
    }

    fn act(&mut self, ctx: &mut AgentCtx<'_>) {
        let c = self.cfg;
        if !ctx.rng.chance(c.p_trade) {
            return;
        }
        let side = if ctx.rng.chance(c.p_buy) {
            Side::Buy
        } else {
            Side::Sell
        };
        let qty = Qty(ctx.rng.range(1, c.max_qty.max(1)));
        match c.slippage_ticks {
            None => {
                ctx.out.market(side, qty);
            }
            Some(k) => {
                let touch = match side {
                    Side::Buy => ctx.book.best_ask(),
                    Side::Sell => ctx.book.best_bid(),
                };
                if let Some(level) = touch {
                    let through = if side == Side::Buy {
                        k * c.tick
                    } else {
                        -k * c.tick
                    };
                    ctx.out.ioc(side, Price(level.price.0 + through), qty);
                }
            }
        }
    }

    fn on_event(&mut self, _: &Delivery) {}
}

/// How a [`MomentumAgent`] trades.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MomentumConfig {
    /// Compare the mid now with the mid this many of its turns ago.
    pub lookback: usize,
    /// Trade when the mid moved at least this many ticks.
    pub threshold_ticks: i64,
    /// Order size.
    pub qty: Qty,
    /// Never trade beyond this absolute position.
    pub max_position: i64,
    /// The instrument's tick size.
    pub tick: i64,
}

impl Default for MomentumConfig {
    fn default() -> Self {
        MomentumConfig {
            lookback: 10,
            threshold_ticks: 2,
            qty: Qty(5),
            max_position: 50,
            tick: 1,
        }
    }
}

/// A trend follower: buys after the mid rose by the threshold over the
/// lookback, sells after it fell, with IOC orders at the touch, within its
/// position limit.
///
/// It samples the mid (as `bid + ask`, in half-ticks, so it stays an
/// integer) once per turn when the book is two-sided; the first
/// `lookback` turns only fill the history. It draws no random numbers.
// @student w23
pub struct MomentumAgent {
    cfg: MomentumConfig,
    mids: VecDeque<i64>,
}

// @student w23
impl MomentumAgent {
    /// A momentum taker with an empty history.
    pub fn new(cfg: MomentumConfig) -> MomentumAgent {
        MomentumAgent {
            cfg,
            mids: VecDeque::with_capacity(cfg.lookback + 1),
        }
    }
}

// @student w23
impl Agent for MomentumAgent {
    fn name(&self) -> &str {
        "momentum"
    }

    fn act(&mut self, ctx: &mut AgentCtx<'_>) {
        let c = self.cfg;
        let (Some(bid), Some(ask)) = (ctx.book.best_bid(), ctx.book.best_ask()) else {
            return;
        };
        let mid2 = bid.price.0 + ask.price.0;
        self.mids.push_back(mid2);
        if self.mids.len() <= c.lookback {
            return;
        }
        let then = self.mids.pop_front().unwrap_or(mid2);
        let moved = mid2 - then;
        let threshold = 2 * c.threshold_ticks * c.tick;
        let position = ctx.inventory.position;
        let qty = c.qty.0 as i64;
        if moved >= threshold && position + qty <= c.max_position {
            ctx.out.ioc(Side::Buy, ask.price, c.qty);
        } else if moved <= -threshold && position - qty >= -c.max_position {
            ctx.out.ioc(Side::Sell, bid.price, c.qty);
        }
    }

    fn on_event(&mut self, _: &Delivery) {}
}
