//! Benchmarks for the `engine` crate. Run with `cargo xtask bench --week N`.
//!
//! The w07 / w08 benchmarks build their order flow here, from `types::Rng`
//! and the engine's public API only, so they run as soon as week 7 is done
//! (the w12 generator is not needed).

use benchkit::{black_box, Report, Unit};
use engine::perf::fastest_engine;
use engine::testkit::{FlowConfig, OrderFlow};
use engine::{
    Book, DynGuarded, Engine, EngineConfig, FastEngine, Guarded, InstrumentRegistry, MaxOrderQty,
    NoRisk, OrderBook, RiskPolicy, ShardedEngine,
};
use std::collections::HashMap;
use types::{
    Command, Event, InstrumentId, InstrumentSpec, NewOrder, OrderId, Price, Rng, Side, TimeInForce,
};

const MID: i64 = 100_000;

/// Resting orders the mixed flow keeps the book around.
const TARGET_RESTING: usize = 1_000;

/// Live order ids, with O(1) random pick and removal.
#[derive(Default)]
struct Live {
    ids: Vec<OrderId>,
    pos: HashMap<OrderId, usize>,
}

impl Live {
    fn insert(&mut self, id: OrderId) {
        if !self.pos.contains_key(&id) {
            self.pos.insert(id, self.ids.len());
            self.ids.push(id);
        }
    }

    fn remove(&mut self, id: OrderId) {
        if let Some(p) = self.pos.remove(&id) {
            self.ids.swap_remove(p);
            if let Some(&moved) = self.ids.get(p) {
                self.pos.insert(moved, p);
            }
        }
    }

    fn pick(&self, rng: &mut Rng) -> Option<OrderId> {
        (!self.ids.is_empty()).then(|| self.ids[rng.range(0, self.ids.len() as u64 - 1) as usize])
    }

    /// Applies one command's events (the bench's view of the book).
    fn observe(&mut self, events: &[Event], book: &impl OrderBook) {
        for e in events {
            match *e {
                Event::Rested { id, .. } => self.insert(id),
                Event::Cancelled { id, .. } => self.remove(id),
                Event::Trade { maker, .. } if !book.contains(maker) => self.remove(maker),
                _ => {}
            }
        }
    }
}

/// Levels away from the touch: 1 + roughly geometric, mostly near the top.
fn depth_ticks(rng: &mut Rng, max: u64) -> i64 {
    1 + rng
        .range(0, max)
        .min(rng.range(0, max))
        .min(rng.range(0, max)) as i64
}

/// A book around `MID` (≈1000 orders over 50 levels a side) and a stream of
/// `n` commands that keeps it there: of the new orders ~70% are passive
/// (priced behind the opposite touch) and ~30% aggressive (limits crossing
/// up to 2 ticks through the touch, IOC or GTC, and some market orders),
/// interleaved with cancels of live orders. The stream is generated against
/// a shadow engine, so replaying it on a clone of the returned engine hits
/// live orders exactly as generated.
fn mixed_flow(n: usize, seed: u64) -> (Engine, Vec<Command>) {
    let (setup, cmds) = mixed_flow_commands(n, seed);
    let mut base = Engine::new(InstrumentSpec::default());
    let mut out = Vec::new();
    for cmd in &setup {
        out.clear();
        base.process(cmd, &mut out);
    }
    (base, cmds)
}

/// [`mixed_flow`] as commands only: the `setup` commands that build the
/// starting book, then the `n`-command stream — to replay into any engine
/// type (or several instruments).
fn mixed_flow_commands(n: usize, seed: u64) -> (Vec<Command>, Vec<Command>) {
    let mut rng = Rng::new(seed);
    let mut engine = Engine::new(InstrumentSpec::default());
    let mut live = Live::default();
    let mut out = Vec::new();
    let mut next_id = 1u64;
    let mut new_id = || {
        next_id += 1;
        next_id - 1
    };
    let mut setup = Vec::new();
    for side in [Side::Buy, Side::Sell] {
        for k in 1..=50i64 {
            let price = if side == Side::Buy { MID - k } else { MID + k };
            for _ in 0..rng.range(5, 15) {
                let cmd = Command::New(NewOrder::limit(new_id(), side, price, rng.range(1, 10)));
                out.clear();
                engine.process(&cmd, &mut out);
                live.observe(&out, engine.book());
                setup.push(cmd);
            }
        }
    }

    let mut cmds = Vec::with_capacity(n);
    while cmds.len() < n {
        let book = engine.book();
        let r = rng.range(0, 99);
        let few = live.ids.len() < TARGET_RESTING * 8 / 10;
        let many = live.ids.len() > TARGET_RESTING * 12 / 10;
        let cmd = if (r < 30 && !few) || many {
            match live.pick(&mut rng) {
                Some(id) => Command::Cancel { id },
                None => continue,
            }
        } else if r < 51 && !few {
            // Aggressive (30% of new orders).
            let side = rng.side();
            let qty = rng.range(1, 25);
            match book.best(side.opposite()) {
                None => continue,
                Some(_) if rng.chance(0.15) => Command::New(NewOrder::market(new_id(), side, qty)),
                Some(touch) => {
                    let through = rng.range_i64(0, 2);
                    let price = if side == Side::Buy {
                        touch.price.0 + through
                    } else {
                        touch.price.0 - through
                    };
                    let tif = if rng.chance(0.5) {
                        TimeInForce::Ioc
                    } else {
                        TimeInForce::Gtc
                    };
                    Command::New(NewOrder::limit(new_id(), side, price, qty).with_tif(tif))
                }
            }
        } else {
            // Passive (70% of new orders): behind the opposite touch.
            let side = rng.side();
            let anchor = match (side, book.best(side.opposite())) {
                (_, Some(touch)) => touch.price.0,
                (Side::Buy, None) => MID + 1,
                (Side::Sell, None) => MID - 1,
            };
            let k = depth_ticks(&mut rng, 40);
            let price = if side == Side::Buy {
                anchor - k
            } else {
                anchor + k
            };
            Command::New(NewOrder::limit(new_id(), side, price, rng.range(1, 10)))
        };
        out.clear();
        engine.process(&cmd, &mut out);
        live.observe(&out, engine.book());
        cmds.push(cmd);
    }
    (setup, cmds)
}

/// Submit → events latency for one command of the mixed flow.
fn match_latency() -> Report {
    const ITER: u64 = 1_000_000;
    // `latency` warms up with ITER / 10 commands before measuring.
    let (base, cmds) = mixed_flow((ITER + ITER / 10) as usize, 0x0707);
    let mut engine = base.clone();
    let mut out = Vec::with_capacity(64);
    let mut next = cmds.iter();
    benchkit::latency(
        "w07.match_latency",
        "Submit→events, mixed flow (70% passive)",
        Unit::Ns,
        ITER,
        || *next.next().expect("stream long enough"),
        |cmd| {
            engine.process(&cmd, &mut out);
            black_box(&out);
            out.clear();
        },
    )
}

/// Commands per second over the whole mixed flow, single thread.
fn engine_throughput() -> Report {
    let (base, cmds) = mixed_flow(1_000_000, 0x0708);
    let mut out = Vec::with_capacity(64);
    benchkit::throughput(
        "w07.engine_throughput",
        "Orders per second, single thread",
        Unit::OpsPerSec,
        cmds.len() as u64,
        || {
            let mut engine = base.clone();
            for cmd in &cmds {
                engine.process(cmd, &mut out);
                out.clear();
            }
            black_box(engine.seq());
        },
    )
}

/// FOK feasibility on a deep book: 1000 levels a side, 1–5 orders each;
/// each check walks 1–100 levels and about half are feasible.
fn fok_check() -> Report {
    let mut rng = Rng::new(0x0808);
    let mut engine = Engine::new(InstrumentSpec::default());
    let mut out = Vec::new();
    let mut id = 0;
    for side in [Side::Buy, Side::Sell] {
        for k in 1..=1_000i64 {
            let price = if side == Side::Buy { MID - k } else { MID + k };
            for _ in 0..rng.range(1, 5) {
                id += 1;
                engine.process(
                    &Command::New(NewOrder::limit(id, side, price, rng.range(1, 10))),
                    &mut out,
                );
            }
        }
    }
    // Average level holds 3 orders × 5.5 lots.
    let per_level = 16;
    let engine = engine;
    benchkit::latency(
        "w08.fok_check",
        "FOK feasibility check, deep book",
        Unit::Ns,
        1_000_000,
        || {
            let side = rng.side();
            let levels = rng.range_i64(1, 100);
            let limit = if side == Side::Buy {
                MID + levels
            } else {
                MID - levels
            };
            let want = rng.range(1, levels as u64 * per_level * 2);
            (side, Price(limit), types::Qty(want))
        },
        |(side, limit, want)| {
            black_box(engine.fillable(side, Some(limit), want));
        },
    )
}

/// The w12 generator's default stream (adds, cancels, reduces, IOC / FOK /
/// post-only / market orders, malformed commands) through the engine alone.
/// The journal and market data stages of engine v1 are wired in later; this
/// measures the matching core on that flow.
fn engine_v1_throughput() -> Report {
    const N: usize = 1_000_000;
    let cfg = FlowConfig {
        seed: 0x1212,
        ..FlowConfig::default()
    };
    let mut flow = OrderFlow::new(cfg);
    let mut shadow = Engine::with_config(cfg.engine_config());
    let mut out = Vec::new();
    let mut cmds = Vec::with_capacity(N);
    for _ in 0..N {
        let cmd = flow.next_command();
        out.clear();
        shadow.process(&cmd, &mut out);
        flow.observe(&cmd, &out);
        cmds.push(cmd);
    }
    benchkit::throughput(
        "w12.engine_v1_throughput",
        "Engine v1 end to end (journal + match + md)",
        Unit::OpsPerSec,
        N as u64,
        || {
            let mut engine = Engine::with_config(cfg.engine_config());
            for cmd in &cmds {
                engine.process(cmd, &mut out);
                out.clear();
            }
            black_box(engine.seq());
        },
    )
}

// ---------------------------------------------------------------------------
// Performance weeks (26, 36, 37, 38, 40): the w07 mixed flow through each
// engine configuration.

/// Commands per second of engines made by `make`, each fed `setup` (the
/// starting book, ~0.1% of the commands, timed too) then `cmds`.
fn rate_of<E>(
    id: &str,
    label: &str,
    setup: &[Command],
    cmds: &[Command],
    mut make: impl FnMut() -> E,
    mut process: impl FnMut(&mut E, &Command, &mut Vec<Event>),
) -> Report {
    let mut out = Vec::with_capacity(64);
    benchkit::throughput(
        id,
        label,
        Unit::OpsPerSec,
        (setup.len() + cmds.len()) as u64,
        || {
            let mut engine = make();
            for cmd in setup.iter().chain(cmds) {
                process(&mut engine, cmd, &mut out);
                out.clear();
            }
            black_box(&engine);
        },
    )
}

/// Submit → events latency of one command of the mixed flow, for any
/// engine (the w07.match_latency method).
fn latency_of<E>(
    id: &str,
    label: &str,
    seed: u64,
    mut engine: E,
    mut process: impl FnMut(&mut E, &Command, &mut Vec<Event>),
) -> Report {
    const ITER: u64 = 1_000_000;
    let (setup, cmds) = mixed_flow_commands((ITER + ITER / 10) as usize, seed);
    let mut out = Vec::with_capacity(64);
    for cmd in &setup {
        process(&mut engine, cmd, &mut out);
        out.clear();
    }
    let mut next = cmds.iter();
    benchkit::latency(
        id,
        label,
        Unit::Ns,
        ITER,
        || *next.next().expect("stream long enough"),
        |cmd| {
            process(&mut engine, &cmd, &mut out);
            black_box(&out);
            out.clear();
        },
    )
}

/// w26: the w07 engine after the profile-guided fixes (sorted-vector price
/// levels, single-probe passive path), on the w07 flow.
fn w26_engine_throughput() -> Report {
    let (setup, cmds) = mixed_flow_commands(1_000_000, 0x2626);
    rate_of(
        "w26.engine_throughput",
        "Engine throughput after profiling fixes",
        &setup,
        &cmds,
        || Engine::new(InstrumentSpec::default()),
        |e, cmd, out| e.process(cmd, out),
    )
}

/// w36: 8 instruments over 4 shards; each command's instrument is uniform
/// at random (per-instrument w07 flows, interleaved). Router on this
/// thread, 4 engine threads, batches of 256, a flush every 64k commands.
fn w36_sharded_throughput() -> Report {
    const INSTRUMENTS: u32 = 8;
    const PER: usize = 125_000;
    let mut rng = Rng::new(0x3636);
    let mut setup = Vec::new();
    let mut queues: Vec<std::vec::IntoIter<Command>> = Vec::new();
    for i in 1..=INSTRUMENTS {
        let tag = |cmd: Command| -> Command {
            let remap = |id: OrderId| OrderId(id.0 * 16 + u64::from(i));
            match cmd {
                Command::New(mut o) => {
                    o.id = remap(o.id);
                    o.instrument = InstrumentId(i);
                    Command::New(o)
                }
                Command::Cancel { id } => Command::Cancel { id: remap(id) },
                Command::Reduce { id, qty } => Command::Reduce { id: remap(id), qty },
            }
        };
        let (s, c) = mixed_flow_commands(PER, 0x3600 + u64::from(i));
        setup.extend(s.into_iter().map(tag));
        queues.push(c.into_iter().map(tag).collect::<Vec<_>>().into_iter());
    }
    let mut cmds = setup;
    let mut remaining: usize = queues.iter().map(|q| q.len()).sum();
    while remaining > 0 {
        let q = rng.range(0, u64::from(INSTRUMENTS) - 1) as usize;
        if let Some(cmd) = queues[q].next() {
            cmds.push(cmd);
            remaining -= 1;
        }
    }
    let mut events = Vec::with_capacity(1 << 18);
    benchkit::throughput(
        "w36.sharded_throughput",
        "4 shards, uniform symbols",
        Unit::OpsPerSec,
        cmds.len() as u64,
        || {
            let mut registry = InstrumentRegistry::new(4);
            for i in 1..=INSTRUMENTS {
                let spec = InstrumentSpec {
                    id: InstrumentId(i),
                    ..InstrumentSpec::default()
                };
                registry.register(spec).expect("fresh registry");
            }
            let mut sharded = ShardedEngine::new(registry, 256);
            for (k, cmd) in cmds.iter().enumerate() {
                let _ = black_box(sharded.send(cmd));
                if k % 65_536 == 65_535 {
                    sharded.flush(&mut events);
                    events.clear();
                }
            }
            black_box(sharded.shutdown(&mut events).len());
            events.clear();
        },
    )
}

/// w37: the same flow through `Guarded<Book, MaxOrderQty<_>>` (static
/// dispatch, inlined) and `DynGuarded` (boxed book, boxed policy), as a
/// throughput ratio: the median of 5 alternating measurements, so a change
/// in machine load hits both sides alike.
fn w37_static_vs_dyn() -> Report {
    const CAP: u64 = 1_000_000;
    let (setup, cmds) = mixed_flow_commands(1_000_000, 0x3737);
    let spec = InstrumentSpec::default();
    let generic = || {
        rate_of(
            "",
            "",
            &setup,
            &cmds,
            || -> Guarded<Book, MaxOrderQty<CAP>> { Guarded::new(Engine::new(spec), MaxOrderQty) },
            |e, cmd, out| e.process(cmd, out),
        )
        .value
    };
    let dynamic = || {
        rate_of(
            "",
            "",
            &setup,
            &cmds,
            || -> DynGuarded {
                let book: Box<dyn OrderBook> = Box::new(Book::with_capacity(1_024, 256));
                let risk: Box<dyn RiskPolicy> = Box::new(MaxOrderQty::<CAP>);
                Guarded::new(Engine::with_book(EngineConfig::new(spec), book), risk)
            },
            |e, cmd, out| e.process(cmd, out),
        )
        .value
    };
    let mut ratios: Vec<f64> = (0..5).map(|_| generic() / dynamic()).collect();
    ratios.sort_by(f64::total_cmp);
    benchkit::ratio(
        "w37.static_vs_dyn",
        "Generic vs dyn engine",
        ratios[ratios.len() / 2],
        1.0,
    )
}

/// w38: the hot/cold-split engine (`FastEngine<Book, NoRisk>`).
fn w38_match_latency() -> Report {
    let engine: FastEngine<Book, NoRisk> = FastEngine::new(
        EngineConfig::new(InstrumentSpec::default()),
        Book::with_capacity(1_024, 256),
        NoRisk,
    );
    latency_of(
        "w38.match_latency",
        "Submit→events after hot/cold split",
        0x3838,
        engine,
        |e, cmd, out| e.process(cmd, out),
    )
}

/// w40: the fastest configuration ([`fastest_engine`]), throughput.
fn w40_engine_throughput() -> Report {
    let (setup, cmds) = mixed_flow_commands(1_000_000, 0x4040);
    rate_of(
        "w40.engine_throughput",
        "Final engine throughput",
        &setup,
        &cmds,
        || fastest_engine(EngineConfig::new(InstrumentSpec::default())),
        |e, cmd, out| e.process(cmd, out),
    )
}

/// w40: the fastest configuration, submit → events latency.
fn w40_match_latency() -> Report {
    latency_of(
        "w40.match_latency",
        "Final submit→events latency",
        0x4041,
        fastest_engine(EngineConfig::new(InstrumentSpec::default())),
        |e, cmd, out| e.process(cmd, out),
    )
}

fn main() {
    benchkit::run(&[
        (
            "w07.match_latency",
            "Submit→events, mixed flow (70% passive)",
            match_latency,
        ),
        (
            "w07.engine_throughput",
            "Orders per second, single thread",
            engine_throughput,
        ),
        (
            "w08.fok_check",
            "FOK feasibility check, deep book",
            fok_check,
        ),
        (
            "w12.engine_v1_throughput",
            "Engine v1 end to end (journal + match + md)",
            engine_v1_throughput,
        ),
        (
            "w26.engine_throughput",
            "Engine throughput after profiling fixes",
            w26_engine_throughput,
        ),
        (
            "w36.sharded_throughput",
            "4 shards, uniform symbols",
            w36_sharded_throughput,
        ),
        (
            "w37.static_vs_dyn",
            "Generic vs dyn engine",
            w37_static_vs_dyn,
        ),
        (
            "w38.match_latency",
            "Submit→events after hot/cold split",
            w38_match_latency,
        ),
        (
            "w40.engine_throughput",
            "Final engine throughput",
            w40_engine_throughput,
        ),
        (
            "w40.match_latency",
            "Final submit→events latency",
            w40_match_latency,
        ),
    ]);
}
