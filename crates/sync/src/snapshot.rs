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

use crate::pad::CachePadded;
use std::fmt;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

// @student-begin w34
/// Slot value of a reader that is not reading.
const IDLE: u64 = 0;

/// One reader's announcement. Padded: readers never share a line.
struct Slot {
    epoch: CachePadded<AtomicU64>,
}

struct Inner<T> {
    /// From `Arc::into_raw`; the cell owns one strong count through it.
    current: AtomicPtr<T>,
    epoch: CachePadded<AtomicU64>,
    /// Writer-side state; holding this lock also prevents reclamation,
    /// which is what makes `SnapshotCell::load` safe.
    writer: Mutex<Vec<(u64, Arc<T>)>>,
    /// Registered reader slots. Readers touch this only when created; the
    /// writer scans it on every publish.
    readers: Mutex<Vec<Arc<Slot>>>,
    /// `AtomicPtr<T>` is `Send + Sync` for any `T`; this restores the
    /// bounds of the `Arc<T>` we logically own.
    _owns: PhantomData<Arc<T>>,
}

impl<T> Inner<T> {
    fn lock_writer(&self) -> MutexGuard<'_, Vec<(u64, Arc<T>)>> {
        self.writer.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Frees every retired version no pinned reader can still see.
    fn reclaim(&self, retired: &mut Vec<(u64, Arc<T>)>) {
        let mut oldest_pinned = u64::MAX;
        {
            let mut readers = self.readers.lock().unwrap_or_else(|e| e.into_inner());
            // A slot only the registry still references belongs to a
            // dropped reader.
            readers.retain(|slot| Arc::strong_count(slot) > 1);
            for slot in readers.iter() {
                let e = slot.epoch.load(Ordering::SeqCst);
                if e != IDLE {
                    oldest_pinned = oldest_pinned.min(e);
                }
            }
        }
        // Keep (e_w, old) only while some reader pinned at e <= e_w.
        retired.retain(|(retired_at, _)| *retired_at >= oldest_pinned);
    }
}

impl<T> Drop for Inner<T> {
    fn drop(&mut self) {
        // SAFETY: `current` always holds a pointer from `Arc::into_raw`
        // whose strong count the cell owns; no handle is left to read it.
        drop(unsafe { Arc::from_raw(*self.current.get_mut()) });
    }
}

/// Unpins a reader slot when dropped, even if the read closure panics.
struct PinGuard<'a>(&'a Slot);

impl Drop for PinGuard<'_> {
    fn drop(&mut self) {
        self.0.epoch.store(IDLE, Ordering::Release);
    }
}
// @student-end

/// The publishing side of a snapshot cell; also hands out readers. Share
/// it by reference or in an `Arc`; concurrent publishers are serialized by
/// an internal lock.
// @student w34
pub struct SnapshotCell<T> {
    inner: Arc<Inner<T>>,
}

/// A reader of a [`SnapshotCell`]. Owned by one thread (`Send`, not
/// `Clone` — create one per reading thread with
/// [`SnapshotCell::reader`]).
// @student w34
pub struct SnapshotReader<T> {
    inner: Arc<Inner<T>>,
    slot: Arc<Slot>,
}

// @student w34
impl<T> SnapshotCell<T> {
    /// A cell whose first version is `value`. Allocates once.
    pub fn new(value: T) -> SnapshotCell<T> {
        SnapshotCell::from_arc(Arc::new(value))
    }

    /// A cell whose first version is `value` (no copy).
    pub fn from_arc(value: Arc<T>) -> SnapshotCell<T> {
        SnapshotCell {
            inner: Arc::new(Inner {
                current: AtomicPtr::new(Arc::into_raw(value).cast_mut()),
                epoch: CachePadded::new(AtomicU64::new(1)),
                writer: Mutex::new(Vec::new()),
                readers: Mutex::new(Vec::new()),
                _owns: PhantomData,
            }),
        }
    }

    /// Makes `value` the current version. Allocates the `Arc`; see
    /// [`publish_arc`](Self::publish_arc).
    pub fn publish(&self, value: T) {
        self.publish_arc(Arc::new(value));
    }

    /// Makes `value` the current version and frees old versions no reader
    /// can still see. Never waits for readers. O(registered readers).
    pub fn publish_arc(&self, value: Arc<T>) {
        let inner = &*self.inner;
        let mut retired = inner.lock_writer();
        let new = Arc::into_raw(value).cast_mut();
        let old = inner.current.swap(new, Ordering::SeqCst);
        let retired_at = inner.epoch.fetch_add(1, Ordering::SeqCst);
        // SAFETY: `old` came from `Arc::into_raw` and the cell owned its
        // strong count; we move that ownership into the retired list.
        retired.push((retired_at, unsafe { Arc::from_raw(old) }));
        inner.reclaim(&mut retired);
    }

    /// A new reader. Takes a lock and allocates one padded slot; do it once
    /// per reading thread, not per read.
    pub fn reader(&self) -> SnapshotReader<T> {
        let slot = Arc::new(Slot {
            epoch: CachePadded::new(AtomicU64::new(IDLE)),
        });
        self.inner
            .readers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Arc::clone(&slot));
        SnapshotReader {
            inner: Arc::clone(&self.inner),
            slot,
        }
    }

    /// The current version, read from the writer side (takes the writer
    /// lock, so it waits for a concurrent publish). Readers should use
    /// [`SnapshotReader::load`].
    pub fn load(&self) -> Arc<T> {
        let _writer = self.inner.lock_writer();
        let p = self.inner.current.load(Ordering::SeqCst);
        // SAFETY: versions are only retired or freed under the writer
        // lock, which we hold, so `p` is the live current version and its
        // strong count is at least 1.
        unsafe {
            Arc::increment_strong_count(p);
            Arc::from_raw(p)
        }
    }

    /// Number of publishes so far.
    pub fn version(&self) -> u64 {
        self.inner.epoch.load(Ordering::SeqCst) - 1
    }

    /// Old versions waiting for readers to move on before being freed.
    /// Zero when no reader was mid-read during the last publish.
    pub fn pending_reclaim(&self) -> usize {
        self.inner.lock_writer().len()
    }

    /// Frees retired versions that no pinned reader can still see (publish
    /// does this too) and returns how many are still pending.
    /// O(registered readers).
    pub fn reclaim(&self) -> usize {
        let mut retired = self.inner.lock_writer();
        self.inner.reclaim(&mut retired);
        retired.len()
    }
}

// @student w34
impl<T> SnapshotReader<T> {
    /// Runs `f` on the current version without touching its reference
    /// count: the reader is pinned for the duration of `f`, which keeps the
    /// version alive. Lock-free and wait-free; costs one `SeqCst` store to
    /// the reader's own cache line. Keep `f` short — a pinned reader delays
    /// reclamation.
    #[inline]
    pub fn read<R>(&mut self, f: impl FnOnce(&T) -> R) -> R {
        let inner = &*self.inner;
        let e = inner.epoch.load(Ordering::SeqCst);
        self.slot.epoch.store(e, Ordering::SeqCst);
        let _unpin = PinGuard(&self.slot);
        let p = inner.current.load(Ordering::SeqCst);
        // SAFETY: we announced epoch `e` before loading `p`, so the writer
        // will not free `p` until we unpin (module docs, safety argument);
        // `p` came from `Arc::into_raw`, so it points at a live `T`.
        // `&mut self` guarantees no other pin on this slot overlaps ours.
        f(unsafe { &*p })
    }

    /// The current version as an `Arc` the caller may keep (one atomic
    /// increment on the version's reference count, which readers of the
    /// same version share — prefer [`read`](Self::read) on hot paths).
    pub fn load(&mut self) -> Arc<T> {
        self.read(|v| {
            let p: *const T = v;
            // SAFETY: `p` is a live `Arc`-managed `T` (we are pinned), so
            // taking another strong count on it is sound.
            unsafe {
                Arc::increment_strong_count(p);
                Arc::from_raw(p)
            }
        })
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
