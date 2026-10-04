//! Week 46 — a reorder buffer for a sequenced feed.
//!
//! UDP may deliver a feed's messages late, twice, or not at all. The
//! reorder buffer sits between the network and the book builder and turns
//! arrivals back into sequence order:
//!
//! * the next expected message passes straight through;
//! * a message from the future waits in the buffer until the hole before
//!   it is filled by a late arrival;
//! * a copy of anything already delivered or buffered is dropped;
//! * a hole that stays open while messages up to `window` ahead of it
//!   arrive is **given up**: the buffer moves on and releases what it held,
//!   in order, and the consumer sees a gap — the book builder then waits
//!   for a snapshot, exactly as for any other loss.
//!
//! The window bounds both memory and the latency a lost message can add:
//! a consumer never waits more than `window` messages for a hole.
//!
//! ```text
//!   next = 7          window = 8
//!   slots: [7 ·] [8 ✓] [9 ·] [10 ✓] [11 ·] [12 ·] [13 ·] [14 ·]
//!   push 7  → pop yields 7, 8; next = 9 (waiting for 9, holding 10)
//!   push 17 → 17 ≥ 9 + 8: give up 9 (lost), release 10; next = 10 … 11
//! ```

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use types::Seq;

// TODO(w46): implement — see the week guide on tepy.dev/hft.

/// What [`ReorderBuffer::push`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushOutcome {
    /// Accepted; [`pop`](ReorderBuffer::pop) releases it in order.
    Accepted,
    /// Already delivered, already buffered, or given up as lost: dropped.
    Duplicate,
    /// Accepted, but it was so far ahead that `lost` sequence numbers
    /// before it were given up; `pop` skips them.
    Skipped { lost: u64 },
}

/// Turns out-of-order, duplicated or lossy arrivals of `(seq, item)` back
/// into sequence order (see the module docs). Memory: `window` slots.
#[derive(Clone, Debug)]
pub struct ReorderBuffer<T> {
    // TODO(w46): design this type's fields.
    _todo: std::marker::PhantomData<(T,)>,
}

#[allow(unused_variables)]
impl<T> ReorderBuffer<T> {
    /// An empty buffer expecting `next` first, holding up to `window`
    /// messages (rounded up to a power of two, at least 1). Allocates the
    /// slots once.
    pub fn new(next: Seq, window: usize) -> ReorderBuffer<T> {
        todo!("w46: new")
    }

    /// Offers one message. O(1), or O(window) when giving up a hole.
    #[inline]
    pub fn push(&mut self, seq: Seq, item: T) -> PushOutcome {
        todo!("w46: push")
    }

    /// The next message in sequence order, if it has arrived (or was
    /// released past a given-up hole). O(1).
    #[inline]
    pub fn pop(&mut self) -> Option<(Seq, T)> {
        todo!("w46: pop")
    }

    /// The sequence number the buffer waits for next.
    pub fn next_seq(&self) -> Seq {
        todo!("w46: next_seq")
    }

    /// Messages held (not yet popped).
    pub fn buffered(&self) -> usize {
        todo!("w46: buffered")
    }

    /// The hole the buffer is waiting on: `Some(next_seq)` when messages
    /// after it are held but it has not arrived.
    pub fn missing(&self) -> Option<Seq> {
        todo!("w46: missing")
    }

    /// Forgets everything before `seq` — after a snapshot that covers it:
    /// drops held messages below `seq` and stops waiting for holes there.
    /// O(min(window, distance)).
    pub fn skip_to(&mut self, seq: Seq) {
        todo!("w46: skip_to")
    }
}

// TODO(w46): implement — see the week guide on tepy.dev/hft.
