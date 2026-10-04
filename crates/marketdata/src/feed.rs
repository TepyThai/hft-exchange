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

use crate::pcap::MAX_UDP_PAYLOAD;
use crate::{L2Snapshot, L2Update};
use types::{Seq, Side};

// @student-begin w46
use crate::Level;
use types::{Price, Qty};
// @student-end

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

// @student w46
impl<'a> FeedPacket<'a> {
    /// Decodes and validates one packet (see the module docs for the
    /// rules). Incremental: O(updates) validation, zero-copy. Snapshot:
    /// O(levels), allocates the level vectors.
    pub fn decode(bytes: &'a [u8]) -> Result<FeedPacket<'a>, WireError> {
        let Some(h) = bytes.get(..HEADER_LEN) else {
            return Err(WireError::Truncated);
        };
        let magic = u16::from_le_bytes([h[0], h[1]]);
        if magic != FEED_MAGIC {
            return Err(WireError::BadMagic(magic));
        }
        if h[2] != FEED_VERSION {
            return Err(WireError::BadVersion(h[2]));
        }
        let count = u16::from_le_bytes([h[4], h[5]]) as usize;
        let count2 = u16::from_le_bytes([h[6], h[7]]) as usize;
        let seq = Seq(u64_at(h, 8));
        let body = &bytes[HEADER_LEN..];
        match h[3] {
            KIND_INCREMENTAL => {
                let expected = HEADER_LEN + count * UPDATE_LEN;
                if count2 != 0 || bytes.len() != expected {
                    return Err(WireError::BadLength {
                        expected,
                        got: bytes.len(),
                    });
                }
                if count == 0 || seq.0 == 0 || seq.0.checked_add(count as u64).is_none() {
                    return Err(WireError::BadSequence);
                }
                if let Some(index) = body.chunks_exact(UPDATE_LEN).position(|e| e[0] > 1) {
                    return Err(WireError::BadSide { index });
                }
                Ok(FeedPacket::Incremental(Incremental { first: seq, body }))
            }
            KIND_SNAPSHOT => {
                let expected = HEADER_LEN + (count + count2) * LEVEL_LEN;
                if bytes.len() != expected {
                    return Err(WireError::BadLength {
                        expected,
                        got: bytes.len(),
                    });
                }
                let level = |e: &[u8]| Level {
                    price: Price(u64_at(e, 0) as i64),
                    qty: Qty(u64_at(e, 8)),
                };
                let mut levels = body.chunks_exact(LEVEL_LEN).map(level);
                let bids: Vec<Level> = levels.by_ref().take(count).collect();
                let asks: Vec<Level> = levels.collect();
                let ordered = |ls: &[Level], side: Side| {
                    ls.iter().enumerate().position(|(i, l)| {
                        l.qty.is_zero()
                            || (i > 0
                                && match side {
                                    Side::Buy => l.price >= ls[i - 1].price,
                                    Side::Sell => l.price <= ls[i - 1].price,
                                })
                    })
                };
                if let Some(index) = ordered(&bids, Side::Buy) {
                    return Err(WireError::BadLevel { index });
                }
                if let Some(i) = ordered(&asks, Side::Sell) {
                    return Err(WireError::BadLevel { index: count + i });
                }
                Ok(FeedPacket::Snapshot(L2Snapshot { seq, bids, asks }))
            }
            kind => Err(WireError::BadKind(kind)),
        }
    }
}

// @student w46
impl<'a> Incremental<'a> {
    /// Sequence number of the first update.
    pub fn first_seq(&self) -> Seq {
        self.first
    }

    /// Number of updates.
    pub fn len(&self) -> usize {
        self.body.len() / UPDATE_LEN
    }

    /// Never true for a decoded packet.
    pub fn is_empty(&self) -> bool {
        self.body.is_empty()
    }

    /// Update `i` (`seq = first + i`). O(1).
    ///
    /// # Panics
    /// If `i >= len()`.
    #[inline]
    pub fn get(&self, i: usize) -> L2Update {
        let e = &self.body[i * UPDATE_LEN..(i + 1) * UPDATE_LEN];
        L2Update {
            seq: Seq(self.first.0 + i as u64),
            side: if e[0] == 0 { Side::Buy } else { Side::Sell },
            price: Price(u64_at(e, 1) as i64),
            qty: Qty(u64_at(e, 9)),
        }
    }

    /// The updates in order. O(1) each.
    pub fn iter(&self) -> Updates<'a> {
        Updates {
            packet: *self,
            next: 0,
        }
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

// @student-begin w46
#[inline]
fn u64_at(b: &[u8], i: usize) -> u64 {
    let mut w = [0u8; 8];
    w.copy_from_slice(&b[i..i + 8]);
    u64::from_le_bytes(w)
}
// @student-end
