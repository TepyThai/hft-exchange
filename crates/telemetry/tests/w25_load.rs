//! Week 25: open-loop load, coordinated omission and its correction.

use telemetry::{Clock, ClosedLoop, Histogram, OpenLoop, SimTime, TimeSource};

#[test]
fn w25_record_corrected_backfills_missing_samples() {
    let mut h = Histogram::new();
    h.record_corrected(1_000, 100);
    // 1000 plus the omitted 900, 800, ..., 100.
    assert_eq!(h.count(), 10);
    assert_eq!(h.sum(), (1..=10).map(|k| k * 100).sum::<u64>());
    assert_eq!((h.min(), h.max()), (100, 1_000));

    // Not a multiple of the interval: 250 -> 250, 150 (50 < interval stops).
    let mut h = Histogram::new();
    h.record_corrected(250, 100);
    assert_eq!((h.count(), h.sum(), h.min()), (2, 400, 150));
}

#[test]
fn w25_record_corrected_is_a_plain_record_when_on_time() {
    let mut a = Histogram::new();
    let mut b = Histogram::new();
    for v in [10, 99, 100] {
        a.record_corrected(v, 100);
        b.record(v);
    }
    a.record_corrected(5_000, 0); // no interval: nothing to correct against
    b.record(5_000);
    assert!(a == b);
}

#[test]
fn w25_open_loop_measures_from_the_intended_start() {
    // Requests every 1000 ns, 200 ns service, request 2 stalls for 3500 ns.
    let schedule = OpenLoop::at_rate(1_000_000, 10);
    assert_eq!(schedule.interval_ns, 1_000);
    assert_eq!(schedule.intended_start(500, 3), 3_500);
    let mut time = SimTime::new(0);
    let report = schedule.run(&mut time, |t, i| {
        t.advance(if i == 2 { 3_500 } else { 200 })
    });

    // r2 starts on time (2000) and ends at 5500; r3..r6 queue behind it:
    // r3 3000->5700 (2700), r4 4000->5900 (1900), r5 5000->6100 (1100),
    // r6 6000 -> starts 6100, ends 6300 (300); the rest are on time (200).
    assert_eq!(report.requests, 10);
    assert_eq!(report.response.count(), 10);
    assert_eq!(report.response.max(), 3_500);
    assert_eq!(
        report.response.sum(),
        5 * 200 + 3_500 + 2_700 + 1_900 + 1_100 + 300
    );
    // A closed-loop view sees one slow request and nothing else.
    assert_eq!(report.service.sum(), 9 * 200 + 3_500);
    assert_eq!(report.service.percentile(0.8), 200);
    assert!(report.response.percentile(0.8) >= 1_900);
    assert_eq!(report.max_lag_ns, 2_500, "r3 went out 2500 ns late");
    assert_eq!(report.elapsed_ns, 9_200);
    assert!((report.offered_rate() - 1e6).abs() < 1e-6);
}

#[test]
fn w25_closed_loop_omits_the_samples_a_stall_hides() {
    let mut time = SimTime::new(0);
    let run = ClosedLoop::new(1_000, 10_000).run(&mut time, |t, i| {
        t.advance(if i == 5_000 { 1_000_000 } else { 200 })
    });
    // While waiting 1 ms for slot 5000, slots 5001..=5999 passed unsent.
    assert_eq!(run.omitted, 999);
    assert_eq!(run.sent, 10_000 - 999);
    assert_eq!(run.raw.count(), run.sent);
    assert_eq!(run.raw.max(), 1_000_000);
    // Correction puts the omitted samples back: one per missed slot.
    assert_eq!(run.corrected.count(), run.sent + 999);
}

#[test]
fn w25_stall_makes_corrected_p99_much_larger_than_uncorrected() {
    let stall = |t: &mut SimTime, i: u64| t.advance(if i == 5_000 { 1_000_000 } else { 200 });

    let closed = ClosedLoop::new(1_000, 10_000).run(&mut SimTime::new(0), stall);
    let open = OpenLoop::at_rate(1_000_000, 10_000).run(&mut SimTime::new(0), stall);

    let raw_p99 = closed.raw.percentile(0.99);
    let corrected_p99 = closed.corrected.percentile(0.99);
    let truth_p99 = open.response.percentile(0.99);
    // The uncorrected closed loop reports the service time, as if the 1 ms
    // stall had hurt one request.
    assert_eq!(raw_p99, 200);
    assert_eq!(open.service.percentile(0.99), 200);
    // Corrected, the p99 is ~0.9 ms: thousands of times larger...
    assert!(
        corrected_p99 > 1_000 * raw_p99,
        "corrected p99 {corrected_p99} vs raw {raw_p99}"
    );
    // ...and close to what the open-loop run measured directly.
    assert!(truth_p99 > 1_000 * raw_p99, "open-loop p99 {truth_p99}");
    let ratio = corrected_p99 as f64 / truth_p99 as f64;
    assert!(
        (0.8..1.25).contains(&ratio),
        "corrected {corrected_p99} vs open-loop {truth_p99}"
    );
    // The worst sample is the same in every view.
    assert_eq!(closed.raw.max(), 1_000_000);
    assert_eq!(closed.corrected.max(), 1_000_000);
    assert_eq!(open.response.max(), 1_000_000);
}

#[test]
fn w25_warmup_is_scheduled_but_not_recorded() {
    let schedule = OpenLoop::at_rate(500_000, 100).with_warmup(50);
    assert_eq!(schedule.warmup, 50);
    let mut seen = Vec::new();
    let mut time = SimTime::new(10_000);
    // A huge stall in the warmup that has fully drained by the time
    // measuring starts.
    let report = schedule.run(&mut time, |t, i| {
        seen.push(i);
        t.advance(if i == 0 { 20_000 } else { 100 });
    });
    assert_eq!(seen, (0..150).collect::<Vec<_>>());
    assert_eq!(report.requests, 100);
    assert_eq!((report.response.min(), report.response.max()), (100, 100));
    assert_eq!(report.elapsed_ns, 99 * 2_000 + 100);
}

#[test]
fn w25_open_loop_on_the_real_clock_keeps_the_schedule() {
    let mut clock = Clock::new();
    let start = clock.now();
    let report = OpenLoop::at_rate(1_000_000, 20_000).run(&mut clock, |c, _| {
        let until = c.now() + 100;
        c.wait_until(until);
    });
    let wall = clock.now() - start;
    assert_eq!(report.requests, 20_000);
    // Never early: 20k requests 1 µs apart take at least ~20 ms...
    assert!(
        wall >= 19_999_000,
        "finished in {wall} ns, ahead of schedule"
    );
    assert!(report.elapsed_ns >= 19_999_000);
    // ...and every response includes the 100 ns of service.
    assert!(report.response.min() >= 100 && report.service.min() >= 100);
    assert!(
        report.response.percentile(0.5) < 1_000_000,
        "median response {}",
        report.response.percentile(0.5)
    );
}
