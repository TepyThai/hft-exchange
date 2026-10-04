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

// @student-begin w01
struct State<T> {
    queue: VecDeque<T>,
    sender_alive: bool,
    receivers: usize,
    /// Receivers blocked in `recv`. `send` only pays for a notify (a futex
    /// syscall) when someone is actually waiting.
    waiting: usize,
}

struct Shared<T> {
    state: Mutex<State<T>>,
    available: Condvar,
}

impl<T> Shared<T> {
    /// Locks the state, ignoring poisoning: no code path panics while
    /// holding the lock with the state half-updated.
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.sender_alive = false;
        drop(state);
        // Wake everyone: each receiver re-checks and returns None once the
        // queue is drained.
        self.shared.available.notify_all();
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.shared.lock().receivers -= 1;
    }
}
// @student-end

/// The single producing end. Not `Clone`: one producer, by construction.
// @student w01
pub struct Sender<T> {
    shared: Arc<Shared<T>>,
}

/// A consuming end. Clone it to add consumers; each message is delivered to
/// exactly one of them.
// @student w01
pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
}

/// Creates an unbounded SPMC channel. Allocates the shared state once; the
/// queue grows as needed.
// @student w01
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            queue: VecDeque::new(),
            sender_alive: true,
            receivers: 1,
            waiting: 0,
        }),
        available: Condvar::new(),
    });
    (
        Sender {
            shared: Arc::clone(&shared),
        },
        Receiver { shared },
    )
}

// @student w01
impl<T> Sender<T> {
    /// Enqueues `value` and wakes one blocked receiver (if any). Returns
    /// the value back if every receiver is gone. O(1) amortized: one lock,
    /// plus a futex wake only when a receiver is waiting.
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        let mut state = self.shared.lock();
        if state.receivers == 0 {
            return Err(SendError(value));
        }
        state.queue.push_back(value);
        let wake = state.waiting > 0;
        drop(state);
        if wake {
            self.shared.available.notify_one();
        }
        Ok(())
    }

    /// Messages currently queued.
    pub fn len(&self) -> usize {
        self.shared.lock().queue.len()
    }

    /// True if no messages are queued.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// @student w01
impl<T> Receiver<T> {
    /// Blocks until a message is available and returns it, or returns
    /// `None` once the sender is dropped **and** the queue is drained.
    pub fn recv(&self) -> Option<T> {
        let mut state = self.shared.lock();
        loop {
            if let Some(v) = state.queue.pop_front() {
                return Some(v);
            }
            if !state.sender_alive {
                return None;
            }
            state.waiting += 1;
            state = self
                .shared
                .available
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
            state.waiting -= 1;
        }
    }

    /// Returns a message if one is queued, without blocking.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        let mut state = self.shared.lock();
        match state.queue.pop_front() {
            Some(v) => Ok(v),
            None if state.sender_alive => Err(TryRecvError::Empty),
            None => Err(TryRecvError::Disconnected),
        }
    }

    /// Messages currently queued.
    pub fn len(&self) -> usize {
        self.shared.lock().queue.len()
    }

    /// True if no messages are queued.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// @student w01
impl<T> Clone for Receiver<T> {
    /// Another consumer on the same queue. O(1): one lock and an `Arc`
    /// clone.
    fn clone(&self) -> Self {
        self.shared.lock().receivers += 1;
        Receiver {
            shared: Arc::clone(&self.shared),
        }
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
