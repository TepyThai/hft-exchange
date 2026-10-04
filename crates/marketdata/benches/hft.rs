//! Benchmarks for the `marketdata` crate. Run with `cargo xtask bench --week N`.

use benchkit::{black_box, Histogram, Report, Unit};
use marketdata::L2Publisher;
use std::time::Instant;
use types::{Event, OrderId, Price, Qty, Rng, Side};

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

fn main() {
    benchkit::run(&[("w10.l2_update", L2_UPDATE_LABEL, bench_l2_update)]);
}
