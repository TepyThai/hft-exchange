//! Week 43 — rolling statistics and a pairs trade on two cointegrated
//! instruments, each on its own engine.

use sim::pairs::{PairConfig, PairMarket, PairsConfig, PairsTrader, RollingStats, X, Y};
use types::Rng;

/// Mean and population variance by a fresh two-pass computation.
fn naive(values: &[f64]) -> (f64, f64) {
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;
    let var = values.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n;
    (mean, var)
}

#[test]
fn w43_rolling_stats_match_a_naive_recompute() {
    let mut rng = Rng::new(43);
    // Prices: a large level, a random walk, small noise and rare jumps —
    // the case where Σx² − n·mean² would lose most of its digits.
    let mut level = 10_000.0;
    let series: Vec<f64> = (0..20_000)
        .map(|_| {
            level += (rng.range(0, 200) as f64 - 100.0) / 100.0;
            if rng.chance(0.001) {
                level += 500.0;
            }
            level + (rng.range(0, 10) as f64) / 7.0
        })
        .collect();
    for window in [1, 2, 3, 17, 250] {
        let mut stats = RollingStats::new(window);
        assert_eq!(stats.window(), window);
        for (i, &x) in series.iter().enumerate() {
            stats.push(x);
            let seen = &series[(i + 1).saturating_sub(window)..=i];
            assert_eq!(stats.len(), seen.len());
            assert_eq!(stats.is_full(), seen.len() == window);
            let (mean, var) = naive(seen);
            let (got_mean, got_var) = (stats.mean().unwrap(), stats.variance().unwrap());
            assert!(
                (got_mean - mean).abs() <= 1e-9 * mean.abs(),
                "w {window} i {i}: {got_mean} vs {mean}"
            );
            // Re-anchored Welford stays within ~1e-8 here; without the
            // periodic exact recompute, rounding drift passes 1e-6.
            assert!(
                (got_var - var).abs() <= 1e-7 + 1e-12 * var,
                "w {window} i {i}: {got_var} vs {var}"
            );
            assert!(got_var >= 0.0);
            if let Some(z) = stats.zscore(x + 1.0) {
                assert!((z - (x + 1.0 - got_mean) / got_var.sqrt()).abs() < 1e-9);
            }
        }
    }
}

#[test]
fn w43_rolling_stats_edge_cases() {
    let mut s = RollingStats::new(3);
    assert!(s.is_empty());
    assert_eq!(
        (s.mean(), s.variance(), s.std_dev(), s.zscore(1.0)),
        (None, None, None, None)
    );
    s.push(1.0);
    assert_eq!(
        (s.mean(), s.variance(), s.zscore(5.0)),
        (Some(1.0), Some(0.0), None)
    );
    for x in [2.0, 3.0, 4.0] {
        s.push(x);
    }
    // The window is now [2, 3, 4]: exact in binary floating point.
    assert_eq!(
        (s.len(), s.mean(), s.variance()),
        (3, Some(3.0), Some(2.0 / 3.0))
    );
    assert_eq!(s.zscore(3.0), Some(0.0));

    let mut one = RollingStats::new(1);
    for x in [5.0, -7.5, 1e9] {
        one.push(x);
        assert_eq!(
            (one.mean(), one.variance(), one.zscore(0.0)),
            (Some(x), Some(0.0), None)
        );
    }
    let mut flat = RollingStats::new(10);
    for _ in 0..25 {
        flat.push(10_000.25);
    }
    assert_eq!(
        (flat.mean(), flat.variance(), flat.zscore(10_001.0)),
        (Some(10_000.25), Some(0.0), None)
    );
    assert!(std::panic::catch_unwind(|| RollingStats::new(0)).is_err());
}

#[test]
fn w43_signals_fire_at_the_thresholds() {
    let cfg = PairsConfig {
        entry_z: 2.0,
        exit_z: 0.5,
        step_z: 1.0,
        max_units: 3,
        ..PairsConfig::default()
    };
    let t = PairsTrader::new(cfg);
    let cases = [
        // (z, current) → target
        ((1.99, 0), 0),
        ((2.0, 0), -1),
        ((-2.0, 0), 1),
        ((2.99, 0), -1),
        ((3.0, 0), -2),
        ((4.0, 0), -3),
        ((9.0, 0), -3), // the position limit
        // Short the spread: hold, scale in, close, flip.
        ((2.5, -2), -2),
        ((4.5, -2), -3),
        ((1.0, -2), -2),
        ((0.51, -2), -2),
        ((0.5, -2), 0),
        ((-1.99, -1), 0),
        ((-2.0, -1), 1),
        ((-3.5, -3), 2),
        // Long the spread: the mirror image.
        ((-0.51, 1), 1),
        ((-0.5, 1), 0),
        ((-3.0, 1), 2),
        ((-2.5, 2), 2),
        ((1.0, 2), 0),
        ((2.0, 2), -1),
    ];
    for ((z, current), want) in cases {
        assert_eq!(t.target_units(z, current), want, "z {z} current {current}");
    }
    let single = PairsTrader::new(PairsConfig { step_z: 0.0, ..cfg });
    assert_eq!(single.target_units(7.0, 0), -1);

    // Through on_prices: no signal until the window is full, then z against
    // the previous window only (no look-ahead).
    let mut t = PairsTrader::new(PairsConfig {
        hedge_ratio: 0.5,
        window: 4,
        ..cfg
    });
    assert_eq!(t.spread(100.0, 60.0), 10.0);
    let mut seen = Vec::new();
    for (px, py) in [(100.0, 60.0), (100.0, 61.0), (102.0, 61.0), (100.0, 61.0)] {
        assert_eq!(t.on_prices(px, py), None);
        seen.push(t.spread(px, py));
    }
    assert_eq!(t.units(), 0);
    let (mean, var) = naive(&seen);
    // Spreads [10, 11, 10, 11]: mean 10.5, std 0.5; 12.25 is z = 3.5.
    let z = t.on_prices(100.0, 62.25).unwrap();
    assert!(
        (z - (12.25 - mean) / var.sqrt()).abs() < 1e-9 && (z - 3.5).abs() < 1e-9,
        "z {z}"
    );
    assert_eq!(t.units(), -2);
    // Short 2 units of 2 lots: sell 4 Y, buy round(4 × 0.5) = 2 X.
    assert_eq!(t.desired_legs(), [2, -4]);
    assert_eq!(t.last_z(), Some(z));
}

#[test]
fn w43_leg_positions_never_exceed_the_limit() {
    let cfg = PairsConfig {
        max_units: 2,
        unit_qty: 3,
        window: 30,
        ..PairsConfig::default()
    };
    // A wild spread keeps the trader at its limit, flipping often.
    let market = PairConfig {
        sigma_s: 20.0,
        phi: 0.9,
        level_qty: 4,
        ..PairConfig::default()
    };
    for seed in [1, 2] {
        let mut m = PairMarket::new(market, seed);
        let mut t = PairsTrader::new(cfg);
        let (mut max_x, mut max_y, mut entries) = (0, 0, 0);
        let mut prev = 0;
        for step in 0..3_000 {
            m.step();
            t.step(&mut m);
            let (x, y) = (t.leg(X).position, t.leg(Y).position);
            // 2 units × 3 lots of Y; X hedges round(6 × 1.5) = 9 lots.
            assert!(
                t.units().abs() <= 2 && y.abs() <= 6 && x.abs() <= 9,
                "seed {seed} step {step}"
            );
            assert_eq!(t.desired_legs()[Y], 3 * t.units() as i64);
            max_x = max_x.max(x.abs());
            max_y = max_y.max(y.abs());
            if prev == 0 && t.units() != 0 {
                entries += 1;
            }
            prev = t.units();
        }
        assert_eq!((max_x, max_y), (9, 6), "seed {seed}: the limit binds");
        assert!(entries > 10, "seed {seed}: {entries} trades");
    }
}

#[test]
fn w43_profitable_on_a_mean_reverting_spread() {
    for seed in [1, 2, 3] {
        let run = |phi: f64| {
            let mut m = PairMarket::new(
                PairConfig {
                    phi,
                    ..PairConfig::default()
                },
                seed,
            );
            let mut t = PairsTrader::new(PairsConfig::default());
            let (mut entries, mut prev) = (0, 0);
            for _ in 0..5_000 {
                m.step();
                t.step(&mut m);
                if prev == 0 && t.units() != 0 {
                    entries += 1;
                }
                prev = t.units();
            }
            (t.pnl([m.fair(X), m.fair(Y)]), entries)
        };
        let (pnl, entries) = run(0.95);
        assert!(entries >= 20, "seed {seed}: {entries} trades");
        assert!(pnl > 0.0, "seed {seed}: pnl {pnl}");
        // The same strategy on a spread that does not mean-revert (a random
        // walk, phi = 1): the edge was the cointegration.
        let (control, _) = run(1.0);
        assert!(control < pnl, "seed {seed}: {control} vs {pnl}");
    }
}
