//! Benchmarks for the `telemetry` crate. Run with `cargo xtask bench --week N`.

use benchkit::{black_box, latency_batched, Report, Unit};
use telemetry::{
    Attribution, Clock, Histogram, Registry, Stage, StageStamps, TimeSource, LATENCY_BOUNDS_NS,
};
use types::Rng;

const W39_LABEL: &str = "Order in -> ack, in-process pipeline (synthetic 4 stages, ~600ns of work)";

fn main() {
    benchkit::run(&[
        (
            "w02.histogram_record",
            "Histogram::record cost",
            bench_histogram_record,
        ),
        (
            "w19.metric_overhead",
            "Hot-path counter + histogram record",
            bench_metric_overhead,
        ),
        ("w21.clock_now", "Clock::now cost", bench_clock_now),
        ("w39.tick_to_ack", W39_LABEL, bench_tick_to_ack),
    ]);
}

/// 4096 latency-like values: log-uniform from 16 ns to ~1 s, so `record`
/// touches buckets all over the array (as real latencies do) instead of one
/// hot cache line.
fn latency_values() -> Vec<u64> {
    let mut rng = Rng::new(2);
    (0..4096)
        .map(|_| {
            let bits = rng.range(4, 30);
            (1u64 << bits) | rng.range(0, (1u64 << bits) - 1)
        })
        .collect()
}

/// A benchkit report from one of our histograms (ns).
fn report(id: &str, label: &str, h: &Histogram) -> Report {
    Report {
        id: id.into(),
        label: label.into(),
        unit: Unit::Ns,
        value: h.percentile(0.5) as f64,
        p50: Some(h.percentile(0.5) as f64),
        p99: Some(h.percentile(0.99) as f64),
        p999: Some(h.percentile(0.999) as f64),
        max: Some(h.max() as f64),
        samples: h.count(),
    }
}

// --- Week 2 -----------------------------------------------------------------

fn bench_histogram_record() -> Report {
    let values = latency_values();
    let mut h = Histogram::new();
    let mut i = 0usize;
    let r = latency_batched(
        "w02.histogram_record",
        "Histogram::record cost",
        Unit::Ns,
        20_000,
        1_000,
        || {
            h.record(black_box(values[i & 4095]));
            i = i.wrapping_add(1);
        },
    );
    black_box(h.count());
    r
}

// --- Week 19 ----------------------------------------------------------------

fn bench_metric_overhead() -> Report {
    let values = latency_values();
    let registry = Registry::new();
    let mut orders = registry.counter("orders_total", "Orders accepted.");
    let mut latency = registry.histogram("order_latency_ns", "Order latency.", LATENCY_BOUNDS_NS);
    // A collector running alongside, as in production: it reads the cells
    // the hot path writes, without slowing it down.
    let collector = registry.spawn_collector(std::time::Duration::from_millis(10), |snap| {
        black_box(snap);
    });
    let mut i = 0usize;
    let r = latency_batched(
        "w19.metric_overhead",
        "Hot-path counter + histogram record",
        Unit::Ns,
        20_000,
        1_000,
        || {
            orders.inc();
            latency.record(black_box(values[i & 4095]));
            i = i.wrapping_add(1);
        },
    );
    collector.stop();
    black_box(registry.collect().counter("orders_total", &[]));
    r
}

// --- Week 21 ----------------------------------------------------------------

fn bench_clock_now() -> Report {
    let clock = Clock::new();
    eprintln!(
        "clock source: {:?}, {} Hz",
        clock.source(),
        clock.frequency_hz()
    );
    latency_batched(
        "w21.clock_now",
        "Clock::now cost",
        Unit::Ns,
        20_000,
        1_000,
        || {
            black_box(black_box(&clock).now());
        },
    )
}

// --- Week 25 ----------------------------------------------------------------
//
// `w25.engine_open_loop` measures the real engine under `OpenLoop`, so it
// lives in the engine crate's benches (telemetry cannot depend on the
// engine).

// --- Week 39 ----------------------------------------------------------------

/// Busy-waits `ns` on `clock`, then stamps `stage`.
#[inline(always)]
fn stage(clock: &mut Clock, stamps: &mut StageStamps, stage: Stage, ns: u64) {
    let until = clock.now() + ns;
    clock.wait_until(until);
    stamps.stamp_now(stage, clock);
}

/// A synthetic in-process order path with known stage costs, stamped with
/// the TSC clock: Ingress → Gateway (decode, 150 ns) → Risk (50 ns) →
/// Match (300 ns) → Ack (encode + write, 100 ns). End-to-end is 600 ns of
/// work plus ~30 ns of clock reads per stage (each busy-wait overshoots by
/// up to one read, then the stamp reads again) — ~730 ns on an EPYC 7543P.
/// The tail is OS jitter (interrupts, preemption).
fn bench_tick_to_ack() -> Report {
    let mut clock = Clock::new();
    let n = 300_000;
    let mut samples = Vec::with_capacity(n);
    for _ in 0..n / 10 {
        // warm-up
        let mut s = StageStamps::new();
        s.stamp_now(Stage::Ingress, &clock);
        stage(&mut clock, &mut s, Stage::Gateway, 150);
        black_box(s);
    }
    for _ in 0..n {
        let mut s = StageStamps::new();
        s.stamp_now(Stage::Ingress, &clock);
        stage(&mut clock, &mut s, Stage::Gateway, 150);
        stage(&mut clock, &mut s, Stage::Risk, 50);
        stage(&mut clock, &mut s, Stage::Match, 300);
        stage(&mut clock, &mut s, Stage::Ack, 100);
        samples.push(s);
    }
    let attribution = Attribution::from_samples(&samples);
    eprint!("{attribution}");
    report("w39.tick_to_ack", W39_LABEL, attribution.end_to_end())
}
