//! Week 1: a blocking single-producer, multi-consumer channel built from a
//! `Mutex` and a `Condvar`.
//!
//! The ownership rules *are* the design:
//!
//! * [`Sender`] is **not** `Clone` — there is exactly one producer, and the
//!   compiler enforces it (`tx.clone()` does not compile).
//! * [`Receiver`] **is** `Clone` — every clone pops from the same queue, so
//!   each message goes to exactly one receiver.
//! * Dropping the `Sender` wakes every blocked receiver; [`Receiver::recv`]
//!   then drains what is left and returns `None`.
//!
//! This is the safe, obvious baseline the lock-free [`crate::spsc`] ring
//! is measured against. Every operation takes the lock (~20–50 ns
//! uncontended, far more under contention); the queue is unbounded.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

/// Why [`Receiver::try_recv`] returned no message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TryRecvError {
    /// The queue is empty but the sender is still alive.
    Empty,
    /// The queue is empty and the sender has been dropped: no message will
    /// ever arrive.
    Disconnected,
}

/// [`Sender::send`] failed because every receiver has been dropped; the
/// undelivered value is returned.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SendError<T>(pub T);

impl<T> fmt::Debug for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SendError(..)")
    }
}

// TODO(w01): implement — see the week guide on tepy.dev/hft.

/// The single producing end. Not `Clone`: one producer, by construction.
pub struct Sender<T> {
    // TODO(w01): design this type's fields.
    _todo: std::marker::PhantomData<(T,)>,
}

/// A consuming end. Clone it to add consumers; each message is delivered to
/// exactly one of them.
pub struct Receiver<T> {
    // TODO(w01): design this type's fields.
    _todo: std::marker::PhantomData<(T,)>,
}

/// Creates an unbounded SPMC channel. Allocates the shared state once; the
/// queue grows as needed.
#[allow(unused_variables)]
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    todo!("w01: channel")
}

#[allow(unused_variables)]
impl<T> Sender<T> {
    /// Enqueues `value` and wakes one blocked receiver (if any). Returns
    /// the value back if every receiver is gone. O(1) amortized: one lock,
    /// plus a futex wake only when a receiver is waiting.
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        todo!("w01: send")
    }

    /// Messages currently queued.
    pub fn len(&self) -> usize {
        todo!("w01: len")
    }

    /// True if no messages are queued.
    pub fn is_empty(&self) -> bool {
        todo!("w01: is_empty")
    }
}

#[allow(unused_variables)]
impl<T> Receiver<T> {
    /// Blocks until a message is available and returns it, or returns
    /// `None` once the sender is dropped **and** the queue is drained.
    pub fn recv(&self) -> Option<T> {
        todo!("w01: recv")
    }

    /// Returns a message if one is queued, without blocking.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        todo!("w01: try_recv")
    }

    /// Messages currently queued.
    pub fn len(&self) -> usize {
        todo!("w01: len")
    }

    /// True if no messages are queued.
    pub fn is_empty(&self) -> bool {
        todo!("w01: is_empty")
    }
}

#[allow(unused_variables)]
impl<T> Clone for Receiver<T> {
    /// Another consumer on the same queue. O(1): one lock and an `Arc`
    /// clone.
    fn clone(&self) -> Self {
        todo!("w01: clone")
    }
}

impl<T> fmt::Debug for Sender<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("spmc::Sender")
            .field("len", &self.len())
            .finish()
    }
}

impl<T> fmt::Debug for Receiver<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("spmc::Receiver")
            .field("len", &self.len())
            .finish()
    }
}
