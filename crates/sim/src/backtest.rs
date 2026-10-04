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
// @student w44
pub struct Backtester {
    cfg: BacktestConfig,
    engine: Engine,
    publisher: L2Publisher,
    book: BookBuilder,
    rng: Rng,
    outbox: Outbox,
    inventory: Inventory,
    in_flight: VecDeque<Timed>,
    events: Vec<Event>,
    now: u64,
    mark: Option<f64>,
    peak: f64,
    max_drawdown: f64,
    replayed: u64,
    sent: u64,
}

// @student w44
impl Backtester {
    /// A backtester with `cfg`.
    pub fn new(cfg: BacktestConfig) -> Backtester {
        Backtester {
            cfg,
            engine: Engine::new(InstrumentSpec::default()),
            publisher: L2Publisher::new(),
            book: BookBuilder::new(),
            rng: Rng::new(cfg.seed),
            outbox: Outbox::new(cfg.account, InstrumentSpec::default().id, cfg.first_id),
            inventory: Inventory::default(),
            in_flight: VecDeque::new(),
            events: Vec::with_capacity(64),
            now: 0,
            mark: None,
            peak: 0.0,
            max_drawdown: 0.0,
            replayed: 0,
            sent: 0,
        }
    }

    /// Its configuration.
    pub fn config(&self) -> &BacktestConfig {
        &self.cfg
    }

    /// The engine after the last run.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// The strategy's market-data view after the last run.
    pub fn book(&self) -> &BookBuilder {
        &self.book
    }

    /// Replays `rec` with `strategy` trading, and reports. O(recorded +
    /// strategy commands).
    pub fn run<S: Agent + ?Sized>(&mut self, rec: &Recording, strategy: &mut S) -> BacktestReport {
        self.reset(rec.spec());
        let entries = rec.entries();
        let mut next = 0;
        loop {
            let own_first = match (entries.get(next), self.in_flight.front()) {
                (None, None) => break,
                (Some(r), Some(o)) => o.at < r.at,
                (None, Some(_)) => true,
                (Some(_), None) => false,
            };
            if own_first {
                let Some(o) = self.in_flight.pop_front() else {
                    break;
                };
                self.now = o.at;
                self.sent += 1;
                self.execute(&o.cmd, strategy);
            } else {
                let r = entries[next];
                next += 1;
                self.now = r.at;
                self.replayed += 1;
                self.execute(&r.cmd, strategy);
                self.act(strategy);
            }
        }
        let inv = self.inventory;
        BacktestReport {
            events_replayed: self.replayed,
            orders_sent: self.sent,
            fills: inv.fills,
            filled_qty: inv.volume,
            position: inv.position,
            cash: inv.cash,
            turnover: inv.turnover,
            final_mark: self.mark,
            pnl: self.equity(),
            max_drawdown: self.max_drawdown,
            end_time: self.now,
        }
    }
}

// @student-begin w44
impl Backtester {
    fn reset(&mut self, spec: InstrumentSpec) {
        let engine_cfg = EngineConfig {
            expected_orders: self.cfg.expected_orders,
            ..EngineConfig::new(spec)
        };
        self.engine = Engine::with_config(engine_cfg);
        self.publisher = L2Publisher::new();
        self.book = BookBuilder::new();
        self.book.apply_snapshot(&L2Snapshot::default());
        self.rng = Rng::new(self.cfg.seed);
        self.outbox = Outbox::new(self.cfg.account, spec.id, self.cfg.first_id);
        self.inventory = Inventory::default();
        self.in_flight.clear();
        self.now = 0;
        self.mark = None;
        self.peak = 0.0;
        self.max_drawdown = 0.0;
        self.replayed = 0;
        self.sent = 0;
    }

    /// Runs one command through the engine, publishes its market data,
    /// delivers the strategy's events and updates the equity curve.
    fn execute<S: Agent + ?Sized>(&mut self, cmd: &Command, strategy: &mut S) {
        self.events.clear();
        self.engine.process(cmd, &mut self.events);
        for i in 0..self.events.len() {
            let event = self.events[i];
            if let Some(u) = self.publisher.on_event(&event) {
                self.book.apply_update(&u);
            }
            match event {
                Event::Trade { taker, maker, .. } => {
                    self.deliver(taker, event, strategy);
                    self.deliver(maker, event, strategy);
                }
                _ => self.deliver(event.order_id(), event, strategy),
            }
        }
        if let (Some(b), Some(a)) = (self.book.best_bid(), self.book.best_ask()) {
            self.mark = Some((b.price.0 + a.price.0) as f64 / 2.0);
        } else if self.mark.is_none() {
            self.mark = self.engine.last_trade_price().map(|p| p.0 as f64);
        }
        let equity = self.equity();
        self.peak = self.peak.max(equity);
        self.max_drawdown = self.max_drawdown.max(self.peak - equity);
    }

    fn deliver<S: Agent + ?Sized>(&mut self, order: OrderId, event: Event, strategy: &mut S) {
        if order.0 < self.cfg.first_id {
            return;
        }
        let d = Delivery {
            account: self.cfg.account,
            order,
            event,
        };
        if let Some(leg) = d.fill() {
            self.inventory.apply(leg.side, leg.price, leg.qty);
        }
        strategy.on_event(&d);
    }

    fn equity(&self) -> f64 {
        match self.mark {
            Some(m) => self.inventory.pnl_at(m),
            None => self.inventory.cash as f64,
        }
    }

    /// The strategy's turn at the current time; its commands go in flight.
    fn act<S: Agent + ?Sized>(&mut self, strategy: &mut S) {
        let mut ctx = AgentCtx {
            now: self.now,
            account: self.cfg.account,
            book: &self.book,
            inventory: &self.inventory,
            rng: &mut self.rng,
            out: &mut self.outbox,
        };
        strategy.act(&mut ctx);
        let at = self.now.saturating_add(self.cfg.latency_ns);
        for cmd in self.outbox.drain() {
            self.in_flight.push_back(Timed { at, cmd });
        }
    }
}
// @student-end
