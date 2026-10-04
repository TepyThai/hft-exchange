//! Week 22: typed configuration — the sample file, every error class at
//! once, environment overrides, and round-tripping the defaults.

use gateway::config::{
    AccountSettings, ConfigError, ConfigErrorKind, Fsync, Origin, RULE_ACCOUNT_BURST, RULE_BURST,
    RULE_INSTRUMENTS, RULE_REFERENCE_TICK,
};
use gateway::toml::{self, Value};
use gateway::{Auth, EngineIdle, ExchangeConfig};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use types::{AccountId, InstrumentId, Price, Rng};

fn repo_file(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(name)
}

fn errors(text: &str) -> Vec<ConfigError> {
    match ExchangeConfig::parse(text) {
        Ok(c) => panic!("expected errors, parsed {c:?}"),
        Err(e) => e.0,
    }
}

/// `(line, short kind)` for compact assertions.
fn summary(errs: &[ConfigError]) -> Vec<(Option<usize>, String)> {
    errs.iter()
        .map(|e| {
            let k = match &e.kind {
                ConfigErrorKind::Io(_) => "io".to_string(),
                ConfigErrorKind::Syntax(_) => "syntax".into(),
                ConfigErrorKind::UnknownSection(s) => format!("unknown-section {s}"),
                ConfigErrorKind::DuplicateSection {
                    section,
                    first_line,
                } => format!("dup-section {section}@{first_line}"),
                ConfigErrorKind::UnknownKey { section, key } => {
                    format!("unknown-key {section}.{key}")
                }
                ConfigErrorKind::DuplicateKey {
                    section,
                    key,
                    first_line,
                } => format!("dup-key {section}.{key}@{first_line}"),
                ConfigErrorKind::WrongType { section, key, .. } => format!("type {section}.{key}"),
                ConfigErrorKind::OutOfRange { section, key, .. } => {
                    format!("range {section}.{key}")
                }
                ConfigErrorKind::Missing { section, key } => format!("missing {section}.{key}"),
                ConfigErrorKind::DuplicateInstrument { id, first_line } => {
                    format!("dup-instrument {id}@{first_line}")
                }
                ConfigErrorKind::DuplicateSymbol { symbol, first_line } => {
                    format!("dup-symbol {symbol}@{first_line}")
                }
                ConfigErrorKind::DuplicateAccount { id, first_line } => {
                    format!("dup-account {id}@{first_line}")
                }
                ConfigErrorKind::Rule(r) => format!("rule {r}"),
            };
            (e.line(), k)
        })
        .collect()
}

const MINIMAL: &str = "[journal]\npath = \"journal.bin\"\n\n[[instrument]]\nid = 1\nsymbol = \"TEST\"\ntick = 1\nlot = 1\n";

#[test]
fn w22_sample_config_parses() {
    let text = std::fs::read_to_string(repo_file("exchange.toml"))
        .expect("exchange.toml at the repo root");
    let c = ExchangeConfig::parse(&text).unwrap_or_else(|e| panic!("sample config:\n{e}"));
    assert_eq!(
        c.gateway.listen,
        "0.0.0.0:9000".parse::<SocketAddr>().unwrap()
    );
    assert_eq!(
        (c.gateway.send_buffer, c.gateway.recv_buffer),
        (65_536, 131_072)
    );
    assert!(c.engine.busy_poll && c.engine.cpu.is_none());
    assert_eq!(c.journal.fsync, Fsync::Always);
    assert_eq!(c.throttle.session_rate, 5_000);
    let symbols: Vec<&str> = c.instruments.iter().map(|i| i.symbol.as_str()).collect();
    assert_eq!(symbols, ["BTC-USD", "ETH-USD"]);
    assert_eq!(
        c.accounts,
        vec![
            AccountSettings { id: 1, token: 1001 },
            AccountSettings { id: 2, token: 1002 }
        ]
    );

    // The typed config drives the gateway.
    let g = c.gateway_config();
    assert_eq!(g.instruments.len(), 2);
    assert_eq!(g.instruments[1].spec.id, InstrumentId(2));
    assert_eq!(
        (g.instruments[1].spec.tick, g.instruments[1].spec.lot),
        (5, 10)
    );
    assert_eq!(g.instruments[1].reference_price, Some(Price(300_000)));
    assert_eq!(
        g.auth,
        Auth::Tokens(vec![(AccountId(1), 1001), (AccountId(2), 1002)])
    );
    assert_eq!(g.engine_idle, EngineIdle::BusyPoll);
    let tuning = g.tuning.expect("buffer sizes configure socket tuning");
    assert_eq!(
        (tuning.send_buffer, tuning.recv_buffer),
        (Some(65_536), Some(131_072))
    );
    assert_eq!(c.limits().max_order_qty.0, 100_000);

    // Effective config: parses back to the same value, same hash.
    let again = ExchangeConfig::parse(&c.to_toml()).unwrap();
    assert_eq!(again, c);
    assert_eq!(again.hash(), c.hash());

    // The container image packages this binary and this file.
    let docker = std::fs::read_to_string(repo_file("Dockerfile")).expect("Dockerfile");
    assert!(docker.contains("--bin exchange") && docker.contains("COPY exchange.toml"));
    assert!(docker.matches("FROM ").count() >= 2, "multi-stage build");
    assert!(repo_file(".dockerignore").exists());
}

#[test]
fn w22_every_error_class_is_reported_at_once() {
    let text = "\
[gateway]
listen = \"localhost\"
io_threads = 0
cancel_on_disconnect = \"yes\"
fsnyc = 1
io_threads = 4
[gateway]
[journal]
fsync = \"sometimes\"
[metrics]
port = 9100
[[instrument]]
id = 1
symbol = \"AAA\"
tick = 5
lot = 1
reference_price = 101
[[instrument]]
id = 1
symbol = \"AAA\"
tick = 1
lot = 1
[[account]]
id = 3
token = 1
[[account]]
id = 3
";
    let got = summary(&errors(text));
    let want: Vec<(Option<usize>, String)> = vec![
        (Some(2), "range gateway.listen".into()),
        (Some(3), "range gateway.io_threads".into()),
        (Some(4), "type gateway.cancel_on_disconnect".into()),
        (Some(5), "unknown-key gateway.fsnyc".into()),
        (Some(6), "dup-key gateway.io_threads@3".into()),
        (Some(7), "dup-section gateway@1".into()),
        (Some(8), "missing journal.path".into()),
        (Some(9), "range journal.fsync".into()),
        (Some(10), "unknown-section metrics".into()),
        (Some(17), format!("rule {RULE_REFERENCE_TICK}")),
        (Some(19), "dup-instrument 1@13".into()),
        (Some(20), "dup-symbol AAA@14".into()),
        (Some(26), "missing account.token".into()),
        (Some(27), "dup-account 3@24".into()),
    ];
    // `port` sits in the unknown [metrics] section: one error, not two.
    assert_eq!(got, want);

    // The rendered messages carry what an operator needs.
    let msg = ExchangeConfig::parse(text).unwrap_err().to_string();
    assert!(
        msg.contains("line 5: unknown key `fsnyc` in [gateway]"),
        "{msg}"
    );
    assert!(
        msg.contains("line 3: `io_threads` in [gateway] is 0; allowed: 1..=256"),
        "{msg}"
    );
}

#[test]
fn w22_no_cascades_and_cross_field_rules() {
    // No instrument at all.
    let got = summary(&errors("[journal]\npath = \"j\"\n"));
    assert_eq!(got, vec![(None, format!("rule {RULE_INSTRUMENTS}"))]);

    // A syntax error: reported with its line, and the key on that line is
    // not reported again as missing.
    let text = MINIMAL
        .replace("lot = 1", "lot = = 1")
        .replace("path = \"journal.bin\"", "path = \"journal.bin");
    let got = summary(&errors(&text));
    assert_eq!(
        got,
        vec![(Some(2), "syntax".into()), (Some(8), "syntax".into())]
    );

    // Throttle rules fire only when their inputs parsed cleanly.
    let with = |extra: &str| format!("{MINIMAL}\n[throttle]\n{extra}");
    let got = summary(&errors(&with("session_rate = 10\nsession_burst = 0\n")));
    assert_eq!(got, vec![(Some(12), format!("rule {RULE_BURST}"))]);
    let got = summary(&errors(&with("session_burst = 50\naccount_burst = 20\n")));
    assert_eq!(got, vec![(Some(12), format!("rule {RULE_ACCOUNT_BURST}"))]);
    let got = summary(&errors(&with(
        "session_burst = \"lots\"\naccount_burst = 20\n",
    )));
    assert_eq!(
        got,
        vec![(Some(11), "type throttle.session_burst".into())],
        "no rule cascade"
    );
    // A tick that failed to parse leaves the reference price unchecked.
    let got = summary(&errors(
        &MINIMAL.replace("tick = 1", "tick = -5\nreference_price = 7"),
    ));
    assert_eq!(got, vec![(Some(7), "range instrument.tick".into())]);
    // Rate 0 means unlimited, so a zero burst is fine there.
    assert!(ExchangeConfig::parse(&with("session_rate = 0\nsession_burst = 0\n")).is_ok());
}

#[test]
fn w22_environment_overrides() {
    let env = [
        ("HFT_GATEWAY_LISTEN", "127.0.0.1:9100"),
        ("HFT_JOURNAL_FSYNC", "interval:5"),
        ("HFT_ENGINE_CPU", "3"),              // a key the file does not set
        ("HFT_MARKETDATA_CONFLATE", "false"), // a section the file does not have
        ("HFT_RISK_MAX_ORDER_QTY", "2_500"),
        ("HFT_BENCH_CPUS", "1,2"), // not a config section: ignored
        ("PATH", "/usr/bin"),
    ];
    let c = ExchangeConfig::parse_with_env(MINIMAL, env).unwrap();
    assert_eq!(
        c.gateway.listen,
        "127.0.0.1:9100".parse::<SocketAddr>().unwrap()
    );
    assert_eq!(c.journal.fsync, Fsync::IntervalMs(5));
    assert_eq!(c.engine.cpu, Some(3));
    assert!(!c.marketdata.conflate);
    assert_eq!(c.risk.max_order_qty, 2_500);

    // Environment beats the file.
    let file = MINIMAL.replace("[journal]\n", "[journal]\nfsync = \"never\"\n");
    let c = ExchangeConfig::parse_with_env(&file, [("HFT_JOURNAL_FSYNC", "always")]).unwrap();
    assert_eq!(c.journal.fsync, Fsync::Always);

    // Bad overrides are errors that name the variable.
    let errs = ExchangeConfig::parse_with_env(
        MINIMAL,
        [
            ("HFT_GATEWAY_LISTN", "x"),
            ("HFT_GATEWAY_IO_THREADS", "four"),
            ("HFT_GATEWAY_IO_THREADS", "999"),
        ],
    )
    .unwrap_err()
    .0;
    let origins: Vec<(&Origin, String)> = errs.iter().map(|e| (&e.origin, e.to_string())).collect();
    assert_eq!(errs.len(), 3, "{origins:?}");
    let threads = Origin::Env("HFT_GATEWAY_IO_THREADS".into());
    assert!(errs
        .iter()
        .any(|e| e.origin == Origin::Env("HFT_GATEWAY_LISTN".into())
            && matches!(&e.kind, ConfigErrorKind::UnknownKey { key, .. } if key == "listn")));
    assert!(errs
        .iter()
        .any(|e| e.origin == threads && matches!(e.kind, ConfigErrorKind::WrongType { .. })));
    assert!(errs.iter().any(|e| e.origin == threads
        && matches!(&e.kind, ConfigErrorKind::OutOfRange { key, value, .. } if key == "io_threads" && value == "999")));
}

#[test]
fn w22_defaults_round_trip() {
    // The defaults render to a file that parses back to the defaults.
    let d = ExchangeConfig::default();
    assert_eq!(ExchangeConfig::parse(&d.to_toml()).unwrap(), d);
    // A file with only the required keys gets exactly the defaults.
    assert_eq!(ExchangeConfig::parse(MINIMAL).unwrap(), d);
    assert_eq!(ExchangeConfig::parse(MINIMAL).unwrap().hash(), d.hash());

    // Random valid configurations survive the round trip, and the hash
    // tells them apart.
    let mut rng = Rng::new(22);
    let mut hashes = std::collections::HashSet::new();
    for _ in 0..200 {
        let mut c = ExchangeConfig::default();
        c.gateway.io_threads = rng.range(1, 64) as usize;
        c.gateway.listen = SocketAddr::from((
            [127, 0, 0, rng.range(1, 9) as u8],
            rng.range(1, 65535) as u16,
        ));
        c.gateway.cancel_on_disconnect = rng.chance(0.5);
        c.gateway.send_buffer = if rng.chance(0.5) {
            0
        } else {
            rng.range(4096, 1 << 20) as u32
        };
        c.engine.cpu = rng.chance(0.5).then(|| rng.range(0, 63) as usize);
        c.risk.max_notional = rng.range(1, 1 << 50);
        c.throttle.session_burst = rng.range(1, 100);
        c.throttle.account_burst = c.throttle.session_burst + rng.range(0, 100);
        c.journal.fsync = match rng.range(0, 2) {
            0 => Fsync::Always,
            1 => Fsync::Never,
            _ => Fsync::IntervalMs(rng.range(1, 60_000)),
        };
        c.journal.path = PathBuf::from(format!("/data/\"j\" {}\\x.bin", rng.range(0, 99)));
        c.instruments = (1..=rng.range(1, 4) as u32)
            .map(|id| {
                let tick = rng.range_i64(1, 10);
                gateway::config::InstrumentSettings {
                    id,
                    symbol: format!("S{id}.X"),
                    tick,
                    lot: rng.range(1, 100),
                    band_ticks: rng.range_i64(0, 1000),
                    reference_price: rng.chance(0.5).then(|| tick * rng.range_i64(-1000, 1000)),
                }
            })
            .collect();
        c.accounts = (1..=rng.range(0, 3) as u32)
            .map(|id| AccountSettings {
                id,
                token: rng.next_u64() >> 1,
            })
            .collect();
        let text = c.to_toml();
        let back = ExchangeConfig::parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        assert_eq!(back, c);
        hashes.insert(c.hash());
    }
    assert_eq!(hashes.len(), 200);
}

#[test]
fn w22_toml_subset_parser() {
    let doc = toml::parse(
        "# comment\n\
         [t]  # trailing comment\n\
         s = \"a\\\"b\\\\c\\n\\u00e9\"\n\
         lit = 'C:\\path'\n\
         n = -1_000\n\
         h = 0xff\n\
         yes = true\n\
         arr = [\n  1,\n  2, # two\n  3,\n]\n\
         nested = [[1, 2], [\"x\"]]\n\
         [[a]]\nk = 1\n[[a]]\nk = 2\n",
    )
    .unwrap();
    let t = doc.table("t").unwrap();
    let v = |k: &str| t.get(k).unwrap().value.clone();
    assert_eq!(v("s"), Value::Str("a\"b\\c\né".into()));
    assert_eq!(v("lit"), Value::Str("C:\\path".into()));
    assert_eq!(
        (v("n"), v("h"), v("yes")),
        (Value::Int(-1000), Value::Int(255), Value::Bool(true))
    );
    assert_eq!(
        v("arr"),
        Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
    );
    assert_eq!(t.get("arr").unwrap().line, 8);
    assert_eq!(
        v("nested"),
        Value::Array(vec![
            Value::Array(vec![Value::Int(1), Value::Int(2)]),
            Value::Array(vec![Value::Str("x".into())])
        ])
    );
    assert_eq!(doc.array("a").count(), 2);

    // Every bad line is reported, with its line number.
    let errs = toml::parse("a = 1.5\nb = \"open\nc = 01\nd = 9223372036854775808\ne = 1__0\nf = yes\ng 1\n[x\nok = 1\n").unwrap_err();
    let lines: Vec<usize> = errs.iter().map(|e| e.line).collect();
    assert_eq!(lines, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    assert!(
        errs[0].to_string().contains("floats are not supported"),
        "{}",
        errs[0]
    );
    assert!(
        errs[5].to_string().contains("strings need quotes"),
        "{}",
        errs[5]
    );
    assert_eq!(toml::parse_int("-9223372036854775808"), Some(i64::MIN));
    assert_eq!(toml::parse_int("1_000_000"), Some(1_000_000));
    assert_eq!(toml::parse_int("_1"), None);
}
