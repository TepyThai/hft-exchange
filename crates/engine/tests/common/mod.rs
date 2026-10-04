//! Test support shared by the performance weeks' test files: a counting
//! global allocator (to prove hot paths never allocate) and a self-contained
//! order-stream generator (built from `types::Rng` only, so these tests do
//! not depend on the learner's w12 generator).

#![allow(dead_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use types::{Command, NewOrder, OrderId, Qty, Rng, Side, TimeInForce};

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

/// A w07-style stream around a drifting mid, generated without looking at
/// any engine (so it is the same for every implementation): ~45% passive
/// GTC limits 1..=40 ticks behind the mid (most near the touch), ~20%
/// aggressive limits crossing the mid by up to 3 ticks (half IOC), ~3%
/// market orders, ~25% cancels and ~5% reduces of previously issued GTC
/// ids (some already filled, so some cancels are rejected), ~2% post-only.
/// Above ~1000 issued-and-not-cancelled ids, half of all commands cancel,
/// so the book reaches a steady state.
pub fn mixed_stream(seed: u64, n: usize) -> Vec<Command> {
    let mut rng = Rng::new(seed);
    let mut mid: i64 = 100_000;
    let mut issued: Vec<OrderId> = Vec::new();
    let mut next_id = 1u64;
    let mut cmds = Vec::with_capacity(n);
    while cmds.len() < n {
        if rng.chance(0.05) {
            mid += if rng.next_u64() & 1 == 0 { 1 } else { -1 };
        }
        let crowded = issued.len() > 1_000 && rng.chance(0.5);
        let r = rng.range(0, 99);
        let cmd = if crowded || r < 25 {
            if issued.is_empty() {
                continue;
            }
            let i = rng.range(0, issued.len() as u64 - 1) as usize;
            Command::Cancel {
                id: issued.swap_remove(i),
            }
        } else if r < 30 {
            if issued.is_empty() {
                continue;
            }
            let id = issued[rng.range(0, issued.len() as u64 - 1) as usize];
            Command::Reduce {
                id,
                qty: Qty(rng.range(1, 6)),
            }
        } else if r < 50 {
            let side = rng.side();
            let through = rng.range_i64(0, 3);
            let price = if side == Side::Buy {
                mid + through
            } else {
                mid - through
            };
            let tif = if rng.chance(0.5) {
                TimeInForce::Ioc
            } else {
                TimeInForce::Gtc
            };
            let order = NewOrder::limit(next_id, side, price, rng.range(1, 20)).with_tif(tif);
            if tif == TimeInForce::Gtc {
                issued.push(order.id);
            }
            next_id += 1;
            Command::New(order)
        } else if r < 53 {
            let order = NewOrder::market(next_id, rng.side(), rng.range(1, 20));
            next_id += 1;
            Command::New(order)
        } else {
            let side = rng.side();
            let depth = 1 + rng.range(0, 39).min(rng.range(0, 39)).min(rng.range(0, 39)) as i64;
            let price = if side == Side::Buy {
                mid - depth
            } else {
                mid + depth
            };
            let mut order = NewOrder::limit(next_id, side, price, rng.range(1, 10));
            if rng.chance(0.04) {
                order = order.post_only();
            }
            issued.push(order.id);
            next_id += 1;
            Command::New(order)
        };
        cmds.push(cmd);
    }
    cmds
}

/// Folds events into an order-sensitive 64-bit digest (field by field, so
/// it does not depend on `Event`'s memory layout or `Hash` impl).
pub fn digest_events(h: &mut engine::StateHasher, events: &[types::Event]) {
    use types::Event;
    let side = |s: Side| if s == Side::Buy { 1 } else { 2 };
    for e in events {
        match *e {
            Event::Accepted { id } => {
                h.write_u64(1);
                h.write_u64(id.0);
            }
            Event::Rejected { id, reason } => {
                h.write_u64(2);
                h.write_u64(id.0);
                h.write_u64(reject_code(reason));
            }
            Event::Trade {
                taker,
                maker,
                taker_side,
                price,
                qty,
            } => {
                h.write_u64(3);
                h.write_u64(taker.0);
                h.write_u64(maker.0);
                h.write_u64(side(taker_side));
                h.write_i64(price.0);
                h.write_u64(qty.0);
            }
            Event::Rested {
                id,
                side: s,
                price,
                qty,
            } => {
                h.write_u64(4);
                h.write_u64(id.0);
                h.write_u64(side(s));
                h.write_i64(price.0);
                h.write_u64(qty.0);
            }
            Event::Cancelled { id, remaining } => {
                h.write_u64(5);
                h.write_u64(id.0);
                h.write_u64(remaining.0);
            }
            Event::Expired { id, remaining } => {
                h.write_u64(6);
                h.write_u64(id.0);
                h.write_u64(remaining.0);
            }
            Event::Reduced { id, remaining } => {
                h.write_u64(7);
                h.write_u64(id.0);
                h.write_u64(remaining.0);
            }
        }
    }
}

fn reject_code(r: types::RejectReason) -> u64 {
    use types::RejectReason::*;
    match r {
        DuplicateOrderId => 1,
        UnknownOrder => 2,
        ZeroQuantity => 3,
        InvalidTick => 4,
        InvalidLot => 5,
        PriceBand => 6,
        WouldTake => 7,
        FokUnfillable => 8,
        NoLiquidity => 9,
        InvalidReduce => 10,
        Risk(_) => 11,
        Halted => 12,
    }
}
