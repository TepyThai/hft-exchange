//! Order gateway: TCP sessions, configuration, batched I/O, socket tuning,
//! and the exchange binary.
//!
//! Curriculum: Weeks 13, 22, 24, 31, 32 (see <https://tepy.dev/hft>).
//!
//! | Week | Module | What |
//! | --- | --- | --- |
//! | 13 | [`session`], [`router`], [`sequencer`], [`server`] | Tokio TCP gateway: framed sessions, logon, client-id → exchange-id remapping, pre-trade hook, a dedicated engine thread fed by the w05 MPSC queue, reports routed back per session, draining shutdown |
//! | 17 | [`hooks::RiskGate`] | The w15 risk engine and w17 throttles as the gateway's pre-trade check |
//! | 22 | [`config`], [`toml`] | Typed exchange configuration from a hand-parsed TOML subset, validation with line numbers, `HFT_*` environment overrides; the `exchange` binary and its container image |
//! | 24 | [`exchange`] | Exchange v2: the journal as the write-ahead point with group commit, durable-before-visible output gating, market-data fan-out (L2 and trade rings, snapshots, WebSocket), restart from the journals, stage stamps and attribution, busy rejects at the edge; the `exchange` binary runs it, `loadtest` measures it |
//! | 31 | [`net`], [`pingpong`] | Socket tuning with read-back, vectored writes that survive partial writes, a loopback ping-pong |
//! | 32 | [`io`] | A submission/completion I/O interface with an epoll backend and a simulated io_uring backend |
//!
//! Provided, not part of a week's task: the [`client`] module, a blocking
//! client for tests, benchmarks and simulated traders, and [`loadgen`],
//! week 24's open-loop load generator.
//!
//! # How week 24 plugs in
//!
//! Everything between the engine and the sessions runs on the engine thread
//! through [`hooks::Stage`]: [`Stage::on_command`] sees every sequenced
//! command (exchange ids) before it is matched — the journal's write-ahead
//! point — and [`Stage::on_events`] sees its events before they are routed
//! — the market-data publisher's input. Reports are released to the
//! sessions per batch, after [`Stage::flush`], so a journal can
//! group-commit and still never acknowledge an order it has not made
//! durable. [`ExchangeStage`] is that stage; [`ExchangeV2`] assembles it
//! with recovered engines ([`Sequencer::with_engines`]) and starts the
//! gateway around them ([`Gateway::start_sequencer`]).

#![warn(missing_docs)]

pub mod client;
pub mod config;
pub mod exchange;
pub mod hooks;
pub mod io;
pub mod loadgen;
pub mod net;
pub mod pingpong;
pub mod router;
pub mod sequencer;
pub mod server;
pub mod session;
pub mod toml;

pub use config::{ConfigError, ConfigErrors, ExchangeConfig, Fsync};
pub use exchange::{
    assemble, journal_path, ExchangeStage, ExchangeV2, Feed, FeedConfig, JournalBackend,
    MemJournals, StageLog, StartError, TimedPreTrade, TradeTick, V2Config, V2Report,
};
pub use hooks::{AllowAll, Hooks, NoStage, PreTrade, RiskGate, Stage};
pub use net::{SocketOptions, SocketTuning};
pub use router::{OrderRouter, Route};
pub use sequencer::Sequencer;
pub use server::{EngineIdle, Gateway, GatewayConfig, GatewayStats, ShutdownReport, BUSY};
pub use session::{Action, Auth, CloseReason, Session, SessionId, SessionState};
