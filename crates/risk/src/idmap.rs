//! A fast hash map for small integer ids (provided — not part of a week).
//!
//! `std`'s default SipHash is DoS-resistant but costs ~15–20ns per lookup;
//! risk keys are exchange-assigned integers (account, instrument, order and
//! session ids), so a multiply-rotate hash is enough and an order of
//! magnitude cheaper. Lookups never allocate; inserts allocate only when the
//! table grows past its capacity (pre-size with `with_capacity_and_hasher`).

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// `HashMap` keyed by small integer ids, hashed with [`IdHasher`].
///
/// Create one with `IdMap::default()` or
/// `IdMap::with_capacity_and_hasher(n, Default::default())`.
pub type IdMap<K, V> = HashMap<K, V, BuildHasherDefault<IdHasher>>;

/// Multiply-xorshift hasher for integer keys.
///
/// Each word is mixed in with one add and one multiply; `finish` applies a
/// xorshift-multiply-xorshift finalizer, so both the low bits (hashbrown's
/// bucket index) and the top 7 bits (its control tag) depend on every key
/// bit: sequential ids, ids that differ only in their high half (e.g.
/// `session << 40 | seq`) and strided ids (`seq << 12`) all spread like
/// random keys (see the tests). Cheaper one-multiply schemes each fail one
/// of those patterns. Two multiplies per key (~2ns), O(1), no allocation.
/// Not DoS-resistant — use only for ids the exchange assigns itself.
#[derive(Clone, Copy, Debug, Default)]
pub struct IdHasher {
    hash: u64,
}

const K: u64 = 0xf135_7aea_2e62_a9c5;

impl IdHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = self.hash.wrapping_add(word).wrapping_mul(K);
    }
}

impl Hasher for IdHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            self.add(u64::from_le_bytes(c.try_into().unwrap()));
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            let mut buf = [0u8; 8];
            buf[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(buf));
        }
    }

    #[inline]
    fn write_u8(&mut self, n: u8) {
        self.add(n as u64);
    }

    #[inline]
    fn write_u16(&mut self, n: u16) {
        self.add(n as u64);
    }

    #[inline]
    fn write_u32(&mut self, n: u32) {
        self.add(n as u64);
    }

    #[inline]
    fn write_u64(&mut self, n: u64) {
        self.add(n);
    }

    #[inline]
    fn write_usize(&mut self, n: usize) {
        self.add(n as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        let h = self.hash ^ (self.hash >> 32);
        let h = h.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        h ^ (h >> 29)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn distinct(keys: impl Iterator<Item = u64>, bits: u32) -> usize {
        let mut low = std::collections::HashSet::new();
        let mut top = std::collections::HashSet::new();
        for k in keys {
            let mut h = IdHasher::default();
            h.write_u64(k);
            let v = h.finish();
            low.insert(v & ((1 << bits) - 1));
            top.insert(v >> 57);
        }
        assert_eq!(top.len(), 128, "control tags must use all 7 top bits");
        low.len()
    }

    #[test]
    fn ids_spread_over_buckets() {
        // 1024 keys into 1024 buckets: uniformly random hashing fills
        // ~647 (1 − 1/e). Sequential ids, ids varying only in the high
        // half, and strided ids must all come close.
        let patterns: [(&str, Vec<u64>); 4] = [
            ("sequential", (0..1024).collect()),
            ("high half", (0..1024).map(|i| (i << 32) | 7).collect()),
            (
                "session << 40",
                (0..1024).map(|i| ((i % 8) << 40) | (i / 8)).collect(),
            ),
            ("stride 4096", (0..1024).map(|i| i * 4096).collect()),
        ];
        for (name, keys) in patterns {
            let n = distinct(keys.into_iter(), 10);
            assert!(n > 600, "{name}: only {n} distinct buckets");
        }
        let mut m: IdMap<u64, u64> = IdMap::default();
        for i in 0..10_000 {
            m.insert(i * 31, i);
        }
        assert_eq!(m.get(&(31 * 77)), Some(&77));
    }
}
