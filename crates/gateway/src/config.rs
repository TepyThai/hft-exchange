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

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
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
    /// `busy_reject` (week 24): answer a new order that finds the engine
    /// queue full with a busy reject instead of waiting (see
    /// [`GatewayConfig::busy_reject`]). Default false.
    pub busy_reject: bool,
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
    /// `listen` (week 24): where the WebSocket feed is served — the first
    /// instrument on this port, the next one on port + 1, and so on.
    /// Default: not served (in-process subscribers only).
    pub listen: Option<SocketAddr>,
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
                busy_reject: false,
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
                listen: None,
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

#[allow(unused_variables)]
impl ExchangeConfig {
    /// Parses and validates `text` (no environment overrides).
    pub fn parse(text: &str) -> Result<ExchangeConfig, ConfigErrors> {
        todo!("w22: parse")
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
        todo!("w22: parse_with_env")
    }

    /// Reads `path` and parses it with the process environment's `HFT_*`
    /// overrides.
    pub fn load(path: &Path) -> Result<ExchangeConfig, ConfigErrors> {
        todo!("w22: load")
    }

    /// The effective configuration as TOML, every key explicit, in a fixed
    /// order: parsing it gives back `self`. O(size).
    pub fn to_toml(&self) -> String {
        todo!("w22: to_toml")
    }

    /// FNV-1a 64 of [`to_toml`](Self::to_toml): equal configurations hash
    /// equal on every machine and every run. Log it at startup; journal it
    /// with the matching-relevant values.
    pub fn hash(&self) -> u64 {
        todo!("w22: hash")
    }

    /// The gateway's runtime configuration.
    pub fn gateway_config(&self) -> GatewayConfig {
        todo!("w22: gateway_config")
    }

    /// The pre-trade limits for every account.
    pub fn limits(&self) -> Limits {
        todo!("w22: limits")
    }

    /// `(per_session, per_account)` throttle buckets.
    pub fn buckets(&self) -> (BucketConfig, BucketConfig) {
        todo!("w22: buckets")
    }

    /// The week-17 pre-trade gate for these limits and throttles.
    pub fn risk_gate(&self) -> RiskGate {
        todo!("w22: risk_gate")
    }
}

// TODO(w22): implement — see the week guide on tepy.dev/hft.
