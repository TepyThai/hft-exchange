//! Week 45 — positions, PnL, exposure and post-trade limits.

mod common;

use risk::{AccountPnl, Breach, Fill, Limits, Position, Positions, PostTradeLimits, RiskEngine};
use types::{
    AccountId, Event, InstrumentId, NewOrder, OrderId, Price, Qty, RejectReason, RiskReason, Rng,
    Side,
};

const A: AccountId = AccountId(1);
const B: AccountId = AccountId(2);
const I1: InstrumentId = InstrumentId(1);
const I2: InstrumentId = InstrumentId(2);
const I3: InstrumentId = InstrumentId(3);

fn fill(account: AccountId, instrument: InstrumentId, side: Side, price: i64, qty: u64) -> Fill {
    Fill {
        order: OrderId(0),
        account,
        instrument,
        side,
        price: Price(price),
        qty: Qty(qty),
    }
}

/// (qty, cost basis, realized)
fn state(p: &Position) -> (i64, i128, i128) {
    (p.qty(), p.cost_basis(), p.realized())
}

#[test]
fn w45_open_add_and_reduce_realize_against_average_cost() {
    let mut p = Position::new();
    assert_eq!(state(&p), (0, 0, 0));
    assert_eq!(p.avg_price(), None);
    assert_eq!(p.apply(Side::Buy, Price(100), Qty(10)), 0);
    assert_eq!(p.apply(Side::Buy, Price(110), Qty(10)), 0);
    assert_eq!(state(&p), (20, 2_100, 0));
    assert_eq!(p.avg_price(), Some(Price(105)));
    assert_eq!(p.unrealized(Price(105)), 0);

    // Reduce: 5 × (120 − 105) = 75; the average is unchanged.
    assert_eq!(p.apply(Side::Sell, Price(120), Qty(5)), 75);
    assert_eq!(state(&p), (15, 1_575, 75));
    assert_eq!(p.avg_price(), Some(Price(105)));
    assert_eq!(p.unrealized(Price(100)), -75);

    // Close: 15 × (100 − 105) = −75; flat, round trip = cash flow = 0.
    assert_eq!(p.apply(Side::Sell, Price(100), Qty(15)), -75);
    assert_eq!(state(&p), (0, 0, 0));
    assert_eq!(p.avg_price(), None);
    assert_eq!(p.unrealized(Price(1_000)), 0);
    // A zero-quantity fill changes nothing.
    assert_eq!(p.apply(Side::Sell, Price(50), Qty(0)), 0);
    assert_eq!(state(&p), (0, 0, 0));
}

#[test]
fn w45_flips_through_zero_close_then_reopen_at_fill_price() {
    let mut p = Position::new();
    p.apply(Side::Buy, Price(100), Qty(5));
    // Long 5 → short 3: close 5 (+50), open 3 short at 110.
    assert_eq!(p.apply(Side::Sell, Price(110), Qty(8)), 50);
    assert_eq!(state(&p), (-3, -330, 50));
    assert_eq!(p.avg_price(), Some(Price(110)));
    assert_eq!(p.unrealized(Price(100)), 30);
    // Short 3 → long 4: close 3 (+60), open 4 long at 90.
    assert_eq!(p.apply(Side::Buy, Price(90), Qty(7)), 60);
    assert_eq!(state(&p), (4, 360, 110));
    assert_eq!(p.avg_price(), Some(Price(90)));
    assert_eq!(p.unrealized(Price(95)), 20);
    // Flip after a partial reduction left a rounding remainder in the basis:
    // the full close releases all of it, the new side starts clean.
    let mut q = Position::new();
    q.apply(Side::Buy, Price(100), Qty(1));
    q.apply(Side::Buy, Price(101), Qty(2));
    q.apply(Side::Sell, Price(100), Qty(1));
    assert_eq!(
        q.apply(Side::Sell, Price(103), Qty(5)),
        4,
        "close 2 lots with basis 202 at 103"
    );
    assert_eq!(state(&q), (-3, -309, 4));
    assert_eq!(q.realized(), (-100 - 202 + 100 + 206) as i128);
}

#[test]
fn w45_short_and_negative_price_positions() {
    let mut p = Position::new();
    p.apply(Side::Sell, Price(50), Qty(10));
    assert_eq!(state(&p), (-10, -500, 0));
    assert_eq!(p.avg_price(), Some(Price(50)));
    assert_eq!(
        p.unrealized(Price(40)),
        100,
        "short gains when the price falls"
    );
    assert_eq!(p.unrealized(Price(60)), -100);
    assert_eq!(p.apply(Side::Buy, Price(45), Qty(4)), 20);
    assert_eq!(state(&p), (-6, -300, 20));
    assert_eq!(
        p.apply(Side::Sell, Price(56), Qty(2)),
        0,
        "adding to a short realizes nothing"
    );
    assert_eq!(state(&p), (-8, -412, 20));

    // Negative prices (spreads, some futures) are just numbers.
    let mut n = Position::new();
    n.apply(Side::Buy, Price(-10), Qty(2));
    assert_eq!(state(&n), (2, -20, 0));
    assert_eq!(n.avg_price(), Some(Price(-10)));
    assert_eq!(n.unrealized(Price(-5)), 10);
    assert_eq!(n.apply(Side::Sell, Price(-4), Qty(2)), 12);
    assert_eq!(state(&n), (0, 0, 12));
}

#[test]
fn w45_cost_basis_keeps_the_rounding_remainder() {
    // Long: basis 302 for 3 lots (avg 100.67, displayed 100).
    let mut p = Position::new();
    p.apply(Side::Buy, Price(100), Qty(1));
    p.apply(Side::Buy, Price(101), Qty(2));
    assert_eq!(p.avg_price(), Some(Price(100)), "display average truncates");
    // 302 × 1/3 = 100.67 → 100 released; the 0.67 stays in the basis.
    assert_eq!(p.apply(Side::Sell, Price(102), Qty(1)), 2);
    assert_eq!(state(&p), (2, 202, 2));
    assert_eq!(p.apply(Side::Sell, Price(102), Qty(1)), 1);
    assert_eq!(p.apply(Side::Sell, Price(102), Qty(1)), 1);
    assert_eq!(state(&p), (0, 0, 4), "flat: exactly 3 × 102 − 302");

    // Short: truncation is toward zero as well.
    let mut s = Position::new();
    s.apply(Side::Sell, Price(100), Qty(1));
    s.apply(Side::Sell, Price(101), Qty(2));
    assert_eq!(s.avg_price(), Some(Price(100)));
    assert_eq!(
        s.apply(Side::Buy, Price(99), Qty(1)),
        1,
        "−302 × 1/3 → −100 released"
    );
    assert_eq!(state(&s), (-2, -202, 1));
    assert_eq!(s.apply(Side::Buy, Price(99), Qty(2)), 4);
    assert_eq!(state(&s), (0, 0, 5), "flat: exactly 302 − 3 × 99");

    // Large values stay exact in i128.
    let mut big = Position::new();
    big.apply(Side::Buy, Price(i64::MAX), Qty(i64::MAX as u64));
    assert_eq!(big.cost_basis(), i64::MAX as i128 * i64::MAX as i128);
    assert_eq!(big.apply(Side::Sell, Price(i64::MAX), Qty(1)), 0);
    assert_eq!(big.unrealized(Price(i64::MAX)), 0);
}

#[test]
fn w45_exposure_aggregates_per_account_at_marks() {
    let mut book = Positions::new(PostTradeLimits::UNLIMITED);
    assert_eq!(book.account(A), None);
    assert_eq!(book.mark(I1), None);
    book.apply_fill(&fill(A, I1, Side::Buy, 100, 10)).unwrap();
    assert_eq!(
        book.mark(I1),
        Some(Price(100)),
        "first fill marks an unmarked instrument"
    );
    book.apply_fill(&fill(A, I2, Side::Sell, 50, 20)).unwrap();
    book.apply_fill(&fill(B, I1, Side::Sell, 101, 4)).unwrap();
    assert_eq!(
        book.mark(I1),
        Some(Price(100)),
        "later fills do not move the mark"
    );
    assert_eq!(
        book.account(A),
        Some(AccountPnl {
            realized: 0,
            unrealized: 0,
            gross_exposure: 1_000 + 1_000,
            net_exposure: 1_000 - 1_000
        })
    );
    assert_eq!(
        book.account(B).unwrap().unrealized,
        4,
        "short 4 at 101, marked at 100"
    );

    let mut breaches = Vec::new();
    book.set_mark(I1, Price(110), |a, b| breaches.push((a, b)));
    book.set_mark(I2, Price(45), |a, b| breaches.push((a, b)));
    book.set_mark(I3, Price(7), |a, b| breaches.push((a, b)));
    assert!(breaches.is_empty());
    assert_eq!(book.mark(I3), Some(Price(7)));
    // A: long 10 I1 @100 (mark 110) = +100; short 20 I2 @50 (mark 45) = +100.
    let a = book.account(A).unwrap();
    assert_eq!(
        a,
        AccountPnl {
            realized: 0,
            unrealized: 200,
            gross_exposure: 1_100 + 900,
            net_exposure: 1_100 - 900
        }
    );
    assert_eq!(a.total(), 200);
    book.apply_fill(&fill(A, I1, Side::Sell, 112, 4)).unwrap();
    let a = book.account(A).unwrap();
    assert_eq!(
        (a.realized, a.unrealized, a.gross_exposure),
        (48, 60 + 100, 660 + 900)
    );
    assert_eq!(book.position(A, I1).map(|p| state(&p)), Some((6, 600, 48)));
    assert_eq!(book.position(B, I2), None);
    // B: short 4 @ 101 at mark 110.
    assert_eq!(
        book.account(B).unwrap(),
        AccountPnl {
            realized: 0,
            unrealized: -36,
            gross_exposure: 440,
            net_exposure: -440
        }
    );
}

#[test]
fn w45_max_loss_breach_trips_the_kill_switch() {
    let mut risk = RiskEngine::new(Limits::UNLIMITED);
    let mut book = Positions::new(PostTradeLimits::UNLIMITED);
    book.set_limits(
        A,
        PostTradeLimits {
            max_loss: 1_000,
            ..PostTradeLimits::UNLIMITED
        },
    );

    // A buys 100 @ 50 from B through the risk engine's event stream.
    risk.on_new(&NewOrder::limit(1, Side::Buy, 50, 100));
    risk.on_new(&NewOrder::limit(2, Side::Sell, 50, 100).with_account(2));
    let trade = Event::Trade {
        taker: OrderId(1),
        maker: OrderId(2),
        taker_side: Side::Buy,
        price: Price(50),
        qty: Qty(100),
    };
    for f in risk.on_event(&trade).into_iter().flatten() {
        if book.apply_fill(&f).is_err() {
            risk.kill_account(f.account);
        }
    }
    assert_eq!(book.position(A, I1).map(|p| p.qty()), Some(100));
    assert_eq!(book.position(B, I1).map(|p| p.qty()), Some(-100));

    // The market drops 10 ticks: −1_000 is still within the limit...
    let mut tripped = Vec::new();
    book.set_mark(I1, Price(40), |a, b| tripped.push((a, b)));
    assert!(tripped.is_empty());
    assert_eq!(book.check_account(A), Ok(()));
    // ...one more tick is not.
    book.set_mark(I1, Price(39), |a, b| {
        tripped.push((a, b));
        risk.kill_account(a);
    });
    assert_eq!(tripped, vec![(A, Breach::MaxLoss)]);
    assert_eq!(book.check_account(A), Err(Breach::MaxLoss));
    assert_eq!(
        risk.check(&NewOrder::limit(3, Side::Sell, 39, 1), Price(39)),
        Err(RejectReason::Risk(RiskReason::KillSwitch))
    );
    assert_eq!(
        risk.check(
            &NewOrder::limit(3, Side::Sell, 39, 1).with_account(2),
            Price(39)
        ),
        Ok(())
    );

    // Realized losses count too: closing at 39 locks in −1_100.
    assert_eq!(
        book.apply_fill(&fill(A, I1, Side::Sell, 39, 100)),
        Err(Breach::MaxLoss)
    );
    assert_eq!(
        book.account(A).map(|a| (a.realized, a.unrealized)),
        Some((-1_100, 0))
    );
    // Unknown accounts pass.
    assert_eq!(book.check_account(AccountId(99)), Ok(()));
}

#[test]
fn w45_gross_exposure_breach_counts_both_sides() {
    let mut book = Positions::new(PostTradeLimits {
        max_loss: i128::MAX,
        max_gross_exposure: 10_000,
    });
    assert_eq!(book.apply_fill(&fill(A, I1, Side::Buy, 100, 50)), Ok(()));
    // A short adds to gross exposure even though it nets against the long.
    assert_eq!(
        book.apply_fill(&fill(A, I2, Side::Sell, 90, 50)),
        Ok(()),
        "5_000 + 4_500"
    );
    assert_eq!(
        book.apply_fill(&fill(A, I2, Side::Sell, 90, 6)),
        Err(Breach::MaxGrossExposure),
        "5_000 + 5_040"
    );
    let a = book.account(A).unwrap();
    assert_eq!((a.gross_exposure, a.net_exposure), (10_040, -40));
    // Reducing brings it back within the limit.
    assert_eq!(book.apply_fill(&fill(A, I1, Side::Sell, 100, 1)), Ok(()));
    // A mark move can breach it again; marks of other instruments cannot.
    let mut hits = Vec::new();
    book.set_mark(I3, Price(1_000_000), |a, b| hits.push((a, b)));
    assert!(hits.is_empty());
    book.set_mark(I1, Price(102), |a, b| hits.push((a, b)));
    assert_eq!(hits, vec![(A, Breach::MaxGrossExposure)]);
    // Per-account limits override the default.
    book.set_limits(A, PostTradeLimits::UNLIMITED);
    assert_eq!(book.check_account(A), Ok(()));
}

/// An independent replay of the documented rules over a full fill list:
/// close against the existing position first, then open the rest.
fn replay(fills: &[(Side, i64, u64)]) -> (i64, i128, i128) {
    let (mut qty, mut cost, mut realized) = (0i128, 0i128, 0i128);
    for &(side, price, q) in fills {
        let p = price as i128;
        let mut d = if side == Side::Buy {
            q as i128
        } else {
            -(q as i128)
        };
        if qty != 0 && (qty > 0) != (d > 0) {
            let sign = qty.signum();
            let close = d.abs().min(qty.abs());
            let share = if close == qty.abs() {
                cost
            } else {
                cost * close / qty.abs()
            };
            realized += sign * close * p - share;
            cost -= share;
            qty -= sign * close;
            d += sign * close;
        }
        qty += d;
        cost += d * p;
    }
    (qty as i64, cost, realized)
}

#[test]
fn w45_randomized_incremental_pnl_matches_recomputation() {
    let accounts = [AccountId(1), AccountId(2), AccountId(3)];
    let instruments = [I1, I2, I3];
    let mut rng = Rng::new(45);
    let mut book = Positions::new(PostTradeLimits::UNLIMITED);
    // Per (account, instrument): the full fill list, cash flow, the exact
    // (f64) average-cost model, and partial reductions since last flat.
    let mut history: Vec<Vec<(Side, i64, u64)>> = vec![Vec::new(); 9];
    let mut exact = [(0i64, 0f64, 0f64); 9]; // (qty, avg, realized)
    let mut partials = [0u32; 9];
    let mut marks: [Option<i64>; 3] = [None; 3];
    let (mut flips, mut reductions) = (0, 0);

    for step in 0..3_000 {
        let (ai, ii) = (rng.range(0, 2) as usize, rng.range(0, 2) as usize);
        let k = ai * 3 + ii;
        if rng.chance(0.15) {
            let m = rng.range_i64(-20, 400);
            book.set_mark(instruments[ii], Price(m), |_, _| panic!("unlimited"));
            marks[ii] = Some(m);
            continue;
        }
        let (side, price, qty) = (rng.side(), rng.range_i64(-20, 400), rng.range(1, 30));
        let f = fill(accounts[ai], instruments[ii], side, price, qty);
        assert_eq!(book.apply_fill(&f), Ok(()));
        marks[ii].get_or_insert(price);
        history[k].push((side, price, qty));
        let pos = book.position(f.account, f.instrument).unwrap();

        // 1. Exact equality with a from-scratch replay of the full list.
        assert_eq!(state(&pos), replay(&history[k]), "step {step} key {k}");

        // 2. Cash-flow identity, independent of rounding.
        let cash: i128 = history[k]
            .iter()
            .map(|&(s, p, q)| {
                if s == Side::Buy {
                    -(p as i128 * q as i128)
                } else {
                    p as i128 * q as i128
                }
            })
            .sum();
        let mark = marks[ii].unwrap();
        assert_eq!(
            pos.realized() + pos.unrealized(Price(mark)),
            cash + pos.qty() as i128 * mark as i128
        );
        if pos.qty() == 0 {
            assert_eq!(
                pos.realized(),
                cash,
                "flat: realized is exactly the cash flow"
            );
        }

        // 3. Within one tick × lot per partial reduction of the exact average-cost value.
        let (eq, avg, er) = &mut exact[k];
        let d = if side == Side::Buy {
            qty as i64
        } else {
            -(qty as i64)
        };
        if *eq != 0 && (*eq > 0) != (d > 0) {
            let close = d.abs().min(eq.abs());
            *er += close as f64 * (price as f64 - *avg) * eq.signum() as f64;
            if close < eq.abs() {
                partials[k] += 1;
                reductions += 1;
            } else if close < d.abs() {
                flips += 1;
            }
            let rest = d + close * eq.signum();
            *eq -= close * eq.signum();
            if *eq == 0 {
                partials[k] = 0;
                *eq = rest;
                *avg = price as f64;
            }
        } else {
            *avg = (*avg * eq.abs() as f64 + price as f64 * d.abs() as f64)
                / (eq.abs() + d.abs()) as f64;
            *eq += d;
        }
        assert_eq!(pos.qty(), *eq);
        assert!(
            (pos.realized() as f64 - *er).abs() <= partials[k] as f64 + 1e-6,
            "step {step}: {} vs {}",
            pos.realized(),
            er
        );

        // 4. Account aggregates equal the sums over the account's positions.
        for (aj, &acct) in accounts.iter().enumerate() {
            let mut want = AccountPnl::default();
            for (ij, &inst) in instruments.iter().enumerate() {
                if let Some(p) = book.position(acct, inst) {
                    let m = marks[ij].unwrap();
                    want.realized += p.realized();
                    want.unrealized += p.unrealized(Price(m));
                    want.gross_exposure +=
                        p.qty().unsigned_abs() as u128 * m.unsigned_abs() as u128;
                    want.net_exposure += p.qty() as i128 * m as i128;
                }
                assert_eq!(
                    history[aj * 3 + ij].is_empty(),
                    book.position(acct, inst).is_none()
                );
            }
            assert_eq!(
                book.account(acct).unwrap_or_default(),
                want,
                "step {step} account {aj}"
            );
        }
    }
    assert!(
        flips > 50 && reductions > 300,
        "flips {flips}, partial reductions {reductions}"
    );

    // Steady-state fills on known positions never allocate.
    let n = common::allocations_in(|| {
        for i in 0..10_000 {
            let f = fill(
                accounts[i % 3],
                instruments[(i / 3) % 3],
                if i % 2 == 0 { Side::Buy } else { Side::Sell },
                100,
                7,
            );
            let _ = book.apply_fill(&f);
        }
    });
    assert_eq!(n, 0, "apply_fill allocated");
}
