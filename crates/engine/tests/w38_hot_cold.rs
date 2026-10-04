//! Week 38: hot path / cold path — the split engine must behave exactly
//! like the engine before the split, on every path, common or rare.

mod common;

use common::{allocations_in, mixed_stream};
use engine::testkit::{differential, shrink, FlowConfig, NaiveEngine, OrderFlow};
use engine::{
    reject_slot, Book, Engine, EngineConfig, FastEngine, Guarded, LadderBook, MaxOrderQty, NoRisk,
    OrderBook, REJECT_KINDS,
};
use risk::{Limits, RiskEngine};
use types::{Command, Event, InstrumentSpec, Qty};

type Fast = FastEngine<Book, NoRisk>;

fn fast(cfg: EngineConfig) -> Fast {
    FastEngine::new(cfg, Book::new(), NoRisk)
}

fn diverges(cfg: &FlowConfig, cmds: &[Command]) -> Option<String> {
    let mut a = fast(cfg.engine_config());
    let mut b = Engine::with_config(cfg.engine_config());
    differential(&mut a, &mut b, cmds)
        .err()
        .map(|m| m.to_string())
}

#[test]
fn w38_matches_the_pre_split_engine_on_random_w12_streams() {
    for seed in 0..1_000 {
        let cfg = FlowConfig::randomized(0x3800 + seed);
        let stream = OrderFlow::new(cfg).take_commands(250);
        if let Some(why) = diverges(&cfg, &stream) {
            let minimal = shrink(&stream, |c| diverges(&cfg, c).is_some());
            panic!(
                "seed {seed}: split engine diverges\n{why}\nminimal: {minimal:#?}\n{}",
                diverges(&cfg, &minimal).unwrap_or_default()
            );
        }
    }
}

#[test]
fn w38_cold_heavy_streams_are_identical() {
    // Mostly rare paths: malformed orders, FOK, post-only, IOC, markets on
    // thin books, bad reduces and stale cancels — with tick 5 / lot 10 and
    // a price band so every spec check fires.
    let cfg = FlowConfig {
        seed: 0x38C,
        spec: InstrumentSpec {
            tick: 5,
            lot: 10,
            band_ticks: 12,
            ..InstrumentSpec::default()
        },
        levels: 6,
        max_lots: 6,
        target_resting: 60,
        drift: 0.1,
        p_cancel: 0.15,
        p_reduce: 0.1,
        p_market: 0.1,
        p_aggressive: 0.25,
        p_ioc: 0.3,
        p_fok: 0.35,
        p_post_only: 0.35,
        p_invalid: 0.25,
        ..FlowConfig::default()
    };
    let stream = OrderFlow::new(cfg).take_commands(60_000);
    let mut split = fast(cfg.engine_config());
    let mut before = Engine::with_config(cfg.engine_config());
    let mut naive = NaiveEngine::new(cfg.engine_config());
    let (mut a, mut b, mut c) = (Vec::new(), Vec::new(), Vec::new());
    let mut rejects = [0u64; REJECT_KINDS];
    for (i, cmd) in stream.iter().enumerate() {
        a.clear();
        b.clear();
        c.clear();
        split.process(cmd, &mut a);
        before.process(cmd, &mut b);
        naive.process(cmd, &mut c);
        assert_eq!(a, b, "#{i} {cmd:?}");
        assert_eq!(a, c, "#{i} {cmd:?} (naive)");
        for e in &a {
            if let Event::Rejected { reason, .. } = e {
                rejects[reject_slot(*reason)] += 1;
            }
        }
    }
    assert_eq!(split.state_hash(), before.state_hash());
    // Every engine-side reject reason was exercised.
    for (slot, n) in rejects.iter().enumerate().take(10) {
        assert!(*n > 0, "reject slot {slot} never fired: {rejects:?}");
    }
}

#[test]
fn w38_reject_counters_count_every_reject() {
    // The cold "logging" path: per-reason counters, touched only on rejects.
    let cfg = FlowConfig {
        seed: 0x38D,
        p_invalid: 0.2,
        p_fok: 0.3,
        p_post_only: 0.3,
        ..FlowConfig::default()
    };
    let stream = OrderFlow::new(cfg).take_commands(40_000);
    let mut e = fast(cfg.engine_config());
    let mut out = Vec::new();
    let mut want = [0u64; REJECT_KINDS];
    for cmd in &stream {
        out.clear();
        e.process(cmd, &mut out);
        for ev in &out {
            if let Event::Rejected { reason, .. } = ev {
                want[reject_slot(*reason)] += 1;
            }
        }
    }
    use types::RejectReason::*;
    for reason in [
        DuplicateOrderId,
        UnknownOrder,
        ZeroQuantity,
        InvalidTick,
        InvalidLot,
        PriceBand,
        WouldTake,
        FokUnfillable,
        NoLiquidity,
        InvalidReduce,
        Halted,
    ] {
        assert_eq!(
            e.reject_count(reason),
            want[reject_slot(reason)],
            "{reason:?}"
        );
    }
    assert!(e.reject_count(WouldTake) > 0 && e.reject_count(FokUnfillable) > 0);
}

#[test]
fn w38_risk_policies_behave_like_the_guarded_engine() {
    let (cfg, stream) = {
        let cfg = FlowConfig {
            seed: 0x38E,
            max_lots: 12,
            ..FlowConfig::default()
        };
        (cfg, OrderFlow::new(cfg).take_commands(30_000))
    };
    let ecfg = cfg.engine_config();
    // A compile-time cap.
    let mut a: FastEngine<Book, MaxOrderQty<7>> = FastEngine::new(ecfg, Book::new(), MaxOrderQty);
    let mut b: Guarded<Book, MaxOrderQty<7>> = Guarded::new(Engine::with_config(ecfg), MaxOrderQty);
    // The w15 risk engine with a position limit (state-dependent rejects).
    let limits = Limits {
        max_position: Qty(40),
        ..Limits::UNLIMITED
    };
    let mut c: FastEngine<LadderBook, RiskEngine> =
        FastEngine::new(ecfg, LadderBook::new(512, 2_048), RiskEngine::new(limits));
    let mut d: Guarded<Book, RiskEngine> =
        Guarded::new(Engine::with_config(ecfg), RiskEngine::new(limits));
    let (mut ea, mut eb, mut ec, mut ed) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (i, cmd) in stream.iter().enumerate() {
        for v in [&mut ea, &mut eb, &mut ec, &mut ed] {
            v.clear();
        }
        a.process(cmd, &mut ea);
        b.process(cmd, &mut eb);
        c.process(cmd, &mut ec);
        d.process(cmd, &mut ed);
        assert_eq!(ea, eb, "#{i} MaxOrderQty");
        assert_eq!(ec, ed, "#{i} RiskEngine");
    }
    assert_eq!(a.seq(), b.engine().seq());
    assert_eq!(a.state_hash(), b.engine().state_hash());
    assert_eq!(c.state_hash(), d.engine().state_hash());
    let acct = types::AccountId(1);
    let inst = types::InstrumentId(1);
    assert_eq!(c.risk().exposure(acct, inst), d.risk().exposure(acct, inst));
    assert!(c.reject_count(types::RejectReason::Risk(types::RiskReason::PositionLimit)) > 0);
}

#[test]
fn w38_tape_seq_and_state_match_the_engine() {
    let cmds = mixed_stream(0x3838, 100_000);
    let cfg = EngineConfig::new(InstrumentSpec::default());
    let mut split = fast(cfg);
    let mut before = Engine::with_config(cfg);
    let mut out = Vec::new();
    for cmd in &cmds {
        out.clear();
        split.process(cmd, &mut out);
        out.clear();
        before.process(cmd, &mut out);
    }
    assert_eq!(split.seq(), before.seq());
    assert_eq!(split.last_trade_price(), before.last_trade_price());
    assert_eq!(split.state_hash(), before.state_hash());
    assert!(
        split.tape().iter().eq(before.tape().iter()),
        "last 64 trades"
    );
    assert_eq!(split.book().len(), before.book().len());
}

#[test]
fn w38_hot_and_cold_paths_do_not_allocate() {
    let cmds = mixed_stream(0x3839, 300_000);
    let mut e = FastEngine::<Book, NoRisk>::new(
        EngineConfig {
            expected_orders: 4_096,
            ..EngineConfig::new(InstrumentSpec::default())
        },
        Book::with_capacity(4_096, 512),
        NoRisk,
    );
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
    assert_eq!(allocs, 0);
    assert!(e.reject_count(types::RejectReason::UnknownOrder) > 0);
}
