//! Week 46 — market replay from captures: packets → books, verified.
//!
//! [`FeedRecorder`] (provided) writes the feed the way a capture appliance
//! would see it: incremental packets to one multicast group, snapshot
//! packets to another, each a full Ethernet/IPv4/UDP frame in a pcap file.
//!
//! [`Replayer`] rebuilds the book from such a capture:
//!
//! ```text
//! pcap record ─ parse_udp ─ FeedPacket::decode ─┬─ incremental ─ ReorderBuffer ─ BookBuilder
//!                                               └─ snapshot ─── recover, or verify
//! ```
//!
//! * Updates go through a [`ReorderBuffer`](crate::ReorderBuffer): late packets are put back in
//!   order, duplicates dropped, and a hole that stays open for a whole
//!   window is given up, which the [`BookBuilder`] sees as a gap.
//! * A recorded snapshot either **recovers** the book — when the builder
//!   is out of sync, or the reorder buffer is waiting on a hole the
//!   snapshot covers — or is **verified**: once the rebuilt book reaches
//!   the snapshot's sequence number, every level must match. A mismatch is
//!   a bug in the publisher, the capture or the replayer, and is the first
//!   thing to look at in an incident.
//!
//! Replay is deterministic: the same capture always gives the same book
//! and the same [`ReplayStats`].

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::feed::{encode_incremental, encode_snapshot, MAX_UPDATES_PER_PACKET};
use crate::pcap::{PcapError, PcapWriter};
use crate::{BookBuilder, DepthBook, L2Snapshot, L2Update, MapDepth};
use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddrV4};
use types::Seq;

// TODO(w46): implement — see the week guide on tepy.dev/hft.

/// Where the recorded publisher sends from.
pub const FEED_SOURCE: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 1), 40_000);
/// Multicast group of the incremental channel.
pub const INCREMENTAL_GROUP: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(239, 1, 1, 1), 30_001);
/// Multicast group of the snapshot (recovery) channel.
pub const SNAPSHOT_GROUP: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(239, 1, 1, 2), 30_002);

/// Writes a feed into a pcap capture: consecutive updates batched into
/// incremental packets (at most `max_batch` each), snapshots as their own
/// packets after the updates they reflect.
#[derive(Debug)]
pub struct FeedRecorder<W: Write> {
    pcap: PcapWriter<W>,
    pending: Vec<L2Update>,
    max_batch: usize,
    packet: Vec<u8>,
}

impl<W: Write> FeedRecorder<W> {
    /// A recorder batching up to 32 updates per packet.
    pub fn new(out: W) -> io::Result<FeedRecorder<W>> {
        FeedRecorder::with_batch(out, 32)
    }

    /// A recorder batching up to `max_batch` updates per packet
    /// (`1..=MAX_UPDATES_PER_PACKET`).
    pub fn with_batch(out: W, max_batch: usize) -> io::Result<FeedRecorder<W>> {
        assert!((1..=MAX_UPDATES_PER_PACKET).contains(&max_batch));
        Ok(FeedRecorder {
            pcap: PcapWriter::new(out)?,
            pending: Vec::with_capacity(max_batch),
            max_batch,
            packet: Vec::with_capacity(2048),
        })
    }

    /// Records one update (sent when the batch is full, a snapshot is
    /// recorded, the sequence jumps, or on [`flush`](FeedRecorder::flush)).
    pub fn record_update(&mut self, ts_ns: u64, u: &L2Update) -> io::Result<()> {
        if self.pending.last().is_some_and(|p| p.seq.0 + 1 != u.seq.0) {
            self.flush(ts_ns)?;
        }
        self.pending.push(*u);
        if self.pending.len() == self.max_batch {
            self.flush(ts_ns)?;
        }
        Ok(())
    }

    /// Sends pending updates, then the snapshot.
    pub fn record_snapshot(&mut self, ts_ns: u64, snap: &L2Snapshot) -> io::Result<()> {
        self.flush(ts_ns)?;
        self.packet.clear();
        encode_snapshot(snap, &mut self.packet);
        self.pcap
            .write_udp(ts_ns, FEED_SOURCE, SNAPSHOT_GROUP, &self.packet)
    }

    /// Sends pending updates as one packet.
    pub fn flush(&mut self, ts_ns: u64) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        self.packet.clear();
        encode_incremental(&self.pending, &mut self.packet);
        self.pending.clear();
        self.pcap
            .write_udp(ts_ns, FEED_SOURCE, INCREMENTAL_GROUP, &self.packet)
    }

    /// Flushes and returns the underlying writer.
    pub fn finish(mut self, ts_ns: u64) -> io::Result<W> {
        self.flush(ts_ns)?;
        self.pcap.into_inner()
    }
}

/// Counters of one replay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReplayStats {
    /// Capture records seen.
    pub records: u64,
    /// Records that were not valid feed packets (bad frame or encoding).
    pub malformed: u64,
    pub incremental_packets: u64,
    pub snapshot_packets: u64,
    /// Updates applied to the book (in sequence, including those replayed
    /// on top of a recovery snapshot).
    pub updates: u64,
    /// Update copies dropped as already delivered or buffered.
    pub duplicates: u64,
    /// Sequence numbers the reorder buffer gave up on.
    pub lost: u64,
    /// Snapshots applied to recover from a gap.
    pub recoveries: u64,
    /// Snapshots that matched the rebuilt book at their sequence number.
    pub verified: u64,
    /// Snapshots that did not.
    pub mismatches: u64,
    /// Snapshots that could not be compared (the book was past them or
    /// out of sync when they arrived).
    pub unverified: u64,
}

/// Rebuilds one book from captured feed packets (see the module docs).
#[derive(Clone, Debug)]
pub struct Replayer<D = MapDepth> {
    // TODO(w46): design this type's fields.
    _todo: std::marker::PhantomData<(D,)>,
}

#[allow(unused_variables)]
impl Replayer {
    /// A replayer over [`MapDepth`] for a capture that starts at the
    /// beginning of the feed (empty book, update 1 next), reordering
    /// within `window` updates.
    pub fn new(window: usize) -> Replayer {
        todo!("w46: new")
    }
}

#[allow(unused_variables)]
impl<D: DepthBook> Replayer<D> {
    /// Like [`new`](Replayer::new), keeping the book in `depth`. A capture
    /// that starts mid-feed works too: the first updates look like a gap
    /// and the first snapshot recovers.
    pub fn with_depth(depth: D, window: usize) -> Replayer<D> {
        todo!("w46: with_depth")
    }

    /// Handles one captured Ethernet frame: counts it, extracts the UDP
    /// payload and hands it to [`on_packet`](Replayer::on_packet).
    /// Frames that are not valid feed packets are counted as malformed.
    pub fn on_frame(&mut self, frame: &[u8]) {
        todo!("w46: on_frame")
    }

    /// Handles one feed packet (a UDP payload). O(updates) plus the book
    /// operations; snapshots O(levels).
    pub fn on_packet(&mut self, payload: &[u8]) {
        todo!("w46: on_packet")
    }

    /// The counters so far.
    pub fn stats(&self) -> &ReplayStats {
        todo!("w46: stats")
    }

    /// The rebuilt book.
    pub fn builder(&self) -> &BookBuilder<D> {
        todo!("w46: builder")
    }

    /// Sequence number of the first snapshot that did not match.
    pub fn first_mismatch(&self) -> Option<Seq> {
        todo!("w46: first_mismatch")
    }
}

// TODO(w46): implement — see the week guide on tepy.dev/hft.

/// Replays a whole capture held in memory into `replayer`. Stops at the
/// first record the pcap reader cannot read (a truncated or corrupt file)
/// and returns that error; everything before it has been applied.
#[allow(unused_variables)]
pub fn replay_capture<D: DepthBook>(
    capture: &[u8],
    replayer: &mut Replayer<D>,
) -> Result<(), PcapError> {
    todo!("w46: replay_capture")
}
