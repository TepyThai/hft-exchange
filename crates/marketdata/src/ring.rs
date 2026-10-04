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

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use std::sync::Arc;

// TODO(w16): implement — see the week guide on tepy.dev/hft.

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
pub struct BroadcastRing<T> {
    // TODO(w16): design this type's fields.
    _todo: std::marker::PhantomData<(T,)>,
}

/// The single writer of a [`BroadcastRing`]. Not `Clone`: there is exactly
/// one per ring.
pub struct RingPublisher<T> {
    // TODO(w16): design this type's fields.
    _todo: std::marker::PhantomData<(T,)>,
}

/// One consumer's cursor into a [`BroadcastRing`]. Each reader sees every
/// value from where it started, independently of the others, unless it
/// lags. Cloning gives a second reader at the same position.
#[repr(align(128))]
pub struct Reader<T> {
    // TODO(w16): design this type's fields.
    _todo: std::marker::PhantomData<(T,)>,
}

/// Creates a ring of `capacity` slots (a power of two, at least 2) and its
/// writer; create readers from the returned ring (or the publisher).
/// Allocates `capacity × slot` bytes once; nothing allocates afterwards.
///
/// # Panics
/// If `capacity` is not a power of two ≥ 2, or `T::WORDS` is not in
/// `1..=MAX_WORDS`.
#[allow(unused_variables)]
pub fn broadcast<T: RingValue>(capacity: usize) -> (RingPublisher<T>, Arc<BroadcastRing<T>>) {
    todo!("w16: broadcast")
}

// TODO(w16): implement — see the week guide on tepy.dev/hft.

#[allow(unused_variables)]
impl<T: RingValue> BroadcastRing<T> {
    /// A reader at the live edge: it receives only values published after
    /// this call. O(1).
    pub fn subscribe(self: &Arc<Self>) -> Reader<T> {
        todo!("w16: subscribe")
    }

    /// A reader at the oldest value still in the ring (up to `capacity`
    /// values back) — for a consumer that will reconcile against a
    /// snapshot taken a little earlier. O(1).
    pub fn subscribe_oldest(self: &Arc<Self>) -> Reader<T> {
        todo!("w16: subscribe_oldest")
    }

    /// Number of slots.
    pub fn capacity(&self) -> usize {
        todo!("w16: capacity")
    }

    /// Number of values published so far (= the sequence number of the
    /// next one). O(1), one atomic load.
    pub fn published(&self) -> u64 {
        todo!("w16: published")
    }
}

#[allow(unused_variables)]
impl<T: RingValue> RingPublisher<T> {
    /// Publishes `value` and returns its sequence number (the first publish
    /// is 0). Never blocks, never reads reader state; overwrites the value
    /// published `capacity` publishes ago. O(1): `WORDS + 2` stores to one
    /// slot plus the cursor.
    #[inline]
    pub fn publish(&mut self, value: T) -> u64 {
        todo!("w16: publish")
    }

    /// A reader at the live edge (see [`BroadcastRing::subscribe`]).
    pub fn subscribe(&self) -> Reader<T> {
        todo!("w16: subscribe")
    }

    /// The ring this publisher writes.
    pub fn ring(&self) -> &Arc<BroadcastRing<T>> {
        todo!("w16: ring")
    }

    /// Sequence number the next publish will get.
    pub fn next_seq(&self) -> u64 {
        todo!("w16: next_seq")
    }
}

#[allow(unused_variables)]
impl<T: RingValue> Reader<T> {
    /// The next value, [`Recv::Empty`] if there is none yet, or
    /// [`Recv::Lagged`] if the writer overwrote it (the reader then moves
    /// to the oldest value still in the ring). Never blocks. O(1): one
    /// slot's stamp twice and its payload words, plus the writer's cursor
    /// after a lag.
    #[inline]
    pub fn try_recv(&mut self) -> Recv<T> {
        todo!("w16: try_recv")
    }

    /// Sequence number this reader will read next.
    pub fn next_seq(&self) -> u64 {
        todo!("w16: next_seq")
    }

    /// How many published values this reader has not read yet (more than
    /// the capacity means it has lagged). One atomic load.
    pub fn backlog(&self) -> u64 {
        todo!("w16: backlog")
    }

    /// The ring this reader reads.
    pub fn ring(&self) -> &Arc<BroadcastRing<T>> {
        todo!("w16: ring")
    }
}

// TODO(w16): implement — see the week guide on tepy.dev/hft.

#[allow(unused_variables)]
impl<T> Clone for Reader<T> {
    fn clone(&self) -> Self {
        todo!("w16: clone")
    }
}
