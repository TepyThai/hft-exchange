//! Week 41 — microstructure analytics: spread, imbalance, microprice, queue
//! position, effective / realized spread and markouts.

use engine::{Engine, EngineConfig, RestingOrder};
use marketdata::{BookBuilder, L2Publisher, L2Snapshot};
use sim::flow::{FlowConfig, OrderFlow};
use sim::micro::{
    effective_spread, queue_position, realized_spread, MarkoutStats, Markouts, QueuePosition,
    QueueTracker, TopOfBook,
};
use types::{Command, Event, InstrumentSpec, NewOrder, OrderId, Price, Qty, Side};

fn run(engine: &mut Engine, cmd: Command) -> Vec<Event> {
    let mut out = Vec::new();
    engine.process(&cmd, &mut out);
    out
}

fn qp(orders_ahead: u32, qty_ahead: u64, remaining: u64) -> Option<QueuePosition> {
    Some(QueuePosition {
        orders_ahead,
        qty_ahead: Qty(qty_ahead),
        remaining: Qty(remaining),
    })
}

#[test]
fn w41_spread_mid_imbalance_and_microprice_are_exact() {
    let t = TopOfBook::new(Some((99, 30)), Some((101, 10)));
    assert_eq!(t.spread(), Some(2));
    assert_eq!(t.mid(), Some(100.0));
    assert_eq!(t.imbalance(), Some(0.5));
    // (99·10 + 101·30) / 40: leans toward the thin ask.
    assert_eq!(t.microprice(), Some(100.5));

    let thin_bid = TopOfBook::new(Some((100, 1)), Some((101, 3)));
    assert_eq!(thin_bid.imbalance(), Some(-0.5));
    assert_eq!(thin_bid.microprice(), Some(100.25));
    assert_eq!(thin_bid.mid(), Some(100.5));

    let even = TopOfBook::new(Some((100, 7)), Some((104, 7)));
    assert_eq!(
        (even.spread(), even.imbalance(), even.microprice()),
        (Some(4), Some(0.0), Some(102.0))
    );

    // One-sided and empty books have no spread, mid, imbalance or microprice.
    for t in [
        TopOfBook::new(Some((100, 5)), None),
        TopOfBook::new(None, Some((100, 5))),
        TopOfBook::default(),
    ] {
        assert!(!t.is_two_sided());
        assert_eq!(
            (t.spread(), t.mid(), t.imbalance(), t.microprice()),
            (None, None, None, None)
        );
    }

    // The same numbers from the engine, the publisher and a feed client.
    let mut engine = Engine::new(InstrumentSpec::default());
    let mut publisher = L2Publisher::new();
    let mut client = BookBuilder::new();
    client.apply_snapshot(&L2Snapshot::default());
    let orders = [
        NewOrder::limit(1, Side::Buy, 99, 10),
        NewOrder::limit(2, Side::Buy, 99, 20),
        NewOrder::limit(3, Side::Buy, 98, 50),
        NewOrder::limit(4, Side::Sell, 101, 10),
        NewOrder::limit(5, Side::Sell, 103, 50),
    ];
    for o in orders {
        for e in run(&mut engine, Command::New(o)) {
            if let Some(u) = publisher.on_event(&e) {
                client.apply_update(&u);
            }
        }
    }
    let from_engine = TopOfBook::from_engine(&engine);
    assert_eq!(from_engine, t);
    assert_eq!(TopOfBook::from_publisher(&publisher), t);
    assert_eq!(TopOfBook::from_builder(&client), t);
}

#[test]
fn w41_queue_position_from_l3_and_from_events_agree_exactly() {
    let mut engine = Engine::new(InstrumentSpec::default());
    for (id, price, qty) in [(1, 100, 5), (2, 100, 7), (3, 100, 4)] {
        run(
            &mut engine,
            Command::New(NewOrder::limit(id, Side::Buy, price, qty)),
        );
    }
    let me = OrderId(3);
    assert_eq!(queue_position(&engine, me), qp(2, 12, 4));
    let mut level = Vec::new();
    engine.level_orders(Side::Buy, Price(100), &mut level);
    let mut tracker = QueueTracker::new(me, &level).unwrap();
    assert_eq!(tracker.id(), me);
    assert_eq!(tracker.position(), qp(2, 12, 4));

    // (command, expected position of order 3 afterwards)
    let steps: Vec<(Command, Option<QueuePosition>)> = vec![
        // Later arrivals queue behind; better prices are not our queue.
        (
            Command::New(NewOrder::limit(4, Side::Buy, 100, 6)),
            qp(2, 12, 4),
        ),
        (
            Command::New(NewOrder::limit(5, Side::Buy, 101, 3)),
            qp(2, 12, 4),
        ),
        (Command::Cancel { id: OrderId(1) }, qp(1, 7, 4)),
        (Command::Cancel { id: OrderId(4) }, qp(1, 7, 4)),
        (
            Command::Reduce {
                id: OrderId(2),
                qty: Qty(3),
            },
            qp(1, 3, 4),
        ),
        // Sells 3 at 101 (order 5), then 2 of order 2 at 100.
        (
            Command::New(NewOrder::market(6, Side::Sell, 5)),
            qp(1, 1, 4),
        ),
        // Order 2's last lot, then 2 of ours.
        (
            Command::New(NewOrder::market(7, Side::Sell, 3)),
            qp(0, 0, 2),
        ),
        (
            Command::Reduce {
                id: me,
                qty: Qty(1),
            },
            qp(0, 0, 1),
        ),
        (Command::New(NewOrder::market(8, Side::Sell, 1)), None),
    ];
    for (i, (cmd, want)) in steps.into_iter().enumerate() {
        for e in run(&mut engine, cmd) {
            tracker.on_event(&e);
        }
        assert_eq!(queue_position(&engine, me), want, "step {i} (L3)");
        assert_eq!(tracker.position(), want, "step {i} (tracker)");
    }
    // Not resting, or not in the given level: no position.
    assert_eq!(queue_position(&engine, OrderId(99)), None);
    let other = [RestingOrder::new(1, Side::Buy, 100, 5)];
    assert!(QueueTracker::new(OrderId(3), &other).is_none());
    assert!(QueueTracker::new(OrderId(3), &[]).is_none());
}

#[test]
fn w41_effective_and_realized_spread_split_cost_into_impact() {
    // Buyer pays 1 over a mid of 100: effective 2; the mid then rises to
    // 100.5, so the provider kept 1 and lost 1 to impact.
    assert_eq!(effective_spread(Side::Buy, Price(101), 100.0), 2.0);
    assert_eq!(realized_spread(Side::Buy, Price(101), 100.5), 1.0);
    // Seller at 99 against a mid of 100; the price then goes *up* to 101:
    // the provider who bought at 99 kept more than the spread.
    assert_eq!(effective_spread(Side::Sell, Price(99), 100.0), 2.0);
    assert_eq!(realized_spread(Side::Sell, Price(99), 101.0), 4.0);
    // Trading at the mid costs nothing.
    assert_eq!(effective_spread(Side::Buy, Price(100), 100.0), 0.0);
    assert_eq!(realized_spread(Side::Sell, Price(100), 100.0), 0.0);
}

#[test]
fn w41_markouts_at_horizons_are_exact() {
    let mut m = Markouts::new(&[0, 2, 5]);
    assert_eq!(m.horizons(), &[0, 2, 5]);
    m.on_mid(0, 100.0);
    m.on_fill(1, Side::Buy, Price(101), Qty(10));
    m.on_mid(1, 100.5); // the mid after the fill, at the same time
    m.on_mid(2, 101.0);
    m.on_mid(3, 102.0);
    m.on_fill(4, Side::Sell, Price(102), Qty(4));
    m.on_mid(6, 101.5);
    m.on_mid(8, 100.0);
    m.on_mid(9, 99.5);
    assert_eq!(
        m.pending(),
        1,
        "the sell's 5-horizon ends at 9: not known until later"
    );
    m.on_fill(9, Side::Buy, Price(99), Qty(1));
    m.finish(9);
    assert_eq!(
        m.pending(),
        2,
        "the last fill's 2- and 5-horizons are in the future"
    );

    let s = |fills, qty, total| MarkoutStats { fills, qty, total };
    // h=0: buy at 101 vs 100.5 → −5; sell at 102 vs 102 → 0; buy at 99 vs 99.5 → +0.5
    // h=2: buy vs 102 (t=3) → +10; sell vs 101.5 (t=6) → +2
    // h=5: buy vs 101.5 (t=6) → +5; sell vs 99.5 (t=9) → +10
    assert_eq!(m.stats(), &[s(3, 15, -4.5), s(2, 14, 12.0), s(2, 14, 15.0)]);
    assert_eq!(m.stats()[1].per_unit(), Some(12.0 / 14.0));
    assert_eq!(MarkoutStats::default().per_unit(), None);

    // No mid ever: nothing can be marked.
    let mut empty = Markouts::new(&[1]);
    empty.on_fill(0, Side::Buy, Price(100), Qty(1));
    empty.finish(10);
    assert_eq!((empty.pending(), empty.stats()[0].fills), (1, 0));
}

/// Mid in effect at `t`: the last recorded at a time ≤ t (else the first).
fn mid_at(history: &[(u64, f64)], t: u64) -> f64 {
    let i = history.partition_point(|&(at, _)| at <= t);
    history[i.saturating_sub(1)].1
}

#[test]
fn w41_randomized_analytics_agree_with_the_engine() {
    const HORIZONS: [u64; 3] = [0, 10, 100];
    for seed in [1, 2, 3] {
        let cfg = FlowConfig::default();
        let mut flow = OrderFlow::new(cfg, seed);
        let mut engine = Engine::with_config(EngineConfig::new(cfg.spec));
        let mut publisher = L2Publisher::new();
        let mut client = BookBuilder::new();
        client.apply_snapshot(&L2Snapshot::default());
        let mut trackers: Vec<QueueTracker> = Vec::new();
        let mut markouts = Markouts::new(&HORIZONS);
        let (mut mids, mut fills) = (Vec::new(), Vec::new());
        let (mut events, mut level) = (Vec::new(), Vec::new());
        let mut tracked = 0;
        const N: u64 = 20_000;
        for t in 0..N {
            let cmd = flow.next_command();
            events.clear();
            engine.process(&cmd, &mut events);
            for e in &events {
                flow.on_event(e);
                if let Some(u) = publisher.on_event(e) {
                    client.apply_update(&u);
                }
                for tr in &mut trackers {
                    tr.on_event(e);
                }
                if let Event::Trade {
                    taker_side,
                    price,
                    qty,
                    ..
                } = *e
                {
                    markouts.on_fill(t, taker_side, price, qty);
                    fills.push((t, taker_side, price, qty));
                }
            }
            // Start following every 25th order that rests.
            for e in &events {
                if let Event::Rested {
                    id, side, price, ..
                } = *e
                {
                    if id.0 % 25 == 0 {
                        engine.level_orders(side, price, &mut level);
                        trackers.push(QueueTracker::new(id, &level).unwrap());
                        tracked += 1;
                    }
                }
            }
            for tr in &trackers {
                assert_eq!(
                    tr.position(),
                    queue_position(&engine, tr.id()),
                    "seed {seed} t {t}"
                );
            }
            trackers.retain(|tr| tr.position().is_some());

            let top = TopOfBook::from_engine(&engine);
            assert_eq!(TopOfBook::from_builder(&client), top);
            assert_eq!(TopOfBook::from_publisher(&publisher), top);
            if let (Some(mid), Some(micro), Some(imb), Some(spread)) =
                (top.mid(), top.microprice(), top.imbalance(), top.spread())
            {
                let (bid, ask) = (
                    top.bid.unwrap().price.0 as f64,
                    top.ask.unwrap().price.0 as f64,
                );
                assert!(spread > 0 && (-1.0..=1.0).contains(&imb));
                assert!(bid <= micro && micro <= ask);
                assert!((micro - (mid + imb * spread as f64 / 2.0)).abs() < 1e-9);
                markouts.on_mid(t, mid);
                mids.push((t, mid));
            }
        }
        markouts.finish(N);
        assert!(tracked > 100, "seed {seed}: tracked only {tracked}");

        for (k, &h) in HORIZONS.iter().enumerate() {
            let mut want = MarkoutStats::default();
            for &(t, side, price, qty) in fills.iter().filter(|f| f.0 + h <= N) {
                let d = if side == Side::Buy { 1.0 } else { -1.0 };
                want.fills += 1;
                want.qty += qty.0;
                want.total += d * (mid_at(&mids, t + h) - price.0 as f64) * qty.0 as f64;
            }
            let got = markouts.stats()[k];
            assert_eq!(
                (got.fills, got.qty),
                (want.fills, want.qty),
                "seed {seed} h {h}"
            );
            assert!((got.total - want.total).abs() <= 1e-9 * want.total.abs().max(1.0));
            assert!(want.fills > 1_000);
        }
    }
}
