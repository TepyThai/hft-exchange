//! Week 23 — simulation and integration testing.
//!
//! Seeded simulated traders drive the whole exchange in process, and tests
//! assert end-to-end outcomes deterministically:
//!
//! ```text
//!   agents ──commands──► Venue::send ─► risk ─► engine ─► L2 publisher
//!     ▲                                  │ reject   │ events     │ updates
//!     │                                  ▼          ▼            ▼
//!     └──── on_event(Delivery) ◄──── Venue::flush ──────────► BookBuilder
//!                                                        (what agents see)
//! ```
//!
//! * [`Agent`] is the trader contract; [`MakerAgent`], [`NoiseAgent`] and
//!   [`MomentumAgent`] are built in, and a test (or a later week) can add
//!   its own.
//! * [`Venue`] is the exchange as a trader sees it; [`InProcess`] is risk →
//!   engine → market data in one thread. The TCP gateway becomes a second
//!   implementation (see the [`Venue`] docs).
//! * [`Runner`] gives agents turns, routes deliveries, keeps each agent's
//!   [`Inventory`] and produces a [`ScenarioReport`].
//!
//! Determinism comes from three rules: every agent draws from its own
//! [`types::Rng`] seeded from the scenario seed, turns run in a fixed
//! order, and the engine itself is deterministic. Nothing reads a clock.
//!
//! What the end-to-end tests check: same seed → same run (reports and
//! engine state hash); positions and cash sum to zero across agents; the
//! book rebuilt from market data equals the engine's book after every
//! step; risk rejections reach the agent and never reach the engine.

mod agent;
mod agents;
mod runner;
mod venue;

pub use agent::{Agent, AgentCtx, Delivery, FillLeg, Inventory, Outbox, Quote, Quotes};
pub use agents::{MakerAgent, MakerConfig, MomentumAgent, MomentumConfig, NoiseAgent, NoiseConfig};
pub use runner::{AgentReport, Runner, ScenarioConfig, ScenarioReport, AGENT_ID_SHIFT};
pub use venue::{InProcess, Venue};
