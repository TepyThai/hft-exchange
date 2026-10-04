//! Week 4: cache-line padding and false sharing.
//!
//! The CPU moves memory between cores in whole cache lines. Two atomics
//! written by different cores that happen to share a line make that line
//! ping-pong between the cores ("false sharing"), even though the threads
//! never touch each other's data. [`CachePadded`] gives a value a line (or
//! two) of its own.
//!
//! # 64 or 128 bytes?
//!
//! Cache lines are 64 bytes on x86-64 (including the AMD EPYC 7543P the
//! reference numbers come from), but Intel's L2 *spatial prefetcher* pulls
//! lines in 128-byte-aligned pairs, so two values on adjacent 64-byte lines
//! can still interfere. Apple's M-series cores have 128-byte lines. We
//! therefore pad to **128 bytes on x86-64, aarch64 and powerpc64** (the same
//! choice as `crossbeam-utils`) and 64 bytes elsewhere. The cost is memory:
//! a padded `AtomicU64` takes 128 bytes instead of 8, which is irrelevant
//! for a handful of hot indices and counters, and the reason not to pad
//! large arrays of cold data.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};

/// Alignment (and minimum size) of a [`CachePadded<T>`] on this target, in
/// bytes. See the module docs for why it is 128 on x86-64.
#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "powerpc64"
))]
pub const CACHE_LINE: usize = 128;
/// Alignment (and minimum size) of a [`CachePadded<T>`] on this target, in
/// bytes. See the module docs for why it is 128 on x86-64.
#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "powerpc64"
)))]
pub const CACHE_LINE: usize = 64;

/// Pads and aligns a value to [`CACHE_LINE`] bytes so it never shares a
/// cache line (or an adjacent-line prefetch pair) with another value.
///
/// `size_of::<CachePadded<T>>()` is `size_of::<T>()` rounded up to a
/// multiple of [`CACHE_LINE`]; access is a plain field access (zero cost).
///
/// ```
/// use sync::CachePadded;
/// use std::sync::atomic::{AtomicUsize, Ordering};
///
/// let head = CachePadded::new(AtomicUsize::new(0));
/// head.store(1, Ordering::Relaxed); // derefs to the inner atomic
/// assert_eq!(std::mem::align_of_val(&head), sync::CACHE_LINE);
/// ```
#[cfg_attr(
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "powerpc64"
    ),
    repr(align(128))
)]
#[cfg_attr(
    not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "powerpc64"
    )),
    repr(align(64))
)]
pub struct CachePadded<T> {
    // TODO(w04): design this type's fields.
    _todo: std::marker::PhantomData<(T,)>,
}

#[allow(unused_variables)]
impl<T> CachePadded<T> {
    /// Wraps `value`. O(1), no allocation.
    pub fn new(value: T) -> CachePadded<T> {
        todo!("w04: new")
    }

    /// Unwraps the inner value.
    pub fn into_inner(self) -> T {
        todo!("w04: into_inner")
    }
}

#[allow(unused_variables)]
impl<T> Deref for CachePadded<T> {
    type Target = T;

    fn deref(&self) -> &T {
        todo!("w04: deref")
    }
}

#[allow(unused_variables)]
impl<T> DerefMut for CachePadded<T> {
    fn deref_mut(&mut self) -> &mut T {
        todo!("w04: deref_mut")
    }
}

impl<T: Default> Default for CachePadded<T> {
    fn default() -> Self {
        CachePadded::new(T::default())
    }
}

impl<T> From<T> for CachePadded<T> {
    fn from(value: T) -> Self {
        CachePadded::new(value)
    }
}

impl<T: fmt::Debug> fmt::Debug for CachePadded<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("CachePadded").field(&**self).finish()
    }
}

/// A fixed set of `u64` counters, one per thread (slot), each on its own
/// cache line — so threads incrementing their own slot never contend.
///
/// The classic use is statistics on the hot path: every thread bumps its
/// own slot, and a reporter occasionally sums them. Memory:
/// `slots × CACHE_LINE` bytes, allocated once in [`PaddedCounters::new`].
pub struct PaddedCounters {
    // TODO(w04): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl PaddedCounters {
    /// `slots` zeroed counters. Allocates `slots × CACHE_LINE` bytes.
    pub fn new(slots: usize) -> PaddedCounters {
        todo!("w04: new")
    }

    /// Number of slots.
    pub fn len(&self) -> usize {
        todo!("w04: len")
    }

    /// True if there are no slots.
    pub fn is_empty(&self) -> bool {
        todo!("w04: is_empty")
    }

    /// Adds `n` to `slot` (relaxed atomic add: a few ns on an uncontended
    /// line). Safe to call from any thread; fastest when each thread owns
    /// its slot. Panics if `slot >= len()`.
    #[inline]
    pub fn add(&self, slot: usize, n: u64) {
        todo!("w04: add")
    }

    /// Current value of one slot (relaxed load).
    pub fn get(&self, slot: usize) -> u64 {
        todo!("w04: get")
    }

    /// Sum of all slots. O(slots); concurrent adds may or may not be
    /// included.
    pub fn sum(&self) -> u64 {
        todo!("w04: sum")
    }
}

/// The false-sharing baseline for [`PaddedCounters`]: the same API, but
/// the counters are packed next to each other (8 per 64-byte line).
/// Provided — the week's lesson is measuring the difference.
pub struct UnpaddedCounters {
    slots: Box<[AtomicU64]>,
}

impl UnpaddedCounters {
    /// `slots` zeroed counters, packed contiguously (`8 × slots` bytes).
    pub fn new(slots: usize) -> UnpaddedCounters {
        UnpaddedCounters {
            slots: (0..slots).map(|_| AtomicU64::new(0)).collect(),
        }
    }

    /// Number of slots.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// True if there are no slots.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Adds `n` to `slot` (relaxed atomic add). Panics if `slot >= len()`.
    #[inline]
    pub fn add(&self, slot: usize, n: u64) {
        self.slots[slot].fetch_add(n, Ordering::Relaxed);
    }

    /// Current value of one slot (relaxed load).
    pub fn get(&self, slot: usize) -> u64 {
        self.slots[slot].load(Ordering::Relaxed)
    }

    /// Sum of all slots. O(slots).
    pub fn sum(&self) -> u64 {
        self.slots.iter().map(|c| c.load(Ordering::Relaxed)).sum()
    }
}
