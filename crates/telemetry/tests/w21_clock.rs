//! Week 21: the calibrated clock and stage timestamps.

use std::time::{Duration, Instant};
use telemetry::{Clock, ClockSource, Stage, StageStamps};

/// Reads `clock` and `Instant` back to back, retrying until the pair is not
/// split by a preemption.
fn paired(clock: &Clock) -> (u64, Instant) {
    let mut best = (u64::MAX, 0, Instant::now());
    for _ in 0..64 {
        let a = clock.now();
        let i = Instant::now();
        let b = clock.now();
        if b - a < best.0 {
            best = (b - a, a + (b - a) / 2, i);
        }
    }
    (best.1, best.2)
}

#[test]
fn w21_now_is_monotonic() {
    for clock in [Clock::new(), Clock::instant()] {
        let mut prev = clock.now();
        for _ in 0..200_000 {
            let t = clock.now();
            assert!(
                t >= prev,
                "{:?} clock went backwards: {prev} -> {t}",
                clock.source()
            );
            prev = t;
        }
        // Copies share the epoch: a copy's reading continues the original's.
        let copy = clock;
        assert!(copy.now() >= prev);
    }
}

#[test]
fn w21_calibrated_clock_agrees_with_instant() {
    let clock = Clock::new();
    if Clock::has_invariant_tsc() {
        assert_eq!(clock.source(), ClockSource::Tsc);
        // EPYC/Xeon TSCs run between ~1 and ~5 GHz.
        let hz = clock.frequency_hz();
        assert!(
            (500_000_000..10_000_000_000).contains(&hz),
            "implausible TSC frequency {hz}"
        );
    } else {
        assert_eq!(clock.source(), ClockSource::Instant);
    }
    let (c0, i0) = paired(&clock);
    std::thread::sleep(Duration::from_millis(30));
    let (c1, i1) = paired(&clock);
    let by_clock = (c1 - c0) as f64;
    let by_instant = i1.duration_since(i0).as_nanos() as f64;
    let err = (by_clock - by_instant).abs() / by_instant;
    // Calibration is good to ~1e-5; allow 1%.
    assert!(
        err < 0.01,
        "clock measured {by_clock} ns, Instant {by_instant} ns (error {err:.5})"
    );
}

#[test]
fn w21_instant_fallback_is_a_nanosecond_clock() {
    let clock = Clock::instant();
    assert_eq!(clock.source(), ClockSource::Instant);
    assert_eq!(clock.frequency_hz(), 1_000_000_000);
    assert_eq!(clock.ticks_to_ns(12_345), 12_345);
    let t0 = clock.now();
    std::thread::sleep(Duration::from_millis(2));
    let t1 = clock.now();
    assert!(
        t1 - t0 >= 2_000_000,
        "slept 2 ms, clock advanced {} ns",
        t1 - t0
    );
    assert!(t1 - t0 < 2_000_000_000);
}

#[test]
fn w21_tick_conversion_is_exact_and_does_not_overflow() {
    let Some(clock) = Clock::tsc_at(3_000_000_000) else {
        assert!(!Clock::has_invariant_tsc());
        return; // no TSC on this target: nothing to convert
    };
    assert_eq!(clock.source(), ClockSource::Tsc);
    assert_eq!(clock.frequency_hz(), 3_000_000_000);
    assert_eq!(clock.ticks_to_ns(0), 0);
    assert_eq!(clock.ticks_to_ns(3), 1);
    assert_eq!(clock.ticks_to_ns(3_000), 1_000);
    let hour = 3_600u64 * 3_000_000_000;
    let ns = clock.ticks_to_ns(hour);
    assert!(
        ns.abs_diff(3_600_000_000_000) <= 1,
        "1 h of ticks -> {ns} ns"
    );
    // A full u64 of ticks converts without panicking or wrapping.
    assert!(clock.ticks_to_ns(u64::MAX) >= u64::MAX / 3 - 1);

    let slow = Clock::tsc_at(1_000).unwrap();
    assert_eq!(slow.ticks_to_ns(1), 1_000_000);
    assert_eq!(
        slow.ticks_to_ns(u64::MAX),
        u64::MAX,
        "saturates instead of wrapping"
    );
}

#[test]
fn w21_stage_stamps_record_the_path() {
    let mut s = StageStamps::new();
    assert!(s.is_empty());
    assert_eq!(s.total(), 0);
    assert!(s.stamp(Stage::Ingress, 1_000));
    assert_eq!(s.total(), 0, "one stamp has no duration");
    assert!(s.stamp(Stage::Gateway, 1_150));
    assert!(s.stamp(Stage::Risk, 1_200));
    assert!(s.stamp(Stage::Match, 1_500));
    assert!(s.stamp(Stage::Ack, 1_600));
    assert_eq!(s.len(), 5);
    assert_eq!(s.get(Stage::Risk), Some(1_200));
    assert_eq!(s.get(Stage::Journal), None);
    assert_eq!(
        s.interval(Stage::Ingress),
        None,
        "the first stamp starts the clock"
    );
    assert_eq!(s.interval(Stage::Gateway), Some(150));
    assert_eq!(s.interval(Stage::Match), Some(300));
    assert_eq!(s.interval(Stage::Ack), Some(100));
    assert_eq!(s.interval(Stage::Publish), None);
    assert_eq!(s.total(), 600);
    let path: Vec<(Stage, u64)> = s.iter().collect();
    assert_eq!(
        path,
        vec![
            (Stage::Ingress, 1_000),
            (Stage::Gateway, 1_150),
            (Stage::Risk, 1_200),
            (Stage::Match, 1_500),
            (Stage::Ack, 1_600)
        ]
    );
    assert_eq!(s.at(4), Some((Stage::Ack, 1_600)));
    assert_eq!(s.at(5), None);

    // Insertion order is the path taken, even if it differs from Stage::ALL.
    let mut acked_first = StageStamps::new();
    assert!(acked_first.stamp(Stage::Match, 10));
    assert!(acked_first.stamp(Stage::Ack, 20));
    assert!(acked_first.stamp(Stage::Publish, 45));
    assert_eq!(acked_first.interval(Stage::Publish), Some(25));
    assert_eq!(acked_first.total(), 35);

    // Stamps from another core may be a few ns behind: intervals saturate.
    let mut skewed = StageStamps::new();
    assert!(skewed.stamp(Stage::Ingress, 500));
    assert!(skewed.stamp(Stage::Gateway, 497));
    assert_eq!(skewed.interval(Stage::Gateway), Some(0));
    assert_eq!(skewed.total(), 0);
}

#[test]
fn w21_stage_stamps_reject_duplicates_and_stay_small() {
    let mut s = StageStamps::new();
    for (i, stage) in Stage::ALL.iter().enumerate() {
        assert!(s.stamp(*stage, i as u64 * 10));
    }
    assert_eq!(s.len(), Stage::COUNT);
    for stage in Stage::ALL {
        assert!(!s.stamp(stage, 999), "{stage:?} stamped twice");
    }
    assert_eq!(s.len(), Stage::COUNT);
    assert_eq!(s.total(), (Stage::COUNT as u64 - 1) * 10);

    // Copy, compared by value, one cache line: cheap to carry with an order.
    let copy = s;
    assert_eq!(copy, s);
    assert!(
        std::mem::size_of::<StageStamps>() <= 64,
        "StageStamps is {} bytes",
        std::mem::size_of::<StageStamps>()
    );

    s.clear();
    assert!(s.is_empty());
    assert!(s.stamp(Stage::Ack, 1));

    let clock = Clock::instant();
    let mut live = StageStamps::new();
    assert!(live.stamp_now(Stage::Ingress, &clock));
    assert!(live.stamp_now(Stage::Ack, &clock));
    assert!(live.get(Stage::Ack).unwrap() >= live.get(Stage::Ingress).unwrap());
    assert_eq!(
        Stage::ALL.map(|s| s.name()),
        ["ingress", "gateway", "risk", "journal", "match", "publish", "ack"]
    );
    assert_eq!(Stage::from_index(Stage::Match.index()), Some(Stage::Match));
}
