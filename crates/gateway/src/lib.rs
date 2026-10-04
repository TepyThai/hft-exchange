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
//! | 31 | [`net`], [`pingpong`] | Socket tuning with read-back, vectored writes that survive partial writes, a loopback ping-pong |
//! | 32 | [`io`] | A submission/completion I/O interface with an epoll backend and a simulated io_uring backend |
//!
//! The [`client`] module is a provided blocking client for tests, benchmarks
//! and simulated traders.
//!
//! # Where week 24 plugs in
//!
//! Everything between the engine and the sessions runs on the engine thread
//! through [`hooks::Stage`]: [`Stage::on_command`] sees every sequenced
//! command (exchange ids) before it is matched — the journal's write-ahead
//! point — and [`Stage::on_events`] sees its events before they are routed
//! — the market-data publisher's input. Reports are released to the
//! sessions per batch, after [`Stage::flush`], so a journal can
//! group-commit and still never acknowledge an order it has not made
//! durable. Install a stage with [`Gateway::start_with`].

#![warn(missing_docs)]

pub mod client;
pub mod config;
pub mod hooks;
pub mod io;
pub mod net;
pub mod pingpong;
pub mod router;
pub mod sequencer;
pub mod server;
pub mod session;
pub mod toml;

pub use config::{ConfigError, ConfigErrors, ExchangeConfig};
pub use hooks::{AllowAll, Hooks, NoStage, PreTrade, RiskGate, Stage};
pub use net::{SocketOptions, SocketTuning};
pub use router::{OrderRouter, Route};
pub use sequencer::Sequencer;
pub use server::{EngineIdle, Gateway, GatewayConfig, GatewayStats, ShutdownReport};
pub use session::{Action, Auth, CloseReason, Session, SessionId, SessionState};
