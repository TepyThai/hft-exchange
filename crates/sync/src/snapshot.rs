//! Week 34: an atomically swappable, epoch-reclaimed snapshot.
//!
//! A [`SnapshotCell<T>`] holds the current version of some read-mostly
//! state (an instrument table, a risk-limit set, a full book snapshot for
//! late joiners). A writer [`publish`](SnapshotCell::publish)es a whole new
//! version; readers ([`SnapshotReader`]) get at the current one **without
//! locks and without ever waiting for the writer** — unlike a seqlock they
//! never retry, and `T` can be large or own heap data.
//!
//! # Representation
//!
//! The current version is an `Arc<T>` stored as a raw pointer in an
//! `AtomicPtr<T>` (`Arc::into_raw`). Publishing swaps the pointer. The hard
//! part is the old version: a reader may have loaded the old pointer an
//! instant before the swap and be about to read through it (or bump its
//! reference count), so the writer cannot drop it right away.
//!
//! # Reclamation: a per-cell epoch with per-reader announcements
//!
//! * The cell has a global `epoch` counter (starts at 1). Each reader owns
//!   a cache-padded `slot` holding the epoch it is reading in, or `0` when
//!   idle.
//! * **Reader** (`pin`): `e = epoch.load()`, `slot.store(e)`,
//!   `p = current.load()`, use `p`, then `slot.store(0)`.
//! * **Writer** (`publish`): `old = current.swap(new)`,
//!   `e_w = epoch.fetch_add(1)`, put `(e_w, old)` on a *retired* list, then
//!   free every retired entry whose epoch is **lower than the epoch of
//!   every pinned reader**.
//!
//! **Safety argument.** All six operations are `SeqCst`, so they appear in
//! one total order S. Suppose a reader's `current.load()` returned `old`.
//! Then that load precedes the writer's swap in S, and so do the reader's
//! earlier `epoch.load()` and `slot.store(e)`; the swap precedes the
//! `fetch_add`, so `e <= e_w`. The writer's scan of the slots comes after
//! all of that, so it sees `e` (or a later value the reader stored after
//! it finished with `old` — `0`, or a re-pin, which is a `SeqCst` store
//! that synchronizes the reader's earlier use of `old` with the writer).
//! Since `e <= e_w`, `old` is kept. Conversely, a reader whose slot shows
//! `e > e_w` read the epoch after the `fetch_add`, hence loaded the pointer
//! after the swap, and cannot hold `old`. Retired versions are freed by
//! dropping the cell's `Arc`, so a reader that took its own reference
//! ([`SnapshotReader::load`]) keeps the version alive as long as it likes.
//! Because a pointer is never reused while any reader may hold it, there is
//! no ABA.
//!
//! **Costs.** A reader pin is one `SeqCst` store to its own cache line
//! (~10–20 ns on x86) and two loads of shared, read-mostly lines; no
//! reader ever writes a line another reader touches. A publish allocates
//! (`Arc::new`), takes the writer lock, and scans every registered reader
//! slot — O(readers). A reader that stays pinned forever (a closure that
//! never returns) blocks reclamation, so retired versions accumulate; keep
//! [`SnapshotReader::read`] closures short.
//!
//! ```
//! use sync::SnapshotCell;
//! let cell = SnapshotCell::new(vec![1, 2, 3]);
//! let mut reader = cell.reader();
//! cell.publish(vec![4, 5]);
//! assert_eq!(reader.read(|v| v.len()), 2);
//! let held = reader.load(); // an Arc: survives later publishes
//! cell.publish(vec![]);
//! assert_eq!(*held, vec![4, 5]);
//! ```

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::pad::CachePadded;
use std::fmt;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

// TODO(w34): implement — see the week guide on tepy.dev/hft.

/// The publishing side of a snapshot cell; also hands out readers. Share
/// it by reference or in an `Arc`; concurrent publishers are serialized by
/// an internal lock.
pub struct SnapshotCell<T> {
    // TODO(w34): design this type's fields.
    _todo: std::marker::PhantomData<(T,)>,
}

/// A reader of a [`SnapshotCell`]. Owned by one thread (`Send`, not
/// `Clone` — create one per reading thread with
/// [`SnapshotCell::reader`]).
pub struct SnapshotReader<T> {
    // TODO(w34): design this type's fields.
    _todo: std::marker::PhantomData<(T,)>,
}

#[allow(unused_variables)]
impl<T> SnapshotCell<T> {
    /// A cell whose first version is `value`. Allocates once.
    pub fn new(value: T) -> SnapshotCell<T> {
        todo!("w34: new")
    }

    /// A cell whose first version is `value` (no copy).
    pub fn from_arc(value: Arc<T>) -> SnapshotCell<T> {
        todo!("w34: from_arc")
    }

    /// Makes `value` the current version. Allocates the `Arc`; see
    /// [`publish_arc`](Self::publish_arc).
    pub fn publish(&self, value: T) {
        todo!("w34: publish")
    }

    /// Makes `value` the current version and frees old versions no reader
    /// can still see. Never waits for readers. O(registered readers).
    pub fn publish_arc(&self, value: Arc<T>) {
        todo!("w34: publish_arc")
    }

    /// A new reader. Takes a lock and allocates one padded slot; do it once
    /// per reading thread, not per read.
    pub fn reader(&self) -> SnapshotReader<T> {
        todo!("w34: reader")
    }

    /// The current version, read from the writer side (takes the writer
    /// lock, so it waits for a concurrent publish). Readers should use
    /// [`SnapshotReader::load`].
    pub fn load(&self) -> Arc<T> {
        todo!("w34: load")
    }

    /// Number of publishes so far.
    pub fn version(&self) -> u64 {
        todo!("w34: version")
    }

    /// Old versions waiting for readers to move on before being freed.
    /// Zero when no reader was mid-read during the last publish.
    pub fn pending_reclaim(&self) -> usize {
        todo!("w34: pending_reclaim")
    }

    /// Frees retired versions that no pinned reader can still see (publish
    /// does this too) and returns how many are still pending.
    /// O(registered readers).
    pub fn reclaim(&self) -> usize {
        todo!("w34: reclaim")
    }
}

#[allow(unused_variables)]
impl<T> SnapshotReader<T> {
    /// Runs `f` on the current version without touching its reference
    /// count: the reader is pinned for the duration of `f`, which keeps the
    /// version alive. Lock-free and wait-free; costs one `SeqCst` store to
    /// the reader's own cache line. Keep `f` short — a pinned reader delays
    /// reclamation.
    #[inline]
    pub fn read<R>(&mut self, f: impl FnOnce(&T) -> R) -> R {
        todo!("w34: read")
    }

    /// The current version as an `Arc` the caller may keep (one atomic
    /// increment on the version's reference count, which readers of the
    /// same version share — prefer [`read`](Self::read) on hot paths).
    pub fn load(&mut self) -> Arc<T> {
        todo!("w34: load")
    }
}

impl<T> fmt::Debug for SnapshotCell<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotCell")
            .field("version", &self.version())
            .finish()
    }
}

impl<T> fmt::Debug for SnapshotReader<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SnapshotReader")
    }
}
