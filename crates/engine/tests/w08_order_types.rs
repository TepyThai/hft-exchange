//! Week 8: time in force, post-only and validation.

use engine::{Engine, EngineConfig, OrderBook, RestingOrder};
use types::{
    Command, Event, InstrumentSpec, NewOrder, OrderId, OrderKind, Price, Qty, RejectReason, Rng,
    Side, TimeInForce,
};

use RejectReason::*;
use Side::{Buy, Sell};
use TimeInForce::{Fok, Gtc, Ioc};

fn run(e: &mut Engine, order: NewOrder) -> Vec<Event> {
    let mut out = Vec::new();
    e.process(&Command::New(order), &mut out);
    out
}

fn cmd(e: &mut Engine, cmd: Command) -> Vec<Event> {
    let mut out = Vec::new();
    e.process(&cmd, &mut out);
    out
}

fn lim(id: u64, side: Side, price: i64, qty: u64) -> NewOrder {
    NewOrder::limit(id, side, price, qty)
}

fn rejected(id: u64, reason: RejectReason) -> Vec<Event> {
    vec![Event::Rejected {
        id: OrderId(id),
        reason,
    }]
}

fn resting(e: &Engine) -> Vec<RestingOrder> {
    let mut out = Vec::new();
    for side in [Buy, Sell] {
        e.book().visit_orders(side, &mut |o| {
            out.push(o);
            true
        });
    }
    out
}

/// Asks: 100×3 (id 1), 101×4 (id 2), 103×5 (id 3); bid 98×2 (id 4).
fn ladder() -> Engine {
    let mut e = Engine::new(InstrumentSpec::default());
    run(&mut e, lim(1, Sell, 100, 3));
    run(&mut e, lim(2, Sell, 101, 4));
    run(&mut e, lim(3, Sell, 103, 5));
    run(&mut e, lim(4, Buy, 98, 2));
    e
}

fn trade(taker: u64, maker: u64, side: Side, price: i64, qty: u64) -> Event {
    Event::Trade {
        taker: OrderId(taker),
        maker: OrderId(maker),
        taker_side: side,
        price: Price(price),
        qty: Qty(qty),
    }
}

#[test]
fn w08_ioc_fills_what_it_can_and_expires_the_rest() {
    let mut e = ladder();
    assert_eq!(
        run(&mut e, lim(10, Buy, 101, 10).with_tif(Ioc)),
        vec![
            Event::Accepted { id: OrderId(10) },
            trade(10, 1, Buy, 100, 3),
            trade(10, 2, Buy, 101, 4),
            Event::Expired {
                id: OrderId(10),
                remaining: Qty(3)
            },
        ]
    );
    assert!(!e.book().contains(OrderId(10)), "IOC never rests");
    // Not crossing at all: accepted, then expires untouched.
    assert_eq!(
        run(&mut e, lim(11, Sell, 99, 5).with_tif(Ioc)),
        vec![
            Event::Accepted { id: OrderId(11) },
            Event::Expired {
                id: OrderId(11),
                remaining: Qty(5)
            }
        ]
    );
    // Fully filled IOC: no Expired.
    assert_eq!(
        run(&mut e, lim(12, Buy, 103, 5).with_tif(Ioc)),
        vec![
            Event::Accepted { id: OrderId(12) },
            trade(12, 3, Buy, 103, 5)
        ]
    );
    assert_eq!(resting(&e), vec![RestingOrder::new(4, Buy, 98, 2)]);
}

#[test]
fn w08_fok_is_all_or_nothing_and_checking_never_mutates() {
    let mut e = ladder();
    // Within limit 101 only 7 are available.
    assert_eq!(e.fillable(Buy, Some(Price(101)), Qty(100)), Qty(7));
    assert_eq!(
        e.fillable(Buy, Some(Price(101)), Qty(5)),
        Qty(5),
        "stops once satisfied"
    );
    assert_eq!(e.fillable(Buy, Some(Price(99)), Qty(5)), Qty(0));
    assert_eq!(
        e.fillable(Buy, None, Qty(100)),
        Qty(12),
        "market: every level"
    );
    assert_eq!(e.fillable(Sell, Some(Price(98)), Qty(9)), Qty(2));
    assert_eq!(e.fillable(Sell, None, Qty(0)), Qty(0));

    let before = resting(&e);
    assert_eq!(
        run(&mut e, lim(10, Buy, 101, 8).with_tif(Fok)),
        rejected(10, FokUnfillable)
    );
    assert_eq!(
        run(&mut e, NewOrder::market(11, Buy, 13).with_tif(Fok)),
        rejected(11, FokUnfillable)
    );
    assert_eq!(
        resting(&e),
        before,
        "a killed FOK leaves the book untouched"
    );
    assert_eq!(e.last_trade_price(), None);

    // Exactly fillable: fills completely, no Rested / Expired.
    assert_eq!(
        run(&mut e, lim(12, Buy, 101, 7).with_tif(Fok)),
        vec![
            Event::Accepted { id: OrderId(12) },
            trade(12, 1, Buy, 100, 3),
            trade(12, 2, Buy, 101, 4)
        ]
    );
    assert_eq!(
        run(&mut e, NewOrder::market(13, Buy, 5).with_tif(Fok)),
        vec![
            Event::Accepted { id: OrderId(13) },
            trade(13, 3, Buy, 103, 5)
        ]
    );
    assert_eq!(resting(&e), vec![RestingOrder::new(4, Buy, 98, 2)]);
}

#[test]
fn w08_post_only_rests_or_is_rejected_never_takes() {
    let mut e = ladder();
    assert_eq!(
        run(&mut e, lim(10, Buy, 100, 1).post_only()),
        rejected(10, WouldTake),
        "at the touch takes"
    );
    assert_eq!(
        run(&mut e, lim(11, Buy, 105, 1).post_only()),
        rejected(11, WouldTake)
    );
    assert_eq!(
        run(&mut e, lim(12, Buy, 99, 1).post_only()),
        vec![
            Event::Accepted { id: OrderId(12) },
            Event::Rested {
                id: OrderId(12),
                side: Buy,
                price: Price(99),
                qty: Qty(1)
            }
        ]
    );
    // Post-only IOC is legal but pointless; post-only FOK can never fill.
    assert_eq!(
        run(&mut e, lim(13, Sell, 102, 1).post_only().with_tif(Ioc)),
        vec![
            Event::Accepted { id: OrderId(13) },
            Event::Expired {
                id: OrderId(13),
                remaining: Qty(1)
            }
        ]
    );
    assert_eq!(
        run(&mut e, lim(14, Sell, 102, 1).post_only().with_tif(Fok)),
        rejected(14, FokUnfillable)
    );
    assert_eq!(
        run(&mut e, lim(15, Sell, 99, 1).post_only().with_tif(Fok)),
        rejected(15, WouldTake)
    );
    // Empty opposite side: post-only always rests.
    let mut empty = Engine::new(InstrumentSpec::default());
    assert_eq!(run(&mut empty, lim(1, Sell, 1, 1).post_only()).len(), 2);
    assert_eq!(e.last_trade_price(), None, "nothing traded");
}

#[test]
fn w08_tick_and_lot_validation() {
    let spec = InstrumentSpec {
        tick: 5,
        lot: 10,
        ..InstrumentSpec::default()
    };
    let mut e = Engine::new(spec);
    assert_eq!(run(&mut e, lim(1, Buy, 101, 10)), rejected(1, InvalidTick));
    assert_eq!(run(&mut e, lim(2, Buy, 100, 15)), rejected(2, InvalidLot));
    assert_eq!(run(&mut e, lim(3, Buy, 100, 0)), rejected(3, ZeroQuantity));
    assert_eq!(
        run(&mut e, NewOrder::market(4, Buy, 25)),
        rejected(4, InvalidLot),
        "market orders have lots too"
    );
    assert_eq!(
        run(&mut e, lim(5, Buy, -15, 20)).len(),
        2,
        "negative multiples of the tick are valid"
    );
    assert_eq!(run(&mut e, lim(6, Sell, -7, 20)), rejected(6, InvalidTick));
    assert_eq!(run(&mut e, lim(7, Buy, 100, 30)).len(), 2);

    let reduce = |e: &mut Engine, qty| {
        cmd(
            e,
            Command::Reduce {
                id: OrderId(7),
                qty: Qty(qty),
            },
        )
    };
    assert_eq!(reduce(&mut e, 15), rejected(7, InvalidLot));
    assert_eq!(reduce(&mut e, 0), rejected(7, ZeroQuantity));
    assert_eq!(reduce(&mut e, 30), rejected(7, InvalidReduce));
    assert_eq!(
        reduce(&mut e, 10),
        vec![Event::Reduced {
            id: OrderId(7),
            remaining: Qty(10)
        }]
    );
    assert_eq!(
        cmd(
            &mut e,
            Command::Reduce {
                id: OrderId(99),
                qty: Qty(10)
            }
        ),
        rejected(99, UnknownOrder)
    );
}

#[test]
fn w08_price_band_follows_the_reference_price() {
    let spec = InstrumentSpec {
        tick: 1,
        lot: 1,
        band_ticks: 10,
        ..InstrumentSpec::default()
    };
    // No reference yet: no band.
    let mut free = Engine::new(spec);
    assert_eq!(run(&mut free, lim(1, Buy, 5, 1)).len(), 2);

    let mut e = Engine::with_config(EngineConfig::new(spec).with_reference_price(Price(1_000)));
    assert_eq!(e.reference_price(), Some(Price(1_000)));
    assert_eq!(
        run(&mut e, lim(1, Sell, 1_010, 1)).len(),
        2,
        "edge of the band is inside"
    );
    assert_eq!(run(&mut e, lim(2, Sell, 1_011, 1)), rejected(2, PriceBand));
    assert_eq!(run(&mut e, lim(3, Buy, 989, 1)), rejected(3, PriceBand));
    assert_eq!(run(&mut e, lim(4, Buy, 990, 1)).len(), 2);
    // A trade at 1010 moves the reference.
    assert_eq!(run(&mut e, NewOrder::market(5, Buy, 1)).len(), 2);
    assert_eq!(e.reference_price(), Some(Price(1_010)));
    assert_eq!(run(&mut e, lim(6, Sell, 1_020, 1)).len(), 2);
    assert_eq!(run(&mut e, lim(7, Buy, 999, 1)), rejected(7, PriceBand));

    // The band is in ticks: with tick 5, 10 ticks = 50 price units.
    let spec5 = InstrumentSpec {
        tick: 5,
        band_ticks: 10,
        ..spec
    };
    let mut e5 = Engine::with_config(EngineConfig::new(spec5).with_reference_price(Price(1_000)));
    assert_eq!(run(&mut e5, lim(1, Sell, 1_050, 1)).len(), 2);
    assert_eq!(run(&mut e5, lim(2, Sell, 1_055, 1)), rejected(2, PriceBand));
}

#[test]
fn w08_first_failing_check_wins_in_documented_order() {
    let spec = InstrumentSpec {
        tick: 5,
        lot: 10,
        band_ticks: 4,
        ..InstrumentSpec::default()
    };
    let mut e = Engine::with_config(EngineConfig::new(spec).with_reference_price(Price(100)));
    run(&mut e, lim(1, Sell, 100, 10));
    let before = resting(&e);

    let check = |e: &Engine, o: NewOrder| e.validate(&o);
    // zero qty + bad tick + out of band + duplicate
    assert_eq!(check(&e, lim(1, Buy, 3, 0)), Err(ZeroQuantity));
    // bad lot + bad tick + out of band
    assert_eq!(check(&e, lim(2, Buy, 3, 5)), Err(InvalidLot));
    // bad tick + out of band + duplicate
    assert_eq!(check(&e, lim(1, Buy, 3, 10)), Err(InvalidTick));
    // out of band + duplicate
    assert_eq!(check(&e, lim(1, Buy, 200, 10)), Err(PriceBand));
    // duplicate + post-only crossing + FOK unfillable
    assert_eq!(
        check(&e, lim(1, Buy, 100, 50).post_only().with_tif(Fok)),
        Err(DuplicateOrderId)
    );
    // post-only crossing + FOK unfillable
    assert_eq!(
        check(&e, lim(2, Buy, 100, 50).post_only().with_tif(Fok)),
        Err(WouldTake)
    );
    // market into an empty side + FOK
    assert_eq!(
        check(&e, NewOrder::market(3, Sell, 10).with_tif(Fok)),
        Err(NoLiquidity)
    );
    assert_eq!(
        check(&e, NewOrder::market(3, Buy, 20).with_tif(Fok)),
        Err(FokUnfillable)
    );
    assert_eq!(
        check(&e, NewOrder::market(3, Buy, 10).with_tif(Fok)),
        Ok(())
    );
    assert_eq!(check(&e, lim(4, Buy, 95, 10)), Ok(()));
    // validate() is read-only, and process() agrees with it.
    assert_eq!(resting(&e), before);
    assert_eq!(
        run(&mut e, lim(2, Buy, 100, 50).post_only().with_tif(Fok)),
        rejected(2, WouldTake)
    );
    // A market order's kind skips the tick and band checks.
    let order = NewOrder {
        kind: OrderKind::Market,
        tif: Gtc,
        ..lim(5, Buy, 0, 10)
    };
    assert_eq!(check(&e, order), Ok(()));
    assert_eq!(
        run(&mut e, order).len(),
        2,
        "GTC market order fills (and would never rest)"
    );
}

#[test]
fn w08_fillable_matches_brute_force_on_random_books() {
    let mut rng = Rng::new(8);
    for round in 0..200 {
        let mut e = Engine::new(InstrumentSpec::default());
        let mut id = 0;
        for _ in 0..rng.range(0, 60) {
            id += 1;
            let side = rng.side();
            let price = if side == Buy {
                rng.range_i64(80, 99)
            } else {
                rng.range_i64(101, 120)
            };
            run(&mut e, lim(id, side, price, rng.range(1, 9)));
        }
        let all = resting(&e);
        for _ in 0..20 {
            let side = rng.side();
            let limit = if rng.chance(0.2) {
                None
            } else {
                Some(Price(rng.range_i64(75, 125)))
            };
            let want = Qty(rng.range(0, 150));
            let brute: u64 = all
                .iter()
                .filter(|o| {
                    o.side == side.opposite() && limit.is_none_or(|l| side.crosses(l, o.price))
                })
                .map(|o| o.qty.0)
                .sum();
            assert_eq!(
                e.fillable(side, limit, want),
                Qty(brute.min(want.0)),
                "round {round}"
            );

            // FOK agrees with fillable(); a killed FOK changes nothing.
            if want.is_zero() {
                continue;
            }
            let mut probe = e.clone();
            id += 1;
            let order = match limit {
                Some(p) => lim(id, side, p.0, want.0).with_tif(Fok),
                None => NewOrder::market(id, side, want.0).with_tif(Fok),
            };
            let out = run(&mut probe, order);
            if brute >= want.0 {
                let filled: u64 = out
                    .iter()
                    .map(|ev| {
                        if let Event::Trade { qty, .. } = ev {
                            qty.0
                        } else {
                            0
                        }
                    })
                    .sum();
                assert_eq!(filled, want.0, "round {round}: feasible FOK must fill");
            } else {
                assert!(matches!(
                    out[0],
                    Event::Rejected {
                        reason: FokUnfillable | NoLiquidity,
                        ..
                    }
                ));
                assert_eq!(resting(&probe), all);
            }
        }
    }
}
