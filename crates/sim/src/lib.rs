//! Simulation: traders, scenarios, microstructure analytics, strategies, backtesting, fault injection.
//!
//! Curriculum: Weeks 23, 41, 42, 43, 44, 48 (see <https://tepy.dev/hft>).
//!
//! | Week | Module | What |
//! | --- | --- | --- |
//! | 23 | [`scenario`] | Simulated traders ([`Agent`]), the in-process pipeline risk → engine → market data ([`InProcess`]), the deterministic [`Runner`] |
//! | 41 | [`micro`] | Spread, imbalance, microprice ([`TopOfBook`]), queue position, effective / realized spread and [`Markouts`] |
//! | 42 | [`mm`] | A [`MarketMaker`]: quotes around the microprice, inventory skew and limits, cancel/replace |
//! | 43 | [`pairs`] | [`RollingStats`] (Welford), a [`PairsTrader`] on two cointegrated instruments ([`PairMarket`]) |
//! | 44 | [`backtest`] | An event-driven [`Backtester`]: a recorded command log replayed through the engine, order latency, fill accounting, drawdown |
//!
//! Provided infrastructure (not exercises): [`flow`] generates synthetic
//! order flow and [`live`] runs an engine + flow + human trader session for
//! the WebAssembly page, the native terminal and the demo server.

pub mod backtest;
pub mod flow;
pub mod live;
pub mod micro;
pub mod mm;
pub mod pairs;
pub mod scenario;

pub use backtest::{BacktestConfig, BacktestReport, Backtester, Recording};
pub use flow::{FlowConfig, OrderFlow};
pub use live::{LiveSession, UserKind};
pub use micro::{Markouts, QueuePosition, QueueTracker, TopOfBook};
pub use mm::{MarketMaker, MmConfig};
pub use pairs::{PairMarket, PairsTrader, RollingStats};
pub use scenario::{
    Agent, AgentCtx, Delivery, InProcess, Inventory, Outbox, Runner, ScenarioConfig,
    ScenarioReport, Venue,
};
