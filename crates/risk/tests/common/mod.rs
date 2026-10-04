//! Test support shared by the week test files: an allocation counter, to
//! check that hot-path calls never allocate.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

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

/// Number of allocations `f` made on this thread.
#[allow(dead_code)]
pub fn allocations_in(f: impl FnOnce()) -> u64 {
    let before = ALLOCATIONS.with(|c| c.get());
    f();
    ALLOCATIONS.with(|c| c.get()) - before
}
