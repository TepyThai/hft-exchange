//! Week 39: per-stage latency attribution and budgets.
//!
//! End-to-end latency is the sum of its stages, but the *tail* of the sum is
//! not the sum of the stage tails: a stage with a modest p99 can still own
//! most of the end-to-end p99 if its rare spikes are what make the slow
//! orders slow. [`Attribution`] therefore reports two things per stage:
//!
//! * its own distribution (a [`Histogram`] of the interval ending at that
//!   stamp — see [`StageStamps::interval`]), checked against [`Budget`]s;
//! * its **tail share**: among the samples whose end-to-end latency is at or
//!   above the end-to-end p99, the fraction of their total time spent in
//!   that stage. The stage with the largest share is the one to optimise to
//!   move the end-to-end p99 ([`Attribution::dominant_stage`]).

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::histogram::Histogram;
use crate::stamps::{Stage, StageStamps};
use std::fmt;

/// A latency budget: the p99 of the interval ending at `stage` must not
/// exceed `p99_ns`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    pub stage: Stage,
    pub p99_ns: u64,
}

/// A budget that was exceeded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Violation {
    pub stage: Stage,
    pub budget_ns: u64,
    /// The stage's measured p99 (histogram precision: ≤ 1% above the true
    /// value).
    pub p99_ns: u64,
}

/// One row of [`Attribution::report`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StageReport {
    pub stage: Stage,
    /// Samples that stamped this stage (and something before it).
    pub count: u64,
    pub p50_ns: u64,
    pub p99_ns: u64,
    pub p999_ns: u64,
    pub max_ns: u64,
    /// Mean time in this stage over the tail samples.
    pub tail_mean_ns: f64,
    /// Fraction (0..=1) of the tail samples' total time spent in this stage.
    pub tail_share: f64,
}

/// Per-stage latency breakdown of many [`StageStamps`].
#[derive(Clone, Debug)]
pub struct Attribution {
    // TODO(w39): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl Attribution {
    /// Attribution with the tail at the end-to-end p99. See
    /// [`with_tail`](Self::with_tail).
    pub fn from_samples(samples: &[StageStamps]) -> Attribution {
        todo!("w39: from_samples")
    }

    /// Builds per-stage and end-to-end histograms from `samples` (two
    /// passes, O(n) plus an O(n) selection; allocates one `u64` per sample).
    ///
    /// A stage's interval is measured from the previous stamp *present in
    /// that sample*, so a reject that skips `Match` charges `Risk → Ack` to
    /// `Ack`. End-to-end is last stamp − first stamp. Samples with fewer than
    /// two stamps are ignored. The tail is every sample whose exact
    /// end-to-end latency is at or above the exact `tail_quantile` (same
    /// index rule as [`Histogram::percentile`]).
    pub fn with_tail(samples: &[StageStamps], tail_quantile: f64) -> Attribution {
        todo!("w39: with_tail")
    }

    /// Samples attributed (those with at least two stamps).
    pub fn samples(&self) -> u64 {
        todo!("w39: samples")
    }

    /// End-to-end (first → last stamp) latency distribution.
    pub fn end_to_end(&self) -> &Histogram {
        todo!("w39: end_to_end")
    }

    /// Distribution of the interval ending at `stage`, if any sample had it.
    pub fn stage(&self, stage: Stage) -> Option<&Histogram> {
        todo!("w39: stage")
    }

    /// The quantile that defines the tail (0.99 for `from_samples`).
    pub fn tail_quantile(&self) -> f64 {
        todo!("w39: tail_quantile")
    }

    /// Exact end-to-end latency at the tail quantile; samples at or above it
    /// form the tail.
    pub fn tail_threshold_ns(&self) -> u64 {
        todo!("w39: tail_threshold_ns")
    }

    /// Number of tail samples.
    pub fn tail_count(&self) -> u64 {
        todo!("w39: tail_count")
    }

    /// Mean time spent in `stage` by the tail samples (0 if none).
    pub fn tail_mean_ns(&self, stage: Stage) -> f64 {
        todo!("w39: tail_mean_ns")
    }

    /// Fraction of the tail samples' total stage time spent in `stage`
    /// (shares of all stages sum to 1 when any time was spent).
    pub fn tail_share(&self, stage: Stage) -> f64 {
        todo!("w39: tail_share")
    }

    /// The stage that contributes the most time to the tail samples — the
    /// one that dominates end-to-end p99. Ties go to the earlier stage;
    /// `None` without samples (or if no time was spent at all).
    pub fn dominant_stage(&self) -> Option<Stage> {
        todo!("w39: dominant_stage")
    }

    /// One row per stage that has samples, in pipeline order.
    pub fn report(&self) -> Vec<StageReport> {
        todo!("w39: report")
    }

    /// Every budget whose stage's p99 exceeds it, in `budgets` order.
    /// Budgets for stages without samples are skipped.
    pub fn check(&self, budgets: &[Budget]) -> Vec<Violation> {
        todo!("w39: check")
    }
}

/// A plain-text table: one line per stage plus end-to-end, with the
/// dominant stage marked.
impl fmt::Display for Attribution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dominant = self.dominant_stage();
        writeln!(
            f,
            "{:<10} {:>9} {:>9} {:>9} {:>9} {:>9} {:>10}",
            "stage", "count", "p50", "p99", "p99.9", "max", "tail share"
        )?;
        for r in self.report() {
            writeln!(
                f,
                "{:<10} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9.1}%{}",
                r.stage.name(),
                r.count,
                r.p50_ns,
                r.p99_ns,
                r.p999_ns,
                r.max_ns,
                r.tail_share * 100.0,
                if Some(r.stage) == dominant {
                    "  <- dominates p99"
                } else {
                    ""
                }
            )?;
        }
        let e = self.end_to_end();
        writeln!(
            f,
            "{:<10} {:>9} {:>9} {:>9} {:>9} {:>9}",
            "end-to-end",
            e.count(),
            e.percentile(0.5),
            e.percentile(0.99),
            e.percentile(0.999),
            e.max()
        )
    }
}
