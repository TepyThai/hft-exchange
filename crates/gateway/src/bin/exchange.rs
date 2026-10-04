//! The exchange binary (week 22): load and validate `exchange.toml`, then
//! run the gateway, the pre-trade gate and the engine thread.
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

use gateway::{ExchangeConfig, Gateway, Hooks, NoStage};
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
    let hooks = Hooks {
        pretrade: Box::new(cfg.risk_gate()),
        stage: Box::new(NoStage),
    };
    let gw = match Gateway::start_with(cfg.gateway_config(), hooks) {
        Ok(gw) => gw,
        Err(e) => {
            eprintln!("cannot start the gateway: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("listening on {}", gw.local_addr());
    let start = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(200));
        if run_for.is_some_and(|d| start.elapsed() >= d) {
            break;
        }
    }
    let report = gw.shutdown(Duration::from_secs(10));
    println!("shut down: {report:?}");
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
