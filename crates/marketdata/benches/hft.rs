//! Benchmarks for the `marketdata` crate. Run with `cargo xtask bench --week N`.

use benchkit::{black_box, Histogram, Report, Unit};
use marketdata::{
    broadcast, replay_capture, DepthBook, FeedRecorder, Kernels, L2Publisher, L2Update,
    LadderDepth, Recv, Replayer,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Instant;
use types::{Event, OrderId, Price, Qty, Rng, Seq, Side};

const L2_UPDATE_LABEL: &str =
    "Engine event → L2 incremental (steady-state book, ~5k orders on ~60 levels)";

/// Generates a consistent, steady-state stream of book-changing events
/// (what a matching engine emits for one instrument): new orders resting
/// near the touch, cancels, partial and full fills of resting orders, and
/// reduces. Keeps its own model of the resting orders so every event refers
/// to a live order with its true remaining quantity.
struct EventGen {
    rng: Rng,
    /// (id, side, price, remaining) of every resting order.
    live: Vec<(OrderId, Side, Price, Qty)>,
    next_id: u64,
    target: usize,
}

impl EventGen {
    fn new(seed: u64, target: usize) -> EventGen {
        EventGen {
            rng: Rng::new(seed),
            live: Vec::with_capacity(target * 2),
            next_id: 0,
            target,
        }
    }

    fn next(&mut self) -> Event {
        let r = self.rng.range(0, 99);
        // Drift the population back to `target`: more adds when small,
        // more cancels when large.
        let add_threshold = if self.live.len() < self.target {
            55
        } else {
            40
        };
        if self.live.is_empty() || r < add_threshold {
            self.next_id += 1;
            let id = OrderId(self.next_id);
            let side = self.rng.side();
            // Geometric-ish distance from the touch: most orders near it.
            let dist = self.rng.range(0, 29).min(self.rng.range(0, 29)) as i64;
            let price = match side {
                Side::Buy => Price(10_000 - 1 - dist),
                Side::Sell => Price(10_000 + 1 + dist),
            };
            let qty = Qty(self.rng.range(1, 200));
            self.live.push((id, side, price, qty));
            return Event::Rested {
                id,
                side,
                price,
                qty,
            };
        }
        let i = self.rng.range(0, self.live.len() as u64 - 1) as usize;
        let (id, side, price, qty) = self.live[i];
        if r < 85 {
            self.live.swap_remove(i);
            Event::Cancelled { id, remaining: qty }
        } else if r < 95 || qty.0 < 2 {
            let fill = Qty(self.rng.range(1, qty.0));
            self.next_id += 1;
            if fill == qty {
                self.live.swap_remove(i);
            } else {
                self.live[i].3 = qty - fill;
            }
            Event::Trade {
                taker: OrderId(self.next_id),
                maker: id,
                taker_side: side.opposite(),
                price,
                qty: fill,
            }
        } else {
            let remaining = Qty(self.rng.range(1, qty.0 - 1));
            self.live[i].3 = remaining;
            Event::Reduced { id, remaining }
        }
    }
}

fn bench_l2_update() -> Report {
    const BATCH: usize = 16;
    const BATCHES: usize = 200_000;
    const WARMUP_BATCHES: usize = 20_000;
    let mut gen = EventGen::new(10, 5_000);
    let mut publisher = L2Publisher::new();
    // Reach steady state before measuring.
    for _ in 0..50_000 {
        black_box(publisher.on_event(&gen.next()));
    }
    let mut hist = Histogram::new();
    let mut batch = [Event::Accepted { id: OrderId(0) }; BATCH];
    for round in 0..WARMUP_BATCHES + BATCHES {
        // Generating events is not part of the measurement.
        batch.iter_mut().for_each(|e| *e = gen.next());
        let t = Instant::now();
        for e in &batch {
            black_box(publisher.on_event(black_box(e)));
        }
        let per_event = t.elapsed().as_nanos() as u64 / BATCH as u64;
        if round >= WARMUP_BATCHES {
            hist.record(per_event);
        }
    }
    assert!(
        publisher.order_count() > 1_000,
        "stream left the book nearly empty"
    );
    hist.report("w10.l2_update", L2_UPDATE_LABEL, Unit::Ns)
}

const FANOUT_LABEL: &str =
    "Publish to 16 readers (L2Update, 4096-slot ring, every reader drains every round)";

/// One writer publishes rounds of L2 updates into a 4,096-slot ring while
/// 16 reader threads drain it. A round ends when every reader has reached
/// the last value (by reading it, or by a `Lagged` skip — the writer
/// never waits for readers). Reports published messages per second over
/// whole rounds. Needs 17 hardware threads to mean anything.
fn bench_fanout() -> Report {
    const READERS: usize = 16;
    const ROUND: u64 = 1 << 20;
    const CAPACITY: usize = 4096;
    let (mut tx, ring) = broadcast::<L2Update>(CAPACITY);
    let start = Arc::new(Barrier::new(READERS + 1));
    let end = Arc::new(Barrier::new(READERS + 1));
    let stop = Arc::new(AtomicBool::new(false));
    let round_end = Arc::new(AtomicU64::new(0));
    let lagged = Arc::new(AtomicU64::new(0));
    let threads: Vec<_> = (0..READERS)
        .map(|_| {
            let mut r = ring.subscribe();
            let (start, end, stop) = (start.clone(), end.clone(), stop.clone());
            let (round_end, lagged) = (round_end.clone(), lagged.clone());
            std::thread::spawn(move || loop {
                start.wait();
                if stop.load(Ordering::Acquire) {
                    return;
                }
                let target = round_end.load(Ordering::Acquire);
                let mut sum = 0u64;
                while r.next_seq() < target {
                    match r.try_recv() {
                        Recv::Item(u) => sum = sum.wrapping_add(u.qty.0),
                        Recv::Lagged(n) => {
                            lagged.fetch_add(n, Ordering::Relaxed);
                        }
                        Recv::Empty => std::hint::spin_loop(),
                    }
                }
                black_box(sum);
                end.wait();
            })
        })
        .collect();

    let mut seq = 0u64;
    let report = benchkit::throughput("w16.fanout", FANOUT_LABEL, Unit::MsgsPerSec, ROUND, || {
        round_end.store(tx.next_seq() + ROUND, Ordering::Release);
        start.wait();
        for i in 0..ROUND {
            seq += 1;
            tx.publish(L2Update {
                seq: Seq(seq),
                side: if i & 1 == 0 { Side::Buy } else { Side::Sell },
                price: Price(10_000 + (i & 63) as i64),
                qty: Qty(i & 1023),
            });
        }
        end.wait();
    });
    stop.store(true, Ordering::Release);
    start.wait();
    for t in threads {
        t.join().unwrap();
    }
    let total = tx.next_seq() * READERS as u64;
    eprintln!(
        "w16.fanout: {:.2}% of reader deliveries lagged ({} cores)",
        100.0 * lagged.load(Ordering::Relaxed) as f64 / total as f64,
        std::thread::available_parallelism().map_or(0, |n| n.get())
    );
    report
}

const DEPTH_LABEL: &str =
    "SIMD vs scalar depth aggregation (depth over N ticks, sweep-to-fill, qty at or better; 4096-tick ladder)";

/// One depth-aggregation query against a ladder.
#[derive(Clone, Copy)]
enum DepthQuery {
    /// Quantity within `ticks` of the best.
    Depth(Side, u64),
    /// Worst price a taker of this size reaches.
    Sweep(Side, Qty),
    /// Quantity at this price or better.
    AtOrBetter(Side, Price),
}

/// Log-uniform in `lo..=hi`: as many short queries as long ones.
fn log_uniform(rng: &mut Rng, lo: u64, hi: u64) -> u64 {
    let (a, b) = ((lo as f64).ln(), (hi as f64).ln());
    let u = (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
    ((a + (b - a) * u).exp() as u64).clamp(lo, hi)
}

/// `LadderDepth`'s vectorized aggregates with the dispatched kernels
/// (AVX2 here) against the same ladder with the scalar kernels, same build
/// flags, same queries. The book: 4,096 prices per side around a mid of
/// 100,000, every price filled near the touch, thinning to ~5% filled 2,000
/// ticks out — L1/L2-resident (64 KiB). The queries: 1,024 per round,
/// equal parts depth-over-N-ticks, sweep-to-fill and quantity-at-or-better,
/// reaching 8–2,048 ticks deep (log-uniform). Reports scalar time / SIMD
/// time (median of interleaved rounds); 1.0 without AVX2.
fn bench_depth_sum_speedup() -> Report {
    const QUERIES: usize = 1024;
    const ROUNDS: usize = 300;
    let simd_kernels = Kernels::detect();
    let mut simd = LadderDepth::with_kernels(4096, simd_kernels);
    let mut scalar = LadderDepth::with_kernels(4096, Kernels::scalar());
    let mut rng = Rng::new(29);
    let mid = 100_000i64;
    for side in [Side::Buy, Side::Sell] {
        for d in 0..2_000i64 {
            let fill = 1.0 / (1.0 + d as f64 / 50.0);
            if rng.chance(fill.max(0.05)) {
                let price = match side {
                    Side::Buy => Price(mid - 1 - d),
                    Side::Sell => Price(mid + 1 + d),
                };
                let qty = Qty(rng.range(1, 500));
                simd.set(side, price, qty);
                scalar.set(side, price, qty);
            }
        }
    }
    let total = |side| simd.depth_qty(side, u64::MAX).0;
    let queries: Vec<DepthQuery> = (0..QUERIES)
        .map(|i| {
            let side = rng.side();
            let ticks = log_uniform(&mut rng, 8, 2_048);
            match i % 3 {
                0 => DepthQuery::Depth(side, ticks),
                1 => {
                    // A size that sweeps about `ticks` deep.
                    let q = simd.depth_qty(side, ticks).0.min(total(side));
                    DepthQuery::Sweep(side, Qty(q.max(1)))
                }
                _ => {
                    let best = simd.best(side).unwrap().price.0;
                    let p = match side {
                        Side::Buy => best - ticks as i64,
                        Side::Sell => best + ticks as i64,
                    };
                    DepthQuery::AtOrBetter(side, Price(p))
                }
            }
        })
        .collect();
    let run = |ladder: &LadderDepth| {
        let mut acc = 0u64;
        for q in &queries {
            acc = acc.wrapping_add(match *q {
                DepthQuery::Depth(side, ticks) => ladder.depth_qty(side, black_box(ticks)).0,
                DepthQuery::Sweep(side, qty) => ladder
                    .sweep_price(side, black_box(qty))
                    .map_or(0, |p| p.0 as u64),
                DepthQuery::AtOrBetter(side, p) => ladder.qty_at_or_better(side, black_box(p)).0,
            });
        }
        acc
    };
    assert_eq!(run(&simd), run(&scalar), "SIMD and scalar disagree");
    let (mut t_simd, mut t_scalar) = (Vec::new(), Vec::new());
    for round in 0..ROUNDS + ROUNDS / 10 {
        let t = Instant::now();
        black_box(run(black_box(&simd)));
        let a = t.elapsed().as_nanos() as f64;
        let t = Instant::now();
        black_box(run(black_box(&scalar)));
        let b = t.elapsed().as_nanos() as f64;
        if round >= ROUNDS / 10 {
            t_simd.push(a);
            t_scalar.push(b);
        }
    }
    let median = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let (simd_ns, scalar_ns) = (median(&mut t_simd), median(&mut t_scalar));
    eprintln!(
        "w29.depth_sum_speedup: {} {:.1} µs vs scalar {:.1} µs per {QUERIES} queries",
        simd_kernels.name(),
        simd_ns / 1e3,
        scalar_ns / 1e3
    );
    benchkit::ratio("w29.depth_sum_speedup", DEPTH_LABEL, scalar_ns, simd_ns)
}

const REPLAY_LABEL: &str =
    "Capture replay into books (pcap → UDP → feed packets → reorder buffer → book, 32 updates/packet)";

/// Replays an in-memory pcap capture of 1M L2 updates (32 per packet, a
/// verified snapshot every 10,000) into a fresh [`Replayer`] (map book,
/// 1,024-update reorder window): parsing records and Ethernet/IPv4/UDP
/// headers, decoding packets, sequencing and applying every update, and
/// verifying every snapshot. Reports updates per second.
fn bench_pcap_replay() -> Report {
    const UPDATES: usize = 1_000_000;
    let mut gen = EventGen::new(46, 5_000);
    let mut publisher = L2Publisher::with_snapshot_interval(10_000);
    let mut rec = FeedRecorder::new(Vec::new()).unwrap();
    let mut n = 0;
    while n < UPDATES {
        if let Some(u) = publisher.on_event(&gen.next()) {
            n += 1;
            rec.record_update(n as u64 * 100, &u).unwrap();
            if let Some(s) = publisher.periodic_snapshot() {
                rec.record_snapshot(n as u64 * 100, &s).unwrap();
            }
        }
    }
    let capture = rec.finish(n as u64 * 100).unwrap();
    let final_book = publisher.snapshot();
    let report = benchkit::throughput(
        "w46.pcap_replay",
        REPLAY_LABEL,
        Unit::MsgsPerSec,
        UPDATES as u64,
        || {
            let mut r = Replayer::new(1024);
            replay_capture(black_box(&capture), &mut r).unwrap();
            assert_eq!(r.stats().updates, UPDATES as u64);
            black_box(r.builder().last_seq());
        },
    );
    let mut r = Replayer::new(1024);
    replay_capture(&capture, &mut r).unwrap();
    assert_eq!(
        r.builder().snapshot(),
        final_book,
        "replay must rebuild the book"
    );
    assert_eq!(
        (r.stats().verified, r.stats().mismatches),
        ((UPDATES / 10_000) as u64, 0)
    );
    report
}

fn main() {
    benchkit::run(&[
        ("w10.l2_update", L2_UPDATE_LABEL, bench_l2_update),
        ("w16.fanout", FANOUT_LABEL, bench_fanout),
        (
            "w29.depth_sum_speedup",
            DEPTH_LABEL,
            bench_depth_sum_speedup,
        ),
        ("w46.pcap_replay", REPLAY_LABEL, bench_pcap_replay),
    ]);
}
