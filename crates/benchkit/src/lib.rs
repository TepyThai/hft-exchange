//! Measurement harness for `hft-exchange` benchmarks.
//!
//! Every crate exposes one bench target (`benches/hft.rs`, `harness = false`)
//! that registers its benchmarks with [`run`]. `cargo xtask bench` runs each
//! benchmark id in its own process and collects the `BENCHJSON` lines this
//! module prints, so one panicking (unimplemented) benchmark never hides the
//! others.
//!
//! ```ignore
//! fn main() {
//!     benchkit::run(&[
//!         ("w06.add_cancel", "Add then cancel, 100k resting orders", bench_add_cancel),
//!     ]);
//! }
//! ```
//!
//! This crate is infrastructure, not part of the curriculum: it is never
//! stripped from the learner template.

use std::time::{Duration, Instant};

pub use std::hint::black_box;

/// Unit of a reported benchmark value. Must match the site's `BenchUnit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    OpsPerSec,
    MsgsPerSec,
    Ns,
    Us,
    Ratio,
    Bytes,
}

impl Unit {
    pub fn as_str(self) -> &'static str {
        match self {
            Unit::OpsPerSec => "ops_per_sec",
            Unit::MsgsPerSec => "msgs_per_sec",
            Unit::Ns => "ns",
            Unit::Us => "us",
            Unit::Ratio => "ratio",
            Unit::Bytes => "bytes",
        }
    }
}

/// A benchmark result. Latency results carry percentiles; throughput and
/// ratio results carry only `value`.
#[derive(Clone, Debug)]
pub struct Report {
    pub id: String,
    pub label: String,
    pub unit: Unit,
    pub value: f64,
    pub p50: Option<f64>,
    pub p99: Option<f64>,
    pub p999: Option<f64>,
    pub max: Option<f64>,
    pub samples: u64,
}

impl Report {
    fn to_json(&self) -> String {
        fn num(v: Option<f64>) -> String {
            v.map(|x| format!("{x:.3}")).unwrap_or_else(|| "null".into())
        }
        format!(
            "{{\"id\":{},\"label\":{},\"unit\":\"{}\",\"value\":{:.3},\"p50\":{},\"p99\":{},\"p999\":{},\"max\":{},\"samples\":{}}}",
            json_str(&self.id),
            json_str(&self.label),
            self.unit.as_str(),
            self.value,
            num(self.p50),
            num(self.p99),
            num(self.p999),
            num(self.max),
            self.samples
        )
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Prints a result in the format `cargo xtask bench` collects.
pub fn emit(report: &Report) {
    println!("BENCHJSON {}", report.to_json());
}

/// A registered benchmark: id, label, and the function that measures it and
/// returns its report.
pub type BenchFn = fn() -> Report;

/// Entry point for a crate's `benches/hft.rs`.
///
/// * `--list` prints `id<TAB>label` for every benchmark
/// * any other argument is a substring filter (cargo passes `--bench`, which
///   is ignored)
pub fn run(benches: &[(&str, &str, BenchFn)]) {
    let args: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("--bench")).collect();
    if args.iter().any(|a| a == "--list") {
        for (id, label, _) in benches {
            println!("{id}\t{label}");
        }
        return;
    }
    let filters: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    for (id, _, f) in benches {
        if filters.is_empty() || filters.iter().any(|flt| id.contains(flt.as_str())) {
            let report = f();
            emit(&report);
        }
    }
}

// ---------------------------------------------------------------------------
// Throughput

/// Runs `body(iterations)` repeatedly (after a warm-up) and reports the
/// median operations per second. `body` must perform `ops_per_call` logical
/// operations per call.
pub fn throughput(
    id: &str,
    label: &str,
    unit: Unit,
    ops_per_call: u64,
    mut body: impl FnMut(),
) -> Report {
    // Warm up for ~200ms or 3 calls, whichever comes later.
    let warm_start = Instant::now();
    let mut warm_calls = 0;
    while warm_calls < 3 || warm_start.elapsed() < Duration::from_millis(200) {
        body();
        warm_calls += 1;
        if warm_start.elapsed() > Duration::from_secs(5) {
            break;
        }
    }
    let mut rates = Vec::new();
    let measure_start = Instant::now();
    while rates.len() < 5 || (measure_start.elapsed() < Duration::from_secs(1) && rates.len() < 50) {
        let t = Instant::now();
        body();
        let secs = t.elapsed().as_secs_f64().max(1e-9);
        rates.push(ops_per_call as f64 / secs);
        if measure_start.elapsed() > Duration::from_secs(10) {
            break;
        }
    }
    rates.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = rates[rates.len() / 2];
    Report {
        id: id.into(),
        label: label.into(),
        unit,
        value: median,
        p50: None,
        p99: None,
        p999: None,
        max: None,
        samples: rates.len() as u64,
    }
}

// ---------------------------------------------------------------------------
// Latency

/// Measures `op` one call at a time and reports its latency distribution in
/// nanoseconds (or microseconds with `Unit::Us`). `setup` runs before each
/// timed call and is excluded from the measurement.
///
/// Timer overhead (~20ns) is included in every sample; benchmarks of very
/// cheap operations should batch with [`latency_batched`].
pub fn latency<S>(
    id: &str,
    label: &str,
    unit: Unit,
    iterations: u64,
    mut setup: impl FnMut() -> S,
    mut op: impl FnMut(S),
) -> Report {
    let mut hist = Histogram::new();
    // Warm-up: 10% of iterations, at least 100.
    for _ in 0..(iterations / 10).max(100) {
        let s = setup();
        op(s);
    }
    for _ in 0..iterations {
        let s = setup();
        let t = Instant::now();
        op(s);
        hist.record(t.elapsed().as_nanos() as u64);
    }
    hist.report(id, label, unit)
}

/// Like [`latency`], but times batches of `batch` calls and records the
/// per-call average of each batch — for operations cheaper than the timer.
pub fn latency_batched(
    id: &str,
    label: &str,
    unit: Unit,
    batches: u64,
    batch: u64,
    mut op: impl FnMut(),
) -> Report {
    let mut hist = Histogram::new();
    for _ in 0..(batches / 10).max(10) {
        for _ in 0..batch {
            op();
        }
    }
    for _ in 0..batches {
        let t = Instant::now();
        for _ in 0..batch {
            op();
        }
        hist.record(t.elapsed().as_nanos() as u64 / batch.max(1));
    }
    hist.report(id, label, unit)
}

/// Reports `a / b` (e.g. a speedup) as a ratio result.
pub fn ratio(id: &str, label: &str, a: f64, b: f64) -> Report {
    Report {
        id: id.into(),
        label: label.into(),
        unit: Unit::Ratio,
        value: if b == 0.0 { 0.0 } else { a / b },
        p50: None,
        p99: None,
        p999: None,
        max: None,
        samples: 1,
    }
}

/// Collects latency samples in nanoseconds for custom multi-threaded
/// benchmarks (e.g. cross-thread wakeups), then reports them.
#[derive(Clone)]
pub struct Histogram {
    /// Log-linear buckets: 2^7 = 128 linear sub-buckets per power of two
    /// (< 1% relative error), covering 1ns .. ~584 years.
    counts: Vec<u64>,
    total: u64,
    max: u64,
}

const SUB_BITS: u32 = 7;
const SUB: u64 = 1 << SUB_BITS;

impl Default for Histogram {
    fn default() -> Self {
        Self::new()
    }
}

impl Histogram {
    pub fn new() -> Self {
        Histogram { counts: vec![0; ((64 - SUB_BITS as usize) + 1) * SUB as usize], total: 0, max: 0 }
    }

    fn index(v: u64) -> usize {
        if v < SUB {
            return v as usize;
        }
        let msb = 63 - v.leading_zeros();
        let shift = msb - SUB_BITS;
        let bucket = (shift + 1) as u64;
        let sub = (v >> shift) - SUB;
        (bucket * SUB + sub) as usize
    }

    fn value_at(idx: usize) -> u64 {
        let idx = idx as u64;
        if idx < SUB {
            return idx;
        }
        let bucket = idx / SUB;
        let sub = idx % SUB;
        let shift = bucket - 1;
        // Midpoint of the bucket range.
        ((SUB + sub) << shift) + ((1u64 << shift) >> 1)
    }

    pub fn record(&mut self, nanos: u64) {
        self.counts[Self::index(nanos)] += 1;
        self.total += 1;
        self.max = self.max.max(nanos);
    }

    pub fn merge(&mut self, other: &Histogram) {
        for (a, b) in self.counts.iter_mut().zip(&other.counts) {
            *a += b;
        }
        self.total += other.total;
        self.max = self.max.max(other.max);
    }

    pub fn len(&self) -> u64 {
        self.total
    }

    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// Value at quantile `q` in [0, 1], in nanoseconds.
    pub fn quantile(&self, q: f64) -> u64 {
        if self.total == 0 {
            return 0;
        }
        let rank = ((q * self.total as f64).ceil() as u64).clamp(1, self.total);
        let mut seen = 0;
        for (i, c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= rank {
                return Self::value_at(i).min(self.max);
            }
        }
        self.max
    }

    pub fn report(&self, id: &str, label: &str, unit: Unit) -> Report {
        let scale = if unit == Unit::Us { 1000.0 } else { 1.0 };
        let p = |q: f64| self.quantile(q) as f64 / scale;
        Report {
            id: id.into(),
            label: label.into(),
            unit,
            value: p(0.5),
            p50: Some(p(0.5)),
            p99: Some(p(0.99)),
            p999: Some(p(0.999)),
            max: Some(self.max as f64 / scale),
            samples: self.total,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_quantiles_are_within_one_percent() {
        let mut h = Histogram::new();
        for v in 1..=100_000u64 {
            h.record(v);
        }
        for (q, want) in [(0.5, 50_000.0), (0.99, 99_000.0), (0.999, 99_900.0)] {
            let got = h.quantile(q) as f64;
            assert!((got - want).abs() / want < 0.01, "q={q} got={got} want={want}");
        }
        assert_eq!(h.quantile(1.0), 100_000);
    }

    #[test]
    fn json_escapes() {
        assert_eq!(json_str("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
    }
}
