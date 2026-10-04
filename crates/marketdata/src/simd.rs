//! Week 29 — SIMD kernels over ladder quantity arrays.
//!
//! The [`LadderDepth`](crate::LadderDepth) keeps each side's quantities in
//! one dense `[u64]`, best level first, so the questions market-data
//! consumers ask become loops over contiguous memory:
//!
//! | kernel | question |
//! | --- | --- |
//! | [`sum`](Kernels::sum) | total quantity over a range of price levels |
//! | [`first_nonzero`](Kernels::first_nonzero) | next non-empty level (best-price rescan) |
//! | [`first_at_least`](Kernels::first_at_least) | first level holding at least `min` |
//! | [`levels_to_fill`](Kernels::levels_to_fill) | how many levels an order of size `q` sweeps |
//!
//! Each has a portable **scalar** version — the straightforward loop, which
//! is both the fallback and the reference in tests — and, on x86-64, an
//! **AVX2** version (4 × u64 lanes, 4 vectors per iteration) behind
//! `is_x86_feature_detected!`. All versions return identical results for
//! every input; sums wrap on overflow (wrapping addition is associative,
//! so lane order cannot change the answer), and `levels_to_fill`
//! accumulates exactly in 128 bits.
//!
//! The honest part: LLVM already vectorizes the scalar `sum` with the
//! baseline SSE2 (2 lanes), so AVX2 buys at most ~2× there. The searches
//! have data-dependent early exits, which LLVM does not vectorize; there
//! the explicit kernels test 16 levels per compare-and-branch.
//!
//! **Dispatch once.** [`Kernels::detect`] picks a table of function
//! pointers the first time it is called; a ladder stores the table, so the
//! hot path never re-checks CPU features.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use std::fmt;

/// One implementation of every kernel. Obtain one with
/// [`Kernels::detect`] (the fastest this CPU supports),
/// [`Kernels::scalar`] or [`Kernels::avx2`]; calls go through function
/// pointers chosen once.
pub struct Kernels {
    name: &'static str,
    sum: fn(&[u64]) -> u64,
    first_nonzero: fn(&[u64]) -> Option<usize>,
    first_at_least: fn(&[u64], u64) -> Option<usize>,
    levels_to_fill: fn(&[u64], u64) -> Option<usize>,
}

impl fmt::Debug for Kernels {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Kernels({})", self.name)
    }
}

static SCALAR: Kernels = Kernels {
    name: "scalar",
    sum: sum_scalar,
    first_nonzero: first_nonzero_scalar,
    first_at_least: first_at_least_scalar,
    levels_to_fill: levels_to_fill_scalar,
};

#[cfg(target_arch = "x86_64")]
static AVX2: Kernels = Kernels {
    name: "avx2",
    sum: sum_avx2_entry,
    first_nonzero: first_nonzero_avx2_entry,
    first_at_least: first_at_least_avx2_entry,
    levels_to_fill: levels_to_fill_avx2_entry,
};

impl Kernels {
    /// The portable scalar kernels (the reference).
    pub fn scalar() -> &'static Kernels {
        &SCALAR
    }

    /// `"scalar"` or `"avx2"`.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Wrapping sum of `xs`. O(len).
    #[inline]
    pub fn sum(&self, xs: &[u64]) -> u64 {
        (self.sum)(xs)
    }

    /// Index of the first non-zero element. O(position).
    #[inline]
    pub fn first_nonzero(&self, xs: &[u64]) -> Option<usize> {
        (self.first_nonzero)(xs)
    }

    /// Index of the first element `>= min` (unsigned). O(position).
    #[inline]
    pub fn first_at_least(&self, xs: &[u64], min: u64) -> Option<usize> {
        (self.first_at_least)(xs, min)
    }

    /// The smallest `k` with `xs[0] + … + xs[k-1] >= target` (computed
    /// exactly, no overflow): `Some(0)` when `target` is 0, `None` when the
    /// whole slice holds less. O(k).
    #[inline]
    pub fn levels_to_fill(&self, xs: &[u64], target: u64) -> Option<usize> {
        (self.levels_to_fill)(xs, target)
    }
}

#[allow(unused_variables)]
impl Kernels {
    /// The fastest kernels this CPU supports: AVX2 when detected at run
    /// time, scalar otherwise. Detects once; later calls are one atomic
    /// load.
    pub fn detect() -> &'static Kernels {
        todo!("w29: detect")
    }

    /// The AVX2 kernels, if this CPU has AVX2 (always `None` off x86-64).
    pub fn avx2() -> Option<&'static Kernels> {
        todo!("w29: avx2")
    }
}

// ---------------------------------------------------------------------------
// Scalar: the straightforward loops

#[allow(unused_variables)]
fn sum_scalar(xs: &[u64]) -> u64 {
    todo!("w29: sum_scalar")
}

#[allow(unused_variables)]
fn first_nonzero_scalar(xs: &[u64]) -> Option<usize> {
    todo!("w29: first_nonzero_scalar")
}

#[allow(unused_variables)]
fn first_at_least_scalar(xs: &[u64], min: u64) -> Option<usize> {
    todo!("w29: first_at_least_scalar")
}

#[allow(unused_variables)]
fn levels_to_fill_scalar(xs: &[u64], target: u64) -> Option<usize> {
    todo!("w29: levels_to_fill_scalar")
}

// ---------------------------------------------------------------------------
// AVX2. Every kernel loads 16 levels (4 vectors) per iteration with
// unaligned loads, then finishes with whole vectors and a scalar tail.
//
// Safety contract: an `avx2` function may only run on a CPU with AVX2. The
// safe `*_entry` wrappers are reachable only through the `AVX2` table,
// which `Kernels::avx2` hands out only after detecting the feature.

#[cfg(target_arch = "x86_64")]
#[allow(unused_variables)]
fn sum_avx2_entry(xs: &[u64]) -> u64 {
    todo!("w29: sum_avx2_entry")
}

#[cfg(target_arch = "x86_64")]
#[allow(unused_variables)]
fn first_nonzero_avx2_entry(xs: &[u64]) -> Option<usize> {
    todo!("w29: first_nonzero_avx2_entry")
}

#[cfg(target_arch = "x86_64")]
#[allow(unused_variables)]
fn first_at_least_avx2_entry(xs: &[u64], min: u64) -> Option<usize> {
    todo!("w29: first_at_least_avx2_entry")
}

#[cfg(target_arch = "x86_64")]
#[allow(unused_variables)]
fn levels_to_fill_avx2_entry(xs: &[u64], target: u64) -> Option<usize> {
    todo!("w29: levels_to_fill_avx2_entry")
}

// TODO(w29): implement — see the week guide on tepy.dev/hft.
