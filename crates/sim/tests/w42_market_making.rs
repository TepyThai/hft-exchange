//! Week 42 — a market maker against simulated flow through the real
//! engine: microprice quoting, inventory skew and limits, cancel/replace.

use risk::Limits;
use sim::micro::TopOfBook;
use sim::mm::{MarketMaker, MmConfig, Quote, Quotes};
use sim::scenario::{
    Agent, AgentCtx, Delivery, MakerAgent, MakerConfig, NoiseAgent, NoiseConfig, Outbox, Runner,
    ScenarioConfig,
};
use std::cell::RefCell;
use std::rc::Rc;
use types::{
    AccountId, Command, Event, InstrumentId, NewOrder, OrderId, OrderKind, Price, Qty, Side,
    TimeInForce,
};

const ME: AccountId = AccountId(7);
const FAIR: i64 = 10_000;

fn cfg(skew: f64, max: i64) -> MmConfig {
    MmConfig {
        quote_qty: Qty(5),
        half_spread_ticks: 1.0,
        skew_ticks_per_lot: skew,
        max_inventory: max,
        requote_ticks: 1,
        tick: 1,
    }
}

fn q(price: i64, qty: u64) -> Option<Quote> {
    Some(Quote {
        price: Price(price),
        qty: Qty(qty),
    })
}

fn post(id: u64, side: Side, price: i64, qty: u64) -> Command {
    Command::New(NewOrder {
        id: OrderId(id),
        account: ME,
        instrument: InstrumentId(1),
        side,
        qty: Qty(qty),
        kind: OrderKind::Limit {
            price: Price(price),
            post_only: true,
        },
        tif: TimeInForce::Gtc,
    })
}

/// Delivers `Cancelled` confirmations for orders `ids`.
fn confirm_cancels_of(mm: &mut MarketMaker, ids: &[u64]) {
    for &id in ids {
        let event = Event::Cancelled {
            id: OrderId(id),
            remaining: Qty(1),
        };
        mm.on_delivery(&Delivery {
            account: ME,
            order: OrderId(id),
            event,
        });
    }
}

/// Delivers the `Cancelled` confirmations of the cancels in `out`.
fn confirm_cancels(mm: &mut MarketMaker, out: &Outbox) {
    for cmd in out.commands() {
        if let Command::Cancel { id } = *cmd {
            let event = Event::Cancelled {
                id,
                remaining: Qty(1),
            };
            mm.on_delivery(&Delivery {
                account: ME,
                order: id,
                event,
            });
        }
    }
}

fn decide(mm: &mut MarketMaker, top: &TopOfBook, out: &mut Outbox) -> Vec<Command> {
    out.clear();
    mm.decide(top, out);
    out.commands().to_vec()
}

/// A fill of `qty` against our resting order `id` by an incoming order.
fn hit(mm: &mut MarketMaker, id: u64, our_side: Side, price: i64, qty: u64) {
    let event = Event::Trade {
        taker: OrderId(900 + id),
        maker: OrderId(id),
        taker_side: our_side.opposite(),
        price: Price(price),
        qty: Qty(qty),
    };
    mm.on_delivery(&Delivery {
        account: ME,
        order: OrderId(id),
        event,
    });
}

#[test]
fn w42_quotes_center_on_the_microprice_and_skew_against_inventory() {
    let mm = MarketMaker::new(cfg(0.25, 20));
    let top = TopOfBook::new(Some((9_997, 20)), Some((10_003, 20)));
    assert_eq!(mm.fair_value(&top), Some(10_000.0));
    let t = |pos| mm.target(&top, pos);
    assert_eq!(
        t(0),
        Quotes {
            bid: q(9_999, 5),
            ask: q(10_001, 5)
        }
    );
    // Long 4: reservation 1 tick lower; short 4: 1 tick higher.
    assert_eq!(
        t(4),
        Quotes {
            bid: q(9_998, 5),
            ask: q(10_000, 5)
        }
    );
    assert_eq!(
        t(-4),
        Quotes {
            bid: q(10_000, 5),
            ask: q(10_002, 5)
        }
    );
    // Near the limit the size shrinks; the ask never goes below the market
    // bid + 1 tick (it would take liquidity instead of providing it).
    assert_eq!(
        t(18),
        Quotes {
            bid: q(9_994, 2),
            ask: q(9_998, 5)
        }
    );
    assert_eq!(
        t(20),
        Quotes {
            bid: None,
            ask: q(9_998, 5)
        }
    );
    assert_eq!(
        t(-20),
        Quotes {
            bid: q(10_002, 5),
            ask: None
        }
    );
    // The fair value leans toward the thin side.
    let thin_ask = TopOfBook::new(Some((99, 30)), Some((101, 10)));
    assert_eq!(mm.fair_value(&thin_ask), Some(100.5));
    assert_eq!(
        mm.target(&thin_ask, 0),
        Quotes {
            bid: q(99, 5),
            ask: q(102, 5)
        }
    );
    // No two-sided market, no quotes.
    assert_eq!(
        mm.target(&TopOfBook::new(Some((99, 1)), None), 0),
        Quotes::default()
    );

    // Over the whole range: both quotes move down as the position grows,
    // never cross each other or the market, and sizes respect the limit.
    let mut prev: Option<Quotes> = None;
    for pos in -20..=20 {
        let now = t(pos);
        if let (Some(b), Some(a)) = (now.bid, now.ask) {
            assert!(b.price < a.price);
        }
        if let Some(b) = now.bid {
            assert!(b.price.0 < 10_003 && b.qty.0 as i64 <= 20 - pos);
        }
        if let Some(a) = now.ask {
            assert!(a.price.0 > 9_997 && a.qty.0 as i64 <= 20 + pos);
        }
        if let Some(p) = prev {
            for (old, new) in [(p.bid, now.bid), (p.ask, now.ask)] {
                if let (Some(o), Some(n)) = (old, new) {
                    assert!(n.price <= o.price, "pos {pos}");
                }
            }
        }
        prev = Some(now);
    }
}

#[test]
fn w42_reacts_to_a_fill_within_the_next_decision() {
    let mut mm = MarketMaker::new(cfg(0.5, 10));
    let mut out = Outbox::new(ME, InstrumentId(1), 1);
    let top = TopOfBook::new(Some((99, 10)), Some((101, 10)));
    assert_eq!(
        decide(&mut mm, &top, &mut out),
        vec![post(1, Side::Buy, 99, 5), post(2, Side::Sell, 101, 5)]
    );

    // 4 lots of the bid fill: long 4, reservation 2 ticks lower.
    hit(&mut mm, 1, Side::Buy, 99, 4);
    assert_eq!(mm.inventory().position, 4);
    assert_eq!(mm.working(Side::Buy).map(|w| w.remaining), Some(Qty(1)));
    assert_eq!(
        decide(&mut mm, &top, &mut out),
        vec![
            Command::Cancel { id: OrderId(1) },
            Command::Cancel { id: OrderId(2) },
            post(3, Side::Buy, 97, 5),
            post(4, Side::Sell, 100, 5),
        ]
    );
    confirm_cancels(&mut mm, &out);
    // The new bid fills completely: long 9, room for 1 more lot.
    hit(&mut mm, 3, Side::Buy, 97, 5);
    assert_eq!(
        decide(&mut mm, &top, &mut out),
        vec![post(5, Side::Buy, 94, 1)]
    );
    // At the limit: no bid at all, the ask stays where it is.
    hit(&mut mm, 5, Side::Buy, 94, 1);
    assert_eq!(decide(&mut mm, &top, &mut out), vec![]);
    assert_eq!(mm.working(Side::Buy), None);
    let inv = mm.inventory();
    assert_eq!(
        (inv.position, inv.cash, inv.fills),
        (10, -(99 * 4 + 97 * 5 + 94), 3)
    );
}

#[test]
fn w42_requotes_only_when_the_target_moves_enough() {
    let mut mm = MarketMaker::new(MmConfig {
        requote_ticks: 2,
        ..cfg(0.0, 20)
    });
    let mut out = Outbox::new(ME, InstrumentId(1), 1);
    let top = |b: i64| TopOfBook::new(Some((b, 10)), Some((b + 2, 10)));
    assert_eq!(decide(&mut mm, &top(99), &mut out).len(), 2);
    assert_eq!(
        decide(&mut mm, &top(99), &mut out),
        vec![],
        "nothing changed"
    );
    assert_eq!(
        decide(&mut mm, &top(100), &mut out),
        vec![],
        "1 tick < threshold"
    );
    assert_eq!(
        decide(&mut mm, &top(101), &mut out),
        vec![
            Command::Cancel { id: OrderId(1) },
            Command::Cancel { id: OrderId(2) },
            post(3, Side::Buy, 101, 5),
            post(4, Side::Sell, 103, 5),
        ]
    );
    confirm_cancels(&mut mm, &out);
    // A one-sided market: pull both quotes.
    let one_sided = TopOfBook::new(Some((101, 10)), None);
    assert_eq!(
        decide(&mut mm, &one_sided, &mut out),
        vec![
            Command::Cancel { id: OrderId(3) },
            Command::Cancel { id: OrderId(4) }
        ]
    );
    // Until the cancels are confirmed those quotes may still fill, so they
    // count against the limit: with max 6, only 1 lot of room is left.
    mm.set_max_inventory(6);
    assert_eq!(
        decide(&mut mm, &top(101), &mut out),
        vec![post(5, Side::Buy, 101, 1), post(6, Side::Sell, 103, 1)]
    );
    confirm_cancels_of(&mut mm, &[3, 4]);
    mm.set_max_inventory(20);
    // A quote smaller than its target is left alone (a partly filled quote
    // keeps its queue priority) ...
    assert_eq!(decide(&mut mm, &top(101), &mut out), vec![]);
    let (b, a) = (
        mm.working(Side::Buy).unwrap(),
        mm.working(Side::Sell).unwrap(),
    );
    assert_eq!((b.remaining, a.remaining), (Qty(1), Qty(1)));
    // ... and once it is gone, the next one is full size.
    confirm_cancels_of(&mut mm, &[5, 6]);
    assert_eq!(
        decide(&mut mm, &top(101), &mut out),
        vec![post(7, Side::Buy, 101, 5), post(8, Side::Sell, 103, 5)]
    );
    // A tighter limit shrinks the quotes in place (keeping queue priority).
    mm.set_max_inventory(3);
    assert_eq!(
        decide(&mut mm, &top(101), &mut out),
        vec![
            Command::Reduce {
                id: OrderId(7),
                qty: Qty(3)
            },
            Command::Reduce {
                id: OrderId(8),
                qty: Qty(3)
            }
        ]
    );
    // Cancels, expiries and rejections retire a working quote.
    let rejected = Event::Rejected {
        id: OrderId(8),
        reason: types::RejectReason::WouldTake,
    };
    mm.on_delivery(&Delivery {
        account: ME,
        order: OrderId(8),
        event: rejected,
    });
    assert_eq!(mm.working(Side::Sell), None);
    assert_eq!(mm.decisions(), 9);
}

/// Lets a test keep a handle on the strategy while the runner owns it.
struct Shared(Rc<RefCell<MarketMaker>>);

impl Agent for Shared {
    fn name(&self) -> &str {
        "market-maker"
    }
    fn act(&mut self, ctx: &mut AgentCtx<'_>) {
        self.0.borrow_mut().act(ctx)
    }
    fn on_event(&mut self, d: &Delivery) {
        self.0.borrow_mut().on_event(d)
    }
}

/// Two wide makers anchored at `FAIR`, three noise takers and the strategy.
fn arena(seed: u64, p_buy: f64, skew: f64) -> (Runner, Rc<RefCell<MarketMaker>>, AccountId) {
    let mut r = Runner::new(ScenarioConfig {
        seed,
        ..ScenarioConfig::default()
    });
    for qty in [20, 30] {
        r.add_agent(Box::new(MakerAgent::new(MakerConfig {
            fair_value: Price(FAIR),
            half_spread_ticks: 3,
            qty: Qty(qty),
            lots_per_tick: 50,
            max_position: 10_000,
            tick: 1,
        })));
    }
    for _ in 0..3 {
        r.add_agent(Box::new(NoiseAgent::new(NoiseConfig {
            p_trade: 0.5,
            p_buy,
            max_qty: 4,
            slippage_ticks: None,
            tick: 1,
        })));
    }
    let mm = Rc::new(RefCell::new(MarketMaker::new(cfg(skew, 20))));
    let me = r.add_agent(Box::new(Shared(mm.clone())));
    (r, mm, me)
}

#[test]
fn w42_never_exceeds_its_inventory_limit() {
    for (seed, p_buy) in [(1, 0.85), (2, 0.15), (3, 0.5)] {
        // A small skew, so one-sided flow can push it all the way.
        let (mut r, mm, me) = arena(seed, p_buy, 0.05);
        let limits = Limits {
            max_position: Qty(20),
            ..Limits::UNLIMITED
        };
        r.venue_mut().set_limits(me, limits);
        let (mut lo, mut hi) = (0, 0);
        for step in 0..3_000 {
            r.step();
            let e = r.venue().risk().exposure(me, InstrumentId(1));
            assert!(e.position.abs() <= 20, "seed {seed} step {step}: {e:?}");
            assert!(
                e.position + e.open_buy.0 as i64 <= 20,
                "seed {seed} step {step}: {e:?}"
            );
            assert!(
                e.position - e.open_sell.0 as i64 >= -20,
                "seed {seed} step {step}: {e:?}"
            );
            assert_eq!(mm.borrow().inventory().position, e.position);
            lo = lo.min(e.position);
            hi = hi.max(e.position);
        }
        // One-sided flow pushes it to the limit (and no further).
        match seed {
            1 => assert_eq!(lo, -20),
            2 => assert_eq!(hi, 20),
            _ => {}
        }
        let rep = r.report();
        let me_rep = rep.agents.iter().find(|a| a.account == me).unwrap();
        assert_eq!(me_rep.risk_rejects, 0, "it never even asks for more");
        assert_eq!(me_rep.inventory, *mm.borrow().inventory());
    }
}

#[test]
fn w42_earns_a_positive_edge_against_noise_flow() {
    for seed in [11, 12, 13] {
        let (mut r, mm, me) = arena(seed, 0.5, 0.1);
        r.run(10_000);
        let inv = *mm.borrow().inventory();
        // Marked at the true fair value, P&L is the edge over all fills.
        let edge = inv.pnl(Price(FAIR));
        assert!(inv.volume > 2_000, "seed {seed}: it trades ({inv:?})");
        assert!(edge > 0, "seed {seed}: edge {edge}");
        // About 1.3 ticks per lot on these seeds; half a tick is a robust bound.
        assert!(
            edge as f64 / inv.volume as f64 > 0.5,
            "seed {seed}: {edge} over {} lots",
            inv.volume
        );
        // The noise takers pay for it.
        let rep = r.report();
        let noise: i128 = rep
            .agents
            .iter()
            .filter(|a| a.name == "noise")
            .map(|a| a.inventory.pnl(Price(FAIR)))
            .sum();
        assert!(noise < 0, "seed {seed}");
        assert_eq!(
            rep.agents.iter().find(|a| a.account == me).unwrap().rejects,
            0
        );
    }
}
