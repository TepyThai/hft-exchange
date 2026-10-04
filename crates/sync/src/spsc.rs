//! Weeks 1, 4, 5: a bounded, lock-free single-producer single-consumer
//! ring buffer.
//!
//! ```text
//!            tail (consumer)         head (producer)
//!               v                       v
//!   [ . . . . | a | b | c | d | e | . . . . ]   capacity = 2^k
//! ```
//!
//! * `head` counts pushes and is written **only** by the [`Producer`];
//!   `tail` counts pops and is written **only** by the [`Consumer`]. Both
//!   grow without bound (wrapping at 2^64); the slot of index `i` is
//!   `i & (capacity - 1)`, and `head - tail` is the number of queued items.
//! * The producer writes a slot, then publishes it with a `Release` store
//!   of `head`; the consumer's `Acquire` load of `head` makes the slot's
//!   contents visible. Symmetrically, the consumer's `Release` store of
//!   `tail` hands the slot back to the producer.
//! * Week 4: `head` and `tail` live on separate cache lines
//!   ([`CachePadded`](crate::CachePadded)), so the producer's stores don't
//!   invalidate the line the consumer is polling and vice versa
//!   ([`index_offsets`] lets tests check the layout).
//! * Week 5: each side keeps a *cached* copy of the other side's index and
//!   only re-reads the shared one when the cache says full (producer) or
//!   empty (consumer) — so in steady state a push or pop touches no cache
//!   line owned by the other core. Batch operations ([`Producer::push_slice`],
//!   [`Consumer::pop_into`], [`Consumer::pop_batch`]) publish one index
//!   update for many items.
//!
//! Single producer / single consumer is enforced by the types: the handles
//! are not `Clone`, and every operation that moves an index takes
//! `&mut self`.
//!
//! ```
//! let (mut tx, mut rx) = sync::spsc::channel::<u64>(4);
//! assert!(tx.try_push(1).is_ok());
//! assert_eq!(rx.try_pop(), Some(1));
//! assert_eq!(rx.try_pop(), None);
//! ```

use crate::pad::CachePadded;
use std::cell::UnsafeCell;
use std::fmt;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

// @student-begin w01
/// State shared by the two handles.
struct Shared<T> {
    /// Total pushes. Written only by the producer (`Release`), read by the
    /// consumer (`Acquire`).
    head: CachePadded<AtomicUsize>,
    /// Total pops. Written only by the consumer (`Release`), read by the
    /// producer (`Acquire`).
    tail: CachePadded<AtomicUsize>,
    /// `capacity - 1`; capacity is a power of two.
    mask: usize,
    /// Slot `i` is initialized iff `tail <= i' < head` for the index `i'`
    /// with `i' & mask == i`.
    buf: Box<[UnsafeCell<MaybeUninit<T>>]>,
}

// SAFETY: the ring moves `T` values from the producer thread to the
// consumer thread, so `T: Send` is required and sufficient. Each slot is
// accessed by at most one side at a time: the producer only writes slots in
// `[head, tail + capacity)`, the consumer only reads slots in `[tail, head)`,
// and ownership of a slot changes hands only through the Release/Acquire
// index stores and loads.
unsafe impl<T: Send> Send for Shared<T> {}
// SAFETY: see `Send` above — shared access never lets both sides touch the
// same slot concurrently.
unsafe impl<T: Send> Sync for Shared<T> {}

impl<T> Shared<T> {
    fn capacity(&self) -> usize {
        self.mask + 1
    }

    /// Raw pointer to the slot holding index `i`.
    fn slot(&self, i: usize) -> *mut T {
        self.buf[i & self.mask].get().cast::<T>()
    }

    /// Raw pointer to slot 0, valid for the whole buffer (for bulk copies).
    fn base(&self) -> *mut T {
        UnsafeCell::raw_get(self.buf.as_ptr()).cast::<T>()
    }
}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        // Both handles are gone, so `&mut self` is exclusive and the
        // indices are final.
        let head = *self.head.get_mut();
        let mut i = *self.tail.get_mut();
        while i != head {
            // SAFETY: every index in [tail, head) was written by the
            // producer and not yet read by the consumer, so the slot holds
            // an initialized `T` that nobody else will drop.
            unsafe { std::ptr::drop_in_place(self.slot(i)) };
            i = i.wrapping_add(1);
        }
    }
}

/// Copies `n` items between `ring` (starting at ring index `start`) and the
/// linear buffer `linear`, splitting the copy at the wrap point.
///
/// # Safety
///
/// `ring` must point at a buffer of `mask + 1` `T`s; the caller must own
/// the ring slots `start .. start + n` (for the direction being copied),
/// `n <= mask + 1`, and `linear` must be valid for `n` items.
unsafe fn copy_wrapping<T: Copy>(
    ring: *mut T,
    mask: usize,
    start: usize,
    linear: *mut T,
    n: usize,
    to_ring: bool,
) {
    let first_slot = start & mask;
    let first = n.min(mask + 1 - first_slot);
    // SAFETY: (both copies) the caller guarantees the ring has `mask + 1`
    // slots and `linear` holds `n` items; `first_slot + first <= mask + 1`
    // and `n - first <= first_slot`, so every pointer stays in bounds. The
    // regions belong to different allocations, so they don't overlap.
    unsafe {
        if to_ring {
            std::ptr::copy_nonoverlapping(linear, ring.add(first_slot), first);
            std::ptr::copy_nonoverlapping(linear.add(first), ring, n - first);
        } else {
            std::ptr::copy_nonoverlapping(ring.add(first_slot), linear, first);
            std::ptr::copy_nonoverlapping(ring, linear.add(first), n - first);
        }
    }
}
// @student-end

/// The pushing end of an SPSC ring. Owned by exactly one thread at a time
/// (it is `Send`, not `Clone`).
// @student w01
pub struct Producer<T> {
    shared: Arc<Shared<T>>,
    /// Local copy of `head`: only this handle writes it.
    head: usize,
    /// Last value of `tail` we read; refreshed only when the ring looks
    /// full (w05).
    cached_tail: usize,
}

/// The popping end of an SPSC ring. Owned by exactly one thread at a time
/// (it is `Send`, not `Clone`).
// @student w01
pub struct Consumer<T> {
    shared: Arc<Shared<T>>,
    /// Local copy of `tail`: only this handle writes it.
    tail: usize,
    /// Last value of `head` we read; refreshed only when the ring looks
    /// empty (w05).
    cached_head: usize,
}

/// Creates a ring holding up to `capacity` items, rounded up to the next
/// power of two (minimum 1) so slot lookup is a mask, not a division.
/// Allocates `capacity × size_of::<T>()` bytes once; pushes and pops never
/// allocate.
///
/// Panics if the rounded capacity overflows `usize`.
// @student w01
pub fn channel<T>(capacity: usize) -> (Producer<T>, Consumer<T>) {
    let cap = capacity
        .max(1)
        .checked_next_power_of_two()
        .expect("spsc capacity overflows usize");
    let buf = (0..cap)
        .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
        .collect();
    let shared = Arc::new(Shared {
        head: CachePadded::new(AtomicUsize::new(0)),
        tail: CachePadded::new(AtomicUsize::new(0)),
        mask: cap - 1,
        buf,
    });
    (
        Producer {
            shared: Arc::clone(&shared),
            head: 0,
            cached_tail: 0,
        },
        Consumer {
            shared,
            tail: 0,
            cached_head: 0,
        },
    )
}

// @student w01
impl<T> Producer<T> {
    /// Pushes `value`, or hands it back if the ring is full. Wait-free:
    /// one slot write and one `Release` store; reads the consumer's index
    /// only when the cached copy says the ring is full.
    #[inline]
    pub fn try_push(&mut self, value: T) -> Result<(), T> {
        let cap = self.shared.capacity();
        if self.head.wrapping_sub(self.cached_tail) == cap {
            // Looks full: refresh. Acquire pairs with the consumer's
            // Release store, so its reads of the freed slots are done.
            self.cached_tail = self.shared.tail.load(Ordering::Acquire);
            if self.head.wrapping_sub(self.cached_tail) == cap {
                return Err(value);
            }
        }
        // SAFETY: head - tail < capacity, so the slot for `head` is free:
        // the consumer has finished with it (Acquire above, or an earlier
        // one) and won't touch it until we publish `head + 1`.
        unsafe { self.shared.slot(self.head).write(value) };
        self.head = self.head.wrapping_add(1);
        self.shared.head.store(self.head, Ordering::Release);
        Ok(())
    }

    /// Maximum number of items the ring holds (a power of two).
    pub fn capacity(&self) -> usize {
        self.shared.capacity()
    }

    /// Items currently queued (exact from the producer's view: the
    /// consumer can only make it smaller).
    pub fn len(&self) -> usize {
        self.head
            .wrapping_sub(self.shared.tail.load(Ordering::Acquire))
    }

    /// True if nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// True if a push would fail right now.
    pub fn is_full(&self) -> bool {
        self.len() == self.capacity()
    }
}

// @student w01
impl<T> Consumer<T> {
    /// Pops the oldest item, or `None` if the ring is empty. Wait-free: one
    /// slot read and one `Release` store; reads the producer's index only
    /// when the cached copy says the ring is empty.
    #[inline]
    pub fn try_pop(&mut self) -> Option<T> {
        if self.tail == self.cached_head {
            // Looks empty: refresh. Acquire pairs with the producer's
            // Release store, making the slot's contents visible.
            self.cached_head = self.shared.head.load(Ordering::Acquire);
            if self.tail == self.cached_head {
                return None;
            }
        }
        // SAFETY: tail < head, so the slot for `tail` holds an initialized
        // value published by the producer (Acquire above), which won't
        // touch it again until we publish `tail + 1`. We read it out
        // exactly once.
        let value = unsafe { self.shared.slot(self.tail).read() };
        self.tail = self.tail.wrapping_add(1);
        self.shared.tail.store(self.tail, Ordering::Release);
        Some(value)
    }

    /// Maximum number of items the ring holds (a power of two).
    pub fn capacity(&self) -> usize {
        self.shared.capacity()
    }

    /// Items currently queued (exact from the consumer's view: the
    /// producer can only make it larger).
    pub fn len(&self) -> usize {
        self.shared
            .head
            .load(Ordering::Acquire)
            .wrapping_sub(self.tail)
    }

    /// True if nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Byte offsets of the producer index (`head`) and the consumer index
/// (`tail`) inside the ring's shared state, for element type `T`.
///
/// Week 4 refits the ring so these are at least a cache line apart: if they
/// share a line, every push invalidates the line the consumer polls (and
/// every pop the producer's), even with no data contention.
// @student w04
pub fn index_offsets<T>() -> (usize, usize) {
    (
        std::mem::offset_of!(Shared<T>, head),
        std::mem::offset_of!(Shared<T>, tail),
    )
}

// @student w05
impl<T: Copy> Producer<T> {
    /// Pushes as many items from the front of `items` as fit and returns
    /// how many were pushed (0 if the ring is full). One `Release` store
    /// publishes the whole batch; the copy is at most two `memcpy`s.
    pub fn push_slice(&mut self, items: &[T]) -> usize {
        let cap = self.shared.capacity();
        let mut free = cap - self.head.wrapping_sub(self.cached_tail);
        if free < items.len() {
            self.cached_tail = self.shared.tail.load(Ordering::Acquire);
            free = cap - self.head.wrapping_sub(self.cached_tail);
        }
        let n = free.min(items.len());
        if n == 0 {
            return 0;
        }
        // SAFETY: the `n <= free` slots starting at `head` are owned by the
        // producer (see `try_push`); `items` holds at least `n` values and
        // `T: Copy`, so a bitwise copy is a valid move with no drop
        // obligations.
        unsafe {
            copy_wrapping(
                self.shared.base(),
                self.shared.mask,
                self.head,
                items.as_ptr().cast_mut(),
                n,
                true,
            )
        };
        self.head = self.head.wrapping_add(n);
        self.shared.head.store(self.head, Ordering::Release);
        n
    }
}

// @student w05
impl<T: Copy> Consumer<T> {
    /// Pops up to `out.len()` items into the front of `out` and returns how
    /// many were popped (0 if the ring is empty). One `Release` store
    /// releases the whole batch; the copy is at most two `memcpy`s.
    pub fn pop_into(&mut self, out: &mut [T]) -> usize {
        let mut avail = self.cached_head.wrapping_sub(self.tail);
        if avail < out.len() {
            self.cached_head = self.shared.head.load(Ordering::Acquire);
            avail = self.cached_head.wrapping_sub(self.tail);
        }
        let n = avail.min(out.len());
        if n == 0 {
            return 0;
        }
        // SAFETY: the `n <= avail` slots starting at `tail` hold initialized
        // values published by the producer (Acquire above) that it won't
        // touch until we publish the new tail; `out` has room for `n` and
        // `T: Copy`, so copying the bits out leaves nothing to drop.
        unsafe {
            copy_wrapping(
                self.shared.base(),
                self.shared.mask,
                self.tail,
                out.as_mut_ptr(),
                n,
                false,
            )
        };
        self.tail = self.tail.wrapping_add(n);
        self.shared.tail.store(self.tail, Ordering::Release);
        n
    }
}

// @student w05
impl<T> Consumer<T> {
    /// Moves up to `max` items onto the end of `out` and returns how many
    /// were moved. Works for any `T` (no `Copy` needed); reserves space in
    /// `out` first, so it allocates only if `out` must grow, and publishes
    /// one index update for the whole batch.
    pub fn pop_batch(&mut self, out: &mut Vec<T>, max: usize) -> usize {
        let mut avail = self.cached_head.wrapping_sub(self.tail);
        if avail < max {
            self.cached_head = self.shared.head.load(Ordering::Acquire);
            avail = self.cached_head.wrapping_sub(self.tail);
        }
        let n = avail.min(max);
        out.reserve(n);
        for k in 0..n {
            // SAFETY: as in `try_pop`, each slot in [tail, tail + n) holds
            // an initialized value we now own; each is read exactly once,
            // and `push` cannot panic or reallocate after `reserve(n)`.
            out.push(unsafe { self.shared.slot(self.tail.wrapping_add(k)).read() });
        }
        self.tail = self.tail.wrapping_add(n);
        self.shared.tail.store(self.tail, Ordering::Release);
        n
    }
}

impl<T> fmt::Debug for Producer<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("spsc::Producer")
            .field("capacity", &self.capacity())
            .field("len", &self.len())
            .finish()
    }
}

impl<T> fmt::Debug for Consumer<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("spsc::Consumer")
            .field("capacity", &self.capacity())
            .field("len", &self.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    /// Spins briefly, then yields — keeps the two threads tightly
    /// interleaved without livelocking a machine with fewer cores than
    /// threads.
    fn backoff(spins: &mut u32) {
        *spins += 1;
        if *spins < 64 {
            std::hint::spin_loop();
        } else {
            *spins = 0;
            std::thread::yield_now();
        }
    }

    /// Moves `n` sequence numbers through a small ring across two threads
    /// and checks every one arrives exactly once, in order.
    fn stress_once(n: u64, capacity: usize) {
        let (mut tx, mut rx) = super::channel::<u64>(capacity);
        let producer = std::thread::spawn(move || {
            let (mut i, mut spins) = (0, 0);
            while i < n {
                match tx.try_push(i) {
                    Ok(()) => i += 1,
                    Err(_) => backoff(&mut spins),
                }
            }
        });
        let (mut expected, mut spins) = (0, 0);
        while expected < n {
            match rx.try_pop() {
                Some(v) => {
                    assert_eq!(v, expected, "item out of order or duplicated");
                    expected += 1;
                }
                None => backoff(&mut spins),
            }
        }
        producer.join().unwrap();
        assert_eq!(rx.try_pop(), None, "extra item after the last one");
    }

    #[test]
    fn w01_spsc_stress_repeated_20x() {
        for round in 0..20 {
            stress_once(250_000, 16 << (round % 4));
        }
    }
}
