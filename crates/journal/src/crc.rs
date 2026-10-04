//! CRC-32/IEEE (the zlib / Ethernet / PNG CRC), table driven.
//!
//! Parameters: reflected polynomial `0xEDB8_8320`, initial value
//! `0xFFFF_FFFF`, final XOR `0xFFFF_FFFF`. Check value:
//! `crc32(b"123456789") == 0xCBF4_3926`.
//!
//! A CRC is not a cryptographic hash: it detects accidental damage (bit
//! flips, torn writes, misframed reads) cheaply and with guarantees — every
//! burst error up to 32 bits is caught — but anyone can forge one.
//!
//! The textbook table-driven CRC ("Sarwate") processes one byte per step
//! with a 256-entry table; each step depends on the previous one through a
//! table load, so it runs at roughly one byte per load latency. *Slicing
//! by 8* uses eight tables to fold eight input bytes per step with eight
//! independent loads, which is several times faster on short frames like
//! journal records. Both give identical results.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
/// The reflected CRC-32/IEEE polynomial.
pub const CRC32_POLY: u32 = 0xEDB8_8320;

// TODO(w09): implement — see the week guide on tepy.dev/hft.

/// Incremental CRC-32/IEEE: feed bytes in any number of pieces, then
/// [`finish`](Crc32::finish). Feeding `a` then `b` gives the same result as
/// feeding `a ++ b` — which is how a journal frame checksums its header and
/// payload without copying them together.
///
/// ```
/// let mut c = journal::Crc32::new();
/// c.update(b"1234");
/// c.update(b"56789");
/// assert_eq!(c.finish(), 0xCBF4_3926);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Crc32 {
    // TODO(w09): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl Crc32 {
    /// A fresh checksum (the CRC of zero bytes is `0`). O(1).
    pub fn new() -> Crc32 {
        todo!("w09: new")
    }

    /// Feeds `bytes`. O(len), no allocation; ~1 cycle per byte with
    /// slicing-by-8.
    pub fn update(&mut self, bytes: &[u8]) {
        todo!("w09: update")
    }

    /// The CRC of everything fed so far. Does not consume or reset the
    /// state, so you can keep feeding. O(1).
    pub fn finish(&self) -> u32 {
        todo!("w09: finish")
    }
}

impl Default for Crc32 {
    fn default() -> Self {
        Crc32::new()
    }
}

/// One-shot CRC-32/IEEE of `bytes`. O(len), no allocation.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(bytes);
    c.finish()
}
