//! Scenario 3 — leader failover under a fault plan: the week 20 hot
//! standby behind the week 12 engine pipeline, a simulated replication
//! network, a crash or a partition mid-stream.
//!
//! ```text
//!  client ── cmds ──► Pipeline<Primary> ─ p_out ─► ChaosLink(to-standby) ─► Standby<Engine>
//!    ▲ acked = what the standby acknowledged   ◄── ChaosLink(to-primary) ◄─ s_out (acks, resends)
//!    │
//!    └── after promotion: Pipeline<Journal> (the promoted standby)
//! ```
//!
//! Everything runs in one thread on one [`ManualClock`]: the primary's and
//! the standby's link ends are `mpsc` channels the simulator pumps through
//! two [`ChaosLink`]s, so a run is a function of its configuration and
//! plan. What is checked after **every** step:
//!
//! * **never two leaders** — the primary while its lease is current, and
//!   the promoted standby, are never both leading;
//! * **no acknowledged command lost** — everything acknowledged to the
//!   client is in the surviving journal: the standby's while it stands by,
//!   the promoted leader's after a failover;
//!
//! and at the promotion:
//!
//! * the promoted journal is exactly a prefix of the client's command
//!   stream (nothing invented, duplicated or reordered);
//! * the promoted engine's state hash equals a replay of that journal and
//!   the reference hash at that sequence number.
//!
//! Liveness: the run ends when every command is acknowledged, and the
//! final state hash must be the reference's; a run that takes more than
//! `max_steps` is [`Stuck`](FailoverViolation::Stuck).

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use super::inject::ChaosLink;
use super::plan::{FaultEvent, FaultPlan, Link};
use crate::flow::{FlowConfig, OrderFlow};
use engine::{Engine, EngineConfig, Pipeline};
use journal::{Journal, ManualClock, Primary, Standby, StandbyStats, ToPrimary, ToStandby};
use std::sync::mpsc::{Receiver, Sender};
use types::{Command, Event, Price, Seq};

// TODO(w48): implement — see the week guide on tepy.dev/hft.

/// Setup of a [`FailoverSim`] run (crashes, partitions and link faults
/// come from the plan).
#[derive(Clone, Copy, Debug)]
pub struct FailoverConfig {
    /// The order flow the client's commands come from.
    pub flow: FlowConfig,
    /// Seeds the order flow.
    pub flow_seed: u64,
    /// Commands the client sends in total.
    pub commands: usize,
    /// Commands the client offers the leader per step.
    pub per_step: usize,
    /// Simulated nanoseconds per step.
    pub step_ns: u64,
    /// The lease, in steps.
    pub lease_steps: u64,
    /// The primary also sends a heartbeat every this many steps while busy
    /// (it always does when idle). 0 = only when idle.
    ///
    /// Under load the frames themselves keep the lease alive. A lost
    /// resend request is repeated by the standby after a quarter of the
    /// lease (this harness found the stall that caused before that fix);
    /// heartbeating while busy still exposes a lost *last* frame sooner
    /// and bounds acknowledgement lag further.
    pub heartbeat_every: u64,
    /// Liveness bound: the run must finish within this many steps.
    pub max_steps: u64,
}

impl Default for FailoverConfig {
    /// 2 000 commands, 4 per step, 1 µs steps, a 20-step lease, a
    /// heartbeat every 8 steps, at most 5 000 steps.
    fn default() -> Self {
        FailoverConfig {
            flow: FlowConfig::default(),
            flow_seed: 1,
            commands: 2_000,
            per_step: 4,
            step_ns: 1_000,
            lease_steps: 20,
            heartbeat_every: 8,
            max_steps: 5_000,
        }
    }
}

/// A broken failover property.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FailoverViolation {
    /// The old primary still held a lease while the promoted standby led.
    TwoLeaders { step: u64 },
    /// A command acknowledged to the client is not in the surviving
    /// journal (which ends at `survived`).
    AckedLost {
        step: u64,
        acked: Seq,
        survived: Seq,
    },
    /// The promoted journal does not hold the client's command at `seq`.
    Diverged { step: u64, seq: Seq },
    /// A state hash at `seq` is not the reference's.
    HashMismatch {
        step: u64,
        seq: Seq,
        expected: u64,
        got: u64,
    },
    /// Not every command was acknowledged within `max_steps`.
    Stuck { step: u64, acked: Seq },
}

/// What a [`FailoverSim`] run did. Equal for equal configuration and plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailoverOutcome {
    /// Steps run.
    pub steps: u64,
    /// When the primary crashed.
    pub crashed_at: Option<u64>,
    /// When the standby promoted itself.
    pub promoted_at: Option<u64>,
    /// What the client had been acknowledged when the standby promoted.
    pub acked_at_failover: Seq,
    /// The promoted journal's last sequence number at the promotion.
    pub survived: Seq,
    /// Commands the client had sent to the old primary that did not
    /// survive (never acknowledged) and sent again.
    pub resent: u64,
    /// Steps from the crash or the partition's start to the promotion.
    pub failover_steps: Option<u64>,
    /// The leader's last sequence number at the end.
    pub final_seq: Seq,
    /// The leader's state hash at the end.
    pub final_hash: u64,
    /// The standby's counters (at the promotion, or at the end).
    pub standby: StandbyStats,
    /// What the plan did: packet faults on both replication links, the
    /// crash.
    pub fault_log: Vec<FaultEvent>,
}

/// Which node leads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Leader {
    /// The original primary, with a current lease.
    Primary,
    /// The promoted standby.
    Promoted,
    /// Nobody (a lease is running out): order entry pauses.
    None,
}

/// The plan executor of scenario 3 (see the [module docs](self)).
///
/// The client's commands are generated up front from the flow (fed back
/// through a shadow engine), with the reference state hash after every
/// prefix. The standby is created first and grants the primary its first
/// lease; leases are `lease_steps × step_ns` on the shared clock.
///
/// One [`step`](FailoverSim::step) (`t` counts from 1; the clock reads
/// `t × step_ns`):
///
/// 1. **crash**: at the plan's crash step the primary dies, with whatever
///    it had not yet put on the wire (logged);
/// 2. **network**: the primary's and the standby's outgoing messages are
///    sent on their [`ChaosLink`]s (`to-standby`, `to-primary`; a
///    partition cuts both); messages due now are handed to the standby and
///    to the primary (or the old primary, after a promotion);
/// 3. **standby**: handles what arrived (applies, acknowledges, grants);
/// 4. **promotion**: if the standby
///    [`can_promote`](journal::Standby::can_promote), it becomes the
///    leader: the checks at promotion run, and the client continues from
///    the promoted journal's end (resending what did not survive);
/// 5. **client**: the old primary processes acknowledgements; the leader
///    is offered up to `per_step` commands (the primary refuses once its
///    lease has run out); the primary heartbeats when idle and every
///    `heartbeat_every` steps; the client's acknowledged sequence number
///    becomes what the standby acknowledged (the promoted leader runs
///    alone and acknowledges what it sequences). A deposed primary keeps
///    running — polling and heartbeating — but is never offered commands;
/// 6. **check**: one leader at most, nothing acknowledged lost.
pub struct FailoverSim {
    // TODO(w48): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl FailoverSim {
    /// A run of `cfg` under `plan`, before its first step. O(commands): the
    /// stream and its reference hashes are computed here.
    pub fn new(cfg: FailoverConfig, plan: &FaultPlan) -> FailoverSim {
        todo!("w48: new")
    }

    /// Runs one step (see the type docs) and checks the invariants. Cost:
    /// O(log in-flight) per message on the links, the standby's and the
    /// leader's engine work (a resend request makes the primary send every
    /// frame from the requested one: O(backlog)), and at a promotion a
    /// replay of the promoted journal.
    pub fn step(&mut self) -> Result<(), FailoverViolation> {
        todo!("w48: step")
    }

    /// True once every command has been acknowledged.
    pub fn is_done(&self) -> bool {
        todo!("w48: is_done")
    }

    /// Runs `cfg` under `plan` until every command is acknowledged (or
    /// `max_steps`), then checks the final state hash: the leader's, and
    /// the standby's if no failover happened.
    pub fn run(
        cfg: FailoverConfig,
        plan: &FaultPlan,
    ) -> Result<FailoverOutcome, FailoverViolation> {
        todo!("w48: run")
    }

    /// Steps run so far (the clock reads `now × step_ns`).
    pub fn now(&self) -> u64 {
        todo!("w48: now")
    }

    /// Who leads now.
    pub fn leader(&self) -> Leader {
        todo!("w48: leader")
    }

    /// The highest sequence number acknowledged to the client.
    pub fn acked(&self) -> Seq {
        todo!("w48: acked")
    }

    /// The client's commands, in order (`commands()[s - 1]` is the command
    /// with sequence number `s`).
    pub fn commands(&self) -> &[Command] {
        todo!("w48: commands")
    }

    /// The reference state hash after command `seq`.
    ///
    /// # Panics
    /// If `seq` is beyond the stream.
    pub fn expected_hash(&self, seq: Seq) -> u64 {
        todo!("w48: expected_hash")
    }

    /// The journal that would survive losing the primary now: the
    /// promoted leader's, else the standby's.
    pub fn surviving_journal(&self) -> Option<&Journal> {
        todo!("w48: surviving_journal")
    }

    /// The state hash of whoever leads (or would: the primary while it is
    /// alive, else the standby).
    pub fn leader_hash(&self) -> u64 {
        todo!("w48: leader_hash")
    }

    /// What the plan did so far, ordered by step.
    pub fn fault_log(&self) -> Vec<FaultEvent> {
        todo!("w48: fault_log")
    }

    /// The outcome so far.
    pub fn outcome(&self) -> FailoverOutcome {
        todo!("w48: outcome")
    }
}

// TODO(w48): implement — see the week guide on tepy.dev/hft.
