//! Week 10 — L2 publisher, book builder, gap detection and recovery.

use marketdata::{
    BookBuilder, L2Publisher, L2Snapshot, L2Update, Level, SnapshotOutcome, UpdateOutcome,
};
use std::collections::{BTreeMap, HashMap, VecDeque};
use types::{
    Command, Event, NewOrder, OrderId, Price, Qty, RejectReason, Rng, Seq, Side, TimeInForce,
};

// ---------------------------------------------------------------------------
// A small price-time matcher, local to these tests, that produces realistic
// event streams: partial fills, sweeps through several levels, IOC/market
// remainders that expire, FOK and post-only rejects, cancels and reduces of
// live and dead orders.

#[derive(Default)]
struct ToyMatcher {
    bids: BTreeMap<Price, VecDeque<(OrderId, Qty)>>,
    asks: BTreeMap<Price, VecDeque<(OrderId, Qty)>>,
    live: HashMap<OrderId, (Side, Price)>,
}

impl ToyMatcher {
    fn book(&mut self, side: Side) -> &mut BTreeMap<Price, VecDeque<(OrderId, Qty)>> {
        match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        }
    }

    fn best(&self, side: Side) -> Option<Price> {
        match side {
            Side::Buy => self.bids.keys().next_back().copied(),
            Side::Sell => self.asks.keys().next().copied(),
        }
    }

    /// Quantity available to a `side` taker within `limit`.
    fn available(&self, side: Side, limit: Option<Price>) -> u64 {
        let book = if side == Side::Buy {
            &self.asks
        } else {
            &self.bids
        };
        book.iter()
            .filter(|(p, _)| limit.is_none_or(|l| side.crosses(l, **p)))
            .flat_map(|(_, q)| q.iter().map(|(_, qty)| qty.0))
            .sum()
    }

    fn submit(&mut self, cmd: &Command, out: &mut Vec<Event>) {
        match *cmd {
            Command::New(o) => self.new_order(o, out),
            Command::Cancel { id } => match self.live.remove(&id) {
                Some((side, price)) => {
                    let level = self.book(side).get_mut(&price).unwrap();
                    let pos = level.iter().position(|(i, _)| *i == id).unwrap();
                    let (_, remaining) = level.remove(pos).unwrap();
                    if level.is_empty() {
                        self.book(side).remove(&price);
                    }
                    out.push(Event::Cancelled { id, remaining });
                }
                None => out.push(Event::Rejected {
                    id,
                    reason: RejectReason::UnknownOrder,
                }),
            },
            Command::Reduce { id, qty } => match self.live.get(&id).copied() {
                Some((side, price)) => {
                    let level = self.book(side).get_mut(&price).unwrap();
                    let entry = level.iter_mut().find(|(i, _)| *i == id).unwrap();
                    if qty.is_zero() || qty >= entry.1 {
                        out.push(Event::Rejected {
                            id,
                            reason: RejectReason::InvalidReduce,
                        });
                    } else {
                        entry.1 = qty;
                        out.push(Event::Reduced { id, remaining: qty });
                    }
                }
                None => out.push(Event::Rejected {
                    id,
                    reason: RejectReason::UnknownOrder,
                }),
            },
        }
    }

    fn new_order(&mut self, o: NewOrder, out: &mut Vec<Event>) {
        let limit = o.limit_price();
        let reject = |reason| Event::Rejected { id: o.id, reason };
        if o.qty.is_zero() {
            return out.push(reject(RejectReason::ZeroQuantity));
        }
        let opposite_best = self.best(o.side.opposite());
        let would_take = opposite_best.is_some_and(|b| limit.is_none_or(|l| o.side.crosses(l, b)));
        if matches!(
            o.kind,
            types::OrderKind::Limit {
                post_only: true,
                ..
            }
        ) && would_take
        {
            return out.push(reject(RejectReason::WouldTake));
        }
        if limit.is_none() && opposite_best.is_none() {
            return out.push(reject(RejectReason::NoLiquidity));
        }
        if o.tif == TimeInForce::Fok && self.available(o.side, limit) < o.qty.0 {
            return out.push(reject(RejectReason::FokUnfillable));
        }
        out.push(Event::Accepted { id: o.id });

        let mut remaining = o.qty;
        while !remaining.is_zero() {
            let Some(best) = self.best(o.side.opposite()) else {
                break;
            };
            if limit.is_some_and(|l| !o.side.crosses(l, best)) {
                break;
            }
            let level = self.book(o.side.opposite()).get_mut(&best).unwrap();
            let mut filled = Vec::new();
            while !remaining.is_zero() {
                let Some(front) = level.front_mut() else {
                    break;
                };
                let fill = remaining.min(front.1);
                out.push(Event::Trade {
                    taker: o.id,
                    maker: front.0,
                    taker_side: o.side,
                    price: best,
                    qty: fill,
                });
                remaining -= fill;
                front.1 -= fill;
                if front.1.is_zero() {
                    filled.push(front.0);
                    level.pop_front();
                }
            }
            if level.is_empty() {
                self.book(o.side.opposite()).remove(&best);
            }
            for id in filled {
                self.live.remove(&id);
            }
        }
        if remaining.is_zero() {
            return;
        }
        match (limit, o.tif) {
            (Some(price), TimeInForce::Gtc) => {
                self.book(o.side)
                    .entry(price)
                    .or_default()
                    .push_back((o.id, remaining));
                self.live.insert(o.id, (o.side, price));
                out.push(Event::Rested {
                    id: o.id,
                    side: o.side,
                    price,
                    qty: remaining,
                });
            }
            _ => out.push(Event::Expired {
                id: o.id,
                remaining,
            }),
        }
    }

    /// The matcher's own book, aggregated by price — the ground truth the
    /// publisher must reproduce from events alone.
    fn l2(&self, seq: Seq) -> L2Snapshot {
        let agg = |(p, q): (&Price, &VecDeque<(OrderId, Qty)>)| Level {
            price: *p,
            qty: Qty(q.iter().map(|(_, qty)| qty.0).sum()),
        };
        L2Snapshot {
            seq,
            bids: self.bids.iter().rev().map(agg).collect(),
            asks: self.asks.iter().map(agg).collect(),
        }
    }
}

/// Random order flow around a drifting mid price.
struct Flow {
    rng: Rng,
    next_id: u64,
    mid: i64,
}

impl Flow {
    fn new(seed: u64) -> Flow {
        Flow {
            rng: Rng::new(seed),
            next_id: 0,
            mid: 1_000,
        }
    }

    fn next_command(&mut self) -> Command {
        let r = &mut self.rng;
        if r.chance(0.02) {
            self.mid += r.range_i64(-2, 2);
        }
        let pick = r.range(0, 99);
        if pick < 62 || self.next_id == 0 {
            self.next_id += 1;
            let side = r.side();
            let qty = r.range(1, 60);
            if pick < 2 {
                return Command::New(NewOrder::market(self.next_id, side, qty));
            }
            // Mostly passive, sometimes aggressive (crossing up to 3 ticks).
            let offset = r.range_i64(-3, 12);
            let price = match side {
                Side::Buy => self.mid - offset,
                Side::Sell => self.mid + offset,
            };
            let mut o = NewOrder::limit(self.next_id, side, price, qty);
            match r.range(0, 19) {
                0 | 1 => o = o.with_tif(TimeInForce::Ioc),
                2 => o = o.with_tif(TimeInForce::Fok),
                3 => o = o.post_only(),
                _ => {}
            }
            Command::New(o)
        } else if pick < 90 {
            Command::Cancel {
                id: OrderId(r.range(1, self.next_id)),
            }
        } else {
            Command::Reduce {
                id: OrderId(r.range(1, self.next_id)),
                qty: Qty(r.range(0, 40)),
            }
        }
    }
}

/// Runs `commands` random commands through the toy matcher, calling `f`
/// with each event and the matcher (after the whole command).
fn run_flow(seed: u64, commands: usize, mut f: impl FnMut(&Event, &ToyMatcher)) {
    let mut m = ToyMatcher::default();
    let mut flow = Flow::new(seed);
    let mut events = Vec::new();
    for _ in 0..commands {
        events.clear();
        m.submit(&flow.next_command(), &mut events);
        for e in &events {
            f(e, &m);
        }
    }
}

fn level(price: i64, qty: u64) -> Level {
    Level {
        price: Price(price),
        qty: Qty(qty),
    }
}

fn upd(seq: u64, side: Side, price: i64, qty: u64) -> L2Update {
    L2Update {
        seq: Seq(seq),
        side,
        price: Price(price),
        qty: Qty(qty),
    }
}

/// Publishes a random flow, returning every update and the publisher.
fn publish(seed: u64, commands: usize) -> (Vec<L2Update>, L2Publisher) {
    let mut p = L2Publisher::new();
    let mut updates = Vec::new();
    run_flow(seed, commands, |e, _| updates.extend(p.on_event(e)));
    (updates, p)
}

// ---------------------------------------------------------------------------
// Tests

#[test]
fn w10_events_map_to_level_updates() {
    use Side::*;
    let mut p = L2Publisher::new();
    let mut on = |e: Event| p.on_event(&e);
    let (o1, o2, o3, taker) = (OrderId(1), OrderId(2), OrderId(3), OrderId(9));

    assert_eq!(on(Event::Accepted { id: o1 }), None);
    assert_eq!(
        on(Event::Rested {
            id: o1,
            side: Buy,
            price: Price(100),
            qty: Qty(10)
        }),
        Some(upd(1, Buy, 100, 10))
    );
    // Two orders at one price aggregate into one level.
    assert_eq!(
        on(Event::Rested {
            id: o2,
            side: Buy,
            price: Price(100),
            qty: Qty(5)
        }),
        Some(upd(2, Buy, 100, 15))
    );
    assert_eq!(
        on(Event::Rested {
            id: o3,
            side: Sell,
            price: Price(102),
            qty: Qty(7)
        }),
        Some(upd(3, Sell, 102, 7))
    );
    // A partial fill lowers the maker's level.
    let trade = |maker, qty| Event::Trade {
        taker,
        maker,
        taker_side: Sell,
        price: Price(100),
        qty: Qty(qty),
    };
    assert_eq!(on(trade(o1, 4)), Some(upd(4, Buy, 100, 11)));
    // Reduced carries the new remaining (5 -> 2): the level drops by 3.
    assert_eq!(
        on(Event::Reduced {
            id: o2,
            remaining: Qty(2)
        }),
        Some(upd(5, Buy, 100, 8))
    );
    // The taker's IOC remainder never rested: no book change.
    assert_eq!(
        on(Event::Expired {
            id: taker,
            remaining: Qty(3)
        }),
        None
    );
    assert_eq!(
        on(Event::Rejected {
            id: OrderId(10),
            reason: RejectReason::WouldTake
        }),
        None
    );
    // Filling the rest of order 1 removes it but not the level.
    assert_eq!(on(trade(o1, 6)), Some(upd(6, Buy, 100, 2)));
    // Last order at a price cancelled: qty 0 = level removed.
    assert_eq!(
        on(Event::Cancelled {
            id: o2,
            remaining: Qty(2)
        }),
        Some(upd(7, Buy, 100, 0))
    );
    // Events about orders that are no longer resting change nothing.
    assert_eq!(
        on(Event::Cancelled {
            id: o2,
            remaining: Qty(2)
        }),
        None
    );
    assert_eq!(on(trade(o1, 1)), None);
    assert_eq!(
        on(Event::Reduced {
            id: o1,
            remaining: Qty(1)
        }),
        None
    );
    // A resting order that expires leaves the book.
    assert_eq!(
        on(Event::Expired {
            id: o3,
            remaining: Qty(7)
        }),
        Some(upd(8, Sell, 102, 0))
    );

    assert_eq!(p.last_seq(), Seq(8));
    assert_eq!(
        (p.order_count(), p.level_count(Buy), p.level_count(Sell)),
        (0, 0, 0)
    );
    assert_eq!(
        p.snapshot(),
        L2Snapshot {
            seq: Seq(8),
            bids: vec![],
            asks: vec![]
        }
    );

    // The L3 view behind it.
    let mut p = L2Publisher::default();
    p.on_event(&Event::Rested {
        id: o1,
        side: Sell,
        price: Price(5),
        qty: Qty(3),
    });
    p.on_event(&Event::Trade {
        taker,
        maker: o1,
        taker_side: Buy,
        price: Price(5),
        qty: Qty(1),
    });
    assert_eq!((p.resting_qty(o1), p.resting_qty(o2)), (Some(Qty(2)), None));
    assert_eq!(p.level_qty(Sell, Price(5)), Qty(2));
    assert_eq!(p.level_qty(Buy, Price(5)), Qty(0));
}

#[test]
fn w10_snapshots_are_ordered_and_truncatable() {
    let mut p = L2Publisher::with_snapshot_interval(3);
    assert_eq!(p.snapshot(), L2Snapshot::default());
    assert_eq!((p.best_bid(), p.best_ask()), (None, None));
    assert_eq!(p.periodic_snapshot(), None);

    let rest = [
        (Side::Buy, 99, 5),
        (Side::Sell, 103, 2),
        (Side::Buy, 101, 1),
        (Side::Sell, 102, 4),
        (Side::Buy, 100, 3),
    ];
    let mut periodic = Vec::new();
    for (i, (side, price, qty)) in rest.into_iter().enumerate() {
        let e = Event::Rested {
            id: OrderId(i as u64 + 1),
            side,
            price: Price(price),
            qty: Qty(qty),
        };
        p.on_event(&e).unwrap();
        periodic.extend(p.periodic_snapshot());
    }
    let full = p.snapshot();
    assert_eq!(full.seq, Seq(5));
    assert_eq!(full.bids, vec![level(101, 1), level(100, 3), level(99, 5)]);
    assert_eq!(full.asks, vec![level(102, 4), level(103, 2)]);
    assert_eq!(
        (full.best_bid(), full.best_ask()),
        (Some(level(101, 1)), Some(level(102, 4)))
    );
    assert_eq!(full.levels(Side::Sell), &full.asks[..]);
    assert_eq!(
        (p.best_bid(), p.best_ask()),
        (full.best_bid(), full.best_ask())
    );

    let top = p.snapshot_top(2);
    assert_eq!(
        top,
        L2Snapshot {
            seq: Seq(5),
            bids: vec![level(101, 1), level(100, 3)],
            asks: full.asks.clone()
        }
    );
    assert_eq!(
        p.snapshot_top(0),
        L2Snapshot {
            seq: Seq(5),
            bids: vec![],
            asks: vec![]
        }
    );
    assert_eq!(p.snapshot_top(100), full);

    // One periodic snapshot after update 3; the next is due after update 6.
    assert_eq!(periodic.len(), 1);
    assert_eq!(periodic[0].seq, Seq(3));
    assert_eq!(periodic[0].bids, vec![level(101, 1), level(99, 5)]);
    p.on_event(&Event::Cancelled {
        id: OrderId(1),
        remaining: Qty(5),
    })
    .unwrap();
    assert_eq!(p.periodic_snapshot().map(|s| s.seq), Some(Seq(6)));
    assert_eq!(p.periodic_snapshot(), None);
    let mut never = L2Publisher::new();
    never.on_event(&Event::Rested {
        id: OrderId(1),
        side: Side::Buy,
        price: Price(1),
        qty: Qty(1),
    });
    assert_eq!(never.periodic_snapshot(), None);
}

#[test]
fn w10_rebuild_matches_publisher_and_matcher_on_random_streams() {
    for (seed, commands, check_every) in [
        (1, 20_000, 500),
        (2, 20_000, 500),
        (3, 20_000, 500),
        (4, 3_000, 1),
    ] {
        let mut m = ToyMatcher::default();
        let mut flow = Flow::new(seed);
        let mut p = L2Publisher::new();
        let mut b = BookBuilder::new();
        assert!(!b.is_synced());
        assert_eq!(
            b.apply_snapshot(&p.snapshot()),
            SnapshotOutcome::Synced {
                dropped: 0,
                replayed: 0
            }
        );
        let (mut updates, mut trades, mut max_levels) = (0u64, 0u64, 0usize);
        let mut events = Vec::new();
        for i in 1..=commands {
            events.clear();
            m.submit(&flow.next_command(), &mut events);
            for e in &events {
                trades += matches!(e, Event::Trade { .. }) as u64;
                if let Some(u) = p.on_event(e) {
                    updates += 1;
                    assert_eq!(b.apply_update(&u), UpdateOutcome::Applied);
                }
            }
            max_levels = max_levels.max(p.level_count(Side::Buy) + p.level_count(Side::Sell));
            if i % check_every == 0 || i == commands {
                // At a command boundary: builder == publisher == the
                // matcher's real book, built from events alone.
                let truth = m.l2(p.last_seq());
                assert_eq!(p.snapshot(), truth, "seed {seed}, command {i}");
                assert_eq!(b.snapshot(), truth, "seed {seed}, command {i}");
                assert_eq!(
                    (b.best_bid(), b.best_ask()),
                    (truth.best_bid(), truth.best_ask())
                );
                assert_eq!(p.order_count(), m.live.len());
            }
        }
        assert!(b.is_synced() && b.last_seq() == Some(p.last_seq()));
        // The stream must have been realistic enough to mean something.
        let n = commands as u64;
        assert!(
            updates > n * 3 / 4 && trades > n / 20 && max_levels > 10,
            "seed {seed}: {updates} updates, {trades} trades, {max_levels} levels"
        );
    }
}

#[test]
fn w10_updates_are_dense_and_unconflated() {
    let mut p = L2Publisher::new();
    let mut before = p.snapshot();
    let mut expected_seq = 1;
    run_flow(6, 5_000, |e, _| {
        let update = p.on_event(e);
        let after = p.snapshot();
        match update {
            None => assert_eq!(
                (after.bids.clone(), after.asks.clone()),
                (before.bids.clone(), before.asks.clone())
            ),
            Some(u) => {
                // Dense numbering, one update per changed level.
                assert_eq!(u.seq, Seq(expected_seq));
                expected_seq += 1;
                assert_eq!(u.qty, p.level_qty(u.side, u.price));
                let changed: Vec<(Side, Price)> = [Side::Buy, Side::Sell]
                    .into_iter()
                    .flat_map(|side| {
                        let (old, new) = (before.levels(side), after.levels(side));
                        let qty = |ls: &[Level], price: Price| {
                            ls.iter().find(|l| l.price == price).map(|l| l.qty)
                        };
                        let mut prices: Vec<Price> =
                            old.iter().chain(new).map(|l| l.price).collect();
                        prices.sort();
                        prices.dedup();
                        prices
                            .into_iter()
                            .filter(move |&pr| qty(old, pr) != qty(new, pr))
                            .map(move |pr| (side, pr))
                    })
                    .collect();
                assert_eq!(changed, vec![(u.side, u.price)], "event {e:?} -> {u:?}");
            }
        }
        before = after;
    });
    assert!(expected_seq > 3_000);

    // Aggressive sweep through three levels: three updates, in book order.
    let mut p = L2Publisher::new();
    let mut m = ToyMatcher::default();
    let mut out = Vec::new();
    for (id, price) in [(1, 101), (2, 102), (3, 103), (4, 101)] {
        m.submit(
            &Command::New(NewOrder::limit(id, Side::Sell, price, 5)),
            &mut out,
        );
    }
    m.submit(
        &Command::New(NewOrder::limit(9, Side::Buy, 103, 17)),
        &mut out,
    );
    let ups: Vec<L2Update> = out.iter().filter_map(|e| p.on_event(e)).collect();
    let tail: Vec<(Side, i64, u64)> = ups[4..]
        .iter()
        .map(|u| (u.side, u.price.0, u.qty.0))
        .collect();
    assert_eq!(
        tail,
        vec![
            (Side::Sell, 101, 5),
            (Side::Sell, 101, 0),
            (Side::Sell, 102, 0),
            (Side::Sell, 103, 3)
        ],
        "maker by maker, not conflated per level"
    );
    assert!(ups.windows(2).all(|w| w[1].seq.0 == w[0].seq.0 + 1));
}

#[test]
fn w10_gap_is_detected_and_updates_buffered() {
    let (updates, _) = publish(7, 2_000);
    let mut b = BookBuilder::new();
    // Before any snapshot, everything is buffered.
    assert_eq!(b.apply_update(&updates[0]), UpdateOutcome::Buffered);
    assert_eq!(b.apply_update(&updates[0]), UpdateOutcome::Duplicate);
    assert_eq!((b.buffered(), b.last_seq(), b.gaps()), (1, None, 0));
    assert_eq!(
        b.apply_snapshot(&L2Snapshot::default()),
        SnapshotOutcome::Synced {
            dropped: 0,
            replayed: 1
        }
    );

    for u in &updates[1..99] {
        assert_eq!(b.apply_update(u), UpdateOutcome::Applied);
    }
    // A/B feeds deliver duplicates: ignored while in sync.
    assert_eq!(b.apply_update(&updates[50]), UpdateOutcome::Duplicate);
    assert_eq!(b.apply_update(&updates[98]), UpdateOutcome::Duplicate);
    assert!(b.is_synced());
    let before_gap = b.snapshot();

    // Update #100 (index 99) is lost.
    assert_eq!(
        b.apply_update(&updates[100]),
        UpdateOutcome::Gap {
            expected: Seq(100),
            received: Seq(101)
        }
    );
    assert!(!b.is_synced());
    assert_eq!(
        (b.gaps(), b.buffered(), b.last_seq()),
        (1, 1, Some(Seq(99)))
    );
    // The book is frozen at the last good state; later updates are buffered.
    for u in &updates[101..150] {
        assert_eq!(b.apply_update(u), UpdateOutcome::Buffered);
    }
    assert_eq!(b.apply_update(&updates[120]), UpdateOutcome::Duplicate);
    assert_eq!(b.apply_update(&updates[10]), UpdateOutcome::Duplicate);
    assert_eq!(b.buffered(), 50);
    assert_eq!(b.snapshot(), before_gap);
    assert_eq!(b.gaps(), 1, "one gap, however many updates follow it");
    // The late packet does not repair the gap (no reordering in week 10).
    assert_eq!(b.apply_update(&updates[99]), UpdateOutcome::Duplicate);
    assert!(!b.is_synced());
}

#[test]
fn w10_snapshot_resync_drops_covered_updates() {
    // Record the publisher's snapshot after every update.
    let mut p = L2Publisher::new();
    let mut updates = vec![];
    let mut snaps = vec![p.snapshot()];
    run_flow(8, 1_000, |e, _| {
        if let Some(u) = p.on_event(e) {
            updates.push(u);
            snaps.push(p.snapshot());
        }
    });
    let at = |s: u64| &snaps[s as usize];
    let u = |s: u64| &updates[s as usize - 1];

    let mut b = BookBuilder::new();
    b.apply_snapshot(at(0));
    (1..=10).for_each(|s| assert_eq!(b.apply_update(u(s)), UpdateOutcome::Applied));
    // Lose 11 and 15; receive 12..=14 and 16..=20.
    for s in (12..=14).chain(16..=20) {
        b.apply_update(u(s));
    }
    assert_eq!(b.buffered(), 8);

    // Too old to help.
    assert_eq!(b.apply_snapshot(at(9)), SnapshotOutcome::Stale);
    assert_eq!(b.apply_snapshot(at(10)), SnapshotOutcome::Stale);
    // Snapshot at 13: drop 12, 13; replay 14; then 15 is missing.
    assert_eq!(
        b.apply_snapshot(at(13)),
        SnapshotOutcome::StillGapped {
            dropped: 2,
            replayed: 1,
            expected: Seq(15),
            received: Seq(16)
        }
    );
    assert!(!b.is_synced());
    assert_eq!(
        (b.last_seq(), b.buffered(), b.gaps()),
        (Some(Seq(14)), 5, 2)
    );
    assert_eq!(b.snapshot(), *at(14));
    // Snapshot at 18: drop 16..=18, replay 19, 20 — in sync again.
    assert_eq!(
        b.apply_snapshot(at(18)),
        SnapshotOutcome::Synced {
            dropped: 3,
            replayed: 2
        }
    );
    assert!(b.is_synced());
    assert_eq!((b.last_seq(), b.buffered()), (Some(Seq(20)), 0));
    assert_eq!(b.snapshot(), *at(20));
    // And continues live.
    for s in 21..=updates.len() as u64 {
        assert_eq!(b.apply_update(u(s)), UpdateOutcome::Applied);
    }
    assert_eq!(b.snapshot(), p.snapshot());

    // A snapshot newer than everything buffered: all dropped.
    let mut c = BookBuilder::new();
    (5..=30).for_each(|s| {
        c.apply_update(u(s));
    });
    assert_eq!(
        c.apply_snapshot(at(40)),
        SnapshotOutcome::Synced {
            dropped: 26,
            replayed: 0
        }
    );
    assert_eq!(c.apply_update(u(40)), UpdateOutcome::Duplicate);
    assert_eq!(c.apply_update(u(41)), UpdateOutcome::Applied);
    assert_eq!(c.snapshot(), *at(41));
    // While in sync, a snapshot ahead of the incrementals moves the book
    // forward; the updates it covers then arrive as duplicates.
    assert_eq!(
        c.apply_snapshot(at(45)),
        SnapshotOutcome::Synced {
            dropped: 0,
            replayed: 0
        }
    );
    (42..=45).for_each(|s| assert_eq!(c.apply_update(u(s)), UpdateOutcome::Duplicate));
    assert_eq!(c.apply_update(u(46)), UpdateOutcome::Applied);
    assert_eq!(c.snapshot(), *at(46));
}

#[test]
fn w10_lossy_feed_with_periodic_snapshots_converges() {
    for seed in [11, 12, 13] {
        let mut rng = Rng::new(seed * 7);
        let mut p = L2Publisher::with_snapshot_interval(64);
        let mut tops = vec![(None, None)]; // publisher's top of book after each update
        let mut clients = [BookBuilder::new(), BookBuilder::new()]; // [lossy, late joiner]
        clients[0].apply_snapshot(&p.snapshot());
        let (mut lost, mut events) = (0, 0u64);
        let check = |b: &BookBuilder, tops: &[(Option<Level>, Option<Level>)]| {
            if b.is_synced() {
                let s = b.last_seq().unwrap().0 as usize;
                assert_eq!(
                    (b.best_bid(), b.best_ask()),
                    tops[s],
                    "seed {seed}: wrong book at seq {s}"
                );
            }
        };
        run_flow(seed, 30_000, |e, _| {
            events += 1;
            if let Some(u) = p.on_event(e) {
                tops.push((p.best_bid(), p.best_ask()));
                // 2% of packets lost.
                if rng.chance(0.02) {
                    lost += 1;
                } else {
                    clients[0].apply_update(&u);
                    if rng.chance(0.05) {
                        clients[0].apply_update(&u); // duplicate delivery
                    }
                    if events > 5_000 {
                        clients[1].apply_update(&u);
                    }
                }
            }
            if let Some(s) = p.periodic_snapshot() {
                clients[0].apply_snapshot(&s);
                if events > 5_000 {
                    clients[1].apply_snapshot(&s);
                }
            }
            clients.iter().for_each(|b| check(b, &tops));
        });
        assert!(lost > 100, "seed {seed}: only {lost} losses");
        assert!(
            clients[0].gaps() > 50,
            "seed {seed}: gaps {}",
            clients[0].gaps()
        );
        // A final snapshot brings everyone to the publisher's exact book.
        let last = p.snapshot();
        for b in &mut clients {
            if !b.is_synced() || b.last_seq() != Some(last.seq) {
                assert!(matches!(
                    b.apply_snapshot(&last),
                    SnapshotOutcome::Synced { .. }
                ));
            }
            assert!(b.is_synced());
            assert_eq!(b.snapshot(), last, "seed {seed}");
        }
    }
}
