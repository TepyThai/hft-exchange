//! Week 21: stage timestamps carried with an order.
//!
//! Each component on the order path stamps the order as it finishes its
//! work: the gateway when the bytes arrive and when the message is decoded,
//! risk when the checks pass, the engine after matching, and so on. The
//! stamps travel with the order (they are a small `Copy` value, one cache
//! line), and week 39's [`Attribution`](crate::Attribution) turns many of
//! them into a per-stage latency breakdown.
//!
//! A stamp marks the moment a stage *finished*; the time spent in a stage
//! (including any queueing before it) is the gap to the previous stamp.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::clock::Clock;

/// A point on the order path. A stamp for stage `S` means "the order has
/// just left `S`"; the interval ending at `S` is the time spent in `S`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Stage {
    /// The order's bytes arrived at the gateway — the "tick" in tick-to-ack.
    /// Usually the first stamp.
    Ingress = 0,
    /// The gateway decoded and validated the message into a `Command`.
    Gateway = 1,
    /// Pre-trade risk checks finished.
    Risk = 2,
    /// The command was sequenced and journaled.
    Journal = 3,
    /// The engine finished matching and produced its events.
    Match = 4,
    /// Market data for those events was published.
    Publish = 5,
    /// The ack / execution report was written back to the client.
    Ack = 6,
}

impl Stage {
    /// Number of stages.
    pub const COUNT: usize = 7;

    /// Every stage, in pipeline order.
    pub const ALL: [Stage; Stage::COUNT] = [
        Stage::Ingress,
        Stage::Gateway,
        Stage::Risk,
        Stage::Journal,
        Stage::Match,
        Stage::Publish,
        Stage::Ack,
    ];

    /// Lower-case name, used in reports and as a Prometheus label value.
    pub fn name(self) -> &'static str {
        match self {
            Stage::Ingress => "ingress",
            Stage::Gateway => "gateway",
            Stage::Risk => "risk",
            Stage::Journal => "journal",
            Stage::Match => "match",
            Stage::Publish => "publish",
            Stage::Ack => "ack",
        }
    }

    /// Position in [`Stage::ALL`] (`0..COUNT`).
    pub fn index(self) -> usize {
        self as usize
    }

    /// The stage at `index` in [`Stage::ALL`], if any.
    pub fn from_index(index: usize) -> Option<Stage> {
        Stage::ALL.get(index).copied()
    }
}

/// Up to [`Stage::COUNT`] `(stage, nanoseconds)` stamps in the order they
/// were taken — one 64-byte cache line, `Copy`, no allocation.
///
/// Each stage may be stamped once. Stamps are kept in insertion order (the
/// path an order actually took), and timestamps are not required to be
/// monotonic: two threads reading a TSC can disagree by a few ns, so
/// intervals saturate at zero instead of going negative.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StageStamps {
    // TODO(w21): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl StageStamps {
    /// No stamps yet.
    pub fn new() -> StageStamps {
        todo!("w21: new")
    }

    /// Records that the order left `stage` at `t_ns`. Returns `false` (and
    /// records nothing) if `stage` was already stamped. O(stamps) ≤ 7
    /// compares, no allocation.
    #[inline]
    pub fn stamp(&mut self, stage: Stage, t_ns: u64) -> bool {
        todo!("w21: stamp")
    }

    /// [`stamp`](Self::stamp) with `clock.now()`.
    #[inline]
    pub fn stamp_now(&mut self, stage: Stage, clock: &Clock) -> bool {
        todo!("w21: stamp_now")
    }

    /// Number of stamps taken.
    pub fn len(&self) -> usize {
        todo!("w21: len")
    }

    /// True if no stamp was taken.
    pub fn is_empty(&self) -> bool {
        todo!("w21: is_empty")
    }

    /// The `i`-th stamp in insertion order.
    pub fn at(&self, i: usize) -> Option<(Stage, u64)> {
        todo!("w21: at")
    }

    /// When the order left `stage`, if it was stamped.
    pub fn get(&self, stage: Stage) -> Option<u64> {
        todo!("w21: get")
    }

    /// Time spent in `stage`: its stamp minus the previous stamp (saturating
    /// at 0). `None` if `stage` was not stamped or is the first stamp.
    pub fn interval(&self, stage: Stage) -> Option<u64> {
        todo!("w21: interval")
    }

    /// Last stamp minus first stamp (saturating), or 0 with fewer than two
    /// stamps — the end-to-end latency.
    pub fn total(&self) -> u64 {
        todo!("w21: total")
    }

    /// Removes every stamp (to reuse the value for the next order).
    pub fn clear(&mut self) {
        todo!("w21: clear")
    }
}

impl StageStamps {
    /// The stamps in insertion order.
    pub fn iter(&self) -> StampIter<'_> {
        StampIter {
            stamps: self,
            next: 0,
        }
    }
}

impl Default for StageStamps {
    fn default() -> StageStamps {
        StageStamps::new()
    }
}

impl<'a> IntoIterator for &'a StageStamps {
    type Item = (Stage, u64);
    type IntoIter = StampIter<'a>;

    fn into_iter(self) -> StampIter<'a> {
        self.iter()
    }
}

/// Iterator over a [`StageStamps`]' `(stage, ns)` pairs in insertion order.
#[derive(Clone, Debug)]
pub struct StampIter<'a> {
    stamps: &'a StageStamps,
    next: usize,
}

impl Iterator for StampIter<'_> {
    type Item = (Stage, u64);

    fn next(&mut self) -> Option<(Stage, u64)> {
        let item = self.stamps.at(self.next)?;
        self.next += 1;
        Some(item)
    }
}
