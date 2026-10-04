//! Week-scoped tests, benchmarks and the JSON report uploaded to tepy.dev.

use crate::{Args, Weeks};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Default, Debug, Clone)]
pub struct WeekTests {
    pub passed: u32,
    pub failed: u32,
    pub failures: Vec<String>,
}

pub struct TestRun {
    pub weeks: BTreeMap<u32, WeekTests>,
    pub build_error: Option<String>,
}

fn week_of(name: &str) -> Option<u32> {
    // Last path segment, e.g. `book::tests::w06_cancel_is_o1`.
    let last = name.rsplit("::").next()?;
    let rest = last.strip_prefix('w')?;
    let digits: String = rest.chars().take(2).collect();
    if rest.as_bytes().get(2) != Some(&b'_') {
        return None;
    }
    digits.parse().ok()
}

pub fn run_tests(weeks: Weeks) -> Result<TestRun, String> {
    let mut cmd = Command::new(cargo());
    cmd.args(["test", "--workspace", "--no-fail-fast", "--profile", "xtest", "--"]);
    cmd.args(weeks.test_filters());
    cmd.arg("--test-threads=1");
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    eprintln!("› cargo test --workspace --profile xtest ({})", describe(weeks));
    let out = cmd.output().map_err(|e| format!("failed to run cargo: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    let mut result = TestRun { weeks: BTreeMap::new(), build_error: None };
    for line in stdout.lines() {
        // `test path::w07_name ... ok` / `... FAILED` / `... ignored`
        let Some(rest) = line.strip_prefix("test ") else { continue };
        let Some((name, status)) = rest.rsplit_once(" ... ") else { continue };
        let Some(week) = week_of(name.trim()) else { continue };
        if !weeks.includes(week) {
            continue;
        }
        let entry = result.weeks.entry(week).or_default();
        match status.trim() {
            "ok" => entry.passed += 1,
            s if s.starts_with("FAILED") => {
                entry.failed += 1;
                entry.failures.push(name.trim().rsplit("::").next().unwrap_or(name).to_string());
            }
            _ => {}
        }
    }
    if result.weeks.is_empty() && !out.status.success() && stderr.contains("error[") {
        let msg: Vec<&str> = stderr.lines().filter(|l| l.starts_with("error")).take(10).collect();
        result.build_error = Some(msg.join("\n"));
    }
    Ok(result)
}

fn describe(weeks: Weeks) -> String {
    match weeks {
        Weeks::One(n) => format!("week {n}"),
        Weeks::UpTo(n) => format!("weeks 1–{n}"),
        Weeks::All => "all weeks".into(),
    }
}

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())
}

pub fn cmd_test(args: &Args) -> Result<bool, String> {
    let run = run_tests(args.weeks)?;
    if let Some(e) = &run.build_error {
        eprintln!("build failed:\n{e}");
        return Ok(false);
    }
    if run.weeks.is_empty() {
        println!("no tests found for {}", describe(args.weeks));
        return Ok(true);
    }
    let mut all_ok = true;
    for (week, t) in &run.weeks {
        let total = t.passed + t.failed;
        let mark = if t.failed == 0 { "✓" } else { "✗" };
        println!("{mark} week {week:>2}: {}/{} tests pass", t.passed, total);
        for f in &t.failures {
            println!("    ✗ {f}");
        }
        all_ok &= t.failed == 0;
    }
    Ok(all_ok)
}

// ---------------------------------------------------------------------------
// Benchmarks

#[derive(Debug, Clone)]
pub struct BenchResult {
    pub id: String,
    pub week: u32,
    /// The raw BENCHJSON object (without the prefix), or None on failure.
    pub json: Option<String>,
    pub error: Option<String>,
}

fn bench_crates(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root.join("crates")) {
        for e in entries.flatten() {
            if e.path().join("benches/hft.rs").exists() {
                if let Some(name) = crate_name(&e.path()) {
                    out.push(name);
                }
            }
        }
    }
    out.sort();
    out
}

fn crate_name(dir: &Path) -> Option<String> {
    let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).ok()?;
    manifest.lines().find_map(|l| {
        let l = l.trim();
        l.strip_prefix("name = ").map(|v| v.trim_matches('"').to_string())
    })
}

fn bench_week(id: &str) -> Option<u32> {
    let rest = id.strip_prefix('w')?;
    if rest.as_bytes().get(2) != Some(&b'.') {
        return None;
    }
    rest[..2].parse().ok()
}

fn list_benches(krate: &str) -> Vec<(String, String)> {
    let out = Command::new(cargo())
        .args(["bench", "-q", "-p", krate, "--bench", "hft", "--", "--list"])
        .stderr(Stdio::null())
        .output();
    let Ok(out) = out else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once('\t').map(|(a, b)| (a.to_string(), b.to_string())))
        .collect()
}

pub fn run_benches(weeks: Weeks, filter: Option<&str>) -> Vec<BenchResult> {
    let root = workspace_root();
    let mut results = Vec::new();
    for krate in bench_crates(&root) {
        for (id, _label) in list_benches(&krate) {
            let Some(week) = bench_week(&id) else { continue };
            if !weeks.includes(week) || filter.is_some_and(|f| !id.contains(f)) {
                continue;
            }
            eprintln!("› bench {id}");
            let out = Command::new(cargo())
                .args(["bench", "-q", "-p", &krate, "--bench", "hft", "--", &id])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output();
            let (json, error) = match out {
                Ok(o) => {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    let line = stdout
                        .lines()
                        .filter_map(|l| l.strip_prefix("BENCHJSON "))
                        .find(|j| j.contains(&format!("\"id\":\"{id}\"")));
                    match line {
                        Some(j) => (Some(j.to_string()), None),
                        None => {
                            let stderr = String::from_utf8_lossy(&o.stderr);
                            let msg = stderr
                                .lines()
                                .find(|l| l.contains("panicked") || l.contains("not yet implemented") || l.starts_with("error"))
                                .unwrap_or("benchmark produced no result")
                                .to_string();
                            (None, Some(msg))
                        }
                    }
                }
                Err(e) => (None, Some(e.to_string())),
            };
            results.push(BenchResult { id, week, json, error });
        }
    }
    results
}

pub fn cmd_bench(args: &Args) -> Result<bool, String> {
    let results = run_benches(args.weeks, args.positional.first().map(|s| s.as_str()));
    if results.is_empty() {
        println!("no benchmarks for {}", describe(args.weeks));
        return Ok(true);
    }
    let mut ok = true;
    for r in &results {
        match (&r.json, &r.error) {
            (Some(j), _) => println!("✓ {}  {}", r.id, summarize_bench(j)),
            (None, e) => {
                ok = false;
                println!("✗ {}  {}", r.id, e.clone().unwrap_or_default());
            }
        }
    }
    Ok(ok)
}

/// `value unit (p50 / p99 / p999)` from a BENCHJSON object.
fn summarize_bench(json: &str) -> String {
    let get = |key: &str| -> Option<String> {
        let pat = format!("\"{key}\":");
        let start = json.find(&pat)? + pat.len();
        let rest = &json[start..];
        let end = rest.find([',', '}']).unwrap_or(rest.len());
        Some(rest[..end].trim_matches('"').to_string())
    };
    let unit = get("unit").unwrap_or_default();
    let value = get("value").unwrap_or_default();
    match (get("p99"), get("p999")) {
        (Some(p99), Some(p999)) if p99 != "null" => {
            format!("p50 {value} {unit} · p99 {p99} · p99.9 {p999}")
        }
        _ => format!("{value} {unit}"),
    }
}

// ---------------------------------------------------------------------------
// Report

pub fn workspace_root() -> std::path::PathBuf {
    let here = std::env::var("CARGO_MANIFEST_DIR").map(std::path::PathBuf::from).unwrap_or_else(|_| ".".into());
    here.parent().map(|p| p.to_path_buf()).unwrap_or(here)
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn opt_str(v: Option<String>) -> String {
    v.map(|s| json_str(&s)).unwrap_or_else(|| "null".into())
}

fn cpu_model() -> Option<String> {
    let info = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    info.lines().find_map(|l| l.strip_prefix("model name").map(|r| r.trim_start_matches([' ', '\t', ':']).trim().to_string()))
}

fn repo_url() -> Option<String> {
    if let (Ok(server), Ok(repo)) = (std::env::var("GITHUB_SERVER_URL"), std::env::var("GITHUB_REPOSITORY")) {
        return Some(format!("{server}/{repo}"));
    }
    let url = git(&["remote", "get-url", "origin"])?;
    // git@github.com:user/repo.git → https://github.com/user/repo
    let url = if let Some(rest) = url.strip_prefix("git@") {
        format!("https://{}", rest.replacen(':', "/", 1))
    } else {
        url
    };
    Some(url.trim_end_matches(".git").to_string())
}

pub fn build_report(args: &Args, tests: &TestRun, benches: &[BenchResult]) -> String {
    let source = args.value("source").map(|s| s.to_string()).unwrap_or_else(|| {
        if std::env::var("CI").is_ok() { "ci".into() } else { "local".into() }
    });
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let rustc = Command::new("rustc").arg("--version").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);

    let tests_json: Vec<String> = tests
        .weeks
        .iter()
        .map(|(week, t)| {
            let failures: Vec<String> = t.failures.iter().map(|f| json_str(f)).collect();
            format!(
                "{{\"week\":{week},\"passed\":{},\"failed\":{},\"total\":{},\"failures\":[{}]}}",
                t.passed,
                t.failed,
                t.passed + t.failed,
                failures.join(",")
            )
        })
        .collect();
    let benches_json: Vec<String> = benches
        .iter()
        .map(|b| match &b.json {
            Some(j) => format!("{{\"week\":{},{}", b.week, &j[1..]),
            None => format!(
                "{{\"week\":{},\"id\":{},\"error\":{}}}",
                b.week,
                json_str(&b.id),
                opt_str(b.error.clone())
            ),
        })
        .collect();

    format!(
        concat!(
            "{{\"schema\":1,\"tool\":\"hft-exchange xtask {}\",\"source\":{},\"createdAt\":{},",
            "\"repo\":{},\"sha\":{},\"branch\":{},\"dirty\":{},",
            "\"machine\":{{\"cpu\":{},\"cores\":{},\"os\":{},\"rustc\":{}}},",
            "\"buildError\":{},\"tests\":[{}],\"benches\":[{}]}}"
        ),
        env!("CARGO_PKG_VERSION"),
        json_str(&source),
        created,
        opt_str(repo_url()),
        opt_str(std::env::var("GITHUB_SHA").ok().or_else(|| git(&["rev-parse", "HEAD"]))),
        opt_str(std::env::var("GITHUB_REF_NAME").ok().or_else(|| git(&["rev-parse", "--abbrev-ref", "HEAD"]))),
        git(&["status", "--porcelain"]).map(|s| !s.is_empty()).unwrap_or(false),
        opt_str(cpu_model()),
        cores,
        json_str(std::env::consts::OS),
        opt_str(rustc),
        opt_str(tests.build_error.clone()),
        tests_json.join(","),
        benches_json.join(","),
    )
}

/// Writes rendered Markdown to `path`, or to stdout for `-`.
fn write_markdown(path: &str, report_json: &str) -> Result<(), String> {
    let md = crate::markdown::render(report_json)?;
    if path == "-" {
        print!("{md}");
    } else {
        std::fs::write(path, &md).map_err(|e| format!("writing {path}: {e}"))?;
        println!("markdown → {path}");
    }
    Ok(())
}

pub fn cmd_report(args: &Args) -> Result<bool, String> {
    if let Some(from) = args.value("from") {
        let Some(path) = args.value("markdown") else {
            return Err("--from renders a saved report: add --markdown PATH (or - for stdout)".into());
        };
        let json = std::fs::read_to_string(from).map_err(|e| format!("reading {from}: {e}"))?;
        write_markdown(path, &json)?;
        return Ok(true);
    }
    let tests = run_tests(args.weeks)?;
    let benches = if args.flag("no-bench") { Vec::new() } else { run_benches(args.weeks, None) };
    let report = build_report(args, &tests, &benches);
    let path = args.value("json").unwrap_or("hft-report.json");
    std::fs::write(path, &report).map_err(|e| format!("writing {path}: {e}"))?;

    let passed: u32 = tests.weeks.values().map(|t| t.passed).sum();
    let failed: u32 = tests.weeks.values().map(|t| t.failed).sum();
    let bench_ok = benches.iter().filter(|b| b.json.is_some()).count();
    println!(
        "report → {path}: {passed} tests pass, {failed} fail across {} weeks; {bench_ok}/{} benchmarks ran",
        tests.weeks.len(),
        benches.len()
    );
    if let Some(md) = args.value("markdown") {
        write_markdown(md, &report)?;
    }

    if args.flag("upload") {
        let token = args
            .value("token")
            .map(|s| s.to_string())
            .or_else(|| std::env::var("TEPY_TOKEN").ok())
            .filter(|t| !t.is_empty());
        let Some(token) = token else {
            return Err("--upload needs a token: create one at https://tepy.dev/hft/lab and pass --token or set TEPY_TOKEN".into());
        };
        let endpoint = args.value("endpoint").unwrap_or("https://tepy.dev/api/hft/reports");
        let out = Command::new("curl")
            .args(["-sS", "-X", "POST", "-H", "content-type: application/json", "-H"])
            .arg(format!("authorization: Bearer {token}"))
            .args(["--data-binary", &format!("@{path}"), "-w", "\n%{http_code}", endpoint])
            .output()
            .map_err(|e| format!("could not run curl (needed for --upload): {e}"))?;
        let text = String::from_utf8_lossy(&out.stdout);
        let (body, code) = text.rsplit_once('\n').unwrap_or((&text, ""));
        if code.starts_with('2') {
            println!("uploaded to {endpoint}: {body}");
        } else {
            return Err(format!("upload failed (HTTP {code}): {body}"));
        }
    }
    Ok(tests.build_error.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_week_from_test_name() {
        assert_eq!(week_of("book::tests::w06_cancel_is_o1"), Some(6));
        assert_eq!(week_of("w12_differential"), Some(12));
        assert_eq!(week_of("tests::helper"), None);
        assert_eq!(week_of("tests::w1_bad"), None);
    }

    #[test]
    fn parses_week_from_bench_id() {
        assert_eq!(bench_week("w06.add_cancel"), Some(6));
        assert_eq!(bench_week("w6.add"), None);
    }
}
