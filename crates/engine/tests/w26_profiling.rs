//! Week 26: profiling — the properties the hotspot fixes rely on.
//!
//! These tests do not care *how* the engine got faster, only that the
//! fixes kept it correct and removed the costs a profile points at:
//! allocation in steady state, level operations that scale with book depth,
//! and work that changes the output.

mod common;

use common::{allocations_in, digest_events, mixed_stream};
use engine::testkit::{differential, NaiveEngine};
use engine::{Engine, EngineConfig, OrderBook, StateHasher};
use std::time::Instant;
use types::{
    Command, Event, InstrumentSpec, NewOrder, OrderId, Price, Qty, RejectReason, Side, TimeInForce,
};

fn config() -> EngineConfig {
    EngineConfig {
        expected_orders: 4_096,
        ..EngineConfig::new(InstrumentSpec::default())
    }
}

fn run(e: &mut Engine, cmd: Command) -> Vec<Event> {
    let mut out = Vec::new();
    e.process(&cmd, &mut out);
    out
}

#[test]
fn w26_steady_state_flow_does_not_allocate() {
    let cmds = mixed_stream(0x2626, 300_000);
    let mut e = Engine::with_config(config());
    let mut out = Vec::with_capacity(256);
    let (warm, measured) = cmds.split_at(100_000);
    for cmd in warm {
        out.clear();
        e.process(cmd, &mut out);
    }
    let allocs = allocations_in(|| {
        for cmd in measured {
            out.clear();
            e.process(cmd, &mut out);
        }
    });
    assert_eq!(
        allocs, 0,
        "{allocs} allocations in 200k steady-state commands (levels opening and \
         closing must reuse memory, e.g. no B-tree node per level)"
    );
    assert!(e.book().len() > 100, "the stream keeps a real book");
}

#[test]
fn w26_level_churn_across_a_wide_band_does_not_allocate() {
    // ~1000 passive orders spread over ±400 ticks, cancelled at random:
    // levels open and close all over the book. After a warm-up that has
    // grown the book to its working size, nothing may allocate.
    let mut e = Engine::with_config(config());
    let mut rng = types::Rng::new(0x26C);
    let mut live: Vec<OrderId> = Vec::with_capacity(4_096);
    let mut out = Vec::with_capacity(64);
    let mut next = 1u64;
    let mut step = |e: &mut Engine, live: &mut Vec<OrderId>, out: &mut Vec<Event>| {
        out.clear();
        let p_add = if live.len() < 1_000 { 0.7 } else { 0.3 };
        if rng.chance(p_add) || live.is_empty() {
            let side = rng.side();
            let d = rng.range_i64(1, 400);
            let price = if side == Side::Buy {
                50_000 - d
            } else {
                50_000 + d
            };
            e.process(
                &Command::New(NewOrder::limit(next, side, price, rng.range(1, 5))),
                out,
            );
            live.push(OrderId(next));
            next += 1;
        } else {
            let at = rng.range(0, live.len() as u64 - 1) as usize;
            e.process(
                &Command::Cancel {
                    id: live.swap_remove(at),
                },
                out,
            );
        }
    };
    for _ in 0..60_000 {
        step(&mut e, &mut live, &mut out);
    }
    let allocs = allocations_in(|| {
        for _ in 0..200_000 {
            step(&mut e, &mut live, &mut out);
        }
    });
    assert_eq!(allocs, 0, "opening/closing price levels allocated");
    assert!(e.book().level_count(Side::Buy) > 200);
}

#[test]
fn w26_events_match_the_recorded_reference() {
    // Digest of every event (field by field) and the final state hash of
    // the reference engine on this stream, recorded before the w26 fixes.
    // Any engine that follows the event contract produces exactly these.
    const EVENTS: u64 = 0xed28_e421_eef2_0b89;
    const STATE: u64 = 0x8aae_e51f_df6b_0171;
    let cmds = mixed_stream(0x2626, 300_000);
    let mut e = Engine::with_config(config());
    let mut out = Vec::new();
    let mut h = StateHasher::new();
    for cmd in &cmds {
        out.clear();
        e.process(cmd, &mut out);
        digest_events(&mut h, &out);
    }
    assert_eq!(h.finish(), EVENTS, "events differ from the pre-fix engine");
    assert_eq!(e.state_hash(), STATE, "final state differs");
    assert_eq!(e.seq().0, cmds.len() as u64);

    // And command by command against the naive reference matcher.
    let short = mixed_stream(0x2627, 20_000);
    let mut a = Engine::with_config(config());
    let mut b = NaiveEngine::new(config());
    if let Err(m) = differential(&mut a, &mut b, &short) {
        panic!("engine diverges from the naive matcher:\n{m}");
    }
}

#[test]
fn w26_duplicate_ids_are_still_caught_on_every_path() {
    // A fast path for passive orders must not skip the duplicate check, nor
    // move it before or after the wrong validation step.
    let mut e = Engine::with_config(config());
    run(&mut e, Command::New(NewOrder::limit(1, Side::Buy, 100, 5)));
    run(&mut e, Command::New(NewOrder::limit(2, Side::Sell, 105, 5)));
    let dup = |reason| {
        vec![Event::Rejected {
            id: OrderId(1),
            reason,
        }]
    };
    let cases = [
        // passive GTC limit, same side and price / other side
        (
            NewOrder::limit(1, Side::Buy, 100, 3),
            RejectReason::DuplicateOrderId,
        ),
        (
            NewOrder::limit(1, Side::Sell, 110, 3),
            RejectReason::DuplicateOrderId,
        ),
        // passive post-only, crossing limit, crossing post-only, IOC, FOK, market
        (
            NewOrder::limit(1, Side::Buy, 99, 3).post_only(),
            RejectReason::DuplicateOrderId,
        ),
        (
            NewOrder::limit(1, Side::Buy, 105, 3),
            RejectReason::DuplicateOrderId,
        ),
        (
            NewOrder::limit(1, Side::Buy, 106, 3).post_only(),
            RejectReason::DuplicateOrderId,
        ),
        (
            NewOrder::limit(1, Side::Buy, 101, 3).with_tif(TimeInForce::Ioc),
            RejectReason::DuplicateOrderId,
        ),
        (
            NewOrder::limit(1, Side::Buy, 200, 50).with_tif(TimeInForce::Fok),
            RejectReason::DuplicateOrderId,
        ),
        (
            NewOrder::market(1, Side::Buy, 2),
            RejectReason::DuplicateOrderId,
        ),
        // static checks still come first
        (
            NewOrder::limit(1, Side::Buy, 100, 0),
            RejectReason::ZeroQuantity,
        ),
    ];
    for (order, reason) in cases {
        let before = e.state_hash();
        assert_eq!(run(&mut e, Command::New(order)), dup(reason), "{order:?}");
        let after = e.state_hash();
        assert_ne!(before, after, "seq advances even for rejects");
        assert_eq!(e.book().len(), 2, "book unchanged by {order:?}");
        assert_eq!(e.book().get(OrderId(1)).unwrap().qty, Qty(5));
    }
    // Once order 1 is gone its id may rest again, on the fast path.
    run(&mut e, Command::Cancel { id: OrderId(1) });
    assert_eq!(
        run(&mut e, Command::New(NewOrder::limit(1, Side::Buy, 101, 4))),
        vec![
            Event::Accepted { id: OrderId(1) },
            Event::Rested {
                id: OrderId(1),
                side: Side::Buy,
                price: Price(101),
                qty: Qty(4)
            }
        ]
    );
}

#[test]
fn w26_touch_operations_do_not_scale_with_book_depth() {
    // Opening and closing a level at the touch must cost about the same on
    // a book 10 levels deep and on one 20_000 levels deep (a vector sorted
    // best-first, or a linear level search, makes this O(levels)).
    fn time_touch_churn(depth: i64) -> f64 {
        let mut e = Engine::with_config(EngineConfig {
            expected_orders: 50_000,
            ..EngineConfig::new(InstrumentSpec::default())
        });
        let mut out = Vec::with_capacity(16);
        let mut id = 0;
        for k in 0..depth {
            for (side, price) in [(Side::Buy, 100_000 - 2 - k), (Side::Sell, 100_002 + k)] {
                id += 1;
                e.process(&Command::New(NewOrder::limit(id, side, price, 1)), &mut out);
            }
        }
        const N: u64 = 20_000;
        let start = Instant::now();
        for i in 0..N {
            // A new best bid and best ask (new levels), then cancel both.
            let (b, a) = (1_000_000 + 2 * i, 1_000_001 + 2 * i);
            for cmd in [
                Command::New(NewOrder::limit(b, Side::Buy, 100_000 - 1, 1)),
                Command::New(NewOrder::limit(a, Side::Sell, 100_001, 1)),
                Command::Cancel { id: OrderId(b) },
                Command::Cancel { id: OrderId(a) },
            ] {
                out.clear();
                e.process(&cmd, &mut out);
            }
        }
        assert_eq!(e.book().level_count(Side::Buy), depth as usize);
        start.elapsed().as_secs_f64() / N as f64
    }
    // Best of three runs each, to shrug off scheduler noise.
    let best = |depth| {
        (0..3)
            .map(|_| time_touch_churn(depth))
            .fold(f64::MAX, f64::min)
    };
    let shallow = best(10);
    let deep = best(20_000);
    assert!(
        deep < shallow * 10.0 + 2e-6,
        "touch churn: {:.0}ns/iter at depth 20k vs {:.0}ns at depth 10",
        deep * 1e9,
        shallow * 1e9
    );
}

#[test]
fn w26_throughput_floor() {
    // A sanity floor 10× below the w26 benchmark target, in the xtest
    // profile: catches an accidental O(n) per command, not tuning.
    let cmds = mixed_stream(0x2628, 400_000);
    let mut best = f64::MAX;
    for _ in 0..3 {
        let mut e = Engine::with_config(config());
        let mut out = Vec::with_capacity(256);
        let start = Instant::now();
        for cmd in &cmds {
            out.clear();
            e.process(cmd, &mut out);
        }
        best = best.min(start.elapsed().as_secs_f64());
    }
    let rate = cmds.len() as f64 / best;
    assert!(rate > 1.0e6, "{:.2}M commands/s", rate / 1e6);
}
