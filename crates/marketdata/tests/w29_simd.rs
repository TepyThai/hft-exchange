//! Week 29 — SIMD kernels and the dense price ladder.

mod common;

use common::{feed_from, level, EventGen};
use marketdata::{
    BookBuilder, DepthBook, Kernels, L2Publisher, L2Snapshot, LadderDepth, Level, MapDepth,
};
use types::{Price, Qty, Rng, Seq, Side};

/// Every kernel set this CPU can run, the reference first.
fn kernel_sets() -> Vec<&'static Kernels> {
    let mut sets = vec![Kernels::scalar(), Kernels::detect()];
    sets.extend(Kernels::avx2());
    sets
}

/// A random quantity array of `len` in one of several shapes.
fn random_array(rng: &mut Rng, len: usize) -> Vec<u64> {
    let shape = rng.range(0, 5);
    (0..len)
        .map(|_| match shape {
            0 => 0, // empty book
            1 => {
                if rng.chance(0.02) {
                    rng.range(1, 500)
                } else {
                    0
                }
            } // sparse
            2 => rng.range(0, 200), // dense
            3 => {
                if rng.chance(0.3) {
                    rng.next_u64()
                } else {
                    0
                }
            } // huge values
            4 => [0, 1, u64::MAX, 1 << 59, (1 << 59) - 1, 1 << 63][rng.range(0, 5) as usize],
            _ => {
                if rng.chance(0.5) {
                    0
                } else {
                    rng.range(1, 3)
                }
            }
        })
        .collect()
}

fn check_kernels(k: &Kernels, xs: &[u64], rng: &mut Rng) {
    let s = Kernels::scalar();
    let ctx = || format!("kernels {} on {} values", k.name(), xs.len());
    assert_eq!(k.sum(xs), s.sum(xs), "sum: {}", ctx());
    assert_eq!(
        k.first_nonzero(xs),
        s.first_nonzero(xs),
        "first_nonzero: {}",
        ctx()
    );
    let pick = |rng: &mut Rng| xs[rng.range(0, xs.len() as u64 - 1) as usize];
    let mut mins = vec![
        0,
        1,
        2,
        199,
        u64::MAX,
        1 << 63,
        (1 << 63) - 1,
        rng.next_u64(),
    ];
    if !xs.is_empty() {
        mins.extend([pick(rng), pick(rng).saturating_add(1)]);
    }
    for min in mins {
        assert_eq!(
            k.first_at_least(xs, min),
            s.first_at_least(xs, min),
            "first_at_least({min}): {}",
            ctx()
        );
    }
    let total: u128 = xs.iter().map(|&x| x as u128).sum();
    let mut targets = vec![0, 1, u64::MAX, rng.next_u64()];
    if total <= u64::MAX as u128 {
        let t = total as u64;
        targets.extend([t, t.saturating_sub(1), t.saturating_add(1), rng.range(0, t)]);
    }
    if !xs.is_empty() {
        // Exactly at a prefix boundary, and one past it.
        let cut = rng.range(0, xs.len() as u64 - 1) as usize;
        let prefix: u128 = xs[..=cut].iter().map(|&x| x as u128).sum();
        if prefix <= u64::MAX as u128 {
            targets.extend([prefix as u64, (prefix as u64).saturating_add(1)]);
        }
    }
    for t in targets {
        assert_eq!(
            k.levels_to_fill(xs, t),
            s.levels_to_fill(xs, t),
            "levels_to_fill({t}): {}",
            ctx()
        );
    }
}

#[test]
fn w29_kernels_match_scalar_on_random_shapes() {
    let mut rng = Rng::new(29);
    let sets = kernel_sets();
    eprintln!(
        "w29: dispatch picked {:?}; available: {:?}",
        Kernels::detect().name(),
        sets.iter().map(|k| k.name()).collect::<Vec<_>>()
    );
    let backing = random_array(&mut rng, 6_000);
    for len in (0..=300).chain([511, 512, 513, 1000, 4096, 4099]) {
        for _ in 0..4 {
            let xs = random_array(&mut rng, len);
            // Also from an unaligned start inside a bigger array.
            let off = rng.range(0, 7) as usize;
            let ys = &backing[off..off + len.min(backing.len() - off)];
            for k in &sets {
                check_kernels(k, &xs, &mut rng);
                check_kernels(k, ys, &mut rng);
            }
        }
    }
}

#[test]
fn w29_kernels_edge_cases_at_lane_boundaries() {
    for k in kernel_sets() {
        let name = k.name();
        assert_eq!(
            (k.sum(&[]), k.first_nonzero(&[]), k.first_at_least(&[], 0)),
            (0, None, None),
            "{name}"
        );
        assert_eq!(
            (k.levels_to_fill(&[], 0), k.levels_to_fill(&[], 1)),
            (Some(0), None)
        );
        for len in [1usize, 3, 4, 5, 15, 16, 17, 31, 32, 33, 47, 48, 49, 64, 100] {
            // A single level at every position: found exactly there.
            for p in 0..len {
                let mut xs = vec![0u64; len];
                xs[p] = 7;
                assert_eq!(k.first_nonzero(&xs), Some(p), "{name} len {len} at {p}");
                assert_eq!(k.sum(&xs), 7);
                assert_eq!(k.levels_to_fill(&xs, 7), Some(p + 1));
                assert_eq!(k.levels_to_fill(&xs, 8), None);
                let mut ys = vec![4u64; len];
                ys[p] = 5;
                assert_eq!(k.first_at_least(&ys, 5), Some(p), "{name} len {len} at {p}");
                assert_eq!(k.first_at_least(&ys, 6), None);
                // Unsigned compare: values with the top bit set are large.
                let mut zs = vec![(1u64 << 63) - 1; len];
                zs[p] = 1 << 63;
                assert_eq!(
                    k.first_at_least(&zs, 1 << 63),
                    Some(p),
                    "{name} len {len} sign"
                );
            }
            // Wrapping sums agree; cumulative search does not overflow.
            let max = vec![u64::MAX; len];
            assert_eq!(k.sum(&max), (len as u64).wrapping_mul(u64::MAX), "{name}");
            assert_eq!(k.levels_to_fill(&max, u64::MAX), Some(1));
            let big = vec![1u64 << 63; len];
            assert_eq!(
                k.levels_to_fill(&big, u64::MAX),
                if len >= 2 { Some(2) } else { None }
            );
            let ones = vec![1u64; len];
            assert_eq!(k.levels_to_fill(&ones, len as u64), Some(len));
            assert_eq!(k.first_at_least(&ones, 0), Some(0));
        }
    }
}

// ---------------------------------------------------------------------------
// The ladder against the map

/// Compares every observable of two depths.
fn assert_same(ladder: &LadderDepth, map: &MapDepth, ctx: &str) {
    for side in [Side::Buy, Side::Sell] {
        assert_eq!(ladder.best(side), map.best(side), "best {side:?}: {ctx}");
        assert_eq!(
            ladder.level_count(side),
            map.level_count(side),
            "count {side:?}: {ctx}"
        );
        assert_eq!(
            ladder.top(side, usize::MAX),
            map.top(side, usize::MAX),
            "levels {side:?}: {ctx}"
        );
        assert_eq!(
            ladder.top(side, 3),
            map.top(side, 3),
            "top 3 {side:?}: {ctx}"
        );
    }
}

#[test]
fn w29_ladder_matches_map_depth_on_random_operations() {
    let mut rng = Rng::new(2929);
    let sets = kernel_sets();
    for (case, window) in [64usize, 64, 1024, 7].into_iter().enumerate() {
        let mut ladder = LadderDepth::with_kernels(window, sets[case % sets.len()]);
        let mut map = MapDepth::default();
        let mut mid = 1_000i64;
        for step in 0..20_000 {
            if rng.chance(0.01) {
                mid += rng.range_i64(-40, 40);
            }
            let side = rng.side();
            let dist = if rng.chance(0.03) {
                rng.range_i64(100, 3_000)
            } else {
                rng.range_i64(0, 20)
            };
            let price = Price(match side {
                Side::Buy => mid - 1 - dist,
                Side::Sell => mid + 1 + dist,
            });
            let qty = Qty(rng.range(0, 100));
            let ctx = format!("case {case} step {step} {side:?} {price:?} {qty:?}");
            match rng.range(0, 99) {
                0..=39 => {
                    ladder.set(side, price, qty);
                    map.set(side, price, qty);
                }
                40..=69 => assert_eq!(
                    ladder.add(side, price, qty),
                    map.add(side, price, qty),
                    "{ctx}"
                ),
                70..=97 => assert_eq!(
                    ladder.sub(side, price, qty),
                    map.sub(side, price, qty),
                    "{ctx}"
                ),
                98 => {
                    let snap = map.snapshot(Seq(step), usize::MAX);
                    ladder.load(&snap);
                    map.load(&snap);
                }
                _ => ladder.recenter(Price(mid + rng.range_i64(-500, 500))),
            }
            assert_eq!(ladder.get(side, price), map.get(side, price), "{ctx}");
            assert_eq!(ladder.best(side), map.best(side), "{ctx}");
            if step % 97 == 0 {
                assert_same(&ladder, &map, &ctx);
            }
        }
        assert_same(&ladder, &map, "end");
        assert!(
            ladder.recenters() > 1,
            "the market moved: the window followed"
        );
    }
}

/// Reference answers from a side's levels, best first.
mod reference {
    use super::*;

    pub fn better_or_equal(side: Side, a: Price, b: Price) -> bool {
        match side {
            Side::Buy => a >= b,
            Side::Sell => a <= b,
        }
    }

    pub fn depth_qty(levels: &[Level], ticks: u64) -> Qty {
        let Some(best) = levels.first() else {
            return Qty(0);
        };
        if ticks == 0 {
            return Qty(0);
        }
        Qty(levels
            .iter()
            .filter(|l| l.price.0.abs_diff(best.price.0) < ticks)
            .fold(0u64, |t, l| t.wrapping_add(l.qty.0)))
    }

    pub fn qty_at_or_better(side: Side, levels: &[Level], price: Price) -> Qty {
        Qty(levels
            .iter()
            .filter(|l| better_or_equal(side, l.price, price))
            .fold(0u64, |t, l| t.wrapping_add(l.qty.0)))
    }

    pub fn sweep_price(levels: &[Level], qty: Qty) -> Option<Price> {
        if qty.0 == 0 {
            return None;
        }
        let mut cum = 0u128;
        levels.iter().find_map(|l| {
            cum += l.qty.0 as u128;
            (cum >= qty.0 as u128).then_some(l.price)
        })
    }

    pub fn first_at_least(levels: &[Level], min: Qty) -> Option<Level> {
        levels.iter().copied().find(|l| l.qty.0 >= min.0.max(1))
    }

    pub fn next_from(side: Side, levels: &[Level], price: Price) -> Option<Level> {
        levels
            .iter()
            .copied()
            .find(|l| better_or_equal(side, price, l.price))
    }
}

#[test]
fn w29_ladder_queries_match_reference_with_every_kernel_set() {
    let mut rng = Rng::new(290);
    for round in 0..60 {
        // Small windows so levels spill on both sides of the window.
        let window = [16usize, 100, 256, 4096][round % 4];
        let mut map = MapDepth::default();
        let base = rng.range_i64(-5_000, 5_000);
        for _ in 0..rng.range(0, 400) {
            let side = rng.side();
            let dist = if rng.chance(0.1) {
                rng.range_i64(0, 2_000)
            } else {
                rng.range_i64(0, 60)
            };
            let price = match side {
                Side::Buy => base - dist,
                Side::Sell => base + 1 + dist,
            };
            let qty = if rng.chance(0.05) {
                rng.next_u64() >> rng.range(0, 8)
            } else {
                rng.range(1, 300)
            };
            map.set(side, Price(price), Qty(qty));
        }
        let snap = map.snapshot(Seq(1), usize::MAX);
        for k in kernel_sets() {
            let mut ladder = LadderDepth::with_kernels(window, k);
            ladder.load(&snap);
            // Push the window off-centre now and then: overflow on one side.
            if round % 3 == 0 {
                ladder.recenter(Price(base + rng.range_i64(-200, 200)));
            }
            assert_same(&ladder, &map, "loaded");
            for side in [Side::Buy, Side::Sell] {
                let levels = map.top(side, usize::MAX);
                let total: u128 = levels.iter().map(|l| l.qty.0 as u128).sum();
                for _ in 0..40 {
                    let ticks = [0, 1, 2, rng.range(0, 64), rng.range(0, 5_000), u64::MAX]
                        [rng.range(0, 5) as usize];
                    assert_eq!(
                        ladder.depth_qty(side, ticks),
                        reference::depth_qty(&levels, ticks),
                        "depth_qty {side:?} {ticks} ({})",
                        k.name()
                    );
                    let price = Price(base + rng.range_i64(-2_100, 2_100));
                    assert_eq!(
                        ladder.qty_at_or_better(side, price),
                        reference::qty_at_or_better(side, &levels, price),
                        "qty_at_or_better {side:?} {price:?} ({})",
                        k.name()
                    );
                    assert_eq!(
                        ladder.next_level_from(side, price),
                        reference::next_from(side, &levels, price),
                        "next_level_from {side:?} {price:?} ({})",
                        k.name()
                    );
                    let q = match rng.range(0, 3) {
                        0 => Qty(rng.range(0, 1_000)),
                        1 => Qty(total.min(u64::MAX as u128) as u64),
                        2 => Qty((total.min(u64::MAX as u128) as u64).saturating_add(1)),
                        _ => Qty(rng.next_u64()),
                    };
                    assert_eq!(
                        ladder.sweep_price(side, q),
                        reference::sweep_price(&levels, q),
                        "sweep_price {side:?} {q:?} ({})",
                        k.name()
                    );
                    let min = Qty([0, 1, 150, 299, rng.next_u64()][rng.range(0, 4) as usize]);
                    assert_eq!(
                        ladder.first_level_at_least(side, min),
                        reference::first_at_least(&levels, min),
                        "first_level_at_least {side:?} {min:?} ({})",
                        k.name()
                    );
                }
            }
        }
    }
}

#[test]
fn w29_ladder_window_edges_and_recentring() {
    let mut l = LadderDepth::with_kernels(8, Kernels::detect());
    assert_eq!(
        (l.window(), l.best(Side::Buy), l.depth_qty(Side::Buy, 10)),
        (None, None, Qty(0))
    );
    // The first level places the window around itself: 8 prices, 96..=103.
    l.set(Side::Buy, Price(100), Qty(5));
    assert_eq!(l.window(), Some((Price(96), Price(103))));
    for (side, p, q) in [
        (Side::Buy, 96, 1),   // lowest window price
        (Side::Sell, 103, 2), // highest
        (Side::Sell, 104, 3), // just outside: overflow
        (Side::Buy, 95, 4),   // just outside: overflow
    ] {
        l.set(side, Price(p), Qty(q));
        assert_eq!(l.get(side, Price(p)), Qty(q));
    }
    assert_eq!(l.overflow_levels(), 2);
    assert_eq!(l.best(Side::Buy), Some(level(100, 5)));
    assert_eq!(l.best(Side::Sell), Some(level(103, 2)));
    assert_eq!(
        l.top(Side::Buy, 9),
        vec![level(100, 5), level(96, 1), level(95, 4)]
    );
    assert_eq!(l.top(Side::Sell, 9), vec![level(103, 2), level(104, 3)]);
    assert_eq!(l.depth_qty(Side::Buy, 5), Qty(6));
    assert_eq!(l.depth_qty(Side::Buy, 6), Qty(10));
    assert_eq!(l.sweep_price(Side::Sell, Qty(4)), Some(Price(104)));
    assert_eq!(l.sweep_price(Side::Sell, Qty(6)), None);
    // Removing the best rescans forward.
    l.set(Side::Buy, Price(100), Qty(0));
    assert_eq!(l.best(Side::Buy), Some(level(96, 1)));
    assert_eq!(l.sub(Side::Buy, Price(96), Qty(9)), Qty(0));
    assert_eq!(
        l.best(Side::Buy),
        Some(level(95, 4)),
        "best now outside the window"
    );
    assert_eq!(l.sub(Side::Buy, Price(1), Qty(1)), Qty(0), "absent level");

    // The market moves up to the top of the window; the first ask beyond
    // it is the new best ask, so the window re-centres on the touch.
    let mut r = LadderDepth::with_kernels(64, Kernels::detect());
    r.set(Side::Buy, Price(100), Qty(1));
    assert_eq!(
        (r.window(), r.recenters()),
        (Some((Price(68), Price(131))), 1)
    );
    r.set(Side::Buy, Price(130), Qty(2));
    r.set(Side::Sell, Price(133), Qty(3));
    assert_eq!(r.recenters(), 2);
    assert_eq!(r.window(), Some((Price(99), Price(162))));
    assert_eq!(r.overflow_levels(), 0, "every level now inside");
    assert_eq!(r.top(Side::Buy, 5), vec![level(130, 2), level(100, 1)]);
    // A far-away level that is not the best does not move the window.
    r.set(Side::Sell, Price(5_000), Qty(1));
    assert_eq!((r.recenters(), r.overflow_levels()), (2, 1));
    // Nor does a best level too far from the other side to fit both.
    r.set(Side::Buy, Price(130), Qty(0));
    r.set(Side::Buy, Price(100), Qty(0));
    r.set(Side::Buy, Price(10), Qty(1));
    assert_eq!(r.recenters(), 2);
    assert_eq!(r.best(Side::Buy), Some(level(10, 1)));

    // Extreme prices never overflow the index arithmetic.
    let mut e = LadderDepth::new(16);
    for p in [i64::MAX, i64::MIN, i64::MAX - 3, 0, i64::MIN + 1] {
        e.set(Side::Sell, Price(p), Qty(1));
        e.set(Side::Buy, Price(p), Qty(2));
    }
    let mut m = MapDepth::default();
    for p in [i64::MAX, i64::MIN, i64::MAX - 3, 0, i64::MIN + 1] {
        m.set(Side::Sell, Price(p), Qty(1));
        m.set(Side::Buy, Price(p), Qty(2));
    }
    assert_same(&e, &m, "extremes");
    assert_eq!(
        e.next_level_from(Side::Buy, Price(i64::MAX)),
        Some(level(i64::MAX, 2))
    );
    assert_eq!(
        e.depth_qty(Side::Sell, u64::MAX),
        Qty(4),
        "all but i64::MAX"
    );
    assert!(
        format!("{e:?}").len() < 2_000,
        "Debug shows a summary, not the arrays"
    );

    // Loading a snapshot centres the window on its touch.
    let mut s = LadderDepth::new(64);
    s.load(&L2Snapshot {
        seq: Seq(9),
        bids: vec![level(-1_000, 3), level(-1_010, 4)],
        asks: vec![level(-990, 1)],
    });
    let (lo, hi) = s.window().unwrap();
    assert!(lo <= Price(-1_010) && Price(-990) <= hi);
    assert_eq!(s.overflow_levels(), 0);
    s.load(&L2Snapshot::default());
    assert_eq!((s.level_count(Side::Buy), s.best(Side::Sell)), (0, None));
}

#[test]
fn w29_publisher_and_builder_on_ladder_match_map() {
    // A drifting market with a far tail, through a small window: the
    // ladder re-centres and spills, and must still agree exactly.
    let mut gen = EventGen::new(2929, 3_000);
    gen.drift = 0.2;
    gen.far = 0.02;
    let mut events = Vec::new();
    for _ in 0..60_000 {
        events.push(gen.next());
    }
    let mut map_pub = L2Publisher::with_snapshot_interval(500);
    let mut lad_pub = L2Publisher::with_depth(LadderDepth::new(64), 500);
    let (mut updates, mut snaps) = (Vec::new(), Vec::new());
    let mut max_overflow = 0;
    for e in &events {
        let (a, b) = (map_pub.on_event(e), lad_pub.on_event(e));
        assert_eq!(a, b, "updates differ at {e:?}");
        updates.extend(a);
        let (sa, sb) = (map_pub.periodic_snapshot(), lad_pub.periodic_snapshot());
        assert_eq!(sa, sb);
        snaps.extend(sa.map(|s| (updates.len(), s)));
        assert_eq!(map_pub.best_bid(), lad_pub.best_bid());
        assert_eq!(map_pub.best_ask(), lad_pub.best_ask());
        max_overflow = max_overflow.max(lad_pub.depth_book().overflow_levels());
    }
    assert_eq!(map_pub.snapshot(), lad_pub.snapshot());
    let recenters = lad_pub.depth_book().recenters();
    assert!(
        recenters > 2 && max_overflow > 0,
        "the window followed the market ({recenters}) and spilled ({max_overflow})"
    );

    // Clients on a 1%-loss feed: map and ladder builders stay identical.
    let mut rng = Rng::new(29);
    let mut map_b = BookBuilder::new();
    let mut lad_b = BookBuilder::with_depth(LadderDepth::with_kernels(128, Kernels::scalar()));
    map_b.apply_snapshot(&L2Snapshot::default());
    lad_b.apply_snapshot(&L2Snapshot::default());
    let mut next_snap = 0;
    for (i, u) in updates.iter().enumerate() {
        if rng.chance(0.99) {
            assert_eq!(map_b.apply_update(u), lad_b.apply_update(u));
        }
        while next_snap < snaps.len() && snaps[next_snap].0 == i + 1 {
            let s = &snaps[next_snap].1;
            assert_eq!(map_b.apply_snapshot(s), lad_b.apply_snapshot(s));
            next_snap += 1;
        }
        if i % 1_000 == 0 {
            assert_eq!(map_b.snapshot(), lad_b.snapshot(), "after update {i}");
            assert_eq!(map_b.depth(Side::Sell, 5), lad_b.depth(Side::Sell, 5));
        }
    }
    assert_eq!(map_b.snapshot(), lad_b.snapshot());
    let book = lad_b.depth_book();
    let best = book.best(Side::Buy).unwrap();
    assert_eq!(book.depth_qty(Side::Buy, 1), best.qty);

    // A feed recorded through the shared helper agrees too.
    let f = feed_from(EventGen::new(7, 500), 2_000, 0);
    let mut b = BookBuilder::with_depth(LadderDepth::new(32));
    b.apply_snapshot(&L2Snapshot::default());
    for u in &f.updates {
        b.apply_update(u);
    }
    assert_eq!(b.snapshot().bids, f.last.bids);
    assert_eq!(b.snapshot().asks, f.last.asks);
}
