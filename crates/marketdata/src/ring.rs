//! Week 16 — a single-writer broadcast ring with per-reader cursors.
//!
//! One [`RingPublisher`] writes; any number of [`Reader`]s, on any threads,
//! read every value independently. The writer **never waits and never
//! looks at readers**: it overwrites the oldest slot. A reader that falls
//! more than `capacity` values behind finds its next slot overwritten and
//! gets [`Recv::Lagged`] with the exact number it missed, then continues
//! from the oldest value still in the ring — its consumer resyncs from a
//! snapshot (week 10's recovery path). Its cost is independent of how many
//! readers there are and how slow they are.
//!
//! # Slots: a seqlock each
//!
//! The capacity is a power of two, so value `seq` lives in slot
//! `seq & (capacity - 1)`. Every slot carries a *stamp* next to its
//! payload words, and the reader decides everything from the one slot it
//! looks at:
//!
//! ```text
//! stamp 0            never written
//! stamp 2·seq + 1    the writer is writing `seq` into this slot
//! stamp 2·seq + 2    the slot holds `seq`
//! ```
//!
//! For a reader that wants `next` (`want = 2·next + 2`):
//! `stamp == want` → copy the payload, then re-check the stamp (a changed
//! stamp means the copy may be torn: treat it as a lap);
//! `stamp < want` → not published yet: [`Recv::Empty`];
//! `stamp > want` → the writer lapped this reader: [`Recv::Lagged`].
//!
//! Payload words are `AtomicU64`s written and read with `Relaxed` loads and
//! stores — a plain read racing with a write is undefined behaviour even if
//! the value is then thrown away. The orderings (Boehm, *Can seqlocks get
//! along with programming language memory models?*):
//!
//! ```text
//! writer                                   reader
//! stamp.store(2s+1, Relaxed)               s1 = stamp.load(Acquire)
//! fence(Release)                           words[..] = payload.load(Relaxed)
//! payload.store(.., Relaxed)               fence(Acquire)
//! stamp.store(2s+2, Release)               s2 = stamp.load(Relaxed); valid iff s1 == s2
//! ```
//!
//! # Layout
//!
//! A slot is the stamp plus [`RingValue::WORDS`] payload words, rounded up
//! to a power of two words so slots never straddle a cache line: an
//! [`L2Update`](crate::L2Update) packs into 3 words, so its slots are 32
//! bytes, two per line. The writer's cursor has a cache line of its own;
//! readers' cursors live in the readers.

use std::sync::Arc;

// @student-begin w16
use std::sync::atomic::{fence, AtomicU64, Ordering};

/// One cache line of slot storage.
#[repr(align(64))]
#[derive(Default)]
struct Line([AtomicU64; 8]);

/// A `u64` on its own cache line (128 bytes: adjacent-line prefetchers
/// pull pairs of lines).
#[repr(align(128))]
#[derive(Default)]
struct Padded(AtomicU64);
// @student-end

/// The largest [`RingValue::WORDS`] a ring accepts: a slot (stamp +
/// payload) must fit in one 64-byte cache line.
pub const MAX_WORDS: usize = 7;

/// A value that can travel through a [`BroadcastRing`]: it packs into a
/// fixed number of `u64` words, which the ring stores in atomics so that a
/// reader racing the writer is never undefined behaviour.
///
/// `unpack(pack(v)) == v` must hold for every value the program sends.
pub trait RingValue: Copy {
    /// Number of words, `1..=MAX_WORDS`.
    const WORDS: usize;

    /// Writes the value into `words` (exactly `WORDS` long).
    fn pack(&self, words: &mut [u64]);

    /// Reads a value back from `words` (exactly `WORDS` long).
    fn unpack(words: &[u64]) -> Self;
}

impl RingValue for u64 {
    const WORDS: usize = 1;

    #[inline]
    fn pack(&self, words: &mut [u64]) {
        words[0] = *self;
    }

    #[inline]
    fn unpack(words: &[u64]) -> Self {
        words[0]
    }
}

/// Three words: `[seq | side << 63, price, qty]`. Feed sequence numbers
/// must stay below 2⁶³ (at a billion updates a second that is 292 years).
impl RingValue for crate::L2Update {
    const WORDS: usize = 3;

    #[inline]
    fn pack(&self, words: &mut [u64]) {
        debug_assert!(self.seq.0 < 1 << 63, "feed seq out of range");
        let side = match self.side {
            types::Side::Buy => 0,
            types::Side::Sell => 1u64 << 63,
        };
        words[0] = self.seq.0 | side;
        words[1] = self.price.0 as u64;
        words[2] = self.qty.0;
    }

    #[inline]
    fn unpack(words: &[u64]) -> Self {
        crate::L2Update {
            seq: types::Seq(words[0] & !(1 << 63)),
            side: if words[0] >> 63 == 0 {
                types::Side::Buy
            } else {
                types::Side::Sell
            },
            price: types::Price(words[1] as i64),
            qty: types::Qty(words[2]),
        }
    }
}

/// What [`Reader::try_recv`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recv<T> {
    /// The next value; the reader advanced past it.
    Item(T),
    /// Nothing new has been published yet.
    Empty,
    /// The writer overwrote this many values before the reader got to
    /// them. The reader now points at the oldest value still in the ring;
    /// its consumer must treat its state as stale (resync from a
    /// snapshot).
    Lagged(u64),
}

/// The shared storage of a broadcast ring. The [`RingPublisher`] writes it;
/// [`Reader`]s created from it with [`subscribe`](BroadcastRing::subscribe)
/// read it. Share it as `Arc<BroadcastRing<T>>`.
// @student w16
pub struct BroadcastRing<T> {
    /// Slot `i` occupies words `i << stride_shift ..`: the stamp, then the
    /// payload.
    lines: Box<[Line]>,
    mask: u64,
    capacity: u64,
    stride_shift: u32,
    /// Sequence number the next publish gets (= values published so far).
    head: Padded,
    _values: std::marker::PhantomData<T>,
}

/// The single writer of a [`BroadcastRing`]. Not `Clone`: there is exactly
/// one per ring.
// @student w16
pub struct RingPublisher<T> {
    ring: Arc<BroadcastRing<T>>,
    next: u64,
}

/// One consumer's cursor into a [`BroadcastRing`]. Each reader sees every
/// value from where it started, independently of the others, unless it
/// lags. Cloning gives a second reader at the same position.
// @student w16
#[repr(align(128))]
pub struct Reader<T> {
    ring: Arc<BroadcastRing<T>>,
    /// The sequence number this reader wants next.
    next: u64,
}

/// Creates a ring of `capacity` slots (a power of two, at least 2) and its
/// writer; create readers from the returned ring (or the publisher).
/// Allocates `capacity × slot` bytes once; nothing allocates afterwards.
///
/// # Panics
/// If `capacity` is not a power of two ≥ 2, or `T::WORDS` is not in
/// `1..=MAX_WORDS`.
// @student w16
pub fn broadcast<T: RingValue>(capacity: usize) -> (RingPublisher<T>, Arc<BroadcastRing<T>>) {
    assert!(
        capacity >= 2 && capacity.is_power_of_two(),
        "capacity must be a power of two >= 2"
    );
    assert!(
        (1..=MAX_WORDS).contains(&T::WORDS),
        "RingValue::WORDS must be in 1..={MAX_WORDS}"
    );
    let stride = (T::WORDS + 1).next_power_of_two();
    let words = capacity * stride;
    let lines = (0..words.div_ceil(8)).map(|_| Line::default()).collect();
    let ring = Arc::new(BroadcastRing {
        lines,
        mask: capacity as u64 - 1,
        capacity: capacity as u64,
        stride_shift: stride.trailing_zeros(),
        head: Padded::default(),
        _values: std::marker::PhantomData,
    });
    let publisher = RingPublisher {
        ring: Arc::clone(&ring),
        next: 0,
    };
    (publisher, ring)
}

// @student-begin w16
impl<T> BroadcastRing<T> {
    #[inline]
    fn word(&self, index: usize) -> &AtomicU64 {
        &self.lines[index >> 3].0[index & 7]
    }

    /// Index of slot `seq`'s stamp word.
    #[inline]
    fn slot(&self, seq: u64) -> usize {
        ((seq & self.mask) as usize) << self.stride_shift
    }
}
// @student-end

// @student w16
impl<T: RingValue> BroadcastRing<T> {
    /// A reader at the live edge: it receives only values published after
    /// this call. O(1).
    pub fn subscribe(self: &Arc<Self>) -> Reader<T> {
        Reader {
            ring: Arc::clone(self),
            next: self.head.0.load(Ordering::Acquire),
        }
    }

    /// A reader at the oldest value still in the ring (up to `capacity`
    /// values back) — for a consumer that will reconcile against a
    /// snapshot taken a little earlier. O(1).
    pub fn subscribe_oldest(self: &Arc<Self>) -> Reader<T> {
        let head = self.head.0.load(Ordering::Acquire);
        Reader {
            ring: Arc::clone(self),
            next: head.saturating_sub(self.capacity),
        }
    }

    /// Number of slots.
    pub fn capacity(&self) -> usize {
        self.capacity as usize
    }

    /// Number of values published so far (= the sequence number of the
    /// next one). O(1), one atomic load.
    pub fn published(&self) -> u64 {
        self.head.0.load(Ordering::Acquire)
    }
}

// @student w16
impl<T: RingValue> RingPublisher<T> {
    /// Publishes `value` and returns its sequence number (the first publish
    /// is 0). Never blocks, never reads reader state; overwrites the value
    /// published `capacity` publishes ago. O(1): `WORDS + 2` stores to one
    /// slot plus the cursor.
    #[inline]
    pub fn publish(&mut self, value: T) -> u64 {
        let seq = self.next;
        let ring = &*self.ring;
        let slot = ring.slot(seq);
        let stamp = ring.word(slot);
        stamp.store(2 * seq + 1, Ordering::Relaxed);
        // The "writing" stamp becomes visible no later than any payload
        // word: a reader that sees a new word also sees the stamp change.
        fence(Ordering::Release);
        let mut words = [0u64; MAX_WORDS];
        value.pack(&mut words[..T::WORDS]);
        for (i, &w) in words[..T::WORDS].iter().enumerate() {
            ring.word(slot + 1 + i).store(w, Ordering::Relaxed);
        }
        // Publish: the payload stores happen-before any Acquire load that
        // reads this stamp.
        stamp.store(2 * seq + 2, Ordering::Release);
        ring.head.0.store(seq + 1, Ordering::Release);
        self.next = seq + 1;
        seq
    }

    /// A reader at the live edge (see [`BroadcastRing::subscribe`]).
    pub fn subscribe(&self) -> Reader<T> {
        self.ring.subscribe()
    }

    /// The ring this publisher writes.
    pub fn ring(&self) -> &Arc<BroadcastRing<T>> {
        &self.ring
    }

    /// Sequence number the next publish will get.
    pub fn next_seq(&self) -> u64 {
        self.next
    }
}

// @student w16
impl<T: RingValue> Reader<T> {
    /// The next value, [`Recv::Empty`] if there is none yet, or
    /// [`Recv::Lagged`] if the writer overwrote it (the reader then moves
    /// to the oldest value still in the ring). Never blocks. O(1): one
    /// slot's stamp twice and its payload words, plus the writer's cursor
    /// after a lag.
    #[inline]
    pub fn try_recv(&mut self) -> Recv<T> {
        let ring = &*self.ring;
        let slot = ring.slot(self.next);
        let stamp = ring.word(slot);
        let want = 2 * self.next + 2;
        let s1 = stamp.load(Ordering::Acquire);
        if s1 == want {
            let mut words = [0u64; MAX_WORDS];
            for (i, w) in words[..T::WORDS].iter_mut().enumerate() {
                *w = ring.word(slot + 1 + i).load(Ordering::Relaxed);
            }
            // The payload loads complete before the stamp is re-read.
            fence(Ordering::Acquire);
            let s2 = stamp.load(Ordering::Relaxed);
            if s2 == s1 {
                self.next += 1;
                return Recv::Item(T::unpack(&words[..T::WORDS]));
            }
            return self.resync(s2);
        }
        if s1 < want {
            return Recv::Empty;
        }
        self.resync(s1)
    }

    /// Sequence number this reader will read next.
    pub fn next_seq(&self) -> u64 {
        self.next
    }

    /// How many published values this reader has not read yet (more than
    /// the capacity means it has lagged). One atomic load.
    pub fn backlog(&self) -> u64 {
        self.ring.published().saturating_sub(self.next)
    }

    /// The ring this reader reads.
    pub fn ring(&self) -> &Arc<BroadcastRing<T>> {
        &self.ring
    }
}

// @student-begin w16
impl<T: RingValue> Reader<T> {
    /// The reader's slot was seen with `stamp`, which belongs to a later
    /// lap: jump to the oldest value still in the ring and report the
    /// skip. The slot itself proves the writer reached
    /// `stamp_seq ≥ next + capacity`, so the reader always moves forward
    /// even if its view of the writer's cursor is stale.
    #[cold]
    fn resync(&mut self, stamp: u64) -> Recv<T> {
        let ring = &*self.ring;
        let stamp_seq = (stamp - 1) / 2;
        let head = ring.head.0.load(Ordering::Acquire);
        let oldest = head
            .saturating_sub(ring.capacity)
            .max(stamp_seq + 1 - ring.capacity);
        let missed = oldest - self.next;
        self.next = oldest;
        Recv::Lagged(missed)
    }
}
// @student-end

// @student w16
impl<T> Clone for Reader<T> {
    fn clone(&self) -> Self {
        Reader {
            ring: Arc::clone(&self.ring),
            next: self.next,
        }
    }
}
