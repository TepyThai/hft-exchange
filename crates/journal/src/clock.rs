//! Injected time for the journal's timed decisions: the week-18 interval
//! fsync policy and the week-20 leader lease.
//!
//! Anything that decides by the clock takes a [`Clock`] instead of calling
//! `Instant::now()` itself, so tests can drive time by hand with a
//! [`ManualClock`] and get the same answer on every run, on any machine,
//! however loaded.
//!
//! Provided infrastructure (not part of a week's exercise).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// A monotonic nanosecond clock. `now_ns` never goes backwards.
///
/// Readings only compare within one clock (and its clones): two clocks
/// created separately have unrelated origins.
pub trait Clock {
    /// Nanoseconds since this clock's origin.
    fn now_ns(&self) -> u64;
}

impl<C: Clock + ?Sized> Clock for &C {
    #[inline]
    fn now_ns(&self) -> u64 {
        (**self).now_ns()
    }
}

impl<C: Clock + ?Sized> Clock for Arc<C> {
    #[inline]
    fn now_ns(&self) -> u64 {
        (**self).now_ns()
    }
}

/// Real monotonic time from [`Instant`], measured from when the clock was
/// created. Copies share the origin, so a copy handed to another thread
/// reads comparable values. About 20 ns per reading (a vDSO call).
#[derive(Clone, Copy, Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    /// A clock whose origin is now.
    pub fn new() -> SystemClock {
        SystemClock {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        SystemClock::new()
    }
}

impl Clock for SystemClock {
    #[inline]
    fn now_ns(&self) -> u64 {
        self.origin.elapsed().as_nanos() as u64
    }
}

/// Simulated time that moves only when told to. Clones share one time
/// (an `Arc<AtomicU64>`), so a test thread can advance the clock a primary
/// and a standby on other threads read. One atomic load per reading.
///
/// ```
/// use journal::{Clock, ManualClock};
/// let clock = ManualClock::new(1_000);
/// let seen_by_other_thread = clock.clone();
/// clock.advance(500);
/// assert_eq!(seen_by_other_thread.now_ns(), 1_500);
/// ```
#[derive(Clone, Debug, Default)]
pub struct ManualClock {
    now: Arc<AtomicU64>,
}

impl ManualClock {
    /// A clock reading `start_ns`.
    pub fn new(start_ns: u64) -> ManualClock {
        ManualClock {
            now: Arc::new(AtomicU64::new(start_ns)),
        }
    }

    /// Moves time forward by `ns` for every clone.
    pub fn advance(&self, ns: u64) {
        self.now.fetch_add(ns, Ordering::AcqRel);
    }

    /// Sets the time to `ns`.
    ///
    /// # Panics
    /// If `ns` is earlier than the current time: clocks never go back.
    pub fn set(&self, ns: u64) {
        let prev = self.now.fetch_max(ns, Ordering::AcqRel);
        assert!(
            prev <= ns,
            "ManualClock cannot go backwards ({prev} -> {ns})"
        );
    }
}

impl Clock for ManualClock {
    #[inline]
    fn now_ns(&self) -> u64 {
        self.now.load(Ordering::Acquire)
    }
}
