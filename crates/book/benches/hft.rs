//! Benchmarks for the `book` crate. Run with `cargo xtask bench --week N`.

use benchkit::{black_box, Report, Unit};
use book::{Book, LadderBook, OrderBook, RestingOrder, SlabBook};
use types::{OrderId, Rng, Side};

const MID: i64 = 1_000_000;
/// Resting orders spread over this many ticks on each side of the mid.
const SPAN: i64 = 500;
const RESTING: usize = 100_000;

fn random_order(rng: &mut Rng, id: u64) -> RestingOrder {
    let side = rng.side();
    let offset = rng.range_i64(1, SPAN);
    let price = match side {
        Side::Buy => MID - offset,
        Side::Sell => MID + offset,
    };
    RestingOrder::new(id, side, price, rng.range(1, 100))
}

/// A book with `RESTING` orders over 2×`SPAN` levels (~100 per level), and
/// the ids of its resting orders.
fn full_book(rng: &mut Rng) -> (Book, Vec<OrderId>) {
    fill(Book::with_capacity(RESTING + 1, 2 * SPAN as usize), rng)
}

/// Fills `book` with `RESTING` random orders (see [`full_book`]).
fn fill<B: OrderBook>(mut book: B, rng: &mut Rng) -> (B, Vec<OrderId>) {
    let mut live = Vec::with_capacity(RESTING);
    for id in 1..=RESTING as u64 {
        book.add(random_order(rng, id)).unwrap();
        live.push(OrderId(id));
    }
    (book, live)
}

/// The w06 add-then-cancel operation on any book: one `latency` sample per
/// pair (add a random order, cancel a random resting one elsewhere).
fn add_cancel_latency<B: OrderBook>(id: &str, label: &str, book: B, seed: u64) -> Report {
    let mut rng = Rng::new(seed);
    let (mut book, mut live) = fill(book, &mut rng);
    let mut next_id = RESTING as u64 + 1;
    benchkit::latency(
        id,
        label,
        Unit::Ns,
        2_000_000,
        || {
            let order = random_order(&mut rng, next_id);
            next_id += 1;
            let slot = rng.range(0, live.len() as u64 - 1) as usize;
            let victim = std::mem::replace(&mut live[slot], order.id);
            (order, victim)
        },
        |(order, victim)| {
            book.add(order).unwrap();
            black_box(book.cancel(victim).unwrap());
        },
    )
}

/// A filled book and a pre-generated stream of 1M (add, cancel) pairs for
/// it (the w06 operation, without a timer per pair).
fn add_cancel_stream<B: OrderBook>(book: B, seed: u64) -> (B, Vec<(RestingOrder, OrderId)>) {
    const PAIRS: usize = 1_000_000;
    let mut rng = Rng::new(seed);
    let (base, mut live) = fill(book, &mut rng);
    let mut pairs = Vec::with_capacity(PAIRS);
    for k in 0..PAIRS as u64 {
        let order = random_order(&mut rng, RESTING as u64 + 1 + k);
        let slot = rng.range(0, live.len() as u64 - 1) as usize;
        pairs.push((order, std::mem::replace(&mut live[slot], order.id)));
    }
    (base, pairs)
}

/// Add/cancel pairs per second over `pairs`, each run on a clone of `base`.
fn add_cancel_rate<B: OrderBook + Clone>(base: &B, pairs: &[(RestingOrder, OrderId)]) -> f64 {
    let report = benchkit::throughput("", "", Unit::OpsPerSec, pairs.len() as u64, || {
        let mut book = base.clone();
        for &(order, victim) in pairs {
            book.add(order).unwrap();
            black_box(book.cancel(victim).unwrap());
        }
        black_box(book.len());
    });
    report.value
}

/// Median of `a() / b()` over `rounds` alternating measurements, so that a
/// change in machine load during the run hits both sides alike.
fn interleaved_ratio(rounds: usize, mut a: impl FnMut() -> f64, mut b: impl FnMut() -> f64) -> f64 {
    let mut ratios: Vec<f64> = (0..rounds)
        .map(|_| {
            let x = a();
            x / b()
        })
        .collect();
    ratios.sort_by(f64::total_cmp);
    ratios[rounds / 2]
}

/// w27: the w06 add/cancel stream on the slab-backed book, sized up front.
fn add_cancel_slab() -> Report {
    add_cancel_latency(
        "w27.add_cancel_slab",
        "Add then cancel with slab storage",
        SlabBook::with_capacity(RESTING + 1, SPAN as usize),
        0x0606,
    )
}

/// w28: the w06 add/cancel stream on the ladder book (window covers the
/// whole ±500-tick book).
fn ladder_add_cancel() -> Report {
    add_cancel_latency(
        "w28.ladder_add_cancel",
        "Ladder book add then cancel",
        LadderBook::new(2_048, RESTING + 1),
        0x0606,
    )
}

/// w28: add/cancel pairs per second, ladder book ÷ w06 book, on the same
/// stream (median of 5 alternating measurements).
fn ladder_speedup() -> Report {
    let (book, pairs) =
        add_cancel_stream(Book::with_capacity(RESTING + 1, 2 * SPAN as usize), 0x2828);
    let (ladder, same) = add_cancel_stream(LadderBook::new(2_048, RESTING + 1), 0x2828);
    assert!(pairs == same, "both books get the same stream");
    let speedup = interleaved_ratio(
        5,
        || add_cancel_rate(&ladder, &pairs),
        || add_cancel_rate(&book, &pairs),
    );
    benchkit::ratio("w28.speedup", "Ladder vs w06 book", speedup, 1.0)
}

/// One op = add a new order at a random level, then cancel a random resting
/// order elsewhere in the book (so the book stays at 100k and the cancel
/// touches cold memory, as real cancels do).
fn add_cancel() -> Report {
    let mut rng = Rng::new(0x0606);
    let (mut book, mut live) = full_book(&mut rng);
    let mut next_id = RESTING as u64 + 1;
    benchkit::latency(
        "w06.add_cancel",
        "Add then cancel, 100k resting orders",
        Unit::Ns,
        2_000_000,
        || {
            let order = random_order(&mut rng, next_id);
            next_id += 1;
            let slot = rng.range(0, live.len() as u64 - 1) as usize;
            let victim = std::mem::replace(&mut live[slot], order.id);
            (order, victim)
        },
        |(order, victim)| {
            book.add(order).unwrap();
            black_box(book.cancel(victim).unwrap());
        },
    )
}

/// Best bid and best ask (with aggregate quantity) on the 100k book.
fn best_price() -> Report {
    let mut rng = Rng::new(0x0607);
    let (book, _) = full_book(&mut rng);
    benchkit::latency_batched(
        "w06.best_price",
        "Best bid/ask query",
        Unit::Ns,
        200_000,
        64,
        || {
            let b = black_box(&book);
            black_box(b.best_bid());
            black_box(b.best_ask());
        },
    )
}

fn main() {
    benchkit::run(&[
        (
            "w06.add_cancel",
            "Add then cancel, 100k resting orders",
            add_cancel,
        ),
        ("w06.best_price", "Best bid/ask query", best_price),
        (
            "w27.add_cancel_slab",
            "Add then cancel with slab storage",
            add_cancel_slab,
        ),
        (
            "w28.ladder_add_cancel",
            "Ladder book add then cancel",
            ladder_add_cancel,
        ),
        ("w28.speedup", "Ladder vs w06 book", ladder_speedup),
    ]);
}
