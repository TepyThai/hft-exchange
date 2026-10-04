//! Week 6: the limit order book.

use book::{Book, BookError, LevelInfo, OrderBook, RestingOrder};
use std::collections::BTreeMap;
use types::{OrderId, Price, Qty, Rng, Side};

fn buy(id: u64, price: i64, qty: u64) -> RestingOrder {
    RestingOrder::new(id, Side::Buy, price, qty)
}

fn sell(id: u64, price: i64, qty: u64) -> RestingOrder {
    RestingOrder::new(id, Side::Sell, price, qty)
}

fn lvl(price: i64, qty: u64, orders: u32) -> LevelInfo {
    LevelInfo {
        price: Price(price),
        qty: Qty(qty),
        orders,
    }
}

fn ids_at(book: &impl OrderBook, side: Side, price: i64) -> Vec<u64> {
    let mut out = Vec::new();
    book.level_orders(side, Price(price), &mut out);
    out.iter().map(|o| o.id.0).collect()
}

#[test]
fn w06_add_sets_best_prices_and_aggregates() {
    let mut b = Book::new();
    assert!(b.is_empty());
    assert_eq!(b.best_bid(), None);
    assert_eq!(b.best_ask(), None);
    assert_eq!(b.front(Side::Buy), None);

    b.add(buy(1, 99, 10)).unwrap();
    b.add(buy(2, 100, 5)).unwrap();
    b.add(buy(3, 100, 7)).unwrap();
    b.add(sell(4, 102, 3)).unwrap();
    b.add(sell(5, 101, 4)).unwrap();

    assert_eq!(b.len(), 5);
    assert_eq!(b.best_bid(), Some(lvl(100, 12, 2)));
    assert_eq!(b.best_ask(), Some(lvl(101, 4, 1)));
    assert_eq!(b.spread(), Some(1));
    assert!(!b.is_crossed());
    assert_eq!(b.level_count(Side::Buy), 2);
    assert_eq!(b.level_count(Side::Sell), 2);
    assert_eq!(b.get(OrderId(3)), Some(buy(3, 100, 7)));
    assert!(b.contains(OrderId(4)));
    assert!(!b.contains(OrderId(42)));

    // A worse price does not move the best; a better one does.
    b.add(sell(6, 105, 1)).unwrap();
    assert_eq!(b.best_ask().unwrap().price, Price(101));
    b.add(buy(7, 100_000, 1)).unwrap(); // the book stores crossed input as-is
    assert_eq!(b.best_bid().unwrap().price, Price(100_000));
    assert!(b.is_crossed());
    // Negative prices are valid ticks (spreads, some futures).
    b.add(sell(8, -5, 1)).unwrap();
    assert_eq!(b.best_ask().unwrap().price, Price(-5));

    b.clear();
    assert!(b.is_empty());
    assert_eq!(b.best_bid(), None);
    assert_eq!(b.level_count(Side::Sell), 0);
    b.add(buy(1, 50, 1)).unwrap(); // ids are free again after clear
    assert_eq!(b.best_bid(), Some(lvl(50, 1, 1)));
}

#[test]
fn w06_fifo_within_level_and_front_is_oldest_at_best() {
    let mut b = Book::new();
    for id in [5, 3, 9, 1] {
        b.add(sell(id, 200, id)).unwrap();
    }
    b.add(sell(7, 199, 2)).unwrap();
    assert_eq!(
        ids_at(&b, Side::Sell, 200),
        vec![5, 3, 9, 1],
        "arrival order, not id order"
    );
    assert_eq!(ids_at(&b, Side::Sell, 199), vec![7]);
    assert_eq!(ids_at(&b, Side::Sell, 198), Vec::<u64>::new());
    assert_eq!(ids_at(&b, Side::Buy, 200), Vec::<u64>::new());
    assert_eq!(b.front(Side::Sell), Some(sell(7, 199, 2)));

    let mut all = Vec::new();
    b.visit_orders(Side::Sell, &mut |o| {
        all.push(o.id.0);
        true
    });
    assert_eq!(all, vec![7, 5, 3, 9, 1], "full priority: price, then time");

    // Visitors stop when the callback returns false.
    let mut first_two = Vec::new();
    b.visit_orders(Side::Sell, &mut |o| {
        first_two.push(o.id.0);
        first_two.len() < 2
    });
    assert_eq!(first_two, vec![7, 5]);
}

#[test]
fn w06_cancel_unlinks_anywhere_and_drops_empty_levels() {
    let mut b = Book::new();
    for id in 1..=5 {
        b.add(buy(id, 100, 10)).unwrap();
    }
    b.add(buy(6, 98, 1)).unwrap();

    // Middle, head, tail.
    assert_eq!(b.cancel(OrderId(3)), Ok(buy(3, 100, 10)));
    assert_eq!(ids_at(&b, Side::Buy, 100), vec![1, 2, 4, 5]);
    assert_eq!(b.cancel(OrderId(1)), Ok(buy(1, 100, 10)));
    assert_eq!(b.front(Side::Buy).unwrap().id, OrderId(2));
    assert_eq!(b.cancel(OrderId(5)), Ok(buy(5, 100, 10)));
    assert_eq!(ids_at(&b, Side::Buy, 100), vec![2, 4]);
    assert_eq!(b.best_bid(), Some(lvl(100, 20, 2)));

    // Appending after a tail cancel still links correctly.
    b.add(buy(7, 100, 3)).unwrap();
    assert_eq!(ids_at(&b, Side::Buy, 100), vec![2, 4, 7]);

    // Emptying the best level promotes the next one.
    for id in [2, 4, 7] {
        b.cancel(OrderId(id)).unwrap();
    }
    assert_eq!(b.level(Side::Buy, Price(100)), None);
    assert_eq!(b.best_bid(), Some(lvl(98, 1, 1)));
    assert_eq!(b.level_count(Side::Buy), 1);
    assert_eq!(
        b.cancel(OrderId(2)),
        Err(BookError::UnknownOrder),
        "already gone"
    );
    b.cancel(OrderId(6)).unwrap();
    assert!(b.is_empty());
    assert_eq!(b.best_bid(), None);
    assert_eq!(b.front(Side::Buy), None);
}

#[test]
fn w06_reduce_keeps_priority_and_errors_leave_book_unchanged() {
    let mut b = Book::new();
    b.add(sell(1, 50, 10)).unwrap();
    b.add(sell(2, 50, 10)).unwrap();
    assert_eq!(b.reduce(OrderId(1), Qty(4)), Ok(sell(1, 50, 4)));
    assert_eq!(
        ids_at(&b, Side::Sell, 50),
        vec![1, 2],
        "reduce keeps time priority"
    );
    assert_eq!(b.best_ask(), Some(lvl(50, 14, 2)));

    let before = (b.len(), b.best_ask(), ids_at(&b, Side::Sell, 50));
    assert_eq!(b.reduce(OrderId(1), Qty(4)), Err(BookError::InvalidReduce));
    assert_eq!(b.reduce(OrderId(1), Qty(5)), Err(BookError::InvalidReduce));
    assert_eq!(b.reduce(OrderId(1), Qty(0)), Err(BookError::ZeroQty));
    assert_eq!(b.reduce(OrderId(9), Qty(1)), Err(BookError::UnknownOrder));
    assert_eq!(b.add(sell(2, 51, 1)), Err(BookError::DuplicateId));
    assert_eq!(
        b.add(buy(2, 40, 1)),
        Err(BookError::DuplicateId),
        "ids are unique across sides"
    );
    assert_eq!(b.add(sell(3, 50, 0)), Err(BookError::ZeroQty));
    assert_eq!(b.cancel(OrderId(9)), Err(BookError::UnknownOrder));
    assert_eq!((b.len(), b.best_ask(), ids_at(&b, Side::Sell, 50)), before);
    assert_eq!(
        b.level(Side::Sell, Price(51)),
        None,
        "failed add leaves no empty level"
    );
    assert_eq!(b.level_count(Side::Sell), 1);
}

#[test]
fn w06_depth_is_best_first_and_truncated() {
    let mut b = Book::new();
    let mut id = 0;
    for p in [101, 105, 103, 102, 104] {
        for _ in 0..2 {
            id += 1;
            b.add(sell(id, p, 5)).unwrap();
            id += 1;
            b.add(buy(id, p - 10, 1)).unwrap();
        }
    }
    let mut out = vec![lvl(0, 0, 0); 7]; // stale content is cleared
    b.depth(Side::Sell, 3, &mut out);
    assert_eq!(out, vec![lvl(101, 10, 2), lvl(102, 10, 2), lvl(103, 10, 2)]);
    b.depth(Side::Buy, 10, &mut out);
    assert_eq!(
        out.iter().map(|l| l.price.0).collect::<Vec<_>>(),
        vec![95, 94, 93, 92, 91]
    );
    b.depth(Side::Buy, 0, &mut out);
    assert!(out.is_empty());
    assert_eq!(b.level(Side::Sell, Price(104)), Some(lvl(104, 10, 2)));
    assert_eq!(b.level(Side::Sell, Price(100)), None);
}

#[test]
fn w06_fill_front_consumes_in_priority_order() {
    let mut b = Book::new();
    b.add(sell(1, 10, 5)).unwrap();
    b.add(sell(2, 10, 3)).unwrap();
    b.add(sell(3, 11, 4)).unwrap();

    assert_eq!(
        b.fill_front(Side::Sell, Qty(2)),
        Qty(3),
        "partial fill keeps the order at the front"
    );
    assert_eq!(b.front(Side::Sell), Some(sell(1, 10, 3)));
    assert_eq!(b.best_ask(), Some(lvl(10, 6, 2)));
    assert_eq!(b.fill_front(Side::Sell, Qty(3)), Qty(0));
    assert!(!b.contains(OrderId(1)), "filled order leaves the index");
    assert_eq!(b.front(Side::Sell), Some(sell(2, 10, 3)));
    assert_eq!(b.fill_front(Side::Sell, Qty(3)), Qty(0));
    assert_eq!(
        b.best_ask(),
        Some(lvl(11, 4, 1)),
        "empty level removed, next level is best"
    );
    assert_eq!(b.level_count(Side::Sell), 1);
    assert_eq!(b.fill_front(Side::Sell, Qty(4)), Qty(0));
    assert_eq!(b.front(Side::Sell), None);
    assert!(b.is_empty());

    let r = std::panic::catch_unwind(|| {
        let mut b = Book::new();
        b.add(sell(1, 10, 5)).unwrap();
        b.fill_front(Side::Sell, Qty(6));
    });
    assert!(r.is_err(), "overfilling the front order must panic");
}

/// Reference model: per side, price → FIFO of (id, qty).
#[derive(Default)]
struct Model {
    sides: [BTreeMap<i64, Vec<(u64, u64)>>; 2],
}

impl Model {
    fn s(side: Side) -> usize {
        (side == Side::Sell) as usize
    }
    fn find(&self, id: u64) -> Option<(Side, i64, usize)> {
        for side in [Side::Buy, Side::Sell] {
            for (&p, q) in &self.sides[Self::s(side)] {
                if let Some(i) = q.iter().position(|&(oid, _)| oid == id) {
                    return Some((side, p, i));
                }
            }
        }
        None
    }
    fn orders(&self, side: Side) -> Vec<RestingOrder> {
        let map = &self.sides[Self::s(side)];
        let levels: Vec<_> = match side {
            Side::Buy => map.iter().rev().collect(),
            Side::Sell => map.iter().collect(),
        };
        levels
            .into_iter()
            .flat_map(|(&p, q)| {
                q.iter()
                    .map(move |&(id, qty)| RestingOrder::new(id, side, p, qty))
            })
            .collect()
    }
}

#[test]
fn w06_randomized_against_model() {
    let mut rng = Rng::new(0x06_06);
    // Exercised through a trait object: the trait must stay object-safe.
    let mut book: Box<dyn OrderBook> = Box::new(Book::with_capacity(64, 16));
    let mut model = Model::default();
    let mut next_id = 1u64;
    for step in 0..20_000 {
        let r = rng.range(0, 99);
        if r < 45 {
            let side = rng.side();
            let price = rng.range_i64(95, 105);
            let qty = rng.range(1, 20);
            let id = if rng.chance(0.05) && next_id > 1 {
                rng.range(1, next_id - 1)
            } else {
                next_id
            };
            let expect_dup = model.find(id).is_some();
            let got = book.add(RestingOrder::new(id, side, price, qty));
            if expect_dup {
                assert_eq!(got, Err(BookError::DuplicateId), "step {step}");
            } else {
                assert_eq!(got, Ok(()), "step {step}");
                model.sides[Model::s(side)]
                    .entry(price)
                    .or_default()
                    .push((id, qty));
            }
            if id == next_id {
                next_id += 1;
            }
        } else if r < 70 {
            let id = rng.range(1, next_id);
            let got = book.cancel(OrderId(id));
            match model.find(id) {
                Some((side, p, i)) => {
                    let q = model.sides[Model::s(side)].get_mut(&p).unwrap();
                    let (_, qty) = q.remove(i);
                    if q.is_empty() {
                        model.sides[Model::s(side)].remove(&p);
                    }
                    assert_eq!(got, Ok(RestingOrder::new(id, side, p, qty)), "step {step}");
                }
                None => assert_eq!(got, Err(BookError::UnknownOrder), "step {step}"),
            }
        } else if r < 82 {
            let id = rng.range(1, next_id);
            let qty = rng.range(0, 20);
            let got = book.reduce(OrderId(id), Qty(qty));
            match model.find(id) {
                None => assert_eq!(got, Err(BookError::UnknownOrder)),
                Some(_) if qty == 0 => assert_eq!(got, Err(BookError::ZeroQty)),
                Some((side, p, i)) => {
                    let entry = &mut model.sides[Model::s(side)].get_mut(&p).unwrap()[i];
                    if qty >= entry.1 {
                        assert_eq!(got, Err(BookError::InvalidReduce));
                    } else {
                        entry.1 = qty;
                        assert_eq!(got, Ok(RestingOrder::new(id, side, p, qty)));
                    }
                }
            }
        } else {
            let side = rng.side();
            let front = model.orders(side).first().copied();
            assert_eq!(book.front(side), front, "step {step}");
            if let Some(f) = front {
                let fill = rng.range(1, f.qty.0);
                let left = book.fill_front(side, Qty(fill));
                assert_eq!(left, Qty(f.qty.0 - fill));
                let map = &mut model.sides[Model::s(side)];
                let q = map.get_mut(&f.price.0).unwrap();
                q[0].1 -= fill;
                if q[0].1 == 0 {
                    q.remove(0);
                    if q.is_empty() {
                        map.remove(&f.price.0);
                    }
                }
            }
        }

        // Full comparison every few steps (cheap, the book stays small).
        if step % 7 == 0 {
            for side in [Side::Buy, Side::Sell] {
                let mut got = Vec::new();
                book.visit_orders(side, &mut |o| {
                    got.push(o);
                    true
                });
                let want = model.orders(side);
                assert_eq!(got, want, "step {step} {side:?}");
                let map = &model.sides[Model::s(side)];
                assert_eq!(book.level_count(side), map.len());
                let best = match side {
                    Side::Buy => map.iter().next_back(),
                    Side::Sell => map.iter().next(),
                };
                let want_best =
                    best.map(|(&p, q)| lvl(p, q.iter().map(|o| o.1).sum(), q.len() as u32));
                assert_eq!(book.best(side), want_best, "step {step} {side:?}");
                for o in &want {
                    assert_eq!(book.get(o.id), Some(*o));
                }
            }
            let total: usize = model
                .sides
                .iter()
                .flat_map(|m| m.values())
                .map(|q| q.len())
                .sum();
            assert_eq!(book.len(), total);
        }
    }
}

#[test]
fn w06_cancel_cost_does_not_grow_with_level_size() {
    // 100k orders in ONE level, cancelled in random order. With an O(1)
    // unlink this takes a few ms; scanning the level would take seconds.
    const N: u64 = 100_000;
    let mut b = Book::with_capacity(N as usize, 1);
    for id in 0..N {
        b.add(buy(id, 1_000, 1)).unwrap();
    }
    let mut ids: Vec<u64> = (0..N).collect();
    let mut rng = Rng::new(6);
    for i in (1..ids.len()).rev() {
        ids.swap(i, rng.range(0, i as u64) as usize);
    }
    let t = std::time::Instant::now();
    for id in ids {
        b.cancel(OrderId(id)).unwrap();
    }
    let elapsed = t.elapsed();
    assert!(b.is_empty());
    assert!(elapsed.as_millis() < 300, "100k cancels took {elapsed:?}");
}
