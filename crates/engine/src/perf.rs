//! Week 40: the performance sprint — the fastest engine configuration, and
//! a regression check between two benchmark reports.
//!
//! ```
//! use engine::perf::{regressions, Regression};
//!
//! let old = r#"BENCHJSON {"id":"w40.match_latency","unit":"ns","value":70,"p50":70,"p99":240,"p999":350}"#;
//! let new = r#"BENCHJSON {"id":"w40.match_latency","unit":"ns","value":72,"p50":72,"p99":310,"p999":360}"#;
//! let found = regressions(old, new, 0.10).unwrap();
//! assert!(matches!(&found[..], [Regression::Worse { by, .. }] if (by - 0.2917).abs() < 1e-3));
//! ```

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::fast::FastEngine;
use crate::policy::NoRisk;
use crate::EngineConfig;
use book::LadderBook;

/// The fastest configuration measured on the reference machine: the w38
/// hot/cold-split engine over the w28 ladder book, statically dispatched,
/// no risk policy. (See the w40 benchmarks; `cargo xtask bench --week 40`.)
pub type FastestEngine = FastEngine<LadderBook, NoRisk>;

/// A [`FastestEngine`] for `config`, its ladder window covering 4096 prices
/// per side and sized for `config.expected_orders`.
pub fn fastest_engine(config: EngineConfig) -> FastestEngine {
    let window = 4_096 * config.spec.tick.max(1) as usize;
    FastEngine::new(
        config,
        LadderBook::new(window, config.expected_orders),
        NoRisk,
    )
}

/// One benchmark result read from a report.
#[derive(Clone, Debug, PartialEq)]
pub struct BenchRecord {
    /// Benchmark id, e.g. `w40.match_latency`.
    pub id: String,
    /// `ns`, `us`, `ops_per_sec`, `msgs_per_sec`, `ratio` or `bytes`.
    pub unit: String,
    /// The headline number (`None` if the benchmark failed in that run).
    pub value: Option<f64>,
    pub p50: Option<f64>,
    pub p99: Option<f64>,
    pub p999: Option<f64>,
}

impl BenchRecord {
    /// The number for `metric`, if the report has it.
    pub fn get(&self, metric: Metric) -> Option<f64> {
        match metric {
            Metric::Value => self.value,
            Metric::P50 => self.p50,
            Metric::P99 => self.p99,
            Metric::P999 => self.p999,
        }
    }
}

/// A report that could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportError(pub String);

impl std::fmt::Display for ReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bad benchmark report: {}", self.0)
    }
}

impl std::error::Error for ReportError {}

/// Which number of a benchmark regressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Metric {
    /// The headline value (throughput, ratio, size).
    Value,
    P50,
    P99,
    P999,
}

/// One regression between two reports.
#[derive(Clone, Debug, PartialEq)]
pub enum Regression {
    /// `metric` of benchmark `id` moved in the bad direction by `by`
    /// (relative: 0.25 = 25% worse) — more than the tolerance.
    Worse {
        id: String,
        metric: Metric,
        old: f64,
        new: f64,
        by: f64,
    },
    /// Benchmark `id` has a result in the old report but none in the new
    /// one (failed, or no longer run).
    Missing { id: String },
}

/// True if smaller numbers are better for `unit` (latencies and sizes);
/// false for rates and ratios.
pub fn lower_is_better(unit: &str) -> bool {
    matches!(unit, "ns" | "us" | "bytes")
}

/// The metrics compared for `unit`: p50 and p99 for latencies (p99.9 is
/// too noisy for an automatic gate), the value for everything else.
pub fn metrics_for(unit: &str) -> &'static [Metric] {
    match unit {
        "ns" | "us" => &[Metric::P50, Metric::P99],
        _ => &[Metric::Value],
    }
}

/// Compares two benchmark reports and lists every regression worse than
/// `tolerance` (relative, e.g. `0.05` = 5%).
///
/// Both inputs are anything [`parse_benches`] reads (an `xtask report`
/// JSON file or `BENCHJSON` lines). For each benchmark with a result in
/// `old`: if `new` has no result for it, that is [`Regression::Missing`];
/// otherwise each metric of [`metrics_for`] its unit is compared in the
/// direction of [`lower_is_better`], and a change worse than `tolerance`
/// is [`Regression::Worse`]. Old values ≤ 0 and benchmarks whose unit
/// changed are skipped; benchmarks only in `new` are ignored. The result
/// is sorted by id, then metric. O((n + m) log(n + m)).
#[allow(unused_variables)]
pub fn regressions(old: &str, new: &str, tolerance: f64) -> Result<Vec<Regression>, ReportError> {
    todo!("w40: regressions")
}

// TODO(w40): implement — see the week guide on tepy.dev/hft.

/// Reads the benchmark records out of a report:
///
/// * the JSON `cargo xtask report` writes (`{"benches": [...], ...}`);
/// * a JSON array of benchmark objects;
/// * the `BENCHJSON {...}` lines a bench binary prints (other lines are
///   ignored), as captured from `cargo bench` or `cargo xtask bench`.
///
/// A failed benchmark (`{"id": ..., "error": ...}`) is a record with every
/// number `None`. Duplicate ids are kept: [`regressions`] compares every
/// successful record of an id in `old` against the *last* successful record
/// of that id in `new`. Provided infrastructure; O(input).
pub fn parse_benches(text: &str) -> Result<Vec<BenchRecord>, ReportError> {
    let trimmed = text.trim_start();
    let values = if trimmed.starts_with('{') || trimmed.starts_with('[') {
        match json::parse(trimmed)? {
            json::Value::Object(fields) => match json::field(&fields, "benches") {
                Some(json::Value::Array(items)) => items.clone(),
                _ => vec![json::Value::Object(fields)],
            },
            json::Value::Array(items) => items,
            _ => return Err(ReportError("expected an object or an array".into())),
        }
    } else {
        let mut items = Vec::new();
        for line in text.lines() {
            if let Some(obj) = line.trim().strip_prefix("BENCHJSON ") {
                items.push(json::parse(obj)?);
            }
        }
        items
    };
    values.iter().map(record).collect()
}

fn record(v: &json::Value) -> Result<BenchRecord, ReportError> {
    let json::Value::Object(fields) = v else {
        return Err(ReportError("a benchmark entry is not an object".into()));
    };
    let id = match json::field(fields, "id") {
        Some(json::Value::String(s)) => s.clone(),
        _ => return Err(ReportError("a benchmark entry has no id".into())),
    };
    let num = |k: &str| match json::field(fields, k) {
        Some(json::Value::Number(n)) => Some(*n),
        _ => None,
    };
    let unit = match json::field(fields, "unit") {
        Some(json::Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    Ok(BenchRecord {
        id,
        unit,
        value: num("value"),
        p50: num("p50"),
        p99: num("p99"),
        p999: num("p999"),
    })
}

/// A minimal JSON reader — enough for benchmark reports (no external
/// crates in the engine).
mod json {
    use super::ReportError;

    #[derive(Clone, Debug, PartialEq)]
    pub enum Value {
        Null,
        Bool(bool),
        Number(f64),
        String(String),
        Array(Vec<Value>),
        Object(Vec<(String, Value)>),
    }

    pub fn field<'a>(fields: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
        fields.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn parse(text: &str) -> Result<Value, ReportError> {
        let mut p = Parser {
            b: text.as_bytes(),
            i: 0,
        };
        let v = p.value(0)?;
        p.ws();
        if p.i != p.b.len() {
            return Err(p.err("trailing characters"));
        }
        Ok(v)
    }

    struct Parser<'a> {
        b: &'a [u8],
        i: usize,
    }

    impl Parser<'_> {
        fn err(&self, what: &str) -> ReportError {
            ReportError(format!("{what} at byte {}", self.i))
        }

        fn ws(&mut self) {
            while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
        }

        fn eat(&mut self, c: u8) -> Result<(), ReportError> {
            self.ws();
            if self.b.get(self.i) == Some(&c) {
                self.i += 1;
                Ok(())
            } else {
                Err(self.err(&format!("expected `{}`", c as char)))
            }
        }

        fn value(&mut self, depth: usize) -> Result<Value, ReportError> {
            if depth > 64 {
                return Err(self.err("nesting too deep"));
            }
            self.ws();
            match self.b.get(self.i) {
                Some(b'{') => {
                    self.i += 1;
                    let mut fields = Vec::new();
                    self.ws();
                    if self.b.get(self.i) == Some(&b'}') {
                        self.i += 1;
                        return Ok(Value::Object(fields));
                    }
                    loop {
                        self.ws();
                        let k = self.string()?;
                        self.eat(b':')?;
                        fields.push((k, self.value(depth + 1)?));
                        self.ws();
                        match self.b.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b'}') => {
                                self.i += 1;
                                return Ok(Value::Object(fields));
                            }
                            _ => return Err(self.err("expected `,` or `}`")),
                        }
                    }
                }
                Some(b'[') => {
                    self.i += 1;
                    let mut items = Vec::new();
                    self.ws();
                    if self.b.get(self.i) == Some(&b']') {
                        self.i += 1;
                        return Ok(Value::Array(items));
                    }
                    loop {
                        items.push(self.value(depth + 1)?);
                        self.ws();
                        match self.b.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b']') => {
                                self.i += 1;
                                return Ok(Value::Array(items));
                            }
                            _ => return Err(self.err("expected `,` or `]`")),
                        }
                    }
                }
                Some(b'"') => Ok(Value::String(self.string()?)),
                Some(b't') => self.word("true", Value::Bool(true)),
                Some(b'f') => self.word("false", Value::Bool(false)),
                Some(b'n') => self.word("null", Value::Null),
                Some(c) if *c == b'-' || c.is_ascii_digit() => self.number(),
                _ => Err(self.err("expected a value")),
            }
        }

        fn word(&mut self, w: &str, v: Value) -> Result<Value, ReportError> {
            if self.b[self.i..].starts_with(w.as_bytes()) {
                self.i += w.len();
                Ok(v)
            } else {
                Err(self.err("bad literal"))
            }
        }

        fn number(&mut self) -> Result<Value, ReportError> {
            let start = self.i;
            while self.i < self.b.len()
                && matches!(
                    self.b[self.i],
                    b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
                )
            {
                self.i += 1;
            }
            std::str::from_utf8(&self.b[start..self.i])
                .ok()
                .and_then(|s| s.parse::<f64>().ok())
                .map(Value::Number)
                .ok_or_else(|| self.err("bad number"))
        }

        fn string(&mut self) -> Result<String, ReportError> {
            if self.b.get(self.i) != Some(&b'"') {
                return Err(self.err("expected a string"));
            }
            self.i += 1;
            let mut out = String::new();
            loop {
                let Some(&c) = self.b.get(self.i) else {
                    return Err(self.err("unterminated string"));
                };
                self.i += 1;
                match c {
                    b'"' => return Ok(out),
                    b'\\' => {
                        let Some(&e) = self.b.get(self.i) else {
                            return Err(self.err("unterminated escape"));
                        };
                        self.i += 1;
                        match e {
                            b'"' => out.push('"'),
                            b'\\' => out.push('\\'),
                            b'/' => out.push('/'),
                            b'n' => out.push('\n'),
                            b't' => out.push('\t'),
                            b'r' => out.push('\r'),
                            b'b' => out.push('\u{8}'),
                            b'f' => out.push('\u{c}'),
                            b'u' => {
                                let hex = self
                                    .b
                                    .get(self.i..self.i + 4)
                                    .and_then(|h| std::str::from_utf8(h).ok())
                                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                                    .ok_or_else(|| self.err("bad \\u escape"))?;
                                self.i += 4;
                                out.push(char::from_u32(hex).unwrap_or('\u{fffd}'));
                            }
                            _ => return Err(self.err("bad escape")),
                        }
                    }
                    _ => {
                        // Copy a run of plain bytes (valid UTF-8 input stays valid).
                        let start = self.i - 1;
                        while self.i < self.b.len() && !matches!(self.b[self.i], b'"' | b'\\') {
                            self.i += 1;
                        }
                        out.push_str(
                            std::str::from_utf8(&self.b[start..self.i])
                                .map_err(|_| self.err("invalid UTF-8"))?,
                        );
                    }
                }
            }
        }
    }
}
