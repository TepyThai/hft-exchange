//! Benchmarks for the `telemetry` crate. Run with `cargo xtask bench --week N`.

use benchkit::{black_box, latency_batched, Report, Unit};
use telemetry::{Clock, Histogram, Registry, LATENCY_BOUNDS_NS};
use types::Rng;

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
//
// `w39.tick_to_ack` measures exchange v2's real order path (decode, risk,
// journal, match, market data, release) in process, so it lives in the
// gateway crate's benches (telemetry cannot depend on the gateway).
