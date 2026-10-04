//! Week 27: a generational slab — preallocated storage with stable keys.

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
// @student w27
#[derive(Clone, Debug)]
pub struct Slab<T: Copy> {
    values: Vec<T>,
    /// Per slot: odd = occupied, even = vacant. Bumped on insert and on
    /// remove, so a slot's generation changes every time it is reused.
    generations: Vec<u32>,
    /// Vacant slot indices (a stack: the most recently freed slot, still
    /// warm in cache, is reused first).
    free: Vec<u32>,
}

// @student w27
impl<T: Copy> Slab<T> {
    /// An empty slab with room for `capacity` values (allocates once, now).
    pub fn with_capacity(capacity: usize) -> Slab<T> {
        Slab {
            values: Vec::with_capacity(capacity),
            generations: Vec::with_capacity(capacity),
            free: Vec::with_capacity(capacity),
        }
    }

    /// Stores `value` and returns its key. O(1); allocates only if more
    /// than [`capacity`](Slab::capacity) values would be live.
    ///
    /// # Panics
    /// If the slab would hold `u32::MAX` slots.
    #[inline]
    pub fn insert(&mut self, value: T) -> SlabKey {
        match self.free.pop() {
            Some(index) => {
                let i = index as usize;
                self.values[i] = value;
                self.generations[i] = self.generations[i].wrapping_add(1);
                SlabKey {
                    index,
                    generation: self.generations[i],
                }
            }
            None => {
                let index = u32::try_from(self.values.len())
                    .ok()
                    .filter(|&i| i != u32::MAX)
                    .expect("slab full");
                self.values.push(value);
                self.generations.push(1);
                // Keep room to free every slot without growing `free`.
                if self.free.capacity() < self.values.len() {
                    self.free.reserve(self.values.capacity() - self.free.len());
                }
                SlabKey {
                    index,
                    generation: 1,
                }
            }
        }
    }

    /// Removes and returns the value for `key`, or `None` if `key` is stale
    /// (already removed, or its slot was reused). O(1), never allocates.
    #[inline]
    pub fn remove(&mut self, key: SlabKey) -> Option<T> {
        let i = key.index as usize;
        if self.generations.get(i) != Some(&key.generation) || key.generation & 1 == 0 {
            return None;
        }
        self.generations[i] = key.generation.wrapping_add(1);
        self.free.push(key.index);
        Some(self.values[i])
    }

    /// The value for `key`, or `None` if `key` is stale. O(1).
    #[inline]
    pub fn get(&self, key: SlabKey) -> Option<&T> {
        let i = key.index as usize;
        (key.generation & 1 == 1 && self.generations.get(i) == Some(&key.generation))
            .then(|| &self.values[i])
    }

    /// Mutable access to the value for `key`, or `None` if `key` is stale.
    #[inline]
    pub fn get_mut(&mut self, key: SlabKey) -> Option<&mut T> {
        let i = key.index as usize;
        if key.generation & 1 == 1 && self.generations.get(i) == Some(&key.generation) {
            Some(&mut self.values[i])
        } else {
            None
        }
    }

    /// True if `key` refers to a live value. O(1).
    #[inline]
    pub fn contains(&self, key: SlabKey) -> bool {
        self.get(key).is_some()
    }

    /// The key of the live value in slot `index`, if any — to turn an
    /// internal link back into a handle. O(1).
    #[inline]
    pub fn key_at(&self, index: u32) -> Option<SlabKey> {
        let generation = *self.generations.get(index as usize)?;
        (generation & 1 == 1).then_some(SlabKey { index, generation })
    }

    /// The value in slot `index` **without a generation check** — for
    /// internal links that are live by construction (a level's FIFO only
    /// links live nodes). Bounds-checked.
    #[inline]
    pub fn at(&self, index: u32) -> &T {
        &self.values[index as usize]
    }

    /// Mutable [`at`](Slab::at).
    #[inline]
    pub fn at_mut(&mut self, index: u32) -> &mut T {
        &mut self.values[index as usize]
    }

    /// Number of live values. O(1).
    #[inline]
    pub fn len(&self) -> usize {
        self.values.len() - self.free.len()
    }

    /// True if no value is live.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many values fit before the slab must grow.
    pub fn capacity(&self) -> usize {
        self.values.capacity()
    }

    /// Removes every value, keeping the allocation. Every outstanding key
    /// becomes stale. O(slots).
    pub fn clear(&mut self) {
        self.free.clear();
        for (i, generation) in self.generations.iter_mut().enumerate().rev() {
            if *generation & 1 == 1 {
                *generation = generation.wrapping_add(1);
            }
            self.free.push(i as u32);
        }
    }
}
