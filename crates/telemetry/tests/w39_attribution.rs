//! Week 39: per-stage latency attribution and budgets.

use telemetry::{Attribution, Budget, Stage, StageStamps, Violation};
use types::Rng;

/// Stamps for one order that spent `costs[k].1` ns in stage `costs[k].0`,
/// arriving at `t0`.
fn order(t0: u64, costs: &[(Stage, u64)]) -> StageStamps {
    let mut s = StageStamps::new();
    let mut t = t0;
    assert!(s.stamp(Stage::Ingress, t));
    for &(stage, cost) in costs {
        t += cost;
        assert!(s.stamp(stage, t));
    }
    s
}

/// Risk is always ~1.1 µs; Match is 100 ns except for rare (0.5%) 20–40 µs
/// spikes. Risk has the larger p99, Match owns the end-to-end tail.
fn spiky_match(n: usize) -> Vec<StageStamps> {
    let mut rng = Rng::new(39);
    (0..n)
        .map(|i| {
            let risk = rng.range(1_000, 1_200);
            let matching = if rng.chance(0.005) {
                rng.range(20_000, 40_000)
            } else {
                100
            };
            order(
                i as u64 * 10_000,
                &[
                    (Stage::Gateway, 150),
                    (Stage::Risk, risk),
                    (Stage::Match, matching),
                    (Stage::Ack, 100),
                ],
            )
        })
        .collect()
}

#[test]
fn w39_per_stage_intervals_are_exact() {
    let samples: Vec<StageStamps> = (0..1_000u64)
        .map(|i| {
            order(
                i * 1_000,
                &[
                    (Stage::Gateway, 150),
                    (Stage::Risk, 50 + i % 10),
                    (Stage::Match, 300),
                    (Stage::Ack, 100),
                ],
            )
        })
        .collect();
    let a = Attribution::from_samples(&samples);
    assert_eq!(a.samples(), 1_000);
    assert!(
        a.stage(Stage::Ingress).is_none(),
        "the first stamp has no interval"
    );
    assert!(a.stage(Stage::Publish).is_none());
    let risk = a.stage(Stage::Risk).unwrap();
    assert_eq!((risk.count(), risk.min(), risk.max()), (1_000, 50, 59));
    assert_eq!(risk.sum(), 1_000 * 50 + 100 * 45);
    for (stage, cost) in [
        (Stage::Gateway, 150),
        (Stage::Match, 300),
        (Stage::Ack, 100),
    ] {
        let h = a.stage(stage).unwrap();
        assert_eq!(
            (h.count(), h.min(), h.max(), h.percentile(0.99)),
            (1_000, cost, cost, cost),
            "{stage:?}"
        );
    }
    let e2e = a.end_to_end();
    assert_eq!((e2e.min(), e2e.max(), e2e.count()), (600, 609, 1_000));

    let rows = a.report();
    assert_eq!(
        rows.iter().map(|r| r.stage).collect::<Vec<_>>(),
        vec![Stage::Gateway, Stage::Risk, Stage::Match, Stage::Ack]
    );
    assert_eq!(
        (
            rows[2].p50_ns,
            rows[2].p99_ns,
            rows[2].max_ns,
            rows[2].count
        ),
        (300, 300, 300, 1_000)
    );
}

#[test]
fn w39_dominant_stage_is_the_one_that_owns_the_tail() {
    let a = Attribution::from_samples(&spiky_match(20_000));
    let risk_p99 = a.stage(Stage::Risk).unwrap().percentile(0.99);
    let match_p99 = a.stage(Stage::Match).unwrap().percentile(0.99);
    // Judged stage by stage, Risk looks like the problem...
    assert!(
        risk_p99 > match_p99,
        "risk p99 {risk_p99}, match p99 {match_p99}"
    );
    assert_eq!(match_p99, 100);
    // ...but the orders that make up the end-to-end p99 are slow because of Match.
    assert_eq!(a.dominant_stage(), Some(Stage::Match));
    assert!(
        a.tail_share(Stage::Match) > 0.6,
        "match share {}",
        a.tail_share(Stage::Match)
    );
    assert!(a.tail_share(Stage::Risk) < 0.3);
    assert!(a.tail_mean_ns(Stage::Match) > 5.0 * a.tail_mean_ns(Stage::Risk));
    // Only ~0.5% of orders spike, so the end-to-end p99 itself still looks
    // healthy; the spikes sit just above it and fill half the tail.
    assert!(a.end_to_end().percentile(0.99) < 2_000);
    assert!(a.end_to_end().percentile(0.999) > 10_000);
    let text = a.to_string();
    assert!(
        text.lines()
            .any(|l| l.starts_with("match") && l.contains("dominates p99")),
        "{text}"
    );
}

#[test]
fn w39_budgets_report_violations_in_order() {
    let a = Attribution::from_samples(&spiky_match(20_000));
    let budgets = [
        Budget {
            stage: Stage::Gateway,
            p99_ns: 200,
        },
        Budget {
            stage: Stage::Risk,
            p99_ns: 500,
        },
        Budget {
            stage: Stage::Match,
            p99_ns: 1_000,
        },
        Budget {
            stage: Stage::Publish,
            p99_ns: 10,
        }, // never stamped: skipped
        Budget {
            stage: Stage::Ack,
            p99_ns: 50,
        },
    ];
    let v = a.check(&budgets);
    assert_eq!(v.len(), 2, "{v:?}");
    assert_eq!((v[0].stage, v[0].budget_ns), (Stage::Risk, 500));
    assert!(
        (1_150..=1_210).contains(&v[0].p99_ns),
        "risk p99 {}",
        v[0].p99_ns
    );
    assert_eq!(
        v[1],
        Violation {
            stage: Stage::Ack,
            budget_ns: 50,
            p99_ns: 100
        }
    );
    assert!(a
        .check(&[Budget {
            stage: Stage::Risk,
            p99_ns: 2_000
        }])
        .is_empty());
}

#[test]
fn w39_missing_stages_are_measured_from_the_previous_stamp() {
    let accepted = order(
        0,
        &[
            (Stage::Gateway, 100),
            (Stage::Risk, 50),
            (Stage::Match, 300),
            (Stage::Ack, 100),
        ],
    );
    let rejected = order(
        0,
        &[(Stage::Gateway, 100), (Stage::Risk, 50), (Stage::Ack, 250)],
    );
    let mut lonely = StageStamps::new();
    assert!(lonely.stamp(Stage::Ingress, 5));
    let a = Attribution::from_samples(&[accepted, rejected, lonely, StageStamps::new()]);
    assert_eq!(a.samples(), 2, "samples with < 2 stamps are ignored");
    assert_eq!(a.stage(Stage::Match).unwrap().count(), 1);
    let ack = a.stage(Stage::Ack).unwrap();
    assert_eq!(
        (ack.count(), ack.min(), ack.max()),
        (2, 100, 250),
        "reject: Risk -> Ack charged to Ack"
    );
    assert_eq!((a.end_to_end().min(), a.end_to_end().max()), (400, 550));
}

#[test]
fn w39_empty_input_attributes_nothing() {
    let a = Attribution::from_samples(&[]);
    assert_eq!(a.samples(), 0);
    assert!(a.end_to_end().is_empty());
    assert_eq!(a.dominant_stage(), None);
    assert_eq!(a.tail_count(), 0);
    assert!(a.report().is_empty());
    assert!(a
        .check(&[Budget {
            stage: Stage::Match,
            p99_ns: 1
        }])
        .is_empty());
    for stage in Stage::ALL {
        assert!(a.stage(stage).is_none());
        assert_eq!(a.tail_share(stage), 0.0);
    }
}

#[test]
fn w39_tail_shares_account_for_all_tail_time() {
    let mut rng = Rng::new(3939);
    let samples: Vec<StageStamps> = (0..10_000)
        .map(|i| {
            let costs = [
                (Stage::Gateway, rng.range(50, 400)),
                (Stage::Risk, rng.range(20, 200)),
                (
                    Stage::Journal,
                    if rng.chance(0.02) {
                        rng.range(2_000, 9_000)
                    } else {
                        rng.range(80, 120)
                    },
                ),
                (Stage::Match, rng.range(100, 900)),
                (Stage::Publish, rng.range(30, 60)),
            ];
            order(i * 100_000, &costs)
        })
        .collect();
    let a = Attribution::with_tail(&samples, 0.95);
    assert_eq!(a.tail_quantile(), 0.95);

    // The tail is exactly the samples at or above the exact p95.
    let mut totals: Vec<u64> = samples.iter().map(|s| s.total()).collect();
    totals.sort_unstable();
    let threshold = totals[9_500];
    assert_eq!(a.tail_threshold_ns(), threshold);
    let tail: Vec<&StageStamps> = samples.iter().filter(|s| s.total() >= threshold).collect();
    assert_eq!(a.tail_count(), tail.len() as u64);

    // Shares sum to 1 and the per-stage tail means add up to the tail's
    // mean end-to-end latency (stamps are monotonic, so e2e = Σ intervals).
    let shares: f64 = Stage::ALL.iter().map(|&s| a.tail_share(s)).sum();
    assert!((shares - 1.0).abs() < 1e-9, "shares sum to {shares}");
    let tail_mean = tail.iter().map(|s| s.total()).sum::<u64>() as f64 / tail.len() as f64;
    let by_stage: f64 = Stage::ALL.iter().map(|&s| a.tail_mean_ns(s)).sum();
    assert!((by_stage - tail_mean).abs() < 1e-6 * tail_mean);
    let journal_tail = tail
        .iter()
        .map(|s| s.interval(Stage::Journal).unwrap())
        .sum::<u64>() as f64
        / tail.len() as f64;
    assert!((a.tail_mean_ns(Stage::Journal) - journal_tail).abs() < 1e-6);
    assert_eq!(a.dominant_stage(), Some(Stage::Journal));
}
