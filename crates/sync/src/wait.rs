//! Week 30: how a consumer thread waits for work.
//!
//! An idle consumer (the engine waiting on its inbound queue) can
//!
//! * **busy-spin** ([`BusySpin`]): poll in a tight loop. Lowest wakeup
//!   latency (one cache-line transfer, ~100 ns) at the price of a whole
//!   core at 100% forever — the right call for a pinned engine thread on an
//!   isolated core;
//! * **spin, then yield** ([`SpinYield`]): poll for a while, then call
//!   `yield_now` between polls. Still burns CPU, but lets other runnable
//!   threads on the core make progress;
//! * **spin, then park** ([`SpinPark`]): poll for a bounded time, then sleep
//!   in the kernel until the producer calls [`WaitStrategy::notify`]. Costs
//!   nothing while idle, but a wakeup goes through the scheduler (tens of
//!   µs, worse from deep C-states).
//!
//! All three implement [`WaitStrategy`], so the consumer loop is written
//! once and the strategy is a deployment decision measured by the w30
//! benchmarks.
//!
//! # Protocol
//!
//! The waiter polls a condition; the producer makes it true and then calls
//! `notify`:
//!
//! ```
//! use std::sync::Arc;
//! use std::time::Duration;
//! use sync::{SpinPark, WaitStrategy};
//!
//! let (mut tx, mut rx) = sync::spsc::channel::<u64>(1024);
//! let wait = Arc::new(SpinPark::new(1_000, Duration::from_millis(1)));
//! let w = Arc::clone(&wait);
//! let consumer = std::thread::spawn(move || {
//!     w.wait_until(|| !rx.is_empty()); // spins, then parks
//!     rx.try_pop()
//! });
//! tx.try_push(7).unwrap();
//! wait.notify(); // after publishing: wakes the consumer if it parked
//! assert_eq!(consumer.join().unwrap(), Some(7));
//! ```
//!
//! [`SpinPark`] supports **one waiting thread at a time** (the consumer of
//! an SPSC/MPSC queue); any number of threads may call `notify`.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use std::sync::atomic::{fence, AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread::Thread;
use std::time::{Duration, Instant};

/// How a thread waits for a condition another thread will make true.
pub trait WaitStrategy: Send + Sync {
    /// Polls `ready` until it returns true (returns `true`) or `deadline`
    /// passes (returns `false`; `None` waits forever). Returns immediately
    /// if `ready()` is already true. `ready` must be cheap and must observe
    /// the producer's writes with `Acquire` (an SPSC `is_empty`, an atomic
    /// load) — it is called many times.
    fn wait_for<F: FnMut() -> bool>(&self, ready: F, deadline: Option<Instant>) -> bool;

    /// Called by a producer **after** making the condition true. Wakes the
    /// waiter if it is sleeping; free for spinning strategies.
    fn notify(&self);

    /// Waits until `ready` returns true, with no deadline.
    fn wait_until<F: FnMut() -> bool>(&self, ready: F) {
        self.wait_for(ready, None);
    }
}

/// How many polls between deadline checks (`Instant::now` costs ~20 ns).
/// A power of two, so the check is a mask.
const CLOCK_EVERY: u32 = 64;

/// Polls in a tight loop with a CPU `pause` hint between polls. Lowest
/// latency; burns a full core while waiting.
#[derive(Clone, Copy, Debug, Default)]
pub struct BusySpin;

/// Spins for `spins` polls, then yields the CPU to the scheduler between
/// polls. Burns CPU while waiting, but cooperates with other threads on
/// the same core.
#[derive(Clone, Copy, Debug)]
pub struct SpinYield {
    /// Polls before the first `yield_now`.
    pub spins: u32,
}

impl SpinYield {
    /// Spin `spins` polls before starting to yield.
    pub fn new(spins: u32) -> SpinYield {
        SpinYield { spins }
    }
}

impl Default for SpinYield {
    fn default() -> Self {
        SpinYield::new(1_000)
    }
}

#[allow(unused_variables)]
impl WaitStrategy for BusySpin {
    #[inline]
    fn wait_for<F: FnMut() -> bool>(&self, mut ready: F, deadline: Option<Instant>) -> bool {
        todo!("w30: wait_for")
    }

    #[inline]
    fn notify(&self) {
        todo!("w30: notify")
    }
}

#[allow(unused_variables)]
impl WaitStrategy for SpinYield {
    fn wait_for<F: FnMut() -> bool>(&self, mut ready: F, deadline: Option<Instant>) -> bool {
        todo!("w30: wait_for")
    }

    #[inline]
    fn notify(&self) {
        todo!("w30: notify")
    }
}

/// Spins for `spins` polls, then parks the thread until [`notify`]
/// (or at most `max_park`, after which it re-polls — so a waiter whose
/// producer forgets to notify, or a cancellation flag nobody notifies for,
/// is still observed within `max_park`).
///
/// `notify` costs one `SeqCst` fence (~10–30 ns) when nobody sleeps, plus
/// an `unpark` (a futex wake syscall) when the waiter is parked.
///
/// [`notify`]: WaitStrategy::notify
pub struct SpinPark {
    // TODO(w30): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl SpinPark {
    /// Spin `spins` polls, then park for at most `max_park` at a time.
    pub fn new(spins: u32, max_park: Duration) -> SpinPark {
        todo!("w30: new")
    }

    /// True while the waiter is parked (or about to park). Diagnostic, used
    /// by benchmarks to make sure they measure the parked path.
    pub fn is_parked(&self) -> bool {
        todo!("w30: is_parked")
    }
}

impl Default for SpinPark {
    /// 10k polls (a few µs) then park, re-polling at least every 1 ms.
    fn default() -> Self {
        SpinPark::new(10_000, Duration::from_millis(1))
    }
}

#[allow(unused_variables)]
impl WaitStrategy for SpinPark {
    fn wait_for<F: FnMut() -> bool>(&self, mut ready: F, deadline: Option<Instant>) -> bool {
        todo!("w30: wait_for")
    }

    fn notify(&self) {
        todo!("w30: notify")
    }
}

impl std::fmt::Debug for SpinPark {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpinPark")
            .field("parked", &self.is_parked())
            .finish()
    }
}
