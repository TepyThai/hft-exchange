//! Built-in simulated traders: market makers, noise takers and momentum
//! takers. Each is deterministic given its random number generator.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
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
pub struct MakerAgent {
    // TODO(w23): design this type's fields.
    _todo: (),
}

// TODO(w23): implement — see the week guide on tepy.dev/hft.

#[allow(unused_variables)]
impl MakerAgent {
    /// A maker with no quotes yet.
    pub fn new(cfg: MakerConfig) -> MakerAgent {
        todo!("w23: new")
    }

    /// The bid and ask the maker wants at `position`; `None` for a side it
    /// must not quote. Pure, O(1).
    pub fn quotes(&self, position: i64) -> Quotes {
        todo!("w23: quotes")
    }
}

#[allow(unused_variables)]
impl Agent for MakerAgent {
    fn name(&self) -> &str {
        todo!("w23: name")
    }

    fn act(&mut self, ctx: &mut AgentCtx<'_>) {
        todo!("w23: act")
    }

    fn on_event(&mut self, d: &Delivery) {
        todo!("w23: on_event")
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
pub struct NoiseAgent {
    // TODO(w23): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl NoiseAgent {
    /// A noise taker.
    pub fn new(cfg: NoiseConfig) -> NoiseAgent {
        todo!("w23: new")
    }
}

#[allow(unused_variables)]
impl Agent for NoiseAgent {
    fn name(&self) -> &str {
        todo!("w23: name")
    }

    fn act(&mut self, ctx: &mut AgentCtx<'_>) {
        todo!("w23: act")
    }

    fn on_event(&mut self, _: &Delivery) {
        todo!("w23: on_event")
    }
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
pub struct MomentumAgent {
    // TODO(w23): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl MomentumAgent {
    /// A momentum taker with an empty history.
    pub fn new(cfg: MomentumConfig) -> MomentumAgent {
        todo!("w23: new")
    }
}

#[allow(unused_variables)]
impl Agent for MomentumAgent {
    fn name(&self) -> &str {
        todo!("w23: name")
    }

    fn act(&mut self, ctx: &mut AgentCtx<'_>) {
        todo!("w23: act")
    }

    fn on_event(&mut self, _: &Delivery) {
        todo!("w23: on_event")
    }
}
