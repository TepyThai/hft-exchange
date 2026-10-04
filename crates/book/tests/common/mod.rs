//! Test support for the w27 / w28 book variants: a counting global
//! allocator (to prove operations never allocate) and a differential driver
//! that runs random operations on a book and on the w06 [`Book`] side by
//! side, comparing every result and, periodically, the whole book.

#![allow(dead_code)]

use book::{Book, LevelInfo, OrderBook, RestingOrder};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use types::{OrderId, Qty, Rng, Side};

/// The system allocator, counting allocations made by the current thread.
pub struct Counting;

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

fn count() {
    // `try_with` fails only while the thread is being torn down.
    let _ = ALLOCATIONS.try_with(|c| c.set(c.get() + 1));
}

// SAFETY: every method forwards to `System` with the caller's arguments
// unchanged, so `System`'s guarantees carry over; the counter is a
// const-initialized thread-local `Cell<u64>` (no allocation, no destructor),
// so counting cannot recurse into the allocator.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: forwarded unchanged (see above).
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: forwarded unchanged (see above).
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        // SAFETY: forwarded unchanged (see above).
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged (see above).
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Number of allocations (including reallocations) `f` made on this thread.
pub fn allocations_in(f: impl FnOnce()) -> u64 {
    let before = ALLOCATIONS.with(|c| c.get());
    f();
    ALLOCATIONS.with(|c| c.get()) - before
}

/// Every resting order of `side`, in priority order.
pub fn orders(book: &dyn OrderBook, side: Side) -> Vec<RestingOrder> {
    let mut out = Vec::new();
    book.visit_orders(side, &mut |o| {
        out.push(o);
        true
    });
    out
}

/// Every level of `side`, best first.
pub fn levels(book: &dyn OrderBook, side: Side) -> Vec<LevelInfo> {
    let mut out = Vec::new();
    book.visit_levels(side, &mut |l| {
        out.push(l);
        true
    });
    out
}

/// Asserts that `got` holds exactly what `want` holds: orders in priority
/// order, levels, best, counts, per-level FIFOs and per-id lookups.
pub fn assert_same(got: &dyn OrderBook, want: &dyn OrderBook, ctx: &str) {
    assert_eq!(got.len(), want.len(), "{ctx}: len");
    for side in [Side::Buy, Side::Sell] {
        let w = orders(want, side);
        assert_eq!(orders(got, side), w, "{ctx}: {side:?} orders");
        let wl = levels(want, side);
        assert_eq!(levels(got, side), wl, "{ctx}: {side:?} levels");
        assert_eq!(
            got.level_count(side),
            want.level_count(side),
            "{ctx}: level count"
        );
        assert_eq!(got.best(side), want.best(side), "{ctx}: {side:?} best");
        assert_eq!(got.front(side), want.front(side), "{ctx}: {side:?} front");
        for l in &wl {
            assert_eq!(got.level(side, l.price), Some(*l), "{ctx}: level {l:?}");
            let (mut a, mut b) = (Vec::new(), Vec::new());
            got.level_orders(side, l.price, &mut a);
            want.level_orders(side, l.price, &mut b);
            assert_eq!(a, b, "{ctx}: FIFO at {:?}", l.price);
        }
        for o in &w {
            assert_eq!(got.get(o.id), Some(*o), "{ctx}: get {:?}", o.id);
        }
    }
}

/// Runs `steps` random operations on `subject` and on a fresh w06 `Book`,
/// asserting identical results after every operation and identical books
/// every `check_every` steps. `price` draws a price for a side.
///
/// The mix: 45% adds (5% reusing an existing id), 25% cancels (some of
/// unknown ids), 12% reduces (some invalid), 18% fills of the front order.
pub fn differential(
    subject: &mut dyn OrderBook,
    seed: u64,
    steps: usize,
    check_every: usize,
    mut price: impl FnMut(&mut Rng, Side) -> i64,
) {
    let mut rng = Rng::new(seed);
    let mut model = Book::new();
    let mut next_id = 1u64;
    for step in 0..steps {
        let ctx = format!("seed {seed:#x} step {step}");
        let r = rng.range(0, 99);
        if r < 45 {
            let side = rng.side();
            let p = price(&mut rng, side);
            let qty = rng.range(1, 20);
            let id = if rng.chance(0.05) && next_id > 1 {
                rng.range(1, next_id - 1)
            } else {
                next_id += 1;
                next_id - 1
            };
            let o = RestingOrder::new(id, side, p, qty);
            assert_eq!(subject.add(o), model.add(o), "{ctx}: add {o:?}");
        } else if r < 70 {
            let id = OrderId(rng.range(1, next_id));
            assert_eq!(subject.cancel(id), model.cancel(id), "{ctx}: cancel {id:?}");
        } else if r < 82 {
            let id = OrderId(rng.range(1, next_id));
            let qty = Qty(rng.range(0, 20));
            assert_eq!(
                subject.reduce(id, qty),
                model.reduce(id, qty),
                "{ctx}: reduce {id:?} to {qty:?}"
            );
        } else {
            let side = rng.side();
            let front = model.front(side);
            assert_eq!(subject.front(side), front, "{ctx}: front {side:?}");
            if let Some(f) = front {
                let fill = Qty(rng.range(1, f.qty.0));
                assert_eq!(
                    subject.fill_front(side, fill),
                    model.fill_front(side, fill),
                    "{ctx}: fill {side:?} {fill:?}"
                );
            }
        }
        if step % check_every == 0 {
            assert_same(subject, &model, &ctx);
        }
    }
    assert_same(subject, &model, "end");
}
