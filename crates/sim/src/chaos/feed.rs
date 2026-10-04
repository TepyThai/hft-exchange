//! Scenario 1 — a feed handler under packet loss, duplication, reordering,
//! delay and corruption.
//!
//! ```text
//!  flow → engine → L2Publisher ─ encode_incremental ─ seal ─┐
//!                       │ (idle step: heartbeat)            │ ChaosLink (plan)
//!                       │                                   ▼
//!                       └── snapshot on request ──► FeedClient: open ─ ReorderBuffer ─ BookBuilder
//!                            (after snapshot_latency)          │ Live / Recovering
//!                                                              └─ wants_snapshot?
//! ```
//!
//! The safety property, checked after every step by [`FeedChecker`]: the
//! client is **never Live with a book that differs from the publisher's
//! book at the same sequence number**. The liveness property, checked at
//! the end of the run: once the faults stop and the line goes quiet
//! (heartbeats only), the client is Live at the publisher's last sequence
//! number with the publisher's book — optionally also "never out of Live
//! for more than `live_within` steps" ([`FeedConfig::live_within`]).

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use super::inject::ChaosLink;
use super::plan::{FaultEvent, FaultPlan, Link};
use crate::flow::{FlowConfig, OrderFlow};
use engine::{Engine, EngineConfig};
use marketdata::{BookBuilder, L2Publisher, L2Snapshot, L2Update, ReorderBuffer};
use types::{Event, Price, Seq};

// TODO(w48): implement — see the week guide on tepy.dev/hft.

/// Whether a consumer vouches for its book.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FeedStatus {
    /// The book is the publisher's book at the consumer's sequence number,
    /// and the consumer knows of no newer update it is missing.
    Live,
    /// A gap is open (or newer updates are known to exist): the book must
    /// not be used — strategies pull their quotes.
    Recovering,
}

/// What a [`FeedClient`] has seen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FeedClientStats {
    /// Datagrams received.
    pub datagrams: u64,
    /// Datagrams that failed the checksum (dropped as lost).
    pub corrupt: u64,
    /// Datagrams with a valid checksum but an invalid body.
    pub malformed: u64,
    /// Heartbeats received.
    pub heartbeats: u64,
    /// Updates applied in sequence (not counting those replayed on top of a
    /// snapshot by the book builder).
    pub applied: u64,
    /// Update copies dropped as already delivered or buffered.
    pub duplicates: u64,
    /// Sequence numbers the reorder buffer gave up on.
    pub lost: u64,
    /// Times the book lost sync (a gap reached the book builder).
    pub gaps: u64,
    /// Snapshot requests made.
    pub snapshot_requests: u64,
    /// Snapshots applied.
    pub snapshots: u64,
    /// Snapshots that were not newer than the book (ignored).
    pub stale_snapshots: u64,
    /// Times the line went silent (nothing heard for more than `patience`
    /// steps).
    pub silences: u64,
}

/// A market-data client that never serves a book it cannot vouch for.
///
/// Datagrams are verified ([`wire::open`](super::wire::open): a checksum failure
/// is a loss), incrementals go through a [`ReorderBuffer`] (late packets
/// put back in order, copies dropped, a hole given up after `window`
/// updates) into a [`BookBuilder`] that starts synced on the empty book at
/// `Seq(0)`. Snapshots come from a recovery channel on request.
///
/// * **Horizon**: the highest sequence number the client knows exists —
///   from any update it received and from heartbeats (`next_seq − 1`).
/// * **Silence**: at a [`wants_snapshot`](FeedClient::wants_snapshot)
///   call, the line is silent if nothing valid (datagram or snapshot) has
///   arrived for more than `patience` steps — even the heartbeats stopped,
///   so the client cannot know what it missed. Anything valid ends it.
/// * **Live** iff the builder is synced, its sequence number has reached
///   the horizon and the line is not silent; otherwise **Recovering**, and
///   [`book`](FeedClient::book) is `None`.
/// * **Asking for a snapshot** ([`wants_snapshot`](FeedClient::wants_snapshot),
///   called once per step): while Recovering, ask at once if the builder
///   is out of sync, or after `patience` steps of Recovering otherwise (a
///   reorder hole that may still fill, a tail loss revealed by a
///   heartbeat). One request at a time; an unanswered one is repeated
///   after `patience` steps.
/// * **A snapshot** newer than the book replaces it (the builder drops
///   what it covers and replays what follows), the reorder buffer skips
///   past it, and whatever it held is applied. One that is not newer is
///   counted stale and ignored (the request stays outstanding).
///
/// Cost: O(updates) per datagram plus the book operations; a snapshot is
/// O(levels + buffered).
#[derive(Clone, Debug)]
pub struct FeedClient {
    // TODO(w48): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl FeedClient {
    /// A client joining at the start of the feed (empty book, update 1
    /// next) at step 0: Live. `window`: reorder window in updates;
    /// `patience`: steps (≥ 1) before asking for a snapshot without a gap
    /// in the builder, before repeating an unanswered request, and of
    /// silence before the line counts as silent.
    pub fn new(window: usize, patience: u64) -> FeedClient {
        todo!("w48: new")
    }

    /// Handles one datagram received at step `now`.
    pub fn on_datagram(&mut self, now: u64, datagram: &[u8]) {
        todo!("w48: on_datagram")
    }

    /// Applies a snapshot received at step `now` from the recovery channel
    /// (see the type docs).
    pub fn on_snapshot(&mut self, now: u64, snap: &L2Snapshot) {
        todo!("w48: on_snapshot")
    }

    /// Called once per step, after the step's datagrams and snapshots:
    /// updates the silence state, and returns true if the client wants a
    /// snapshot now (the request is then recorded as outstanding). See the
    /// type docs.
    pub fn wants_snapshot(&mut self, now: u64) -> bool {
        todo!("w48: wants_snapshot")
    }

    /// Live or Recovering (see the type docs). O(1).
    pub fn status(&self) -> FeedStatus {
        todo!("w48: status")
    }

    /// The book — only while Live: never serve a book you cannot vouch
    /// for.
    pub fn book(&self) -> Option<&BookBuilder> {
        todo!("w48: book")
    }

    /// The sequence number the book reflects.
    pub fn last_seq(&self) -> Seq {
        todo!("w48: last_seq")
    }

    /// The highest sequence number the client knows exists.
    pub fn horizon(&self) -> Seq {
        todo!("w48: horizon")
    }

    /// Counters so far.
    pub fn stats(&self) -> FeedClientStats {
        todo!("w48: stats")
    }
}

// TODO(w48): implement — see the week guide on tepy.dev/hft.

/// A broken safety or liveness property of a market-data consumer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FeedViolation {
    /// Reported Live with a book that is not the publisher's book at the
    /// same sequence number.
    WrongLiveBook { step: u64, consumer: u32, seq: Seq },
    /// Reported Live at a sequence number the publisher has not reached.
    AheadOfPublisher {
        step: u64,
        consumer: u32,
        seq: Seq,
        head: Seq,
    },
    /// Reported Live at a lower sequence number than before.
    WentBackwards {
        step: u64,
        consumer: u32,
        seq: Seq,
        before: Seq,
    },
    /// Not Live for longer than the configured bound (since `since`).
    SlowRecovery {
        step: u64,
        consumer: u32,
        since: u64,
    },
    /// Not Live at the publisher's last sequence number, with its book,
    /// when the run ended.
    NotConverged {
        step: u64,
        consumer: u32,
        seq: Seq,
        head: Seq,
    },
}

/// The safety check for one consumer, run after every step: if the
/// consumer is Live (its `book()` is `Some`), its book must equal the
/// publisher's book at the consumer's sequence number, which must not be
/// beyond the publisher's head nor below the last sequence number it was
/// Live at.
///
/// It keeps its own copy of the truth — a [`BookBuilder`] advanced through
/// the publisher's update history — so the cost is amortized O(updates)
/// over a run plus O(levels) per Live check (two snapshots, allocated).
#[derive(Clone, Debug)]
pub struct FeedChecker {
    // TODO(w48): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl FeedChecker {
    /// A checker for consumer number `consumer` (used in violations) of a
    /// feed that starts empty at `Seq(0)`.
    pub fn new(consumer: u32) -> FeedChecker {
        todo!("w48: new")
    }

    /// Checks the consumer's `book` (its `book()`: `Some` only while Live)
    /// against `published`, every update so far in sequence order
    /// (`published[i].seq == i + 1`).
    pub fn check(
        &mut self,
        step: u64,
        published: &[L2Update],
        book: Option<&BookBuilder>,
    ) -> Result<(), FeedViolation> {
        todo!("w48: check")
    }
}

/// Setup of a [`FeedSim`] run (the faults come from the plan).
#[derive(Clone, Copy, Debug)]
pub struct FeedConfig {
    /// The order flow that drives the publisher.
    pub flow: FlowConfig,
    /// Seeds the order flow (the market); the plan's seed drives the faults.
    pub flow_seed: u64,
    /// Steps with traffic.
    pub steps: u64,
    /// Commands the publisher's engine processes per traffic step.
    pub commands_per_step: u32,
    /// Quiet steps at the end: heartbeats only. Faults should be over by
    /// then; the client must converge within them.
    pub quiet_steps: u64,
    /// The client's reorder window, in updates.
    pub window: usize,
    /// The client's patience, in steps (see [`FeedClient`]).
    pub patience: u64,
    /// Steps from a snapshot request to its answer (≥ 1).
    pub snapshot_latency: u64,
    /// Liveness bound: if set, the client may not stay out of Live for
    /// more than this many consecutive steps.
    pub live_within: Option<u64>,
}

impl Default for FeedConfig {
    /// 600 traffic steps of 5 commands, 60 quiet steps, window 32,
    /// patience 4, snapshots answered 2 steps after the request, no
    /// liveness bound during the run.
    fn default() -> Self {
        FeedConfig {
            flow: FlowConfig::default(),
            flow_seed: 1,
            steps: 600,
            commands_per_step: 5,
            quiet_steps: 60,
            window: 32,
            patience: 4,
            snapshot_latency: 2,
            live_within: None,
        }
    }
}

/// What a [`FeedSim`] run did. Two runs of the same configuration and plan
/// produce equal outcomes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FeedOutcome {
    /// Steps run.
    pub steps: u64,
    /// Updates published.
    pub updates: u64,
    /// Datagrams sent (packets and heartbeats).
    pub datagrams: u64,
    /// Steps at whose end the client was Live.
    pub live_steps: u64,
    /// The longest run of consecutive steps the client ended not Live.
    pub longest_recovery: u64,
    /// The client's counters.
    pub client: FeedClientStats,
    /// The client's book at the end.
    pub book: L2Snapshot,
    /// What the plan did.
    pub fault_log: Vec<FaultEvent>,
}

/// The plan executor of scenario 1 (see the [module docs](self)): one
/// publisher, one [`FeedClient`] behind a market-data [`ChaosLink`], a
/// snapshot service, and the [`FeedChecker`] after every step.
///
/// One [`step`](FeedSim::step) at step `t` (from 0):
///
/// 1. **publish**: while `t < steps`, the engine processes
///    `commands_per_step` commands from the flow; the step's updates go out
///    as one sealed incremental packet (a heartbeat with the next sequence
///    number if there were none, and every quiet step);
/// 2. **deliver**: the link's datagrams due at `t` go to the client;
/// 3. **answer**: snapshot requests due at `t` are answered with the
///    publisher's current book;
/// 4. **ask**: if the client [`wants_snapshot`](FeedClient::wants_snapshot),
///    the answer is due at `t + snapshot_latency`;
/// 5. **check**: the [`FeedChecker`], then the liveness bound.
///
/// Everything is a function of the configuration and the plan: no clocks,
/// no threads.
pub struct FeedSim {
    // TODO(w48): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl FeedSim {
    /// A run of `cfg` under `plan`, before its first step.
    pub fn new(cfg: FeedConfig, plan: &FaultPlan) -> FeedSim {
        todo!("w48: new")
    }

    /// Runs one step (see the type docs) and checks the invariants. Cost:
    /// `commands_per_step` engine commands, O(updates) to encode, the
    /// client's work, and the checker (two snapshots, allocated, while the
    /// client is Live).
    pub fn step(&mut self) -> Result<(), FeedViolation> {
        todo!("w48: step")
    }

    /// True once every traffic and quiet step has run.
    pub fn is_done(&self) -> bool {
        todo!("w48: is_done")
    }

    /// Runs `cfg` under `plan` to the end (`steps + quiet_steps` steps)
    /// and checks convergence: the
    /// client must end Live at the publisher's last sequence number with
    /// its book. Returns the first violation.
    pub fn run(cfg: FeedConfig, plan: &FaultPlan) -> Result<FeedOutcome, FeedViolation> {
        todo!("w48: run")
    }

    /// Steps run so far (the next step's number).
    pub fn now(&self) -> u64 {
        todo!("w48: now")
    }

    /// The client.
    pub fn client(&self) -> &FeedClient {
        todo!("w48: client")
    }

    /// The publisher (the truth now).
    pub fn publisher(&self) -> &L2Publisher {
        todo!("w48: publisher")
    }

    /// Every update published so far, in sequence order.
    pub fn published(&self) -> &[L2Update] {
        todo!("w48: published")
    }

    /// What the plan did so far.
    pub fn fault_log(&self) -> &[FaultEvent] {
        todo!("w48: fault_log")
    }

    /// The outcome so far. O(levels + log); allocates.
    pub fn outcome(&self) -> FeedOutcome {
        todo!("w48: outcome")
    }
}
