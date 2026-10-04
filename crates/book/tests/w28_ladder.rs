//! Week 28: the ladder book — dense levels around the touch, sparse
//! fallback for far prices.

mod common;

use book::{Book, LadderBook, LevelInfo, OrderBook, RestingOrder};
use common::{allocations_in, assert_same, differential, levels, orders};
use types::{OrderId, Price, Qty, Rng, Side};

#[test]
fn w28_ladder_matches_book_on_random_ops() {
    // Narrow prices: all dense, many orders per level.
    for seed in 0..4u64 {
        let mut b: Box<dyn OrderBook> = Box::new(LadderBook::new(64, 64));
        differential(b.as_mut(), 0x2800 + seed, 20_000, 7, |rng, _| {
            rng.range_i64(95, 105)
        });
    }
    // Near the touch with a tail of far prices on both sides of the window
    // (sparse above and below), a window smaller than the spread of prices.
    let mut b = LadderBook::new(128, 1_024);
    differential(&mut b, 0x28FF, 40_000, 53, |rng, side| {
        let d = if rng.chance(0.1) {
            rng.range_i64(0, 5_000)
        } else {
            rng.range_i64(0, 40)
        };
        if side == Side::Buy {
            10_000 - d
        } else {
            10_001 + d
        }
    });
}

#[test]
fn w28_window_follows_a_drifting_market() {
    // The mid trends ~10_000 ticks up and back down (with noise), far
    // beyond the 256-price window: the ladder must re-centre in both
    // directions and keep the same answers as the w06 book.
    let mut rng = Rng::new(0x28D);
    let mut ladder = LadderBook::new(256, 4_096);
    let mut model = Book::new();
    let (mut mid, mut highest): (i64, i64) = (0, 0);
    let mut live: Vec<OrderId> = Vec::new();
    let mut next = 1u64;
    for step in 0..60_000 {
        let trend = if step < 30_000 { 1 } else { -1 };
        mid += rng.range_i64(-2, 2) + if step % 3 == 0 { trend } else { 0 };
        highest = highest.max(mid);
        let r = rng.range(0, 99);
        if r < 50 || live.is_empty() {
            let side = rng.side();
            let d = rng.range_i64(1, 60);
            let price = if side == Side::Buy { mid - d } else { mid + d };
            let o = RestingOrder::new(next, side, price, rng.range(1, 9));
            next += 1;
            assert_eq!(ladder.add(o), model.add(o));
            live.push(o.id);
        } else if r < 80 {
            let at = rng.range(0, live.len() as u64 - 1) as usize;
            let id = live.swap_remove(at);
            assert_eq!(ladder.cancel(id), model.cancel(id), "step {step}");
        } else {
            // Sweep a few levels of one side (moves the touch away).
            let side = rng.side();
            for _ in 0..rng.range(1, 6) {
                let Some(f) = model.front(side) else { break };
                assert_eq!(ladder.front(side), Some(f), "step {step}");
                assert_eq!(
                    ladder.fill_front(side, f.qty),
                    model.fill_front(side, f.qty)
                );
                live.retain(|&id| id != f.id);
            }
        }
        assert_eq!(ladder.best_bid(), model.best_bid(), "step {step}");
        assert_eq!(ladder.best_ask(), model.best_ask(), "step {step}");
        if step % 997 == 0 {
            assert_same(&ladder, &model, &format!("step {step}"));
        }
    }
    assert!(
        highest > 5_000,
        "the walk must leave the first window ({highest})"
    );
    assert_same(&ladder, &model, "end");
}

#[test]
fn w28_far_and_extreme_prices_use_the_sparse_side() {
    let mut b = LadderBook::new(64, 64);
    let mut model = Book::new();
    let prices = [
        100,
        99,
        i64::MIN,
        i64::MIN + 1,
        -5,
        0,
        1_000_000,
        i64::MAX,
        i64::MAX - 1,
        101,
        40,
        163,
    ];
    let mut id = 0;
    for (k, &p) in prices.iter().enumerate() {
        for side in [Side::Buy, Side::Sell] {
            id += 1;
            let o = RestingOrder::new(id, side, p, k as u64 + 1);
            b.add(o).unwrap();
            model.add(o).unwrap();
        }
    }
    assert_same(&b, &model, "after adds");
    assert_eq!(b.best_bid().unwrap().price, Price(i64::MAX));
    assert_eq!(b.best_ask().unwrap().price, Price(i64::MIN));
    // Drain both sides through the front in priority order.
    for side in [Side::Buy, Side::Sell] {
        while let Some(f) = model.front(side) {
            assert_eq!(b.front(side), Some(f));
            assert_eq!(b.fill_front(side, f.qty), model.fill_front(side, f.qty));
            assert_same(&b, &model, "draining");
        }
    }
    assert!(b.is_empty());
    assert_eq!(b.level_count(Side::Buy) + b.level_count(Side::Sell), 0);
}

#[test]
fn w28_next_best_is_found_across_bitmap_words() {
    // Levels 1, 63, 64, 65, 130, 511 ticks from the touch: emptying the best
    // must find the next one in the same word, the next word, or many words
    // away — in both directions.
    let gaps = [0i64, 1, 63, 64, 65, 130, 511];
    let mut b = LadderBook::new(1_024, 64);
    let mut id = 0;
    for &g in &gaps {
        id += 1;
        b.add(RestingOrder::new(id, Side::Buy, 5_000 - g, 1))
            .unwrap();
        id += 1;
        b.add(RestingOrder::new(id, Side::Sell, 5_001 + g, 1))
            .unwrap();
    }
    let bids: Vec<i64> = levels(&b, Side::Buy).iter().map(|l| l.price.0).collect();
    assert_eq!(bids, gaps.iter().map(|g| 5_000 - g).collect::<Vec<_>>());
    for &g in &gaps {
        assert_eq!(b.best_bid().unwrap().price, Price(5_000 - g));
        assert_eq!(b.best_ask().unwrap().price, Price(5_001 + g));
        b.fill_front(Side::Buy, Qty(1));
        b.fill_front(Side::Sell, Qty(1));
    }
    assert_eq!((b.best_bid(), b.best_ask()), (None, None));

    // Cancelling levels out of order behind the best keeps the bitmap exact.
    let mut model = Book::new();
    for k in 0..300i64 {
        let o = RestingOrder::new(1_000 + k as u64, Side::Buy, 9_000 - 3 * k, 2);
        b.add(o).unwrap();
        model.add(o).unwrap();
    }
    for k in (0..300u64).filter(|k| k % 3 != 1) {
        let id = OrderId(1_000 + k);
        assert_eq!(b.cancel(id), model.cancel(id));
    }
    assert_same(&b, &model, "after scattered cancels");
    let want: Vec<LevelInfo> = levels(&model, Side::Buy);
    let mut got = Vec::new();
    b.depth(Side::Buy, 10, &mut got);
    assert_eq!(got, want[..10]);
}

#[test]
fn w28_steady_state_near_the_touch_does_not_allocate() {
    let mut b = LadderBook::new(2_048, 4_096);
    let mut rng = Rng::new(0x28A);
    let mut live: Vec<OrderId> = Vec::with_capacity(4_096);
    let mut next = 1u64;
    let mut step = |b: &mut LadderBook, live: &mut Vec<OrderId>, rng: &mut Rng| {
        if live.len() < 3_000 && (rng.chance(0.55) || live.is_empty()) {
            let side = rng.side();
            let d = rng.range_i64(0, 200);
            let price = if side == Side::Buy {
                100_000 - d
            } else {
                100_001 + d
            };
            b.add(RestingOrder::new(next, side, price, rng.range(1, 9)))
                .unwrap();
            live.push(OrderId(next));
            next += 1;
        } else {
            let at = rng.range(0, live.len() as u64 - 1) as usize;
            b.cancel(live.swap_remove(at)).unwrap();
        }
    };
    for _ in 0..20_000 {
        step(&mut b, &mut live, &mut rng);
    }
    let allocs = allocations_in(|| {
        for _ in 0..200_000 {
            step(&mut b, &mut live, &mut rng);
        }
    });
    assert_eq!(allocs, 0, "adds and cancels inside the window allocated");
    assert_eq!(b.len(), live.len());
}

#[test]
fn w28_visits_merge_sparse_and_dense_in_priority_order() {
    let mut b = LadderBook::new(64, 256);
    let mut model = Book::new();
    // Touch at 1_000/1_001; some levels far above and below each window.
    let mut rng = Rng::new(0x28C);
    for id in 1..=400u64 {
        let side = rng.side();
        let d = match rng.range(0, 3) {
            0 => rng.range_i64(0, 20),
            1 => rng.range_i64(100, 400),
            _ => rng.range_i64(0, 63),
        };
        let price = if side == Side::Buy {
            1_000 - d
        } else {
            1_001 + d
        };
        let o = RestingOrder::new(id, side, price, rng.range(1, 5));
        b.add(o).unwrap();
        model.add(o).unwrap();
    }
    // A bid far *above* the bid window and an ask far *below* the ask
    // window (a crossed book is legal for a book): sparse on the better
    // side of the window.
    for (id, side, price) in [(900, Side::Buy, 5_000), (901, Side::Sell, -5_000)] {
        let o = RestingOrder::new(id, side, price, 1);
        b.add(o).unwrap();
        model.add(o).unwrap();
    }
    assert_same(&b, &model, "mixed sparse/dense");
    for side in [Side::Buy, Side::Sell] {
        // Early stop works across the sparse/dense boundary.
        for n in [1, 2, 5, 37] {
            let (mut got, mut want) = (Vec::new(), Vec::new());
            b.depth(side, n, &mut got);
            model.depth(side, n, &mut want);
            assert_eq!(got, want, "{side:?} depth {n}");
        }
        let mut seen = 0;
        b.visit_orders(side, &mut |_| {
            seen += 1;
            seen < 50
        });
        assert_eq!(seen, 50);
        assert_eq!(orders(&b, side).len(), orders(&model, side).len());
    }
}
