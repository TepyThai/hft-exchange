//! Week 12: property-based testing of the engine.
//!
//! * [`OrderFlow`] — a seeded random order-stream generator (adds, cancels,
//!   reduces, market / IOC / FOK / post-only orders and malformed commands
//!   around a drifting mid). Deterministic: same [`FlowConfig`], same stream.
//! * [`InvariantChecker`] — checks every command's events against the
//!   event-order contract and the book against a ledger of open orders:
//!   never crossed, FIFO respected, quantity conserved, no zero or phantom
//!   orders.
//! * [`NaiveEngine`] — a deliberately simple reference matcher (a `Vec`
//!   scanned on every operation) with the same semantics as [`Engine`].
//! * [`differential`] — runs two [`Matcher`]s on the same stream and reports
//!   the first divergence; [`shrink`] minimises a failing stream by deleting
//!   commands.
//!
//! A typical property test:
//!
//! ```
//! use engine::testkit::{differential, shrink, FlowConfig, NaiveEngine, OrderFlow};
//! use engine::Engine;
//!
//! for seed in 0..20 {
//!     let cfg = FlowConfig::randomized(seed);
//!     let stream = OrderFlow::new(cfg).take_commands(200);
//!     let fails = |cmds: &[types::Command]| {
//!         let mut a = Engine::with_config(cfg.engine_config());
//!         let mut b = NaiveEngine::new(cfg.engine_config());
//!         differential(&mut a, &mut b, cmds).is_err()
//!     };
//!     if fails(&stream) {
//!         panic!("seed {seed}: minimal failing stream {:?}", shrink(&stream, fails));
//!     }
//! }
//! ```
//!
//! [`Engine`]: crate::Engine

mod check;
mod diff;
mod flow;
mod naive;

pub use check::{InvariantChecker, Totals, Violation};
pub use diff::{differential, shrink, Matcher, Mismatch};
pub use flow::{FlowConfig, OrderFlow};
pub use naive::NaiveEngine;
