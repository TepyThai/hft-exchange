//! Deterministic state hashing for replay checks.
//!
//! Provided infrastructure (not part of a week's exercise).

/// A small, deterministic, order-sensitive 64-bit hasher over integer words.
///
/// Used by [`Engine::state_hash`](crate::Engine::state_hash): the journal
/// (w09) and recovery (w18) replay commands into a fresh engine and compare
/// hashes, so the result must be identical across runs, processes and
/// platforms — no `RandomState`, no `usize`, no pointer values. Not
/// cryptographic; collisions are only astronomically unlikely by accident.
#[derive(Clone, Copy, Debug)]
pub struct StateHasher(u64);

impl Default for StateHasher {
    fn default() -> Self {
        StateHasher::new()
    }
}

impl StateHasher {
    /// A hasher in its initial state.
    pub const fn new() -> StateHasher {
        StateHasher(0xCBF2_9CE4_8422_2325)
    }

    /// Mixes one word into the state. O(1).
    #[inline]
    pub fn write_u64(&mut self, x: u64) {
        self.0 = (self.0.rotate_left(5) ^ x).wrapping_mul(0x517C_C1B7_2722_0A95);
    }

    /// Mixes a signed word (two's complement bits).
    #[inline]
    pub fn write_i64(&mut self, x: i64) {
        self.write_u64(x as u64);
    }

    /// The hash of everything written so far (SplitMix64 finaliser over the
    /// state, so nearby inputs give unrelated outputs).
    pub fn finish(&self) -> u64 {
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}
