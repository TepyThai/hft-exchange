//! Week 25: measuring latency correctly — open-loop load and coordinated
//! omission.
//!
//! A **closed-loop** benchmark sends a request, waits for the reply, then
//! sends the next. When the system stalls for 50 ms, the benchmark stalls
//! with it: it records *one* slow sample, and the hundreds of requests real
//! clients would have sent during the stall — each of which would have
//! waited — are never sent at all. The tester has *coordinated* with the
//! system to *omit* exactly the bad samples (Gil Tene's "coordinated
//! omission"), and its p99 can be off by orders of magnitude.
//!
//! Two fixes, both here:
//!
//! * [`OpenLoop`]: schedule request `i` at `start + i · interval` no
//!   matter what, and measure each response from its **intended** start.
//!   A stall delays the requests behind it, and their queueing delay is
//!   charged to them. `LoadReport::response` is the honest distribution;
//!   `LoadReport::service` (from the actual send) is what a closed-loop
//!   tester would have reported.
//! * [`Histogram::record_corrected`]: when you only have closed-loop
//!   measurements, back-fill the omitted samples from the expected
//!   interval. [`ClosedLoop`] demonstrates both the bias and the
//!   correction.
//!
//! Both run against any [`TimeSource`]: the real [`Clock`] for benchmarks,
//! or [`SimTime`] — simulated time that only moves when told to — for
//! exact, deterministic tests of the harness itself.

use crate::clock::Clock;
use crate::histogram::Histogram;

/// Nanosecond time the load harness schedules against.
pub trait TimeSource {
    /// Current time in ns (any fixed epoch).
    fn now(&mut self) -> u64;

    /// Returns once `now() >= deadline`. The default busy-waits — a load
    /// generator that sleeps wakes up tens of µs late.
    fn wait_until(&mut self, deadline: u64) {
        while self.now() < deadline {
            std::hint::spin_loop();
        }
    }
}

impl TimeSource for Clock {
    #[inline]
    fn now(&mut self) -> u64 {
        Clock::now(self)
    }
}

/// Simulated time for deterministic tests and models: `now` only changes
/// through [`advance`](SimTime::advance) (the service "spending" time) or
/// [`wait_until`](TimeSource::wait_until) (which jumps instead of spinning).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SimTime {
    now: u64,
}

impl SimTime {
    /// Simulated time starting at `start` ns.
    pub fn new(start: u64) -> SimTime {
        SimTime { now: start }
    }

    /// Moves time forward by `ns` — call it from a simulated service.
    pub fn advance(&mut self, ns: u64) {
        self.now += ns;
    }
}

impl TimeSource for SimTime {
    fn now(&mut self) -> u64 {
        self.now
    }

    fn wait_until(&mut self, deadline: u64) {
        self.now = self.now.max(deadline);
    }
}

/// An open-loop load schedule: `warmup + requests` requests, one every
/// `interval_ns`, issued on schedule regardless of how long earlier ones
/// took.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenLoop {
    /// Gap between intended request starts (1e9 / rate).
    pub interval_ns: u64,
    /// Requests measured.
    pub requests: u64,
    /// Requests issued (on schedule) before measuring starts.
    pub warmup: u64,
}

// @student w25
impl OpenLoop {
    /// `requests` requests at `per_sec` requests per second, no warmup.
    ///
    /// # Panics
    ///
    /// If `per_sec` is 0 or above 1e9.
    pub fn at_rate(per_sec: u64, requests: u64) -> OpenLoop {
        assert!(
            (1..=1_000_000_000).contains(&per_sec),
            "rate must be in 1..=1e9 per second"
        );
        OpenLoop {
            interval_ns: 1_000_000_000 / per_sec,
            requests,
            warmup: 0,
        }
    }

    /// The same schedule preceded by `warmup` unmeasured requests.
    pub fn with_warmup(self, warmup: u64) -> OpenLoop {
        OpenLoop { warmup, ..self }
    }

    /// Intended start of request `i` (counting warmup) for a run that began
    /// at `start`.
    pub fn intended_start(&self, start: u64, i: u64) -> u64 {
        start + i * self.interval_ns
    }

    /// Runs the schedule on one thread: for each request, wait until its
    /// intended start (or go immediately if already late — never skip,
    /// never re-base the schedule), call `service(time, i)`, and record
    ///
    /// * `response`: completion − **intended** start (includes queueing
    ///   behind earlier slow requests — what clients experience), and
    /// * `service`: completion − actual start (what a closed-loop tester
    ///   reports).
    ///
    /// `i` counts from 0 including warmup. Two clock reads plus two histogram
    /// records of overhead per request; no allocation after setup.
    pub fn run<T, F>(&self, time: &mut T, service: F) -> LoadReport
    where
        T: TimeSource,
        F: FnMut(&mut T, u64),
    {
        let mut service = service;
        let mut report = LoadReport {
            response: Histogram::new(),
            service: Histogram::new(),
            requests: 0,
            elapsed_ns: 0,
            max_lag_ns: 0,
            interval_ns: self.interval_ns,
        };
        let total = self.warmup + self.requests;
        let start = time.now();
        let mut measure_start = start;
        for i in 0..total {
            let intended = self.intended_start(start, i);
            time.wait_until(intended);
            let sent = time.now();
            if i == self.warmup {
                measure_start = intended;
            }
            service(time, i);
            let done = time.now();
            if i >= self.warmup {
                report.response.record(done.saturating_sub(intended));
                report.service.record(done.saturating_sub(sent));
                report.max_lag_ns = report.max_lag_ns.max(sent.saturating_sub(intended));
                report.requests += 1;
            }
        }
        report.elapsed_ns = time.now().saturating_sub(measure_start);
        report
    }
}

/// What an [`OpenLoop`] run measured.
#[derive(Clone, Debug)]
pub struct LoadReport {
    /// Completion − intended start: the coordinated-omission-free latency.
    pub response: Histogram,
    /// Completion − actual start: what a closed-loop tester would report.
    pub service: Histogram,
    /// Requests measured (warmup excluded).
    pub requests: u64,
    /// From the first measured request's intended start to the end.
    pub elapsed_ns: u64,
    /// How far behind schedule the generator fell at worst (actual −
    /// intended start). Near the service time when healthy; large values
    /// mean the system (or the generator) could not keep up.
    pub max_lag_ns: u64,
    /// The schedule's interval.
    pub interval_ns: u64,
}

impl LoadReport {
    /// Requests per second actually completed.
    pub fn achieved_rate(&self) -> f64 {
        if self.elapsed_ns == 0 {
            0.0
        } else {
            self.requests as f64 * 1e9 / self.elapsed_ns as f64
        }
    }

    /// Requests per second the schedule asked for.
    pub fn offered_rate(&self) -> f64 {
        if self.interval_ns == 0 {
            0.0
        } else {
            1e9 / self.interval_ns as f64
        }
    }
}

/// A rate-limited closed-loop tester — the classic coordinated-omission
/// trap, kept to demonstrate and correct it.
///
/// It aims to send one request per `interval_ns` slot, but waits for each
/// reply before sending the next. Slots that pass while it is blocked are
/// *omitted* — never sent, never measured — and each sample is timed from
/// the actual send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClosedLoop {
    /// Expected gap between requests.
    pub interval_ns: u64,
    /// Slots in the run (sent + omitted).
    pub slots: u64,
}

// @student w25
impl ClosedLoop {
    /// `slots` slots of `interval_ns` each.
    ///
    /// # Panics
    ///
    /// If `interval_ns` is 0.
    pub fn new(interval_ns: u64, slots: u64) -> ClosedLoop {
        assert!(interval_ns > 0, "interval must be positive");
        ClosedLoop { interval_ns, slots }
    }

    /// Runs the loop. Slot `k` is due at `start + k · interval`; after each
    /// reply the next request goes out at the first slot that is not already
    /// in the past, and the skipped slots are counted as omitted. Every
    /// sample (reply − actual send) is recorded twice: as-is into `raw` and
    /// with [`Histogram::record_corrected`] into `corrected`.
    pub fn run<T, F>(&self, time: &mut T, service: F) -> ClosedLoopReport
    where
        T: TimeSource,
        F: FnMut(&mut T, u64),
    {
        let mut service = service;
        let mut report = ClosedLoopReport {
            raw: Histogram::new(),
            corrected: Histogram::new(),
            sent: 0,
            omitted: 0,
        };
        let start = time.now();
        let mut slot = 0;
        while slot < self.slots {
            time.wait_until(start + slot * self.interval_ns);
            let sent = time.now();
            service(time, slot);
            let done = time.now();
            let latency = done.saturating_sub(sent);
            report.raw.record(latency);
            report.corrected.record_corrected(latency, self.interval_ns);
            report.sent += 1;
            // First slot due at or after `done`; the ones in between were
            // never sent.
            let next = done
                .saturating_sub(start)
                .div_ceil(self.interval_ns)
                .clamp(slot + 1, self.slots);
            report.omitted += next - (slot + 1);
            slot = next;
        }
        report
    }
}

/// What a [`ClosedLoop`] run measured.
#[derive(Clone, Debug)]
pub struct ClosedLoopReport {
    /// Reply − actual send for every request sent (biased low).
    pub raw: Histogram,
    /// The same samples recorded with coordinated-omission correction.
    pub corrected: Histogram,
    /// Requests sent.
    pub sent: u64,
    /// Slots skipped because the tester was still waiting for a reply.
    pub omitted: u64,
}
