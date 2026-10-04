//! Week 22: typed exchange configuration.
//!
//! `exchange.toml` (a sample lives at the repository root) is parsed by the
//! hand-written [`toml`](crate::toml) subset parser, overridden by `HFT_*`
//! environment variables, then validated into an [`ExchangeConfig`]. The
//! rules, in the order the guide gives them:
//!
//! * **Parse into types once.** After [`ExchangeConfig::parse`] returns
//!   `Ok`, nothing re-reads strings or re-checks ranges.
//! * **Unknown keys and sections are errors**, with the line: a misspelled
//!   `fsnyc` must not silently leave the default in place.
//! * **Every error at once.** Syntax errors, unknown and duplicate keys,
//!   wrong types, out-of-range values, missing keys, duplicate instruments
//!   and broken cross-field rules are all collected into one
//!   [`ConfigErrors`].
//! * **No cascades.** A cross-field rule is checked only when every field
//!   it reads parsed cleanly; a missing key is not reported when the file
//!   has syntax errors (the missing line may be the broken one).
//! * **Defaults are visible.** [`ExchangeConfig::to_toml`] renders the
//!   effective configuration with every default filled in, and
//!   [`ExchangeConfig::hash`] fingerprints it for the logs and the journal.
//!
//! # Environment overrides
//!
//! `HFT_<SECTION>_<KEY>=value` overrides `key` in `[section]` (for the
//! single-table sections: gateway, engine, risk, throttle, journal,
//! marketdata), e.g. `HFT_GATEWAY_LISTEN=0.0.0.0:9100` or
//! `HFT_JOURNAL_FSYNC=never`. The value is read as the key's type (no
//! quotes for strings). A variable naming one of those sections with an
//! unknown key is an error; other `HFT_*` variables are ignored.
//! Instruments and accounts are not overridable: they are matching state
//! and belong in the reviewed file.

use crate::hooks::RiskGate;
#[allow(unused_imports)] // used by the implementation the learner writes
use crate::net::SocketTuning;
#[allow(unused_imports)] // used by the implementation the learner writes
use crate::server::{EngineIdle, GatewayConfig};
#[allow(unused_imports)] // used by the implementation the learner writes
use crate::session::Auth;
#[allow(unused_imports)] // used by the implementation the learner writes
use crate::toml::{Document, Entry, ParseErrorKind, Table, Value};
#[allow(unused_imports)] // used by the implementation the learner writes
use engine::EngineConfig;
use risk::{BucketConfig, Limits};
#[allow(unused_imports)] // used by the implementation the learner writes
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
#[allow(unused_imports)] // used by the implementation the learner writes
use std::time::Duration;
#[allow(unused_imports)] // used by the implementation the learner writes
use types::{AccountId, InstrumentId, InstrumentSpec, Price, Qty};

/// When the journal calls `fsync`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fsync {
    /// After every batch, before its reports are released.
    Always,
    /// Never (the OS flushes when it likes): fastest, loses the tail on a
    /// power cut.
    Never,
    /// At most this many milliseconds apart.
    IntervalMs(u64),
}

impl fmt::Display for Fsync {
    /// The config syntax: `always`, `never`, `interval:<ms>`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fsync::Always => f.write_str("always"),
            Fsync::Never => f.write_str("never"),
            Fsync::IntervalMs(ms) => write!(f, "interval:{ms}"),
        }
    }
}

/// `[gateway]`: sessions and sockets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewaySettings {
    /// `listen`: address to accept sessions on. Default `0.0.0.0:9000`.
    pub listen: SocketAddr,
    /// `io_threads`: Tokio workers, 1..=256. Default 2.
    pub io_threads: usize,
    /// `inbound_capacity`: sessions → engine queue, 2..=16 777 216. Default 4096.
    pub inbound_capacity: usize,
    /// `outbound_capacity`: reports queued per session, 1..=1 048 576. Default 4096.
    pub outbound_capacity: usize,
    /// `read_buffer`: bytes per session read, 512..=1 048 576. Default 16384.
    pub read_buffer: usize,
    /// `logon_timeout_ms`: 1..=600 000. Default 5000.
    pub logon_timeout_ms: u64,
    /// `cancel_on_disconnect`. Default true.
    pub cancel_on_disconnect: bool,
    /// `send_buffer`: requested `SO_SNDBUF` in bytes; 0 = kernel default,
    /// else 4096..=67 108 864. Default 0.
    pub send_buffer: u32,
    /// `recv_buffer`: requested `SO_RCVBUF`, as `send_buffer`. Default 0.
    pub recv_buffer: u32,
    /// `vectored_writes`. Default false.
    pub vectored_writes: bool,
}

/// `[engine]`: the matching thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineSettings {
    /// `cpu`: pin the engine thread here, 0..=4095. Default: not pinned.
    pub cpu: Option<usize>,
    /// `busy_poll`: spin when idle (else back off and sleep). Default true.
    pub busy_poll: bool,
}

/// `[risk]`: per-account pre-trade limits (applied to every account).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RiskSettings {
    /// `max_order_qty` in lots, ≥ 1. Default 1 000 000.
    pub max_order_qty: u64,
    /// `max_notional` in ticks × lots, ≥ 1. Default 10¹².
    pub max_notional: u64,
    /// `max_position` in lots per instrument, ≥ 1. Default 10 000 000.
    pub max_position: u64,
    /// `price_band_ticks` around the reference price, 0 = off. Default 0.
    pub price_band_ticks: u64,
}

/// `[throttle]`: token buckets (orders per second; rate 0 = unlimited).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThrottleSettings {
    /// `session_rate`. Default 10 000.
    pub session_rate: u64,
    /// `session_burst`. Default 100.
    pub session_burst: u64,
    /// `account_rate` (all of an account's sessions together). Default 50 000.
    pub account_rate: u64,
    /// `account_burst`. Default 500.
    pub account_burst: u64,
}

/// `[journal]`: the event log (week 18 / 24).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalSettings {
    /// `path` (required).
    pub path: PathBuf,
    /// `fsync`: `always`, `never` or `interval:<ms>` (1..=60 000). Default always.
    pub fsync: Fsync,
}

/// `[marketdata]`: the feed (week 16 / 24).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarketDataSettings {
    /// `depth`: levels per side in snapshots, 1..=1000. Default 10.
    pub depth: usize,
    /// `snapshot_interval`: updates between snapshots, 1..=10⁹. Default 1000.
    pub snapshot_interval: u64,
    /// `subscriber_queue`: updates buffered per subscriber, 16..=1 048 576. Default 4096.
    pub subscriber_queue: usize,
    /// `conflate`: conflate a lagging subscriber's updates. Default true.
    pub conflate: bool,
}

/// One `[[instrument]]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstrumentSettings {
    /// `id` (required), 1..=4 294 967 295, unique.
    pub id: u32,
    /// `symbol` (required): 1–16 of `A-Z 0-9 . _ -`, unique.
    pub symbol: String,
    /// `tick` (required): price increment in ticks' base unit, ≥ 1.
    pub tick: i64,
    /// `lot` (required): quantity increment, ≥ 1.
    pub lot: u64,
    /// `band_ticks`: price band in ticks around the reference, ≥ 0 (0 =
    /// off). Default 0.
    pub band_ticks: i64,
    /// `reference_price`: band reference before the first trade (a
    /// multiple of `tick`). Default: none.
    pub reference_price: Option<i64>,
}

/// One `[[account]]`: a logon credential. No accounts: any logon is
/// accepted (development only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountSettings {
    /// `id` (required), unique.
    pub id: u32,
    /// `token` (required), ≥ 0.
    pub token: u64,
}

/// The whole exchange configuration, validated. Build it with
/// [`ExchangeConfig::parse`] / [`load`](ExchangeConfig::load); the
/// `Default` is the configuration every optional key falls back to (plus
/// one test instrument and a relative journal path).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExchangeConfig {
    /// `[gateway]`.
    pub gateway: GatewaySettings,
    /// `[engine]`.
    pub engine: EngineSettings,
    /// `[risk]`.
    pub risk: RiskSettings,
    /// `[throttle]`.
    pub throttle: ThrottleSettings,
    /// `[journal]`.
    pub journal: JournalSettings,
    /// `[marketdata]`.
    pub marketdata: MarketDataSettings,
    /// `[[instrument]]`, in file order.
    pub instruments: Vec<InstrumentSettings>,
    /// `[[account]]`, in file order.
    pub accounts: Vec<AccountSettings>,
}

impl Default for ExchangeConfig {
    fn default() -> Self {
        ExchangeConfig {
            gateway: GatewaySettings {
                listen: SocketAddr::from(([0, 0, 0, 0], 9000)),
                io_threads: 2,
                inbound_capacity: 4096,
                outbound_capacity: 4096,
                read_buffer: 16 * 1024,
                logon_timeout_ms: 5000,
                cancel_on_disconnect: true,
                send_buffer: 0,
                recv_buffer: 0,
                vectored_writes: false,
            },
            engine: EngineSettings {
                cpu: None,
                busy_poll: true,
            },
            risk: RiskSettings {
                max_order_qty: 1_000_000,
                max_notional: 1_000_000_000_000,
                max_position: 10_000_000,
                price_band_ticks: 0,
            },
            throttle: ThrottleSettings {
                session_rate: 10_000,
                session_burst: 100,
                account_rate: 50_000,
                account_burst: 500,
            },
            journal: JournalSettings {
                path: PathBuf::from("journal.bin"),
                fsync: Fsync::Always,
            },
            marketdata: MarketDataSettings {
                depth: 10,
                snapshot_interval: 1000,
                subscriber_queue: 4096,
                conflate: true,
            },
            instruments: vec![InstrumentSettings {
                id: 1,
                symbol: "TEST".into(),
                tick: 1,
                lot: 1,
                band_ticks: 0,
                reference_price: None,
            }],
            accounts: Vec::new(),
        }
    }
}

/// Where an error was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// A line of the file (1-based).
    Line(usize),
    /// An environment variable.
    Env(String),
    /// The file as a whole (e.g. a required section is absent).
    File,
}

/// One configuration problem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError {
    /// Where.
    pub origin: Origin,
    /// What.
    pub kind: ConfigErrorKind,
}

impl ConfigError {
    /// The line, if the error points into the file.
    pub fn line(&self) -> Option<usize> {
        match self.origin {
            Origin::Line(n) => Some(n),
            _ => None,
        }
    }
}

/// What a [`ConfigError`] is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigErrorKind {
    /// The file could not be read.
    Io(String),
    /// TOML syntax.
    Syntax(String),
    /// A section name the schema does not know.
    UnknownSection(String),
    /// `[section]` given twice (`first_line`: the first).
    DuplicateSection {
        /// The section.
        section: String,
        /// Line of the first header.
        first_line: usize,
    },
    /// A key the section does not have.
    UnknownKey {
        /// The section (`""` for keys before any header).
        section: String,
        /// The key.
        key: String,
    },
    /// A key given twice in one table.
    DuplicateKey {
        /// The section.
        section: String,
        /// The key.
        key: String,
        /// Line of the first occurrence.
        first_line: usize,
    },
    /// The value has the wrong type.
    WrongType {
        /// The section.
        section: String,
        /// The key.
        key: String,
        /// What the key takes.
        expected: &'static str,
        /// What the file has.
        found: &'static str,
    },
    /// The value has the right type but is not allowed.
    OutOfRange {
        /// The section.
        section: String,
        /// The key.
        key: String,
        /// The value as written.
        value: String,
        /// What is allowed.
        allowed: String,
    },
    /// A required key is absent.
    Missing {
        /// The section.
        section: String,
        /// The key.
        key: &'static str,
    },
    /// Two instruments with the same id.
    DuplicateInstrument {
        /// The id.
        id: u32,
        /// Line of the first one.
        first_line: usize,
    },
    /// Two instruments with the same symbol.
    DuplicateSymbol {
        /// The symbol.
        symbol: String,
        /// Line of the first one.
        first_line: usize,
    },
    /// Two accounts with the same id.
    DuplicateAccount {
        /// The id.
        id: u32,
        /// Line of the first one.
        first_line: usize,
    },
    /// A cross-field rule (one of the `RULE_*` constants).
    Rule(&'static str),
}

/// Every instrument needs at least one.
pub const RULE_INSTRUMENTS: &str = "at least one [[instrument]] is required";
/// Instrument reference prices must be on the tick grid.
pub const RULE_REFERENCE_TICK: &str =
    "an instrument's reference_price must be a multiple of its tick";
/// A throttle with a rate needs a burst of at least one order.
pub const RULE_BURST: &str = "a throttle burst must be at least 1 when its rate is set";
/// An account bucket smaller than a session bucket starves the session's burst.
pub const RULE_ACCOUNT_BURST: &str =
    "throttle.account_burst must be at least throttle.session_burst";

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.origin {
            Origin::Line(n) => write!(f, "line {n}: ")?,
            Origin::Env(var) => write!(f, "{var}: ")?,
            Origin::File => {}
        }
        let sect = |s: &str| {
            if s.is_empty() {
                "the top level".to_string()
            } else {
                format!("[{s}]")
            }
        };
        match &self.kind {
            ConfigErrorKind::Io(e) => write!(f, "cannot read the file: {e}"),
            ConfigErrorKind::Syntax(m) => f.write_str(m),
            ConfigErrorKind::UnknownSection(s) => write!(f, "unknown section [{s}]"),
            ConfigErrorKind::DuplicateSection {
                section,
                first_line,
            } => {
                write!(
                    f,
                    "section [{section}] given twice (first on line {first_line})"
                )
            }
            ConfigErrorKind::UnknownKey { section, key } => {
                write!(f, "unknown key `{key}` in {}", sect(section))
            }
            ConfigErrorKind::DuplicateKey {
                section,
                key,
                first_line,
            } => {
                write!(
                    f,
                    "key `{key}` given twice in {} (first on line {first_line})",
                    sect(section)
                )
            }
            ConfigErrorKind::WrongType {
                section,
                key,
                expected,
                found,
            } => {
                write!(
                    f,
                    "`{key}` in {} must be {expected}, found {found}",
                    sect(section)
                )
            }
            ConfigErrorKind::OutOfRange {
                section,
                key,
                value,
                allowed,
            } => {
                write!(
                    f,
                    "`{key}` in {} is {value}; allowed: {allowed}",
                    sect(section)
                )
            }
            ConfigErrorKind::Missing { section, key } => {
                write!(f, "missing required key `{key}` in {}", sect(section))
            }
            ConfigErrorKind::DuplicateInstrument { id, first_line } => {
                write!(
                    f,
                    "instrument id {id} defined twice (first on line {first_line})"
                )
            }
            ConfigErrorKind::DuplicateSymbol { symbol, first_line } => {
                write!(
                    f,
                    "instrument symbol {symbol} defined twice (first on line {first_line})"
                )
            }
            ConfigErrorKind::DuplicateAccount { id, first_line } => {
                write!(f, "account {id} defined twice (first on line {first_line})")
            }
            ConfigErrorKind::Rule(r) => f.write_str(r),
        }
    }
}

/// Every problem found in one pass, in the order found (syntax first).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigErrors(pub Vec<ConfigError>);

impl ConfigErrors {
    /// The errors.
    pub fn errors(&self) -> &[ConfigError] {
        &self.0
    }
}

impl fmt::Display for ConfigErrors {
    /// One error per line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, e) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{e}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigErrors {}

/// Prefix of environment overrides.
pub const ENV_PREFIX: &str = "HFT_";

// @student w22
impl ExchangeConfig {
    /// Parses and validates `text` (no environment overrides).
    pub fn parse(text: &str) -> Result<ExchangeConfig, ConfigErrors> {
        ExchangeConfig::parse_with_env(text, std::iter::empty::<(String, String)>())
    }

    /// Parses `text`, applies the `HFT_*` overrides among `env` (pairs of
    /// variable name and value; other variables are ignored), and
    /// validates the result. Collects every error. O(file size + env).
    pub fn parse_with_env<I, K, V>(text: &str, env: I) -> Result<ExchangeConfig, ConfigErrors>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let (mut doc, syntax) = crate::toml::parse_partial(text);
        let mut v = Validator {
            errors: Vec::new(),
            env_origin: HashMap::new(),
            syntax_errors: syntax
                .iter()
                .any(|e| matches!(e.kind, ParseErrorKind::Syntax(_))),
        };
        for e in syntax {
            let kind = match e.kind {
                ParseErrorKind::Syntax(m) => ConfigErrorKind::Syntax(m),
                ParseErrorKind::DuplicateKey {
                    table,
                    key,
                    first_line,
                } => ConfigErrorKind::DuplicateKey {
                    section: table,
                    key,
                    first_line,
                },
                ParseErrorKind::DuplicateTable { name, first_line } => {
                    ConfigErrorKind::DuplicateSection {
                        section: name,
                        first_line,
                    }
                }
            };
            v.error(Origin::Line(e.line), kind);
        }
        for (name, value) in env {
            v.apply_env(&mut doc, name.as_ref(), value.as_ref());
        }
        let config = v.validate(&doc);
        // File errors in line order, then environment and whole-file ones.
        v.errors.sort_by_key(|e| match e.origin {
            Origin::Line(n) => (0, n),
            Origin::Env(_) => (1, 0),
            Origin::File => (2, 0),
        });
        if v.errors.is_empty() {
            Ok(config)
        } else {
            Err(ConfigErrors(v.errors))
        }
    }

    /// Reads `path` and parses it with the process environment's `HFT_*`
    /// overrides.
    pub fn load(path: &Path) -> Result<ExchangeConfig, ConfigErrors> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            ConfigErrors(vec![ConfigError {
                origin: Origin::File,
                kind: ConfigErrorKind::Io(format!("{}: {e}", path.display())),
            }])
        })?;
        ExchangeConfig::parse_with_env(&text, std::env::vars())
    }

    /// The effective configuration as TOML, every key explicit, in a fixed
    /// order: parsing it gives back `self`. O(size).
    pub fn to_toml(&self) -> String {
        let mut out = String::new();
        let kv = |out: &mut String, k: &str, v: Value| {
            out.push_str(&format!("{k} = {v}\n"));
        };
        let int = |n: u64| Value::Int(n as i64);
        let g = &self.gateway;
        out.push_str("[gateway]\n");
        kv(&mut out, "listen", Value::Str(g.listen.to_string()));
        kv(&mut out, "io_threads", int(g.io_threads as u64));
        kv(&mut out, "inbound_capacity", int(g.inbound_capacity as u64));
        kv(
            &mut out,
            "outbound_capacity",
            int(g.outbound_capacity as u64),
        );
        kv(&mut out, "read_buffer", int(g.read_buffer as u64));
        kv(&mut out, "logon_timeout_ms", int(g.logon_timeout_ms));
        kv(
            &mut out,
            "cancel_on_disconnect",
            Value::Bool(g.cancel_on_disconnect),
        );
        kv(&mut out, "send_buffer", int(g.send_buffer as u64));
        kv(&mut out, "recv_buffer", int(g.recv_buffer as u64));
        kv(&mut out, "vectored_writes", Value::Bool(g.vectored_writes));
        out.push_str("\n[engine]\n");
        if let Some(cpu) = self.engine.cpu {
            kv(&mut out, "cpu", int(cpu as u64));
        }
        kv(&mut out, "busy_poll", Value::Bool(self.engine.busy_poll));
        let r = &self.risk;
        out.push_str("\n[risk]\n");
        kv(&mut out, "max_order_qty", int(r.max_order_qty));
        kv(&mut out, "max_notional", int(r.max_notional));
        kv(&mut out, "max_position", int(r.max_position));
        kv(&mut out, "price_band_ticks", int(r.price_band_ticks));
        let t = &self.throttle;
        out.push_str("\n[throttle]\n");
        kv(&mut out, "session_rate", int(t.session_rate));
        kv(&mut out, "session_burst", int(t.session_burst));
        kv(&mut out, "account_rate", int(t.account_rate));
        kv(&mut out, "account_burst", int(t.account_burst));
        out.push_str("\n[journal]\n");
        kv(
            &mut out,
            "path",
            Value::Str(self.journal.path.to_string_lossy().into_owned()),
        );
        kv(
            &mut out,
            "fsync",
            Value::Str(self.journal.fsync.to_string()),
        );
        let m = &self.marketdata;
        out.push_str("\n[marketdata]\n");
        kv(&mut out, "depth", int(m.depth as u64));
        kv(&mut out, "snapshot_interval", int(m.snapshot_interval));
        kv(&mut out, "subscriber_queue", int(m.subscriber_queue as u64));
        kv(&mut out, "conflate", Value::Bool(m.conflate));
        for i in &self.instruments {
            out.push_str("\n[[instrument]]\n");
            kv(&mut out, "id", int(i.id as u64));
            kv(&mut out, "symbol", Value::Str(i.symbol.clone()));
            kv(&mut out, "tick", Value::Int(i.tick));
            kv(&mut out, "lot", int(i.lot));
            kv(&mut out, "band_ticks", Value::Int(i.band_ticks));
            if let Some(p) = i.reference_price {
                kv(&mut out, "reference_price", Value::Int(p));
            }
        }
        for a in &self.accounts {
            out.push_str("\n[[account]]\n");
            kv(&mut out, "id", int(a.id as u64));
            kv(&mut out, "token", int(a.token));
        }
        out
    }

    /// FNV-1a 64 of [`to_toml`](Self::to_toml): equal configurations hash
    /// equal on every machine and every run. Log it at startup; journal it
    /// with the matching-relevant values.
    pub fn hash(&self) -> u64 {
        self.to_toml()
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
                (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
            })
    }

    /// The gateway's runtime configuration.
    pub fn gateway_config(&self) -> GatewayConfig {
        let g = &self.gateway;
        let tuning = (g.send_buffer > 0 || g.recv_buffer > 0).then(|| SocketTuning {
            recv_buffer: (g.recv_buffer > 0).then_some(g.recv_buffer),
            send_buffer: (g.send_buffer > 0).then_some(g.send_buffer),
            ..SocketTuning::latency()
        });
        GatewayConfig {
            listen: g.listen,
            instruments: self
                .instruments
                .iter()
                .map(|i| EngineConfig {
                    reference_price: i.reference_price.map(Price),
                    ..EngineConfig::new(InstrumentSpec {
                        id: InstrumentId(i.id),
                        tick: i.tick,
                        lot: i.lot,
                        band_ticks: i.band_ticks,
                    })
                })
                .collect(),
            auth: if self.accounts.is_empty() {
                Auth::Open
            } else {
                Auth::Tokens(
                    self.accounts
                        .iter()
                        .map(|a| (AccountId(a.id), a.token))
                        .collect(),
                )
            },
            io_threads: g.io_threads,
            inbound_capacity: g.inbound_capacity,
            outbound_capacity: g.outbound_capacity,
            read_buffer: g.read_buffer,
            logon_timeout: Duration::from_millis(g.logon_timeout_ms),
            cancel_on_disconnect: g.cancel_on_disconnect,
            engine_idle: if self.engine.busy_poll {
                EngineIdle::BusyPoll
            } else {
                EngineIdle::default()
            },
            engine_cpu: self.engine.cpu,
            tuning,
            vectored_writes: g.vectored_writes,
        }
    }

    /// The pre-trade limits for every account.
    pub fn limits(&self) -> Limits {
        Limits {
            max_order_qty: Qty(self.risk.max_order_qty),
            max_notional: self.risk.max_notional as u128,
            max_position: Qty(self.risk.max_position),
            price_band_ticks: self.risk.price_band_ticks,
        }
    }

    /// `(per_session, per_account)` throttle buckets.
    pub fn buckets(&self) -> (BucketConfig, BucketConfig) {
        let bucket = |rate: u64, burst: u64| {
            if rate == 0 {
                BucketConfig::UNLIMITED
            } else {
                BucketConfig::per_second(rate, burst)
            }
        };
        let t = &self.throttle;
        (
            bucket(t.session_rate, t.session_burst),
            bucket(t.account_rate, t.account_burst),
        )
    }

    /// The week-17 pre-trade gate for these limits and throttles.
    pub fn risk_gate(&self) -> RiskGate {
        let (session, account) = self.buckets();
        RiskGate::new(self.limits(), session, account)
    }
}

// @student-begin w22
/// The single-table sections and their keys (for unknown-key detection and
/// the type of environment overrides).
const SECTIONS: &[(&str, &[(&str, Kind)])] = &[
    (
        "gateway",
        &[
            ("listen", Kind::Str),
            ("io_threads", Kind::Int),
            ("inbound_capacity", Kind::Int),
            ("outbound_capacity", Kind::Int),
            ("read_buffer", Kind::Int),
            ("logon_timeout_ms", Kind::Int),
            ("cancel_on_disconnect", Kind::Bool),
            ("send_buffer", Kind::Int),
            ("recv_buffer", Kind::Int),
            ("vectored_writes", Kind::Bool),
        ],
    ),
    ("engine", &[("cpu", Kind::Int), ("busy_poll", Kind::Bool)]),
    (
        "risk",
        &[
            ("max_order_qty", Kind::Int),
            ("max_notional", Kind::Int),
            ("max_position", Kind::Int),
            ("price_band_ticks", Kind::Int),
        ],
    ),
    (
        "throttle",
        &[
            ("session_rate", Kind::Int),
            ("session_burst", Kind::Int),
            ("account_rate", Kind::Int),
            ("account_burst", Kind::Int),
        ],
    ),
    ("journal", &[("path", Kind::Str), ("fsync", Kind::Str)]),
    (
        "marketdata",
        &[
            ("depth", Kind::Int),
            ("snapshot_interval", Kind::Int),
            ("subscriber_queue", Kind::Int),
            ("conflate", Kind::Bool),
        ],
    ),
];

const INSTRUMENT_KEYS: &[&str] = &[
    "id",
    "symbol",
    "tick",
    "lot",
    "band_ticks",
    "reference_price",
];
const ACCOUNT_KEYS: &[&str] = &["id", "token"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Str,
    Int,
    Bool,
}

struct Validator {
    errors: Vec<ConfigError>,
    /// `(section, key)` → the variable that set it.
    env_origin: HashMap<(String, String), String>,
    /// Missing keys are not reported when lines failed to parse.
    syntax_errors: bool,
}

/// Reads one table's fields, recording errors and which keys failed.
struct Fields<'a> {
    section: &'static str,
    table: Option<&'a Table>,
    failed: HashSet<&'static str>,
}

impl Validator {
    fn error(&mut self, origin: Origin, kind: ConfigErrorKind) {
        self.errors.push(ConfigError { origin, kind });
    }

    fn origin(&self, section: &str, entry: &Entry) -> Origin {
        match self
            .env_origin
            .get(&(section.to_string(), entry.key.clone()))
        {
            Some(var) => Origin::Env(var.clone()),
            None => Origin::Line(entry.line),
        }
    }

    fn apply_env(&mut self, doc: &mut Document, name: &str, raw: &str) {
        let Some(rest) = name.strip_prefix(ENV_PREFIX) else {
            return;
        };
        let Some((section, keys)) = SECTIONS.iter().find(|(s, _)| {
            rest.get(..s.len())
                .is_some_and(|p| p.eq_ignore_ascii_case(s))
                && rest.as_bytes().get(s.len()) == Some(&b'_')
        }) else {
            return;
        };
        let key = rest[section.len() + 1..].to_ascii_lowercase();
        let origin = Origin::Env(name.to_string());
        let Some(&(_, kind)) = keys.iter().find(|(k, _)| *k == key) else {
            return self.error(
                origin,
                ConfigErrorKind::UnknownKey {
                    section: section.to_string(),
                    key,
                },
            );
        };
        let value = match kind {
            Kind::Str => Value::Str(raw.to_string()),
            Kind::Int => match crate::toml::parse_int(raw.trim()) {
                Some(n) => Value::Int(n),
                None => {
                    return self.error(
                        origin,
                        ConfigErrorKind::WrongType {
                            section: section.to_string(),
                            key,
                            expected: "an integer",
                            found: "text",
                        },
                    )
                }
            },
            Kind::Bool => match raw.trim() {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => {
                    return self.error(
                        origin,
                        ConfigErrorKind::WrongType {
                            section: section.to_string(),
                            key,
                            expected: "true or false",
                            found: "text",
                        },
                    )
                }
            },
        };
        let table = match doc
            .tables
            .iter()
            .position(|t| t.name == *section && !t.array)
        {
            Some(i) => &mut doc.tables[i],
            None => {
                doc.tables.push(Table {
                    name: section.to_string(),
                    array: false,
                    line: 0,
                    entries: Vec::new(),
                });
                doc.tables.last_mut().expect("just pushed")
            }
        };
        match table.entries.iter_mut().find(|e| e.key == key) {
            Some(e) => e.value = value,
            None => table.entries.push(Entry {
                key: key.clone(),
                value,
                line: 0,
            }),
        }
        self.env_origin
            .insert((section.to_string(), key), name.to_string());
    }

    fn validate(&mut self, doc: &Document) -> ExchangeConfig {
        let mut c = ExchangeConfig::default();
        // Sections the schema does not know, and keys before any header.
        for t in &doc.tables {
            let known = t.name.is_empty()
                || SECTIONS.iter().any(|(s, _)| *s == t.name && !t.array)
                || (t.array && (t.name == "instrument" || t.name == "account"));
            if !known {
                let name = if t.array {
                    format!("[{}]", t.name)
                } else {
                    t.name.clone()
                };
                self.error(Origin::Line(t.line), ConfigErrorKind::UnknownSection(name));
            }
        }
        if let Some(root) = doc.tables.first() {
            for e in &root.entries {
                self.error(
                    Origin::Line(e.line),
                    ConfigErrorKind::UnknownKey {
                        section: String::new(),
                        key: e.key.clone(),
                    },
                );
            }
        }

        let mut f = Fields::new("gateway", doc.table("gateway"));
        let g = &mut c.gateway;
        if let Some(x) = f.get(self, "listen", |v| match v {
            Value::Str(s) => s
                .parse::<SocketAddr>()
                .map_err(|_| Bad::Range("a socket address like \"0.0.0.0:9000\"".into())),
            other => Err(Bad::Type("a string", other.type_name())),
        }) {
            g.listen = x;
        }
        if let Some(x) = f.uint(self, "io_threads", 1, 256) {
            g.io_threads = x as usize;
        }
        if let Some(x) = f.uint(self, "inbound_capacity", 2, 1 << 24) {
            g.inbound_capacity = x as usize;
        }
        if let Some(x) = f.uint(self, "outbound_capacity", 1, 1 << 20) {
            g.outbound_capacity = x as usize;
        }
        if let Some(x) = f.uint(self, "read_buffer", 512, 1 << 20) {
            g.read_buffer = x as usize;
        }
        if let Some(x) = f.uint(self, "logon_timeout_ms", 1, 600_000) {
            g.logon_timeout_ms = x;
        }
        if let Some(x) = f.boolean(self, "cancel_on_disconnect") {
            g.cancel_on_disconnect = x;
        }
        if let Some(x) = f.get(self, "send_buffer", buffer_size) {
            g.send_buffer = x;
        }
        if let Some(x) = f.get(self, "recv_buffer", buffer_size) {
            g.recv_buffer = x;
        }
        if let Some(x) = f.boolean(self, "vectored_writes") {
            g.vectored_writes = x;
        }
        f.finish(self);

        let mut f = Fields::new("engine", doc.table("engine"));
        if let Some(x) = f.uint(self, "cpu", 0, 4095) {
            c.engine.cpu = Some(x as usize);
        }
        if let Some(x) = f.boolean(self, "busy_poll") {
            c.engine.busy_poll = x;
        }
        f.finish(self);

        let mut f = Fields::new("risk", doc.table("risk"));
        let r = &mut c.risk;
        if let Some(x) = f.uint(self, "max_order_qty", 1, i64::MAX as u64) {
            r.max_order_qty = x;
        }
        if let Some(x) = f.uint(self, "max_notional", 1, i64::MAX as u64) {
            r.max_notional = x;
        }
        if let Some(x) = f.uint(self, "max_position", 1, i64::MAX as u64) {
            r.max_position = x;
        }
        if let Some(x) = f.uint(self, "price_band_ticks", 0, i64::MAX as u64) {
            r.price_band_ticks = x;
        }
        f.finish(self);

        let mut f = Fields::new("throttle", doc.table("throttle"));
        let t = &mut c.throttle;
        if let Some(x) = f.uint(self, "session_rate", 0, 1_000_000_000) {
            t.session_rate = x;
        }
        if let Some(x) = f.uint(self, "session_burst", 0, 1_000_000_000) {
            t.session_burst = x;
        }
        if let Some(x) = f.uint(self, "account_rate", 0, 1_000_000_000) {
            t.account_rate = x;
        }
        if let Some(x) = f.uint(self, "account_burst", 0, 1_000_000_000) {
            t.account_burst = x;
        }
        let clean = |keys: &[&str]| keys.iter().all(|k| !f.failed.contains(k));
        if (clean(&["session_rate", "session_burst"]) && t.session_rate > 0 && t.session_burst == 0)
            || (clean(&["account_rate", "account_burst"])
                && t.account_rate > 0
                && t.account_burst == 0)
        {
            let line = f.line_of("session_burst").or(f.line_of("account_burst"));
            self.error(
                line.map_or(Origin::File, Origin::Line),
                ConfigErrorKind::Rule(RULE_BURST),
            );
        } else if clean(&[
            "session_rate",
            "session_burst",
            "account_rate",
            "account_burst",
        ]) && t.session_rate > 0
            && t.account_rate > 0
            && t.account_burst < t.session_burst
        {
            let line = f.line_of("account_burst");
            self.error(
                line.map_or(Origin::File, Origin::Line),
                ConfigErrorKind::Rule(RULE_ACCOUNT_BURST),
            );
        }
        f.finish(self);

        let mut f = Fields::new("journal", doc.table("journal"));
        match f.get(self, "path", |v| match v {
            Value::Str(s) if !s.trim().is_empty() => Ok(PathBuf::from(s)),
            Value::Str(_) => Err(Bad::Range("a non-empty path".into())),
            other => Err(Bad::Type("a string", other.type_name())),
        }) {
            Some(p) => c.journal.path = p,
            None => f.require(self, "path"),
        }
        if let Some(x) = f.get(self, "fsync", |v| match v {
            Value::Str(s) => parse_fsync(s).ok_or_else(|| {
                Bad::Range("always, never or interval:<ms> with ms in 1..=60000".into())
            }),
            other => Err(Bad::Type("a string", other.type_name())),
        }) {
            c.journal.fsync = x;
        }
        f.finish(self);

        let mut f = Fields::new("marketdata", doc.table("marketdata"));
        let m = &mut c.marketdata;
        if let Some(x) = f.uint(self, "depth", 1, 1000) {
            m.depth = x as usize;
        }
        if let Some(x) = f.uint(self, "snapshot_interval", 1, 1_000_000_000) {
            m.snapshot_interval = x;
        }
        if let Some(x) = f.uint(self, "subscriber_queue", 16, 1 << 20) {
            m.subscriber_queue = x as usize;
        }
        if let Some(x) = f.boolean(self, "conflate") {
            m.conflate = x;
        }
        f.finish(self);

        c.instruments = self.instruments(doc);
        c.accounts = self.accounts(doc);
        c
    }

    fn instruments(&mut self, doc: &Document) -> Vec<InstrumentSettings> {
        let mut out = Vec::new();
        let mut ids: HashMap<u32, usize> = HashMap::new();
        let mut symbols: HashMap<String, usize> = HashMap::new();
        let mut any = false;
        for t in doc.array("instrument") {
            any = true;
            let mut f = Fields::new("instrument", Some(t));
            let id = f.uint(self, "id", 1, u32::MAX as u64).map(|x| x as u32);
            let symbol = f.get(self, "symbol", |v| match v {
                Value::Str(s) if valid_symbol(s) => Ok(s.clone()),
                Value::Str(_) => Err(Bad::Range("1 to 16 of A-Z 0-9 . _ -".into())),
                other => Err(Bad::Type("a string", other.type_name())),
            });
            let tick = f.int(self, "tick", 1, i64::MAX);
            let lot = f.uint(self, "lot", 1, i64::MAX as u64);
            let band = f.int(self, "band_ticks", 0, i64::MAX);
            let reference = f.int(self, "reference_price", i64::MIN, i64::MAX);
            for (key, present) in [
                ("id", id.is_some()),
                ("symbol", symbol.is_some()),
                ("tick", tick.is_some()),
                ("lot", lot.is_some()),
            ] {
                if !present {
                    f.require(self, key);
                }
            }
            if let (Some(tick), Some(p)) = (tick, reference) {
                if p % tick != 0 {
                    let line = f.line_of("reference_price").unwrap_or(t.line);
                    self.error(
                        Origin::Line(line),
                        ConfigErrorKind::Rule(RULE_REFERENCE_TICK),
                    );
                }
            }
            if let Some(id) = id {
                if let Some(&first_line) = ids.get(&id) {
                    let line = f.line_of("id").unwrap_or(t.line);
                    self.error(
                        Origin::Line(line),
                        ConfigErrorKind::DuplicateInstrument { id, first_line },
                    );
                } else {
                    ids.insert(id, f.line_of("id").unwrap_or(t.line));
                }
            }
            if let Some(symbol) = &symbol {
                if let Some(&first_line) = symbols.get(symbol) {
                    let line = f.line_of("symbol").unwrap_or(t.line);
                    self.error(
                        Origin::Line(line),
                        ConfigErrorKind::DuplicateSymbol {
                            symbol: symbol.clone(),
                            first_line,
                        },
                    );
                } else {
                    symbols.insert(symbol.clone(), f.line_of("symbol").unwrap_or(t.line));
                }
            }
            f.finish_keys(self, INSTRUMENT_KEYS);
            if let (Some(id), Some(symbol), Some(tick), Some(lot)) = (id, symbol, tick, lot) {
                out.push(InstrumentSettings {
                    id,
                    symbol,
                    tick,
                    lot,
                    band_ticks: band.unwrap_or(0),
                    reference_price: reference,
                });
            }
        }
        if !any && !self.syntax_errors {
            self.error(Origin::File, ConfigErrorKind::Rule(RULE_INSTRUMENTS));
        }
        out
    }

    fn accounts(&mut self, doc: &Document) -> Vec<AccountSettings> {
        let mut out = Vec::new();
        let mut ids: HashMap<u32, usize> = HashMap::new();
        for t in doc.array("account") {
            let mut f = Fields::new("account", Some(t));
            let id = f.uint(self, "id", 1, u32::MAX as u64).map(|x| x as u32);
            let token = f.uint(self, "token", 0, i64::MAX as u64);
            if id.is_none() {
                f.require(self, "id");
            }
            if token.is_none() {
                f.require(self, "token");
            }
            if let Some(id) = id {
                if let Some(&first_line) = ids.get(&id) {
                    let line = f.line_of("id").unwrap_or(t.line);
                    self.error(
                        Origin::Line(line),
                        ConfigErrorKind::DuplicateAccount { id, first_line },
                    );
                } else {
                    ids.insert(id, f.line_of("id").unwrap_or(t.line));
                }
            }
            f.finish_keys(self, ACCOUNT_KEYS);
            if let (Some(id), Some(token)) = (id, token) {
                out.push(AccountSettings { id, token });
            }
        }
        out
    }
}

/// Why a value was refused.
enum Bad {
    /// `(expected, found)` type names.
    Type(&'static str, &'static str),
    /// Right type, not allowed: what is allowed.
    Range(String),
}

impl<'a> Fields<'a> {
    fn new(section: &'static str, table: Option<&'a Table>) -> Fields<'a> {
        Fields {
            section,
            table,
            failed: HashSet::new(),
        }
    }

    fn line_of(&self, key: &str) -> Option<usize> {
        self.table?.get(key).map(|e| e.line).filter(|&l| l > 0)
    }

    /// The converted value of `key`: `None` if absent or refused (refusals
    /// are recorded, and the key is marked failed).
    fn get<T>(
        &mut self,
        v: &mut Validator,
        key: &'static str,
        conv: impl FnOnce(&Value) -> Result<T, Bad>,
    ) -> Option<T> {
        let entry = self.table?.get(key)?;
        match conv(&entry.value) {
            Ok(x) => Some(x),
            Err(bad) => {
                self.failed.insert(key);
                let section = self.section.to_string();
                let kind = match bad {
                    Bad::Type(expected, found) => ConfigErrorKind::WrongType {
                        section,
                        key: key.into(),
                        expected,
                        found,
                    },
                    Bad::Range(allowed) => ConfigErrorKind::OutOfRange {
                        section,
                        key: key.into(),
                        value: entry.value.to_string(),
                        allowed,
                    },
                };
                let origin = v.origin(self.section, entry);
                v.error(origin, kind);
                None
            }
        }
    }

    fn int(&mut self, v: &mut Validator, key: &'static str, lo: i64, hi: i64) -> Option<i64> {
        self.get(v, key, |val| match *val {
            Value::Int(n) if (lo..=hi).contains(&n) => Ok(n),
            Value::Int(_) => Err(Bad::Range(range_text(lo, hi))),
            ref other => Err(Bad::Type("an integer", other.type_name())),
        })
    }

    fn uint(&mut self, v: &mut Validator, key: &'static str, lo: u64, hi: u64) -> Option<u64> {
        self.int(v, key, lo as i64, hi.min(i64::MAX as u64) as i64)
            .map(|n| n as u64)
    }

    fn boolean(&mut self, v: &mut Validator, key: &'static str) -> Option<bool> {
        self.get(v, key, |val| match *val {
            Value::Bool(b) => Ok(b),
            ref other => Err(Bad::Type("true or false", other.type_name())),
        })
    }

    /// Records `key` as missing, unless it failed to convert (already
    /// reported) or lines failed to parse (it may be on one of them).
    fn require(&mut self, v: &mut Validator, key: &'static str) {
        if self.failed.contains(key) || v.syntax_errors {
            return;
        }
        let origin = match self.table {
            Some(t) if t.line > 0 => Origin::Line(t.line),
            _ => Origin::File,
        };
        v.error(
            origin,
            ConfigErrorKind::Missing {
                section: self.section.to_string(),
                key,
            },
        );
    }

    /// Unknown keys of a single-table section.
    fn finish(self, v: &mut Validator) {
        let keys: Vec<&str> = SECTIONS
            .iter()
            .find(|(s, _)| *s == self.section)
            .map(|(_, ks)| ks.iter().map(|(k, _)| *k).collect())
            .unwrap_or_default();
        self.finish_keys(v, &keys);
    }

    fn finish_keys(self, v: &mut Validator, keys: &[&str]) {
        let Some(table) = self.table else { return };
        for e in &table.entries {
            if !keys.contains(&e.key.as_str()) {
                let origin = v.origin(self.section, e);
                v.error(
                    origin,
                    ConfigErrorKind::UnknownKey {
                        section: self.section.to_string(),
                        key: e.key.clone(),
                    },
                );
            }
        }
    }
}

fn range_text(lo: i64, hi: i64) -> String {
    match (lo, hi) {
        (i64::MIN, i64::MAX) => "any integer".into(),
        (lo, i64::MAX) => format!("an integer >= {lo}"),
        (lo, hi) => format!("{lo}..={hi}"),
    }
}

fn buffer_size(v: &Value) -> Result<u32, Bad> {
    match *v {
        Value::Int(0) => Ok(0),
        Value::Int(n) if (4096..=64 << 20).contains(&n) => Ok(n as u32),
        Value::Int(_) => Err(Bad::Range("0 (kernel default) or 4096..=67108864".into())),
        ref other => Err(Bad::Type("an integer", other.type_name())),
    }
}

fn parse_fsync(s: &str) -> Option<Fsync> {
    match s {
        "always" => Some(Fsync::Always),
        "never" => Some(Fsync::Never),
        _ => {
            let ms = crate::toml::parse_int(s.strip_prefix("interval:")?)?;
            (1..=60_000)
                .contains(&ms)
                .then_some(Fsync::IntervalMs(ms as u64))
        }
    }
}

fn valid_symbol(s: &str) -> bool {
    (1..=16).contains(&s.len())
        && s.bytes().all(|c| {
            c.is_ascii_uppercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
        })
}
// @student-end
