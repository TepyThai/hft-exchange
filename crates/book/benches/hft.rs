//! Benchmarks for the `book` crate. Run with `cargo xtask bench --week N`.

use benchkit::{black_box, Report, Unit};
use book::{Book, OrderBook, RestingOrder};
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
    let mut book = Book::with_capacity(RESTING + 1, 2 * SPAN as usize);
    let mut live = Vec::with_capacity(RESTING);
    for id in 1..=RESTING as u64 {
        book.add(random_order(rng, id)).unwrap();
        live.push(OrderId(id));
    }
    (book, live)
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
    ]);
}
