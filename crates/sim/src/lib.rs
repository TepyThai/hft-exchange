//! Simulation: traders, scenarios, microstructure analytics, strategies, backtesting, fault injection.
//!
//! Curriculum: Weeks 23, 41, 42, 43, 44, 48 (see https://tepy.dev/hft).
//!
//! Provided infrastructure (not exercises): [`flow`] generates synthetic
//! order flow and [`live`] runs an engine + flow + human trader session for
//! the WebAssembly page, the native terminal and the demo server.

pub mod flow;
pub mod live;

pub use flow::{FlowConfig, OrderFlow};
pub use live::{LiveSession, UserKind};
