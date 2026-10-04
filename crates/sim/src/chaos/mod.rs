//! Week 48 — chaos engineering: deterministic fault injection with
//! invariants checked after every step.
//!
//! Every component runs in one process, on simulated time, with faults
//! injected at the seams the simulator owns — the network between
//! components and the processes themselves — from a seeded
//! [`FaultPlan`]. A run is a pure function of its configuration and its
//! plan, so a failing run is a plan you can print, paste and replay.
//!
//! | Fault | Seam | Scenario | Invariant it threatens |
//! | --- | --- | --- | --- |
//! | drop / duplicate / reorder / delay a feed packet | `md` [`ChaosLink`] | [`FeedSim`] | never Live with a wrong book; Live again once the faults stop |
//! | corrupt a feed packet | `md` [`ChaosLink`] + the datagram checksum ([`wire`]) | [`FeedSim`] | a damaged packet is a loss, never applied |
//! | stalled (slow) consumer | [`Stall`](Fault::Stall) on a ring reader | [`FanoutSim`] | the publisher never waits; the lapped reader resyncs; others undisturbed |
//! | crash the primary | the process ([`CrashPrimary`](Fault::CrashPrimary)) | [`FailoverSim`] | no acknowledged command lost; promoted state = replayed journal |
//! | partition primary ↔ standby | both replication links ([`Partition`](Fault::Partition)) | [`FailoverSim`] | never two leaders |
//! | drop / duplicate / delay / corrupt replication traffic | `to-standby`, `to-primary` | [`FailoverSim`] | the standby converges (resends); leases still decide |
//! | drop / duplicate / delay / reorder market data, reset / stall order sessions | [`ChaosVenue`] around any [`Venue`](crate::Venue) | the w23 [`Runner`](crate::Runner) | each report exactly once, in order; inventories conserved; gap recovery resyncs |
//!
//! # Determinism, and why it holds
//!
//! * **Decisions are pure functions.** Whether fault `i` hits packet `n`
//!   of a link is `draw(plan.stream(i), n) % 1000 < per_mille` (see
//!   [`plan`] and [`Fate`]). Nothing is drawn from a shared generator.
//! * **No clocks, no threads, no hash-order iteration** in the
//!   simulators: time is a step counter (a [`ManualClock`](journal::ManualClock)
//!   where a lease needs nanoseconds), queues are ordered maps, and every
//!   component is a state machine stepped in a fixed order.
//! * **Every run keeps a fault log** ([`FaultEvent`]): same plan, same log;
//!   another seed, other faults.
//!
//! # A failing seed: report, replay, shrink
//!
//! Sweep seeds; when one fails, print its plan — that line *is* the bug
//! report. Paste it back to replay the failure exactly (same outcome,
//! same fault log), then shrink it to the smallest plan that still fails
//! before you start debugging. Here the property is a failover SLO — the
//! standby leads within one lease of the crash — under lossy replication:
//!
//! ```no_run
//! use sim::chaos::{shrink_plan, FailoverConfig, FailoverSim, Fault, FaultPlan, Link};
//!
//! let cfg = FailoverConfig::default();
//! let (to_s, to_p) = (Link::ToStandby, Link::ToPrimary);
//! let schedule = FaultPlan::new(0)
//!     .with(Fault::Drop { link: to_s, per_mille: 50 }, 0..2_000)
//!     .with(Fault::Delay { link: to_s, per_mille: 60, max_steps: 4 }, 0..2_000)
//!     .with(Fault::Drop { link: to_p, per_mille: 50 }, 0..2_000)
//!     .with(Fault::CrashPrimary, 222..223);
//! let fails = |p: &FaultPlan| match FailoverSim::run(cfg, p) {
//!     Ok(out) => out.failover_steps.is_some_and(|s| s > cfg.lease_steps),
//!     Err(_) => true, // a broken invariant fails too
//! };
//!
//! // 1. Sweep seeds; report the first failure as its plan.
//! let failing = (1..100)
//!     .map(|seed| schedule.reseeded(seed))
//!     .find(|p| fails(p))
//!     .expect("a seed that breaks the SLO");
//! let report = failing.to_string(); // e.g. "seed=… drop(to-standby,50)@0..2000 … crash@222"
//!
//! // 2. Replay it from the report: the same run, every time.
//! let replayed: FaultPlan = report.parse().unwrap();
//! assert_eq!(FailoverSim::run(cfg, &replayed), FailoverSim::run(cfg, &failing));
//!
//! // 3. Shrink it: fewer faults, narrower windows, lower rates.
//! let minimal = shrink_plan(&replayed, fails);
//! println!("minimal failing plan: {minimal}");
//! // e.g. "seed=1 delay(to-standby,48,4)@86..221 crash@222": a frame the
//! // primary sent just before dying arrives late; the standby takes it as
//! // a sign of life and renews its promise not to take over.
//! ```
//!
//! Shrinking is safe because a decision depends only on the seed, the
//! fault and the packet number: removing a fault, narrowing a window or
//! lowering a rate leaves every other decision as it was. (On a link whose
//! traffic reacts to faults — replication resends — a change can renumber
//! later packets; shrinking still only keeps plans that fail.) Turn the
//! minimal plan into a regression test (its text in a `#[test]`), fix the
//! bug, keep the test.
//!
//! # What the learner writes
//!
//! The plan types, the fault enums, the text format, [`shrink_plan`] and
//! the datagram [`wire`] are the contract (provided). The week's work is
//! the [`Injector`] and [`ChaosLink`] (the injector), the four scenario
//! drivers [`FeedSim`], [`FanoutSim`], [`FailoverSim`] and [`ChaosVenue`]
//! with the consumers they drive ([`FeedClient`], [`RingConsumer`]) (the
//! plan executor), and [`FeedChecker`] and the per-step checks of each
//! driver (the invariant checks).

pub mod failover;
pub mod fanout;
pub mod feed;
pub mod inject;
pub mod plan;
pub mod venue;
pub mod wire;

pub use failover::{FailoverConfig, FailoverOutcome, FailoverSim, FailoverViolation, Leader};
pub use fanout::{ConsumerStats, FanoutConfig, FanoutOutcome, FanoutSim, RingConsumer};
pub use feed::{
    FeedChecker, FeedClient, FeedClientStats, FeedConfig, FeedOutcome, FeedSim, FeedStatus,
    FeedViolation,
};
pub use inject::{ChaosLink, Injector, Packet};
pub use plan::{
    draw, shrink_plan, Fate, Fault, FaultEvent, FaultPlan, Link, ParsePlanError, Scheduled,
};
pub use venue::{ChaosVenue, SessionStats};
