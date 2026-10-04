//! Shared domain types for the exchange.
//!
//! Every crate speaks these types: the gateway decodes wire messages into
//! [`Command`]s, the engine turns commands into [`Event`]s, the journal
//! records commands, market data publishes from events, risk inspects
//! orders and fills. Keeping them in one small crate is what lets each
//! subsystem be built (and tested) on its own.
//!
//! Prices are integer ticks and quantities are integer lots — no floats on
//! the order path, ever. Conversion to human units happens at the edges.

use std::fmt;

/// Price in integer ticks (tick size is per instrument). Signed so spreads
/// and price differences are representable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Price(pub i64);

/// Quantity in integer lots.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Qty(pub u64);

/// Exchange-assigned order id, unique for the life of the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct OrderId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct AccountId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct InstrumentId(pub u32);

/// Engine sequence number assigned to every accepted command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Seq(pub u64);

impl Qty {
    pub const ZERO: Qty = Qty(0);

    pub fn is_zero(self) -> bool {
        self.0 == 0
    }

    pub fn saturating_sub(self, other: Qty) -> Qty {
        Qty(self.0.saturating_sub(other.0))
    }

    pub fn min(self, other: Qty) -> Qty {
        Qty(self.0.min(other.0))
    }
}

impl std::ops::Add for Qty {
    type Output = Qty;
    fn add(self, rhs: Qty) -> Qty {
        Qty(self.0 + rhs.0)
    }
}

impl std::ops::AddAssign for Qty {
    fn add_assign(&mut self, rhs: Qty) {
        self.0 += rhs.0;
    }
}

impl std::ops::Sub for Qty {
    type Output = Qty;
    fn sub(self, rhs: Qty) -> Qty {
        Qty(self.0 - rhs.0)
    }
}

impl std::ops::SubAssign for Qty {
    fn sub_assign(&mut self, rhs: Qty) {
        self.0 -= rhs.0;
    }
}

impl fmt::Display for Price {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for Qty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn opposite(self) -> Side {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }

    /// True if a `self`-side order at `limit` can trade with a resting
    /// order at `resting` (buyers pay up to their limit, sellers accept
    /// down to theirs).
    pub fn crosses(self, limit: Price, resting: Price) -> bool {
        match self {
            Side::Buy => resting <= limit,
            Side::Sell => resting >= limit,
        }
    }
}

/// How long an order may rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum TimeInForce {
    /// Good till cancelled: rests until filled or cancelled.
    #[default]
    Gtc,
    /// Immediate or cancel: fill what you can now, cancel the rest.
    Ioc,
    /// Fill or kill: fill entirely now or not at all.
    Fok,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OrderKind {
    /// Limit order. `post_only` orders are rejected rather than taking
    /// liquidity.
    Limit { price: Price, post_only: bool },
    /// Market order: takes liquidity at any price; never rests.
    Market,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NewOrder {
    pub id: OrderId,
    pub account: AccountId,
    pub instrument: InstrumentId,
    pub side: Side,
    pub qty: Qty,
    pub kind: OrderKind,
    pub tif: TimeInForce,
}

impl NewOrder {
    /// A plain GTC limit order — the most common case in tests.
    pub fn limit(id: u64, side: Side, price: i64, qty: u64) -> NewOrder {
        NewOrder {
            id: OrderId(id),
            account: AccountId(1),
            instrument: InstrumentId(1),
            side,
            qty: Qty(qty),
            kind: OrderKind::Limit { price: Price(price), post_only: false },
            tif: TimeInForce::Gtc,
        }
    }

    pub fn market(id: u64, side: Side, qty: u64) -> NewOrder {
        NewOrder {
            id: OrderId(id),
            account: AccountId(1),
            instrument: InstrumentId(1),
            side,
            qty: Qty(qty),
            kind: OrderKind::Market,
            tif: TimeInForce::Ioc,
        }
    }

    pub fn with_tif(mut self, tif: TimeInForce) -> NewOrder {
        self.tif = tif;
        self
    }

    pub fn with_account(mut self, account: u32) -> NewOrder {
        self.account = AccountId(account);
        self
    }

    pub fn post_only(mut self) -> NewOrder {
        if let OrderKind::Limit { price, .. } = self.kind {
            self.kind = OrderKind::Limit { price, post_only: true };
        }
        self
    }

    pub fn limit_price(&self) -> Option<Price> {
        match self.kind {
            OrderKind::Limit { price, .. } => Some(price),
            OrderKind::Market => None,
        }
    }
}

/// Inbound commands, in the order the sequencer assigns them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    New(NewOrder),
    Cancel { id: OrderId },
    /// Reduce the remaining quantity of a resting order to `qty`
    /// (keeps time priority; increases are rejected).
    Reduce { id: OrderId, qty: Qty },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RejectReason {
    DuplicateOrderId,
    UnknownOrder,
    ZeroQuantity,
    /// Price is not a multiple of the instrument's tick size.
    InvalidTick,
    /// Quantity is not a multiple of the lot size.
    InvalidLot,
    /// Price outside the allowed band around the reference price.
    PriceBand,
    /// Post-only order would have taken liquidity.
    WouldTake,
    /// Fill-or-kill could not be filled entirely.
    FokUnfillable,
    /// Market order with no liquidity on the other side.
    NoLiquidity,
    /// Reduce to a quantity not lower than the current remaining quantity.
    InvalidReduce,
    /// Rejected by pre-trade risk.
    Risk(RiskReason),
    /// Instrument halted / kill switch engaged.
    Halted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RiskReason {
    MaxOrderQty,
    MaxNotional,
    PositionLimit,
    RateLimit,
    KillSwitch,
}

/// Outbound events. For one inbound command the engine emits them in a
/// fixed, documented order (see the engine crate), which is what makes the
/// whole exchange deterministic and replayable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Event {
    Accepted { id: OrderId },
    Rejected { id: OrderId, reason: RejectReason },
    /// A fill between an incoming (taker) and a resting (maker) order at
    /// the maker's price.
    Trade { taker: OrderId, maker: OrderId, taker_side: Side, price: Price, qty: Qty },
    /// The remainder of an order now rests on the book.
    Rested { id: OrderId, side: Side, price: Price, qty: Qty },
    /// Cancelled by request; `remaining` is what was left on the book.
    Cancelled { id: OrderId, remaining: Qty },
    /// IOC / market remainder that could not rest.
    Expired { id: OrderId, remaining: Qty },
    /// A resting order's remaining quantity was reduced.
    Reduced { id: OrderId, remaining: Qty },
}

impl Event {
    /// The order this event is about (the taker, for trades).
    pub fn order_id(&self) -> OrderId {
        match *self {
            Event::Accepted { id }
            | Event::Rejected { id, .. }
            | Event::Rested { id, .. }
            | Event::Cancelled { id, .. }
            | Event::Expired { id, .. }
            | Event::Reduced { id, .. } => id,
            Event::Trade { taker, .. } => taker,
        }
    }
}

/// Instrument reference data the engine and risk validate against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InstrumentSpec {
    pub id: InstrumentId,
    pub tick: i64,
    pub lot: u64,
    /// Allowed distance from the reference price, in ticks (0 = no band).
    pub band_ticks: i64,
}

impl Default for InstrumentSpec {
    fn default() -> Self {
        InstrumentSpec { id: InstrumentId(1), tick: 1, lot: 1, band_ticks: 0 }
    }
}

/// Small, fast, deterministic PRNG (SplitMix64) for tests, simulations and
/// benchmarks — reproducible from a seed, no external crates.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `lo..=hi`.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        debug_assert!(lo <= hi);
        lo + self.next_u64() % (hi - lo + 1)
    }

    pub fn range_i64(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next_u64() % ((hi - lo + 1) as u64)) as i64
    }

    /// True with probability `p`.
    pub fn chance(&mut self, p: f64) -> bool {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64 <= p
    }

    pub fn side(&mut self) -> Side {
        if self.next_u64() & 1 == 0 {
            Side::Buy
        } else {
            Side::Sell
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crosses() {
        assert!(Side::Buy.crosses(Price(100), Price(99)));
        assert!(Side::Buy.crosses(Price(100), Price(100)));
        assert!(!Side::Buy.crosses(Price(100), Price(101)));
        assert!(Side::Sell.crosses(Price(100), Price(101)));
        assert!(!Side::Sell.crosses(Price(100), Price(99)));
    }

    #[test]
    fn rng_is_deterministic() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        let mut r = Rng::new(1);
        for _ in 0..1000 {
            let v = r.range(3, 9);
            assert!((3..=9).contains(&v));
        }
    }
}
