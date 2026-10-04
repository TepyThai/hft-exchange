//! Week 5: a bounded multi-producer single-consumer queue with per-slot
//! sequence numbers (Dmitry Vyukov's bounded queue, specialised to one
//! consumer).
//!
//! Each slot carries a sequence number that says whose turn it is:
//!
//! * `seq == pos` — the slot is free for the producer that claims ring
//!   position `pos`;
//! * `seq == pos + 1` — the slot holds the item for position `pos`, ready
//!   for the consumer;
//! * after popping, the consumer sets `seq = pos + capacity`, handing the
//!   slot to the producer one lap later.
//!
//! Producers claim positions on the shared `enqueue` counter, then write
//! their slot and publish it with a `Release` store of its sequence number
//! — so producers only contend on that one counter, never on a lock. The
//! single consumer needs no read-modify-write at all.
//!
//! Two ways to claim:
//!
//! * [`Producer::try_push`] — `compare_exchange`, only after seeing the slot
//!   is free, so it can fail cleanly when the queue is full. Under heavy
//!   contention most CASes lose and retry, each retry costing another
//!   cache-line round trip.
//! * [`Producer::push`] — `fetch_add`, which never fails: one round trip per
//!   claim however many producers contend, then wait (spin) until the slot
//!   is free. Use it when back-pressure (waiting while the queue is full) is
//!   the right behaviour, as for gateway sessions feeding the engine; on
//!   the reference machine it is ~1.7× the throughput of `try_push` with
//!   four producers on different cores.
//!
//! Progress caveat: a producer that claims a position and is then
//! descheduled before publishing makes the consumer see the queue as empty
//! at that position until it resumes (later items wait behind it, FIFO is
//! preserved). That is the price of a fixed array without per-item
//! allocation, and why the gateway pins its threads (week 30).
//!
//! ```
//! let (tx, mut rx) = sync::mpsc::channel::<u32>(8);
//! let tx2 = tx.clone();
//! tx.try_push(1).unwrap();
//! tx2.try_push(2).unwrap();
//! assert_eq!(rx.try_pop(), Some(1));
//! assert_eq!(rx.try_pop(), Some(2));
//! ```

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::pad::CachePadded;
use std::cell::UnsafeCell;
use std::fmt;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

// TODO(w05): implement — see the week guide on tepy.dev/hft.

/// A producing end. `Clone` it to add producers; all clones push into the
/// same queue. `Send + Sync`: producers may also share one handle by
/// reference.
pub struct Producer<T> {
    // TODO(w05): design this type's fields.
    _todo: std::marker::PhantomData<(T,)>,
}

/// The single consuming end (not `Clone`).
pub struct Consumer<T> {
    // TODO(w05): design this type's fields.
    _todo: std::marker::PhantomData<(T,)>,
}

/// Creates a queue holding up to `capacity` items, rounded up to a power of
/// two (minimum 2). Allocates the slot array once; pushes and pops never
/// allocate.
///
/// Panics if the rounded capacity overflows `usize`.
#[allow(unused_variables)]
pub fn channel<T>(capacity: usize) -> (Producer<T>, Consumer<T>) {
    todo!("w05: channel")
}

#[allow(unused_variables)]
impl<T> Producer<T> {
    /// Pushes `value`, or hands it back if the queue is full. Never waits.
    /// Lock-free: one CAS on the shared position (retried when another
    /// producer wins the race — see [`push`](Self::push) for the
    /// high-contention alternative), one slot write, one `Release` store.
    pub fn try_push(&self, value: T) -> Result<(), T> {
        todo!("w05: try_push")
    }

    /// Pushes `value`, waiting while the queue is full (back-pressure: spins
    /// briefly, then yields between checks).
    /// Claims a position with one `fetch_add` — no retry loop however many
    /// producers contend — then waits for that slot to be free, writes it,
    /// and publishes it with one `Release` store. Producers waiting on a
    /// full queue are served in claim order.
    ///
    /// Returns the value back only if the consumer has been dropped (the
    /// space would never come).
    pub fn push(&self, value: T) -> Result<(), T> {
        todo!("w05: push")
    }

    /// Maximum number of items the queue holds (a power of two).
    pub fn capacity(&self) -> usize {
        todo!("w05: capacity")
    }

    /// Approximate number of queued items (claimed but unpopped). A
    /// monitoring value: it can be stale by the time you read it.
    pub fn len(&self) -> usize {
        todo!("w05: len")
    }

    /// True if [`len`](Self::len) is zero.
    pub fn is_empty(&self) -> bool {
        todo!("w05: is_empty")
    }
}

#[allow(unused_variables)]
impl<T> Clone for Producer<T> {
    /// Another producer on the same queue (an `Arc` clone).
    fn clone(&self) -> Self {
        todo!("w05: clone")
    }
}

#[allow(unused_variables)]
impl<T> Consumer<T> {
    /// Pops the oldest published item, or `None` if the next position is
    /// not published yet. Wait-free: no CAS, one `Acquire` load, one slot
    /// read, one `Release` store.
    pub fn try_pop(&mut self) -> Option<T> {
        todo!("w05: try_pop")
    }

    /// Maximum number of items the queue holds (a power of two).
    pub fn capacity(&self) -> usize {
        todo!("w05: capacity")
    }

    /// Approximate number of queued items (positions claimed by producers
    /// and not yet popped).
    pub fn len(&self) -> usize {
        todo!("w05: len")
    }

    /// True if [`len`](Self::len) is zero.
    pub fn is_empty(&self) -> bool {
        todo!("w05: is_empty")
    }
}

impl<T> fmt::Debug for Producer<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("mpsc::Producer")
            .field("capacity", &self.capacity())
            .field("len", &self.len())
            .finish()
    }
}

impl<T> fmt::Debug for Consumer<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("mpsc::Consumer")
            .field("capacity", &self.capacity())
            .field("len", &self.len())
            .finish()
    }
}
