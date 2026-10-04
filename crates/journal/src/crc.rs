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

/// The reflected CRC-32/IEEE polynomial.
pub const CRC32_POLY: u32 = 0xEDB8_8320;

// @student-begin w09
/// `TABLES[0]` is the classic byte table; `TABLES[k][b]` is the CRC of byte
/// `b` followed by `k` zero bytes, which is what slicing-by-8 folds with.
static TABLES: [[u32; 256]; 8] = make_tables();

const fn make_tables() -> [[u32; 256]; 8] {
    let mut t = [[0u32; 256]; 8];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ CRC32_POLY
            } else {
                c >> 1
            };
            k += 1;
        }
        t[0][i] = c;
        i += 1;
    }
    let mut i = 0;
    while i < 256 {
        let mut s = 1;
        while s < 8 {
            let prev = t[s - 1][i];
            t[s][i] = (prev >> 8) ^ t[0][(prev & 0xFF) as usize];
            s += 1;
        }
        i += 1;
    }
    t
}

/// Folds `bytes` into the (pre-inverted) running state.
#[inline]
fn fold(mut state: u32, bytes: &[u8]) -> u32 {
    let t = &TABLES;
    let mut chunks = bytes.chunks_exact(8);
    for c in &mut chunks {
        let lo = u32::from_le_bytes([c[0], c[1], c[2], c[3]]) ^ state;
        let hi = u32::from_le_bytes([c[4], c[5], c[6], c[7]]);
        state = t[7][lo as u8 as usize]
            ^ t[6][(lo >> 8) as u8 as usize]
            ^ t[5][(lo >> 16) as u8 as usize]
            ^ t[4][(lo >> 24) as usize]
            ^ t[3][hi as u8 as usize]
            ^ t[2][(hi >> 8) as u8 as usize]
            ^ t[1][(hi >> 16) as u8 as usize]
            ^ t[0][(hi >> 24) as usize];
    }
    for &b in chunks.remainder() {
        state = t[0][(state as u8 ^ b) as usize] ^ (state >> 8);
    }
    state
}
// @student-end

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
// @student w09
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Crc32 {
    /// Running register, kept inverted (init `!0`, final XOR on finish).
    state: u32,
}

// @student w09
impl Crc32 {
    /// A fresh checksum (the CRC of zero bytes is `0`). O(1).
    pub fn new() -> Crc32 {
        Crc32 { state: !0 }
    }

    /// Feeds `bytes`. O(len), no allocation; ~1 cycle per byte with
    /// slicing-by-8.
    pub fn update(&mut self, bytes: &[u8]) {
        self.state = fold(self.state, bytes);
    }

    /// The CRC of everything fed so far. Does not consume or reset the
    /// state, so you can keep feeding. O(1).
    pub fn finish(&self) -> u32 {
        !self.state
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
