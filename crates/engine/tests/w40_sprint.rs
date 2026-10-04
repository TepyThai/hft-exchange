//! Week 40: the performance sprint — the fastest configuration stays
//! correct, and the regression check between two benchmark reports.

mod common;

use common::{allocations_in, mixed_stream};
use engine::perf::{fastest_engine, parse_benches, regressions, Metric, Regression};
use engine::testkit::{differential, FlowConfig, OrderFlow};
use engine::{Engine, EngineConfig};
use types::InstrumentSpec;

/// A report as `cargo xtask report` writes it.
fn report(benches: &[&str]) -> String {
    format!(
        concat!(
            r#"{{"schema":1,"tool":"hft-exchange xtask 0.1.0","source":"local","createdAt":1,"#,
            r#""machine":{{"cpu":"x","cores":4}},"tests":[{{"week":40,"passed":7,"failed":0,"total":7,"failures":[]}}],"#,
            r#""benches":[{}]}}"#
        ),
        benches.join(",")
    )
}

fn latency(id: &str, p50: f64, p99: f64) -> String {
    format!(
        r#"{{"week":40,"id":"{id}","label":"x \"quoted\"","unit":"ns","value":{p50},"p50":{p50},"p99":{p99},"p999":{},"max":1e5,"samples":1000000}}"#,
        p99 * 2.0
    )
}

fn rate(id: &str, unit: &str, v: f64) -> String {
    format!(
        r#"{{"week":40,"id":"{id}","label":"y","unit":"{unit}","value":{v},"p50":null,"p99":null,"p999":null,"max":null,"samples":20}}"#
    )
}

#[test]
fn w40_identical_reports_have_no_regressions() {
    let r = report(&[
        &latency("w40.match_latency", 70.0, 240.0),
        &rate("w40.engine_throughput", "ops_per_sec", 2.0e7),
        &rate("w28.speedup", "ratio", 1.4),
    ]);
    assert_eq!(regressions(&r, &r, 0.0), Ok(vec![]));
    let empty = report(&[]);
    assert_eq!(
        regressions(&empty, &r, 0.05),
        Ok(vec![]),
        "new benches are not regressions"
    );
}

#[test]
fn w40_latency_regressions_respect_direction_and_tolerance() {
    let old = report(&[&latency("w07.match_latency", 80.0, 300.0)]);
    // p99 20% slower, p50 faster: only p99 is reported.
    let slower = report(&[&latency("w07.match_latency", 70.0, 360.0)]);
    let found = regressions(&old, &slower, 0.10).unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    match &found[0] {
        Regression::Worse {
            id,
            metric,
            old,
            new,
            by,
        } => {
            assert_eq!((id.as_str(), *metric), ("w07.match_latency", Metric::P99));
            assert_eq!((*old, *new), (300.0, 360.0));
            assert!((by - 0.2).abs() < 1e-9);
        }
        other => panic!("{other:?}"),
    }
    // Within tolerance: nothing. Above a looser tolerance: nothing either.
    let noisy = report(&[&latency("w07.match_latency", 83.0, 315.0)]);
    assert_eq!(regressions(&old, &noisy, 0.05), Ok(vec![]));
    assert_eq!(regressions(&old, &slower, 0.25), Ok(vec![]));
    // Both percentiles worse: both reported, p50 first.
    let both = report(&[&latency("w07.match_latency", 100.0, 400.0)]);
    let metrics: Vec<Metric> = regressions(&old, &both, 0.10)
        .unwrap()
        .iter()
        .map(|r| match r {
            Regression::Worse { metric, .. } => *metric,
            Regression::Missing { .. } => panic!("not missing"),
        })
        .collect();
    assert_eq!(metrics, vec![Metric::P50, Metric::P99]);
}

#[test]
fn w40_rates_and_ratios_regress_when_they_drop() {
    let old = report(&[
        &rate("w40.engine_throughput", "ops_per_sec", 2.0e7),
        &rate("w28.speedup", "ratio", 1.5),
        &rate("w11.frame", "bytes", 50.0),
    ]);
    let new = report(&[
        &rate("w40.engine_throughput", "ops_per_sec", 1.5e7),
        &rate("w28.speedup", "ratio", 1.8),
        &rate("w11.frame", "bytes", 64.0),
    ]);
    let found = regressions(&old, &new, 0.05).unwrap();
    let ids: Vec<(&str, f64)> = found
        .iter()
        .map(|r| match r {
            Regression::Worse { id, by, .. } => (id.as_str(), *by),
            Regression::Missing { id } => (id.as_str(), f64::NAN),
        })
        .collect();
    assert_eq!(ids.len(), 2, "{found:?}");
    assert_eq!(
        ids[0].0, "w11.frame",
        "sorted by id; bytes: lower is better"
    );
    assert!((ids[0].1 - 0.28).abs() < 1e-9);
    assert_eq!(ids[1].0, "w40.engine_throughput");
    assert!((ids[1].1 - 0.25).abs() < 1e-9);
}

#[test]
fn w40_failed_and_vanished_benchmarks_are_missing() {
    let old = report(&[
        &latency("w38.match_latency", 70.0, 250.0),
        &rate("w36.sharded_throughput", "ops_per_sec", 9e6),
        &rate("w37.static_vs_dyn", "ratio", 1.2),
        r#"{"week":27,"id":"w27.add_cancel_slab","error":"not yet implemented"}"#,
    ]);
    let new = report(&[
        r#"{"week":38,"id":"w38.match_latency","error":"panicked at todo"}"#,
        &rate("w37.static_vs_dyn", "ratio", 1.2),
        &latency("w27.add_cancel_slab", 120.0, 300.0),
    ]);
    assert_eq!(
        regressions(&old, &new, 0.05),
        Ok(vec![
            Regression::Missing {
                id: "w36.sharded_throughput".into()
            },
            Regression::Missing {
                id: "w38.match_latency".into()
            },
        ])
    );
}

#[test]
fn w40_reads_benchjson_lines_and_rejects_garbage() {
    let lines = "   Compiling engine\nBENCHJSON {\"id\":\"w40.match_latency\",\"label\":\"a\",\"unit\":\"ns\",\"value\":70.000,\"p50\":70.000,\"p99\":240.000,\"p999\":350.000,\"max\":9.0,\"samples\":10}\nnoise\nBENCHJSON {\"id\":\"w40.engine_throughput\",\"label\":\"b\",\"unit\":\"ops_per_sec\",\"value\":2.5e7,\"p50\":null,\"p99\":null,\"p999\":null,\"max\":null,\"samples\":20}\n";
    let records = parse_benches(lines).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].p99, Some(240.0));
    assert_eq!(records[1].value, Some(2.5e7));
    // A BENCHJSON capture compared with a full report.
    let new = report(&[
        &latency("w40.match_latency", 70.0, 400.0),
        &rate("w40.engine_throughput", "ops_per_sec", 2.6e7),
    ]);
    let found = regressions(lines, &new, 0.1).unwrap();
    assert!(matches!(
        &found[..],
        [Regression::Worse {
            metric: Metric::P99,
            ..
        }]
    ));
    for bad in [
        "{\"benches\": [",
        "[1, 2]",
        "{\"benches\":[{\"unit\":\"ns\"}]}",
        "BENCHJSON {oops}",
    ] {
        assert!(regressions(bad, &new, 0.1).is_err(), "{bad}");
    }
}

#[test]
fn w40_fastest_configuration_is_the_same_engine() {
    for seed in 0..300 {
        let cfg = FlowConfig::randomized(0x4000 + seed);
        let stream = OrderFlow::new(cfg).take_commands(300);
        let mut fast = fastest_engine(cfg.engine_config());
        let mut reference = Engine::with_config(cfg.engine_config());
        if let Err(m) = differential(&mut fast, &mut reference, &stream) {
            panic!("seed {seed}: {m}");
        }
    }
    let cmds = mixed_stream(0x4040, 300_000);
    let cfg = EngineConfig {
        expected_orders: 4_096,
        ..EngineConfig::new(InstrumentSpec::default())
    };
    let mut fast = fastest_engine(cfg);
    let mut reference = Engine::with_config(cfg);
    let mut out = Vec::with_capacity(256);
    for cmd in &cmds[..100_000] {
        out.clear();
        fast.process(cmd, &mut out);
        out.clear();
        reference.process(cmd, &mut out);
    }
    let allocs = allocations_in(|| {
        for cmd in &cmds[100_000..] {
            out.clear();
            fast.process(cmd, &mut out);
        }
    });
    assert_eq!(
        allocs, 0,
        "the fastest configuration allocated in steady state"
    );
    for cmd in &cmds[100_000..] {
        out.clear();
        reference.process(cmd, &mut out);
    }
    assert_eq!(fast.state_hash(), reference.state_hash());
}
