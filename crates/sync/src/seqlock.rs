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

use std::cell::UnsafeCell;
use std::fmt;
use std::mem::MaybeUninit;
use std::sync::atomic::{fence, AtomicUsize, Ordering};

/// A value readers copy optimistically while one writer updates it; see the
/// module docs. Writers are serialized by a CAS on the sequence number, so
/// a second concurrent writer is safe (it spins), but the design target is
/// a single writer.
// @student w34
pub struct SeqLock<T> {
    /// Even: stable. Odd: a write is in progress. `seq / 2` writes done.
    seq: AtomicUsize,
    data: UnsafeCell<T>,
}

// @student-begin w34
// SAFETY: readers on other threads receive copies of `T` and writers move
// a `T` in from any thread, so `T: Send` is needed (as for `Mutex<T>`).
// Shared access is sound because writers are mutually excluded by the odd
// sequence number, and readers only *use* a copy that the sequence proves
// no write overlapped (see `read`).
unsafe impl<T: Copy + Send> Sync for SeqLock<T> {}
// @student-end

// @student w34
impl<T: Copy> SeqLock<T> {
    /// A seqlock holding `value`, version 0.
    pub fn new(value: T) -> SeqLock<T> {
        SeqLock {
            seq: AtomicUsize::new(0),
            data: UnsafeCell::new(value),
        }
    }

    /// One read attempt: the value if no write was in progress or
    /// overlapped the copy, else `None`. Never blocks.
    #[inline]
    pub fn try_read(&self) -> Option<T> {
        let s1 = self.seq.load(Ordering::Acquire);
        if s1 & 1 == 1 {
            return None;
        }
        // SAFETY: `data` is a valid, aligned `T` location for the life of
        // `self`. The read may race with a writer (see the module docs):
        // it is volatile and lands in a `MaybeUninit`, so a torn copy is
        // never interpreted as a `T` — we only `assume_init` below after
        // the sequence check proves no write overlapped it.
        let copy = unsafe { std::ptr::read_volatile(self.data.get().cast::<MaybeUninit<T>>()) };
        // Keep the copy before the second counter read (pairs with the
        // writer's Release fence after it made the counter odd).
        fence(Ordering::Acquire);
        let s2 = self.seq.load(Ordering::Relaxed);
        if s1 == s2 {
            // SAFETY: the counter was even and unchanged across the copy,
            // so no write overlapped it and the bytes are the complete `T`
            // published by the write that made the counter `s1`.
            Some(unsafe { copy.assume_init() })
        } else {
            None
        }
    }

    /// Reads a consistent copy, retrying (spinning) while writes overlap.
    /// Never blocks the writer.
    #[inline]
    pub fn read(&self) -> T {
        loop {
            if let Some(v) = self.try_read() {
                return v;
            }
            std::hint::spin_loop();
        }
    }

    /// Publishes `value`. Readers that overlap it retry. If another write
    /// is in progress, spins until it finishes.
    #[inline]
    pub fn write(&self, value: T) {
        let mut s = self.seq.load(Ordering::Relaxed);
        loop {
            if s & 1 == 0 {
                // Acquire: this write's data stores stay after the
                // counter becomes odd, and see the previous writer's data.
                match self.seq.compare_exchange_weak(
                    s,
                    s.wrapping_add(1),
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(cur) => s = cur,
                }
            } else {
                std::hint::spin_loop();
                s = self.seq.load(Ordering::Relaxed);
            }
        }
        // Order the odd counter before the data stores as seen by readers.
        fence(Ordering::Release);
        // SAFETY: the odd counter excludes every other writer, so this is
        // the only write to `data`; concurrent readers may copy it, but
        // discard what they copied (the counter changed), see `try_read`.
        unsafe { std::ptr::write_volatile(self.data.get(), value) };
        // Release: a reader that sees the even counter sees all the data.
        self.seq.store(s.wrapping_add(2), Ordering::Release);
    }

    /// Number of completed writes.
    pub fn version(&self) -> usize {
        self.seq.load(Ordering::Acquire) / 2
    }

    /// Mutable access without synchronization — `&mut self` proves there
    /// are no readers or writers.
    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }

    /// Consumes the lock, returning the value.
    pub fn into_inner(self) -> T {
        self.data.into_inner()
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
