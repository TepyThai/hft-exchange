//! Measurement: latency histograms, per-thread metrics, clocks, open-loop load harness, stage attribution.
//!
//! Curriculum: Weeks 2, 19, 21, 25, 39 (see https://tepy.dev/hft).
//!
//! | Week | Module | What you build |
//! | --- | --- | --- |
//! | 2 | [`histogram`] | [`Histogram`]: log-linear buckets, O(1) record, percentiles within 1%, exact merge |
//! | 19 | [`metrics`] | [`Registry`], per-thread [`Counter`] / [`LocalHistogram`], [`Collector`], Prometheus text |
//! | 21 | [`clock`], [`stamps`] | [`Clock`] (calibrated TSC, `Instant` fallback), [`StageStamps`] |
//! | 25 | [`load`] | [`OpenLoop`] load harness, [`ClosedLoop`] and coordinated-omission correction |
//! | 39 | [`attribution`] | [`Attribution`]: per-stage histograms, tail attribution, [`Budget`] checks |
//!
//! # Using it from other crates
//!
//! ```
//! use telemetry::{Attribution, Budget, Clock, Histogram, Registry, Stage, StageStamps, LATENCY_BOUNDS_NS};
//!
//! // One clock for the whole process, so stamps from different threads compare.
//! let clock = Clock::global();
//!
//! // Per-thread metrics: create handles once per thread, record on the hot path.
//! let registry = Registry::new();
//! let mut accepted = registry.counter("orders_accepted_total", "Orders accepted.");
//! let mut latency = registry.histogram("order_latency_ns", "Ingress to ack.", LATENCY_BOUNDS_NS);
//!
//! // Stamp the order as it moves through the pipeline.
//! let mut stamps = StageStamps::new();
//! stamps.stamp_now(Stage::Ingress, clock);
//! stamps.stamp_now(Stage::Gateway, clock);
//! stamps.stamp_now(Stage::Match, clock);
//! stamps.stamp_now(Stage::Ack, clock);
//! accepted.inc();
//! latency.record(stamps.total());
//!
//! // Off the hot path: expose and attribute.
//! let text = registry.collect().to_prometheus();
//! assert!(text.contains("orders_accepted_total 1"));
//! let attribution = Attribution::from_samples(&[stamps]);
//! let _violations = attribution.check(&[Budget { stage: Stage::Match, p99_ns: 1_000 }]);
//!
//! // Ad-hoc distributions.
//! let mut h = Histogram::new();
//! h.record(stamps.total());
//! let _p99 = h.percentile(0.99);
//! ```

pub mod attribution;
pub mod clock;
pub mod histogram;
pub mod load;
pub mod metrics;
pub mod stamps;

pub use attribution::{Attribution, Budget, StageReport, Violation};
pub use clock::{Clock, ClockSource};
pub use histogram::{Bucket, BucketLayout, Histogram, DEFAULT_PRECISION, MAX_PRECISION};
pub use load::{ClosedLoop, ClosedLoopReport, LoadReport, OpenLoop, SimTime, TimeSource};
pub use metrics::{
    Collector, Counter, CounterSample, HistogramSample, LocalHistogram, Registry, Snapshot,
    LATENCY_BOUNDS_NS,
};
pub use stamps::{Stage, StageStamps, StampIter};
