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
// @student w21
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StageStamps {
    times: [u64; Stage::COUNT],
    stages: [Stage; Stage::COUNT],
    len: u8,
}

// @student w21
impl StageStamps {
    /// No stamps yet.
    pub fn new() -> StageStamps {
        StageStamps {
            times: [0; Stage::COUNT],
            stages: [Stage::Ingress; Stage::COUNT],
            len: 0,
        }
    }

    /// Records that the order left `stage` at `t_ns`. Returns `false` (and
    /// records nothing) if `stage` was already stamped. O(stamps) ≤ 7
    /// compares, no allocation.
    #[inline]
    pub fn stamp(&mut self, stage: Stage, t_ns: u64) -> bool {
        let len = self.len as usize;
        if self.stages[..len].contains(&stage) {
            return false;
        }
        // Distinct stages never exceed the capacity.
        self.stages[len] = stage;
        self.times[len] = t_ns;
        self.len += 1;
        true
    }

    /// [`stamp`](Self::stamp) with `clock.now()`.
    #[inline]
    pub fn stamp_now(&mut self, stage: Stage, clock: &Clock) -> bool {
        self.stamp(stage, clock.now())
    }

    /// Number of stamps taken.
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// True if no stamp was taken.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The `i`-th stamp in insertion order.
    pub fn at(&self, i: usize) -> Option<(Stage, u64)> {
        if i < self.len as usize {
            Some((self.stages[i], self.times[i]))
        } else {
            None
        }
    }

    /// When the order left `stage`, if it was stamped.
    pub fn get(&self, stage: Stage) -> Option<u64> {
        let len = self.len as usize;
        self.stages[..len]
            .iter()
            .position(|&s| s == stage)
            .map(|i| self.times[i])
    }

    /// Time spent in `stage`: its stamp minus the previous stamp (saturating
    /// at 0). `None` if `stage` was not stamped or is the first stamp.
    pub fn interval(&self, stage: Stage) -> Option<u64> {
        let len = self.len as usize;
        let i = self.stages[..len].iter().position(|&s| s == stage)?;
        if i == 0 {
            return None;
        }
        Some(self.times[i].saturating_sub(self.times[i - 1]))
    }

    /// Last stamp minus first stamp (saturating), or 0 with fewer than two
    /// stamps — the end-to-end latency.
    pub fn total(&self) -> u64 {
        let len = self.len as usize;
        if len < 2 {
            return 0;
        }
        self.times[len - 1].saturating_sub(self.times[0])
    }

    /// Removes every stamp (to reuse the value for the next order).
    pub fn clear(&mut self) {
        self.len = 0;
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
