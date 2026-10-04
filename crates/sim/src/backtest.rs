//! Week 44 — an event-driven backtester that replays a recorded market
//! through the real matching engine, with order latency.
//!
//! # What is recorded: the command log
//!
//! A [`Recording`] is the exchange's sequenced **command log** (new orders,
//! cancels, reduces of every participant) with the simulated time each
//! command arrived, in nanoseconds — the journal of week 9 plus a
//! timestamp. Recording commands rather than L2 updates is what makes the
//! backtest honest about execution:
//!
//! * replaying commands rebuilds the full **L3** book in the real engine,
//!   so a strategy order queues behind the orders that were really there,
//!   trades against them at their prices, and changes what later recorded
//!   orders fill (market impact) — an L2 replay can only guess at fills;
//! * the market data the strategy sees is produced live from that engine
//!   (L2 publisher → book builder), exactly as in production, so it can
//!   only react to what had happened by then: no look-ahead.
//!
//! The price of realism: once the strategy has traded, the replayed
//! history diverges from the real one (a recorded cancel may hit an order
//! the strategy already filled — the engine just rejects it).
//!
//! # Time
//!
//! Simulated time jumps from event to event. The strategy gets a turn
//! ([`Agent::act`]) after every recorded command — every market event —
//! at that command's time; what it sends reaches the engine
//! [`latency_ns`](BacktestConfig::latency_ns) later. The results of its
//! own orders (acks, fills, cancels) are delivered the moment the engine
//! produces them ([`Agent::on_event`]), and it reacts at its next turn.
//! Turns are not given after its own orders arrive: a strategy that sends
//! something on every turn would otherwise trigger itself forever, so the
//! number of turns is exactly the number of recorded commands.
//!
//! At equal times a recorded command goes first (a strategy order never
//! jumps ahead of the market at the same nanosecond). With a constant
//! latency, orders in flight stay in FIFO order, so a queue suffices; a
//! jittered latency model would need a priority queue ordered by
//! (arrival, send order). After the last recorded command the orders still
//! in flight arrive and are processed.
//!
//! # Accounting
//!
//! Strategy fills update an [`Inventory`]. After every processed command
//! the strategy is marked at the mid (the last two-sided mid, else the
//! last trade price); equity is `cash + position × mark`, and the report
//! keeps its final value and the largest peak-to-trough fall (max
//! drawdown, from a starting equity of 0).

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::flow::{FlowConfig, OrderFlow};
use crate::scenario::Agent;
use engine::{Engine, EngineConfig};
use marketdata::BookBuilder;
use types::{AccountId, Command, InstrumentSpec, Rng};
// Used only by the implementation (unused in the learner template).
#[allow(unused_imports)]
use {
    crate::scenario::{AgentCtx, Delivery, Inventory, Outbox},
    marketdata::{L2Publisher, L2Snapshot},
    std::collections::VecDeque,
    types::{Event, OrderId},
};

/// One recorded command and the simulated time it reached the exchange.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Timed {
    /// Arrival time, nanoseconds.
    pub at: u64,
    /// The command, as sequenced.
    pub cmd: Command,
}

/// A recorded market: the commands one instrument's engine sequenced, in
/// order, with non-decreasing arrival times. See the module docs for why
/// commands and not market data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recording {
    spec: InstrumentSpec,
    entries: Vec<Timed>,
}

impl Recording {
    /// An empty recording of instrument `spec`.
    pub fn new(spec: InstrumentSpec) -> Recording {
        Recording {
            spec,
            entries: Vec::new(),
        }
    }

    /// The instrument recorded.
    pub fn spec(&self) -> InstrumentSpec {
        self.spec
    }

    /// Appends `cmd` arriving at `at`. Amortized O(1).
    ///
    /// # Panics
    /// If `at` is earlier than the previous entry.
    pub fn push(&mut self, at: u64, cmd: Command) {
        if let Some(last) = self.entries.last() {
            assert!(at >= last.at, "recording times must not go backwards");
        }
        self.entries.push(Timed { at, cmd });
    }

    /// The recorded commands, oldest first.
    pub fn entries(&self) -> &[Timed] {
        &self.entries
    }

    /// Number of recorded commands.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if nothing was recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// A synthetic recording: `commands` commands of [`OrderFlow`] (run
    /// against a scratch engine so its cancels target live orders), with
    /// exponentially distributed gaps of mean `mean_gap_ns` (a Poisson
    /// arrival process; every gap is at least 1 ns). Deterministic for a
    /// seed. O(commands).
    pub fn synthetic(cfg: FlowConfig, commands: usize, mean_gap_ns: u64, seed: u64) -> Recording {
        let mut flow = OrderFlow::new(cfg, seed);
        let mut engine = Engine::with_config(EngineConfig::new(cfg.spec));
        let mut rng = Rng::new(seed ^ 0x5DEE_CE66_D1CE_4E5B);
        let mut rec = Recording {
            spec: cfg.spec,
            entries: Vec::with_capacity(commands),
        };
        let mut events = Vec::with_capacity(64);
        let mut t = 0u64;
        for _ in 0..commands {
            let u = ((rng.next_u64() >> 11) as f64 + 1.0) / (1u64 << 53) as f64;
            t += ((-u.ln() * mean_gap_ns as f64) as u64).max(1);
            let cmd = flow.next_command();
            events.clear();
            engine.process(&cmd, &mut events);
            for e in &events {
                flow.on_event(e);
            }
            rec.entries.push(Timed { at: t, cmd });
        }
        rec
    }
}

/// How a backtest runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BacktestConfig {
    /// A strategy command reaches the engine this many nanoseconds after
    /// the decision that sent it.
    pub latency_ns: u64,
    /// The strategy's account.
    pub account: AccountId,
    /// The strategy's first order id. Its ids must not collide with the
    /// recording's: every id `>= first_id` is the strategy's.
    pub first_id: u64,
    /// Seeds the strategy's random number generator.
    pub seed: u64,
    /// Capacity hint: resting orders to pre-allocate room for.
    pub expected_orders: usize,
}

impl Default for BacktestConfig {
    fn default() -> Self {
        BacktestConfig {
            latency_ns: 0,
            account: AccountId(9_999),
            first_id: 1 << 56,
            seed: 1,
            expected_orders: 4_096,
        }
    }
}

/// The summary of one backtest.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BacktestReport {
    /// Recorded commands replayed.
    pub events_replayed: u64,
    /// Strategy commands that reached the engine.
    pub orders_sent: u64,
    /// Strategy fills (trade legs).
    pub fills: u64,
    /// Total quantity filled, both sides.
    pub filled_qty: u64,
    /// Final position, lots.
    pub position: i64,
    /// Final cash: + sells, − buys, in price units × lots.
    pub cash: i128,
    /// Traded notional `Σ |price| × qty`.
    pub turnover: u128,
    /// The mark used for the final P&L (`None`: never marked).
    pub final_mark: Option<f64>,
    /// `cash + position × final_mark` (just `cash` if never marked).
    pub pnl: f64,
    /// Largest fall of equity from a previous peak (≥ 0).
    pub max_drawdown: f64,
    /// Simulated time of the last processed command, ns.
    pub end_time: u64,
}

/// Replays a [`Recording`] through a fresh engine with a strategy trading
/// in it; see the module docs. A backtester can run many recordings; each
/// [`run`](Backtester::run) starts from an empty book.
///
/// Cost per replayed command: the engine's, plus O(log levels) per
/// market-data update, the strategy's `act` and O(1) accounting.
pub struct Backtester {
    // TODO(w44): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl Backtester {
    /// A backtester with `cfg`.
    pub fn new(cfg: BacktestConfig) -> Backtester {
        todo!("w44: new")
    }

    /// Its configuration.
    pub fn config(&self) -> &BacktestConfig {
        todo!("w44: config")
    }

    /// The engine after the last run.
    pub fn engine(&self) -> &Engine {
        todo!("w44: engine")
    }

    /// The strategy's market-data view after the last run.
    pub fn book(&self) -> &BookBuilder {
        todo!("w44: book")
    }

    /// Replays `rec` with `strategy` trading, and reports. O(recorded +
    /// strategy commands).
    pub fn run<S: Agent + ?Sized>(&mut self, rec: &Recording, strategy: &mut S) -> BacktestReport {
        todo!("w44: run")
    }
}

// TODO(w44): implement — see the week guide on tepy.dev/hft.
