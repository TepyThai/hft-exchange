//! Week 23 — simulated traders driving risk → engine → market data, with
//! deterministic end-to-end assertions.

use engine::OrderBook;
use marketdata::Level;
use risk::Limits;
use sim::scenario::{
    Agent, AgentCtx, Delivery, InProcess, MakerAgent, MakerConfig, MomentumAgent, MomentumConfig,
    NoiseAgent, NoiseConfig, Runner, ScenarioConfig,
};
use std::cell::RefCell;
use std::rc::Rc;
use types::{Event, InstrumentId, OrderId, Price, Qty, RejectReason, RiskReason, Side};

type Log = Rc<RefCell<Vec<Delivery>>>;
type Turn = Box<dyn FnMut(&mut AgentCtx<'_>)>;

/// A user-defined agent: runs one closure per turn and logs every delivery.
struct Scripted {
    turns: std::collections::VecDeque<Turn>,
    log: Log,
}

impl Scripted {
    fn new(turns: Vec<Turn>) -> (Scripted, Log) {
        let log = Log::default();
        (
            Scripted {
                turns: turns.into(),
                log: log.clone(),
            },
            log,
        )
    }
}

impl Agent for Scripted {
    fn name(&self) -> &str {
        "scripted"
    }

    fn act(&mut self, ctx: &mut AgentCtx<'_>) {
        if let Some(mut f) = self.turns.pop_front() {
            f(ctx);
        }
    }

    fn on_event(&mut self, d: &Delivery) {
        self.log.borrow_mut().push(*d);
    }
}

fn maker(fair: i64, half_spread: i64, qty: u64) -> Box<MakerAgent> {
    Box::new(MakerAgent::new(MakerConfig {
        fair_value: Price(fair),
        half_spread_ticks: half_spread,
        qty: Qty(qty),
        lots_per_tick: 4,
        max_position: 60,
        tick: 1,
    }))
}

/// Two makers, three noise takers (market and IOC), one momentum taker.
fn market(seed: u64) -> Runner {
    let mut r = Runner::new(ScenarioConfig {
        seed,
        ..ScenarioConfig::default()
    });
    r.add_agent(maker(10_000, 2, 10));
    r.add_agent(maker(10_000, 3, 25));
    for slippage in [None, Some(0), Some(2)] {
        r.add_agent(Box::new(NoiseAgent::new(NoiseConfig {
            p_trade: 0.6,
            max_qty: 8,
            slippage_ticks: slippage,
            ..NoiseConfig::default()
        })));
    }
    r.add_agent(Box::new(MomentumAgent::new(MomentumConfig {
        lookback: 5,
        threshold_ticks: 1,
        ..MomentumConfig::default()
    })));
    r
}

fn engine_levels(v: &InProcess, side: Side) -> Vec<Level> {
    let mut out = Vec::new();
    v.engine().levels(side, usize::MAX, &mut out);
    out.iter()
        .map(|l| Level {
            price: l.price,
            qty: l.qty,
        })
        .collect()
}

#[test]
fn w23_same_seed_same_run_other_seed_differs() {
    let mut a = market(7);
    let mut b = market(7);
    let mut c = market(8);
    for r in [&mut a, &mut b, &mut c] {
        r.run(2_000);
    }
    let (ra, rb, rc) = (a.report(), b.report(), c.report());
    assert_eq!(ra, rb);
    assert_eq!(
        a.venue().engine().state_hash(),
        b.venue().engine().state_hash()
    );
    assert_ne!(ra, rc);
    assert_ne!(
        a.venue().engine().state_hash(),
        c.venue().engine().state_hash()
    );
    // A meaningful run: everybody did something.
    assert_eq!(ra.steps, 2_000);
    assert!(ra.trades > 500, "{ra:?}");
    for agent in &ra.agents {
        assert!(agent.inventory.fills > 0, "{agent:?}");
    }
}

#[test]
fn w23_positions_and_cash_are_conserved() {
    for seed in 1..=5 {
        let mut r = market(seed);
        r.run(1_500);
        let rep = r.report();
        let (mut pos, mut cash, mut volume, mut fills) = (0i64, 0i128, 0u64, 0u64);
        for a in &rep.agents {
            pos += a.inventory.position;
            cash += a.inventory.cash;
            volume += a.inventory.volume;
            fills += a.inventory.fills;
            // The runner's view of each position agrees with pre-trade risk's.
            let exposure = r.venue().risk().exposure(a.account, InstrumentId(1));
            assert_eq!(exposure.position, a.inventory.position, "seed {seed} {a:?}");
            assert_eq!(r.inventory(a.account), Some(a.inventory));
        }
        assert_eq!(pos, 0, "seed {seed}");
        assert_eq!(cash, 0, "seed {seed}");
        assert_eq!(fills, 2 * rep.trades, "every trade has two legs");
        assert_eq!(volume, 2 * rep.volume);
        assert!(rep.trades > 0);
    }
}

#[test]
fn w23_market_data_book_equals_engine_book_after_every_step() {
    for seed in [3, 11] {
        let mut r = market(seed);
        for step in 0..1_000 {
            r.step();
            let (book, venue) = (r.book(), r.venue());
            assert!(
                book.is_synced() && book.gaps() == 0,
                "seed {seed} step {step}"
            );
            for side in [Side::Buy, Side::Sell] {
                assert_eq!(
                    book.depth(side, usize::MAX),
                    engine_levels(venue, side),
                    "seed {seed} step {step} {side:?}"
                );
            }
            assert_eq!(*book.snapshot().bids, *venue.publisher().snapshot().bids);
            assert!(!venue.engine().book().is_crossed());
        }
    }
}

#[test]
fn w23_risk_rejects_reach_the_agent_and_never_the_engine() {
    let mut r = Runner::new(ScenarioConfig::default());
    let resting = Rc::new(RefCell::new(OrderId(0)));
    let (rest, cancel) = (resting.clone(), resting.clone());
    let (agent, log) = Scripted::new(vec![
        Box::new(move |ctx| {
            ctx.out.limit(Side::Buy, Price(9_990), Qty(20)); // too large
            *rest.borrow_mut() = ctx.out.limit(Side::Buy, Price(9_990), Qty(10));
            ctx.out.limit(Side::Buy, Price(9_989), Qty(10)); // 10 open + 10 > 15
        }),
        Box::new(move |ctx| {
            ctx.out.limit(Side::Sell, Price(10_010), Qty(1)); // kill switch engaged
            ctx.out.cancel(*cancel.borrow()); // cancels are never blocked
        }),
    ]);
    let me = r.add_agent(Box::new(agent));
    let limits = Limits {
        max_order_qty: Qty(10),
        max_position: Qty(15),
        ..Limits::UNLIMITED
    };
    r.venue_mut().set_limits(me, limits);

    r.step();
    let first = *resting.borrow();
    let reasons: Vec<Event> = log.borrow().iter().map(|d| d.event).collect();
    let base = me.0 as u64 * (1 << sim::scenario::AGENT_ID_SHIFT);
    assert_eq!(
        reasons,
        vec![
            Event::Rejected {
                id: OrderId(base),
                reason: RejectReason::Risk(RiskReason::MaxOrderQty)
            },
            Event::Accepted { id: first },
            Event::Rested {
                id: first,
                side: Side::Buy,
                price: Price(9_990),
                qty: Qty(10)
            },
            Event::Rejected {
                id: OrderId(base + 2),
                reason: RejectReason::Risk(RiskReason::PositionLimit)
            },
        ]
    );
    assert_eq!(
        r.venue().engine().seq().0,
        1,
        "only the accepted order reached the engine"
    );

    r.venue_mut().risk_mut().global_kill_switch().engage();
    log.borrow_mut().clear();
    r.step();
    let events: Vec<Event> = log.borrow().iter().map(|d| d.event).collect();
    assert_eq!(
        events,
        vec![
            Event::Rejected {
                id: OrderId(base + 3),
                reason: RejectReason::Risk(RiskReason::KillSwitch)
            },
            Event::Cancelled {
                id: first,
                remaining: Qty(10)
            },
        ]
    );
    assert!(log.borrow().iter().all(|d| d.account == me));
    assert_eq!(r.venue().engine().seq().0, 2);
    let rep = r.report();
    assert_eq!((rep.agents[0].rejects, rep.agents[0].risk_rejects), (3, 3));
    assert_eq!((rep.agents[0].orders, rep.agents[0].cancels), (4, 1));
    assert_eq!(r.book().best_bid(), None);
}

#[test]
fn w23_fills_reach_both_owners_and_makers_skew_after_a_fill() {
    let mut r = Runner::new(ScenarioConfig::default());
    let mk = r.add_agent(Box::new(MakerAgent::new(MakerConfig {
        fair_value: Price(10_000),
        half_spread_ticks: 2,
        qty: Qty(10),
        lots_per_tick: 2,
        max_position: 100,
        tick: 1,
    })));
    let (taker, log) = Scripted::new(vec![
        Box::new(|_| {}),
        Box::new(|ctx| {
            ctx.out.market(Side::Buy, Qty(4));
            // Another account's order cannot be cancelled.
            ctx.out.cancel(OrderId(1 << sim::scenario::AGENT_ID_SHIFT));
        }),
    ]);
    let tk = r.add_agent(Box::new(taker));

    r.step(); // maker quotes first: 9_998 / 10_002, 10 lots each
    let lvl = |p, q| {
        Some(Level {
            price: Price(p),
            qty: Qty(q),
        })
    };
    assert_eq!(
        (r.book().best_bid(), r.book().best_ask()),
        (lvl(9_998, 10), lvl(10_002, 10))
    );

    r.step(); // the taker goes first this step, then the maker re-quotes
    let base = 2u64 << sim::scenario::AGENT_ID_SHIFT;
    let ask_id = OrderId((1 << sim::scenario::AGENT_ID_SHIFT) + 1);
    let trade = Event::Trade {
        taker: OrderId(base),
        maker: ask_id,
        taker_side: Side::Buy,
        price: Price(10_002),
        qty: Qty(4),
    };
    let got: Vec<Delivery> = log.borrow().clone();
    assert_eq!(got[0].event, Event::Accepted { id: OrderId(base) });
    assert_eq!(
        got[1],
        Delivery {
            account: tk,
            order: OrderId(base),
            event: trade
        }
    );
    let leg = got[1].fill().unwrap();
    assert_eq!(
        (leg.side, leg.price, leg.qty, leg.maker),
        (Side::Buy, Price(10_002), Qty(4), false)
    );
    assert_eq!(
        got[2].event,
        Event::Rejected {
            id: OrderId(1 << sim::scenario::AGENT_ID_SHIFT),
            reason: RejectReason::UnknownOrder
        }
    );
    assert_eq!(got.len(), 3);

    let (m, t) = (r.inventory(mk).unwrap(), r.inventory(tk).unwrap());
    assert_eq!((m.position, m.cash, m.fills), (-4, 40_008, 1));
    assert_eq!((t.position, t.cash, t.fills), (4, -40_008, 1));
    assert_eq!(m.pnl(Price(10_000)), 8);
    // Short 4 at 2 lots per tick: the maker's center moved up 2 ticks.
    assert_eq!(
        (r.book().best_bid(), r.book().best_ask()),
        (lvl(10_000, 10), lvl(10_004, 10))
    );
    assert_eq!(r.report().trades, 1);
}

#[test]
fn w23_noise_and_momentum_agents_follow_their_rules() {
    // A noise taker that always buys with market orders only ever buys.
    let mut r = Runner::new(ScenarioConfig {
        seed: 5,
        ..ScenarioConfig::default()
    });
    r.add_agent(maker(10_000, 1, 50));
    let noise = r.add_agent(Box::new(NoiseAgent::new(NoiseConfig {
        p_trade: 1.0,
        p_buy: 1.0,
        max_qty: 3,
        slippage_ticks: None,
        tick: 1,
    })));
    let mut maker_low = 0;
    for _ in 0..50 {
        r.step();
        maker_low = maker_low.min(r.report().agents[0].inventory.position);
    }
    let inv = r.inventory(noise).unwrap();
    assert!(
        inv.position > 0 && inv.position as u64 == inv.volume,
        "{inv:?}"
    );
    // The maker sold down to its limit of 60, never beyond; after that the
    // market orders found no liquidity.
    let rep = r.report();
    assert_eq!(rep.agents[0].inventory.position, -inv.position);
    assert_eq!((maker_low, inv.position), (-60, 60));
    assert!(rep.agents[1].rejects > 0);

    // A momentum taker against a scripted book that trends up, then down.
    let mut r = Runner::new(ScenarioConfig::default());
    let path: Vec<i64> = (0..8)
        .chain((-8..8).rev())
        .map(|k| 10_000 + 2 * k)
        .collect();
    let mut turns: Vec<Turn> = Vec::new();
    let quotes = Rc::new(RefCell::new(Vec::<OrderId>::new()));
    for center in path {
        let quotes = quotes.clone();
        turns.push(Box::new(move |ctx| {
            for id in quotes.borrow_mut().drain(..) {
                ctx.out.cancel(id);
            }
            let bid = ctx.out.limit(Side::Buy, Price(center - 1), Qty(100));
            let ask = ctx.out.limit(Side::Sell, Price(center + 1), Qty(100));
            quotes.borrow_mut().extend([bid, ask]);
        }));
    }
    let (mover, _) = Scripted::new(turns);
    r.add_agent(Box::new(mover));
    let mom = r.add_agent(Box::new(MomentumAgent::new(MomentumConfig {
        lookback: 2,
        threshold_ticks: 3,
        qty: Qty(2),
        max_position: 8,
        tick: 1,
    })));
    let mut positions = Vec::new();
    for _ in 0..24 {
        r.step();
        positions.push(r.inventory(mom).unwrap().position);
    }
    // Up 4 ticks per 2 turns ≥ 3: buys of 2 until the limit of 8, then the
    // trend reverses and it sells down to −8.
    assert_eq!(*positions.iter().max().unwrap(), 8);
    assert_eq!(*positions.last().unwrap(), -8);
    assert!(positions.iter().all(|p| p.abs() <= 8));
    let rising = &positions[..9];
    assert!(rising.windows(2).all(|w| w[1] >= w[0]), "{positions:?}");
}
