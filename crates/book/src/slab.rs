//! Week 27: a generational slab — preallocated storage with stable keys.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
/// A stable handle to a value in a [`Slab`]: the slot index plus the
/// slot's **generation** when the value was inserted.
///
/// Slots are reused after removal; the generation is bumped every time a
/// slot is freed, so a key kept after its value was removed (an order that
/// has since been filled, its slot now holding a different order) no longer
/// matches and every checked access returns `None` instead of the wrong
/// value — the classic ABA problem of index handles, solved with 4 bytes.
/// (Generations wrap: a stale key could only match again after 2³¹ reuses
/// of its slot.)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SlabKey {
    index: u32,
    generation: u32,
}

impl SlabKey {
    /// The slot this key points to (stable for the value's lifetime).
    #[inline]
    pub fn index(self) -> u32 {
        self.index
    }

    /// The slot generation this key was issued for.
    #[inline]
    pub fn generation(self) -> u32 {
        self.generation
    }
}

/// Fixed-size records in one `Vec`, addressed by [`SlabKey`]s, with a free
/// list so removed slots are reused.
///
/// * **No allocation in steady state.** [`Slab::with_capacity`] reserves
///   room for `n` values and for the free list up front; insert and remove
///   allocate only when more than `capacity` values are live at once (the
///   slab then grows like a `Vec`, amortized O(1)).
/// * **Stable keys.** A value never moves while it is live, so its key (and
///   its raw [`index`](SlabKey::index), for internal links such as a
///   linked list threaded through the slab) stays valid until it is removed.
/// * **Stale keys are detected** by the generation check (see [`SlabKey`]).
///
/// Values are `Copy` plain-data records (order nodes, price levels): a freed
/// slot keeps its stale value until reused, so nothing is dropped or moved
/// on removal. Every operation is O(1).
#[derive(Clone, Debug)]
pub struct Slab<T: Copy> {
    // TODO(w27): design this type's fields.
    _todo: std::marker::PhantomData<(T,)>,
}

#[allow(unused_variables)]
impl<T: Copy> Slab<T> {
    /// An empty slab with room for `capacity` values (allocates once, now).
    pub fn with_capacity(capacity: usize) -> Slab<T> {
        todo!("w27: with_capacity")
    }

    /// Stores `value` and returns its key. O(1); allocates only if more
    /// than [`capacity`](Slab::capacity) values would be live.
    ///
    /// # Panics
    /// If the slab would hold `u32::MAX` slots.
    #[inline]
    pub fn insert(&mut self, value: T) -> SlabKey {
        todo!("w27: insert")
    }

    /// Removes and returns the value for `key`, or `None` if `key` is stale
    /// (already removed, or its slot was reused). O(1), never allocates.
    #[inline]
    pub fn remove(&mut self, key: SlabKey) -> Option<T> {
        todo!("w27: remove")
    }

    /// The value for `key`, or `None` if `key` is stale. O(1).
    #[inline]
    pub fn get(&self, key: SlabKey) -> Option<&T> {
        todo!("w27: get")
    }

    /// Mutable access to the value for `key`, or `None` if `key` is stale.
    #[inline]
    pub fn get_mut(&mut self, key: SlabKey) -> Option<&mut T> {
        todo!("w27: get_mut")
    }

    /// True if `key` refers to a live value. O(1).
    #[inline]
    pub fn contains(&self, key: SlabKey) -> bool {
        todo!("w27: contains")
    }

    /// The key of the live value in slot `index`, if any — to turn an
    /// internal link back into a handle. O(1).
    #[inline]
    pub fn key_at(&self, index: u32) -> Option<SlabKey> {
        todo!("w27: key_at")
    }

    /// The value in slot `index` **without a generation check** — for
    /// internal links that are live by construction (a level's FIFO only
    /// links live nodes). Bounds-checked.
    #[inline]
    pub fn at(&self, index: u32) -> &T {
        todo!("w27: at")
    }

    /// Mutable [`at`](Slab::at).
    #[inline]
    pub fn at_mut(&mut self, index: u32) -> &mut T {
        todo!("w27: at_mut")
    }

    /// Number of live values. O(1).
    #[inline]
    pub fn len(&self) -> usize {
        todo!("w27: len")
    }

    /// True if no value is live.
    #[inline]
    pub fn is_empty(&self) -> bool {
        todo!("w27: is_empty")
    }

    /// How many values fit before the slab must grow.
    pub fn capacity(&self) -> usize {
        todo!("w27: capacity")
    }

    /// Removes every value, keeping the allocation. Every outstanding key
    /// becomes stale. O(slots).
    pub fn clear(&mut self) {
        todo!("w27: clear")
    }
}
