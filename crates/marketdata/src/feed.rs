//! Week 46 — the binary wire encoding of the L2 feed (one UDP datagram per
//! packet), as published, captured and replayed.
//!
//! Modelled on MoldUDP64-style sequenced feeds: a packet carries a run of
//! consecutive updates and only the first sequence number, so a receiver
//! detects loss, duplication and reordering from the header alone.
//! Snapshots travel as their own packets (in production on a separate
//! recovery channel) and say which update they reflect.
//!
//! ```text
//! header, 16 bytes, little-endian, no padding:
//!   0  u16  magic     0x444D  (bytes "MD")
//!   2  u8   version   1
//!   3  u8   kind      1 = incremental, 2 = snapshot
//!   4  u16  count     incremental: number of updates (≥ 1); snapshot: bid levels
//!   6  u16  count2    incremental: 0;                        snapshot: ask levels
//!   8  u64  seq       incremental: feed seq of the first update — update i
//!                     has seq + i (≥ 1, no overflow);
//!                     snapshot: the update the book reflects (0 = none yet)
//! incremental body: count × 17 bytes
//!   0  u8   side      0 = buy, 1 = sell
//!   1  i64  price     ticks
//!   9  u64  qty       new level total (0 = level removed)
//! snapshot body: (count + count2) × 16 bytes, bids best (highest) first,
//! then asks best (lowest) first; prices strictly ordered, qty > 0
//!   0  i64  price
//!   8  u64  qty
//! ```
//!
//! Decoding is strict — the packet length must match the counts exactly,
//! and anything outside the layout is an error — because a capture is
//! evidence: a corrupt packet must be reported, not half-applied.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::pcap::MAX_UDP_PAYLOAD;
use crate::{L2Snapshot, L2Update};
use types::{Seq, Side};

// TODO(w46): implement — see the week guide on tepy.dev/hft.

/// First two bytes of every feed packet (`"MD"`).
pub const FEED_MAGIC: u16 = 0x444D;
pub const FEED_VERSION: u8 = 1;
pub const KIND_INCREMENTAL: u8 = 1;
pub const KIND_SNAPSHOT: u8 = 2;
pub const HEADER_LEN: usize = 16;
pub const UPDATE_LEN: usize = 17;
pub const LEVEL_LEN: usize = 16;
/// Most updates one packet can carry.
pub const MAX_UPDATES_PER_PACKET: usize = (MAX_UDP_PAYLOAD - HEADER_LEN) / UPDATE_LEN;
/// Most levels (both sides) one snapshot packet can carry. A production
/// feed splits bigger books over several packets.
pub const MAX_SNAPSHOT_LEVELS: usize = (MAX_UDP_PAYLOAD - HEADER_LEN) / LEVEL_LEN;

/// A feed packet that does not follow the layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    /// Shorter than a header.
    Truncated,
    BadMagic(u16),
    BadVersion(u8),
    BadKind(u8),
    /// The length does not match the header's counts.
    BadLength {
        expected: usize,
        got: usize,
    },
    /// An incremental packet with no updates, or seq 0, or a run whose
    /// sequence numbers would overflow.
    BadSequence,
    /// A side byte other than 0 or 1 (update `index`).
    BadSide {
        index: usize,
    },
    /// A snapshot level with zero quantity or out of order (level `index`,
    /// counting bids then asks).
    BadLevel {
        index: usize,
    },
}

/// A decoded packet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedPacket<'a> {
    Incremental(Incremental<'a>),
    Snapshot(L2Snapshot),
}

/// A validated run of consecutive updates, decoded lazily from the packet
/// bytes (zero-copy).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Incremental<'a> {
    first: Seq,
    /// `len × UPDATE_LEN` bytes, already validated.
    body: &'a [u8],
}

/// Appends an incremental packet carrying `updates`. O(n).
///
/// # Panics
/// If `updates` is empty, longer than [`MAX_UPDATES_PER_PACKET`], or not
/// consecutive sequence numbers starting at ≥ 1.
pub fn encode_incremental(updates: &[L2Update], out: &mut Vec<u8>) {
    assert!(
        (1..=MAX_UPDATES_PER_PACKET).contains(&updates.len()),
        "1..={MAX_UPDATES_PER_PACKET} updates per packet"
    );
    let first = updates[0].seq;
    assert!(first.0 >= 1, "feed sequence numbers start at 1");
    header(KIND_INCREMENTAL, updates.len(), 0, first, out);
    for (i, u) in updates.iter().enumerate() {
        assert_eq!(u.seq.0, first.0 + i as u64, "updates must be consecutive");
        out.push(match u.side {
            Side::Buy => 0,
            Side::Sell => 1,
        });
        out.extend_from_slice(&u.price.0.to_le_bytes());
        out.extend_from_slice(&u.qty.0.to_le_bytes());
    }
}

/// Appends a snapshot packet. O(levels).
///
/// # Panics
/// If the snapshot has more than [`MAX_SNAPSHOT_LEVELS`] levels.
pub fn encode_snapshot(snap: &L2Snapshot, out: &mut Vec<u8>) {
    let (b, a) = (snap.bids.len(), snap.asks.len());
    assert!(
        b + a <= MAX_SNAPSHOT_LEVELS,
        "snapshot too large for one packet"
    );
    header(KIND_SNAPSHOT, b, a, snap.seq, out);
    for l in snap.bids.iter().chain(&snap.asks) {
        out.extend_from_slice(&l.price.0.to_le_bytes());
        out.extend_from_slice(&l.qty.0.to_le_bytes());
    }
}

fn header(kind: u8, count: usize, count2: usize, seq: Seq, out: &mut Vec<u8>) {
    out.extend_from_slice(&FEED_MAGIC.to_le_bytes());
    out.push(FEED_VERSION);
    out.push(kind);
    out.extend_from_slice(&(count as u16).to_le_bytes());
    out.extend_from_slice(&(count2 as u16).to_le_bytes());
    out.extend_from_slice(&seq.0.to_le_bytes());
}

#[allow(unused_variables)]
impl<'a> FeedPacket<'a> {
    /// Decodes and validates one packet (see the module docs for the
    /// rules). Incremental: O(updates) validation, zero-copy. Snapshot:
    /// O(levels), allocates the level vectors.
    pub fn decode(bytes: &'a [u8]) -> Result<FeedPacket<'a>, WireError> {
        todo!("w46: decode")
    }
}

#[allow(unused_variables)]
impl<'a> Incremental<'a> {
    /// Sequence number of the first update.
    pub fn first_seq(&self) -> Seq {
        todo!("w46: first_seq")
    }

    /// Number of updates.
    pub fn len(&self) -> usize {
        todo!("w46: len")
    }

    /// Never true for a decoded packet.
    pub fn is_empty(&self) -> bool {
        todo!("w46: is_empty")
    }

    /// Update `i` (`seq = first + i`). O(1).
    ///
    /// # Panics
    /// If `i >= len()`.
    #[inline]
    pub fn get(&self, i: usize) -> L2Update {
        todo!("w46: get")
    }

    /// The updates in order. O(1) each.
    pub fn iter(&self) -> Updates<'a> {
        todo!("w46: iter")
    }
}

/// Iterator over an [`Incremental`]'s updates.
#[derive(Clone, Debug)]
pub struct Updates<'a> {
    packet: Incremental<'a>,
    next: usize,
}

impl Iterator for Updates<'_> {
    type Item = L2Update;

    #[inline]
    fn next(&mut self) -> Option<L2Update> {
        if self.next == self.packet.len() {
            return None;
        }
        self.next += 1;
        Some(self.packet.get(self.next - 1))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.packet.len() - self.next;
        (left, Some(left))
    }
}

impl ExactSizeIterator for Updates<'_> {}

// TODO(w46): implement — see the week guide on tepy.dev/hft.
