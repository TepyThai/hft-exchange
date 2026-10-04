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

use super::inject::ChaosLink;
use super::plan::{FaultEvent, FaultPlan, Link};
use crate::flow::{FlowConfig, OrderFlow};
use engine::{Engine, EngineConfig, Pipeline};
use journal::{Journal, ManualClock, Primary, Standby, StandbyStats, ToPrimary, ToStandby};
use std::sync::mpsc::{Receiver, Sender};
use types::{Command, Event, Price, Seq};

// @student-begin w48
use journal::{replay, PrimaryLink, StandbyLink};
use std::sync::mpsc;
use std::time::Duration;
// @student-end

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
    /// Under load the frames themselves keep the lease alive, but only a
    /// heartbeat makes the week 20 standby repeat a resend request: it asks
    /// once per missing sequence number, so if that request is lost and
    /// the primary never goes idle, replication — and with it every
    /// acknowledgement — stalls until it does. Heartbeating while busy
    /// bounds the stall.
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
// @student w48
pub struct FailoverSim {
    cfg: FailoverConfig,
    plan: FaultPlan,
    commands: Vec<Command>,
    /// `expect[s]`: the reference state hash after command `s`.
    expect: Vec<u64>,
    clock: ManualClock,
    primary: Option<Pipeline<Primary<ManualClock>>>,
    p_out: Receiver<ToStandby>,
    p_in: Sender<ToPrimary>,
    standby: Option<Standby<Engine, ManualClock>>,
    s_out: Receiver<ToPrimary>,
    s_in: Sender<ToStandby>,
    to_standby: ChaosLink<ToStandby>,
    to_primary: ChaosLink<ToPrimary>,
    leader: Option<Pipeline<Journal>>,
    next: usize,
    acked: Seq,
    now: u64,
    crashed_at: Option<u64>,
    promoted_at: Option<u64>,
    acked_at_failover: Seq,
    survived: Seq,
    resent: u64,
    standby_stats: StandbyStats,
    log: Vec<FaultEvent>,
    inbox_s: Vec<ToStandby>,
    inbox_p: Vec<ToPrimary>,
}

// @student w48
impl FailoverSim {
    /// A run of `cfg` under `plan`, before its first step. O(commands): the
    /// stream and its reference hashes are computed here.
    pub fn new(cfg: FailoverConfig, plan: &FaultPlan) -> FailoverSim {
        let engine_cfg =
            EngineConfig::new(cfg.flow.spec).with_reference_price(Price(cfg.flow.fair_value));
        // The client's stream, and the reference hash after every prefix.
        let mut flow = OrderFlow::new(cfg.flow, cfg.flow_seed);
        let mut shadow = Engine::with_config(engine_cfg);
        let mut events: Vec<Event> = Vec::new();
        let mut commands = Vec::with_capacity(cfg.commands);
        let mut expect = Vec::with_capacity(cfg.commands + 1);
        expect.push(shadow.state_hash());
        for _ in 0..cfg.commands {
            let cmd = flow.next_command();
            events.clear();
            shadow.process(&cmd, &mut events);
            for ev in &events {
                flow.on_event(ev);
            }
            commands.push(cmd);
            expect.push(shadow.state_hash());
        }
        // Each node's link ends are channels to and from the simulator.
        let (p_tx, p_out) = mpsc::channel();
        let (p_in, p_rx) = mpsc::channel();
        let (s_tx, s_out) = mpsc::channel();
        let (s_in, s_rx) = mpsc::channel();
        let clock = ManualClock::new(0);
        let lease = Duration::from_nanos(cfg.lease_steps * cfg.step_ns);
        let standby = Standby::new(
            Journal::new(),
            Engine::with_config(engine_cfg),
            StandbyLink { tx: s_tx, rx: s_rx },
            clock.clone(),
            lease,
        );
        let primary = Primary::new(
            Journal::new(),
            PrimaryLink { tx: p_tx, rx: p_rx },
            clock.clone(),
            lease,
        );
        let primary = Pipeline::with_journal(primary, Engine::with_config(engine_cfg))
            .expect("a fresh journal and a fresh engine agree");
        FailoverSim {
            cfg,
            plan: plan.clone(),
            commands,
            expect,
            clock,
            primary: Some(primary),
            p_out,
            p_in,
            standby: Some(standby),
            s_out,
            s_in,
            to_standby: ChaosLink::new(plan, Link::ToStandby),
            to_primary: ChaosLink::new(plan, Link::ToPrimary),
            leader: None,
            next: 0,
            acked: Seq(0),
            now: 0,
            crashed_at: None,
            promoted_at: None,
            acked_at_failover: Seq(0),
            survived: Seq(0),
            resent: 0,
            standby_stats: StandbyStats::default(),
            log: Vec::new(),
            inbox_s: Vec::new(),
            inbox_p: Vec::new(),
        }
    }

    /// Runs one step (see the type docs) and checks the invariants. Cost:
    /// O(log in-flight) per message on the links, the standby's and the
    /// leader's engine work (a resend request makes the primary send every
    /// frame from the requested one: O(backlog)), and at a promotion a
    /// replay of the promoted journal.
    pub fn step(&mut self) -> Result<(), FailoverViolation> {
        self.now += 1;
        let t = self.now;
        self.clock.set(t * self.cfg.step_ns);
        // 1. Crash.
        if self.plan.crash_at() == Some(t) && self.primary.is_some() {
            self.primary = None;
            while self.p_out.try_recv().is_ok() {}
            self.crashed_at = Some(t);
            self.log.push(FaultEvent::Crash { step: t });
        }
        // 2. Network.
        while let Ok(msg) = self.p_out.try_recv() {
            self.to_standby.send(t, msg);
        }
        while let Ok(msg) = self.s_out.try_recv() {
            self.to_primary.send(t, msg);
        }
        self.to_standby.recv(t, &mut self.inbox_s);
        for msg in self.inbox_s.drain(..) {
            if self.standby.is_some() {
                let _ = self.s_in.send(msg);
            }
        }
        self.to_primary.recv(t, &mut self.inbox_p);
        for msg in self.inbox_p.drain(..) {
            if self.primary.is_some() {
                let _ = self.p_in.send(msg);
            }
        }
        // 3. Standby.
        if let Some(s) = self.standby.as_mut() {
            let _ = s.poll();
        }
        // 4. Promotion.
        if self.standby.as_ref().is_some_and(|s| s.can_promote()) {
            self.promote(t)?;
        }
        // 5. Client.
        if let Some(p) = self.primary.as_mut() {
            let _ = p.journal_mut().poll();
            let mut sent = 0;
            if self.leader.is_none() {
                while sent < self.cfg.per_step && self.next < self.commands.len() {
                    if p.process(&self.commands[self.next]).is_err() {
                        break;
                    }
                    self.next += 1;
                    sent += 1;
                }
                self.acked = self.acked.max(p.journal().acked_seq());
            }
            let every = self.cfg.heartbeat_every;
            if sent == 0 || (every > 0 && t.is_multiple_of(every)) {
                let _ = p.journal_mut().heartbeat();
            }
        }
        if let Some(l) = self.leader.as_mut() {
            let mut sent = 0;
            while sent < self.cfg.per_step && self.next < self.commands.len() {
                let out = match l.process(&self.commands[self.next]) {
                    Ok(out) => out,
                    Err(never) => match never {},
                };
                self.acked = out.seq;
                self.next += 1;
                sent += 1;
            }
        }
        // 6. Check.
        if self.leaders() > 1 {
            return Err(FailoverViolation::TwoLeaders { step: t });
        }
        let survived = match (&self.standby, &self.leader) {
            (Some(s), _) => s.applied_seq(),
            (None, Some(l)) => l.journal().last_seq().unwrap_or_default(),
            (None, None) => self.acked,
        };
        if self.acked > survived {
            return Err(FailoverViolation::AckedLost {
                step: t,
                acked: self.acked,
                survived,
            });
        }
        Ok(())
    }

    /// True once every command has been acknowledged.
    pub fn is_done(&self) -> bool {
        self.acked.0 == self.commands.len() as u64
    }

    /// Runs `cfg` under `plan` until every command is acknowledged (or
    /// `max_steps`), then checks the final state hash: the leader's, and
    /// the standby's if no failover happened.
    pub fn run(
        cfg: FailoverConfig,
        plan: &FaultPlan,
    ) -> Result<FailoverOutcome, FailoverViolation> {
        let mut sim = FailoverSim::new(cfg, plan);
        while !sim.is_done() {
            if sim.now >= cfg.max_steps {
                return Err(FailoverViolation::Stuck {
                    step: sim.now,
                    acked: sim.acked,
                });
            }
            sim.step()?;
        }
        let n = Seq(sim.commands.len() as u64);
        let expected = sim.expect[n.0 as usize];
        let mut hashes = vec![sim.leader_hash()];
        if let Some(s) = &sim.standby {
            hashes.push(s.machine().state_hash());
        }
        for got in hashes {
            if got != expected {
                return Err(FailoverViolation::HashMismatch {
                    step: sim.now,
                    seq: n,
                    expected,
                    got,
                });
            }
        }
        Ok(sim.outcome())
    }

    /// Steps run so far (the clock reads `now × step_ns`).
    pub fn now(&self) -> u64 {
        self.now
    }

    /// Who leads now.
    pub fn leader(&self) -> Leader {
        if self.leader.is_some() {
            Leader::Promoted
        } else if self
            .primary
            .as_ref()
            .is_some_and(|p| p.journal().is_leader())
        {
            Leader::Primary
        } else {
            Leader::None
        }
    }

    /// The highest sequence number acknowledged to the client.
    pub fn acked(&self) -> Seq {
        self.acked
    }

    /// The client's commands, in order (`commands()[s - 1]` is the command
    /// with sequence number `s`).
    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    /// The reference state hash after command `seq`.
    ///
    /// # Panics
    /// If `seq` is beyond the stream.
    pub fn expected_hash(&self, seq: Seq) -> u64 {
        self.expect[seq.0 as usize]
    }

    /// The journal that would survive losing the primary now: the
    /// promoted leader's, else the standby's.
    pub fn surviving_journal(&self) -> Option<&Journal> {
        match (&self.leader, &self.standby) {
            (Some(l), _) => Some(l.journal()),
            (None, Some(s)) => Some(s.journal()),
            (None, None) => None,
        }
    }

    /// The state hash of whoever leads (or would: the primary while it is
    /// alive, else the standby).
    pub fn leader_hash(&self) -> u64 {
        match (&self.leader, &self.primary, &self.standby) {
            (Some(l), _, _) => l.engine().state_hash(),
            (None, Some(p), _) => p.engine().state_hash(),
            (None, None, Some(s)) => s.machine().state_hash(),
            (None, None, None) => unreachable!("the standby only goes away by promotion"),
        }
    }

    /// What the plan did so far, ordered by step.
    pub fn fault_log(&self) -> Vec<FaultEvent> {
        let mut log: Vec<FaultEvent> = self
            .to_standby
            .injector()
            .log()
            .iter()
            .chain(self.to_primary.injector().log())
            .chain(&self.log)
            .copied()
            .collect();
        log.sort_by_key(FaultEvent::step);
        log
    }

    /// The outcome so far.
    pub fn outcome(&self) -> FailoverOutcome {
        let fault_start = self.crashed_at.or_else(|| {
            self.plan
                .faults
                .iter()
                .filter(|s| s.fault == super::plan::Fault::Partition)
                .map(|s| s.from)
                .min()
        });
        let final_seq = match (&self.leader, &self.primary) {
            (Some(l), _) => l.journal().last_seq().unwrap_or_default(),
            (None, Some(p)) => p.journal().journal().last_seq().unwrap_or_default(),
            (None, None) => self.survived,
        };
        FailoverOutcome {
            steps: self.now,
            crashed_at: self.crashed_at,
            promoted_at: self.promoted_at,
            acked_at_failover: self.acked_at_failover,
            survived: self.survived,
            resent: self.resent,
            failover_steps: self
                .promoted_at
                .zip(fault_start)
                .map(|(p, f)| p.saturating_sub(f)),
            final_seq,
            final_hash: self.leader_hash(),
            standby: self
                .standby
                .as_ref()
                .map_or(self.standby_stats, |s| s.stats()),
            fault_log: self.fault_log(),
        }
    }
}

// @student-begin w48
impl FailoverSim {
    /// Leaders right now: the primary while its lease is current, plus the
    /// promoted standby.
    fn leaders(&self) -> u32 {
        let primary = self
            .primary
            .as_ref()
            .is_some_and(|p| p.journal().is_leader());
        primary as u32 + self.leader.is_some() as u32
    }

    /// The standby takes over: check what survived, then continue from it.
    fn promote(&mut self, t: u64) -> Result<(), FailoverViolation> {
        let Some(standby) = self.standby.take() else {
            return Ok(());
        };
        let stats = standby.stats();
        let (journal, engine) = match standby.promote() {
            Ok(parts) => parts,
            Err(standby) => {
                self.standby = Some(*standby);
                return Ok(());
            }
        };
        self.standby_stats = stats;
        let last = journal.last_seq().unwrap_or_default();
        if self.acked > last {
            return Err(FailoverViolation::AckedLost {
                step: t,
                acked: self.acked,
                survived: last,
            });
        }
        // Exactly a prefix of the client's stream.
        for (i, frame) in journal.iter().enumerate() {
            let cmd = frame.ok().and_then(|f| f.command().ok());
            if cmd.is_none() || cmd.as_ref() != self.commands.get(i) {
                let seq = Seq(i as u64 + 1);
                return Err(FailoverViolation::Diverged { step: t, seq });
            }
        }
        // The promoted state is the replayed journal is the reference.
        let expected = self.expect[last.0 as usize];
        let engine_cfg = EngineConfig::new(self.cfg.flow.spec)
            .with_reference_price(Price(self.cfg.flow.fair_value));
        let mut replayed = Engine::with_config(engine_cfg);
        let _ = replay(&journal, journal.first_seq(), &mut replayed);
        for got in [engine.state_hash(), replayed.state_hash()] {
            if got != expected {
                return Err(FailoverViolation::HashMismatch {
                    step: t,
                    seq: last,
                    expected,
                    got,
                });
            }
        }
        let leader = Pipeline::with_journal(journal, engine)
            .expect("the standby applied every frame it journaled");
        self.resent = (self.next as u64).saturating_sub(last.0);
        self.next = last.0 as usize;
        self.promoted_at = Some(t);
        self.acked_at_failover = self.acked;
        self.survived = last;
        self.acked = last;
        self.leader = Some(leader);
        Ok(())
    }
}
// @student-end
