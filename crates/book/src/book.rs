//! The w06 order book.

#[allow(unused_imports)] // used by the reference internals
use crate::IdMap;
use crate::{BookError, LevelInfo, OrderBook, RestingOrder};
#[allow(unused_imports)] // used by the reference internals
use std::collections::{btree_map, hash_map, BTreeMap};
use types::{OrderId, Price, Qty, Side};

// @student-begin w06
/// "No index" sentinel for the intrusive links.
const NIL: u32 = u32::MAX;

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
/// after removal, so a warmed-up book never allocates per order.
#[derive(Clone, Debug)]
struct Arena<T> {
    items: Vec<T>,
    free: Vec<u32>,
}

impl<T> Arena<T> {
    fn with_capacity(n: usize) -> Self {
        Arena {
            items: Vec::with_capacity(n),
            free: Vec::new(),
        }
    }

    #[inline]
    fn insert(&mut self, item: T) -> u32 {
        match self.free.pop() {
            Some(i) => {
                self.items[i as usize] = item;
                i
            }
            None => {
                let i = u32::try_from(self.items.len()).expect("arena full");
                assert!(i != NIL, "arena full");
                self.items.push(item);
                i
            }
        }
    }

    /// Marks slot `i` free. The stale value stays until the slot is reused.
    #[inline]
    fn remove(&mut self, i: u32) {
        self.free.push(i);
    }

    fn clear(&mut self) {
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
fn side_ix(side: Side) -> usize {
    match side {
        Side::Buy => 0,
        Side::Sell => 1,
    }
}

/// True if `a` has better priority than `b` on `side`.
#[inline]
fn better(side: Side, a: Price, b: Price) -> bool {
    match side {
        Side::Buy => a > b,
        Side::Sell => a < b,
    }
}

/// Finds the level for `price`, creating it (and updating the cached best)
/// if needed. Takes the fields it needs so the caller can keep a borrow of
/// the id index alive.
#[inline]
fn level_for(
    map: &mut BTreeMap<Price, u32>,
    levels: &mut Arena<Level>,
    best: &mut u32,
    side: Side,
    price: Price,
) -> u32 {
    // Fast path: most adds join the touch.
    if *best != NIL && levels[*best].price == price {
        return *best;
    }
    match map.entry(price) {
        btree_map::Entry::Occupied(e) => *e.get(),
        btree_map::Entry::Vacant(e) => {
            let li = levels.insert(Level {
                price,
                total: Qty::ZERO,
                count: 0,
                head: NIL,
                tail: NIL,
                side,
            });
            e.insert(li);
            if *best == NIL || better(side, price, levels[*best].price) {
                *best = li;
            }
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

    /// Removes an empty level; if it was the best, the next best comes from
    /// the tree (the only O(log L) step on the cancel / fill paths).
    #[cold]
    fn remove_level(&mut self, li: u32) {
        let Level { price, side, .. } = self.levels[li];
        let s = side_ix(side);
        self.prices[s].remove(&price);
        self.levels.remove(li);
        if self.best[s] == li {
            let next = match side {
                Side::Buy => self.prices[s].last_key_value(),
                Side::Sell => self.prices[s].first_key_value(),
            };
            self.best[s] = next.map_or(NIL, |(_, &l)| l);
        }
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
// @student w06
#[derive(Clone, Debug)]
pub struct Book {
    // Representation and why:
    //
    // * Price levels: one `BTreeMap<Price, level index>` per side. Prices
    //   are sparse and unbounded (a ladder array needs a fixed price window;
    //   that is w28), levels come and go far less often than orders, and
    //   ordered iteration gives depth and FOK walks for free. Opening or
    //   removing a level is O(log L).
    // * Orders: nodes in a Vec-backed arena with a free list, linked into an
    //   intrusive doubly linked FIFO per level (`prev`/`next` indices in the
    //   node). Append, pop-front and unlink-from-the-middle are all O(1), and
    //   slots are reused, so a warmed-up book does not allocate per order.
    //   (A `VecDeque` per level with tombstones also cancels in O(1), but
    //   leaves dead entries for the matcher to skip and grows under
    //   cancel-heavy flow.)
    // * Order index: `OrderId -> node` in a hash map with a one-multiply
    //   integer hasher (`IdMap`), so cancel and reduce never search a level.
    // * Cached best level per side: best bid/ask and the matching front are
    //   O(1); the tree is consulted only when the best level empties.
    //
    // | operation                | cost                                   |
    // | add                      | O(1) at an existing level, O(log L) new |
    // | cancel                   | O(1), O(log L) if it empties its level  |
    // | reduce, get, best, front | O(1)                                    |
    // | fill_front               | O(1), O(log L) if it empties the best   |
    // | level                    | O(log L)                                |
    nodes: Arena<Node>,
    levels: Arena<Level>,
    /// `prices[0]` bids, `prices[1]` asks: price → level index.
    prices: [BTreeMap<Price, u32>; 2],
    /// Cached best level index per side (`NIL` when the side is empty).
    best: [u32; 2],
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
            prices: [BTreeMap::new(), BTreeMap::new()],
            best: [NIL, NIL],
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
            &mut self.prices[s],
            &mut self.levels,
            &mut self.best[s],
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
        let li = self.best[side_ix(side)];
        (li != NIL).then(|| self.level_info(li))
    }

    fn level(&self, side: Side, price: Price) -> Option<LevelInfo> {
        self.prices[side_ix(side)]
            .get(&price)
            .map(|&li| self.level_info(li))
    }

    #[inline]
    fn front(&self, side: Side) -> Option<RestingOrder> {
        let li = self.best[side_ix(side)];
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
        let li = self.best[side_ix(side)];
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
        let map = &self.prices[side_ix(side)];
        match side {
            Side::Buy => {
                for &li in map.values().rev() {
                    if !f(self.level_info(li)) {
                        return;
                    }
                }
            }
            Side::Sell => {
                for &li in map.values() {
                    if !f(self.level_info(li)) {
                        return;
                    }
                }
            }
        }
    }

    fn visit_level(&self, side: Side, price: Price, f: &mut dyn FnMut(RestingOrder) -> bool) {
        if let Some(&li) = self.prices[side_ix(side)].get(&price) {
            self.walk_level(li, f);
        }
    }

    fn visit_orders(&self, side: Side, f: &mut dyn FnMut(RestingOrder) -> bool) {
        let map = &self.prices[side_ix(side)];
        match side {
            Side::Buy => {
                for &li in map.values().rev() {
                    if !self.walk_level(li, f) {
                        return;
                    }
                }
            }
            Side::Sell => {
                for &li in map.values() {
                    if !self.walk_level(li, f) {
                        return;
                    }
                }
            }
        }
    }

    #[inline]
    fn len(&self) -> usize {
        self.index.len()
    }

    #[inline]
    fn level_count(&self, side: Side) -> usize {
        self.prices[side_ix(side)].len()
    }

    fn clear(&mut self) {
        self.nodes.clear();
        self.levels.clear();
        self.prices[0].clear();
        self.prices[1].clear();
        self.best = [NIL, NIL];
        self.index.clear();
    }
}
