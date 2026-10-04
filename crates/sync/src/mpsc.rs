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

use crate::pad::CachePadded;
use std::cell::UnsafeCell;
use std::fmt;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

// @student-begin w05
struct Slot<T> {
    /// Whose turn it is (see the module docs).
    seq: AtomicUsize,
    value: UnsafeCell<MaybeUninit<T>>,
}

struct Shared<T> {
    /// Next position a producer will claim.
    enqueue: CachePadded<AtomicUsize>,
    /// Next position the consumer will pop. Written only by the consumer;
    /// read by `len` and by `Drop`.
    dequeue: CachePadded<AtomicUsize>,
    /// Cleared when the consumer is dropped, so `push` stops waiting for
    /// space that will never come.
    consumer_alive: AtomicBool,
    mask: usize,
    slots: Box<[Slot<T>]>,
}

// SAFETY: values of `T` move from producer threads to the consumer thread
// (`T: Send`). A slot's value is written only by the one producer that
// claimed that position, once it observes `seq == pos`, and read only by the
// consumer after observing `seq == pos + 1` with Acquire; the sequence
// numbers make those accesses mutually exclusive.
unsafe impl<T: Send> Send for Shared<T> {}
// SAFETY: as above.
unsafe impl<T: Send> Sync for Shared<T> {}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        // All handles are gone, so nothing races. A claimed position is not
        // necessarily filled (a `push` that gave up after the consumer was
        // dropped), so trust the slot's sequence number, not the counters:
        // the slot holds the value for `pos` iff `seq == pos + 1`, which
        // matches at most one `pos` per slot.
        let end = *self.enqueue.get_mut();
        let mut pos = *self.dequeue.get_mut();
        while pos != end {
            let slot = &mut self.slots[pos & self.mask];
            if *slot.seq.get_mut() == pos.wrapping_add(1) {
                // SAFETY: `seq == pos + 1` means the producer for `pos`
                // published an initialized `T` that was never popped.
                unsafe { slot.value.get_mut().assume_init_drop() };
                *slot.seq.get_mut() = pos;
            }
            pos = pos.wrapping_add(1);
        }
    }
}

impl<T> Drop for Consumer<T> {
    fn drop(&mut self) {
        self.shared.consumer_alive.store(false, Ordering::Release);
    }
}
// @student-end

/// A producing end. `Clone` it to add producers; all clones push into the
/// same queue. `Send + Sync`: producers may also share one handle by
/// reference.
// @student w05
pub struct Producer<T> {
    shared: Arc<Shared<T>>,
}

/// The single consuming end (not `Clone`).
// @student w05
pub struct Consumer<T> {
    shared: Arc<Shared<T>>,
    /// Local copy of `dequeue` (only this handle advances it).
    pos: usize,
}

/// Creates a queue holding up to `capacity` items, rounded up to a power of
/// two (minimum 2). Allocates the slot array once; pushes and pops never
/// allocate.
///
/// Panics if the rounded capacity overflows `usize`.
// @student w05
pub fn channel<T>(capacity: usize) -> (Producer<T>, Consumer<T>) {
    let cap = capacity
        .max(2)
        .checked_next_power_of_two()
        .expect("mpsc capacity overflows usize");
    let slots = (0..cap)
        .map(|i| Slot {
            seq: AtomicUsize::new(i),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        })
        .collect();
    let shared = Arc::new(Shared {
        enqueue: CachePadded::new(AtomicUsize::new(0)),
        dequeue: CachePadded::new(AtomicUsize::new(0)),
        consumer_alive: AtomicBool::new(true),
        mask: cap - 1,
        slots,
    });
    (
        Producer {
            shared: Arc::clone(&shared),
        },
        Consumer { shared, pos: 0 },
    )
}

// @student w05
impl<T> Producer<T> {
    /// Pushes `value`, or hands it back if the queue is full. Never waits.
    /// Lock-free: one CAS on the shared position (retried when another
    /// producer wins the race — see [`push`](Self::push) for the
    /// high-contention alternative), one slot write, one `Release` store.
    pub fn try_push(&self, value: T) -> Result<(), T> {
        let shared = &*self.shared;
        let mut pos = shared.enqueue.load(Ordering::Relaxed);
        loop {
            let slot = &shared.slots[pos & shared.mask];
            // Acquire: if the slot was just freed by the consumer, its read
            // of the old value happens-before our write.
            let seq = slot.seq.load(Ordering::Acquire);
            let lag = seq.wrapping_sub(pos) as isize;
            if lag == 0 {
                // Our turn for this slot, if we win the position.
                match shared.enqueue.compare_exchange_weak(
                    pos,
                    pos.wrapping_add(1),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // SAFETY: winning the CAS for `pos` while
                        // `seq == pos` gives this producer exclusive
                        // ownership of the (empty) slot until it publishes.
                        unsafe { (*slot.value.get()).write(value) };
                        slot.seq.store(pos.wrapping_add(1), Ordering::Release);
                        return Ok(());
                    }
                    Err(current) => pos = current,
                }
            } else if lag < 0 {
                // The slot still holds the item from one lap ago: full.
                return Err(value);
            } else {
                // Another producer already took `pos`; catch up.
                pos = shared.enqueue.load(Ordering::Relaxed);
            }
        }
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
        let shared = &*self.shared;
        let pos = shared.enqueue.fetch_add(1, Ordering::Relaxed);
        let slot = &shared.slots[pos & shared.mask];
        // Acquire pairs with the consumer's Release when it frees the slot.
        let mut spins = 0u32;
        while slot.seq.load(Ordering::Acquire) != pos {
            if !shared.consumer_alive.load(Ordering::Acquire) {
                // Abandon the claim: nobody will ever read this position,
                // and `Drop` ignores claimed-but-unfilled slots.
                return Err(value);
            }
            // A full queue is the exceptional case: spin briefly, then let
            // the consumer (perhaps sharing this core) run.
            if spins < 128 {
                spins += 1;
                std::hint::spin_loop();
            } else {
                std::thread::yield_now();
            }
        }
        // SAFETY: `fetch_add` gave this producer the unique claim on `pos`,
        // and `seq == pos` means the consumer has finished with the slot's
        // previous value; nobody else touches it until we publish.
        unsafe { (*slot.value.get()).write(value) };
        slot.seq.store(pos.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    /// Maximum number of items the queue holds (a power of two).
    pub fn capacity(&self) -> usize {
        self.shared.mask + 1
    }

    /// Approximate number of queued items (claimed but unpopped). A
    /// monitoring value: it can be stale by the time you read it.
    pub fn len(&self) -> usize {
        let popped = self.shared.dequeue.load(Ordering::Relaxed);
        let claimed = self.shared.enqueue.load(Ordering::Relaxed);
        // The two loads are not a snapshot; clamp a transiently negative
        // or oversized difference.
        (claimed.wrapping_sub(popped) as isize).clamp(0, self.capacity() as isize) as usize
    }

    /// True if [`len`](Self::len) is zero.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// @student w05
impl<T> Clone for Producer<T> {
    /// Another producer on the same queue (an `Arc` clone).
    fn clone(&self) -> Self {
        Producer {
            shared: Arc::clone(&self.shared),
        }
    }
}

// @student w05
impl<T> Consumer<T> {
    /// Pops the oldest published item, or `None` if the next position is
    /// not published yet. Wait-free: no CAS, one `Acquire` load, one slot
    /// read, one `Release` store.
    pub fn try_pop(&mut self) -> Option<T> {
        let shared = &*self.shared;
        let slot = &shared.slots[self.pos & shared.mask];
        // Acquire pairs with the producer's Release store of `seq`.
        if slot.seq.load(Ordering::Acquire) != self.pos.wrapping_add(1) {
            return None;
        }
        // SAFETY: `seq == pos + 1` means the producer for `pos` finished
        // writing and won't touch the slot again until we hand it back
        // below; we read the value exactly once.
        let value = unsafe { (*slot.value.get()).assume_init_read() };
        // Release: our read is done before a producer can reuse the slot.
        slot.seq
            .store(self.pos.wrapping_add(shared.mask + 1), Ordering::Release);
        self.pos = self.pos.wrapping_add(1);
        shared.dequeue.store(self.pos, Ordering::Relaxed);
        Some(value)
    }

    /// Maximum number of items the queue holds (a power of two).
    pub fn capacity(&self) -> usize {
        self.shared.mask + 1
    }

    /// Approximate number of queued items (positions claimed by producers
    /// and not yet popped).
    pub fn len(&self) -> usize {
        self.shared
            .enqueue
            .load(Ordering::Relaxed)
            .wrapping_sub(self.pos)
            .min(self.capacity())
    }

    /// True if [`len`](Self::len) is zero.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
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
