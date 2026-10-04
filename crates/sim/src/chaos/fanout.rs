//! Scenario 2 — slow consumers on the week 16 fan-out ring.
//!
//! ```text
//!  flow → engine → L2Publisher ─ publish ─► BroadcastRing ─► RingConsumer 0 (healthy)
//!                       │                        ├────────► RingConsumer 1 (stalled by the plan)
//!                       └─ periodic_snapshot ─► SnapshotCell ◄─ resync after Lagged
//! ```
//!
//! The writer of a broadcast ring never waits for readers: a consumer
//! that stops reading is lapped, learns it from [`Recv::Lagged`], and
//! resyncs from the latest snapshot in the [`SnapshotCell`]. What the
//! scenario asserts:
//!
//! * the publisher publishes every update on time, whatever the readers do
//!   (it never blocks);
//! * a stalled consumer lags, is never Live with a wrong book (the
//!   [`FeedChecker`] after every step), resyncs and ends consistent;
//! * healthy consumers never lag and are never disturbed.
//!
//! [`Recv::Lagged`]: marketdata::Recv::Lagged

use super::feed::{FeedChecker, FeedStatus, FeedViolation};
use super::plan::{FaultEvent, FaultPlan};
use crate::flow::{FlowConfig, OrderFlow};
use engine::{Engine, EngineConfig};
use marketdata::SnapshotCell;
use marketdata::{BookBuilder, BroadcastRing, L2Publisher, L2Update, Reader, RingPublisher};
use std::sync::Arc;
use types::{Event, Price};

// @student-begin w48
use marketdata::{broadcast, Recv, SnapshotOutcome, UpdateOutcome};
// @student-end

/// What a [`RingConsumer`] has seen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ConsumerStats {
    /// Updates read from the ring.
    pub read: u64,
    /// Times the ring lapped this consumer.
    pub lags: u64,
    /// Updates it missed because of that.
    pub missed: u64,
    /// Times the book lost sync.
    pub gaps: u64,
    /// Snapshots applied from the cell to recover.
    pub resyncs: u64,
}

/// A consumer of the fan-out ring that keeps a book and recovers from
/// lagging by resyncing from the [`SnapshotCell`].
///
/// * It joins at the **oldest** value still in the ring and applies the
///   cell's snapshot: updates the snapshot covers are duplicates, the rest
///   continue it (the publisher stores a snapshot at least every
///   `capacity / 2` updates, so they always do).
/// * [`poll`](RingConsumer::poll) reads up to `budget` updates into its
///   [`BookBuilder`]. On [`Lagged`](marketdata::Recv::Lagged) it
///   counts the loss and reads the next (oldest) value at once, so the
///   builder sees the gap in the same poll; a lag does not use budget.
/// * While the builder is out of sync it applies the cell's snapshot if it
///   is newer than the book (several polls may be needed, if the cell
///   still predates the gap).
/// * Live iff the builder is synced; [`book`](RingConsumer::book) is
///   `None` otherwise.
///
/// Cost: O(1) per update read plus the book operation; a resync is
/// O(levels) and clones the snapshot.
// @student w48
pub struct RingConsumer {
    reader: Reader<L2Update>,
    builder: BookBuilder,
    stats: ConsumerStats,
}

// @student w48
impl RingConsumer {
    /// Joins the feed carried by `ring`, starting from `snapshots`.
    pub fn new(ring: &Arc<BroadcastRing<L2Update>>, snapshots: &SnapshotCell) -> RingConsumer {
        let reader = ring.subscribe_oldest();
        let mut builder = BookBuilder::new();
        builder.apply_snapshot(&snapshots.load());
        RingConsumer {
            reader,
            builder,
            stats: ConsumerStats::default(),
        }
    }

    /// Reads up to `budget` updates, then resyncs from `snapshots` if out
    /// of sync. Returns the number of updates read.
    pub fn poll(&mut self, snapshots: &SnapshotCell, budget: usize) -> usize {
        let mut read = 0;
        while read < budget {
            match self.reader.try_recv() {
                Recv::Item(u) => {
                    read += 1;
                    self.stats.read += 1;
                    if let UpdateOutcome::Gap { .. } = self.builder.apply_update(&u) {
                        self.stats.gaps += 1;
                    }
                }
                Recv::Empty => break,
                Recv::Lagged(missed) => {
                    self.stats.lags += 1;
                    self.stats.missed += missed;
                }
            }
        }
        let behind = |b: &BookBuilder| snapshots.seq() > b.last_seq().unwrap_or_default();
        if !self.builder.is_synced() && behind(&self.builder) {
            match self.builder.apply_snapshot(&snapshots.load()) {
                SnapshotOutcome::Stale => {}
                SnapshotOutcome::Synced { .. } | SnapshotOutcome::StillGapped { .. } => {
                    self.stats.resyncs += 1;
                }
            }
        }
        read
    }

    /// Live iff the book is synced.
    pub fn status(&self) -> FeedStatus {
        if self.builder.is_synced() {
            FeedStatus::Live
        } else {
            FeedStatus::Recovering
        }
    }

    /// The book, only while Live.
    pub fn book(&self) -> Option<&BookBuilder> {
        self.builder.is_synced().then_some(&self.builder)
    }

    /// Values published that this consumer has not read yet.
    pub fn backlog(&self) -> u64 {
        self.reader.backlog()
    }

    /// Counters so far.
    pub fn stats(&self) -> ConsumerStats {
        self.stats
    }
}

/// Setup of a [`FanoutSim`] run (stalls come from the plan's
/// [`Stall`](super::Fault::Stall) faults, by consumer number).
#[derive(Clone, Copy, Debug)]
pub struct FanoutConfig {
    /// The order flow that drives the publisher.
    pub flow: FlowConfig,
    /// Seeds the order flow.
    pub flow_seed: u64,
    /// Steps with traffic.
    pub steps: u64,
    /// Commands per traffic step.
    pub commands_per_step: u32,
    /// Steps without traffic at the end, for consumers to catch up.
    pub settle_steps: u64,
    /// Ring capacity (a power of two ≥ 2).
    pub capacity: usize,
    /// The publisher stores a snapshot in the cell every this many updates
    /// (at most `capacity / 2`).
    pub snapshot_interval: u64,
    /// Number of consumers.
    pub consumers: u32,
    /// Updates a consumer reads per step when not stalled.
    pub budget: usize,
}

impl Default for FanoutConfig {
    /// 400 steps of 4 commands then 10 quiet steps, a 256-slot ring with a
    /// snapshot every 64 updates, 3 consumers reading up to 64 updates per
    /// step.
    fn default() -> Self {
        FanoutConfig {
            flow: FlowConfig::default(),
            flow_seed: 1,
            steps: 400,
            commands_per_step: 4,
            settle_steps: 10,
            capacity: 256,
            snapshot_interval: 64,
            consumers: 3,
            budget: 64,
        }
    }
}

/// What a [`FanoutSim`] run did. Equal for equal configuration and plan.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FanoutOutcome {
    /// Steps run.
    pub steps: u64,
    /// Updates published (= written to the ring).
    pub published: u64,
    /// Snapshots stored in the cell.
    pub snapshots: u64,
    /// Per consumer.
    pub consumers: Vec<ConsumerStats>,
    /// Stalls and resumes, as they happened.
    pub fault_log: Vec<FaultEvent>,
}

/// The plan executor of scenario 2: one publisher writing a
/// [`BroadcastRing`] and a [`SnapshotCell`], `consumers`
/// [`RingConsumer`]s, a [`FeedChecker`] per consumer after every step.
///
/// One [`step`](FanoutSim::step) at step `t`: while `t < steps`, the
/// engine processes `commands_per_step` commands and every update is
/// published to the ring at once (and a periodic snapshot stored in the
/// cell when due); then every consumer not stalled at `t` polls with its
/// budget (a stall starting or ending is logged); then the checks run.
// @student w48
pub struct FanoutSim {
    cfg: FanoutConfig,
    plan: FaultPlan,
    flow: OrderFlow,
    engine: Engine,
    publisher: L2Publisher,
    ring_tx: RingPublisher<L2Update>,
    snapshots: SnapshotCell,
    consumers: Vec<RingConsumer>,
    checkers: Vec<FeedChecker>,
    stalled: Vec<bool>,
    published: Vec<L2Update>,
    stored: u64,
    log: Vec<FaultEvent>,
    now: u64,
    events: Vec<Event>,
}

// @student w48
impl FanoutSim {
    /// A run of `cfg` under `plan`, before its first step.
    ///
    /// # Panics
    /// If the capacity is not a power of two ≥ 2 or the snapshot interval
    /// is not in `1..=capacity / 2`.
    pub fn new(cfg: FanoutConfig, plan: &FaultPlan) -> FanoutSim {
        assert!(
            (1..=cfg.capacity as u64 / 2).contains(&cfg.snapshot_interval),
            "snapshot_interval must be in 1..=capacity/2"
        );
        let engine_cfg =
            EngineConfig::new(cfg.flow.spec).with_reference_price(Price(cfg.flow.fair_value));
        let (ring_tx, ring) = broadcast::<L2Update>(cfg.capacity);
        let snapshots = SnapshotCell::default();
        let n = cfg.consumers as usize;
        FanoutSim {
            cfg,
            plan: plan.clone(),
            flow: OrderFlow::new(cfg.flow, cfg.flow_seed),
            engine: Engine::with_config(engine_cfg),
            publisher: L2Publisher::with_snapshot_interval(cfg.snapshot_interval),
            consumers: (0..n)
                .map(|_| RingConsumer::new(&ring, &snapshots))
                .collect(),
            checkers: (0..n as u32).map(FeedChecker::new).collect(),
            stalled: vec![false; n],
            ring_tx,
            snapshots,
            published: Vec::new(),
            stored: 0,
            log: Vec::new(),
            now: 0,
            events: Vec::with_capacity(64),
        }
    }

    /// Runs one step (see the type docs) and checks every consumer. Cost:
    /// `commands_per_step` engine commands, O(1) per update published, each
    /// consumer's poll, and a check per Live consumer (two snapshots,
    /// allocated).
    pub fn step(&mut self) -> Result<(), FeedViolation> {
        let t = self.now;
        self.now += 1;
        if t < self.cfg.steps {
            for _ in 0..self.cfg.commands_per_step {
                let cmd = self.flow.next_command();
                self.events.clear();
                self.engine.process(&cmd, &mut self.events);
                for ev in &self.events {
                    self.flow.on_event(ev);
                    if let Some(u) = self.publisher.on_event(ev) {
                        self.ring_tx.publish(u);
                        self.published.push(u);
                        if let Some(snap) = self.publisher.periodic_snapshot() {
                            self.snapshots.store(snap);
                            self.stored += 1;
                        }
                    }
                }
            }
        }
        for i in 0..self.consumers.len() {
            let stalled = self.plan.stalled(i as u32, t);
            if stalled != self.stalled[i] {
                self.stalled[i] = stalled;
                let consumer = i as u32;
                self.log.push(if stalled {
                    FaultEvent::Stall { step: t, consumer }
                } else {
                    FaultEvent::Resume { step: t, consumer }
                });
            }
            if !stalled {
                self.consumers[i].poll(&self.snapshots, self.cfg.budget);
            }
        }
        for (checker, consumer) in self.checkers.iter_mut().zip(&self.consumers) {
            checker.check(t, &self.published, consumer.book())?;
        }
        Ok(())
    }

    /// True once every traffic and settle step has run.
    pub fn is_done(&self) -> bool {
        self.now >= self.cfg.steps + self.cfg.settle_steps
    }

    /// Runs `cfg` under `plan` to the end; every consumer must end Live at
    /// the publisher's last sequence number with its book.
    pub fn run(cfg: FanoutConfig, plan: &FaultPlan) -> Result<FanoutOutcome, FeedViolation> {
        let mut sim = FanoutSim::new(cfg, plan);
        while !sim.is_done() {
            sim.step()?;
        }
        let truth = sim.publisher.snapshot();
        for (i, c) in sim.consumers.iter().enumerate() {
            if !c.book().is_some_and(|b| b.snapshot() == truth) {
                return Err(FeedViolation::NotConverged {
                    step: sim.now,
                    consumer: i as u32,
                    seq: c.builder.last_seq().unwrap_or_default(),
                    head: truth.seq,
                });
            }
        }
        Ok(sim.outcome())
    }

    /// Steps run so far.
    pub fn now(&self) -> u64 {
        self.now
    }

    /// Consumer `i`.
    ///
    /// # Panics
    /// If there is no such consumer.
    pub fn consumer(&self, i: usize) -> &RingConsumer {
        &self.consumers[i]
    }

    /// The ring.
    pub fn ring(&self) -> &Arc<BroadcastRing<L2Update>> {
        self.ring_tx.ring()
    }

    /// The publisher (the truth now).
    pub fn publisher(&self) -> &L2Publisher {
        &self.publisher
    }

    /// Every update published so far, in sequence order.
    pub fn published(&self) -> &[L2Update] {
        &self.published
    }

    /// Stalls and resumes so far.
    pub fn fault_log(&self) -> &[FaultEvent] {
        &self.log
    }

    /// The outcome so far.
    pub fn outcome(&self) -> FanoutOutcome {
        FanoutOutcome {
            steps: self.now,
            published: self.published.len() as u64,
            snapshots: self.stored,
            consumers: self.consumers.iter().map(RingConsumer::stats).collect(),
            fault_log: self.log.clone(),
        }
    }
}
