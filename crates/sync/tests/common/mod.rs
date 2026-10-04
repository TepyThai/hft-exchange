//! Helpers shared by the week test files (each `tests/*.rs` is its own
//! crate and uses a subset).
#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// SplitMix64 — the same generator as `types::Rng`, inlined so the `sync`
/// crate stays free of workspace dependencies.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `lo..=hi`.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next_u64() % (hi - lo + 1)
    }

    pub fn chance(&mut self, p: f64) -> bool {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64 <= p
    }
}

/// Spins briefly, then yields — keeps threads tightly interleaved without
/// livelocking a machine with fewer cores than threads (CI runners).
pub fn backoff(spins: &mut u32) {
    *spins += 1;
    if *spins < 64 {
        std::hint::spin_loop();
    } else {
        *spins = 0;
        std::thread::yield_now();
    }
}

/// Runs `f` on its own thread and fails the test if it does not finish in
/// `secs` seconds (a deadlock or lost wakeup), instead of hanging CI.
pub fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(v) => {
            handle.join().unwrap();
            v
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            // `f` panicked: surface its panic.
            match handle.join() {
                Err(p) => std::panic::resume_unwind(p),
                Ok(()) => unreachable!(),
            }
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("did not finish within {secs}s (deadlock or lost wakeup?)")
        }
    }
}

/// A value that counts its drops, to prove containers drop each item
/// exactly once.
pub struct Tracked {
    pub id: u64,
    drops: Arc<AtomicUsize>,
}

impl Tracked {
    pub fn new(id: u64, drops: &Arc<AtomicUsize>) -> Tracked {
        Tracked {
            id,
            drops: Arc::clone(drops),
        }
    }
}

impl Drop for Tracked {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}
