//! Week 2: the log-linear latency histogram.

use telemetry::{BucketLayout, Histogram};
use types::Rng;

/// The exact value `Histogram::percentile` promises to approximate: the
/// sample at index ⌊q·n⌋ of the sorted samples (clamped to n − 1).
fn exact_percentile(sorted: &[u64], q: f64) -> u64 {
    let rank = ((q * sorted.len() as f64) as usize).min(sorted.len() - 1);
    sorted[rank]
}

/// `got` is never below `want` and at most `rel` above it.
fn assert_within(got: u64, want: u64, rel: f64, what: &str) {
    assert!(got >= want, "{what}: got {got} below the true value {want}");
    let err = (got - want) as f64 / want.max(1) as f64;
    assert!(
        err <= rel,
        "{what}: got {got}, true {want}, relative error {err:.5} > {rel}"
    );
}

const QUANTILES: [f64; 9] = [0.0, 0.1, 0.25, 0.5, 0.9, 0.99, 0.999, 0.9999, 1.0];

#[test]
fn w02_empty_histogram_reports_zero() {
    let h = Histogram::new();
    assert!(h.is_empty());
    assert_eq!(h.count(), 0);
    assert_eq!(h.percentile(0.5), 0);
    assert_eq!(h.percentile(0.99), 0);
    assert_eq!((h.min(), h.max(), h.sum()), (0, 0, 0));
    assert_eq!(h.mean(), 0.0);
    assert!(
        h.max_relative_error() < 0.01,
        "default precision must be better than 1%"
    );
}

#[test]
fn w02_single_and_small_values_are_exact() {
    let mut h = Histogram::new();
    h.record(123_456_789);
    for q in QUANTILES {
        assert_eq!(
            h.percentile(q),
            123_456_789,
            "one sample: every percentile is that sample (q={q})"
        );
    }
    assert_eq!(
        (h.min(), h.max(), h.mean()),
        (123_456_789, 123_456_789, 123_456_789.0)
    );

    // Below 2^(precision+1) every value has its own bucket.
    let mut h = Histogram::new();
    let values: Vec<u64> = (0..256).collect();
    for &v in values.iter().rev() {
        h.record(v);
    }
    for q in QUANTILES {
        assert_eq!(h.percentile(q), exact_percentile(&values, q), "q={q}");
    }
    assert_eq!(h.sum(), 255 * 256 / 2);
}

#[test]
fn w02_percentiles_match_sorted_samples_within_error() {
    let mut rng = Rng::new(2);
    for precision in [3, 5, 7, 10] {
        for dist in 0..3 {
            let mut h = Histogram::with_precision(precision);
            let mut samples = Vec::new();
            for _ in 0..50_000 {
                let v = match dist {
                    // Uniform over a latency-like range.
                    0 => rng.range(100, 100_000),
                    // Log-uniform from 1 ns to ~1 s.
                    1 => {
                        let bits = rng.range(0, 30);
                        (1u64 << bits) | rng.range(0, (1u64 << bits) - 1)
                    }
                    // Mostly fast, with a heavy tail.
                    _ => {
                        if rng.chance(0.02) {
                            rng.range(50_000, 5_000_000)
                        } else {
                            rng.range(200, 400)
                        }
                    }
                };
                h.record(v);
                samples.push(v);
            }
            samples.sort_unstable();
            let rel = h.max_relative_error();
            assert!(rel <= 1.0 / (1u64 << precision) as f64);
            for q in QUANTILES {
                let what = format!("precision {precision}, distribution {dist}, q={q}");
                assert_within(h.percentile(q), exact_percentile(&samples, q), rel, &what);
            }
            assert_eq!(h.count(), samples.len() as u64);
            assert_eq!(h.sum(), samples.iter().sum::<u64>());
            assert_eq!(h.min(), samples[0]);
            assert_eq!(h.max(), *samples.last().unwrap());
        }
    }
}

#[test]
fn w02_tail_is_not_hidden_by_the_mode() {
    // Same shape as the week 2 drill: 99% fast, 1% slow. p99 must land in
    // the slow mode and p50 in the fast one.
    let mut h = Histogram::new();
    for _ in 0..9_900 {
        h.record(100);
    }
    for _ in 0..100 {
        h.record(1_000_000);
    }
    assert_eq!(h.percentile(0.5), 100);
    assert_within(h.percentile(0.99), 1_000_000, h.max_relative_error(), "p99");
    assert_eq!(h.percentile(0.98), 100);
    assert!((h.mean() - 10_099.0).abs() < 1e-6);
}

#[test]
fn w02_merge_equals_recording_everything() {
    let mut rng = Rng::new(22);
    let mut all = Histogram::new();
    let mut parts = [Histogram::new(), Histogram::new(), Histogram::new()];
    for i in 0..30_000u64 {
        let magnitude = rng.range(1, 40);
        let v = rng.range(1, 1 << magnitude);
        all.record(v);
        parts[(i % 3) as usize].record(v);
    }
    let mut merged = Histogram::new();
    for p in &parts {
        merged.merge(p);
    }
    merged.merge(&Histogram::new()); // merging an empty histogram is a no-op
    assert_eq!(merged.count(), all.count());
    assert_eq!(merged.sum(), all.sum());
    assert_eq!((merged.min(), merged.max()), (all.min(), all.max()));
    for q in QUANTILES {
        assert_eq!(merged.percentile(q), all.percentile(q), "q={q}");
    }
    assert_eq!(merged.buckets(), all.buckets());
    assert!(merged == all);
}

#[test]
fn w02_record_n_and_reset() {
    let mut a = Histogram::new();
    let mut b = Histogram::new();
    for _ in 0..1_000 {
        a.record(5_000);
    }
    a.record(7);
    b.record_n(5_000, 1_000);
    b.record_n(7, 1);
    b.record_n(1_000_000, 0); // n = 0 records nothing
    assert!(a == b);
    assert_eq!(b.count(), 1_001);
    assert_eq!(b.max(), 5_000);
    assert_eq!(b.min(), 7);

    b.reset();
    assert!(b.is_empty());
    assert_eq!(
        (b.percentile(0.99), b.min(), b.max(), b.sum()),
        (0, 0, 0, 0)
    );
    assert!(b.buckets().is_empty());
    b.record(42);
    assert_eq!((b.min(), b.max(), b.percentile(0.5)), (42, 42, 42));
}

#[test]
fn w02_extreme_values_and_bucket_layout() {
    let mut h = Histogram::new();
    h.record(0);
    h.record(u64::MAX);
    assert_eq!(h.percentile(0.0), 0);
    assert_eq!(h.percentile(1.0), u64::MAX);
    assert_eq!(h.max(), u64::MAX);
    assert_eq!(h.sum(), u64::MAX, "the sum saturates instead of wrapping");
    assert_eq!(h.count_at_or_below(0), 1);
    assert_eq!(h.count_at_or_below(u64::MAX), 2);

    // Buckets tile the whole u64 range without gaps, and every value maps
    // to a bucket whose bounds contain it and whose width respects the
    // precision.
    for precision in [1, 4, 7] {
        let layout = BucketLayout::new(precision);
        assert_eq!(layout.index(u64::MAX), layout.bucket_count() - 1);
        assert_eq!(layout.highest(layout.bucket_count() - 1), u64::MAX);
        for i in 1..layout.bucket_count() {
            assert_eq!(
                layout.lowest(i),
                layout.highest(i - 1) + 1,
                "gap before bucket {i}"
            );
        }
        let mut rng = Rng::new(precision as u64);
        for _ in 0..10_000 {
            let v = rng.next_u64() >> rng.range(0, 63);
            let i = layout.index(v);
            assert!(layout.lowest(i) <= v && v <= layout.highest(i));
            let width = layout.highest(i) - layout.lowest(i);
            assert!(width as f64 <= v as f64 * layout.max_relative_error());
        }
    }
}
