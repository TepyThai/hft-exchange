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
// @student w21
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    source: ClockSource,
    /// `Instant` at the epoch (used by the Instant source).
    epoch: std::time::Instant,
    /// TSC value at the epoch.
    base_ticks: u64,
    /// Nanoseconds per tick as the fixed-point number `mult / 2^shift`,
    /// with `shift` as large as possible for precision (`mult < 2^63`).
    mult: u64,
    shift: u32,
    /// Ticks per second (1e9 for the Instant source).
    hz: u64,
}

// @student w21
impl Clock {
    /// The best clock for this machine: a TSC clock calibrated over 10 ms if
    /// the CPU has an invariant TSC, otherwise [`Clock::instant`]. Blocks for
    /// the calibration window; create it once at startup (or use
    /// [`Clock::global`]).
    pub fn new() -> Clock {
        Clock::tsc(Duration::from_millis(10)).unwrap_or_else(Clock::instant)
    }

    /// A clock backed by `std::time::Instant`. Always available; ~20 ns per
    /// read.
    pub fn instant() -> Clock {
        Clock {
            source: ClockSource::Instant,
            epoch: std::time::Instant::now(),
            base_ticks: 0,
            mult: 1,
            shift: 0,
            hz: 1_000_000_000,
        }
    }

    /// A TSC clock calibrated against `Instant` over `window` (longer is more
    /// accurate: rate error ≈ read jitter / window, ~1e-8 for 10 ms).
    /// Returns `None` when the target is not x86_64 or the CPU does not
    /// advertise an invariant TSC. Sleeps for `window`.
    pub fn tsc(window: Duration) -> Option<Clock> {
        if !Clock::has_invariant_tsc() {
            return None;
        }
        let (t0, i0) = paired_read();
        std::thread::sleep(window);
        let (t1, i1) = paired_read();
        let ns = i1.duration_since(i0).as_nanos();
        let ticks = t1.wrapping_sub(t0) as u128;
        if ns == 0 || ticks == 0 {
            return None;
        }
        let (mult, shift) = mult_shift(ns, ticks);
        let hz = (ticks * 1_000_000_000 / ns).min(u64::MAX as u128) as u64;
        Some(Clock {
            source: ClockSource::Tsc,
            epoch: i1,
            base_ticks: t1,
            mult,
            shift,
            hz,
        })
    }

    /// A TSC clock for a known TSC frequency (e.g. from CPUID leaf `0x15` or
    /// the kernel's `tsc_khz`), skipping calibration. `None` without an
    /// invariant TSC.
    ///
    /// # Panics
    ///
    /// If `hz == 0`.
    pub fn tsc_at(hz: u64) -> Option<Clock> {
        assert!(hz > 0, "TSC frequency must be positive");
        if !Clock::has_invariant_tsc() {
            return None;
        }
        let (mult, shift) = mult_shift(1_000_000_000, hz as u128);
        Some(Clock {
            source: ClockSource::Tsc,
            epoch: std::time::Instant::now(),
            base_ticks: read_tsc(),
            mult,
            shift,
            hz,
        })
    }

    /// True on x86_64 CPUs that advertise an invariant TSC (constant rate,
    /// keeps ticking in deep C-states). Always false on other targets.
    pub fn has_invariant_tsc() -> bool {
        invariant_tsc()
    }

    /// Which source this clock reads.
    pub fn source(&self) -> ClockSource {
        self.source
    }

    /// Ticks per second of [`raw`](Clock::raw) (1e9 for the Instant source).
    pub fn frequency_hz(&self) -> u64 {
        self.hz
    }

    /// Nanoseconds since this clock's epoch. Monotonic on a thread;
    /// comparable across threads (see the module docs for TSC caveats).
    /// TSC: one `rdtsc`, a subtraction, a multiply and a shift.
    #[inline]
    pub fn now(&self) -> u64 {
        match self.source {
            ClockSource::Tsc => self.ticks_to_ns(read_tsc().wrapping_sub(self.base_ticks)),
            ClockSource::Instant => self.epoch.elapsed().as_nanos() as u64,
        }
    }

    /// The raw tick counter: the TSC value, or ns since the epoch for the
    /// Instant source. Cheapest possible read; convert differences with
    /// [`ticks_to_ns`](Clock::ticks_to_ns).
    #[inline]
    pub fn raw(&self) -> u64 {
        match self.source {
            ClockSource::Tsc => read_tsc(),
            ClockSource::Instant => self.epoch.elapsed().as_nanos() as u64,
        }
    }

    /// Converts a tick count (a difference of [`raw`](Clock::raw) reads) to
    /// nanoseconds, rounded to nearest: one 64×64→128-bit multiply, an add
    /// and a shift. Exact to the calibration (the fixed-point rate is good to
    /// ~1e-18); no overflow for any `u64` input (saturates at `u64::MAX` ns).
    #[inline]
    pub fn ticks_to_ns(&self, ticks: u64) -> u64 {
        let half = (1u128 << self.shift) >> 1;
        let ns = (ticks as u128 * self.mult as u128 + half) >> self.shift;
        if ns > u64::MAX as u128 {
            u64::MAX
        } else {
            ns as u64
        }
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

// @student-begin w21
/// Fixed-point `ns / ticks` as `(mult, shift)` with `mult / 2^shift ≈ ns /
/// ticks`: the largest `shift ≤ 63` that keeps `mult < 2^63`, so `ticks ·
/// mult` fits in a `u128` for every `u64` tick count.
fn mult_shift(ns: u128, ticks: u128) -> (u64, u32) {
    let mut shift = 63;
    loop {
        let mult = (ns << shift) / ticks;
        if mult < (1 << 63) || shift == 0 {
            return (mult.min(u64::MAX as u128) as u64, shift);
        }
        shift -= 1;
    }
}

/// Reads (TSC, Instant) as close together as possible: the TSC reads that
/// bracket the `Instant` read with the smallest gap win, and the TSC value
/// is the midpoint of that bracket.
fn paired_read() -> (u64, std::time::Instant) {
    let mut best_gap = u64::MAX;
    let mut best = (read_tsc(), std::time::Instant::now());
    for _ in 0..16 {
        let a = read_tsc();
        let i = std::time::Instant::now();
        let b = read_tsc();
        let gap = b.wrapping_sub(a);
        if gap < best_gap {
            best_gap = gap;
            best = (a + gap / 2, i);
        }
    }
    best
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn read_tsc() -> u64 {
    // SAFETY: RDTSC is part of the x86_64 baseline instruction set (every
    // x86_64 CPU implements it), reads a register and touches no memory; the
    // intrinsic is `unsafe` only because it is a raw instruction wrapper.
    unsafe { core::arch::x86_64::_rdtsc() }
}

#[cfg(not(target_arch = "x86_64"))]
#[inline(always)]
fn read_tsc() -> u64 {
    // No TSC: `Clock::tsc` returns `None`, so this is never used for time.
    0
}

#[cfg(target_arch = "x86_64")]
fn invariant_tsc() -> bool {
    use core::arch::x86_64::__cpuid;
    // SAFETY: CPUID is part of the x86_64 baseline instruction set and has
    // no memory-safety preconditions (the intrinsic is a safe fn on newer
    // toolchains, hence the allow).
    #[allow(unused_unsafe)]
    let max_extended = unsafe { __cpuid(0x8000_0000) }.eax;
    if max_extended < 0x8000_0007 {
        return false;
    }
    // SAFETY: as above; leaf 0x8000_0007 exists (checked against the
    // maximum extended leaf).
    #[allow(unused_unsafe)]
    let power = unsafe { __cpuid(0x8000_0007) };
    power.edx & (1 << 8) != 0
}

#[cfg(not(target_arch = "x86_64"))]
fn invariant_tsc() -> bool {
    false
}
// @student-end
