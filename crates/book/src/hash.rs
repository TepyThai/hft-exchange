//! A fast, deterministic hasher for integer keys such as [`OrderId`].
//!
//! `std`'s default `RandomState` (SipHash-1-3) is DoS-resistant but costs
//! ~15–20ns per lookup and is seeded randomly per process. Order ids are
//! assigned by the exchange, not chosen by an attacker, so a one-multiply
//! hash is enough — and being deterministic keeps the whole engine a pure
//! function of its input (iteration order never leaks into output anyway).
//!
//! Provided infrastructure (not part of a week's exercise).

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use types::OrderId;

/// Folded-multiply hasher: `x * K` as a 128-bit product, high half XOR low
/// half. One `mul` per `u64`, good bit mixing in both the low bits (bucket
/// index) and the high bits (control bytes) of `hashbrown`'s table, even for
/// sequential or strided ids.
#[derive(Clone, Copy, Debug, Default)]
pub struct IdHasher(u64);

const K: u64 = 0x9E37_79B9_7F4A_7C15;

impl IdHasher {
    #[inline]
    fn mix(&mut self, x: u64) {
        let m = u128::from(x ^ self.0).wrapping_mul(u128::from(K));
        self.0 = (m as u64) ^ ((m >> 64) as u64);
    }
}

impl Hasher for IdHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut buf = [0u8; 8];
            buf[..chunk.len()].copy_from_slice(chunk);
            self.mix(u64::from_le_bytes(buf));
        }
    }

    #[inline]
    fn write_u64(&mut self, x: u64) {
        self.mix(x);
    }

    #[inline]
    fn write_u32(&mut self, x: u32) {
        self.mix(u64::from(x));
    }

    #[inline]
    fn write_usize(&mut self, x: usize) {
        self.mix(x as u64);
    }
}

/// `BuildHasher` for [`IdHasher`] (zero-sized, deterministic).
pub type IdBuildHasher = BuildHasherDefault<IdHasher>;

/// `HashMap` keyed by [`OrderId`] using [`IdHasher`].
/// Create with `IdMap::default()` or
/// `IdMap::with_capacity_and_hasher(n, Default::default())`.
pub type IdMap<V> = HashMap<OrderId, V, IdBuildHasher>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::BuildHasher;

    fn h(x: u64) -> u64 {
        IdBuildHasher::default().hash_one(OrderId(x))
    }

    #[test]
    fn hasher_is_deterministic_and_spreads_strided_ids() {
        assert_eq!(h(42), h(42));
        // Strided ids must not collapse onto a few buckets (low 10 bits).
        let mut seen = std::collections::HashSet::new();
        for i in 0..1024u64 {
            seen.insert(h(i * 1024) & 1023);
        }
        assert!(seen.len() > 500, "only {} distinct buckets", seen.len());
    }
}
