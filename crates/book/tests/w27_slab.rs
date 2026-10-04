//! Week 27: allocation-free hot paths — the generational slab and the
//! slab-backed book.

mod common;

use book::{BookError, OrderBook, RestingOrder, Slab, SlabBook};
use common::{allocations_in, assert_same, differential};
use types::{OrderId, Qty, Rng, Side};

#[test]
fn w27_slab_keys_are_stable_and_stale_keys_are_rejected() {
    let mut slab: Slab<u64> = Slab::with_capacity(4);
    assert!(slab.is_empty());
    let a = slab.insert(10);
    let b = slab.insert(20);
    assert_ne!(a, b);
    assert_eq!((slab.get(a), slab.get(b)), (Some(&10), Some(&20)));
    *slab.get_mut(b).unwrap() = 21;
    assert_eq!(slab.len(), 2);

    // Remove `a`: its key is now stale for every operation.
    assert_eq!(slab.remove(a), Some(10));
    assert_eq!(slab.remove(a), None, "double remove");
    assert_eq!(slab.get(a), None);
    assert!(slab.get_mut(a).is_none());
    assert!(!slab.contains(a));
    assert_eq!(slab.key_at(a.index()), None);

    // The slot is reused for `c`, under a new generation: the old key must
    // not see (or remove) the new value — the ABA problem.
    let c = slab.insert(30);
    assert_eq!(c.index(), a.index(), "freed slot is reused");
    assert_ne!(c.generation(), a.generation());
    assert_eq!(slab.get(a), None);
    assert_eq!(slab.remove(a), None);
    assert_eq!(slab.get(c), Some(&30));
    assert_eq!(slab.key_at(c.index()), Some(c));
    assert_eq!(*slab.at(c.index()), 30);
    assert_eq!(slab.len(), 2);

    // A key from another slab slot range is simply unknown.
    let mut other: Slab<u64> = Slab::with_capacity(64);
    for i in 0..10 {
        other.insert(i);
    }
    let far = other.insert(99);
    assert_eq!(slab.get(far), None);

    // `clear` invalidates every outstanding key but keeps the storage.
    slab.clear();
    assert!(slab.is_empty());
    assert_eq!((slab.get(b), slab.get(c)), (None, None));
    let d = slab.insert(40);
    assert_eq!(slab.get(d), Some(&40));
    assert!(d != b && d != c, "a key is never issued twice");
}

#[test]
fn w27_slab_reuses_slots_without_allocating() {
    const N: usize = 1_000;
    let mut slab: Slab<[u64; 4]> = Slab::with_capacity(N);
    let mut rng = Rng::new(27);
    let mut live = Vec::with_capacity(N);
    let allocs = allocations_in(|| {
        for i in 0..N as u64 {
            live.push(slab.insert([i; 4]));
        }
        // Heavy churn at full capacity: remove and insert at random.
        for step in 0..50_000u64 {
            let at = rng.range(0, live.len() as u64 - 1) as usize;
            let key = live.swap_remove(at);
            assert!(slab.remove(key).is_some());
            live.push(slab.insert([step; 4]));
        }
    });
    assert_eq!(allocs, 0, "a slab within its capacity must not allocate");
    assert_eq!(slab.len(), N);
    assert_eq!(slab.capacity(), N);

    // Beyond capacity the slab grows like a Vec, and every key stays valid.
    for _ in 0..N {
        live.push(slab.insert([7; 4]));
    }
    assert!(slab.capacity() >= 2 * N);
    assert!(live.iter().all(|&k| slab.contains(k)));
    // Freeing everything after growth still never allocates.
    let allocs = allocations_in(|| {
        for &k in &live {
            slab.remove(k).unwrap();
        }
    });
    assert_eq!(allocs, 0);
    assert!(slab.is_empty());
}

#[test]
fn w27_slab_book_matches_book_on_random_ops() {
    // Narrow prices (many orders per level, FIFO and level churn), through a
    // trait object like the engine's dyn configuration.
    for seed in 0..4u64 {
        let mut b: Box<dyn OrderBook> = Box::new(SlabBook::with_capacity(64, 8));
        differential(b.as_mut(), 0x2700 + seed, 20_000, 7, |rng, _| {
            rng.range_i64(95, 105)
        });
    }
    // Wide prices around a touch: levels open and close constantly.
    let mut b = SlabBook::with_capacity(4_096, 512);
    differential(&mut b, 0x27FF, 30_000, 101, |rng, side| {
        let d = rng.range_i64(0, 300);
        if side == Side::Buy {
            10_000 - d
        } else {
            10_001 + d
        }
    });
}

#[test]
fn w27_add_cancel_and_fills_never_allocate_within_capacity() {
    const ORDERS: usize = 2_000;
    const LEVELS: usize = 200;
    let mut book = SlabBook::with_capacity(ORDERS, LEVELS);
    let mut rng = Rng::new(0x27A);
    let mut live: Vec<OrderId> = Vec::with_capacity(ORDERS);
    let mut next = 1u64;
    // Measured from the very first operation: no warm-up needed when the
    // book is sized up front.
    let allocs = allocations_in(|| {
        for _ in 0..200_000 {
            let r = rng.range(0, 99);
            if live.len() < ORDERS - 1 && (r < 50 || live.is_empty()) {
                let side = rng.side();
                let off = rng.range_i64(0, LEVELS as i64 - 2);
                let price = if side == Side::Buy {
                    1_000 - off
                } else {
                    1_001 + off
                };
                let order = RestingOrder::new(next, side, price, rng.range(1, 9));
                book.add(order).unwrap();
                live.push(order.id);
                next += 1;
            } else if r < 85 {
                let at = rng.range(0, live.len() as u64 - 1) as usize;
                book.cancel(live.swap_remove(at)).unwrap();
            } else if r < 92 {
                let id = live[rng.range(0, live.len() as u64 - 1) as usize];
                let q = book.get(id).unwrap().qty;
                if q.0 > 1 {
                    book.reduce(id, Qty(q.0 - 1)).unwrap();
                }
            } else {
                let side = rng.side();
                if let Some(front) = book.front(side) {
                    if book.fill_front(side, front.qty).is_zero() {
                        let at = live.iter().position(|&i| i == front.id).unwrap();
                        live.swap_remove(at);
                    }
                }
            }
        }
    });
    assert_eq!(allocs, 0, "SlabBook allocated within its capacity");
    assert_eq!(book.len(), live.len());
    assert!(book.order_capacity() >= ORDERS);
}

#[test]
fn w27_stale_order_keys_never_touch_a_reused_slot() {
    let mut book = SlabBook::with_capacity(8, 4);
    book.add(RestingOrder::new(1, Side::Sell, 101, 5)).unwrap();
    let k1 = book.key_of(OrderId(1)).unwrap();
    assert_eq!(
        book.get_by_key(k1),
        Some(RestingOrder::new(1, Side::Sell, 101, 5))
    );
    assert_eq!(book.key_of(OrderId(99)), None);

    // Order 1 fills completely; order 2 takes its slot.
    assert_eq!(book.fill_front(Side::Sell, Qty(5)), Qty(0));
    book.add(RestingOrder::new(2, Side::Sell, 102, 7)).unwrap();
    let k2 = book.key_of(OrderId(2)).unwrap();
    assert_eq!(k2.index(), k1.index(), "the freed slot is reused");
    assert_ne!(k2, k1);

    // The stale handle is refused and leaves order 2 alone.
    assert_eq!(book.get_by_key(k1), None);
    assert_eq!(book.cancel_by_key(k1), Err(BookError::UnknownOrder));
    assert_eq!(book.get(OrderId(2)).unwrap().qty, Qty(7));
    assert_eq!(book.len(), 1);

    // The live handle cancels without an id lookup, exactly once.
    assert_eq!(
        book.cancel_by_key(k2),
        Ok(RestingOrder::new(2, Side::Sell, 102, 7))
    );
    assert_eq!(book.cancel_by_key(k2), Err(BookError::UnknownOrder));
    assert!(book.is_empty());
    assert_eq!(book.best_ask(), None);
    assert_eq!(book.level_count(Side::Sell), 0);
}

#[test]
fn w27_cancel_by_key_and_by_id_agree() {
    let mut rng = Rng::new(0x27B);
    let mut slab = SlabBook::with_capacity(1_024, 64);
    let mut reference = book::Book::new();
    let mut ids = Vec::new();
    for id in 1..=1_000u64 {
        let side = rng.side();
        let price = if side == Side::Buy {
            500 - rng.range_i64(0, 30)
        } else {
            501 + rng.range_i64(0, 30)
        };
        let o = RestingOrder::new(id, side, price, rng.range(1, 50));
        slab.add(o).unwrap();
        reference.add(o).unwrap();
        ids.push(OrderId(id));
    }
    for i in (1..ids.len()).rev() {
        ids.swap(i, rng.range(0, i as u64) as usize);
    }
    for (n, id) in ids.into_iter().enumerate() {
        let want = reference.cancel(id);
        let got = if n % 2 == 0 {
            let key = slab.key_of(id).unwrap();
            slab.cancel_by_key(key)
        } else {
            slab.cancel(id)
        };
        assert_eq!(got, want);
        if n % 97 == 0 {
            assert_same(&slab, &reference, &format!("after {n} cancels"));
        }
    }
    assert!(slab.is_empty());
}

#[test]
fn w27_clear_invalidates_keys_and_reuses_storage() {
    let mut book = SlabBook::with_capacity(256, 32);
    let mut keys = Vec::new();
    for id in 1..=200u64 {
        let side = if id % 2 == 0 { Side::Buy } else { Side::Sell };
        let price = if side == Side::Buy {
            100 - (id % 20) as i64
        } else {
            101 + (id % 20) as i64
        };
        book.add(RestingOrder::new(id, side, price, 3)).unwrap();
        keys.push(book.key_of(OrderId(id)).unwrap());
    }
    book.clear();
    assert!(book.is_empty());
    assert_eq!(book.best_bid(), None);
    assert!(keys.iter().all(|&k| book.get_by_key(k).is_none()));
    // Refilling a cleared book reuses its storage.
    let allocs = allocations_in(|| {
        for id in 1..=200u64 {
            book.add(RestingOrder::new(id, Side::Buy, 50 + (id % 20) as i64, 1))
                .unwrap();
        }
    });
    assert_eq!(allocs, 0);
    assert_eq!(book.len(), 200);
    assert!(keys.iter().all(|&k| book.get_by_key(k).is_none()));
}
