//! Week 2: a fixed-bucket, log-linear latency histogram.
//!
//! Latency is a distribution, not a number. Keeping every sample and sorting
//! is exact but O(n log n) and allocates; a histogram with the right bucket
//! layout records in O(1) with no allocation and answers any percentile to
//! a known relative error.
//!
//! # Layout
//!
//! With precision `p`, values below `2^p` each get their own bucket. Every
//! power-of-two range above that, `[2^k, 2^(k+1))`, is split into `2^p`
//! equal-width linear sub-buckets of width `2^(k-p)` — so values below
//! `2^(p+1)` are still recorded exactly. A bucket's width is therefore at most `2^-p` of the
//! values it holds, so reporting the bucket's highest value is never more
//! than `2^-p` above the true sample: precision 7 (the default) gives
//! 128 sub-buckets per power of two and < 0.79% error, which is HdrHistogram's
//! "2 significant digits". The whole `u64` range fits in `(65 - p) * 2^p`
//! buckets (7,424 at p = 7, 58 KiB of counts).
//!
//! ```text
//! value:   0 1 2 … 127 | 128 129 … 255 | 256 258 … 510 | 512 516 …
//! width:   1 1 1 …   1 |   1   1 …   1 |   2   2 …   2 |   4   4 …
//! ```
//!
//! # Percentile definition
//!
//! `percentile(q)` returns (the bucket containing) the sample at index
//! `⌊q·n⌋` of the sorted samples, clamped to `n − 1` — so `percentile(0.99)`
//! of 10,000 samples is the 9,901st smallest, the first sample *above* the
//! fastest 99%. The answer is the highest value in that bucket, clamped to
//! the recorded `[min, max]`: never below the true value and at most the
//! layout's relative error above it.

use std::fmt;

/// Default precision: 2^7 = 128 linear sub-buckets per power of two, so a
/// reported value is within 1/128 ≈ 0.78% of the true sample (< 1%).
pub const DEFAULT_PRECISION: u32 = 7;

/// Largest supported precision: 2^12 sub-buckets per power of two
/// (0.024% error, 53 × 4096 buckets ≈ 1.7 MB of counts).
pub const MAX_PRECISION: u32 = 12;

/// The bucket layout of a log-linear histogram with `2^precision` linear
/// sub-buckets per power of two.
///
/// Shared by [`Histogram`] and by anything that keeps counts in the same
/// shape (the per-thread atomic histograms of week 19), so that bucket
/// arrays can be merged index by index. All methods are O(1), branch-light
/// and allocation-free.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BucketLayout {
    precision: u32,
}

// @student w02
impl BucketLayout {
    /// A layout with `2^precision` sub-buckets per power of two.
    ///
    /// # Panics
    ///
    /// If `precision` is not in `1..=MAX_PRECISION`.
    pub fn new(precision: u32) -> BucketLayout {
        assert!(
            (1..=MAX_PRECISION).contains(&precision),
            "histogram precision must be in 1..={MAX_PRECISION}, got {precision}"
        );
        BucketLayout { precision }
    }

    /// The precision this layout was built with (log2 of the sub-bucket count).
    pub fn precision(self) -> u32 {
        self.precision
    }

    /// Upper bound on `(reported − true) / true` for any value: `2^-precision`.
    pub fn max_relative_error(self) -> f64 {
        1.0 / (1u64 << self.precision) as f64
    }

    /// Number of buckets needed to cover every `u64`: `(65 − p) · 2^p`.
    pub fn bucket_count(self) -> usize {
        (65 - self.precision as usize) << self.precision
    }

    /// The bucket `value` falls in. O(1): one `leading_zeros`, a shift and
    /// a subtraction.
    #[inline]
    pub fn index(self, value: u64) -> usize {
        let p = self.precision;
        let sub = 1u64 << p;
        if value < sub {
            return value as usize;
        }
        // value has its top bit at `msb >= p`; keep the p bits below it.
        let msb = 63 - value.leading_zeros();
        let shift = msb - p;
        let group = shift as u64 + 1;
        (group * sub + ((value >> shift) - sub)) as usize
    }

    /// Smallest value that maps to bucket `index`.
    pub fn lowest(self, index: usize) -> u64 {
        let p = self.precision;
        let sub = 1u64 << p;
        let index = index as u64;
        if index < sub {
            return index;
        }
        let group = index >> p;
        let offset = index & (sub - 1);
        (sub + offset) << (group - 1)
    }

    /// Largest value that maps to bucket `index`.
    pub fn highest(self, index: usize) -> u64 {
        let p = self.precision;
        let sub = 1u64 << p;
        if (index as u64) < sub {
            return index as u64;
        }
        let group = (index as u64) >> p;
        let width = 1u64 << (group - 1);
        self.lowest(index) + (width - 1)
    }
}

/// One non-empty bucket of a [`Histogram`]: every recorded value in
/// `low..=high` was counted here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bucket {
    pub low: u64,
    pub high: u64,
    pub count: u64,
}

/// A fixed-bucket log-linear histogram of `u64` samples (typically
/// nanoseconds).
///
/// * [`record`](Histogram::record) is O(1), allocation-free and branch-light
///   — cheap enough for the hot path (a few ns).
/// * [`percentile`](Histogram::percentile) is O(buckets) and accurate to
///   [`max_relative_error`](Histogram::max_relative_error) (< 1% by default).
/// * Two histograms with the same precision [`merge`](Histogram::merge)
///   exactly: merging per-thread histograms equals recording everything in
///   one.
///
/// `count`, `sum`, `min` and `max` are tracked exactly. The sum saturates at
/// `u64::MAX` (≈ 584 years of nanoseconds).
// @student w02
#[derive(Clone, PartialEq, Eq)]
pub struct Histogram {
    layout: BucketLayout,
    counts: Box<[u64]>,
    count: u64,
    sum: u64,
    /// `u64::MAX` while empty, so `record` needs no "first sample" branch.
    min: u64,
    max: u64,
}

// @student w02
impl Histogram {
    /// An empty histogram with [`DEFAULT_PRECISION`] (< 1% error). Allocates
    /// its bucket array once (58 KiB).
    pub fn new() -> Histogram {
        Histogram::with_precision(DEFAULT_PRECISION)
    }

    /// An empty histogram with `2^precision` sub-buckets per power of two:
    /// relative error `2^-precision`, memory `(65 − p) · 2^p · 8` bytes.
    ///
    /// # Panics
    ///
    /// If `precision` is not in `1..=MAX_PRECISION`.
    pub fn with_precision(precision: u32) -> Histogram {
        let layout = BucketLayout::new(precision);
        Histogram {
            layout,
            counts: vec![0; layout.bucket_count()].into_boxed_slice(),
            count: 0,
            sum: 0,
            min: u64::MAX,
            max: 0,
        }
    }

    /// The bucket layout (precision) of this histogram.
    pub fn layout(&self) -> BucketLayout {
        self.layout
    }

    /// Upper bound on the relative error of [`percentile`](Self::percentile).
    pub fn max_relative_error(&self) -> f64 {
        self.layout.max_relative_error()
    }

    /// Records one sample. O(1), no allocation.
    #[inline]
    pub fn record(&mut self, value: u64) {
        let i = self.layout.index(value);
        self.counts[i] += 1;
        self.count += 1;
        self.sum = self.sum.saturating_add(value);
        if value < self.min {
            self.min = value;
        }
        if value > self.max {
            self.max = value;
        }
    }

    /// Records `n` samples of the same value. O(1). `n == 0` is a no-op.
    #[inline]
    pub fn record_n(&mut self, value: u64, n: u64) {
        if n == 0 {
            return;
        }
        let i = self.layout.index(value);
        self.counts[i] += n;
        self.count += n;
        self.sum = self.sum.saturating_add(value.saturating_mul(n));
        self.min = self.min.min(value);
        self.max = self.max.max(value);
    }

    /// Adds every sample of `other` into `self`. O(buckets). The result is
    /// exactly what recording both sample streams into one histogram gives.
    ///
    /// # Panics
    ///
    /// If the two histograms have different precisions.
    pub fn merge(&mut self, other: &Histogram) {
        assert_eq!(
            self.layout, other.layout,
            "cannot merge histograms with different precision"
        );
        if other.count == 0 {
            return;
        }
        for (a, b) in self.counts.iter_mut().zip(other.counts.iter()) {
            *a += b;
        }
        self.count += other.count;
        self.sum = self.sum.saturating_add(other.sum);
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
    }

    /// The value at quantile `q` in `[0, 1]` (`0.99` is p99): the sample at
    /// index `⌊q·n⌋` of the sorted samples (clamped to `n − 1`), reported as
    /// its bucket's highest value clamped to `[min, max]`. Never below the
    /// true value, at most [`max_relative_error`](Self::max_relative_error)
    /// above it. `q` outside `[0, 1]` is clamped; an empty histogram
    /// returns 0. O(buckets).
    pub fn percentile(&self, q: f64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let q = if q.is_nan() { 0.0 } else { q.clamp(0.0, 1.0) };
        let rank = ((q * self.count as f64) as u64).min(self.count - 1);
        let mut seen = 0u64;
        for (i, &c) in self.counts.iter().enumerate() {
            seen += c;
            if seen > rank {
                return self.layout.highest(i).clamp(self.min, self.max);
            }
        }
        self.max
    }

    /// Number of recorded samples.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// True if nothing has been recorded.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Smallest recorded sample (exact), or 0 when empty.
    pub fn min(&self) -> u64 {
        if self.count == 0 {
            0
        } else {
            self.min
        }
    }

    /// Largest recorded sample (exact), or 0 when empty.
    pub fn max(&self) -> u64 {
        self.max
    }

    /// Sum of all recorded samples (exact, saturating).
    pub fn sum(&self) -> u64 {
        self.sum
    }

    /// Arithmetic mean (exact up to f64 rounding), or 0.0 when empty.
    pub fn mean(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.sum as f64 / self.count as f64
        }
    }

    /// Number of samples whose bucket starts at or below `value` — i.e.
    /// samples `<= value`, resolved to bucket precision (samples sharing
    /// `value`'s bucket count as `<=`). O(buckets).
    pub fn count_at_or_below(&self, value: u64) -> u64 {
        let last = self.layout.index(value);
        self.counts[..=last].iter().sum()
    }

    /// The non-empty buckets, lowest first. O(buckets); allocates the result.
    pub fn buckets(&self) -> Vec<Bucket> {
        self.counts
            .iter()
            .enumerate()
            .filter(|(_, &c)| c > 0)
            .map(|(i, &c)| Bucket {
                low: self.layout.lowest(i),
                high: self.layout.highest(i),
                count: c,
            })
            .collect()
    }

    /// Forgets every sample, keeping the allocation. O(buckets).
    pub fn reset(&mut self) {
        self.counts.fill(0);
        self.count = 0;
        self.sum = 0;
        self.min = u64::MAX;
        self.max = 0;
    }
}

// @student w19
impl Histogram {
    /// Rebuilds a histogram from raw parts: `counts[i]` samples in bucket `i`
    /// of `layout`, plus the exact `sum`, `min` and `max` tracked alongside.
    /// Used by code that keeps its own bucket storage — the per-thread
    /// atomic histograms of [`metrics`](crate::metrics) — to hand out a
    /// regular `Histogram` at collection time. O(buckets).
    ///
    /// `min`/`max` that disagree with the buckets (a concurrent reader can
    /// see a bucket increment before the matching min/max update, or vice
    /// versa) are replaced by the bounds of the lowest/highest non-empty
    /// bucket.
    ///
    /// # Panics
    ///
    /// If `counts.len() != layout.bucket_count()`.
    pub fn from_parts(
        layout: BucketLayout,
        counts: &[u64],
        sum: u64,
        min: u64,
        max: u64,
    ) -> Histogram {
        assert_eq!(
            counts.len(),
            layout.bucket_count(),
            "bucket array does not match the layout"
        );
        let mut h = Histogram {
            layout,
            counts: counts.to_vec().into_boxed_slice(),
            count: counts.iter().sum(),
            sum,
            min: u64::MAX,
            max: 0,
        };
        if h.count == 0 {
            h.sum = 0;
            return h;
        }
        let first = counts.iter().position(|&c| c > 0).unwrap_or(0);
        let last = counts.iter().rposition(|&c| c > 0).unwrap_or(0);
        h.min = if min <= layout.highest(first) {
            min
        } else {
            layout.lowest(first)
        };
        h.max = if max >= layout.lowest(last) {
            max
        } else {
            layout.highest(last)
        };
        h
    }
}

// @student w25
impl Histogram {
    /// Records `value` and corrects for coordinated omission, the way
    /// HdrHistogram's `recordValueWithExpectedInterval` does.
    ///
    /// A closed-loop tester that expects to send a request every
    /// `expected_interval` but was blocked for `value` never sent the
    /// requests that were due meanwhile — the stall is recorded once
    /// instead of once per victim. This back-fills those missing samples:
    /// `value − interval`, `value − 2·interval`, … down to `interval`.
    ///
    /// `expected_interval == 0` or `value <= expected_interval` records just
    /// `value`. Cost: O(value / expected_interval) records.
    pub fn record_corrected(&mut self, value: u64, expected_interval: u64) {
        self.record(value);
        if expected_interval == 0 || value <= expected_interval {
            return;
        }
        let mut missing = value - expected_interval;
        while missing >= expected_interval {
            self.record(missing);
            missing -= expected_interval;
        }
    }
}

impl Default for Histogram {
    fn default() -> Histogram {
        Histogram::new()
    }
}

impl fmt::Debug for Histogram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Histogram")
            .field("precision", &self.layout().precision())
            .field("count", &self.count())
            .field("min", &self.min())
            .field("p50", &self.percentile(0.5))
            .field("p99", &self.percentile(0.99))
            .field("p999", &self.percentile(0.999))
            .field("max", &self.max())
            .field("mean", &self.mean())
            .finish()
    }
}
