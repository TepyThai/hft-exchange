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

// @student w29
impl Kernels {
    /// The fastest kernels this CPU supports: AVX2 when detected at run
    /// time, scalar otherwise. Detects once; later calls are one atomic
    /// load.
    pub fn detect() -> &'static Kernels {
        static CHOSEN: std::sync::OnceLock<&'static Kernels> = std::sync::OnceLock::new();
        CHOSEN.get_or_init(|| Kernels::avx2().unwrap_or(&SCALAR))
    }

    /// The AVX2 kernels, if this CPU has AVX2 (always `None` off x86-64).
    pub fn avx2() -> Option<&'static Kernels> {
        #[cfg(target_arch = "x86_64")]
        {
            if std::arch::is_x86_feature_detected!("avx2") {
                return Some(&AVX2);
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Scalar: the straightforward loops

// @student w29
fn sum_scalar(xs: &[u64]) -> u64 {
    xs.iter().fold(0u64, |acc, &x| acc.wrapping_add(x))
}

// @student w29
fn first_nonzero_scalar(xs: &[u64]) -> Option<usize> {
    xs.iter().position(|&x| x != 0)
}

// @student w29
fn first_at_least_scalar(xs: &[u64], min: u64) -> Option<usize> {
    xs.iter().position(|&x| x >= min)
}

// @student w29
fn levels_to_fill_scalar(xs: &[u64], target: u64) -> Option<usize> {
    if target == 0 {
        return Some(0);
    }
    let mut cum = 0u128;
    for (i, &x) in xs.iter().enumerate() {
        cum += x as u128;
        if cum >= target as u128 {
            return Some(i + 1);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// AVX2. Every kernel loads 16 levels (4 vectors) per iteration with
// unaligned loads, then finishes with whole vectors and a scalar tail.
//
// Safety contract: an `avx2` function may only run on a CPU with AVX2. The
// safe `*_entry` wrappers are reachable only through the `AVX2` table,
// which `Kernels::avx2` hands out only after detecting the feature.

// @student w29
#[cfg(target_arch = "x86_64")]
fn sum_avx2_entry(xs: &[u64]) -> u64 {
    // SAFETY: only reachable through the AVX2 table (see above).
    unsafe { sum_avx2(xs) }
}

// @student w29
#[cfg(target_arch = "x86_64")]
fn first_nonzero_avx2_entry(xs: &[u64]) -> Option<usize> {
    // SAFETY: only reachable through the AVX2 table (see above).
    unsafe { first_nonzero_avx2(xs) }
}

// @student w29
#[cfg(target_arch = "x86_64")]
fn first_at_least_avx2_entry(xs: &[u64], min: u64) -> Option<usize> {
    // SAFETY: only reachable through the AVX2 table (see above).
    unsafe { first_at_least_avx2(xs, min) }
}

// @student w29
#[cfg(target_arch = "x86_64")]
fn levels_to_fill_avx2_entry(xs: &[u64], target: u64) -> Option<usize> {
    // SAFETY: only reachable through the AVX2 table (see above).
    unsafe { levels_to_fill_avx2(xs, target) }
}

// @student-begin w29
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

/// Loads 4 × u64 from `xs[at..at + 4]`.
///
/// # Safety
/// AVX2 must be available and `at + 4 <= xs.len()`.
#[cfg(target_arch = "x86_64")]
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn load4(xs: &[u64], at: usize) -> __m256i {
    debug_assert!(at + 4 <= xs.len());
    // SAFETY: the caller guarantees 4 elements (32 bytes) from `at`;
    // `loadu` has no alignment requirement.
    unsafe { _mm256_loadu_si256(xs.as_ptr().add(at) as *const __m256i) }
}

/// Wrapping sum of the 4 lanes.
#[cfg(target_arch = "x86_64")]
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn hsum(v: __m256i) -> u64 {
    let s = _mm_add_epi64(_mm256_castsi256_si128(v), _mm256_extracti128_si256::<1>(v));
    (_mm_cvtsi128_si64(s) as u64).wrapping_add(_mm_extract_epi64::<1>(s) as u64)
}

/// Bit `i` set where lane `i` is non-zero.
#[cfg(target_arch = "x86_64")]
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn nonzero_mask(v: __m256i) -> u32 {
    let zero = _mm256_cmpeq_epi64(v, _mm256_setzero_si256());
    !(_mm256_movemask_pd(_mm256_castsi256_pd(zero)) as u32) & 0xF
}

/// Bit `i` set where lane `i` is below `min` — both operands already
/// XORed with the sign bit, so the signed compare orders them as unsigned.
#[cfg(target_arch = "x86_64")]
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn below(v: __m256i, min_flipped: __m256i) -> __m256i {
    let flip = _mm256_set1_epi64x(i64::MIN);
    _mm256_cmpgt_epi64(min_flipped, _mm256_xor_si256(v, flip))
}

/// # Safety
/// AVX2 must be available.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn sum_avx2(xs: &[u64]) -> u64 {
    let n = xs.len();
    let (mut a, mut b, mut c, mut d) = (
        _mm256_setzero_si256(),
        _mm256_setzero_si256(),
        _mm256_setzero_si256(),
        _mm256_setzero_si256(),
    );
    let mut i = 0;
    // Four independent accumulators keep four adds in flight.
    while i + 16 <= n {
        // SAFETY: i + 16 <= n.
        unsafe {
            a = _mm256_add_epi64(a, load4(xs, i));
            b = _mm256_add_epi64(b, load4(xs, i + 4));
            c = _mm256_add_epi64(c, load4(xs, i + 8));
            d = _mm256_add_epi64(d, load4(xs, i + 12));
        }
        i += 16;
    }
    while i + 4 <= n {
        // SAFETY: i + 4 <= n.
        a = _mm256_add_epi64(a, unsafe { load4(xs, i) });
        i += 4;
    }
    let v = _mm256_add_epi64(_mm256_add_epi64(a, b), _mm256_add_epi64(c, d));
    // SAFETY: AVX2 is available (this function's contract).
    let mut total = unsafe { hsum(v) };
    for &x in &xs[i..] {
        total = total.wrapping_add(x);
    }
    total
}

/// # Safety
/// AVX2 must be available.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn first_nonzero_avx2(xs: &[u64]) -> Option<usize> {
    let n = xs.len();
    let mut i = 0;
    while i + 16 <= n {
        // SAFETY: i + 16 <= n; AVX2 per the contract.
        unsafe {
            let (a, b, c, d) = (
                load4(xs, i),
                load4(xs, i + 4),
                load4(xs, i + 8),
                load4(xs, i + 12),
            );
            let any = _mm256_or_si256(_mm256_or_si256(a, b), _mm256_or_si256(c, d));
            if _mm256_testz_si256(any, any) == 0 {
                let mask = nonzero_mask(a)
                    | nonzero_mask(b) << 4
                    | nonzero_mask(c) << 8
                    | nonzero_mask(d) << 12;
                return Some(i + mask.trailing_zeros() as usize);
            }
        }
        i += 16;
    }
    while i + 4 <= n {
        // SAFETY: i + 4 <= n; AVX2 per the contract.
        let mask = unsafe { nonzero_mask(load4(xs, i)) };
        if mask != 0 {
            return Some(i + mask.trailing_zeros() as usize);
        }
        i += 4;
    }
    xs[i..].iter().position(|&x| x != 0).map(|k| i + k)
}

/// Bit `i` set where lane `i` of a [`below`] result is clear (the value is
/// at least the minimum).
#[cfg(target_arch = "x86_64")]
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn lanes(lt: __m256i) -> u32 {
    !(_mm256_movemask_pd(_mm256_castsi256_pd(lt)) as u32) & 0xF
}

/// # Safety
/// AVX2 must be available.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn first_at_least_avx2(xs: &[u64], min: u64) -> Option<usize> {
    let n = xs.len();
    let m = _mm256_set1_epi64x((min ^ (1 << 63)) as i64);
    let mut i = 0;
    while i + 16 <= n {
        // SAFETY: i + 16 <= n; AVX2 per the contract.
        unsafe {
            let (a, b, c, d) = (
                below(load4(xs, i), m),
                below(load4(xs, i + 4), m),
                below(load4(xs, i + 8), m),
                below(load4(xs, i + 12), m),
            );
            let all_below = _mm256_and_si256(_mm256_and_si256(a, b), _mm256_and_si256(c, d));
            if _mm256_movemask_pd(_mm256_castsi256_pd(all_below)) != 0xF {
                let hits = lanes(a) | lanes(b) << 4 | lanes(c) << 8 | lanes(d) << 12;
                return Some(i + hits.trailing_zeros() as usize);
            }
        }
        i += 16;
    }
    while i + 4 <= n {
        // SAFETY: i + 4 <= n; AVX2 per the contract.
        let hits = unsafe { lanes(below(load4(xs, i), m)) };
        if hits != 0 {
            return Some(i + hits.trailing_zeros() as usize);
        }
        i += 4;
    }
    xs[i..].iter().position(|&x| x >= min).map(|k| i + k)
}

/// Two phases: sum whole blocks of 16 with vector adds until the block
/// that crosses the target, then walk that block level by level. A block
/// holding a value ≥ 2⁵⁹ could overflow a lane sum, so it is walked
/// exactly instead.
///
/// # Safety
/// AVX2 must be available.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn levels_to_fill_avx2(xs: &[u64], target: u64) -> Option<usize> {
    if target == 0 {
        return Some(0);
    }
    let n = xs.len();
    let target = target as u128;
    let high = _mm256_set1_epi64x(!((1u64 << 59) - 1) as i64);
    let mut cum = 0u128;
    let mut i = 0;
    while i + 16 <= n {
        // SAFETY: i + 16 <= n; AVX2 per the contract.
        let block = unsafe {
            let (a, b, c, d) = (
                load4(xs, i),
                load4(xs, i + 4),
                load4(xs, i + 8),
                load4(xs, i + 12),
            );
            let any = _mm256_or_si256(_mm256_or_si256(a, b), _mm256_or_si256(c, d));
            if _mm256_testz_si256(any, high) == 1 {
                // Every value < 2^59: the 16-value sum fits in 63 bits.
                Some(hsum(_mm256_add_epi64(
                    _mm256_add_epi64(a, b),
                    _mm256_add_epi64(c, d),
                )) as u128)
            } else {
                None
            }
        };
        match block {
            Some(s) if cum + s < target => cum += s,
            _ => {
                for (k, &x) in xs[i..i + 16].iter().enumerate() {
                    cum += x as u128;
                    if cum >= target {
                        return Some(i + k + 1);
                    }
                }
            }
        }
        i += 16;
    }
    for (k, &x) in xs[i..].iter().enumerate() {
        cum += x as u128;
        if cum >= target {
            return Some(i + k + 1);
        }
    }
    None
}
// @student-end
