//! The w06 order book.

#[allow(unused_imports)] // used by the reference internals
use crate::IdMap;
use crate::{BookError, LevelInfo, OrderBook, RestingOrder};
#[allow(unused_imports)] // used by the reference internals
use std::collections::hash_map;
use types::{OrderId, Price, Qty, Side};

// @student-begin w06
/// "No index" sentinel for the intrusive links.
pub(crate) const NIL: u32 = u32::MAX;

/// One resting order: a node in its level's intrusive, doubly linked FIFO.
/// 32 bytes, so two nodes share a cache line.
#[derive(Clone, Copy, Debug)]
struct Node {
    id: OrderId,
    qty: Qty,
    prev: u32,
    next: u32,
    level: u32,
}

/// One price level: aggregates plus the head (oldest) and tail (newest) of
/// its FIFO.
#[derive(Clone, Copy, Debug)]
struct Level {
    price: Price,
    total: Qty,
    count: u32,
    head: u32,
    tail: u32,
    side: Side,
}

/// Vec-backed storage with a free list: stable `u32` handles, slots reused
/// after removal, so a warmed-up book never allocates per order. The free
/// list is reserved alongside the items, so freeing never allocates either.
///
/// Shared with the w28 [`LadderBook`](crate::LadderBook).
#[derive(Clone, Debug)]
pub(crate) struct Arena<T> {
    items: Vec<T>,
    free: Vec<u32>,
}

impl<T> Arena<T> {
    pub(crate) fn with_capacity(n: usize) -> Self {
        Arena {
            items: Vec::with_capacity(n),
            free: Vec::with_capacity(n),
        }
    }

    #[inline]
    pub(crate) fn insert(&mut self, item: T) -> u32 {
        match self.free.pop() {
            Some(i) => {
                self.items[i as usize] = item;
                i
            }
            None => {
                let i = u32::try_from(self.items.len()).expect("arena full");
                assert!(i != NIL, "arena full");
                self.items.push(item);
                // Room to free every slot without growing `free`.
                if self.free.capacity() < self.items.len() {
                    self.free.reserve(self.items.capacity() - self.free.len());
                }
                i
            }
        }
    }

    /// Marks slot `i` free. The stale value stays until the slot is reused.
    #[inline]
    pub(crate) fn remove(&mut self, i: u32) {
        self.free.push(i);
    }

    pub(crate) fn clear(&mut self) {
        self.items.clear();
        self.free.clear();
    }
}

impl<T> std::ops::Index<u32> for Arena<T> {
    type Output = T;
    #[inline]
    fn index(&self, i: u32) -> &T {
        &self.items[i as usize]
    }
}

impl<T> std::ops::IndexMut<u32> for Arena<T> {
    #[inline]
    fn index_mut(&mut self, i: u32) -> &mut T {
        &mut self.items[i as usize]
    }
}

#[inline]
pub(crate) fn side_ix(side: Side) -> usize {
    match side {
        Side::Buy => 0,
        Side::Sell => 1,
    }
}

/// One side's price levels as a sorted vector with the **best price last**.
///
/// `keys` are prices mapped so that "better" means "larger" (bids: the
/// price; asks: its bitwise complement `!p`, which reverses the order and
/// cannot overflow), sorted ascending, so the best level is always at the
/// back: matching removes it with a `pop`, and a new level near the touch
/// is inserted with a short memmove. `levels[i]` is the arena index of the
/// level at `keys[i]`. Keeping the keys in their own dense `Vec<i64>`
/// (struct of arrays) makes the binary search touch as few cache lines as
/// possible, and a warmed-up index never allocates (a `BTreeMap` allocates
/// and frees nodes as levels come and go).
///
/// Shared with the w27 [`SlabBook`](crate::SlabBook).
#[derive(Clone, Debug, Default)]
pub(crate) struct PriceIndex {
    pub(crate) keys: Vec<i64>,
    pub(crate) levels: Vec<u32>,
}

impl PriceIndex {
    pub(crate) fn with_capacity(n: usize) -> PriceIndex {
        PriceIndex {
            keys: Vec::with_capacity(n),
            levels: Vec::with_capacity(n),
        }
    }

    /// The sort key of `price` on `side` (better = larger).
    #[inline]
    pub(crate) fn key(side: Side, price: Price) -> i64 {
        match side {
            Side::Buy => price.0,
            Side::Sell => !price.0,
        }
    }

    /// Arena index of the best level, or `NIL`.
    #[inline]
    pub(crate) fn best(&self) -> u32 {
        self.levels.last().copied().unwrap_or(NIL)
    }

    /// Position of `key`, or where it would be inserted. The touch is
    /// checked first: most lookups (adds joining the best level, the
    /// matcher emptying it) are there.
    #[inline]
    pub(crate) fn find(&self, key: i64) -> Result<usize, usize> {
        match self.keys.last() {
            Some(&last) if last == key => Ok(self.keys.len() - 1),
            Some(&last) if last < key => Err(self.keys.len()),
            _ => self.keys.binary_search(&key),
        }
    }

    /// Arena index of the level at `price`, if it exists. O(log L).
    #[inline]
    pub(crate) fn get(&self, side: Side, price: Price) -> Option<u32> {
        self.find(Self::key(side, price))
            .ok()
            .map(|i| self.levels[i])
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.keys.len()
    }

    /// Inserts level `li` with sort key `key` at position `at` (from
    /// [`find`](Self::find)). O(levels better than it).
    #[inline]
    pub(crate) fn insert(&mut self, at: usize, key: i64, li: u32) {
        self.keys.insert(at, key);
        self.levels.insert(at, li);
    }

    /// Removes the level with sort key `key`, which must be indexed.
    /// O(1) for the best level, else O(log L + levels better than it).
    #[inline]
    pub(crate) fn remove(&mut self, key: i64) {
        let i = self.find(key).expect("a live level is indexed");
        self.keys.remove(i);
        self.levels.remove(i);
    }

    pub(crate) fn clear(&mut self) {
        self.keys.clear();
        self.levels.clear();
    }
}

/// Finds the level for `price`, creating it if needed. Takes the fields it
/// needs so the caller can keep a borrow of the id index alive.
#[inline]
fn level_for(index: &mut PriceIndex, levels: &mut Arena<Level>, side: Side, price: Price) -> u32 {
    let key = PriceIndex::key(side, price);
    match index.find(key) {
        Ok(i) => index.levels[i],
        Err(i) => {
            let li = levels.insert(Level {
                price,
                total: Qty::ZERO,
                count: 0,
                head: NIL,
                tail: NIL,
                side,
            });
            index.insert(i, key, li);
            li
        }
    }
}

impl Book {
    #[inline]
    fn order_at(&self, ni: u32) -> RestingOrder {
        let n = &self.nodes[ni];
        let l = &self.levels[n.level];
        RestingOrder {
            id: n.id,
            side: l.side,
            price: l.price,
            qty: n.qty,
        }
    }

    #[inline]
    fn level_info(&self, li: u32) -> LevelInfo {
        let l = &self.levels[li];
        LevelInfo {
            price: l.price,
            qty: l.total,
            orders: l.count,
        }
    }

    /// Unlinks node `ni` from its level's FIFO, frees it, and removes the
    /// level if it is now empty. The caller has already removed the id from
    /// the index.
    #[inline]
    fn unlink(&mut self, ni: u32) {
        let Node {
            prev,
            next,
            level: li,
            qty,
            ..
        } = self.nodes[ni];
        if prev == NIL {
            self.levels[li].head = next;
        } else {
            self.nodes[prev].next = next;
        }
        if next == NIL {
            self.levels[li].tail = prev;
        } else {
            self.nodes[next].prev = prev;
        }
        self.nodes.remove(ni);
        let level = &mut self.levels[li];
        level.total -= qty;
        level.count -= 1;
        if level.count == 0 {
            self.remove_level(li);
        }
    }

    /// Removes an empty level from its side's index and frees it. The best
    /// level (the common case: the matcher emptied it) is a `pop`; any
    /// other is a binary search plus a memmove of the better levels.
    #[inline]
    fn remove_level(&mut self, li: u32) {
        let Level { price, side, .. } = self.levels[li];
        self.sides[side_ix(side)].remove(PriceIndex::key(side, price));
        self.levels.remove(li);
    }

    /// Walks the FIFO of level `li`; returns false if `f` stopped early.
    #[inline]
    fn walk_level(&self, li: u32, f: &mut dyn FnMut(RestingOrder) -> bool) -> bool {
        let level = &self.levels[li];
        let mut ni = level.head;
        while ni != NIL {
            let n = &self.nodes[ni];
            if !f(RestingOrder {
                id: n.id,
                side: level.side,
                price: level.price,
                qty: n.qty,
            }) {
                return false;
            }
            ni = n.next;
        }
        true
    }
}
// @student-end

/// The week-6 limit order book for one instrument: the reference
/// implementation of [`OrderBook`].
///
/// What the w06 tests and benchmarks hold it to:
///
/// * cancel, reduce and lookup by id in **O(1)** — an id → location index,
///   and per-level FIFOs that can drop an order from the middle without a
///   scan (emptying a level may cost O(log L) more, L = levels on a side);
/// * add in O(log L) or better; best price and the matching front in O(1)
///   or O(log L);
/// * top-N depth and visits in O(log L + visited), best first;
/// * no allocation per operation once warmed up, beyond opening a new
///   price level.
///
/// Since w26 (profiling) it also never allocates once warmed up, not even
/// to open a level: the w26 tests hold the engine to that.
// @student w06
#[derive(Clone, Debug)]
pub struct Book {
    // Representation and why:
    //
    // * Price levels: one sorted vector of prices per side with the best
    //   price at the back (`PriceIndex`). Levels near the touch, where
    //   nearly all activity is, are found with one compare and opened or
    //   closed with a short memmove; any other level is a binary search
    //   over a dense `Vec<i64>`. Ordered iteration (depth, FOK walks) is a
    //   reverse slice walk.
    //   The first version used a `BTreeMap<Price, level>` per side. A
    //   profile of the w07 flow (w26) put ~20% of engine time in the
    //   B-tree search on every add away from the touch, and ~9% in level
    //   removal; the tree also allocated and freed nodes as levels came and
    //   went, so the engine was never allocation-free. A sorted vector
    //   fixes all three, at the price of an O(L) worst case to open or
    //   close a level far from the touch (rare, and still a memmove).
    // * Orders: nodes in a Vec-backed arena with a free list, linked into an
    //   intrusive doubly linked FIFO per level (`prev`/`next` indices in the
    //   node). Append, pop-front and unlink-from-the-middle are all O(1), and
    //   slots are reused, so a warmed-up book does not allocate per order.
    //   (A `VecDeque` per level with tombstones also cancels in O(1), but
    //   leaves dead entries for the matcher to skip and grows under
    //   cancel-heavy flow.)
    // * Order index: `OrderId -> node` in a hash map with a one-multiply
    //   integer hasher (`IdMap`), so cancel and reduce never search a level.
    //
    // | operation                | cost (d = levels between it and the touch) |
    // | add                      | O(1) at the best, O(log L) elsewhere; +O(d) to open a level |
    // | cancel                   | O(1); +O(log L + d) if it empties its level |
    // | reduce, get, best, front | O(1)                                    |
    // | fill_front               | O(1) (emptying the best level is a pop) |
    // | level                    | O(log L)                                |
    nodes: Arena<Node>,
    levels: Arena<Level>,
    /// `sides[0]` bids, `sides[1]` asks: sorted price → level index.
    sides: [PriceIndex; 2],
    index: IdMap<u32>,
}

// @student w06
impl Book {
    /// An empty book. Allocates nothing until the first order.
    pub fn new() -> Book {
        Book::with_capacity(0, 0)
    }

    /// An empty book with room for `orders` resting orders and `levels`
    /// price levels before any reallocation.
    pub fn with_capacity(orders: usize, levels: usize) -> Book {
        Book {
            nodes: Arena::with_capacity(orders),
            levels: Arena::with_capacity(levels),
            sides: [
                PriceIndex::with_capacity(levels),
                PriceIndex::with_capacity(levels),
            ],
            index: IdMap::with_capacity_and_hasher(orders, Default::default()),
        }
    }
}

impl Default for Book {
    fn default() -> Book {
        Book::new()
    }
}

// @student w06
impl OrderBook for Book {
    #[inline]
    fn add(&mut self, order: RestingOrder) -> Result<(), BookError> {
        if order.qty.is_zero() {
            return Err(BookError::ZeroQty);
        }
        // One hash probe: the vacant entry is filled once the node exists.
        let slot = match self.index.entry(order.id) {
            hash_map::Entry::Occupied(_) => return Err(BookError::DuplicateId),
            hash_map::Entry::Vacant(v) => v,
        };
        let s = side_ix(order.side);
        let li = level_for(
            &mut self.sides[s],
            &mut self.levels,
            order.side,
            order.price,
        );
        let level = &mut self.levels[li];
        let tail = level.tail;
        let ni = self.nodes.insert(Node {
            id: order.id,
            qty: order.qty,
            prev: tail,
            next: NIL,
            level: li,
        });
        if tail == NIL {
            level.head = ni;
        } else {
            self.nodes[tail].next = ni;
        }
        level.tail = ni;
        level.total += order.qty;
        level.count += 1;
        slot.insert(ni);
        Ok(())
    }

    #[inline]
    fn cancel(&mut self, id: OrderId) -> Result<RestingOrder, BookError> {
        let ni = self.index.remove(&id).ok_or(BookError::UnknownOrder)?;
        let order = self.order_at(ni);
        self.unlink(ni);
        Ok(order)
    }

    #[inline]
    fn reduce(&mut self, id: OrderId, qty: Qty) -> Result<RestingOrder, BookError> {
        let ni = *self.index.get(&id).ok_or(BookError::UnknownOrder)?;
        if qty.is_zero() {
            return Err(BookError::ZeroQty);
        }
        let node = &mut self.nodes[ni];
        if qty >= node.qty {
            return Err(BookError::InvalidReduce);
        }
        let delta = node.qty - qty;
        node.qty = qty;
        let li = node.level;
        self.levels[li].total -= delta;
        Ok(self.order_at(ni))
    }

    #[inline]
    fn get(&self, id: OrderId) -> Option<RestingOrder> {
        self.index.get(&id).map(|&ni| self.order_at(ni))
    }

    #[inline]
    fn contains(&self, id: OrderId) -> bool {
        self.index.contains_key(&id)
    }

    #[inline]
    fn best(&self, side: Side) -> Option<LevelInfo> {
        let li = self.sides[side_ix(side)].best();
        (li != NIL).then(|| self.level_info(li))
    }

    fn level(&self, side: Side, price: Price) -> Option<LevelInfo> {
        self.sides[side_ix(side)]
            .get(side, price)
            .map(|li| self.level_info(li))
    }

    #[inline]
    fn front(&self, side: Side) -> Option<RestingOrder> {
        let li = self.sides[side_ix(side)].best();
        if li == NIL {
            return None;
        }
        let level = &self.levels[li];
        let n = &self.nodes[level.head];
        Some(RestingOrder {
            id: n.id,
            side,
            price: level.price,
            qty: n.qty,
        })
    }

    #[inline]
    fn fill_front(&mut self, side: Side, qty: Qty) -> Qty {
        let li = self.sides[side_ix(side)].best();
        assert!(li != NIL, "fill_front: no {side:?} orders");
        let ni = self.levels[li].head;
        let node = &mut self.nodes[ni];
        assert!(
            !qty.is_zero() && qty <= node.qty,
            "fill_front: qty {qty} not in 1..={}",
            node.qty
        );
        node.qty -= qty;
        let remaining = node.qty;
        if remaining.is_zero() {
            let id = node.id;
            self.index.remove(&id);
            // `unlink` subtracts the node's (now zero) qty; account the fill here.
            self.levels[li].total -= qty;
            self.unlink(ni);
        } else {
            self.levels[li].total -= qty;
        }
        remaining
    }

    fn visit_levels(&self, side: Side, f: &mut dyn FnMut(LevelInfo) -> bool) {
        // Best first = back to front.
        for &li in self.sides[side_ix(side)].levels.iter().rev() {
            if !f(self.level_info(li)) {
                return;
            }
        }
    }

    fn visit_level(&self, side: Side, price: Price, f: &mut dyn FnMut(RestingOrder) -> bool) {
        if let Some(li) = self.sides[side_ix(side)].get(side, price) {
            self.walk_level(li, f);
        }
    }

    fn visit_orders(&self, side: Side, f: &mut dyn FnMut(RestingOrder) -> bool) {
        for &li in self.sides[side_ix(side)].levels.iter().rev() {
            if !self.walk_level(li, f) {
                return;
            }
        }
    }

    #[inline]
    fn len(&self) -> usize {
        self.index.len()
    }

    #[inline]
    fn level_count(&self, side: Side) -> usize {
        self.sides[side_ix(side)].len()
    }

    fn clear(&mut self) {
        self.nodes.clear();
        self.levels.clear();
        self.sides[0].clear();
        self.sides[1].clear();
        self.index.clear();
    }
}
