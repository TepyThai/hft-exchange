//! Week 37: compile-time specialization — risk policies as type
//! parameters, static vs dynamic dispatch, const-generic capacities.

mod common;

use common::allocations_in;
use engine::testkit::{FlowConfig, OrderFlow};
use engine::{
    Book, DynGuarded, Engine, EngineConfig, Guarded, LadderBook, MaxOrderQty, NoRisk, OrderBook,
    RiskPolicy, TapeRing, TradePrint, TradeTape,
};
use risk::{Limits, RiskEngine};
use types::{
    AccountId, Command, Event, InstrumentId, NewOrder, OrderId, Price, Qty, RejectReason,
    RiskReason, Rng, Seq, Side,
};

fn stream(seed: u64, n: usize) -> (FlowConfig, Vec<Command>) {
    let cfg = FlowConfig {
        seed,
        max_lots: 12,
        ..FlowConfig::default()
    };
    (cfg, OrderFlow::new(cfg).take_commands(n))
}

fn dyn_guarded(cfg: EngineConfig, risk: Box<dyn RiskPolicy>) -> DynGuarded {
    let book: Box<dyn OrderBook> = Box::new(Book::new());
    Guarded::new(Engine::with_book(cfg, book), risk)
}

#[test]
fn w37_static_and_dyn_configurations_agree() {
    let (cfg, cmds) = stream(0x37, 30_000);
    let ecfg = cfg.engine_config();
    let mut plain = Engine::with_config(ecfg);
    let mut none: Guarded<Book, NoRisk> = Guarded::new(Engine::with_config(ecfg), NoRisk);
    let mut capped: Guarded<Book, MaxOrderQty<8>> =
        Guarded::new(Engine::with_config(ecfg), MaxOrderQty);
    let mut ladder: Guarded<LadderBook, MaxOrderQty<8>> = Guarded::new(
        Engine::with_book(ecfg, LadderBook::new(256, 1_024)),
        MaxOrderQty,
    );
    let mut dynamic = dyn_guarded(ecfg, Box::new(MaxOrderQty::<8>));
    let (mut a, mut b, mut c, mut d, mut e) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut risk_rejects = 0;
    for (i, cmd) in cmds.iter().enumerate() {
        for v in [&mut a, &mut b, &mut c, &mut d, &mut e] {
            v.clear();
        }
        plain.process(cmd, &mut a);
        none.process(cmd, &mut b);
        capped.process(cmd, &mut c);
        ladder.process(cmd, &mut d);
        dynamic.process(cmd, &mut e);
        assert_eq!(a, b, "#{i}: Guarded<_, NoRisk> must equal the bare engine");
        assert_eq!(c, d, "#{i}: book choice changed the events");
        assert_eq!(c, e, "#{i}: dyn dispatch changed the events");
        risk_rejects += c
            .iter()
            .filter(|ev| {
                matches!(
                    ev,
                    Event::Rejected {
                        reason: RejectReason::Risk(_),
                        ..
                    }
                )
            })
            .count();
    }
    assert!(risk_rejects > 100, "the cap must bite ({risk_rejects})");
    assert_eq!(plain.state_hash(), none.engine().state_hash());
    assert_eq!(capped.engine().state_hash(), dynamic.engine().state_hash());
    assert_eq!(capped.engine().state_hash(), ladder.engine().state_hash());
    // Risk rejects never reached the engine: no sequence number.
    assert_eq!(
        capped.engine().seq().0 + risk_rejects as u64,
        plain.seq().0,
        "risk-rejected orders must not be sequenced"
    );
}

#[test]
fn w37_max_order_qty_rejects_in_front_of_the_engine() {
    let mut g: Guarded<Book, MaxOrderQty<10>> =
        Guarded::new(Engine::new(Default::default()), MaxOrderQty);
    let run = |g: &mut Guarded<Book, MaxOrderQty<10>>, cmd: Command| {
        let mut out = Vec::new();
        g.process(&cmd, &mut out);
        out
    };
    let big = run(&mut g, Command::New(NewOrder::limit(1, Side::Buy, 100, 11)));
    assert_eq!(
        big,
        vec![Event::Rejected {
            id: OrderId(1),
            reason: RejectReason::Risk(RiskReason::MaxOrderQty)
        }]
    );
    assert_eq!(g.engine().seq(), Seq(0), "the engine never saw it");
    assert!(g.engine().book().is_empty());
    // At the limit: passes risk, the engine accepts it.
    let ok = run(&mut g, Command::New(NewOrder::limit(2, Side::Buy, 100, 10)));
    assert_eq!(ok[0], Event::Accepted { id: OrderId(2) });
    // Zero passes the risk cap and is the engine's to reject.
    let zero = run(&mut g, Command::New(NewOrder::limit(3, Side::Buy, 100, 0)));
    assert_eq!(
        zero,
        vec![Event::Rejected {
            id: OrderId(3),
            reason: RejectReason::ZeroQuantity
        }]
    );
    // Cancels are never risk-checked.
    let cancel = run(&mut g, Command::Cancel { id: OrderId(2) });
    assert!(matches!(cancel[..], [Event::Cancelled { .. }]));
    assert_eq!(g.engine().seq(), Seq(3));
    // The limit is part of the type.
    let strict = MaxOrderQty::<1>;
    let order = NewOrder::limit(9, Side::Sell, 100, 2);
    assert!(strict.check(&order, Price(100)).is_err());
    assert!(MaxOrderQty::<2>.check(&order, Price(100)).is_ok());
    assert!(NoRisk.check(&order, Price(100)).is_ok());
}

#[test]
fn w37_risk_engine_policy_tracks_exposure_through_the_engine() {
    let limits = Limits {
        max_position: Qty(10),
        ..Limits::UNLIMITED
    };
    let mut g: Guarded<Book, RiskEngine> =
        Guarded::new(Engine::new(Default::default()), RiskEngine::new(limits));
    let mut out = Vec::new();
    let mut send = |g: &mut Guarded<Book, RiskEngine>, order: NewOrder| {
        out.clear();
        g.process(&Command::New(order), &mut out);
        out.clone()
    };
    let (a, b, inst) = (AccountId(1), AccountId(2), InstrumentId(1));
    send(
        &mut g,
        NewOrder::limit(1, Side::Buy, 100, 6).with_account(1),
    );
    assert_eq!(g.risk().exposure(a, inst).open_buy, Qty(6));
    // Account 2 sells 4 into it: account 1 is now long 4 with 2 still open.
    let t = send(
        &mut g,
        NewOrder::limit(2, Side::Sell, 100, 4).with_account(2),
    );
    assert!(matches!(t[1], Event::Trade { qty: Qty(4), .. }));
    let ea = g.risk().exposure(a, inst);
    assert_eq!((ea.position, ea.open_buy), (4, Qty(2)));
    assert_eq!(g.risk().exposure(b, inst).position, -4);
    // 4 + 2 + 5 > 10: the projected position breaks the limit.
    let r = send(&mut g, NewOrder::limit(3, Side::Buy, 99, 5).with_account(1));
    assert_eq!(
        r,
        vec![Event::Rejected {
            id: OrderId(3),
            reason: RejectReason::Risk(RiskReason::PositionLimit)
        }]
    );
    // 4 + 2 + 4 = 10 is fine.
    let ok = send(&mut g, NewOrder::limit(4, Side::Buy, 99, 4).with_account(1));
    assert_eq!(ok[0], Event::Accepted { id: OrderId(4) });
    // A cancel goes to the engine and releases the open quantity in risk.
    let mut cancel = Vec::new();
    g.process(&Command::Cancel { id: OrderId(1) }, &mut cancel);
    assert!(matches!(cancel[..], [Event::Cancelled { .. }]));
    assert_eq!(g.risk().exposure(a, inst).open_buy, Qty(4));
    assert_eq!(g.risk().open_order_count(), 1);
    // The kill switch works through the policy too.
    g.risk_mut().kill_account(a);
    let killed = send(&mut g, NewOrder::limit(5, Side::Buy, 98, 1).with_account(1));
    assert_eq!(
        killed,
        vec![Event::Rejected {
            id: OrderId(5),
            reason: RejectReason::Risk(RiskReason::KillSwitch)
        }]
    );
    let (engine, _risk) = g.into_parts();
    assert_eq!(engine.book().len(), 1);
}

#[test]
fn w37_boxed_policy_keeps_a_shared_kill_switch() {
    let risk = RiskEngine::new(Limits::UNLIMITED);
    let switch = risk.global_kill_switch();
    let mut g = dyn_guarded(EngineConfig::default(), Box::new(risk));
    let mut out = Vec::new();
    g.process(
        &Command::New(NewOrder::limit(1, Side::Buy, 100, 1)),
        &mut out,
    );
    assert_eq!(out[0], Event::Accepted { id: OrderId(1) });
    switch.engage();
    out.clear();
    g.process(
        &Command::New(NewOrder::limit(2, Side::Buy, 100, 1)),
        &mut out,
    );
    assert_eq!(
        out,
        vec![Event::Rejected {
            id: OrderId(2),
            reason: RejectReason::Risk(RiskReason::KillSwitch)
        }]
    );
    // Cancels still go through while the switch is engaged.
    out.clear();
    g.process(&Command::Cancel { id: OrderId(1) }, &mut out);
    assert!(matches!(out[..], [Event::Cancelled { .. }]));
}

fn print(n: u64) -> TradePrint {
    TradePrint {
        seq: Seq(n),
        taker: OrderId(n),
        maker: OrderId(n + 1),
        taker_side: if n.is_multiple_of(2) {
            Side::Buy
        } else {
            Side::Sell
        },
        price: Price(n as i64 * 3),
        qty: Qty(n % 7 + 1),
    }
}

#[test]
fn w37_tape_ring_behaves_like_the_trade_tape() {
    let mut rng = Rng::new(0x37E);
    let mut ring = TapeRing::<8>::new();
    let mut tape = TradeTape::new(8);
    assert!(ring.is_empty() && ring.last().is_none());
    for n in 0..1_000u64 {
        if rng.chance(0.01) {
            ring.clear();
            tape.clear();
        } else {
            ring.push(print(n));
            tape.push(print(n));
        }
        assert_eq!(ring.len(), tape.len());
        assert_eq!(ring.last(), tape.last());
        assert!(ring.iter().eq(tape.iter()), "after push {n}");
        assert!(ring.iter().rev().eq(tape.iter().rev()));
    }
}

#[test]
fn w37_tape_ring_capacity_is_a_compile_time_constant() {
    assert_eq!(TapeRing::<64>::CAPACITY, 64);
    assert_eq!(TapeRing::<1>::CAPACITY, 1);
    // An inline array: no heap, no capacity field.
    assert!(std::mem::size_of::<TapeRing<64>>() <= 64 * std::mem::size_of::<TradePrint>() + 16);
    let allocs = allocations_in(|| {
        let mut ring = TapeRing::<64>::new();
        for n in 0..10_000 {
            ring.push(print(n));
        }
        assert_eq!(ring.len(), 64);
        assert_eq!(ring.iter().next().unwrap().seq, Seq(10_000 - 64));
        let mut one = TapeRing::<1>::new();
        one.push(print(1));
        one.push(print(2));
        assert_eq!((one.len(), one.last().unwrap().seq), (1, Seq(2)));
    });
    assert_eq!(allocs, 0);
}
