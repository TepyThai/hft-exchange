//! Benchmarks for the `sim` crate. Run with `cargo xtask bench --week N`.

use benchkit::{black_box, Histogram, Report, Unit};
use engine::{Engine, EngineConfig};
use marketdata::{BookBuilder, L2Publisher, L2Snapshot, L2Update};
use sim::backtest::{BacktestConfig, Backtester, Recording};
use sim::flow::{FlowConfig, OrderFlow};
use sim::mm::{MarketMaker, MmConfig};
use sim::scenario::{
    Agent, AgentCtx, Delivery, MakerAgent, MakerConfig, MomentumAgent, MomentumConfig, NoiseAgent,
    NoiseConfig, Runner, ScenarioConfig,
};
use std::time::Instant;
use types::{AccountId, Command, Event, InstrumentId, OrderId, Price, Qty, Side};

fn main() {
    benchkit::run(&[
        (
            "w23.sim_throughput",
            "Simulated orders through the full pipeline",
            bench_sim_throughput,
        ),
        (
            "w42.strategy_decision",
            "Market data → quote decision",
            bench_strategy_decision,
        ),
        (
            "w44.replay_speed",
            "Backtest replay speed",
            bench_replay_speed,
        ),
    ]);
}

// ---------------------------------------------------------------------------
// w23

const SIM_STEPS: u64 = 5_000;

/// Three makers, six noise takers (market and IOC), two momentum takers —
/// about seven commands per step.
fn market(seed: u64) -> Runner {
    let mut r = Runner::new(ScenarioConfig {
        seed,
        expected_orders: 1_024,
        ..ScenarioConfig::default()
    });
    for (half_spread, qty) in [(1, 10), (2, 20), (3, 40)] {
        r.add_agent(Box::new(MakerAgent::new(MakerConfig {
            fair_value: Price(10_000),
            half_spread_ticks: half_spread,
            qty: Qty(qty),
            lots_per_tick: 5,
            max_position: 200,
            tick: 1,
        })));
    }
    for slippage in [None, None, Some(0), Some(1), Some(2), Some(3)] {
        r.add_agent(Box::new(NoiseAgent::new(NoiseConfig {
            p_trade: 0.6,
            max_qty: 8,
            slippage_ticks: slippage,
            ..NoiseConfig::default()
        })));
    }
    for lookback in [3, 8] {
        r.add_agent(Box::new(MomentumAgent::new(MomentumConfig {
            lookback,
            threshold_ticks: 1,
            ..MomentumConfig::default()
        })));
    }
    r
}

/// Commands per second through risk → engine → market data → agents,
/// building a fresh scenario per call (setup is ~1% of the call).
fn bench_sim_throughput() -> Report {
    let mut probe = market(1);
    probe.run(SIM_STEPS);
    let commands = probe.report().commands;
    benchkit::throughput(
        "w23.sim_throughput",
        "Simulated orders through the full pipeline",
        Unit::OpsPerSec,
        commands,
        || {
            let mut r = market(1);
            r.run(SIM_STEPS);
            black_box(r.now());
        },
    )
}

// ---------------------------------------------------------------------------
// w42

const DECISIONS: usize = 200_000;
const WARMUP: usize = 20_000;

/// The L2 feed of a realistic book: `n` updates from the synthetic flow.
fn l2_stream(n: usize, seed: u64) -> Vec<L2Update> {
    let cfg = FlowConfig::default();
    let mut flow = OrderFlow::new(cfg, seed);
    let mut engine = Engine::with_config(EngineConfig::new(cfg.spec));
    let mut publisher = L2Publisher::new();
    let (mut events, mut updates) = (Vec::new(), Vec::with_capacity(n));
    while updates.len() < n {
        let cmd = flow.next_command();
        events.clear();
        engine.process(&cmd, &mut events);
        for e in &events {
            flow.on_event(e);
            updates.extend(publisher.on_event(e));
        }
    }
    updates.truncate(n);
    updates
}

/// One market-data update applied to the strategy's book, then its quote
/// decision (`market_top` + `decide`), timed per update. Between updates,
/// untimed, the strategy's commands are acknowledged (accepted, cancelled,
/// reduced) as a venue would, so its own-order list stays at its steady
/// size; its quotes never rest on this replayed book, so the book is the
/// market's.
fn bench_strategy_decision() -> Report {
    let stream = l2_stream(WARMUP + DECISIONS, 7);
    let mut book = BookBuilder::new();
    book.apply_snapshot(&L2Snapshot::default());
    let mut mm = MarketMaker::new(MmConfig::default());
    let mut out = sim::scenario::Outbox::new(AccountId(1), InstrumentId(1), 1 << 40);
    let mut hist = Histogram::new();
    for (i, u) in stream.iter().enumerate() {
        let t = Instant::now();
        book.apply_update(u);
        let top = mm.market_top(&book);
        mm.decide(&top, &mut out);
        let ns = t.elapsed().as_nanos() as u64;
        if i >= WARMUP {
            hist.record(ns);
        }
        for cmd in out.drain() {
            let (order, event) = match cmd {
                Command::New(o) => (o.id, Event::Accepted { id: o.id }),
                Command::Cancel { id } => (
                    id,
                    Event::Cancelled {
                        id,
                        remaining: Qty(1),
                    },
                ),
                Command::Reduce { id, qty } => (id, Event::Reduced { id, remaining: qty }),
            };
            mm.on_delivery(&Delivery {
                account: AccountId(1),
                order,
                event,
            });
        }
    }
    black_box(mm.decisions());
    hist.report(
        "w42.strategy_decision",
        "Market data → quote decision",
        Unit::Ns,
    )
}

// ---------------------------------------------------------------------------
// w44

const REPLAY: usize = 200_000;

/// A light strategy: keeps one lot bid at the touch, refreshed every 64
/// turns, so the backtester's own costs dominate.
#[derive(Default)]
struct Joiner {
    turns: u64,
    resting: Option<OrderId>,
}

impl Agent for Joiner {
    fn name(&self) -> &str {
        "joiner"
    }

    fn act(&mut self, ctx: &mut AgentCtx<'_>) {
        self.turns += 1;
        if !self.turns.is_multiple_of(64) {
            return;
        }
        if let Some(id) = self.resting.take() {
            ctx.out.cancel(id);
        }
        if let Some(bid) = ctx.book.best_bid() {
            self.resting = Some(ctx.out.limit(Side::Buy, bid.price, Qty(1)));
        }
    }

    fn on_event(&mut self, d: &Delivery) {
        if matches!(d.event, Event::Cancelled { .. } | Event::Trade { .. })
            && self.resting == Some(d.order)
        {
            self.resting = None;
        }
    }
}

/// Recorded commands replayed per second: engine, L2 publisher, the
/// strategy's book builder, a strategy turn per command, accounting.
fn bench_replay_speed() -> Report {
    let rec = Recording::synthetic(FlowConfig::default(), REPLAY, 1_000, 11);
    let mut bt = Backtester::new(BacktestConfig {
        latency_ns: 5_000,
        ..BacktestConfig::default()
    });
    benchkit::throughput(
        "w44.replay_speed",
        "Backtest replay speed",
        Unit::MsgsPerSec,
        REPLAY as u64,
        || {
            let report = bt.run(&rec, &mut Joiner::default());
            black_box(report.events_replayed);
        },
    )
}
