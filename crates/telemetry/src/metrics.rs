//! Week 19: per-thread metrics with zero hot-path contention, merged off the
//! hot path, exposed in Prometheus text format.
//!
//! A shared `AtomicU64::fetch_add` counter costs a locked read-modify-write
//! and, worse, bounces its cache line between every core that touches it.
//! Instead every thread gets its **own** cells:
//!
//! * [`Registry::counter`] / [`Registry::histogram`] hand out a fresh
//!   per-thread handle ([`Counter`] / [`LocalHistogram`]) each time they are
//!   called. Call them once per thread at startup, never per operation.
//! * A handle has exactly one writer (its methods take `&mut self`), so it
//!   updates its cells with plain relaxed loads and stores — no `lock`
//!   prefix, no shared cache line, a few ns.
//! * [`Registry::collect`] (or a background [`Collector`]) reads every
//!   thread's cells with relaxed loads and merges handles that share a name
//!   and label set into one [`Snapshot`]. Values of dropped handles are kept.
//! * [`Snapshot::to_prometheus`] renders the text exposition format
//!   (version 0.0.4) that a `/metrics` endpoint serves.
//!
//! A snapshot taken while threads are recording is not an atomic cut: a
//! histogram's `_sum` may include a sample whose bucket is not yet visible.
//! Every value is eventually exact once recording stops.
//!
//! ```
//! use telemetry::{Registry, LATENCY_BOUNDS_NS};
//!
//! let registry = Registry::new();
//! let reg = registry.clone();
//! std::thread::spawn(move || {
//!     let mut orders = reg.counter("orders_total", "Orders accepted.");
//!     let mut latency = reg.histogram("order_latency_ns", "Order latency.", LATENCY_BOUNDS_NS);
//!     orders.inc();
//!     latency.record(420);
//! })
//! .join()
//! .unwrap();
//! let text = registry.collect().to_prometheus();
//! assert!(text.contains("orders_total 1\n"));
//! ```

use crate::histogram::Histogram;
use std::sync::OnceLock;
use std::time::Duration;

/// Default Prometheus `le` bounds for nanosecond latency histograms:
/// 100 ns … 10 ms in 1-2.5-5 steps.
pub const LATENCY_BOUNDS_NS: &[u64] = &[
    100, 250, 500, 1_000, 2_500, 5_000, 10_000, 25_000, 50_000, 100_000, 250_000, 500_000,
    1_000_000, 2_500_000, 5_000_000, 10_000_000,
];

/// A set of metrics shared by every thread of a process.
///
/// Cloning is cheap (one `Arc`) and every clone sees the same metrics.
/// Registration and collection take a lock; recording through the handles
/// never does.
// @student w19
#[derive(Clone)]
pub struct Registry {
    inner: Arc<Inner>,
}

// @student w19
impl Registry {
    /// An empty registry.
    pub fn new() -> Registry {
        Registry {
            inner: Arc::new(Inner {
                families: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    /// A new per-thread handle for counter `name` (no labels). See
    /// [`counter_with_labels`](Self::counter_with_labels).
    pub fn counter(&self, name: &str, help: &str) -> Counter {
        self.counter_with_labels(name, help, &[])
    }

    /// A new per-thread handle for the counter series `name{labels}`.
    /// Every handle of the same series is summed at collection. Takes the
    /// registry lock and allocates; call once per thread, not per event.
    ///
    /// `help` is taken from the first registration of `name`.
    ///
    /// # Panics
    ///
    /// If `name` or a label name is not a valid Prometheus identifier, or
    /// `name` is already registered as a histogram.
    pub fn counter_with_labels(&self, name: &str, help: &str, labels: &[(&str, &str)]) -> Counter {
        validate_metric_name(name);
        let labels = normalize_labels(labels, false);
        let cell = Arc::new(CounterCell(AtomicU64::new(0)));
        let mut families = lock(&self.inner.families);
        let family = families
            .entry(name.to_string())
            .or_insert_with(|| Family::Counter {
                help: help.to_string(),
                series: BTreeMap::new(),
            });
        match family {
            Family::Counter { series, .. } => {
                series.entry(labels).or_default().live.push(cell.clone())
            }
            Family::Histogram { .. } => {
                panic!("metric `{name}` is already registered as a histogram")
            }
        }
        Counter { cell }
    }

    /// A new per-thread handle for histogram `name` (no labels) exposed with
    /// the given `le` bounds. See
    /// [`histogram_with_labels`](Self::histogram_with_labels).
    pub fn histogram(&self, name: &str, help: &str, bounds: &[u64]) -> LocalHistogram {
        self.histogram_with_labels(name, help, bounds, &[])
    }

    /// A new per-thread handle for the histogram series `name{labels}`.
    ///
    /// Samples are kept at full [`Histogram`] precision (< 1% error); the
    /// `bounds` (strictly increasing, `+Inf` implied) only choose which
    /// cumulative `_bucket{le=...}` lines the exposition prints. Allocates a
    /// 58 KiB bucket array per handle; call once per thread.
    ///
    /// # Panics
    ///
    /// If a name is invalid, a label is called `le`, `bounds` is not
    /// strictly increasing, or `name` is already registered as a counter or
    /// with different bounds.
    pub fn histogram_with_labels(
        &self,
        name: &str,
        help: &str,
        bounds: &[u64],
        labels: &[(&str, &str)],
    ) -> LocalHistogram {
        validate_metric_name(name);
        assert!(
            bounds.windows(2).all(|w| w[0] < w[1]),
            "histogram `{name}`: bounds must be strictly increasing"
        );
        let labels = normalize_labels(labels, true);
        let cell = Arc::new(HistCell::new());
        let mut families = lock(&self.inner.families);
        let family = families
            .entry(name.to_string())
            .or_insert_with(|| Family::Histogram {
                help: help.to_string(),
                bounds: bounds.to_vec(),
                series: BTreeMap::new(),
            });
        match family {
            Family::Histogram {
                bounds: existing,
                series,
                ..
            } => {
                assert_eq!(
                    existing.as_slice(),
                    bounds,
                    "histogram `{name}` registered with different bounds"
                );
                series
                    .entry(labels)
                    .or_insert_with(|| HistSeries {
                        live: Vec::new(),
                        retired: Histogram::new(),
                    })
                    .live
                    .push(cell.clone());
            }
            Family::Counter { .. } => panic!("metric `{name}` is already registered as a counter"),
        }
        LocalHistogram { cell }
    }

    /// Reads every thread's cells and merges them per series. Families are
    /// sorted by name, series by labels. Handles that were dropped are folded
    /// into a per-series total and released, so memory stays bounded by the
    /// number of live handles. Takes the registry lock; O(handles × buckets).
    pub fn collect(&self) -> Snapshot {
        let mut families = lock(&self.inner.families);
        let mut snapshot = Snapshot::default();
        for (name, family) in families.iter_mut() {
            match family {
                Family::Counter { help, series } => {
                    for (labels, s) in series.iter_mut() {
                        let CounterSeries { live, retired } = s;
                        // `get_mut` succeeds only once the handle is gone, and
                        // synchronises with its last write.
                        live.retain_mut(|cell| match Arc::get_mut(cell) {
                            Some(v) => {
                                *retired = retired.wrapping_add(*v.0.get_mut());
                                false
                            }
                            None => true,
                        });
                        let value = live.iter().fold(*retired, |acc, cell| {
                            acc.wrapping_add(cell.0.load(Ordering::Relaxed))
                        });
                        snapshot.counters.push(CounterSample {
                            name: name.clone(),
                            help: help.clone(),
                            labels: labels.clone(),
                            value,
                        });
                    }
                }
                Family::Histogram {
                    help,
                    bounds,
                    series,
                } => {
                    for (labels, s) in series.iter_mut() {
                        let HistSeries { live, retired } = s;
                        live.retain_mut(|cell| match Arc::get_mut(cell) {
                            Some(c) => {
                                retired.merge(&c.snapshot());
                                false
                            }
                            None => true,
                        });
                        let mut histogram = retired.clone();
                        for cell in live.iter() {
                            histogram.merge(&cell.snapshot());
                        }
                        snapshot.histograms.push(HistogramSample {
                            name: name.clone(),
                            help: help.clone(),
                            labels: labels.clone(),
                            bounds: bounds.clone(),
                            histogram,
                        });
                    }
                }
            }
        }
        snapshot
    }

    /// Starts a background thread that calls `sink(self.collect())` every
    /// `period`, plus once more when the returned [`Collector`] is stopped
    /// or dropped. Typical sinks store the rendered text for a `/metrics`
    /// endpoint or log a summary.
    pub fn spawn_collector<F>(&self, period: Duration, sink: F) -> Collector
    where
        F: FnMut(Snapshot) + Send + 'static,
    {
        let mut sink = sink;
        let (stop, stopped) = mpsc::channel::<()>();
        let registry = self.clone();
        let thread = std::thread::Builder::new()
            .name("telemetry-collector".into())
            .spawn(move || loop {
                match stopped.recv_timeout(period) {
                    Err(mpsc::RecvTimeoutError::Timeout) => sink(registry.collect()),
                    // Stop requested (sender dropped): publish a final snapshot.
                    _ => {
                        sink(registry.collect());
                        break;
                    }
                }
            })
            .expect("failed to spawn the telemetry collector thread");
        Collector {
            stop: Some(stop),
            thread: Some(thread),
        }
    }
}

impl Registry {
    /// A process-wide registry, created on first use.
    pub fn global() -> &'static Registry {
        static GLOBAL: OnceLock<Registry> = OnceLock::new();
        GLOBAL.get_or_init(Registry::new)
    }

    /// `self.collect().to_prometheus()`.
    pub fn render_prometheus(&self) -> String {
        self.collect().to_prometheus()
    }
}

impl Default for Registry {
    fn default() -> Registry {
        Registry::new()
    }
}

/// One thread's handle on a counter series. Single writer (`&mut self`), so
/// [`add`](Counter::add) is a relaxed load + store on a cell no other thread
/// writes: ~1 ns, no contention. `Send`, so it can be created on one thread
/// and moved to the thread that uses it.
// @student w19
pub struct Counter {
    cell: Arc<CounterCell>,
}

// @student w19
impl Counter {
    /// Adds 1.
    #[inline]
    pub fn inc(&mut self) {
        self.add(1);
    }

    /// Adds `n` (wrapping at `u64::MAX`, as Prometheus counters do).
    #[inline]
    pub fn add(&mut self, n: u64) {
        bump(&self.cell.0, n);
    }

    /// This handle's own total (not the series total — see
    /// [`Registry::collect`]).
    pub fn get(&self) -> u64 {
        self.cell.0.load(Ordering::Relaxed)
    }
}

/// One thread's handle on a histogram series: a [`Histogram`]-layout bucket
/// array this thread alone writes, with relaxed stores. `record` costs a
/// bucket index plus a handful of uncontended relaxed loads and stores (no
/// `lock` prefix), ~2-3 ns.
// @student w19
pub struct LocalHistogram {
    cell: Arc<HistCell>,
}

// @student w19
impl LocalHistogram {
    /// Records one sample (ns). O(1), no allocation, no lock, no RMW.
    #[inline]
    pub fn record(&mut self, value: u64) {
        self.cell.record(value);
    }

    /// Samples recorded through this handle.
    pub fn count(&self) -> u64 {
        self.cell.count.load(Ordering::Relaxed)
    }
}

/// A background collection thread started by [`Registry::spawn_collector`].
/// Dropping it (or calling [`stop`](Collector::stop)) runs one final
/// collection and joins the thread.
// @student w19
pub struct Collector {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

// @student w19
impl Collector {
    /// Stops the thread after one final collection and waits for it.
    pub fn stop(self) {
        drop(self);
    }
}

// @student w19
impl Drop for Collector {
    fn drop(&mut self) {
        // Disconnecting the channel wakes the thread for its final round.
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Collected metrics, ready to inspect or render.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// Counter series, sorted by (name, labels).
    pub counters: Vec<CounterSample>,
    /// Histogram series, sorted by (name, labels).
    pub histograms: Vec<HistogramSample>,
}

/// One counter series in a [`Snapshot`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CounterSample {
    pub name: String,
    pub help: String,
    /// Sorted by label name.
    pub labels: Vec<(String, String)>,
    pub value: u64,
}

/// One histogram series in a [`Snapshot`]: every thread's samples merged.
#[derive(Clone, Debug)]
pub struct HistogramSample {
    pub name: String,
    pub help: String,
    /// Sorted by label name.
    pub labels: Vec<(String, String)>,
    /// The `le` bounds exposed (`+Inf` implied).
    pub bounds: Vec<u64>,
    pub histogram: Histogram,
}

impl Snapshot {
    /// The value of counter series `name{labels}` (labels in any order).
    pub fn counter(&self, name: &str, labels: &[(&str, &str)]) -> Option<u64> {
        self.counters
            .iter()
            .find(|c| c.name == name && same_labels(&c.labels, labels))
            .map(|c| c.value)
    }

    /// The merged histogram of series `name{labels}` (labels in any order).
    pub fn histogram(&self, name: &str, labels: &[(&str, &str)]) -> Option<&Histogram> {
        self.histograms
            .iter()
            .find(|h| h.name == name && same_labels(&h.labels, labels))
            .map(|h| &h.histogram)
    }

    // @student w19
    /// Renders the Prometheus text exposition format (0.0.4).
    ///
    /// Families are sorted by name, each introduced by `# HELP` (omitted
    /// when the help text is empty) and `# TYPE` lines, followed by its
    /// series sorted by labels. Counters print `name{labels} value`;
    /// histograms print one cumulative `name_bucket{labels,le="B"}` line per
    /// bound, `le="+Inf"`, then `name_sum` and `name_count`. HELP text
    /// escapes `\` and newlines; label values also escape `"`. Every line
    /// ends with `\n`.
    ///
    /// ```text
    /// # HELP orders_total Orders accepted.
    /// # TYPE orders_total counter
    /// orders_total{side="buy"} 7
    /// # TYPE latency_ns histogram
    /// latency_ns_bucket{le="100"} 2
    /// latency_ns_bucket{le="+Inf"} 3
    /// latency_ns_sum 1234
    /// latency_ns_count 3
    /// ```
    pub fn to_prometheus(&self) -> String {
        use std::fmt::Write;

        enum Entry<'a> {
            Counter(&'a CounterSample),
            Histogram(&'a HistogramSample),
        }
        impl Entry<'_> {
            fn key(&self) -> (&str, &[(String, String)]) {
                match self {
                    Entry::Counter(c) => (&c.name, &c.labels),
                    Entry::Histogram(h) => (&h.name, &h.labels),
                }
            }
        }

        let mut entries: Vec<Entry<'_>> = self
            .counters
            .iter()
            .map(Entry::Counter)
            .chain(self.histograms.iter().map(Entry::Histogram))
            .collect();
        entries.sort_by(|a, b| a.key().cmp(&b.key()));

        let mut out = String::new();
        let mut current: Option<&str> = None;
        for entry in &entries {
            let (name, help, kind) = match entry {
                Entry::Counter(c) => (c.name.as_str(), c.help.as_str(), "counter"),
                Entry::Histogram(h) => (h.name.as_str(), h.help.as_str(), "histogram"),
            };
            if current != Some(name) {
                current = Some(name);
                if !help.is_empty() {
                    let _ = writeln!(out, "# HELP {name} {}", escape(help, false));
                }
                let _ = writeln!(out, "# TYPE {name} {kind}");
            }
            match entry {
                Entry::Counter(c) => {
                    let _ = writeln!(out, "{name}{} {}", render_labels(&c.labels, None), c.value);
                }
                Entry::Histogram(h) => {
                    for &bound in &h.bounds {
                        let le = bound.to_string();
                        let count = h.histogram.count_at_or_below(bound);
                        let _ = writeln!(
                            out,
                            "{name}_bucket{} {count}",
                            render_labels(&h.labels, Some(&le))
                        );
                    }
                    let _ = writeln!(
                        out,
                        "{name}_bucket{} {}",
                        render_labels(&h.labels, Some("+Inf")),
                        h.histogram.count()
                    );
                    let labels = render_labels(&h.labels, None);
                    let _ = writeln!(out, "{name}_sum{labels} {}", h.histogram.sum());
                    let _ = writeln!(out, "{name}_count{labels} {}", h.histogram.count());
                }
            }
        }
        out
    }
}

/// True if `have` (sorted) and `want` (any order) are the same label set.
fn same_labels(have: &[(String, String)], want: &[(&str, &str)]) -> bool {
    have.len() == want.len()
        && want
            .iter()
            .all(|(k, v)| have.iter().any(|(hk, hv)| hk == k && hv == v))
}

// @student-begin w19
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use crate::histogram::{BucketLayout, DEFAULT_PRECISION};

type LabelSet = Vec<(String, String)>;

struct Inner {
    families: Mutex<BTreeMap<String, Family>>,
}

enum Family {
    Counter {
        help: String,
        series: BTreeMap<LabelSet, CounterSeries>,
    },
    Histogram {
        help: String,
        bounds: Vec<u64>,
        series: BTreeMap<LabelSet, HistSeries>,
    },
}

#[derive(Default)]
struct CounterSeries {
    /// Cells of handles that may still be written.
    live: Vec<Arc<CounterCell>>,
    /// Total of handles already dropped.
    retired: u64,
}

struct HistSeries {
    live: Vec<Arc<HistCell>>,
    retired: Histogram,
}

/// One counter handle's value, alone on its cache lines: cells of
/// different threads are separate allocations that could otherwise share a
/// line and turn every increment into a cache-line transfer (false
/// sharing). 128 bytes covers adjacent-line prefetching.
#[repr(align(128))]
struct CounterCell(AtomicU64);

/// A single-writer histogram in [`Histogram`]'s bucket layout, readable
/// from other threads at any time. Aligned like [`CounterCell`] so the hot
/// `count`/`sum`/`min`/`max` words never share a line with another thread's.
#[repr(align(128))]
struct HistCell {
    layout: BucketLayout,
    buckets: Box<[AtomicU64]>,
    count: AtomicU64,
    sum: AtomicU64,
    min: AtomicU64,
    max: AtomicU64,
}

impl HistCell {
    fn new() -> HistCell {
        let layout = BucketLayout::new(DEFAULT_PRECISION);
        HistCell {
            layout,
            buckets: (0..layout.bucket_count())
                .map(|_| AtomicU64::new(0))
                .collect(),
            count: AtomicU64::new(0),
            sum: AtomicU64::new(0),
            min: AtomicU64::new(u64::MAX),
            max: AtomicU64::new(0),
        }
    }

    /// Only the owning handle calls this (single writer), so plain relaxed
    /// load + store is a correct increment.
    #[inline]
    fn record(&self, value: u64) {
        bump(&self.buckets[self.layout.index(value)], 1);
        bump(&self.count, 1);
        let sum = self.sum.load(Ordering::Relaxed);
        self.sum.store(sum.saturating_add(value), Ordering::Relaxed);
        if value < self.min.load(Ordering::Relaxed) {
            self.min.store(value, Ordering::Relaxed);
        }
        if value > self.max.load(Ordering::Relaxed) {
            self.max.store(value, Ordering::Relaxed);
        }
    }

    fn snapshot(&self) -> Histogram {
        let counts: Vec<u64> = self
            .buckets
            .iter()
            .map(|b| b.load(Ordering::Relaxed))
            .collect();
        Histogram::from_parts(
            self.layout,
            &counts,
            self.sum.load(Ordering::Relaxed),
            self.min.load(Ordering::Relaxed),
            self.max.load(Ordering::Relaxed),
        )
    }
}

/// Single-writer increment: no `lock`-prefixed instruction.
#[inline]
fn bump(cell: &AtomicU64, n: u64) {
    cell.store(
        cell.load(Ordering::Relaxed).wrapping_add(n),
        Ordering::Relaxed,
    );
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic while holding the lock cannot leave the maps inconsistent
    // (every mutation is a single insert/push), so poisoning is ignored.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn validate_metric_name(name: &str) {
    let ok = !name.is_empty()
        && name.chars().enumerate().all(|(i, c)| {
            c == '_' || c == ':' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit())
        });
    assert!(ok, "invalid Prometheus metric name `{name}`");
}

fn normalize_labels(labels: &[(&str, &str)], histogram: bool) -> LabelSet {
    let mut out: LabelSet = labels
        .iter()
        .map(|&(k, v)| (k.to_string(), v.to_string()))
        .collect();
    for (k, _) in &out {
        let ok = !k.is_empty()
            && !k.starts_with("__")
            && k.chars()
                .enumerate()
                .all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()));
        assert!(ok, "invalid Prometheus label name `{k}`");
        assert!(
            !(histogram && k == "le"),
            "`le` is reserved for histogram buckets"
        );
    }
    out.sort();
    assert!(
        out.windows(2).all(|w| w[0].0 != w[1].0),
        "duplicate label name"
    );
    out
}

/// Escapes a HELP text (`label == false`: `\` and newline) or a label value
/// (`label == true`: also `"`).
fn escape(s: &str, label: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '"' if label => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out
}

/// `{a="1",b="2",le="100"}`, or the empty string when there is nothing to print.
fn render_labels(labels: &[(String, String)], le: Option<&str>) -> String {
    let mut parts: Vec<String> = labels
        .iter()
        .map(|(k, v)| format!("{k}=\"{}\"", escape(v, true)))
        .collect();
    if let Some(le) = le {
        parts.push(format!("le=\"{le}\""));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("{{{}}}", parts.join(","))
    }
}
// @student-end
