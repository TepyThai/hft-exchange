//! The agent contract: what a simulated trader sees, what it can send, and
//! what it is told back.
//!
//! Everything here is provided except [`Inventory`]'s accounting; the
//! agents, the venue and the runner that use it are the week's exercise.

use marketdata::BookBuilder;
use types::{
    AccountId, Command, Event, InstrumentId, NewOrder, OrderId, OrderKind, Price, Qty, Rng, Side,
    TimeInForce,
};

/// A simulated trader.
///
/// The runner calls [`act`](Agent::act) once per turn and
/// [`on_event`](Agent::on_event) for every event about one of the agent's
/// own orders, in the order the venue produced them — including
/// pre-trade risk rejections, which never reach the engine. Implement it to
/// add your own trader to a scenario; the w42 market maker and the w44
/// backtester use the same trait.
pub trait Agent {
    /// Short name for reports (e.g. `"maker"`).
    fn name(&self) -> &str;

    /// The agent's turn: look at `ctx` (the public book, its own
    /// inventory) and send commands through `ctx.out`.
    fn act(&mut self, ctx: &mut AgentCtx<'_>);

    /// An event about one of this agent's orders. For a trade,
    /// [`Delivery::order`] says which leg is the agent's and
    /// [`Delivery::fill`] gives the leg's side, price and quantity.
    fn on_event(&mut self, delivery: &Delivery);
}

/// What an agent sees and uses on its turn.
pub struct AgentCtx<'a> {
    /// Simulated time: the step number in the scenario runner (w23),
    /// nanoseconds in the backtester (w44).
    pub now: u64,
    /// The agent's account; [`Outbox`] stamps it on every order.
    pub account: AccountId,
    /// The public book, rebuilt from the market-data feed. This is all the
    /// agent knows about other traders' orders (no look-ahead, no L3).
    pub book: &'a BookBuilder,
    /// The agent's fills so far.
    pub inventory: &'a Inventory,
    /// The agent's own random number generator, seeded from the scenario
    /// seed, so a run is reproducible whatever the other agents do.
    pub rng: &'a mut Rng,
    /// Where the agent's commands go; they are sent in order when the turn
    /// ends.
    pub out: &'a mut Outbox,
}

/// Collects one agent's commands: assigns order ids from the agent's own
/// id range and stamps its account and instrument on every order.
///
/// Ids are unique per agent by construction (each agent gets a disjoint
/// range), which is what a gateway's client-order-id mapping provides in a
/// real exchange. Every method is O(1); the command buffer is reused, so a
/// warmed-up outbox does not allocate.
#[derive(Clone, Debug)]
pub struct Outbox {
    account: AccountId,
    instrument: InstrumentId,
    next_id: u64,
    commands: Vec<Command>,
}

impl Outbox {
    /// An empty outbox whose first order id is `first_id`.
    pub fn new(account: AccountId, instrument: InstrumentId, first_id: u64) -> Outbox {
        Outbox {
            account,
            instrument,
            next_id: first_id,
            commands: Vec::with_capacity(16),
        }
    }

    /// The account stamped on every order.
    pub fn account(&self) -> AccountId {
        self.account
    }

    /// The instrument stamped on every order.
    pub fn instrument(&self) -> InstrumentId {
        self.instrument
    }

    /// The id the next order will get.
    pub fn peek_id(&self) -> OrderId {
        OrderId(self.next_id)
    }

    /// Queues a new order and returns its id.
    pub fn submit(&mut self, side: Side, kind: OrderKind, qty: Qty, tif: TimeInForce) -> OrderId {
        let id = OrderId(self.next_id);
        self.next_id += 1;
        self.commands.push(Command::New(NewOrder {
            id,
            account: self.account,
            instrument: self.instrument,
            side,
            qty,
            kind,
            tif,
        }));
        id
    }

    /// Queues a good-till-cancel limit order.
    pub fn limit(&mut self, side: Side, price: Price, qty: Qty) -> OrderId {
        self.submit(
            side,
            OrderKind::Limit {
                price,
                post_only: false,
            },
            qty,
            TimeInForce::Gtc,
        )
    }

    /// Queues a post-only GTC limit order (rejected rather than taking).
    pub fn post_only(&mut self, side: Side, price: Price, qty: Qty) -> OrderId {
        self.submit(
            side,
            OrderKind::Limit {
                price,
                post_only: true,
            },
            qty,
            TimeInForce::Gtc,
        )
    }

    /// Queues an immediate-or-cancel limit order.
    pub fn ioc(&mut self, side: Side, price: Price, qty: Qty) -> OrderId {
        self.submit(
            side,
            OrderKind::Limit {
                price,
                post_only: false,
            },
            qty,
            TimeInForce::Ioc,
        )
    }

    /// Queues a market order.
    pub fn market(&mut self, side: Side, qty: Qty) -> OrderId {
        self.submit(side, OrderKind::Market, qty, TimeInForce::Ioc)
    }

    /// Queues a cancel.
    pub fn cancel(&mut self, id: OrderId) {
        self.commands.push(Command::Cancel { id });
    }

    /// Queues a reduce of a resting order to `qty` (keeps time priority).
    pub fn reduce(&mut self, id: OrderId, qty: Qty) {
        self.commands.push(Command::Reduce { id, qty });
    }

    /// The commands queued so far, oldest first.
    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    /// Removes and returns the queued commands, oldest first.
    pub fn drain(&mut self) -> std::vec::Drain<'_, Command> {
        self.commands.drain(..)
    }

    /// Drops the queued commands (ids already handed out stay used).
    pub fn clear(&mut self) {
        self.commands.clear();
    }

    /// Number of queued commands.
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// True if nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

/// One event about one order, delivered to the account that owns the order
/// — an execution report.
///
/// A trade concerns two orders and is delivered twice, once per leg (twice
/// to the same account if it traded with itself); `order` says which leg
/// this copy is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Delivery {
    /// The account that owns `order`.
    pub account: AccountId,
    /// The order this delivery is about (for a trade: this account's leg).
    pub order: OrderId,
    /// What happened, exactly as the engine (or risk) reported it.
    pub event: Event,
}

impl Delivery {
    /// For a trade, this leg's fill (side, price, quantity, maker or
    /// taker); `None` for every other event. O(1).
    pub fn fill(&self) -> Option<FillLeg> {
        match self.event {
            Event::Trade {
                taker,
                maker,
                taker_side,
                price,
                qty,
            } => {
                let is_maker = self.order == maker && self.order != taker;
                if self.order != taker && !is_maker {
                    return None;
                }
                let side = if is_maker {
                    taker_side.opposite()
                } else {
                    taker_side
                };
                Some(FillLeg {
                    order: self.order,
                    side,
                    price,
                    qty,
                    maker: is_maker,
                })
            }
            _ => None,
        }
    }
}

/// One side of a trade, from the point of view of the order's owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FillLeg {
    /// The order that filled.
    pub order: OrderId,
    /// The side of *this* order.
    pub side: Side,
    /// Trade price (the maker's price).
    pub price: Price,
    /// Quantity filled.
    pub qty: Qty,
    /// True if this order was resting (maker), false if it was incoming.
    pub maker: bool,
}

/// A trader's position and cash from its fills, plus activity counters.
///
/// Cash is in price units × lots: a buy of `q` at `p` costs `p × q`. Across
/// all traders of a closed simulation, positions and cash both sum to zero
/// (every trade has a buyer and a seller), which makes a good end-to-end
/// check.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Inventory {
    /// Net position in lots: long > 0, short < 0.
    pub position: i64,
    /// Cash flow so far: + sells, − buys.
    pub cash: i128,
    /// Number of fills (trade legs).
    pub fills: u64,
    /// Total filled quantity, both sides.
    pub volume: u64,
    /// Total traded notional `Σ |price| × qty`.
    pub turnover: u128,
}

// @student w23
impl Inventory {
    /// Applies one fill. O(1).
    pub fn apply(&mut self, side: Side, price: Price, qty: Qty) {
        let notional = price.0 as i128 * qty.0 as i128;
        match side {
            Side::Buy => {
                self.position += qty.0 as i64;
                self.cash -= notional;
            }
            Side::Sell => {
                self.position -= qty.0 as i64;
                self.cash += notional;
            }
        }
        self.fills += 1;
        self.volume += qty.0;
        self.turnover += notional.unsigned_abs();
    }

    /// Mark-to-market profit and loss at `mark`: `cash + position × mark`,
    /// exact. O(1).
    pub fn pnl(&self, mark: Price) -> i128 {
        self.cash + self.position as i128 * mark.0 as i128
    }

    /// [`pnl`](Inventory::pnl) at a fractional mark (a mid or microprice).
    /// O(1).
    pub fn pnl_at(&self, mark: f64) -> f64 {
        self.cash as f64 + self.position as f64 * mark
    }
}

/// One quote: a price and a size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Quote {
    /// Limit price.
    pub price: Price,
    /// Size.
    pub qty: Qty,
}

/// The quotes a market maker wants; `None` for a side it must not quote.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Quotes {
    /// The buy quote.
    pub bid: Option<Quote>,
    /// The sell quote.
    pub ask: Option<Quote>,
}

impl Quotes {
    /// The quote on `side` (`Buy`: the bid).
    pub fn side(&self, side: Side) -> Option<Quote> {
        match side {
            Side::Buy => self.bid,
            Side::Sell => self.ask,
        }
    }
}
