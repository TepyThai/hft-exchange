//! The exchange binary (weeks 22 and 24): load and validate
//! `exchange.toml`, then run exchange v2 — the gateway, the pre-trade gate,
//! the engine thread with its journals (`[journal] path` and `fsync`) and
//! the market-data fan-out (`[marketdata]`, served over WebSocket when
//! `listen` is set). On start it recovers every instrument's book from its
//! journal; on shutdown it drains, syncs, and prints where the time went.
//!
//! ```text
//! exchange [--config PATH] [--check-config | --print-config] [--run-for SECS]
//! ```
//!
//! * `--config PATH` — the configuration file (default: `$HFT_CONFIG`, else
//!   `exchange.toml`). `HFT_<SECTION>_<KEY>` variables override keys.
//! * `--check-config` — validate, print every error, exit non-zero on any
//!   (run it in CI and in the deploy pipeline against the exact file).
//! * `--print-config` — print the effective configuration (defaults filled
//!   in) and exit.
//! * `--run-for SECS` — shut down cleanly after `SECS` seconds (smoke
//!   tests); otherwise run until killed.
//!
//! Exit codes: 0 ok, 1 runtime failure, 2 configuration or usage error.

use gateway::{ExchangeConfig, ExchangeV2, V2Config};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

const USAGE: &str =
    "usage: exchange [--config PATH] [--check-config | --print-config] [--run-for SECS]";

fn main() -> ExitCode {
    let mut config = std::env::var_os("HFT_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("exchange.toml"));
    let (mut check, mut print, mut run_for) = (false, false, None);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => match args.next() {
                Some(p) => config = PathBuf::from(p),
                None => return usage("--config needs a path"),
            },
            "--check-config" => check = true,
            "--print-config" => print = true,
            "--run-for" => match args.next().and_then(|s| s.parse::<u64>().ok()) {
                Some(secs) => run_for = Some(Duration::from_secs(secs)),
                None => return usage("--run-for needs a number of seconds"),
            },
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => return usage(&format!("unknown argument `{other}`")),
        }
    }

    let cfg = match ExchangeConfig::load(&config) {
        Ok(cfg) => cfg,
        Err(errors) => {
            eprintln!(
                "{}: {} configuration error(s):",
                config.display(),
                errors.errors().len()
            );
            for e in errors.errors() {
                eprintln!("  {e}");
            }
            return ExitCode::from(2);
        }
    };
    let hash = cfg.hash();
    if print {
        print!(
            "# effective configuration, hash {hash:016x}\n{}",
            cfg.to_toml()
        );
        return ExitCode::SUCCESS;
    }
    if check {
        println!("{}: ok (hash {hash:016x})", config.display());
        return ExitCode::SUCCESS;
    }

    println!(
        "exchange {} starting: config {} (hash {hash:016x}), {} instrument(s), listen {}",
        env!("CARGO_PKG_VERSION"),
        config.display(),
        cfg.instruments.len(),
        cfg.gateway.listen
    );
    let exchange = match ExchangeV2::start(V2Config::from_config(&cfg), cfg.risk_gate()) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("cannot start: {e}");
            return ExitCode::FAILURE;
        }
    };
    for r in exchange.recovery() {
        println!(
            "instrument {}: journal {}: {} command(s) replayed, state {:016x}{}",
            r.instrument.0,
            gateway::journal_path(&cfg.journal.path, r.instrument).display(),
            r.replayed,
            r.state_hash,
            match &r.torn_tail {
                Some(t) => format!(", torn tail of {} byte(s) cut", t.dropped_bytes),
                None => String::new(),
            }
        );
    }
    for (instrument, addr) in exchange.md_addrs() {
        println!("market data for instrument {}: ws://{addr}/", instrument.0);
    }
    println!(
        "order entry on {}, fsync {}",
        exchange.local_addr(),
        cfg.journal.fsync
    );
    let start = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(200));
        if run_for.is_some_and(|d| start.elapsed() >= d) {
            break;
        }
    }
    let attribution = exchange.stage_log().attribution();
    let stats = exchange.stats();
    let report = exchange.shutdown(Duration::from_secs(10));
    println!("shut down: {:?}", report.gateway);
    println!("{:?}; busy rejects {}", report.pipeline, stats.busy_rejects);
    if attribution.samples() > 0 {
        print!(
            "where the time went, ns (last {} orders):\n{attribution}",
            attribution.samples()
        );
    }
    if report.is_clean() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn usage(msg: &str) -> ExitCode {
    eprintln!("error: {msg}\n{USAGE}");
    ExitCode::from(2)
}
