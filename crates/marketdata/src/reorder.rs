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

use types::Seq;

// @student-begin w46
use std::collections::VecDeque;
// @student-end

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
// @student w46
#[derive(Clone, Debug)]
pub struct ReorderBuffer<T> {
    /// The sequence number `pop` delivers next from `slots`.
    next: u64,
    /// `window` (a power of two) slots; seq `s` lives at `s & mask` while
    /// `next <= s < next + window`.
    slots: Vec<Option<T>>,
    mask: u64,
    /// Items released past a given-up hole, waiting for `pop`, in order;
    /// all below `next`.
    ready: VecDeque<(Seq, T)>,
    /// Items in `slots`.
    held: usize,
}

// @student w46
impl<T> ReorderBuffer<T> {
    /// An empty buffer expecting `next` first, holding up to `window`
    /// messages (rounded up to a power of two, at least 1). Allocates the
    /// slots once.
    pub fn new(next: Seq, window: usize) -> ReorderBuffer<T> {
        let window = window.max(1).next_power_of_two();
        ReorderBuffer {
            next: next.0,
            slots: (0..window).map(|_| None).collect(),
            mask: window as u64 - 1,
            ready: VecDeque::new(),
            held: 0,
        }
    }

    /// Offers one message. O(1), or O(window) when giving up a hole.
    #[inline]
    pub fn push(&mut self, seq: Seq, item: T) -> PushOutcome {
        let s = seq.0;
        if s < self.next {
            return PushOutcome::Duplicate;
        }
        let window = self.mask + 1;
        let mut lost = 0;
        if s - self.next >= window {
            lost = self.advance(s - window + 1);
        }
        let slot = &mut self.slots[(s & self.mask) as usize];
        if slot.is_some() {
            return PushOutcome::Duplicate;
        }
        *slot = Some(item);
        self.held += 1;
        if lost > 0 {
            PushOutcome::Skipped { lost }
        } else {
            PushOutcome::Accepted
        }
    }

    /// The next message in sequence order, if it has arrived (or was
    /// released past a given-up hole). O(1).
    #[inline]
    pub fn pop(&mut self) -> Option<(Seq, T)> {
        if let Some(r) = self.ready.pop_front() {
            return Some(r);
        }
        let item = self.slots[(self.next & self.mask) as usize].take()?;
        self.held -= 1;
        let seq = Seq(self.next);
        self.next += 1;
        Some((seq, item))
    }

    /// The sequence number the buffer waits for next.
    pub fn next_seq(&self) -> Seq {
        Seq(self.next)
    }

    /// Messages held (not yet popped).
    pub fn buffered(&self) -> usize {
        self.held + self.ready.len()
    }

    /// The hole the buffer is waiting on: `Some(next_seq)` when messages
    /// after it are held but it has not arrived.
    pub fn missing(&self) -> Option<Seq> {
        (self.ready.is_empty()
            && self.held > 0
            && self.slots[(self.next & self.mask) as usize].is_none())
        .then_some(Seq(self.next))
    }

    /// Forgets everything before `seq` — after a snapshot that covers it:
    /// drops held messages below `seq` and stops waiting for holes there.
    /// O(min(window, distance)).
    pub fn skip_to(&mut self, seq: Seq) {
        self.ready.retain(|(s, _)| *s >= seq);
        while self.next < seq.0 && self.held > 0 {
            if self.slots[(self.next & self.mask) as usize]
                .take()
                .is_some()
            {
                self.held -= 1;
            }
            self.next += 1;
        }
        self.next = self.next.max(seq.0);
    }
}

// @student-begin w46
impl<T> ReorderBuffer<T> {
    /// Gives up every hole below `new_next`: releases held messages below
    /// it to `ready` in order and returns how many sequence numbers were
    /// lost.
    #[cold]
    fn advance(&mut self, new_next: u64) -> u64 {
        let window = self.mask + 1;
        let (from, to) = (self.next, new_next);
        let mut released = 0;
        // Only the window's slots can hold anything.
        for s in from..to.min(from + window) {
            if let Some(item) = self.slots[(s & self.mask) as usize].take() {
                self.ready.push_back((Seq(s), item));
                self.held -= 1;
                released += 1;
            }
        }
        self.next = to;
        (to - from) - released
    }
}
// @student-end
