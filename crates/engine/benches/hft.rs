//! Benchmarks for the `engine` crate. Run with `cargo xtask bench --week N`.
//!
//! The w07 / w08 benchmarks build their order flow here, from `types::Rng`
//! and the engine's public API only, so they run as soon as week 7 is done
//! (the w12 generator is not needed).

use benchkit::{black_box, Report, Unit};
use engine::testkit::{FlowConfig, OrderFlow};
use engine::{Engine, OrderBook};
use std::collections::HashMap;
use types::{Command, Event, InstrumentSpec, NewOrder, OrderId, Price, Rng, Side, TimeInForce};

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
    let mut rng = Rng::new(seed);
    let mut engine = Engine::new(InstrumentSpec::default());
    let mut live = Live::default();
    let mut out = Vec::new();
    let mut next_id = 1u64;
    let mut new_id = || {
        next_id += 1;
        next_id - 1
    };
    for side in [Side::Buy, Side::Sell] {
        for k in 1..=50i64 {
            let price = if side == Side::Buy { MID - k } else { MID + k };
            for _ in 0..rng.range(5, 15) {
                let cmd = Command::New(NewOrder::limit(new_id(), side, price, rng.range(1, 10)));
                out.clear();
                engine.process(&cmd, &mut out);
                live.observe(&out, engine.book());
            }
        }
    }
    let base = engine.clone();

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
    (base, cmds)
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
    ]);
}
