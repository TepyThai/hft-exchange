//! `demo`: run the demo exchange (see the crate docs and `demo --help`).

use demo::client::http_get;
use demo::{Config, Demo, Invocation, USAGE};
use std::process::ExitCode;

fn main() -> ExitCode {
    let config = match Config::parse(std::env::args().skip(1), |k| std::env::var(k).ok()) {
        Ok(Invocation::Serve(config)) => config,
        Ok(Invocation::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Invocation::HealthCheck(config)) => {
            return match http_get(config.http_addr, "/health") {
                Ok(r) if r.status == 200 => ExitCode::SUCCESS,
                Ok(r) => {
                    eprintln!("demo: not ready ({}): {}", r.status, r.text());
                    ExitCode::FAILURE
                }
                Err(e) => {
                    eprintln!("demo: no answer from {}: {e}", config.http_addr);
                    ExitCode::FAILURE
                }
            };
        }
        Err(e) => {
            eprintln!("demo: {e}");
            return ExitCode::from(2);
        }
    };

    let (rate, seed, duration) = (config.rate, config.seed, config.duration);
    let demo = match Demo::start(config) {
        Ok(demo) => demo,
        Err(e) => {
            eprintln!("demo: cannot start: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Tests and scripts read these two lines to find the ports.
    println!("http listening on http://{}", demo.http_addr());
    println!("ws listening on ws://{}", demo.ws_addr());
    let rate = if rate == 0 {
        "unthrottled".to_string()
    } else {
        format!("{rate} commands/s")
    };
    let until = duration.map_or("until stopped".to_string(), |d| format!("for {d:?}"));
    eprintln!("demo: seed {seed}, {rate}, running {until}");

    let stats = demo.wait();
    println!("stopped: {}", stats.to_json());
    ExitCode::SUCCESS
}
