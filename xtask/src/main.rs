//! `cargo xtask` — workspace tasks for hft-exchange.
//!
//! ```text
//! cargo xtask test   [--week N | --upto N]            week-scoped tests (names start with wNN_)
//! cargo xtask bench  [--week N | --upto N] [FILTER]   benchmarks (ids start with wNN.)
//! cargo xtask report [--upto N] [--no-bench] [--upload] [--token T] [--endpoint URL] [--source ci|local]
//! cargo xtask strip  --out DIR [--check]              generate the learner template from the reference
//! ```
//!
//! Reports are JSON (schema 1) written to `hft-report.json`; `--upload`
//! POSTs them to tepy.dev (token from `--token` or `TEPY_TOKEN`), where the
//! week pages and the lab dashboard show your test status and benchmark
//! history.

mod report;
mod strip;

use std::process::ExitCode;

pub const MAX_WEEK: u32 = 52;

#[derive(Clone, Copy, Debug)]
pub enum Weeks {
    One(u32),
    UpTo(u32),
    All,
}

impl Weeks {
    pub fn includes(self, week: u32) -> bool {
        match self {
            Weeks::One(n) => week == n,
            Weeks::UpTo(n) => week <= n,
            Weeks::All => true,
        }
    }

    pub fn test_filters(self) -> Vec<String> {
        let range = match self {
            Weeks::One(n) => n..=n,
            Weeks::UpTo(n) => 1..=n,
            Weeks::All => 1..=MAX_WEEK,
        };
        range.map(|n| format!("w{n:02}_")).collect()
    }
}

pub struct Args {
    pub weeks: Weeks,
    pub flags: Vec<String>,
    pub values: std::collections::HashMap<String, String>,
    pub positional: Vec<String>,
}

impl Args {
    fn parse(raw: &[String]) -> Result<Args, String> {
        let mut weeks = Weeks::All;
        let mut flags = Vec::new();
        let mut values = std::collections::HashMap::new();
        let mut positional = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            let a = &raw[i];
            let takes_value = matches!(a.as_str(), "--week" | "--upto" | "--token" | "--endpoint" | "--source" | "--out" | "--json");
            if takes_value {
                let v = raw.get(i + 1).ok_or_else(|| format!("{a} needs a value"))?.clone();
                match a.as_str() {
                    "--week" => weeks = Weeks::One(parse_week(&v)?),
                    "--upto" => weeks = Weeks::UpTo(parse_week(&v)?),
                    _ => {
                        values.insert(a.trim_start_matches("--").to_string(), v);
                    }
                }
                i += 2;
            } else if let Some(flag) = a.strip_prefix("--") {
                flags.push(flag.to_string());
                i += 1;
            } else {
                positional.push(a.clone());
                i += 1;
            }
        }
        Ok(Args { weeks, flags, values, positional })
    }

    pub fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|f| f == name)
    }

    pub fn value(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(|s| s.as_str())
    }
}

fn parse_week(v: &str) -> Result<u32, String> {
    let n: u32 = v.parse().map_err(|_| format!("not a week number: {v}"))?;
    if !(1..=MAX_WEEK).contains(&n) {
        return Err(format!("week must be 1..={MAX_WEEK}"));
    }
    Ok(n)
}

const USAGE: &str = "usage: cargo xtask <test|bench|report|strip> [--week N | --upto N] [options]
  test    run week-scoped tests (names start with wNN_)
  bench   run benchmarks (ids start with wNN.) — optional substring FILTER
  report  tests + benchmarks → hft-report.json; --upload sends it to tepy.dev
          (--token or TEPY_TOKEN, --endpoint, --source ci|local, --no-bench)
  strip   --out DIR [--check]: generate the learner template (bodies of
          `// @student wNN` items become todo!()); --check also builds it";

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = raw.first() else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let args = match Args::parse(&raw[1..]) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    let result = match cmd.as_str() {
        "test" => report::cmd_test(&args),
        "bench" => report::cmd_bench(&args),
        "report" => report::cmd_report(&args),
        "strip" => strip::cmd_strip(&args),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(true)
        }
        other => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
