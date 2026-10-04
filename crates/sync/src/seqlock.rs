//! Week 34: a sequence lock — readers that never block the writer.
//!
//! The writer bumps a sequence counter to an **odd** value, writes the
//! data, and bumps it to the next **even** value. A reader reads the
//! counter, copies the data, and reads the counter again: if both reads
//! are the same even number, no write overlapped the copy and the copy is
//! consistent; otherwise it retries. Readers write nothing shared, so any
//! number of them scale perfectly and the writer never waits for them —
//! ideal for a top-of-book snapshot written by one market-data thread and
//! read by many.
//!
//! Costs: a read is two loads of the counter plus a copy of `T` (a few ns
//! when the line is cached; one cache miss after each write). A write is
//! one CAS, a copy, and one store. Keep `T` small (a few cache lines) — a
//! reader retries for as long as writes keep overlapping its copy.
//!
//! # The data race, and why it is handled this way
//!
//! A reader copies the data *while the writer may be writing it*. In the
//! C++/Rust memory model that copy is a data race. The standard mitigation
//! (used by Linux, `crossbeam`'s `AtomicCell`, and the `seqlock` crate) is:
//! copy with `ptr::read_volatile` into a `MaybeUninit<T>` (so the compiler
//! cannot assume the bytes are stable and a torn copy is never treated as
//! a valid `T`), order the copy between the two counter reads with
//! `Acquire` load + `Acquire` fence, and only `assume_init` the copy once
//! the counter proves it was not torn. `T: Copy` guarantees there is no
//! drop glue and no ownership to duplicate.
//!
//! ```
//! use sync::SeqLock;
//! let tob = SeqLock::new((100i64, 101i64));
//! tob.write((100, 102));
//! assert_eq!(tob.read(), (100, 102));
//! assert_eq!(tob.version(), 1);
//! ```

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use std::cell::UnsafeCell;
use std::fmt;
use std::mem::MaybeUninit;
use std::sync::atomic::{fence, AtomicUsize, Ordering};

/// A value readers copy optimistically while one writer updates it; see the
/// module docs. Writers are serialized by a CAS on the sequence number, so
/// a second concurrent writer is safe (it spins), but the design target is
/// a single writer.
pub struct SeqLock<T> {
    // TODO(w34): design this type's fields.
    _todo: std::marker::PhantomData<(T,)>,
}

// TODO(w34): implement — see the week guide on tepy.dev/hft.

#[allow(unused_variables)]
impl<T: Copy> SeqLock<T> {
    /// A seqlock holding `value`, version 0.
    pub fn new(value: T) -> SeqLock<T> {
        todo!("w34: new")
    }

    /// One read attempt: the value if no write was in progress or
    /// overlapped the copy, else `None`. Never blocks.
    #[inline]
    pub fn try_read(&self) -> Option<T> {
        todo!("w34: try_read")
    }

    /// Reads a consistent copy, retrying (spinning) while writes overlap.
    /// Never blocks the writer.
    #[inline]
    pub fn read(&self) -> T {
        todo!("w34: read")
    }

    /// Publishes `value`. Readers that overlap it retry. If another write
    /// is in progress, spins until it finishes.
    #[inline]
    pub fn write(&self, value: T) {
        todo!("w34: write")
    }

    /// Number of completed writes.
    pub fn version(&self) -> usize {
        todo!("w34: version")
    }

    /// Mutable access without synchronization — `&mut self` proves there
    /// are no readers or writers.
    pub fn get_mut(&mut self) -> &mut T {
        todo!("w34: get_mut")
    }

    /// Consumes the lock, returning the value.
    pub fn into_inner(self) -> T {
        todo!("w34: into_inner")
    }
}

impl<T: Copy + Default> Default for SeqLock<T> {
    fn default() -> Self {
        SeqLock::new(T::default())
    }
}

impl<T: Copy + fmt::Debug> fmt::Debug for SeqLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SeqLock")
            .field("version", &self.version())
            .field("value", &self.read())
            .finish()
    }
}
