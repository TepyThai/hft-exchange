//! The fault plan: what goes wrong, where and when — as data.
//!
//! A [`FaultPlan`] is a seed plus a list of [`Scheduled`] faults, each
//! active over a window of simulation steps. It is the whole description
//! of a chaos run: the same plan (and the same scenario configuration)
//! reproduces the same faults and the same outcome, bit for bit, so a
//! failing run is a line of text you can paste into a bug report:
//!
//! ```text
//! seed=81723 drop(md,150)@0..600 dup(md,50)@0..600 delay(md,80,6)@0..600 crash@412
//! ```
//!
//! ([`Display`](fmt::Display) prints that format, [`FromStr`] reads it
//! back; see [`FaultPlan`] for the grammar.)
//!
//! # Per-packet decisions are pure functions
//!
//! Every packet a link carries is numbered from 0 in send order. Whether
//! fault `i` hits packet `n` depends only on `draw(plan.stream(i), n)`:
//! the plan seed, the fault itself (not its window) and the packet number.
//! Nothing is consumed from a shared generator, so
//!
//! * a plan replays exactly, whatever else the scenario does;
//! * removing one fault, narrowing a window or lowering a rate while
//!   shrinking a failing plan ([`shrink_plan`]) leaves every other
//!   decision about the same packet unchanged — a lower rate hits a
//!   *subset* of the packets the higher one hit, because a fault hits when
//!   `draw % 1000 < per_mille`. (Where a link's traffic itself reacts to
//!   faults — a standby asking for resends — a change can still shift the
//!   numbering of later packets.)
//!
//! How the faults on one link compose for a packet is [`Fate`]'s contract.

use std::fmt;
use std::ops::Range;
use std::str::FromStr;

/// A wire the plan can damage. Every link numbers its own packets and
/// takes its own decisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Link {
    /// The UDP market-data feed: incremental packets and heartbeats (L2
    /// updates in [`ChaosVenue`](super::ChaosVenue)). Text: `md`.
    MarketData,
    /// Execution reports on a trader's order session, a TCP stream
    /// ([`ChaosVenue`](super::ChaosVenue)). Text: `exec`.
    ExecReports,
    /// Replication, primary → standby: journal frames and heartbeats.
    /// Text: `to-standby`.
    ToStandby,
    /// Replication, standby → primary: acknowledgements and resend
    /// requests. Text: `to-primary`.
    ToPrimary,
}

impl Link {
    /// Every link, in text-format order.
    pub const ALL: [Link; 4] = [
        Link::MarketData,
        Link::ExecReports,
        Link::ToStandby,
        Link::ToPrimary,
    ];

    /// The link's name in the plan text format.
    pub fn name(self) -> &'static str {
        match self {
            Link::MarketData => "md",
            Link::ExecReports => "exec",
            Link::ToStandby => "to-standby",
            Link::ToPrimary => "to-primary",
        }
    }

    /// True for the two replication links, which a
    /// [`Partition`](Fault::Partition) cuts.
    pub fn is_replication(self) -> bool {
        matches!(self, Link::ToStandby | Link::ToPrimary)
    }
}

impl fmt::Display for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One kind of fault. Rates are in per mille (`0..=1000`) per packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fault {
    /// Lose a packet. Text: `drop(link,per_mille)`.
    Drop { link: Link, per_mille: u16 },
    /// Deliver a second copy right after the first. Text:
    /// `dup(link,per_mille)`.
    Duplicate { link: Link, per_mille: u16 },
    /// Hold a packet back until the next one on the link has gone out
    /// (swap neighbours). Text: `reorder(link,per_mille)`.
    Reorder { link: Link, per_mille: u16 },
    /// Deliver `1..=max_steps` steps late (`max_steps` ≥ 1; 0 acts as 1).
    /// Text: `delay(link,per_mille,max_steps)`.
    Delay {
        link: Link,
        per_mille: u16,
        max_steps: u32,
    },
    /// Flip one bit of the packet. Text: `corrupt(link,per_mille)`.
    Corrupt { link: Link, per_mille: u16 },
    /// Primary ↔ standby: nothing gets through on either replication link.
    /// Text: `partition`.
    Partition,
    /// Consumer `consumer` stops reading (a stalled, slow client). Text:
    /// `stall(consumer)`.
    Stall { consumer: u32 },
    /// The primary process dies at the window's first step (the window's
    /// end is always `from + 1`). Text: `crash`.
    CrashPrimary,
}

impl Fault {
    /// The link a per-packet fault acts on; `None` for process faults and
    /// for [`Partition`](Fault::Partition) (which acts on both replication
    /// links, see [`acts_on`](Fault::acts_on)).
    pub fn link(&self) -> Option<Link> {
        match *self {
            Fault::Drop { link, .. }
            | Fault::Duplicate { link, .. }
            | Fault::Reorder { link, .. }
            | Fault::Delay { link, .. }
            | Fault::Corrupt { link, .. } => Some(link),
            Fault::Partition | Fault::Stall { .. } | Fault::CrashPrimary => None,
        }
    }

    /// True if this fault judges packets on `link`.
    pub fn acts_on(&self, link: Link) -> bool {
        match self {
            Fault::Partition => link.is_replication(),
            _ => self.link() == Some(link),
        }
    }

    /// The per-packet hit rate of a random fault, in per mille.
    pub fn per_mille(&self) -> Option<u16> {
        match *self {
            Fault::Drop { per_mille, .. }
            | Fault::Duplicate { per_mille, .. }
            | Fault::Reorder { per_mille, .. }
            | Fault::Delay { per_mille, .. }
            | Fault::Corrupt { per_mille, .. } => Some(per_mille),
            _ => None,
        }
    }

    /// The same fault with another rate (random faults only; others are
    /// returned unchanged). Used by [`shrink_plan`].
    pub fn with_per_mille(self, p: u16) -> Fault {
        match self {
            Fault::Drop { link, .. } => Fault::Drop { link, per_mille: p },
            Fault::Duplicate { link, .. } => Fault::Duplicate { link, per_mille: p },
            Fault::Reorder { link, .. } => Fault::Reorder { link, per_mille: p },
            Fault::Delay {
                link, max_steps, ..
            } => Fault::Delay {
                link,
                per_mille: p,
                max_steps,
            },
            Fault::Corrupt { link, .. } => Fault::Corrupt { link, per_mille: p },
            other => other,
        }
    }

    /// A digest of the fault's content without its window: the identity
    /// its random stream is derived from. O(1).
    fn key(&self) -> u64 {
        let link = |l: Link| l as u64 + 1;
        let words: [u64; 4] = match *self {
            Fault::Drop { link: l, per_mille } => [1, link(l), per_mille as u64, 0],
            Fault::Duplicate { link: l, per_mille } => [2, link(l), per_mille as u64, 0],
            Fault::Reorder { link: l, per_mille } => [3, link(l), per_mille as u64, 0],
            Fault::Delay {
                link: l,
                per_mille,
                max_steps,
            } => [4, link(l), per_mille as u64, max_steps as u64],
            Fault::Corrupt { link: l, per_mille } => [5, link(l), per_mille as u64, 0],
            Fault::Partition => [6, 0, 0, 0],
            Fault::Stall { consumer } => [7, consumer as u64, 0, 0],
            Fault::CrashPrimary => [8, 0, 0, 0],
        };
        // Rates are deliberately left out: lowering a rate while shrinking
        // must keep the same draws (then it hits a subset of the packets).
        let [kind, a, _rate, b] = words;
        mix64(mix64(mix64(kind) ^ a) ^ b)
    }
}

/// A fault and the steps it is active in: `from <= step < to`. What a
/// "step" is belongs to the scenario (a simulation step, a venue flush).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Scheduled {
    pub fault: Fault,
    pub from: u64,
    pub to: u64,
}

impl Scheduled {
    /// True if the fault is active at `step`.
    pub fn is_active(&self, step: u64) -> bool {
        self.from <= step && step < self.to
    }
}

/// A deterministic, replayable fault-injection plan: a seed and a list of
/// scheduled faults (see the [module docs](self)).
///
/// # Text format
///
/// Whitespace-separated tokens; [`Display`](fmt::Display) writes the
/// canonical form and [`FromStr`] reads it (any order of fault tokens,
/// `seed=` first):
///
/// ```text
/// plan    := "seed=" u64 (" " fault "@" window)*
/// fault   := "drop(" link "," pm ")" | "dup(" link "," pm ")"
///          | "reorder(" link "," pm ")" | "delay(" link "," pm "," steps ")"
///          | "corrupt(" link "," pm ")" | "partition" | "stall(" consumer ")"
///          | "crash"
/// window  := from ".." to          (crash: just the step)
/// link    := "md" | "exec" | "to-standby" | "to-primary"
/// pm      := 0..=1000              (per mille)
/// ```
///
/// ```
/// use sim::chaos::{Fault, FaultPlan, Link};
///
/// let plan = FaultPlan::new(7)
///     .with(Fault::Drop { link: Link::MarketData, per_mille: 50 }, 0..800)
///     .with(Fault::CrashPrimary, 450..451);
/// let text = plan.to_string();
/// assert_eq!(text, "seed=7 drop(md,50)@0..800 crash@450");
/// assert_eq!(text.parse::<FaultPlan>().unwrap(), plan);
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct FaultPlan {
    /// Seeds every per-packet decision.
    pub seed: u64,
    /// The faults, in plan order (the order in which a packet meets them).
    pub faults: Vec<Scheduled>,
}

impl FaultPlan {
    /// An empty plan: nothing goes wrong.
    pub fn new(seed: u64) -> FaultPlan {
        FaultPlan {
            seed,
            faults: Vec::new(),
        }
    }

    /// Adds `fault`, active over `steps` (a crash only at `steps.start`).
    pub fn with(mut self, fault: Fault, steps: Range<u64>) -> FaultPlan {
        let to = match fault {
            Fault::CrashPrimary => steps.start + 1,
            _ => steps.end,
        };
        self.faults.push(Scheduled {
            fault,
            from: steps.start,
            to,
        });
        self
    }

    /// The same faults with another seed: same schedule, different packets.
    pub fn reseeded(&self, seed: u64) -> FaultPlan {
        FaultPlan {
            seed,
            faults: self.faults.clone(),
        }
    }

    /// The plan without fault `index` (for shrinking).
    ///
    /// # Panics
    /// If `index` is out of range.
    pub fn without(&self, index: usize) -> FaultPlan {
        let mut p = self.clone();
        p.faults.remove(index);
        p
    }

    /// The random stream of fault `index`: derived from the seed, the
    /// fault's content (not its window or rate) and how many identical
    /// faults precede it, so editing other faults does not change it.
    /// O(index).
    ///
    /// # Panics
    /// If `index` is out of range.
    pub fn stream(&self, index: usize) -> u64 {
        let key = self.faults[index].fault.key();
        let twins = self.faults[..index]
            .iter()
            .filter(|s| s.fault.key() == key)
            .count() as u64;
        mix64(self.seed ^ mix64(key ^ twins.wrapping_mul(GOLDEN)))
    }

    /// The step at which the primary crashes (the earliest
    /// [`CrashPrimary`](Fault::CrashPrimary)), if any.
    pub fn crash_at(&self) -> Option<u64> {
        self.faults
            .iter()
            .filter(|s| s.fault == Fault::CrashPrimary)
            .map(|s| s.from)
            .min()
    }

    /// True if a [`Partition`](Fault::Partition) is active at `step`.
    pub fn partitioned(&self, step: u64) -> bool {
        self.faults
            .iter()
            .any(|s| s.fault == Fault::Partition && s.is_active(step))
    }

    /// True if consumer `consumer` is stalled at `step`.
    pub fn stalled(&self, consumer: u32, step: u64) -> bool {
        self.faults
            .iter()
            .any(|s| s.fault == Fault::Stall { consumer } && s.is_active(step))
    }

    /// The first step from which no fault is active any more (0 for an
    /// empty plan): the run's "faults stop" point for liveness checks.
    pub fn quiet_from(&self) -> u64 {
        self.faults.iter().map(|s| s.to).max().unwrap_or(0)
    }
}

/// The `packet`-th output (from 0) of a SplitMix64 generator seeded with
/// `stream` — what [`types::Rng::new(stream)`](types::Rng) returns on its
/// `packet + 1`-th call — computed directly. O(1).
pub fn draw(stream: u64, packet: u64) -> u64 {
    mix64(stream.wrapping_add(packet.wrapping_add(1).wrapping_mul(GOLDEN)))
}

const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

/// The SplitMix64 finalizer: a bijective 64-bit mix.
fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// What a plan does to one packet on one link.
///
/// # How faults compose (the injector's contract)
///
/// For packet number `n` sent at `step` on a link, the faults that act on
/// the link and are active at `step` are consulted:
///
/// 1. An active [`Partition`](Fault::Partition) loses the packet
///    (`partitioned`); nothing else is consulted.
/// 2. Otherwise each random fault, **in plan order**, takes
///    `r = draw(plan.stream(i), n)` and hits iff `r % 1000 < per_mille`:
///    * `Drop` sets `dropped` and stops: nothing after it is consulted;
///    * `Corrupt` sets `corrupt = Some((r >> 32) as u32)` (the bit
///      selector; a later hit replaces it);
///    * `Duplicate` sets `duplicate`;
///    * `Delay` adds `1 + (r >> 32) % max_steps` to `delay`;
///    * `Reorder` sets `reorder`.
///
/// Packet numbers count every packet judged on the link, hit or not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Fate {
    /// Lost to a random drop.
    pub dropped: bool,
    /// Lost to a partition.
    pub partitioned: bool,
    /// Damaged: flip bit `selector % bits` of the packet.
    pub corrupt: Option<u32>,
    /// A second copy follows the first.
    pub duplicate: bool,
    /// Delivered this many steps late.
    pub delay: u64,
    /// Held back until the next packet on the link has gone out.
    pub reorder: bool,
}

impl Fate {
    /// Delivered now, intact, once.
    pub const CLEAN: Fate = Fate {
        dropped: false,
        partitioned: false,
        corrupt: None,
        duplicate: false,
        delay: 0,
        reorder: false,
    };

    /// True if nothing happens to the packet.
    pub fn is_clean(&self) -> bool {
        *self == Fate::CLEAN
    }

    /// True if the packet never arrives.
    pub fn is_lost(&self) -> bool {
        self.dropped || self.partitioned
    }
}

/// One entry of a run's fault log: what the plan actually did. Two runs of
/// the same plan produce identical logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FaultEvent {
    /// Packet `packet` on `link`, sent at `step`, met a fault.
    Packet {
        step: u64,
        link: Link,
        packet: u64,
        fate: Fate,
    },
    /// The primary crashed.
    Crash { step: u64 },
    /// A consumer stopped reading.
    Stall { step: u64, consumer: u32 },
    /// A stalled consumer started reading again.
    Resume { step: u64, consumer: u32 },
}

impl FaultEvent {
    /// The step it happened at.
    pub fn step(&self) -> u64 {
        match *self {
            FaultEvent::Packet { step, .. }
            | FaultEvent::Crash { step }
            | FaultEvent::Stall { step, .. }
            | FaultEvent::Resume { step, .. } => step,
        }
    }
}

/// Shrinks a failing plan: returns a plan for which `fails` is still true,
/// with as few faults, windows as narrow and rates as low as greedy search
/// finds. Deterministic; the seed is kept (so are the draws, see the
/// [module docs](self)). Rounds of
///
/// 1. removing whole faults, one at a time, while the plan still fails;
/// 2. narrowing each window: raising `from`, then lowering `to`, by
///    halving steps (a crash is a point: left alone);
/// 3. lowering each random fault's rate, by halving steps, down to 1 ‰ (a
///    lower rate hits a subset of the same packets),
///
/// repeat until a round changes nothing. The result is 1-minimal: removing
/// any single remaining fault makes it pass. Each round calls `fails`
/// O(faults² + faults · log(window · 1000)) times; there are rarely more
/// than two.
///
/// # Panics
/// If `fails(plan)` is false.
pub fn shrink_plan(plan: &FaultPlan, mut fails: impl FnMut(&FaultPlan) -> bool) -> FaultPlan {
    assert!(fails(plan), "shrink_plan needs a failing plan");
    let mut best = plan.clone();
    loop {
        let before = best.clone();
        // 1. Remove faults until no single removal still fails.
        let mut i = 0;
        while i < best.faults.len() {
            let candidate = best.without(i);
            if fails(&candidate) {
                best = candidate;
                i = 0;
            } else {
                i += 1;
            }
        }
        // 2. Narrow windows.
        for i in 0..best.faults.len() {
            if best.faults[i].fault == Fault::CrashPrimary {
                continue;
            }
            let mut step = (best.faults[i].to - best.faults[i].from) / 2;
            while step > 0 {
                let mut candidate = best.clone();
                let s = &mut candidate.faults[i];
                s.from += step;
                if s.from < s.to && fails(&candidate) {
                    best = candidate;
                } else {
                    step /= 2;
                }
            }
            let mut step = (best.faults[i].to - best.faults[i].from) / 2;
            while step > 0 {
                let mut candidate = best.clone();
                let s = &mut candidate.faults[i];
                match s.to.checked_sub(step).filter(|&to| to > s.from) {
                    Some(to) => s.to = to,
                    None => {
                        step /= 2;
                        continue;
                    }
                }
                if fails(&candidate) {
                    best = candidate;
                } else {
                    step /= 2;
                }
            }
        }
        // 3. Lower rates.
        for i in 0..best.faults.len() {
            let Some(p) = best.faults[i].fault.per_mille() else {
                continue;
            };
            let mut step = p / 2;
            while step > 0 {
                let current = best.faults[i].fault.per_mille().unwrap_or(0);
                if current <= step {
                    step /= 2;
                    continue;
                }
                let mut candidate = best.clone();
                candidate.faults[i].fault = best.faults[i].fault.with_per_mille(current - step);
                if fails(&candidate) {
                    best = candidate;
                } else {
                    step /= 2;
                }
            }
        }
        if best == before {
            return best;
        }
    }
}

impl fmt::Display for Scheduled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.fault {
            Fault::Drop { link, per_mille } => write!(f, "drop({link},{per_mille})")?,
            Fault::Duplicate { link, per_mille } => write!(f, "dup({link},{per_mille})")?,
            Fault::Reorder { link, per_mille } => write!(f, "reorder({link},{per_mille})")?,
            Fault::Delay {
                link,
                per_mille,
                max_steps,
            } => write!(f, "delay({link},{per_mille},{max_steps})")?,
            Fault::Corrupt { link, per_mille } => write!(f, "corrupt({link},{per_mille})")?,
            Fault::Partition => f.write_str("partition")?,
            Fault::Stall { consumer } => write!(f, "stall({consumer})")?,
            Fault::CrashPrimary => return write!(f, "crash@{}", self.from),
        }
        write!(f, "@{}..{}", self.from, self.to)
    }
}

impl fmt::Display for FaultPlan {
    /// The canonical text form (see [`FaultPlan`]).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "seed={}", self.seed)?;
        for s in &self.faults {
            write!(f, " {s}")?;
        }
        Ok(())
    }
}

/// A plan text that does not follow the format (see [`FaultPlan`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsePlanError {
    /// The offending token.
    pub token: String,
    /// What is wrong with it.
    pub reason: &'static str,
}

impl fmt::Display for ParsePlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bad fault-plan token `{}`: {}", self.token, self.reason)
    }
}

impl std::error::Error for ParsePlanError {}

impl FromStr for FaultPlan {
    type Err = ParsePlanError;

    /// Parses the text form (see [`FaultPlan`]). O(len).
    fn from_str(s: &str) -> Result<FaultPlan, ParsePlanError> {
        let mut tokens = s.split_whitespace();
        let err = |token: &str, reason| ParsePlanError {
            token: token.to_string(),
            reason,
        };
        let first = tokens.next().ok_or_else(|| err("", "empty plan"))?;
        let seed = first
            .strip_prefix("seed=")
            .ok_or_else(|| err(first, "a plan starts with seed=N"))?
            .parse()
            .map_err(|_| err(first, "the seed is not a u64"))?;
        let mut plan = FaultPlan::new(seed);
        for token in tokens {
            plan.faults
                .push(parse_scheduled(token).map_err(|reason| err(token, reason))?);
        }
        Ok(plan)
    }
}

fn parse_scheduled(token: &str) -> Result<Scheduled, &'static str> {
    let (spec, window) = token.split_once('@').ok_or("missing @window")?;
    let (name, args) = match spec.split_once('(') {
        Some((name, rest)) => (
            name,
            rest.strip_suffix(')')
                .ok_or("unclosed argument list")?
                .split(',')
                .collect::<Vec<_>>(),
        ),
        None => (spec, Vec::new()),
    };
    let link = |i: usize| -> Result<Link, &'static str> {
        let a = args.get(i).ok_or("missing link")?;
        Link::ALL
            .into_iter()
            .find(|l| l.name() == *a)
            .ok_or("unknown link")
    };
    let num = |i: usize| -> Result<u64, &'static str> {
        args.get(i)
            .ok_or("missing argument")?
            .parse()
            .map_err(|_| "argument is not a number")
    };
    let rate = |i: usize| -> Result<u16, &'static str> {
        match num(i)? {
            p @ 0..=1000 => Ok(p as u16),
            _ => Err("rate is per mille: 0..=1000"),
        }
    };
    let arity = |n: usize| {
        if args.len() == n {
            Ok(())
        } else {
            Err("wrong number of arguments")
        }
    };
    let fault = match name {
        "drop" => {
            arity(2)?;
            Fault::Drop {
                link: link(0)?,
                per_mille: rate(1)?,
            }
        }
        "dup" => {
            arity(2)?;
            Fault::Duplicate {
                link: link(0)?,
                per_mille: rate(1)?,
            }
        }
        "reorder" => {
            arity(2)?;
            Fault::Reorder {
                link: link(0)?,
                per_mille: rate(1)?,
            }
        }
        "delay" => {
            arity(3)?;
            let max_steps = u32::try_from(num(2)?).map_err(|_| "max_steps too large")?;
            if max_steps == 0 {
                return Err("max_steps must be at least 1");
            }
            Fault::Delay {
                link: link(0)?,
                per_mille: rate(1)?,
                max_steps,
            }
        }
        "corrupt" => {
            arity(2)?;
            Fault::Corrupt {
                link: link(0)?,
                per_mille: rate(1)?,
            }
        }
        "partition" => {
            arity(0)?;
            Fault::Partition
        }
        "stall" => {
            arity(1)?;
            Fault::Stall {
                consumer: u32::try_from(num(0)?).map_err(|_| "consumer too large")?,
            }
        }
        "crash" => {
            arity(0)?;
            let at = window.parse().map_err(|_| "crash@N takes one step")?;
            return Ok(Scheduled {
                fault: Fault::CrashPrimary,
                from: at,
                to: at + 1,
            });
        }
        _ => return Err("unknown fault"),
    };
    let (from, to) = window.split_once("..").ok_or("window is from..to")?;
    let from: u64 = from.parse().map_err(|_| "window start is not a u64")?;
    let to: u64 = to.parse().map_err(|_| "window end is not a u64")?;
    if from > to {
        return Err("window ends before it starts");
    }
    Ok(Scheduled { fault, from, to })
}
