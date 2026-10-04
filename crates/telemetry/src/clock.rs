//! Week 21: a cheap monotonic nanosecond clock.
//!
//! An exchange timestamps every order several times on its way through
//! (see [`StageStamps`](crate::StageStamps)), so the clock read must cost
//! nanoseconds, never go backwards, and convert to real time well enough
//! that a latency number means something.
//!
//! * **TSC** (x86_64): `rdtsc` reads the CPU's time-stamp counter in a few
//!   ns without entering the kernel. On CPUs with an *invariant* TSC
//!   (CPUID `0x8000_0007` EDX bit 8 — every x86_64 server CPU of the last
//!   decade) it ticks at a constant rate regardless of frequency scaling and
//!   C-states, and the kernel keeps it synchronised across cores. We
//!   calibrate its rate against [`Instant`](std::time::Instant) over a
//!   short window and convert ticks to ns with one 64×64→128-bit multiply
//!   and a shift.
//! * **Instant** fallback (other targets, or no invariant TSC):
//!   `Instant::now()` is `clock_gettime(CLOCK_MONOTONIC)` through the vDSO —
//!   ~20 ns, always correct.
//!
//! # Assumptions and caveats of the TSC path
//!
//! * Invariant TSC, synchronised across cores (Linux reports
//!   `constant_tsc nonstop_tsc` and picks `tsc` as its clocksource). If the
//!   kernel marked the TSC unstable, or a VM migrates between hosts with
//!   different TSC rates, use [`Clock::instant`].
//! * `rdtsc` is not serialising: the CPU may execute it a few dozen cycles
//!   before or after neighbouring instructions. Fine for stage timestamps
//!   (100 ns scale); for cycle-accurate micro-measurements fence it
//!   (`lfence; rdtsc`) or use `rdtscp`.
//! * The calibrated rate is accurate to roughly `read jitter / window`. With
//!   paired reads (the TSC read on both sides of `Instant::now()`, smallest
//!   bracket wins) and a 10 ms window that is a few parts in 1e8 — tens of
//!   ns of drift per second against `Instant`, measured on an EPYC 7543P.
//!   Recalibrate, or use `Instant`, for long intervals or wall-clock work.
//! * Timestamps from the same `Clock` (or copies of it) are comparable
//!   across threads; timestamps from two separately calibrated clocks are
//!   not (different epochs). Share one — [`Clock::global`] is the easy way.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use std::sync::OnceLock;
use std::time::Duration;

/// Where a [`Clock`] gets its ticks from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClockSource {
    /// The x86_64 time-stamp counter, calibrated against `Instant`.
    Tsc,
    /// `std::time::Instant` (vDSO `clock_gettime(CLOCK_MONOTONIC)`).
    Instant,
}

/// A monotonic nanosecond clock: calibrated TSC on x86_64 with an invariant
/// TSC, `Instant` otherwise.
///
/// [`now`](Clock::now) returns nanoseconds since the clock was created. A
/// `Clock` is `Copy`; copies share the epoch and calibration, so pass it (or
/// [`Clock::global`]) to every stage that timestamps the same orders.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    // TODO(w21): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl Clock {
    /// The best clock for this machine: a TSC clock calibrated over 10 ms if
    /// the CPU has an invariant TSC, otherwise [`Clock::instant`]. Blocks for
    /// the calibration window; create it once at startup (or use
    /// [`Clock::global`]).
    pub fn new() -> Clock {
        todo!("w21: new")
    }

    /// A clock backed by `std::time::Instant`. Always available; ~20 ns per
    /// read.
    pub fn instant() -> Clock {
        todo!("w21: instant")
    }

    /// A TSC clock calibrated against `Instant` over `window` (longer is more
    /// accurate: rate error ≈ read jitter / window, ~1e-8 for 10 ms).
    /// Returns `None` when the target is not x86_64 or the CPU does not
    /// advertise an invariant TSC. Sleeps for `window`.
    pub fn tsc(window: Duration) -> Option<Clock> {
        todo!("w21: tsc")
    }

    /// A TSC clock for a known TSC frequency (e.g. from CPUID leaf `0x15` or
    /// the kernel's `tsc_khz`), skipping calibration. `None` without an
    /// invariant TSC.
    ///
    /// # Panics
    ///
    /// If `hz == 0`.
    pub fn tsc_at(hz: u64) -> Option<Clock> {
        todo!("w21: tsc_at")
    }

    /// True on x86_64 CPUs that advertise an invariant TSC (constant rate,
    /// keeps ticking in deep C-states). Always false on other targets.
    pub fn has_invariant_tsc() -> bool {
        todo!("w21: has_invariant_tsc")
    }

    /// Which source this clock reads.
    pub fn source(&self) -> ClockSource {
        todo!("w21: source")
    }

    /// Ticks per second of [`raw`](Clock::raw) (1e9 for the Instant source).
    pub fn frequency_hz(&self) -> u64 {
        todo!("w21: frequency_hz")
    }

    /// Nanoseconds since this clock's epoch. Monotonic on a thread;
    /// comparable across threads (see the module docs for TSC caveats).
    /// TSC: one `rdtsc`, a subtraction, a multiply and a shift.
    #[inline]
    pub fn now(&self) -> u64 {
        todo!("w21: now")
    }

    /// The raw tick counter: the TSC value, or ns since the epoch for the
    /// Instant source. Cheapest possible read; convert differences with
    /// [`ticks_to_ns`](Clock::ticks_to_ns).
    #[inline]
    pub fn raw(&self) -> u64 {
        todo!("w21: raw")
    }

    /// Converts a tick count (a difference of [`raw`](Clock::raw) reads) to
    /// nanoseconds, rounded to nearest: one 64×64→128-bit multiply, an add
    /// and a shift. Exact to the calibration (the fixed-point rate is good to
    /// ~1e-18); no overflow for any `u64` input (saturates at `u64::MAX` ns).
    #[inline]
    pub fn ticks_to_ns(&self, ticks: u64) -> u64 {
        todo!("w21: ticks_to_ns")
    }
}

impl Clock {
    /// A process-wide clock, created (and calibrated) on first use. Use it
    /// wherever timestamps from different components must be comparable.
    pub fn global() -> &'static Clock {
        static GLOBAL: OnceLock<Clock> = OnceLock::new();
        GLOBAL.get_or_init(Clock::new)
    }
}

impl Default for Clock {
    fn default() -> Clock {
        Clock::new()
    }
}

// TODO(w21): implement — see the week guide on tepy.dev/hft.
