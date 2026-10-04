//! Week 15 — pre-trade risk checks, exposure tracking and kill switches.

mod common;

use risk::{Exposure, Fill, Limits, RiskEngine};
use types::{
    AccountId, Event, InstrumentId, NewOrder, OrderId, Price, Qty, RejectReason, RiskReason, Rng,
    Side, TimeInForce,
};

const A: AccountId = AccountId(1);
const B: AccountId = AccountId(2);
const I1: InstrumentId = InstrumentId(1);
const I2: InstrumentId = InstrumentId(2);
const REF: Price = Price(1_000);

fn risk(r: RiskReason) -> Result<(), RejectReason> {
    Err(RejectReason::Risk(r))
}

fn buy(id: u64, price: i64, qty: u64) -> NewOrder {
    NewOrder::limit(id, Side::Buy, price, qty)
}

fn sell(id: u64, price: i64, qty: u64) -> NewOrder {
    NewOrder::limit(id, Side::Sell, price, qty)
}

fn exposure(position: i64, open_buy: u64, open_sell: u64) -> Exposure {
    Exposure {
        position,
        open_buy: Qty(open_buy),
        open_sell: Qty(open_sell),
    }
}

fn trade(taker: u64, maker: u64, taker_side: Side, price: i64, qty: u64) -> Event {
    Event::Trade {
        taker: OrderId(taker),
        maker: OrderId(maker),
        taker_side,
        price: Price(price),
        qty: Qty(qty),
    }
}

#[test]
fn w15_limits_reject_in_documented_order() {
    let mut r = RiskEngine::new(Limits::UNLIMITED);
    r.set_limits(
        A,
        Limits {
            max_order_qty: Qty(100),
            max_notional: 60_000,
            max_position: Qty(500),
            price_band_ticks: 10,
        },
    );
    assert_eq!(r.limits(A).max_order_qty, Qty(100));
    assert_eq!(
        r.limits(B),
        Limits::UNLIMITED,
        "unconfigured accounts use the default"
    );

    assert_eq!(r.check(&buy(1, 1_000, 50), REF), Ok(()));
    // Size beats band beats notional beats position.
    assert_eq!(
        r.check(&buy(2, 5_000, 101), REF),
        risk(RiskReason::MaxOrderQty)
    );
    assert_eq!(
        r.check(&buy(3, 1_011, 100), REF),
        Err(RejectReason::PriceBand)
    );
    assert_eq!(
        r.check(&buy(4, 1_010, 60), REF),
        risk(RiskReason::MaxNotional),
        "60_600 > 60_000"
    );
    assert_eq!(
        r.check(&buy(5, 1_000, 60), REF),
        Ok(()),
        "60_000 is exactly the limit"
    );
    r.set_position(A, I1, 480);
    assert_eq!(
        r.check(&buy(6, 1_000, 21), REF),
        risk(RiskReason::PositionLimit)
    );
    assert_eq!(r.check(&buy(7, 1_000, 20), REF), Ok(()));
    // Kill switches beat everything.
    r.kill_account(A);
    assert_eq!(
        r.check(&buy(8, 5_000, 101), REF),
        risk(RiskReason::KillSwitch)
    );
    // The default limits apply to other accounts.
    assert_eq!(
        r.check(&buy(9, 5_000, 1_000_000).with_account(2), REF),
        Ok(())
    );

    let blocked = RiskEngine::new(Limits::BLOCKED);
    assert_eq!(
        blocked.check(&buy(10, 1_000, 1), REF),
        risk(RiskReason::MaxOrderQty)
    );
}

#[test]
fn w15_projected_position_counts_same_side_open_orders() {
    let mut r = RiskEngine::new(Limits {
        max_position: Qty(100),
        ..Limits::UNLIMITED
    });

    // Open buys count against further buys (in flight, before any ack).
    r.on_new(&buy(1, 1_000, 60));
    assert_eq!(r.exposure(A, I1), exposure(0, 60, 0));
    assert_eq!(
        r.check(&buy(2, 1_000, 40), REF),
        Ok(()),
        "0 + 60 + 40 = 100"
    );
    assert_eq!(
        r.check(&buy(2, 1_000, 41), REF),
        risk(RiskReason::PositionLimit)
    );
    // Opposite-side open orders are never credited.
    r.on_new(&sell(3, 1_010, 50));
    assert_eq!(
        r.check(&buy(4, 1_000, 41), REF),
        risk(RiskReason::PositionLimit)
    );
    assert_eq!(
        r.check(&sell(4, 1_000, 50), REF),
        Ok(()),
        "0 - 50 - 50 = -100"
    );
    assert_eq!(
        r.check(&sell(4, 1_000, 51), REF),
        risk(RiskReason::PositionLimit)
    );

    // A fill moves quantity from open to position: the buy side is unchanged,
    // the sell side gains room.
    let f = r.on_event(&trade(1, 99, Side::Buy, 1_000, 60));
    assert!(f[0].is_some() && f[1].is_none());
    assert_eq!(r.exposure(A, I1), exposure(60, 0, 50));
    assert_eq!(r.check(&buy(5, 1_000, 40), REF), Ok(()));
    assert_eq!(
        r.check(&buy(5, 1_000, 41), REF),
        risk(RiskReason::PositionLimit)
    );
    assert_eq!(
        r.check(&sell(5, 1_000, 110), REF),
        Ok(()),
        "60 - 50 - 110 = -100"
    );
    assert_eq!(
        r.check(&sell(5, 1_000, 111), REF),
        risk(RiskReason::PositionLimit)
    );

    // Over the limit (e.g. limits were lowered): reducing orders still pass.
    r.set_position(A, I1, 200);
    assert_eq!(
        r.check(&buy(6, 1_000, 1), REF),
        risk(RiskReason::PositionLimit)
    );
    assert_eq!(
        r.check(&sell(6, 1_000, 150), REF),
        Ok(()),
        "200 - 50 - 150 = 0"
    );
    assert_eq!(
        r.check(&sell(6, 1_000, 250), REF),
        Ok(()),
        "200 - 50 - 250 = -100"
    );
    assert_eq!(
        r.check(&sell(6, 1_000, 251), REF),
        risk(RiskReason::PositionLimit)
    );

    // Each instrument has its own exposure.
    let mut other = buy(7, 1_000, 100);
    other.instrument = I2;
    assert_eq!(r.check(&other, REF), Ok(()));
    other.qty = Qty(101);
    assert_eq!(r.check(&other, REF), risk(RiskReason::PositionLimit));
}

#[test]
fn w15_events_update_open_qty_and_position() {
    let mut r = RiskEngine::new(Limits::UNLIMITED);
    assert_eq!(r.exposure(A, I1), Exposure::default());

    // Order 1: buy 100, accepted, partially filled as taker, rests, reduced, cancelled.
    r.on_new(&buy(1, 1_000, 100));
    assert_eq!(
        r.on_event(&Event::Accepted { id: OrderId(1) }),
        [None, None]
    );
    let fills = r.on_event(&trade(1, 50, Side::Buy, 999, 30));
    let want = Fill {
        order: OrderId(1),
        account: A,
        instrument: I1,
        side: Side::Buy,
        price: Price(999),
        qty: Qty(30),
    };
    assert_eq!(fills, [Some(want), None]);
    assert_eq!(r.exposure(A, I1), exposure(30, 70, 0));
    r.on_event(&Event::Rested {
        id: OrderId(1),
        side: Side::Buy,
        price: Price(1_000),
        qty: Qty(70),
    });
    assert_eq!(r.exposure(A, I1), exposure(30, 70, 0));
    r.on_event(&Event::Reduced {
        id: OrderId(1),
        remaining: Qty(25),
    });
    assert_eq!(r.exposure(A, I1), exposure(30, 25, 0));
    // Now filled as maker by someone else's sell.
    let fills = r.on_event(&trade(77, 1, Side::Sell, 1_000, 5));
    assert_eq!(fills[0], None);
    assert_eq!(
        fills[1].map(|f| (f.order, f.side, f.qty)),
        Some((OrderId(1), Side::Buy, Qty(5)))
    );
    assert_eq!(r.exposure(A, I1), exposure(35, 20, 0));
    r.on_event(&Event::Cancelled {
        id: OrderId(1),
        remaining: Qty(20),
    });
    assert_eq!(r.exposure(A, I1), exposure(35, 0, 0));
    assert_eq!(r.open_order_count(), 0);

    // Order 2: IOC sell, filled 10 of 40, rest expires.
    r.on_new(&sell(2, 990, 40).with_tif(TimeInForce::Ioc));
    assert_eq!(r.exposure(A, I1), exposure(35, 0, 40));
    r.on_event(&Event::Accepted { id: OrderId(2) });
    r.on_event(&trade(2, 60, Side::Sell, 995, 10));
    r.on_event(&Event::Expired {
        id: OrderId(2),
        remaining: Qty(30),
    });
    assert_eq!(r.exposure(A, I1), exposure(25, 0, 0));

    // Order 3: rejected by the engine (e.g. post-only would take).
    r.on_new(&buy(3, 1_000, 15).post_only());
    assert_eq!(r.exposure(A, I1), exposure(25, 15, 0));
    r.on_event(&Event::Rejected {
        id: OrderId(3),
        reason: RejectReason::WouldTake,
    });
    assert_eq!(r.exposure(A, I1), exposure(25, 0, 0));

    // Order 4: fully filled in two trades; no event needed afterwards.
    r.on_new(&sell(4, 1_000, 25));
    r.on_event(&Event::Accepted { id: OrderId(4) });
    r.on_event(&trade(4, 61, Side::Sell, 1_000, 20));
    r.on_event(&trade(4, 62, Side::Sell, 1_000, 5));
    assert_eq!(r.exposure(A, I1), exposure(0, 0, 0));
    assert_eq!(r.open_order_count(), 0);
}

#[test]
fn w15_kill_switches_global_and_per_account() {
    let mut r = RiskEngine::new(Limits::UNLIMITED);
    let ops = r.global_kill_switch();
    assert!(!ops.is_engaged());

    // Engaged from another thread, seen by the engine thread.
    std::thread::spawn(move || ops.engage()).join().unwrap();
    assert!(r.is_killed(A) && r.is_killed(B));
    assert_eq!(
        r.check(&buy(1, 1_000, 1), REF),
        risk(RiskReason::KillSwitch)
    );
    assert_eq!(
        r.check(&buy(1, 1_000, 1).with_account(2), REF),
        risk(RiskReason::KillSwitch)
    );
    r.global_kill_switch().disengage();
    assert_eq!(r.check(&buy(1, 1_000, 1), REF), Ok(()));

    // Per account: only that account is blocked; set_limits does not revive it.
    r.on_new(&buy(1, 1_000, 5));
    r.on_new(&buy(2, 1_000, 5).with_account(2));
    r.on_new(&sell(3, 1_010, 5));
    r.kill_account(A);
    r.set_limits(A, Limits::UNLIMITED);
    assert!(r.is_killed(A) && !r.is_killed(B));
    assert_eq!(
        r.check(&sell(4, 1_000, 1), REF),
        risk(RiskReason::KillSwitch)
    );
    assert_eq!(r.check(&sell(4, 1_000, 1).with_account(2), REF), Ok(()));
    // What the gateway cancels after a kill.
    assert_eq!(r.open_orders(A), vec![OrderId(1), OrderId(3)]);
    assert_eq!(r.open_orders(B), vec![OrderId(2)]);
    r.revive_account(A);
    assert_eq!(r.check(&sell(4, 1_000, 1), REF), Ok(()));
    // Reviving an account that was never killed is a no-op.
    r.revive_account(AccountId(9));
    assert!(!r.is_killed(AccountId(9)));
}

#[test]
fn w15_notional_and_band_edge_cases() {
    let mut r = RiskEngine::new(Limits::UNLIMITED);

    // u128 notional: i64::MAX ticks × u64::MAX lots does not overflow.
    let exact = i64::MAX as u128 * u64::MAX as u128;
    r.set_limits(
        A,
        Limits {
            max_notional: exact,
            ..Limits::UNLIMITED
        },
    );
    let huge = buy(1, i64::MAX, u64::MAX);
    assert_eq!(r.check(&huge, Price(i64::MAX)), Ok(()));
    r.set_limits(
        A,
        Limits {
            max_notional: exact - 1,
            ..Limits::UNLIMITED
        },
    );
    assert_eq!(
        r.check(&huge, Price(i64::MAX)),
        risk(RiskReason::MaxNotional)
    );
    // Negative prices count by magnitude.
    r.set_limits(
        A,
        Limits {
            max_notional: 1_000,
            ..Limits::UNLIMITED
        },
    );
    assert_eq!(r.check(&sell(2, -100, 10), Price(-100)), Ok(()));
    assert_eq!(
        r.check(&sell(2, -101, 10), Price(-100)),
        risk(RiskReason::MaxNotional)
    );

    // Market orders: no band, notional at the reference price.
    r.set_limits(
        A,
        Limits {
            max_notional: 10_000,
            price_band_ticks: 5,
            ..Limits::UNLIMITED
        },
    );
    assert_eq!(r.check(&NewOrder::market(3, Side::Buy, 10), REF), Ok(()));
    assert_eq!(
        r.check(&NewOrder::market(3, Side::Buy, 11), REF),
        risk(RiskReason::MaxNotional)
    );

    // Band boundaries, both sides of the reference, including negative prices.
    r.set_limits(
        A,
        Limits {
            price_band_ticks: 5,
            ..Limits::UNLIMITED
        },
    );
    for (price, ok) in [(995, true), (1_005, true), (994, false), (1_006, false)] {
        let want = if ok {
            Ok(())
        } else {
            Err(RejectReason::PriceBand)
        };
        assert_eq!(r.check(&buy(4, price, 1), REF), want, "price {price}");
        assert_eq!(r.check(&sell(4, price, 1), REF), want, "price {price}");
    }
    assert_eq!(r.check(&buy(5, -3, 1), Price(2)), Ok(()));
    assert_eq!(
        r.check(&buy(5, -4, 1), Price(2)),
        Err(RejectReason::PriceBand)
    );
    // Extreme distance does not overflow.
    assert_eq!(
        r.check(&buy(5, i64::MAX, 1), Price(i64::MIN)),
        Err(RejectReason::PriceBand)
    );
    // Band 0 disables the check.
    r.set_limits(A, Limits::UNLIMITED);
    assert_eq!(r.check(&buy(6, i64::MAX, 1), Price(i64::MIN)), Ok(()));
}

#[test]
fn w15_duplicate_and_unknown_ids_are_harmless() {
    let mut r = RiskEngine::new(Limits::UNLIMITED);

    // Unknown orders: ignored, no fills.
    assert_eq!(
        r.on_event(&trade(500, 501, Side::Buy, 1_000, 10)),
        [None, None]
    );
    r.on_event(&Event::Cancelled {
        id: OrderId(502),
        remaining: Qty(3),
    });
    assert_eq!(r.exposure(A, I1), Exposure::default());

    // A second order reusing a live id: on_new ignores it, and the engine's
    // DuplicateOrderId reject does not release the original.
    r.on_new(&buy(1, 1_000, 10));
    r.on_event(&Event::Accepted { id: OrderId(1) });
    r.on_new(&buy(1, 1_000, 99));
    assert_eq!(r.exposure(A, I1), exposure(0, 10, 0));
    r.on_event(&Event::Rejected {
        id: OrderId(1),
        reason: RejectReason::DuplicateOrderId,
    });
    assert_eq!(r.exposure(A, I1), exposure(0, 10, 0));

    // Self-trade: both legs tracked for the same account; the position nets out.
    r.on_new(&sell(2, 1_000, 4));
    let fills = r.on_event(&trade(2, 1, Side::Sell, 1_000, 4));
    assert_eq!(
        fills.map(|f| f.map(|f| (f.order, f.side))),
        [
            Some((OrderId(2), Side::Sell)),
            Some((OrderId(1), Side::Buy))
        ]
    );
    assert_eq!(r.exposure(A, I1), exposure(0, 6, 0));

    // An id reused after its order finished: tracked anew, and the
    // duplicate reject (never acknowledged) releases it.
    r.on_new(&sell(2, 1_000, 7));
    assert_eq!(r.exposure(A, I1), exposure(0, 6, 7));
    r.on_event(&Event::Rejected {
        id: OrderId(2),
        reason: RejectReason::DuplicateOrderId,
    });
    assert_eq!(r.exposure(A, I1), exposure(0, 6, 0));
    assert_eq!(r.open_order_count(), 1);
}

#[test]
fn w15_check_and_events_never_allocate() {
    let mut r = RiskEngine::with_capacity(
        Limits {
            max_position: Qty(1_000),
            ..Limits::UNLIMITED
        },
        64,
        1_024,
    );
    r.set_limits(
        B,
        Limits {
            price_band_ticks: 50,
            ..Limits::UNLIMITED
        },
    );
    let mut rng = Rng::new(15);
    let orders: Vec<NewOrder> = (0..512)
        .map(|i| {
            let mut o =
                NewOrder::limit(i, rng.side(), rng.range_i64(900, 1_100), rng.range(1, 400));
            o.account = AccountId(rng.range(1, 8) as u32);
            o
        })
        .collect();
    for o in &orders[..256] {
        r.on_new(o);
    }
    let mut passed = 0;
    let n = common::allocations_in(|| {
        for _ in 0..20 {
            for o in &orders {
                passed += r.check(o, REF).is_ok() as u32;
            }
        }
        // Event bookkeeping for known orders does not allocate either.
        for o in &orders[..128] {
            r.on_event(&Event::Accepted { id: o.id });
            r.on_event(&trade(o.id.0, 9_999, o.side, 1_000, 1));
            r.on_event(&Event::Cancelled {
                id: o.id,
                remaining: Qty(0),
            });
        }
    });
    assert_eq!(n, 0, "check / on_event allocated");
    assert!(
        passed > 0 && passed < 20 * 512,
        "the stream should mix passes and rejects ({passed})"
    );
}

/// A naive model of the documented rules, recomputing every exposure from
/// the full history of orders and fills.
#[derive(Clone, Copy)]
struct Live {
    id: OrderId,
    account: AccountId,
    instrument: InstrumentId,
    side: Side,
    remaining: u64,
}

fn model_exposure(
    live: &[Live],
    fills: &[(AccountId, InstrumentId, i64)],
    a: AccountId,
    i: InstrumentId,
) -> Exposure {
    let mut e = Exposure {
        position: fills
            .iter()
            .filter(|f| f.0 == a && f.1 == i)
            .map(|f| f.2)
            .sum(),
        ..Exposure::default()
    };
    for o in live.iter().filter(|o| o.account == a && o.instrument == i) {
        match o.side {
            Side::Buy => e.open_buy.0 += o.remaining,
            Side::Sell => e.open_sell.0 += o.remaining,
        }
    }
    e
}

fn model_check(
    order: &NewOrder,
    reference: Price,
    limits: &Limits,
    killed: bool,
    e: Exposure,
) -> Result<(), RejectReason> {
    if killed {
        return risk(RiskReason::KillSwitch);
    }
    if order.qty.0 > limits.max_order_qty.0 {
        return risk(RiskReason::MaxOrderQty);
    }
    let price = match order.limit_price() {
        Some(p) => {
            if limits.price_band_ticks > 0
                && (p.0 - reference.0).unsigned_abs() > limits.price_band_ticks
            {
                return Err(RejectReason::PriceBand);
            }
            p
        }
        None => reference,
    };
    if price.0.unsigned_abs() as u128 * order.qty.0 as u128 > limits.max_notional {
        return risk(RiskReason::MaxNotional);
    }
    let (pos, q, max) = (
        e.position as i128,
        order.qty.0 as i128,
        limits.max_position.0 as i128,
    );
    let ok = match order.side {
        Side::Buy => pos + e.open_buy.0 as i128 + q <= max,
        Side::Sell => pos - e.open_sell.0 as i128 - q >= -max,
    };
    if ok {
        Ok(())
    } else {
        risk(RiskReason::PositionLimit)
    }
}

/// Applies a trade between live orders `t` (taker) and `m` (maker) to the
/// model and the engine, checking the fills the engine reports.
fn do_trade(
    r: &mut RiskEngine,
    live: &mut [Live],
    fills: &mut Vec<(AccountId, InstrumentId, i64)>,
    t: usize,
    m: usize,
    qty: u64,
) {
    let (taker, maker) = (live[t], live[m]);
    let got = r.on_event(&trade(taker.id.0, maker.id.0, taker.side, 1_000, qty));
    for (leg, o) in got.iter().zip([taker, maker]) {
        let f = leg.expect("both legs are tracked");
        assert_eq!(
            (f.order, f.account, f.instrument, f.side, f.qty),
            (o.id, o.account, o.instrument, o.side, Qty(qty))
        );
        let signed = if o.side == Side::Buy {
            qty as i64
        } else {
            -(qty as i64)
        };
        fills.push((o.account, o.instrument, signed));
    }
    live[t].remaining -= qty;
    live[m].remaining -= qty;
}

#[test]
fn w15_randomized_exposure_matches_recomputation() {
    let accounts = [AccountId(1), AccountId(2), AccountId(3), AccountId(4)];
    let instruments = [I1, I2];
    let limits = [
        Limits {
            max_order_qty: Qty(40),
            max_position: Qty(120),
            price_band_ticks: 15,
            ..Limits::UNLIMITED
        },
        Limits {
            max_notional: 30_000,
            max_position: Qty(200),
            ..Limits::UNLIMITED
        },
        Limits {
            max_order_qty: Qty(25),
            max_notional: 25_000,
            max_position: Qty(60),
            price_band_ticks: 8,
        },
        Limits::UNLIMITED,
    ];
    let mut r = RiskEngine::new(Limits::UNLIMITED);
    for (a, l) in accounts.iter().zip(&limits) {
        r.set_limits(*a, *l);
    }
    let mut rng = Rng::new(0x15);
    let mut live: Vec<Live> = Vec::new();
    let mut fills: Vec<(AccountId, InstrumentId, i64)> = Vec::new();
    let mut killed = [false; 4];
    let mut next_id = 1u64;
    let (mut passed, mut rejected) = (0, 0);
    let mut reasons = std::collections::BTreeMap::new();

    for step in 0..4_000 {
        match rng.range(0, 9) {
            0..=3 => {
                let ai = rng.range(0, 3) as usize;
                let mut o = if rng.chance(0.1) {
                    NewOrder::market(next_id, rng.side(), rng.range(1, 50))
                } else {
                    NewOrder::limit(
                        next_id,
                        rng.side(),
                        rng.range_i64(980, 1_020),
                        rng.range(1, 50),
                    )
                };
                next_id += 1;
                o.account = accounts[ai];
                o.instrument = instruments[rng.range(0, 1) as usize];
                let reference = Price(rng.range_i64(995, 1_005));
                let e = model_exposure(&live, &fills, o.account, o.instrument);
                let want = model_check(&o, reference, &limits[ai], killed[ai], e);
                assert_eq!(
                    r.check(&o, reference),
                    want,
                    "step {step}: {o:?} with {e:?}"
                );
                if let Err(why) = want {
                    *reasons.entry(format!("{why:?}")).or_insert(0) += 1;
                    rejected += 1;
                    continue;
                }
                passed += 1;
                r.on_new(&o);
                live.push(Live {
                    id: o.id,
                    account: o.account,
                    instrument: o.instrument,
                    side: o.side,
                    remaining: o.qty.0,
                });
                // The toy engine: sometimes rejects, else accepts, maybe
                // trades against a resting opposite order, then rests or expires.
                if rng.chance(0.05) {
                    r.on_event(&Event::Rejected {
                        id: o.id,
                        reason: RejectReason::WouldTake,
                    });
                    live.pop();
                    continue;
                }
                r.on_event(&Event::Accepted { id: o.id });
                let t = live.len() - 1;
                if let Some(m) = live.iter().position(|x| {
                    x.instrument == o.instrument && x.side != o.side && x.remaining > 0
                }) {
                    let qty = live[t].remaining.min(live[m].remaining);
                    do_trade(&mut r, &mut live, &mut fills, t, m, qty);
                }
                let rem = live[t].remaining;
                if rem > 0 && o.limit_price().is_none() {
                    r.on_event(&Event::Expired {
                        id: o.id,
                        remaining: Qty(rem),
                    });
                    live[t].remaining = 0;
                } else if rem > 0 {
                    r.on_event(&Event::Rested {
                        id: o.id,
                        side: o.side,
                        price: REF,
                        qty: Qty(rem),
                    });
                }
            }
            4 if !live.is_empty() => {
                let k = rng.range(0, live.len() as u64 - 1) as usize;
                r.on_event(&Event::Cancelled {
                    id: live[k].id,
                    remaining: Qty(live[k].remaining),
                });
                live[k].remaining = 0;
            }
            5 if !live.is_empty() => {
                let k = rng.range(0, live.len() as u64 - 1) as usize;
                if live[k].remaining > 1 {
                    let to = rng.range(1, live[k].remaining - 1);
                    r.on_event(&Event::Reduced {
                        id: live[k].id,
                        remaining: Qty(to),
                    });
                    live[k].remaining = to;
                }
            }
            6..=8 if live.len() >= 2 => {
                let t = rng.range(0, live.len() as u64 - 1) as usize;
                let found = live
                    .iter()
                    .position(|x| x.instrument == live[t].instrument && x.side != live[t].side);
                if let Some(m) = found {
                    let qty = rng.range(1, live[t].remaining.min(live[m].remaining));
                    do_trade(&mut r, &mut live, &mut fills, t, m, qty);
                }
            }
            9 => {
                let ai = rng.range(0, 3) as usize;
                if killed[ai] {
                    killed[ai] = false;
                    r.revive_account(accounts[ai]);
                } else if rng.chance(0.2) {
                    killed[ai] = true;
                    r.kill_account(accounts[ai]);
                }
            }
            _ => {}
        }
        live.retain(|o| o.remaining > 0);
        assert_eq!(r.open_order_count(), live.len(), "step {step}");
        for &a in &accounts {
            for &i in &instruments {
                assert_eq!(
                    r.exposure(a, i),
                    model_exposure(&live, &fills, a, i),
                    "step {step}: {a:?} {i:?}"
                );
            }
        }
    }
    // The stream exercises every rule, not just the easy ones.
    assert!(
        passed > 500 && fills.len() > 500,
        "passed {passed}, {} fills",
        fills.len()
    );
    assert_eq!(reasons.len(), 5, "{reasons:?}");
    assert!(
        reasons.values().all(|&n| n >= 20),
        "{reasons:?} ({rejected} rejected)"
    );
}
