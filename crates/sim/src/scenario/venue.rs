//! Where agents' commands go: the [`Venue`] trait and the in-process
//! pipeline **risk → engine → market data**.
//!
//! The runner talks to the exchange only through [`Venue`], in two calls:
//! [`send`](Venue::send) a command on behalf of an account, then
//! [`flush`](Venue::flush) to collect what came back — execution reports
//! per account ([`Delivery`]) and the L2 feed ([`L2Update`]). That is
//! exactly the shape of a network session, so the TCP gateway (w13/w24)
//! slots in as another implementation:
//!
//! * `send` encodes the command and writes it on the account's session;
//! * `flush` reads until every command sent so far has been answered
//!   (each command produces at least one response), demultiplexes
//!   execution reports by session into deliveries and appends the
//!   market-data packets it received;
//! * `market_snapshot` asks the snapshot channel, for joining late or
//!   recovering from a gap.
//!
//! Because `flush` waits for every response, a run over the network stays
//! as deterministic as the in-process one: the same seed gives the same
//! command sequence, and the exchange is deterministic.

use super::agent::Delivery;
use super::runner::ScenarioConfig;
use marketdata::{L2Snapshot, L2Update};
use types::{AccountId, Command};
// Used only by the implementation (unused in the learner template).
#[allow(unused_imports)]
use {
    engine::{Engine, EngineConfig, OrderBook},
    marketdata::L2Publisher,
    risk::{IdMap, Limits, RiskEngine},
    types::{Event, OrderId, Price, RejectReason},
};

/// An exchange as a trader sees it: commands in, execution reports and
/// market data out.
pub trait Venue {
    /// Sends `cmd` on behalf of `account`. New orders are stamped with
    /// `account` (a session can only trade for its own account); cancels
    /// and reduces of another account's order are rejected with
    /// `UnknownOrder`.
    fn send(&mut self, account: AccountId, cmd: &Command);

    /// Appends everything the venue produced for the commands sent so far:
    /// one [`Delivery`] per (order, event), in the order the events
    /// happened, and the market-data updates. A network venue blocks until
    /// every sent command has been answered.
    fn flush(&mut self, deliveries: &mut Vec<Delivery>, market_data: &mut Vec<L2Update>);

    /// A full snapshot of the market-data feed, to start a book or to
    /// recover from a sequence gap.
    fn market_snapshot(&mut self) -> L2Snapshot;
}

/// The whole exchange in one process, single-threaded: pre-trade risk
/// ([`RiskEngine`]), the matching engine and the L2 publisher.
///
/// Per new order: duplicate-id check → `risk.check` → `risk.on_new` →
/// `engine.process`; every engine event then goes to `risk.on_event`, to
/// the publisher, and to the owners of the orders it concerns. Risk
/// rejections are delivered as `Event::Rejected` and never reach the
/// engine (its sequence number does not move).
///
/// Cost per command: the engine's, plus O(1) risk and owner lookups and
/// O(log levels) per market-data update; buffers are reused.
// @student w23
pub struct InProcess {
    engine: Engine,
    risk: RiskEngine,
    publisher: L2Publisher,
    /// Live orders → owning account.
    owners: IdMap<OrderId, AccountId>,
    reference: Price,
    events: Vec<Event>,
    deliveries: Vec<Delivery>,
    updates: Vec<L2Update>,
}

// @student w23
impl InProcess {
    /// A fresh exchange for `cfg.spec`: an empty book, the default
    /// pre-trade limits, and `cfg.reference_price` as the risk (and price
    /// band) reference until the first trade.
    pub fn new(cfg: &ScenarioConfig) -> InProcess {
        let engine_cfg = EngineConfig {
            expected_orders: cfg.expected_orders,
            ..EngineConfig::new(cfg.spec).with_reference_price(cfg.reference_price)
        };
        InProcess {
            engine: Engine::with_config(engine_cfg),
            risk: RiskEngine::with_capacity(cfg.default_limits, 64, cfg.expected_orders),
            publisher: L2Publisher::new(),
            owners: IdMap::with_capacity_and_hasher(cfg.expected_orders, Default::default()),
            reference: cfg.reference_price,
            events: Vec::with_capacity(64),
            deliveries: Vec::with_capacity(64),
            updates: Vec::with_capacity(64),
        }
    }

    /// Sets `account`'s pre-trade limits.
    pub fn set_limits(&mut self, account: AccountId, limits: Limits) {
        self.risk.set_limits(account, limits);
    }

    /// The matching engine (read-only).
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// The pre-trade risk engine (read-only).
    pub fn risk(&self) -> &RiskEngine {
        &self.risk
    }

    /// The pre-trade risk engine, e.g. to engage a kill switch.
    pub fn risk_mut(&mut self) -> &mut RiskEngine {
        &mut self.risk
    }

    /// The market-data publisher (read-only).
    pub fn publisher(&self) -> &L2Publisher {
        &self.publisher
    }

    /// Reference price for risk checks: the last trade, else the
    /// configured reference. O(1).
    pub fn reference_price(&self) -> Price {
        self.engine.last_trade_price().unwrap_or(self.reference)
    }
}

// @student-begin w23
impl InProcess {
    fn reject(&mut self, account: AccountId, id: OrderId, reason: RejectReason) {
        let event = Event::Rejected { id, reason };
        self.deliveries.push(Delivery {
            account,
            order: id,
            event,
        });
    }

    fn deliver(&mut self, order: OrderId, event: Event) {
        if let Some(&account) = self.owners.get(&order) {
            self.deliveries.push(Delivery {
                account,
                order,
                event,
            });
        }
    }

    /// Runs `cmd` through the engine and fans its events out.
    fn execute(&mut self, cmd: &Command) {
        self.events.clear();
        self.engine.process(cmd, &mut self.events);
        for i in 0..self.events.len() {
            let event = self.events[i];
            self.risk.on_event(&event);
            if let Some(u) = self.publisher.on_event(&event) {
                self.updates.push(u);
            }
            match event {
                Event::Trade { taker, maker, .. } => {
                    self.deliver(taker, event);
                    self.deliver(maker, event);
                    if !self.engine.book().contains(maker) {
                        self.owners.remove(&maker);
                    }
                }
                Event::Cancelled { id, .. } | Event::Expired { id, .. } => {
                    self.deliver(id, event);
                    self.owners.remove(&id);
                }
                _ => self.deliver(event.order_id(), event),
            }
        }
        // A new order that is not resting now is done (filled, expired or
        // rejected by the engine).
        if let Command::New(o) = cmd {
            if !self.engine.book().contains(o.id) {
                self.owners.remove(&o.id);
            }
        }
    }
}
// @student-end

// @student w23
impl Venue for InProcess {
    fn send(&mut self, account: AccountId, cmd: &Command) {
        match *cmd {
            Command::New(mut order) => {
                order.account = account;
                if self.owners.contains_key(&order.id) {
                    return self.reject(account, order.id, RejectReason::DuplicateOrderId);
                }
                if let Err(reason) = self.risk.check(&order, self.reference_price()) {
                    return self.reject(account, order.id, reason);
                }
                self.risk.on_new(&order);
                self.owners.insert(order.id, account);
                self.execute(&Command::New(order));
            }
            Command::Cancel { id } | Command::Reduce { id, .. } => {
                if self.owners.get(&id) != Some(&account) {
                    return self.reject(account, id, RejectReason::UnknownOrder);
                }
                self.execute(cmd);
            }
        }
    }

    fn flush(&mut self, deliveries: &mut Vec<Delivery>, market_data: &mut Vec<L2Update>) {
        deliveries.append(&mut self.deliveries);
        market_data.append(&mut self.updates);
    }

    fn market_snapshot(&mut self) -> L2Snapshot {
        self.publisher.snapshot()
    }
}
