//! Week 19: per-thread counters and histograms, collection, Prometheus text.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telemetry::{Histogram, Registry, LATENCY_BOUNDS_NS};
use types::Rng;

#[test]
fn w19_counters_sum_across_threads() {
    let registry = Registry::new();
    let threads: Vec<_> = (0..8u64)
        .map(|t| {
            let reg = registry.clone();
            std::thread::spawn(move || {
                let mut orders = reg.counter("orders_total", "Orders accepted.");
                let mut side = reg.counter_with_labels(
                    "fills_total",
                    "",
                    &[("side", if t % 2 == 0 { "buy" } else { "sell" })],
                );
                for _ in 0..10_000 {
                    orders.inc();
                }
                side.add(t + 1);
                assert_eq!(orders.get(), 10_000, "a handle reports its own total");
                (orders, side) // keep the handles alive until joined
            })
        })
        .collect();
    let handles: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    let snap = registry.collect();
    assert_eq!(snap.counter("orders_total", &[]), Some(80_000));
    assert_eq!(
        snap.counter("fills_total", &[("side", "buy")]),
        Some(1 + 3 + 5 + 7)
    );
    assert_eq!(
        snap.counter("fills_total", &[("side", "sell")]),
        Some(2 + 4 + 6 + 8)
    );
    assert_eq!(snap.counter("fills_total", &[]), None);
    assert_eq!(snap.counters.len(), 3);
    drop(handles);
}

#[test]
fn w19_histograms_merge_across_threads() {
    let registry = Registry::new();
    let values = |t: u64| {
        let mut rng = Rng::new(1900 + t);
        (0..20_000).map(move |_| {
            if rng.chance(0.01) {
                rng.range(100_000, 2_000_000)
            } else {
                rng.range(150, 900)
            }
        })
    };
    let threads: Vec<_> = (0..4u64)
        .map(|t| {
            let reg = registry.clone();
            std::thread::spawn(move || {
                let mut h = reg.histogram("order_latency_ns", "Order latency.", LATENCY_BOUNDS_NS);
                for v in values(t) {
                    h.record(v);
                }
                assert_eq!(h.count(), 20_000);
                h
            })
        })
        .collect();
    let handles: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();

    let mut expected = Histogram::new();
    for t in 0..4 {
        for v in values(t) {
            expected.record(v);
        }
    }
    let snap = registry.collect();
    let got = snap
        .histogram("order_latency_ns", &[])
        .expect("histogram collected");
    assert_eq!(got.count(), 80_000);
    assert_eq!(got.sum(), expected.sum());
    assert_eq!((got.min(), got.max()), (expected.min(), expected.max()));
    for q in [0.0, 0.5, 0.9, 0.99, 0.999, 1.0] {
        assert_eq!(got.percentile(q), expected.percentile(q), "q={q}");
    }
    drop(handles);
}

#[test]
fn w19_dropped_handles_keep_their_values() {
    let registry = Registry::new();
    {
        let mut c = registry.counter("sessions_total", "Sessions opened.");
        c.add(5);
        let mut h = registry.histogram("rtt_ns", "", &[1_000]);
        h.record(700);
        h.record(3_000);
    }
    let first = registry.collect();
    assert_eq!(first.counter("sessions_total", &[]), Some(5));
    // Collecting again must not count a retired handle twice.
    let second = registry.collect();
    assert_eq!(second.counter("sessions_total", &[]), Some(5));
    assert_eq!(second.histogram("rtt_ns", &[]).unwrap().count(), 2);

    let mut c = registry.counter("sessions_total", "Sessions opened.");
    c.add(2);
    let mut h = registry.histogram("rtt_ns", "", &[1_000]);
    h.record(10);
    let third = registry.collect();
    assert_eq!(third.counter("sessions_total", &[]), Some(7));
    let rtt = third.histogram("rtt_ns", &[]).unwrap();
    assert_eq!(
        (rtt.count(), rtt.min(), rtt.max(), rtt.sum()),
        (3, 10, 3_000, 3_710)
    );
}

#[test]
fn w19_prometheus_exposition_is_exact() {
    let registry = Registry::new();
    let mut buys =
        registry.counter_with_labels("orders_total", "Orders accepted.", &[("side", "buy")]);
    let mut sells =
        registry.counter_with_labels("orders_total", "Orders accepted.", &[("side", "sell")]);
    let mut errors = registry.counter("errors_total", "");
    buys.add(7);
    sells.add(3);
    errors.inc();
    let mut latency = registry.histogram(
        "latency_ns",
        "Order latency\\ns.\nIngress to ack.",
        &[10, 100, 1_000],
    );
    for v in [5, 10, 50, 100, 2_000] {
        latency.record(v);
    }
    let mut empty = registry.histogram("idle_ns", "Never recorded.", &[]);
    let _ = &mut empty;

    let text = registry.collect().to_prometheus();
    let expected = "\
# TYPE errors_total counter
errors_total 1
# HELP idle_ns Never recorded.
# TYPE idle_ns histogram
idle_ns_bucket{le=\"+Inf\"} 0
idle_ns_sum 0
idle_ns_count 0
# HELP latency_ns Order latency\\\\ns.\\nIngress to ack.
# TYPE latency_ns histogram
latency_ns_bucket{le=\"10\"} 2
latency_ns_bucket{le=\"100\"} 4
latency_ns_bucket{le=\"1000\"} 4
latency_ns_bucket{le=\"+Inf\"} 5
latency_ns_sum 2165
latency_ns_count 5
# HELP orders_total Orders accepted.
# TYPE orders_total counter
orders_total{side=\"buy\"} 7
orders_total{side=\"sell\"} 3
";
    assert_eq!(text, expected);
    assert_eq!(registry.render_prometheus(), expected);
}

#[test]
fn w19_prometheus_labels_are_sorted_and_escaped() {
    let registry = Registry::new();
    let mut risk = registry.histogram_with_labels(
        "stage_latency_ns",
        "Per-stage latency.",
        &[100, 1_000],
        &[("venue", "X\"1\\"), ("stage", "risk")],
    );
    let mut gateway = registry.histogram_with_labels(
        "stage_latency_ns",
        "Per-stage latency.",
        &[100, 1_000],
        &[("stage", "gateway")],
    );
    risk.record(50);
    risk.record(500);
    gateway.record(5_000);

    let snap = registry.collect();
    assert_eq!(
        snap.histogram(
            "stage_latency_ns",
            &[("venue", "X\"1\\"), ("stage", "risk")]
        )
        .unwrap()
        .count(),
        2
    );
    assert_eq!(
        snap.histogram(
            "stage_latency_ns",
            &[("stage", "risk"), ("venue", "X\"1\\")]
        )
        .unwrap()
        .count(),
        2
    );
    let expected = "\
# HELP stage_latency_ns Per-stage latency.
# TYPE stage_latency_ns histogram
stage_latency_ns_bucket{stage=\"gateway\",le=\"100\"} 0
stage_latency_ns_bucket{stage=\"gateway\",le=\"1000\"} 0
stage_latency_ns_bucket{stage=\"gateway\",le=\"+Inf\"} 1
stage_latency_ns_sum{stage=\"gateway\"} 5000
stage_latency_ns_count{stage=\"gateway\"} 1
stage_latency_ns_bucket{stage=\"risk\",venue=\"X\\\"1\\\\\",le=\"100\"} 1
stage_latency_ns_bucket{stage=\"risk\",venue=\"X\\\"1\\\\\",le=\"1000\"} 2
stage_latency_ns_bucket{stage=\"risk\",venue=\"X\\\"1\\\\\",le=\"+Inf\"} 2
stage_latency_ns_sum{stage=\"risk\",venue=\"X\\\"1\\\\\"} 550
stage_latency_ns_count{stage=\"risk\",venue=\"X\\\"1\\\\\"} 2
";
    assert_eq!(snap.to_prometheus(), expected);
}

#[test]
fn w19_collect_while_recording_is_monotonic_and_ends_exact() {
    let registry = Registry::new();
    let reg = registry.clone();
    let writer = std::thread::spawn(move || {
        let mut c = reg.counter("events_total", "");
        let mut h = reg.histogram("event_ns", "", LATENCY_BOUNDS_NS);
        for i in 0..2_000_000u64 {
            c.inc();
            h.record(i % 1_000);
        }
    });
    let mut last = 0;
    while !writer.is_finished() {
        let snap = registry.collect();
        let now = snap.counter("events_total", &[]).unwrap_or(0);
        assert!(now >= last, "counter went backwards: {last} -> {now}");
        last = now;
    }
    writer.join().unwrap();
    let snap = registry.collect();
    assert_eq!(snap.counter("events_total", &[]), Some(2_000_000));
    let h = snap.histogram("event_ns", &[]).unwrap();
    assert_eq!((h.count(), h.min(), h.max()), (2_000_000, 0, 999));
    assert_eq!(h.sum(), 2_000 * (999 * 1_000 / 2));
}

#[test]
fn w19_background_collector_publishes_snapshots() {
    let registry = Registry::new();
    let seen = Arc::new(Mutex::new(Vec::<u64>::new()));
    let sink = seen.clone();
    let collector = registry.spawn_collector(Duration::from_millis(1), move |snap| {
        sink.lock()
            .unwrap()
            .push(snap.counter("ticks_total", &[]).unwrap_or(0));
    });
    let mut ticks = registry.counter("ticks_total", "");
    ticks.add(10);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !seen.lock().unwrap().contains(&10) {
        assert!(
            Instant::now() < deadline,
            "collector never published the recorded value"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    ticks.add(5);
    collector.stop();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.last(), Some(&15), "stop() publishes a final snapshot");
    assert!(seen.windows(2).all(|w| w[0] <= w[1]));
}
