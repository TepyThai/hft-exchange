//! Exchange v2 under load (week 24): starts the exchange in-process and
//! drives it over loopback TCP with the open-loop generator
//! ([`gateway::loadgen`]), one run per offered rate. For each rate it
//! prints the client's end-to-end latency (intended send → ack, sockets
//! included), the busy rejects, and the server's stage attribution of the
//! same orders (week 39's table: where the slowest orders spent their
//! time).
//!
//! ```text
//! loadtest [--config PATH] [--rates 20000,50000,100000] [--sessions 2]
//!          [--commands 100000] [--fsync always|never|interval:MS]
//!          [--journal DIR | --memory] [--busy-reject] [--io-threads N]
//!          [--inbound N]
//! ```
//!
//! * `--config PATH` — instruments, queues and risk limits from an
//!   `exchange.toml` (default: one test instrument). Order entry listens on
//!   an ephemeral loopback port, logons are open and the throttles are off
//!   (the limits still run), so the generator measures the pipeline rather
//!   than the rate limits.
//! * `--journal DIR` — where the journals go (default: a fresh temporary
//!   directory, removed afterwards); `--memory` keeps them in memory.
//! * `--fsync` — overrides the configuration's policy.
//! * `--busy-reject` — answer orders that find the engine queue full with
//!   a busy reject instead of back-pressure.
//! * `--io-threads N`, `--inbound N` — the gateway's Tokio workers and its
//!   sessions → engine queue capacity (default: the configuration's). A
//!   small queue with `--busy-reject` shows edge rejects under overload.
//!
//! Each session is a busy-polling thread, and so is the engine thread:
//! give the process `sessions + io threads + 2` free cores (`taskset`) for
//! numbers worth comparing.

use gateway::config::Fsync;
use gateway::loadgen::{self, LoadConfig};
use gateway::session::Auth;
use gateway::{ExchangeConfig, ExchangeV2, JournalBackend, MemJournals, RiskGate, V2Config};
use risk::BucketConfig;
use sim::flow::FlowConfig;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str =
    "usage: loadtest [--config PATH] [--rates R,R,..] [--sessions N] [--commands N] \
                     [--fsync always|never|interval:MS] [--journal DIR | --memory] [--busy-reject] \
                     [--io-threads N] [--inbound N]";

fn main() -> ExitCode {
    let mut config_path: Option<PathBuf> = None;
    let mut rates = vec![20_000u64, 50_000, 100_000];
    let (mut sessions, mut commands) = (2usize, 100_000u64);
    let mut io_threads: Option<usize> = None;
    let mut inbound: Option<usize> = None;
    let mut fsync: Option<Fsync> = None;
    let mut journal_dir: Option<PathBuf> = None;
    let (mut memory, mut busy_reject) = (false, false);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        let parsed: Result<(), String> = (|| {
            match arg.as_str() {
                "--config" => config_path = Some(PathBuf::from(value()?)),
                "--rates" => {
                    rates = value()?
                        .split(',')
                        .map(|r| r.trim().replace('_', "").parse::<u64>())
                        .collect::<Result<_, _>>()
                        .map_err(|e| format!("--rates: {e}"))?
                }
                "--sessions" => sessions = value()?.parse().map_err(|e| format!("{e}"))?,
                "--commands" => commands = value()?.parse().map_err(|e| format!("{e}"))?,
                "--io-threads" => io_threads = Some(value()?.parse().map_err(|e| format!("{e}"))?),
                "--inbound" => inbound = Some(value()?.parse().map_err(|e| format!("{e}"))?),
                "--fsync" => {
                    fsync = Some(match value()?.as_str() {
                        "always" => Fsync::Always,
                        "never" => Fsync::Never,
                        s => match s.strip_prefix("interval:").map(str::parse::<u64>) {
                            Some(Ok(ms)) if ms > 0 => Fsync::IntervalMs(ms),
                            _ => return Err(format!("bad --fsync `{s}`")),
                        },
                    })
                }
                "--journal" => journal_dir = Some(PathBuf::from(value()?)),
                "--memory" => memory = true,
                "--busy-reject" => busy_reject = true,
                "-h" | "--help" => {
                    println!("{USAGE}");
                    std::process::exit(0);
                }
                other => return Err(format!("unknown argument `{other}`")),
            }
            Ok(())
        })();
        if let Err(msg) = parsed {
            eprintln!("error: {msg}\n{USAGE}");
            return ExitCode::from(2);
        }
    }

    let file = match &config_path {
        Some(p) => match ExchangeConfig::load(p) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{}:\n{e}", p.display());
                return ExitCode::from(2);
            }
        },
        None => ExchangeConfig::default(),
    };
    let mut config = V2Config::from_config(&file);
    config.gateway.listen = "127.0.0.1:0".parse().expect("address");
    config.gateway.auth = Auth::Open;
    config.gateway.busy_reject |= busy_reject;
    config.feed.listen = None;
    if let Some(n) = io_threads {
        config.gateway.io_threads = n;
    }
    if let Some(n) = inbound {
        config.gateway.inbound_capacity = n;
    }
    config.stage_samples = (commands as usize).max(1 << 16);
    if let Some(f) = fsync {
        config.fsync = f;
    }
    let temp = std::env::temp_dir().join(format!("hft-loadtest-{}", std::process::id()));
    let cleanup = journal_dir.is_none() && !memory;
    config.journal = if memory {
        JournalBackend::Memory(MemJournals::new())
    } else {
        let dir = journal_dir.unwrap_or_else(|| temp.clone());
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("{}: {e}", dir.display());
            return ExitCode::FAILURE;
        }
        JournalBackend::File(dir.join("journal.bin"))
    };
    let spec = config.gateway.instruments[0].spec;
    let fair = file.instruments[0]
        .reference_price
        .unwrap_or(10_000 * spec.tick);
    let risk = RiskGate::new(
        file.limits(),
        BucketConfig::UNLIMITED,
        BucketConfig::UNLIMITED,
    );
    println!(
        "exchange v2: {} instrument(s), fsync {}, journal {:?}, busy reject {}, inbound queue {}",
        config.gateway.instruments.len(),
        config.fsync,
        config.journal,
        config.gateway.busy_reject,
        config.gateway.inbound_capacity,
    );
    let exchange = match ExchangeV2::start(config, risk) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e}");
            if cleanup {
                let _ = std::fs::remove_dir_all(&temp);
            }
            return ExitCode::FAILURE;
        }
    };

    let mut ok = true;
    for (n, &rate) in rates.iter().enumerate() {
        let load = LoadConfig {
            sessions,
            flow: FlowConfig {
                spec,
                fair_value: fair,
                ..FlowConfig::default()
            },
            seed: 24 + 1000 * n as u64,
            // Fresh accounts per run: the flows start with no resting orders.
            first_account: 1 + (n * sessions) as u32,
            ..LoadConfig::new(rate, commands)
        };
        let busy_before = exchange.stats().busy_rejects;
        match loadgen::run(exchange.local_addr(), &load) {
            Ok(r) => {
                println!("\n== {rate} commands/s ==\n{}", r.summary());
                println!(
                    "achieved {:.0} responses/s; gateway busy rejects {}",
                    r.achieved_rate(),
                    exchange.stats().busy_rejects - busy_before
                );
                let samples = exchange.stage_log().samples_since(r.measure_start_ns);
                print!(
                    "server stages, ns ({} orders):\n{}",
                    samples.len(),
                    telemetry::Attribution::from_samples(&samples)
                );
            }
            Err(e) => {
                eprintln!("load run at {rate}/s failed: {e}");
                ok = false;
                break;
            }
        }
        // Let the sessions' disconnects (and cancels) drain.
        std::thread::sleep(Duration::from_millis(100));
    }
    let report = exchange.shutdown(Duration::from_secs(10));
    println!("\nshut down: {:?}", report.pipeline);
    if cleanup {
        let _ = std::fs::remove_dir_all(&temp);
    }
    if ok && report.is_clean() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
