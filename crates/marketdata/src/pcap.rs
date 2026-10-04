//! Week 46 — pcap captures of the feed: writing (provided) and reading.
//!
//! The classic libpcap file format, the one every capture tool reads and
//! writes (`tcpdump -w`, Wireshark, exchange-provided historical data):
//!
//! ```text
//! global header, 24 bytes (in the writer's byte order):
//!   0  u32  magic          0xA1B2C3D4 (µs timestamps) or 0xA1B23C4D (ns);
//!                          read byte-swapped → the file is big-endian
//!   4  u16  version_major  2
//!   6  u16  version_minor  4
//!   8  i32  thiszone       0
//!  12  u32  sigfigs        0
//!  16  u32  snaplen        max bytes stored per packet
//!  20  u32  linktype       1 = Ethernet
//! then records, each a 16-byte header plus the captured bytes:
//!   0  u32  ts_sec
//!   4  u32  ts_frac        µs or ns, by the magic (< 10⁶ or < 10⁹)
//!   8  u32  incl_len       bytes stored (≤ snaplen, ≤ orig_len)
//!  12  u32  orig_len       bytes on the wire
//! ```
//!
//! Each captured feed packet is a full frame, built the way it left the
//! publisher's NIC (all big-endian, "network order"):
//!
//! ```text
//! Ethernet II  14   dst MAC (01:00:5e + low 23 bits of the group), src MAC,
//!                   ethertype 0x0800 (an 802.1Q tag 0x8100 + 2 bytes may precede it)
//! IPv4         20   0x45, DSCP/ECN, total length, id, flags/fragment (DF),
//!                   TTL, protocol 17, header checksum, src, dst
//! UDP           8   src port, dst port, length, checksum (0 = none, legal on IPv4)
//! payload           one feed packet (see `crate::feed`)
//! ```
//!
//! The writer emits little-endian nanosecond captures; the reader accepts
//! both resolutions and both byte orders. Reading is zero-copy over the
//! capture in memory (`std::fs::read` the file first).

use std::io::{self, Write};
use std::net::SocketAddrV4;

// @student-begin w46
use std::net::Ipv4Addr;
// @student-end

/// Magic of a capture with microsecond timestamps.
pub const PCAP_MAGIC_MICROS: u32 = 0xA1B2_C3D4;
/// Magic of a capture with nanosecond timestamps.
pub const PCAP_MAGIC_NANOS: u32 = 0xA1B2_3C4D;
/// Link type of Ethernet II frames.
pub const LINKTYPE_ETHERNET: u32 = 1;
/// Snapshot length the writer declares (the largest IPv4 packet).
pub const SNAPLEN: u32 = 65_535;
/// Records larger than this are corrupt whatever the header claims.
pub const MAX_RECORD: usize = 256 * 1024;
pub const GLOBAL_HEADER_LEN: usize = 24;
pub const RECORD_HEADER_LEN: usize = 16;
pub const ETHERNET_LEN: usize = 14;
pub const IPV4_LEN: usize = 20;
pub const UDP_LEN: usize = 8;
/// The largest UDP payload an IPv4 packet can carry.
pub const MAX_UDP_PAYLOAD: usize = 65_535 - IPV4_LEN - UDP_LEN;

/// What a capture's global header says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PcapHeader {
    /// Timestamps are nanoseconds (otherwise microseconds).
    pub nanos: bool,
    /// The file was written big-endian.
    pub big_endian: bool,
    pub version: (u16, u16),
    pub snaplen: u32,
    pub linktype: u32,
}

/// One captured packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PcapRecord<'a> {
    /// Capture time in nanoseconds since the Unix epoch.
    pub ts_ns: u64,
    /// Length on the wire (`data` may be shorter if the capture cut it).
    pub orig_len: u32,
    pub data: &'a [u8],
}

/// A capture that cannot be read (further).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PcapError {
    /// Not a pcap file (the first four bytes, as read little-endian).
    BadMagic(u32),
    /// The file ends inside the header or record that starts at `offset`
    /// — typically a capture cut off mid-write.
    Truncated { offset: usize },
    /// The record at `offset` has impossible lengths or timestamp.
    BadRecord { offset: usize },
    /// Frames are not Ethernet.
    UnsupportedLinkType(u32),
}

/// Why a captured frame is not a UDP datagram we can use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketError {
    /// The frame ends before its headers or declared lengths do.
    Truncated,
    /// Ethertype is not IPv4.
    NotIpv4(u16),
    /// IP version is not 4 or the header length is invalid.
    BadIpHeader,
    /// The IPv4 header checksum does not verify.
    BadChecksum,
    /// A fragment: feed packets are never fragmented.
    Fragmented,
    /// IP protocol is not UDP (17).
    NotUdp(u8),
    /// UDP length disagrees with the IP length.
    BadUdpLength,
}

/// A UDP datagram inside a captured frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UdpDatagram<'a> {
    pub src: SocketAddrV4,
    pub dst: SocketAddrV4,
    pub payload: &'a [u8],
}

// ---------------------------------------------------------------------------
// Writing (provided)

/// The RFC 1071 Internet checksum of `bytes`: the ones' complement of the
/// ones' complement sum of 16-bit big-endian words. Over a header that
/// includes its own checksum field, a valid header gives 0. O(len).
pub fn internet_checksum(bytes: &[u8]) -> u16 {
    let mut sum = 0u32;
    let mut words = bytes.chunks_exact(2);
    for w in &mut words {
        sum += u16::from_be_bytes([w[0], w[1]]) as u32;
    }
    if let [last] = words.remainder() {
        sum += (*last as u32) << 8;
    }
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Appends an Ethernet + IPv4 + UDP frame carrying `payload` from `src`
/// to `dst` (multicast destinations get the matching multicast MAC).
/// O(len).
///
/// # Panics
/// If `payload` is longer than [`MAX_UDP_PAYLOAD`].
pub fn udp_frame(
    src: SocketAddrV4,
    dst: SocketAddrV4,
    ip_id: u16,
    payload: &[u8],
    out: &mut Vec<u8>,
) {
    assert!(payload.len() <= MAX_UDP_PAYLOAD, "UDP payload too large");
    let d = dst.ip().octets();
    let dst_mac = if dst.ip().is_multicast() {
        [0x01, 0x00, 0x5E, d[1] & 0x7F, d[2], d[3]]
    } else {
        [0x02, 0x00, d[0], d[1], d[2], d[3]]
    };
    let s = src.ip().octets();
    out.extend_from_slice(&dst_mac);
    out.extend_from_slice(&[0x02, 0x00, s[0], s[1], s[2], s[3]]);
    out.extend_from_slice(&0x0800u16.to_be_bytes());

    let ip_start = out.len();
    let total = (IPV4_LEN + UDP_LEN + payload.len()) as u16;
    out.extend_from_slice(&[0x45, 0x00]);
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(&ip_id.to_be_bytes());
    out.extend_from_slice(&[0x40, 0x00, 64, 17, 0, 0]); // DF, TTL 64, UDP, checksum 0
    out.extend_from_slice(&s);
    out.extend_from_slice(&d);
    let checksum = internet_checksum(&out[ip_start..ip_start + IPV4_LEN]);
    out[ip_start + 10..ip_start + 12].copy_from_slice(&checksum.to_be_bytes());

    out.extend_from_slice(&src.port().to_be_bytes());
    out.extend_from_slice(&dst.port().to_be_bytes());
    out.extend_from_slice(&((UDP_LEN + payload.len()) as u16).to_be_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(payload);
}

/// Writes a little-endian, nanosecond-resolution Ethernet capture.
#[derive(Debug)]
pub struct PcapWriter<W: Write> {
    out: W,
    ip_id: u16,
    frame: Vec<u8>,
}

impl<W: Write> PcapWriter<W> {
    /// Starts a capture: writes the global header.
    pub fn new(mut out: W) -> io::Result<PcapWriter<W>> {
        let mut h = Vec::with_capacity(GLOBAL_HEADER_LEN);
        h.extend_from_slice(&PCAP_MAGIC_NANOS.to_le_bytes());
        h.extend_from_slice(&2u16.to_le_bytes());
        h.extend_from_slice(&4u16.to_le_bytes());
        h.extend_from_slice(&0i32.to_le_bytes());
        h.extend_from_slice(&0u32.to_le_bytes());
        h.extend_from_slice(&SNAPLEN.to_le_bytes());
        h.extend_from_slice(&LINKTYPE_ETHERNET.to_le_bytes());
        out.write_all(&h)?;
        Ok(PcapWriter {
            out,
            ip_id: 0,
            frame: Vec::with_capacity(2048),
        })
    }

    /// Appends one record holding `frame` (stored whole) at `ts_ns`.
    ///
    /// # Panics
    /// If `frame` is longer than [`SNAPLEN`].
    pub fn write_record(&mut self, ts_ns: u64, frame: &[u8]) -> io::Result<()> {
        assert!(frame.len() <= SNAPLEN as usize, "frame longer than snaplen");
        let mut h = [0u8; RECORD_HEADER_LEN];
        h[0..4].copy_from_slice(&((ts_ns / 1_000_000_000) as u32).to_le_bytes());
        h[4..8].copy_from_slice(&((ts_ns % 1_000_000_000) as u32).to_le_bytes());
        h[8..12].copy_from_slice(&(frame.len() as u32).to_le_bytes());
        h[12..16].copy_from_slice(&(frame.len() as u32).to_le_bytes());
        self.out.write_all(&h)?;
        self.out.write_all(frame)
    }

    /// Appends one UDP datagram as a full Ethernet/IPv4/UDP frame.
    pub fn write_udp(
        &mut self,
        ts_ns: u64,
        src: SocketAddrV4,
        dst: SocketAddrV4,
        payload: &[u8],
    ) -> io::Result<()> {
        let mut frame = std::mem::take(&mut self.frame);
        frame.clear();
        udp_frame(src, dst, self.ip_id, payload, &mut frame);
        self.ip_id = self.ip_id.wrapping_add(1);
        let result = self.write_record(ts_ns, &frame);
        self.frame = frame;
        result
    }

    /// The underlying writer (flushed).
    pub fn into_inner(mut self) -> io::Result<W> {
        self.out.flush()?;
        Ok(self.out)
    }
}

// ---------------------------------------------------------------------------
// Reading

/// Iterates the records of a capture held in memory, zero-copy. Yields
/// `Err` once — for the first record it cannot read — and then stops; the
/// records before it are good.
// @student w46
#[derive(Clone, Debug)]
pub struct PcapReader<'a> {
    bytes: &'a [u8],
    pos: usize,
    header: PcapHeader,
    done: bool,
}

// @student w46
impl<'a> PcapReader<'a> {
    /// Parses the global header. O(1).
    pub fn new(bytes: &'a [u8]) -> Result<PcapReader<'a>, PcapError> {
        let Some(h) = bytes.get(..GLOBAL_HEADER_LEN) else {
            return Err(PcapError::Truncated { offset: 0 });
        };
        let le = u32::from_le_bytes([h[0], h[1], h[2], h[3]]);
        let (nanos, big_endian) = match (le, le.swap_bytes()) {
            (PCAP_MAGIC_MICROS, _) => (false, false),
            (PCAP_MAGIC_NANOS, _) => (true, false),
            (_, PCAP_MAGIC_MICROS) => (false, true),
            (_, PCAP_MAGIC_NANOS) => (true, true),
            _ => return Err(PcapError::BadMagic(le)),
        };
        let u16_at = |i: usize| {
            let b = [h[i], h[i + 1]];
            if big_endian {
                u16::from_be_bytes(b)
            } else {
                u16::from_le_bytes(b)
            }
        };
        let u32_at = |i: usize| read_u32(h, i, big_endian);
        let header = PcapHeader {
            nanos,
            big_endian,
            version: (u16_at(4), u16_at(6)),
            snaplen: u32_at(16),
            linktype: u32_at(20),
        };
        if header.linktype != LINKTYPE_ETHERNET {
            return Err(PcapError::UnsupportedLinkType(header.linktype));
        }
        Ok(PcapReader {
            bytes,
            pos: GLOBAL_HEADER_LEN,
            header,
            done: false,
        })
    }

    /// The global header.
    pub fn header(&self) -> PcapHeader {
        self.header
    }

    /// Byte offset of the next record (the length of the good prefix once
    /// iteration has stopped on an error).
    pub fn offset(&self) -> usize {
        self.pos
    }

    fn next_record(&mut self) -> Option<Result<PcapRecord<'a>, PcapError>> {
        if self.done || self.pos == self.bytes.len() {
            return None;
        }
        let at = self.pos;
        let fail = |this: &mut Self, e| {
            this.done = true;
            Some(Err(e))
        };
        let Some(h) = self.bytes.get(at..at + RECORD_HEADER_LEN) else {
            return fail(self, PcapError::Truncated { offset: at });
        };
        let be = self.header.big_endian;
        let (sec, frac) = (read_u32(h, 0, be) as u64, read_u32(h, 4, be) as u64);
        let (incl, orig) = (read_u32(h, 8, be) as usize, read_u32(h, 12, be));
        let frac_limit = if self.header.nanos {
            1_000_000_000
        } else {
            1_000_000
        };
        let snaplen = self.header.snaplen as usize;
        if incl > MAX_RECORD
            || incl > orig as usize
            || (snaplen > 0 && incl > snaplen)
            || frac >= frac_limit
        {
            return fail(self, PcapError::BadRecord { offset: at });
        }
        let start = at + RECORD_HEADER_LEN;
        let Some(data) = self.bytes.get(start..start + incl) else {
            return fail(self, PcapError::Truncated { offset: at });
        };
        self.pos = start + incl;
        let ts_ns = sec * 1_000_000_000
            + if self.header.nanos {
                frac
            } else {
                frac * 1_000
            };
        Some(Ok(PcapRecord {
            ts_ns,
            orig_len: orig,
            data,
        }))
    }
}

impl<'a> Iterator for PcapReader<'a> {
    type Item = Result<PcapRecord<'a>, PcapError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_record()
    }
}

// @student-begin w46
#[inline]
fn read_u32(b: &[u8], i: usize, big_endian: bool) -> u32 {
    let w = [b[i], b[i + 1], b[i + 2], b[i + 3]];
    if big_endian {
        u32::from_be_bytes(w)
    } else {
        u32::from_le_bytes(w)
    }
}

#[inline]
fn be16(b: &[u8], i: usize) -> u16 {
    u16::from_be_bytes([b[i], b[i + 1]])
}
// @student-end

/// Extracts the UDP datagram from an Ethernet II frame (optionally
/// 802.1Q-tagged) carrying an unfragmented IPv4 packet with a valid header
/// checksum. Bytes past the IP total length (Ethernet padding) are
/// ignored. O(IP header), zero-copy.
// @student w46
pub fn parse_udp(frame: &[u8]) -> Result<UdpDatagram<'_>, PacketError> {
    if frame.len() < ETHERNET_LEN {
        return Err(PacketError::Truncated);
    }
    let mut at = 12;
    let mut ethertype = be16(frame, at);
    if ethertype == 0x8100 {
        at += 4;
        if frame.len() < at + 2 {
            return Err(PacketError::Truncated);
        }
        ethertype = be16(frame, at);
    }
    if ethertype != 0x0800 {
        return Err(PacketError::NotIpv4(ethertype));
    }
    let ip = &frame[at + 2..];
    if ip.len() < IPV4_LEN {
        return Err(PacketError::Truncated);
    }
    let ihl = (ip[0] & 0x0F) as usize * 4;
    if ip[0] >> 4 != 4 || ihl < IPV4_LEN {
        return Err(PacketError::BadIpHeader);
    }
    let total = be16(ip, 2) as usize;
    if ip.len() < ihl || ip.len() < total {
        return Err(PacketError::Truncated);
    }
    if total < ihl + UDP_LEN {
        return Err(PacketError::BadUdpLength);
    }
    if internet_checksum(&ip[..ihl]) != 0 {
        return Err(PacketError::BadChecksum);
    }
    let flags_frag = be16(ip, 6);
    if flags_frag & 0x2000 != 0 || flags_frag & 0x1FFF != 0 {
        return Err(PacketError::Fragmented);
    }
    if ip[9] != 17 {
        return Err(PacketError::NotUdp(ip[9]));
    }
    let addr = |i: usize| Ipv4Addr::new(ip[i], ip[i + 1], ip[i + 2], ip[i + 3]);
    let udp = &ip[ihl..total];
    let udp_len = be16(udp, 4) as usize;
    if udp_len != udp.len() {
        return Err(PacketError::BadUdpLength);
    }
    Ok(UdpDatagram {
        src: SocketAddrV4::new(addr(12), be16(udp, 0)),
        dst: SocketAddrV4::new(addr(16), be16(udp, 2)),
        payload: &udp[UDP_LEN..],
    })
}
