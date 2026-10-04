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

use crate::feed::{encode_incremental, encode_snapshot, MAX_UPDATES_PER_PACKET};
use crate::pcap::{PcapError, PcapWriter};
use crate::{BookBuilder, DepthBook, L2Snapshot, L2Update, MapDepth};
use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddrV4};
use types::Seq;

// @student-begin w46
use crate::feed::FeedPacket;
use crate::pcap::{parse_udp, PcapReader};
use crate::reorder::PushOutcome;
use crate::reorder::ReorderBuffer;
use crate::{SnapshotOutcome, UpdateOutcome};
use std::collections::VecDeque;
use types::Side;
// @student-end

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
// @student w46
#[derive(Clone, Debug)]
pub struct Replayer<D = MapDepth> {
    builder: BookBuilder<D>,
    reorder: ReorderBuffer<L2Update>,
    /// Snapshots ahead of the book, waiting to be verified, in seq order.
    pending: VecDeque<L2Snapshot>,
    stats: ReplayStats,
    first_mismatch: Option<Seq>,
}

// @student w46
impl Replayer {
    /// A replayer over [`MapDepth`] for a capture that starts at the
    /// beginning of the feed (empty book, update 1 next), reordering
    /// within `window` updates.
    pub fn new(window: usize) -> Replayer {
        Replayer::with_depth(MapDepth::default(), window)
    }
}

// @student w46
impl<D: DepthBook> Replayer<D> {
    /// Like [`new`](Replayer::new), keeping the book in `depth`. A capture
    /// that starts mid-feed works too: the first updates look like a gap
    /// and the first snapshot recovers.
    pub fn with_depth(depth: D, window: usize) -> Replayer<D> {
        let mut builder = BookBuilder::with_depth(depth);
        builder.apply_snapshot(&L2Snapshot::default());
        Replayer {
            builder,
            reorder: ReorderBuffer::new(Seq(1), window),
            pending: VecDeque::new(),
            stats: ReplayStats::default(),
            first_mismatch: None,
        }
    }

    /// Handles one captured Ethernet frame: counts it, extracts the UDP
    /// payload and hands it to [`on_packet`](Replayer::on_packet).
    /// Frames that are not valid feed packets are counted as malformed.
    pub fn on_frame(&mut self, frame: &[u8]) {
        self.stats.records += 1;
        match parse_udp(frame) {
            Ok(d) => self.on_packet(d.payload),
            Err(_) => self.stats.malformed += 1,
        }
    }

    /// Handles one feed packet (a UDP payload). O(updates) plus the book
    /// operations; snapshots O(levels).
    pub fn on_packet(&mut self, payload: &[u8]) {
        match FeedPacket::decode(payload) {
            Err(_) => self.stats.malformed += 1,
            Ok(FeedPacket::Incremental(inc)) => {
                self.stats.incremental_packets += 1;
                for u in inc.iter() {
                    match self.reorder.push(u.seq, u) {
                        PushOutcome::Accepted => {}
                        PushOutcome::Duplicate => self.stats.duplicates += 1,
                        PushOutcome::Skipped { lost } => self.stats.lost += lost,
                    }
                }
                self.drain();
            }
            Ok(FeedPacket::Snapshot(snap)) => {
                self.stats.snapshot_packets += 1;
                self.on_snapshot(snap);
            }
        }
    }

    /// The counters so far.
    pub fn stats(&self) -> &ReplayStats {
        &self.stats
    }

    /// The rebuilt book.
    pub fn builder(&self) -> &BookBuilder<D> {
        &self.builder
    }

    /// Sequence number of the first snapshot that did not match.
    pub fn first_mismatch(&self) -> Option<Seq> {
        self.first_mismatch
    }
}

// @student-begin w46
impl<D: DepthBook> Replayer<D> {
    /// Applies every update the reorder buffer can release.
    #[inline]
    fn drain(&mut self) {
        while let Some((_, u)) = self.reorder.pop() {
            match self.builder.apply_update(&u) {
                UpdateOutcome::Applied => {
                    self.stats.updates += 1;
                    if !self.pending.is_empty() {
                        self.check_pending();
                    }
                }
                UpdateOutcome::Duplicate => self.stats.duplicates += 1,
                UpdateOutcome::Gap { .. } | UpdateOutcome::Buffered => {}
            }
        }
    }

    fn on_snapshot(&mut self, snap: L2Snapshot) {
        let last = self.builder.last_seq().unwrap_or_default();
        let covers_hole = self.reorder.missing().is_some_and(|hole| hole <= snap.seq);
        if !self.builder.is_synced() || covers_hole {
            match self.builder.apply_snapshot(&snap) {
                SnapshotOutcome::Stale => self.stats.unverified += 1,
                SnapshotOutcome::Synced { replayed, .. }
                | SnapshotOutcome::StillGapped { replayed, .. } => {
                    self.stats.recoveries += 1;
                    self.stats.updates += replayed as u64;
                    self.reorder.skip_to(Seq(snap.seq.0 + 1));
                    self.check_pending();
                    self.drain();
                }
            }
            return;
        }
        if snap.seq == last {
            self.verify(&snap);
        } else if snap.seq > last {
            self.pending.push_back(snap);
        } else {
            self.stats.unverified += 1;
        }
    }

    /// Verifies pending snapshots the book has reached; drops those it
    /// passed.
    fn check_pending(&mut self) {
        let last = self.builder.last_seq().unwrap_or_default();
        while let Some(front) = self.pending.front() {
            if front.seq > last {
                break;
            }
            let snap = self.pending.pop_front().unwrap_or_default();
            if snap.seq == last && self.builder.is_synced() {
                self.verify(&snap);
            } else {
                self.stats.unverified += 1;
            }
        }
    }

    /// Compares the book with `snap` level by level, without allocating.
    fn verify(&mut self, snap: &L2Snapshot) {
        let book = &self.builder;
        let same = [(Side::Buy, &snap.bids), (Side::Sell, &snap.asks)]
            .into_iter()
            .all(|(side, levels)| {
                book.level_count(side) == levels.len()
                    && levels
                        .iter()
                        .all(|l| book.level_qty(side, l.price) == l.qty)
            });
        if same {
            self.stats.verified += 1;
        } else {
            self.stats.mismatches += 1;
            self.first_mismatch.get_or_insert(snap.seq);
        }
    }
}
// @student-end

/// Replays a whole capture held in memory into `replayer`. Stops at the
/// first record the pcap reader cannot read (a truncated or corrupt file)
/// and returns that error; everything before it has been applied.
// @student w46
pub fn replay_capture<D: DepthBook>(
    capture: &[u8],
    replayer: &mut Replayer<D>,
) -> Result<(), PcapError> {
    for record in PcapReader::new(capture)? {
        replayer.on_frame(record?.data);
    }
    Ok(())
}
