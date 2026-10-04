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

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
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
pub struct InProcess {
    // TODO(w23): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl InProcess {
    /// A fresh exchange for `cfg.spec`: an empty book, the default
    /// pre-trade limits, and `cfg.reference_price` as the risk (and price
    /// band) reference until the first trade.
    pub fn new(cfg: &ScenarioConfig) -> InProcess {
        todo!("w23: new")
    }

    /// Sets `account`'s pre-trade limits.
    pub fn set_limits(&mut self, account: AccountId, limits: Limits) {
        todo!("w23: set_limits")
    }

    /// The matching engine (read-only).
    pub fn engine(&self) -> &Engine {
        todo!("w23: engine")
    }

    /// The pre-trade risk engine (read-only).
    pub fn risk(&self) -> &RiskEngine {
        todo!("w23: risk")
    }

    /// The pre-trade risk engine, e.g. to engage a kill switch.
    pub fn risk_mut(&mut self) -> &mut RiskEngine {
        todo!("w23: risk_mut")
    }

    /// The market-data publisher (read-only).
    pub fn publisher(&self) -> &L2Publisher {
        todo!("w23: publisher")
    }

    /// Reference price for risk checks: the last trade, else the
    /// configured reference. O(1).
    pub fn reference_price(&self) -> Price {
        todo!("w23: reference_price")
    }
}

// TODO(w23): implement — see the week guide on tepy.dev/hft.

#[allow(unused_variables)]
impl Venue for InProcess {
    fn send(&mut self, account: AccountId, cmd: &Command) {
        todo!("w23: send")
    }

    fn flush(&mut self, deliveries: &mut Vec<Delivery>, market_data: &mut Vec<L2Update>) {
        todo!("w23: flush")
    }

    fn market_snapshot(&mut self) -> L2Snapshot {
        todo!("w23: market_snapshot")
    }
}
