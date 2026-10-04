//! Market data: L2 incrementals and snapshots, client book builder with gap recovery, fan-out, SIMD aggregation, capture replay.
//!
//! Curriculum: Weeks 10, 16, 29, 46 (see <https://tepy.dev/hft>).
//!
//! # Week 10 — books on the wire
//!
//! The engine's [`Event`](types::Event)s describe the book *by order* (L3).
//! Most consumers want it *by price* (L2): the total resting quantity at
//! each price on each side. This crate turns one instrument's event stream
//! into an L2 feed and rebuilds books from that feed on the other side.
//!
//! * [`L2Publisher`] consumes events and emits one [`L2Update`] for every
//!   change to a price level, numbered with its own dense feed sequence
//!   (1, 2, 3, …), plus full or top-N [`L2Snapshot`]s on demand or
//!   periodically.
//! * [`BookBuilder`] is the client: it applies a snapshot, then the
//!   incrementals that follow it; it detects sequence gaps (lost packets),
//!   buffers incrementals while waiting for a fresh snapshot, and resyncs
//!   by discarding buffered updates the snapshot already covers.
//!
//! ## The feed contract
//!
//! * Update `n` carries the new aggregate quantity of one level (`qty == 0`
//!   means the level is gone). Applying it is idempotent: it *sets* the
//!   level, it does not add to it.
//! * Updates are not conflated: every level change is its own update, in
//!   the order the events happened, so a consumer that applies `1..=n`
//!   holds exactly the publisher's book after update `n`.
//! * A snapshot with `seq == s` is the book after update `s` (`s == 0`:
//!   before any update). A consumer applies it, drops buffered updates
//!   `<= s`, then needs `s + 1` next.
//!
//! The book by price is kept in a [`DepthBook`]: [`MapDepth`] (ordered
//! maps) by default; both the publisher and the builder are generic over it.
//!
//! # Week 16 — fan-out
//!
//! * [`broadcast`] creates a single-writer broadcast ring ([`RingPublisher`],
//!   [`BroadcastRing`], [`Reader`]): the writer never waits; each reader
//!   has its own cursor and learns from [`Recv::Lagged`] exactly how much
//!   it missed when it fell more than the capacity behind. Slots are
//!   per-slot seqlocks over atomic words ([`RingValue`]).
//! * [`Conflator`] keeps only the latest update per level for a slow
//!   consumer, in first-dirty order.
//! * [`ws`] is a std-only WebSocket adapter (RFC 6455 handshake with
//!   in-crate SHA-1/base64, framing) and [`serve_ws`] streams a snapshot
//!   then conflated update batches ([`json`]) to each browser.
//!
//! # Week 29 — SIMD
//!
//! [`LadderDepth`] is a [`DepthBook`] of dense quantity arrays around the
//! touch, best level first on both sides, so depth aggregation and level
//! search become forward scans that [`Kernels`] vectorize (AVX2 with
//! runtime detection, chosen once; a scalar fallback with identical
//! results).
//!
//! # Week 46 — replay from captures
//!
//! * [`feed`] defines the binary packet layout of the feed (sequenced
//!   incremental packets, snapshot packets).
//! * [`pcap`] writes and reads libpcap captures of Ethernet/IPv4/UDP
//!   frames carrying those packets.
//! * [`ReorderBuffer`] restores sequence order (late packets buffered,
//!   duplicates dropped, holes given up after a window).
//! * [`Replayer`] / [`replay_capture`] rebuild the book with a
//!   [`BookBuilder`], recover from gaps with recorded snapshots and verify
//!   the book against them.

pub mod builder;
pub mod conflate;
pub mod depth;
pub mod feed;
pub mod json;
pub mod ladder;
pub mod pcap;
pub mod publisher;
pub mod reorder;
pub mod replay;
pub mod ring;
pub mod simd;
pub mod ws;

pub use builder::{BookBuilder, SnapshotOutcome, UpdateOutcome};
pub use conflate::Conflator;
pub use depth::{DepthBook, MapDepth};
pub use ladder::LadderDepth;
pub use publisher::L2Publisher;
pub use reorder::{PushOutcome, ReorderBuffer};
pub use replay::{replay_capture, FeedRecorder, ReplayStats, Replayer};
pub use ring::{broadcast, BroadcastRing, Reader, Recv, RingPublisher, RingValue};
pub use simd::Kernels;
pub use ws::{serve_ws, serve_ws_client, SnapshotCell};

use types::{Price, Qty, Seq, Side};

/// Aggregated quantity resting at one price.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Level {
    pub price: Price,
    pub qty: Qty,
}

/// One incremental L2 change: level `(side, price)` now holds `qty` in
/// total (`0` = the level was removed). 32 bytes, `Copy` — cheap to put in
/// a ring buffer (week 16).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct L2Update {
    /// Feed sequence number: dense, starting at 1, one per update. Not the
    /// engine's command sequence — one command can cause several updates
    /// (a sweep through three levels) or none (a rejected order).
    pub seq: Seq,
    pub side: Side,
    pub price: Price,
    /// New aggregate quantity at this level; zero removes the level.
    pub qty: Qty,
}

/// The book by price as of feed sequence number `seq`: every update
/// `<= seq` is reflected, none after it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct L2Snapshot {
    pub seq: Seq,
    /// Best (highest) price first.
    pub bids: Vec<Level>,
    /// Best (lowest) price first.
    pub asks: Vec<Level>,
}

impl L2Snapshot {
    /// The levels of one side, best first.
    pub fn levels(&self, side: Side) -> &[Level] {
        match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        }
    }

    /// Highest bid, if any.
    pub fn best_bid(&self) -> Option<Level> {
        self.bids.first().copied()
    }

    /// Lowest ask, if any.
    pub fn best_ask(&self) -> Option<Level> {
        self.asks.first().copied()
    }
}
