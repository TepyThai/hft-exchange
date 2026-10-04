//! `cargo xtask report --markdown PATH`: render a report (JSON schema 1) as
//! a Markdown benchmark report — a provenance table (commit, machine, date),
//! the tests per week, latency and throughput tables, and failed
//! benchmarks. The README and docs publish this output instead of numbers
//! typed by hand.
//!
//! [`render`] is a pure function of the JSON, so a fixed report always
//! renders to the same bytes (the golden test below holds it to that).
//! Number formatting follows the week 51 `bench-table` drill: three
//! significant figures, truncated, with the unit switched to keep them.

use std::fmt::Write as _;

/// Renders a schema-1 report. Errors name what is missing or malformed.
pub fn render(report_json: &str) -> Result<String, String> {
    let report = Json::parse(report_json)?;
    let schema = report.get("schema").and_then(Json::as_f64);
    if schema != Some(1.0) {
        return Err(format!("unsupported report schema {schema:?} (expected 1)"));
    }
    let text = |v: Option<&Json>| v.and_then(Json::as_str).map(str::to_string);
    let mut out = String::from("# Benchmark report\n\n");

    // Provenance: what ran where. A reader must be able to reproduce it.
    let sha = text(report.get("sha"));
    let short = sha
        .as_deref()
        .map(|s| &s[..s.len().min(12)])
        .unwrap_or("unknown");
    let mut commit = format!("`{short}`");
    if let Some(branch) = text(report.get("branch")) {
        let _ = write!(commit, " on `{branch}`");
    }
    if report.get("dirty") == Some(&Json::Bool(true)) {
        commit.push_str(" (uncommitted changes)");
    }
    if let (Some(repo), Some(sha)) = (text(report.get("repo")), &sha) {
        let _ = write!(commit, " · [source]({repo}/tree/{sha})");
    }
    let machine = report.get("machine");
    let field = |k: &str| machine.and_then(|m| m.get(k));
    let cpu = text(field("cpu")).unwrap_or_else(|| "unknown CPU".into());
    let cores = field("cores")
        .and_then(Json::as_f64)
        .map_or("? cores".into(), |c| format!("{c} cores"));
    let os = text(field("os")).unwrap_or_else(|| "unknown OS".into());
    let rustc = text(field("rustc")).unwrap_or_else(|| "unknown rustc".into());
    let created = report
        .get("createdAt")
        .and_then(Json::as_f64)
        .map_or("unknown".into(), |ms| utc_minutes(ms as i64));
    let source = text(report.get("source")).unwrap_or_else(|| "unknown".into());
    let tool = text(report.get("tool")).unwrap_or_else(|| "xtask".into());
    let _ = write!(
        out,
        "| | |\n| --- | --- |\n| Commit | {} |\n| Machine | {} · {} · {} |\n| Toolchain | {} |\n| Run | {} · {} · {} |\n\n",
        cell(&commit),
        cell(&cpu),
        cores,
        cell(&os),
        cell(&rustc),
        created,
        cell(&source),
        cell(&tool)
    );
    out.push_str(
        "Benchmarks depend on the machine, its load and its tuning: compare runs from the same machine and profile only.\n\n",
    );

    if let Some(err) = text(report.get("buildError")) {
        let _ = write!(out, "**Build failed:**\n\n```text\n{err}\n```\n\n");
    }

    // Tests.
    let tests = report.get("tests").and_then(Json::as_array).unwrap_or(&[]);
    if !tests.is_empty() {
        out.push_str("## Tests\n\n| Week | Passed | Failed |\n| ---: | ---: | --- |\n");
        let (mut passed, mut failed) = (0.0, 0.0);
        for t in tests {
            let n = |k: &str| t.get(k).and_then(Json::as_f64).unwrap_or(0.0);
            passed += n("passed");
            failed += n("failed");
            let failures: Vec<String> = t
                .get("failures")
                .and_then(Json::as_array)
                .unwrap_or(&[])
                .iter()
                .filter_map(Json::as_str)
                .map(|f| format!("`{}`", cell(f)))
                .collect();
            let fail = if failures.is_empty() {
                format!("{}", n("failed"))
            } else {
                format!("{} ({})", n("failed"), failures.join(", "))
            };
            let _ = writeln!(out, "| {} | {} | {} |", n("week"), n("passed"), fail);
        }
        let _ = writeln!(out, "| **all** | **{passed}** | **{failed}** |\n");
    }

    // Benchmarks, split by kind: distributions and single values.
    let benches = report
        .get("benches")
        .and_then(Json::as_array)
        .unwrap_or(&[]);
    let mut latency = Vec::new();
    let mut values = Vec::new();
    let mut failures = Vec::new();
    for b in benches {
        let s = |k: &str| b.get(k).and_then(Json::as_str).unwrap_or("");
        let f = |k: &str| b.get(k).and_then(Json::as_f64);
        let week = f("week").map_or("?".into(), |w| w.to_string());
        let id = format!("`{}`", cell(s("id")));
        if let Some(err) = b.get("error").and_then(Json::as_str) {
            failures.push(format!("| {week} | {id} | {} |", cell(err)));
            continue;
        }
        let unit = s("unit");
        let label = cell(s("label"));
        match (f("p50"), f("p99"), f("p999")) {
            (Some(p50), Some(p99), Some(p999)) if matches!(unit, "ns" | "us") => {
                let samples = f("samples").map_or("–".into(), |n| group(n as u64));
                let max = f("max").map_or("–".into(), |m| fmt_value(m, unit));
                latency.push(format!(
                    "| {week} | {id} | {label} | {} | {} | {} | {max} | {samples} |",
                    fmt_value(p50, unit),
                    fmt_value(p99, unit),
                    fmt_value(p999, unit)
                ));
            }
            _ => {
                let value = f("value").map_or("–".into(), |v| fmt_value(v, unit));
                values.push(format!("| {week} | {id} | {label} | {value} |"));
            }
        }
    }
    if !latency.is_empty() {
        out.push_str("## Latency\n\n| Week | Id | Benchmark | p50 | p99 | p99.9 | max | samples |\n| ---: | --- | --- | ---: | ---: | ---: | ---: | ---: |\n");
        for row in &latency {
            out.push_str(row);
            out.push('\n');
        }
        out.push('\n');
    }
    if !values.is_empty() {
        out.push_str("## Throughput and ratios\n\n| Week | Id | Benchmark | Result |\n| ---: | --- | --- | ---: |\n");
        for row in &values {
            out.push_str(row);
            out.push('\n');
        }
        out.push('\n');
    }
    if !failures.is_empty() {
        out.push_str("## Did not run\n\n| Week | Id | Error |\n| ---: | --- | --- |\n");
        for row in &failures {
            out.push_str(row);
            out.push('\n');
        }
        out.push('\n');
    }
    if benches.is_empty() {
        out.push_str("No benchmarks in this report (`--no-bench`).\n");
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    Ok(out)
}

/// A value in its unit: latencies as ns/µs/ms/s, rates with k/M/G, ratios
/// as `2.32×`, sizes in B/KiB/MiB/GiB. Three significant figures,
/// truncated.
pub fn fmt_value(v: f64, unit: &str) -> String {
    match unit {
        "ns" => fmt_ns(v),
        "us" => fmt_ns(v * 1_000.0),
        "ops_per_sec" => fmt_scaled(v, 1_000.0, &["", " k", " M", " G"], " ops/s"),
        "msgs_per_sec" => fmt_scaled(v, 1_000.0, &["", " k", " M", " G"], " msgs/s"),
        "ratio" => format!("{}×", sig3(v)),
        "bytes" => fmt_scaled(v, 1_024.0, &[" B", " KiB", " MiB", " GiB"], ""),
        other => format!("{} {other}", sig3(v)),
    }
}

/// Nanoseconds as `850 ns`, `1.23 µs`, `12.3 ms`, `1.50 s` (sub-10 ns
/// values keep one decimal: `3.4 ns`).
pub fn fmt_ns(ns: f64) -> String {
    if !ns.is_finite() || ns < 0.0 {
        return "–".into();
    }
    if ns < 10.0 && ns.fract() != 0.0 {
        return format!("{:.1} ns", (ns * 10.0).trunc() / 10.0);
    }
    if ns < 1_000.0 {
        return format!("{} ns", ns.trunc());
    }
    fmt_scaled(ns, 1_000.0, &["ns", " µs", " ms", " s"], "")
}

/// Divides by `step` until below it, then three significant figures with
/// the matching suffix.
fn fmt_scaled(v: f64, step: f64, suffixes: &[&str], tail: &str) -> String {
    if !v.is_finite() {
        return "–".into();
    }
    let mut v = v;
    let mut i = 0;
    while v.abs() >= step && i + 1 < suffixes.len() {
        v /= step;
        i += 1;
    }
    let number = if i == 0 && v.fract() == 0.0 {
        format!("{v}")
    } else {
        sig3(v)
    };
    format!("{number}{}{tail}", suffixes[i])
}

/// Three significant figures, truncated: 1.234 → `1.23`, 12.34 → `12.3`,
/// 123.4 → `123`, 1234 → `1234`.
fn sig3(v: f64) -> String {
    let a = v.abs();
    let (scale, decimals) = if a >= 100.0 {
        (1.0, 0)
    } else if a >= 10.0 {
        (10.0, 1)
    } else {
        (100.0, 2)
    };
    // Nudge before truncating so 1.15 (stored as 1.1499…) stays 1.15.
    let t = ((v * scale) + v.signum() * 1e-9).trunc() / scale;
    format!("{t:.decimals$}")
}

/// `1234567` → `1,234,567`.
fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Text safe inside a table cell.
fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace(['\n', '\r'], " ")
}

/// Unix milliseconds → `2026-10-04 15:28 UTC`.
fn utc_minutes(ms: i64) -> String {
    let secs = ms.div_euclid(1_000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        rem / 3_600,
        rem % 3_600 / 60
    )
}

// ---------------------------------------------------------------------------
// A small JSON reader (xtask has no dependencies).

/// A JSON value. Objects keep their key order.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// Parses one JSON document (RFC 8259), whitespace around it allowed.
    pub fn parse(text: &str) -> Result<Json, String> {
        let mut p = Parser {
            s: text.as_bytes(),
            i: 0,
        };
        let v = p.value(0)?;
        p.ws();
        if p.i != p.s.len() {
            return Err(p.err("trailing characters"));
        }
        Ok(v)
    }

    /// Field `key` of an object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

const MAX_DEPTH: usize = 64;

impl Parser<'_> {
    fn err(&self, what: &str) -> String {
        format!("invalid report JSON at byte {}: {what}", self.i)
    }

    fn ws(&mut self) {
        while matches!(self.s.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_DEPTH {
            return Err(self.err("nested too deeply"));
        }
        self.ws();
        match self.s.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                let mut fields = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Json::Obj(fields));
                }
                loop {
                    self.ws();
                    if self.s.get(self.i) != Some(&b'"') {
                        return Err(self.err("expected a key"));
                    }
                    let key = self.string()?;
                    self.ws();
                    if self.s.get(self.i) != Some(&b':') {
                        return Err(self.err("expected `:`"));
                    }
                    self.i += 1;
                    fields.push((key, self.value(depth + 1)?));
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Json::Obj(fields));
                        }
                        _ => return Err(self.err("expected `,` or `}`")),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Json::Arr(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Json::Arr(items));
                        }
                        _ => return Err(self.err("expected `,` or `]`")),
                    }
                }
            }
            Some(b'"') => self.string().map(Json::Str),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(self.err("expected a value")),
        }
    }

    fn literal(&mut self, word: &str, v: Json) -> Result<Json, String> {
        if self.s[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            Err(self.err("bad literal"))
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        if self.s.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        let digits = |p: &mut Self| {
            let from = p.i;
            while p.s.get(p.i).is_some_and(u8::is_ascii_digit) {
                p.i += 1;
            }
            p.i > from
        };
        if !digits(self) {
            return Err(self.err("bad number"));
        }
        if self.s.get(self.i) == Some(&b'.') {
            self.i += 1;
            if !digits(self) {
                return Err(self.err("bad fraction"));
            }
        }
        if matches!(self.s.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.s.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if !digits(self) {
                return Err(self.err("bad exponent"));
            }
        }
        let text =
            std::str::from_utf8(&self.s[start..self.i]).map_err(|_| self.err("bad number"))?;
        text.parse()
            .map(Json::Num)
            .map_err(|_| self.err("bad number"))
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1; // opening quote
        let mut out = String::new();
        loop {
            let start = self.i;
            while self
                .s
                .get(self.i)
                .is_some_and(|&c| c != b'"' && c != b'\\' && c >= 0x20)
            {
                self.i += 1;
            }
            out.push_str(
                std::str::from_utf8(&self.s[start..self.i])
                    .map_err(|_| self.err("invalid UTF-8"))?,
            );
            match self.s.get(self.i) {
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    let esc = *self
                        .s
                        .get(self.i + 1)
                        .ok_or_else(|| self.err("unterminated escape"))?;
                    self.i += 2;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let c = if (0xD800..0xDC00).contains(&hi) {
                                if !self.s[self.i..].starts_with(b"\\u") {
                                    return Err(self.err("unpaired surrogate"));
                                }
                                self.i += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err(self.err("unpaired surrogate"));
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else {
                                hi
                            };
                            out.push(char::from_u32(c).ok_or_else(|| self.err("bad \\u escape"))?);
                        }
                        _ => return Err(self.err("bad escape")),
                    }
                }
                _ => return Err(self.err("unterminated string")),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self
            .s
            .get(self.i..self.i + 4)
            .ok_or_else(|| self.err("short \\u escape"))?;
        let text = std::str::from_utf8(digits).map_err(|_| self.err("bad \\u escape"))?;
        let v = u32::from_str_radix(text, 16).map_err(|_| self.err("bad \\u escape"))?;
        self.i += 4;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = r#"{"schema":1,"tool":"hft-exchange xtask 0.1.0","source":"local","createdAt":1791126808341,
        "repo":"https://github.com/TepyThai/hft-exchange","sha":"fe4938fe6e761a7ea1b6eccca91c5e65265c8e8d","branch":"reference","dirty":false,
        "machine":{"cpu":"AMD EPYC 7543P 32-Core Processor","cores":64,"os":"linux","rustc":"rustc 1.97.1 (8bab26f4f 2026-07-14)"},
        "buildError":null,
        "tests":[{"week":6,"passed":8,"failed":0,"total":8,"failures":[]},{"week":7,"passed":7,"failed":1,"total":8,"failures":["w07_partial_fill"]}],
        "benches":[
          {"week":6,"id":"w06.add_cancel","label":"Add then cancel, 100k resting orders","unit":"ns","value":41.000,"p50":41.000,"p99":63.000,"p999":137.000,"max":2400.000,"samples":1000000},
          {"week":16,"id":"w16.fanout","label":"Broadcast | 8 readers","unit":"msgs_per_sec","value":37123456.789,"p50":null,"p99":null,"p999":null,"max":null,"samples":0},
          {"week":24,"id":"w24.e2e_latency","label":"Loopback order to ack","unit":"us","value":12.5,"p50":12.5,"p99":48.25,"p999":1234.5,"max":5000.0,"samples":20000},
          {"week":29,"id":"w29.depth_sum_speedup","label":"AVX2 vs scalar","unit":"ratio","value":2.3456,"p50":null,"p99":null,"p999":null,"max":null,"samples":5},
          {"week":40,"id":"w40.sprint","error":"thread 'main' panicked: not yet implemented: w40"}
        ]}"#;

    const GOLDEN: &str = "# Benchmark report

| | |
| --- | --- |
| Commit | `fe4938fe6e76` on `reference` · [source](https://github.com/TepyThai/hft-exchange/tree/fe4938fe6e761a7ea1b6eccca91c5e65265c8e8d) |
| Machine | AMD EPYC 7543P 32-Core Processor · 64 cores · linux |
| Toolchain | rustc 1.97.1 (8bab26f4f 2026-07-14) |
| Run | 2026-10-04 15:13 UTC · local · hft-exchange xtask 0.1.0 |

Benchmarks depend on the machine, its load and its tuning: compare runs from the same machine and profile only.

## Tests

| Week | Passed | Failed |
| ---: | ---: | --- |
| 6 | 8 | 0 |
| 7 | 7 | 1 (`w07_partial_fill`) |
| **all** | **15** | **1** |

## Latency

| Week | Id | Benchmark | p50 | p99 | p99.9 | max | samples |
| ---: | --- | --- | ---: | ---: | ---: | ---: | ---: |
| 6 | `w06.add_cancel` | Add then cancel, 100k resting orders | 41 ns | 63 ns | 137 ns | 2.40 µs | 1,000,000 |
| 24 | `w24.e2e_latency` | Loopback order to ack | 12.5 µs | 48.2 µs | 1.23 ms | 5.00 ms | 20,000 |

## Throughput and ratios

| Week | Id | Benchmark | Result |
| ---: | --- | --- | ---: |
| 16 | `w16.fanout` | Broadcast \\| 8 readers | 37.1 M msgs/s |
| 29 | `w29.depth_sum_speedup` | AVX2 vs scalar | 2.34× |

## Did not run

| Week | Id | Error |
| ---: | --- | --- |
| 40 | `w40.sprint` | thread 'main' panicked: not yet implemented: w40 |
";

    #[test]
    fn renders_a_fixed_report_byte_for_byte() {
        let md = render(REPORT).unwrap();
        assert_eq!(md, GOLDEN, "\n--- got ---\n{md}");
        assert_eq!(render(REPORT).unwrap(), md, "deterministic");
    }

    #[test]
    fn handles_missing_fields_and_refuses_bad_input() {
        let md = render(r#"{"schema":1,"sha":null,"machine":null,"buildError":"error[E0425]: x","tests":[],"benches":[]}"#).unwrap();
        assert!(md.contains("| Commit | `unknown` |"), "{md}");
        assert!(
            md.contains("| Machine | unknown CPU · ? cores · unknown OS |"),
            "{md}"
        );
        assert!(
            md.contains("**Build failed:**\n\n```text\nerror[E0425]: x\n```"),
            "{md}"
        );
        assert!(
            md.ends_with("No benchmarks in this report (`--no-bench`).\n"),
            "{md}"
        );
        assert!(render(r#"{"schema":2}"#).unwrap_err().contains("schema"));
        assert!(render("{\"schema\":1,").is_err());
        assert!(render("[1, 2] x").is_err());
    }

    #[test]
    fn formats_three_significant_figures_truncated() {
        let cases = [
            (850.0, "ns", "850 ns"),
            (3.45, "ns", "3.4 ns"),
            (1.0, "ns", "1 ns"),
            (1_999.0, "ns", "1.99 µs"),
            (12_345.0, "ns", "12.3 µs"),
            (123_456.0, "ns", "123 µs"),
            (1_500_000_000.0, "ns", "1.50 s"),
            (3_600_000_000_000.0, "ns", "3600 s"),
            (1.15, "us", "1.15 µs"),
            (999.0, "ops_per_sec", "999 ops/s"),
            (20_000_000.0, "msgs_per_sec", "20.0 M msgs/s"),
            (1_234.0, "ops_per_sec", "1.23 k ops/s"),
            (2.0, "ratio", "2.00×"),
            (1_536.0, "bytes", "1.50 KiB"),
            (512.0, "bytes", "512 B"),
        ];
        for (v, unit, want) in cases {
            assert_eq!(fmt_value(v, unit), want, "{v} {unit}");
        }
        assert_eq!(group(1_234_567), "1,234,567");
        assert_eq!(group(999), "999");
        assert_eq!(utc_minutes(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc_minutes(951_782_400_000), "2000-02-29 00:00 UTC");
    }

    #[test]
    fn json_reader() {
        let v =
            Json::parse(r#" {"a":[1,-2.5e3,true,null],"s":"q\"\\\/\n\u00e9\ud83d\ude00","e":{}} "#)
                .unwrap();
        assert_eq!(
            v.get("a").unwrap().as_array().unwrap()[1],
            Json::Num(-2500.0)
        );
        assert_eq!(v.get("s").unwrap().as_str(), Some("q\"\\/\né😀"));
        assert_eq!(v.get("e"), Some(&Json::Obj(Vec::new())));
        for bad in [
            "",
            "{",
            "[1,]",
            "{\"a\" 1}",
            "01x",
            "\"\\x\"",
            "\"\\ud83d\"",
            "-",
            "1.",
            "tru",
            "\"a\nb\"",
        ] {
            assert!(Json::parse(bad).is_err(), "{bad:?} should not parse");
        }
        assert!(Json::parse(&"[".repeat(100))
            .unwrap_err()
            .contains("deeply"));
    }
}
