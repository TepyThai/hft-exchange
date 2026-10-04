//! The simulated feed wire (provided): what a datagram on the market-data
//! [`ChaosLink`](super::ChaosLink) looks like.
//!
//! ```text
//! datagram := body ‖ crc32(body) (u32, little-endian)
//! body     := a feed packet (marketdata::feed layout: incremental or snapshot)
//!           | heartbeat: magic 0x4248 ("HB", u16 LE) ‖ next_seq (u64 LE)
//! ```
//!
//! The trailing CRC-32 plays the part of the checks a real receiver gets
//! from its NIC (the Ethernet frame check sequence, the UDP checksum): a
//! datagram damaged in transit fails [`open`] and must be treated as
//! **lost**, never half-applied. The week 46 feed layout has no checksum of
//! its own, and its decoder only validates structure — a flipped bit in a
//! price or a quantity decodes fine.
//!
//! A heartbeat is what a quiet line sends (as MoldUDP64 does): the
//! publisher's next sequence number, so a receiver that lost the last
//! packets before a lull finds out.

use journal::crc32;
use marketdata::feed::{FeedPacket, WireError};
use std::fmt;
use types::Seq;

/// First two bytes of a heartbeat body (`"HB"`).
pub const HEARTBEAT_MAGIC: u16 = 0x4248;
/// Length of a heartbeat body.
pub const HEARTBEAT_LEN: usize = 10;
/// Length of the trailing checksum.
pub const FCS_LEN: usize = 4;

/// Appends a datagram carrying `packet` (an encoded feed packet). O(len).
pub fn seal_packet(packet: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(packet);
    out.extend_from_slice(&crc32(packet).to_le_bytes());
}

/// Appends a heartbeat datagram announcing `next_seq`. O(1).
pub fn seal_heartbeat(next_seq: Seq, out: &mut Vec<u8>) {
    let start = out.len();
    out.extend_from_slice(&HEARTBEAT_MAGIC.to_le_bytes());
    out.extend_from_slice(&next_seq.0.to_le_bytes());
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_le_bytes());
}

/// A verified datagram.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Datagram<'a> {
    /// A feed packet (zero-copy for incrementals).
    Packet(FeedPacket<'a>),
    /// "The next update I will publish is `next_seq`."
    Heartbeat { next_seq: Seq },
}

/// Why a datagram was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// Too short to hold a checksum.
    Truncated,
    /// The checksum does not match: damaged in transit (a loss).
    Checksum,
    /// The checksum matches but the body is not a valid feed packet: the
    /// sender is broken.
    Malformed(WireError),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenError::Truncated => f.write_str("datagram too short"),
            OpenError::Checksum => f.write_str("datagram checksum mismatch"),
            OpenError::Malformed(e) => write!(f, "malformed feed packet: {e:?}"),
        }
    }
}

impl std::error::Error for OpenError {}

/// Verifies and decodes a datagram. O(len).
pub fn open(datagram: &[u8]) -> Result<Datagram<'_>, OpenError> {
    let Some(split) = datagram.len().checked_sub(FCS_LEN) else {
        return Err(OpenError::Truncated);
    };
    let (body, fcs) = datagram.split_at(split);
    if crc32(body).to_le_bytes() != fcs {
        return Err(OpenError::Checksum);
    }
    if body.len() == HEARTBEAT_LEN && body[..2] == HEARTBEAT_MAGIC.to_le_bytes() {
        let mut seq = [0u8; 8];
        seq.copy_from_slice(&body[2..]);
        return Ok(Datagram::Heartbeat {
            next_seq: Seq(u64::from_le_bytes(seq)),
        });
    }
    FeedPacket::decode(body)
        .map(Datagram::Packet)
        .map_err(OpenError::Malformed)
}
