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

use super::inject::ChaosLink;
use super::plan::{FaultEvent, FaultPlan, Link};
use crate::flow::{FlowConfig, OrderFlow};
use engine::{Engine, EngineConfig};
use marketdata::{BookBuilder, L2Publisher, L2Snapshot, L2Update, ReorderBuffer};
use types::{Event, Price, Seq};

// @student-begin w48
use super::wire::{open, seal_heartbeat, seal_packet, Datagram, OpenError};
use marketdata::feed::{encode_incremental, FeedPacket};
use marketdata::{PushOutcome, SnapshotOutcome, UpdateOutcome};
use std::collections::VecDeque;
// @student-end

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
// @student w48
#[derive(Clone, Debug)]
pub struct FeedClient {
    builder: BookBuilder,
    reorder: ReorderBuffer<L2Update>,
    horizon: Seq,
    patience: u64,
    behind_since: Option<u64>,
    requested_at: Option<u64>,
    /// Step of the last valid datagram or applied snapshot.
    last_heard: u64,
    silent: bool,
    stats: FeedClientStats,
}

// @student w48
impl FeedClient {
    /// A client joining at the start of the feed (empty book, update 1
    /// next) at step 0: Live. `window`: reorder window in updates;
    /// `patience`: steps (≥ 1) before asking for a snapshot without a gap
    /// in the builder, before repeating an unanswered request, and of
    /// silence before the line counts as silent.
    pub fn new(window: usize, patience: u64) -> FeedClient {
        let mut builder = BookBuilder::new();
        builder.apply_snapshot(&L2Snapshot::default());
        FeedClient {
            builder,
            reorder: ReorderBuffer::new(Seq(1), window),
            horizon: Seq(0),
            patience: patience.max(1),
            behind_since: None,
            requested_at: None,
            last_heard: 0,
            silent: false,
            stats: FeedClientStats::default(),
        }
    }

    /// Handles one datagram received at step `now`.
    pub fn on_datagram(&mut self, now: u64, datagram: &[u8]) {
        self.stats.datagrams += 1;
        let datagram = match open(datagram) {
            Err(OpenError::Checksum | OpenError::Truncated) => {
                self.stats.corrupt += 1;
                return;
            }
            Err(OpenError::Malformed(_)) => {
                self.stats.malformed += 1;
                return;
            }
            Ok(d) => d,
        };
        self.heard(now);
        match datagram {
            Datagram::Heartbeat { next_seq } => {
                self.stats.heartbeats += 1;
                self.horizon = self.horizon.max(Seq(next_seq.0.saturating_sub(1)));
            }
            Datagram::Packet(FeedPacket::Snapshot(snap)) => self.on_snapshot(now, &snap),
            Datagram::Packet(FeedPacket::Incremental(inc)) => {
                let last = Seq(inc.first_seq().0 + inc.len() as u64 - 1);
                self.horizon = self.horizon.max(last);
                for u in inc.iter() {
                    match self.reorder.push(u.seq, u) {
                        PushOutcome::Accepted => {}
                        PushOutcome::Duplicate => self.stats.duplicates += 1,
                        PushOutcome::Skipped { lost } => self.stats.lost += lost,
                    }
                }
                self.drain();
            }
        }
    }

    /// Applies a snapshot received at step `now` from the recovery channel
    /// (see the type docs).
    pub fn on_snapshot(&mut self, now: u64, snap: &L2Snapshot) {
        match self.builder.apply_snapshot(snap) {
            SnapshotOutcome::Stale => self.stats.stale_snapshots += 1,
            SnapshotOutcome::Synced { .. } | SnapshotOutcome::StillGapped { .. } => {
                self.heard(now);
                self.stats.snapshots += 1;
                self.requested_at = None;
                self.horizon = self.horizon.max(snap.seq);
                self.reorder.skip_to(Seq(snap.seq.0 + 1));
                self.drain();
            }
        }
    }

    /// Called once per step, after the step's datagrams and snapshots:
    /// updates the silence state, and returns true if the client wants a
    /// snapshot now (the request is then recorded as outstanding). See the
    /// type docs.
    pub fn wants_snapshot(&mut self, now: u64) -> bool {
        let silent = now.saturating_sub(self.last_heard) > self.patience;
        if silent && !self.silent {
            self.stats.silences += 1;
        }
        self.silent = silent;
        if self.status() == FeedStatus::Live {
            self.behind_since = None;
            return false;
        }
        let since = *self.behind_since.get_or_insert(now);
        if self
            .requested_at
            .is_some_and(|at| now.saturating_sub(at) < self.patience)
        {
            return false;
        }
        if !self.builder.is_synced() || now.saturating_sub(since) >= self.patience {
            self.requested_at = Some(now);
            self.stats.snapshot_requests += 1;
            return true;
        }
        false
    }

    /// Live or Recovering (see the type docs). O(1).
    pub fn status(&self) -> FeedStatus {
        if self.builder.is_synced() && self.last_seq() >= self.horizon && !self.silent {
            FeedStatus::Live
        } else {
            FeedStatus::Recovering
        }
    }

    /// The book — only while Live: never serve a book you cannot vouch
    /// for.
    pub fn book(&self) -> Option<&BookBuilder> {
        (self.status() == FeedStatus::Live).then_some(&self.builder)
    }

    /// The sequence number the book reflects.
    pub fn last_seq(&self) -> Seq {
        self.builder.last_seq().unwrap_or_default()
    }

    /// The highest sequence number the client knows exists.
    pub fn horizon(&self) -> Seq {
        self.horizon
    }

    /// Counters so far.
    pub fn stats(&self) -> FeedClientStats {
        self.stats
    }
}

// @student-begin w48
impl FeedClient {
    /// Something valid arrived at `now`: the line is not silent.
    fn heard(&mut self, now: u64) {
        self.last_heard = self.last_heard.max(now);
        self.silent = false;
    }

    /// Applies every update the reorder buffer releases.
    fn drain(&mut self) {
        while let Some((_, u)) = self.reorder.pop() {
            match self.builder.apply_update(&u) {
                UpdateOutcome::Applied => self.stats.applied += 1,
                UpdateOutcome::Duplicate => self.stats.duplicates += 1,
                UpdateOutcome::Gap { .. } => self.stats.gaps += 1,
                UpdateOutcome::Buffered => {}
            }
        }
    }
}
// @student-end

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
// @student w48
#[derive(Clone, Debug)]
pub struct FeedChecker {
    consumer: u32,
    truth: BookBuilder,
    /// Updates applied to `truth`.
    applied: usize,
    live_seq: Seq,
}

// @student w48
impl FeedChecker {
    /// A checker for consumer number `consumer` (used in violations) of a
    /// feed that starts empty at `Seq(0)`.
    pub fn new(consumer: u32) -> FeedChecker {
        let mut truth = BookBuilder::new();
        truth.apply_snapshot(&L2Snapshot::default());
        FeedChecker {
            consumer,
            truth,
            applied: 0,
            live_seq: Seq(0),
        }
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
        let Some(book) = book else {
            return Ok(());
        };
        let consumer = self.consumer;
        let seq = book.last_seq().unwrap_or_default();
        let head = Seq(published.len() as u64);
        if seq > head {
            return Err(FeedViolation::AheadOfPublisher {
                step,
                consumer,
                seq,
                head,
            });
        }
        if seq < self.live_seq {
            return Err(FeedViolation::WentBackwards {
                step,
                consumer,
                seq,
                before: self.live_seq,
            });
        }
        self.live_seq = seq;
        while (self.applied as u64) < seq.0 {
            self.truth.apply_update(&published[self.applied]);
            self.applied += 1;
        }
        if book.snapshot() != self.truth.snapshot() {
            return Err(FeedViolation::WrongLiveBook {
                step,
                consumer,
                seq,
            });
        }
        Ok(())
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
// @student w48
pub struct FeedSim {
    cfg: FeedConfig,
    flow: OrderFlow,
    engine: Engine,
    publisher: L2Publisher,
    published: Vec<L2Update>,
    link: ChaosLink<Vec<u8>>,
    client: FeedClient,
    checker: FeedChecker,
    answers: VecDeque<u64>,
    now: u64,
    datagrams: u64,
    live_steps: u64,
    behind_since: Option<u64>,
    longest_recovery: u64,
    events: Vec<Event>,
    fresh: Vec<L2Update>,
    packet: Vec<u8>,
    inbox: Vec<Vec<u8>>,
}

// @student w48
impl FeedSim {
    /// A run of `cfg` under `plan`, before its first step.
    pub fn new(cfg: FeedConfig, plan: &FaultPlan) -> FeedSim {
        let engine_cfg =
            EngineConfig::new(cfg.flow.spec).with_reference_price(Price(cfg.flow.fair_value));
        FeedSim {
            cfg,
            flow: OrderFlow::new(cfg.flow, cfg.flow_seed),
            engine: Engine::with_config(engine_cfg),
            publisher: L2Publisher::new(),
            published: Vec::new(),
            link: ChaosLink::new(plan, Link::MarketData),
            client: FeedClient::new(cfg.window, cfg.patience),
            checker: FeedChecker::new(0),
            answers: VecDeque::new(),
            now: 0,
            datagrams: 0,
            live_steps: 0,
            behind_since: None,
            longest_recovery: 0,
            events: Vec::with_capacity(64),
            fresh: Vec::with_capacity(64),
            packet: Vec::with_capacity(256),
            inbox: Vec::new(),
        }
    }

    /// Runs one step (see the type docs) and checks the invariants. Cost:
    /// `commands_per_step` engine commands, O(updates) to encode, the
    /// client's work, and the checker (two snapshots, allocated, while the
    /// client is Live).
    pub fn step(&mut self) -> Result<(), FeedViolation> {
        let t = self.now;
        self.now += 1;
        // 1. Publish.
        self.fresh.clear();
        if t < self.cfg.steps {
            for _ in 0..self.cfg.commands_per_step {
                let cmd = self.flow.next_command();
                self.events.clear();
                self.engine.process(&cmd, &mut self.events);
                for ev in &self.events {
                    self.flow.on_event(ev);
                    if let Some(u) = self.publisher.on_event(ev) {
                        self.fresh.push(u);
                    }
                }
            }
        }
        self.published.extend_from_slice(&self.fresh);
        if self.fresh.is_empty() {
            let mut datagram = Vec::with_capacity(16);
            seal_heartbeat(Seq(self.publisher.last_seq().0 + 1), &mut datagram);
            self.link.send(t, datagram);
            self.datagrams += 1;
        } else {
            for chunk in self.fresh.chunks(256) {
                self.packet.clear();
                encode_incremental(chunk, &mut self.packet);
                let mut datagram = Vec::with_capacity(self.packet.len() + 4);
                seal_packet(&self.packet, &mut datagram);
                self.link.send(t, datagram);
                self.datagrams += 1;
            }
        }
        // 2. Deliver.
        self.link.recv(t, &mut self.inbox);
        for datagram in self.inbox.drain(..) {
            self.client.on_datagram(t, &datagram);
        }
        // 3. Answer snapshot requests that are due.
        while self.answers.front().is_some_and(|&due| due <= t) {
            self.answers.pop_front();
            self.client.on_snapshot(t, &self.publisher.snapshot());
        }
        // 4. Ask.
        if self.client.wants_snapshot(t) {
            self.answers.push_back(t + self.cfg.snapshot_latency.max(1));
        }
        // 5. Check.
        self.checker.check(t, &self.published, self.client.book())?;
        if self.client.status() == FeedStatus::Live {
            self.behind_since = None;
            self.live_steps += 1;
        } else {
            let since = *self.behind_since.get_or_insert(t);
            let out = t - since + 1;
            self.longest_recovery = self.longest_recovery.max(out);
            if self.cfg.live_within.is_some_and(|bound| out > bound) {
                return Err(FeedViolation::SlowRecovery {
                    step: t,
                    consumer: 0,
                    since,
                });
            }
        }
        Ok(())
    }

    /// True once every traffic and quiet step has run.
    pub fn is_done(&self) -> bool {
        self.now >= self.cfg.steps + self.cfg.quiet_steps
    }

    /// Runs `cfg` under `plan` to the end (`steps + quiet_steps` steps)
    /// and checks convergence: the
    /// client must end Live at the publisher's last sequence number with
    /// its book. Returns the first violation.
    pub fn run(cfg: FeedConfig, plan: &FaultPlan) -> Result<FeedOutcome, FeedViolation> {
        let mut sim = FeedSim::new(cfg, plan);
        while !sim.is_done() {
            sim.step()?;
        }
        let head = sim.publisher.last_seq();
        let converged = sim
            .client
            .book()
            .is_some_and(|b| b.snapshot() == sim.publisher.snapshot());
        if !converged {
            return Err(FeedViolation::NotConverged {
                step: sim.now,
                consumer: 0,
                seq: sim.client.last_seq(),
                head,
            });
        }
        Ok(sim.outcome())
    }

    /// Steps run so far (the next step's number).
    pub fn now(&self) -> u64 {
        self.now
    }

    /// The client.
    pub fn client(&self) -> &FeedClient {
        &self.client
    }

    /// The publisher (the truth now).
    pub fn publisher(&self) -> &L2Publisher {
        &self.publisher
    }

    /// Every update published so far, in sequence order.
    pub fn published(&self) -> &[L2Update] {
        &self.published
    }

    /// What the plan did so far.
    pub fn fault_log(&self) -> &[FaultEvent] {
        self.link.injector().log()
    }

    /// The outcome so far. O(levels + log); allocates.
    pub fn outcome(&self) -> FeedOutcome {
        FeedOutcome {
            steps: self.now,
            updates: self.published.len() as u64,
            datagrams: self.datagrams,
            live_steps: self.live_steps,
            longest_recovery: self.longest_recovery,
            client: self.client.stats(),
            book: self.client.builder.snapshot(),
            fault_log: self.fault_log().to_vec(),
        }
    }
}
