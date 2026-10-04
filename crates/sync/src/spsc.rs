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

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::pad::CachePadded;
use std::cell::UnsafeCell;
use std::fmt;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

// TODO(w01): implement — see the week guide on tepy.dev/hft.

/// The pushing end of an SPSC ring. Owned by exactly one thread at a time
/// (it is `Send`, not `Clone`).
pub struct Producer<T> {
    // TODO(w01): design this type's fields.
    _todo: std::marker::PhantomData<(T,)>,
}

/// The popping end of an SPSC ring. Owned by exactly one thread at a time
/// (it is `Send`, not `Clone`).
pub struct Consumer<T> {
    // TODO(w01): design this type's fields.
    _todo: std::marker::PhantomData<(T,)>,
}

/// Creates a ring holding up to `capacity` items, rounded up to the next
/// power of two (minimum 1) so slot lookup is a mask, not a division.
/// Allocates `capacity × size_of::<T>()` bytes once; pushes and pops never
/// allocate.
///
/// Panics if the rounded capacity overflows `usize`.
#[allow(unused_variables)]
pub fn channel<T>(capacity: usize) -> (Producer<T>, Consumer<T>) {
    todo!("w01: channel")
}

#[allow(unused_variables)]
impl<T> Producer<T> {
    /// Pushes `value`, or hands it back if the ring is full. Wait-free:
    /// one slot write and one `Release` store; reads the consumer's index
    /// only when the cached copy says the ring is full.
    #[inline]
    pub fn try_push(&mut self, value: T) -> Result<(), T> {
        todo!("w01: try_push")
    }

    /// Maximum number of items the ring holds (a power of two).
    pub fn capacity(&self) -> usize {
        todo!("w01: capacity")
    }

    /// Items currently queued (exact from the producer's view: the
    /// consumer can only make it smaller).
    pub fn len(&self) -> usize {
        todo!("w01: len")
    }

    /// True if nothing is queued.
    pub fn is_empty(&self) -> bool {
        todo!("w01: is_empty")
    }

    /// True if a push would fail right now.
    pub fn is_full(&self) -> bool {
        todo!("w01: is_full")
    }
}

#[allow(unused_variables)]
impl<T> Consumer<T> {
    /// Pops the oldest item, or `None` if the ring is empty. Wait-free: one
    /// slot read and one `Release` store; reads the producer's index only
    /// when the cached copy says the ring is empty.
    #[inline]
    pub fn try_pop(&mut self) -> Option<T> {
        todo!("w01: try_pop")
    }

    /// Maximum number of items the ring holds (a power of two).
    pub fn capacity(&self) -> usize {
        todo!("w01: capacity")
    }

    /// Items currently queued (exact from the consumer's view: the
    /// producer can only make it larger).
    pub fn len(&self) -> usize {
        todo!("w01: len")
    }

    /// True if nothing is queued.
    pub fn is_empty(&self) -> bool {
        todo!("w01: is_empty")
    }
}

/// Byte offsets of the producer index (`head`) and the consumer index
/// (`tail`) inside the ring's shared state, for element type `T`.
///
/// Week 4 refits the ring so these are at least a cache line apart: if they
/// share a line, every push invalidates the line the consumer polls (and
/// every pop the producer's), even with no data contention.
#[allow(unused_variables)]
pub fn index_offsets<T>() -> (usize, usize) {
    todo!("w04: index_offsets")
}

#[allow(unused_variables)]
impl<T: Copy> Producer<T> {
    /// Pushes as many items from the front of `items` as fit and returns
    /// how many were pushed (0 if the ring is full). One `Release` store
    /// publishes the whole batch; the copy is at most two `memcpy`s.
    pub fn push_slice(&mut self, items: &[T]) -> usize {
        todo!("w05: push_slice")
    }
}

#[allow(unused_variables)]
impl<T: Copy> Consumer<T> {
    /// Pops up to `out.len()` items into the front of `out` and returns how
    /// many were popped (0 if the ring is empty). One `Release` store
    /// releases the whole batch; the copy is at most two `memcpy`s.
    pub fn pop_into(&mut self, out: &mut [T]) -> usize {
        todo!("w05: pop_into")
    }
}

#[allow(unused_variables)]
impl<T> Consumer<T> {
    /// Moves up to `max` items onto the end of `out` and returns how many
    /// were moved. Works for any `T` (no `Copy` needed); reserves space in
    /// `out` first, so it allocates only if `out` must grow, and publishes
    /// one index update for the whole batch.
    pub fn pop_batch(&mut self, out: &mut Vec<T>, max: usize) -> usize {
        todo!("w05: pop_batch")
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
