//! Week 28: the ladder book — a dense array of price levels around the
//! touch, with a sparse fallback for far prices.

#[allow(unused_imports)] // used by the reference internals
use crate::IdMap;
use crate::{BookError, LevelInfo, OrderBook, RestingOrder};
#[allow(unused_imports)] // used by the reference internals
use std::collections::{hash_map, BTreeMap};
use types::{OrderId, Price, Qty, Side};

// @student-begin w28
use crate::book::{side_ix, Arena, NIL};

/// One resting order: a node in its level's intrusive FIFO. 32 bytes.
#[derive(Clone, Copy, Debug)]
struct Node {
    id: OrderId,
    qty: Qty,
    prev: u32,
    next: u32,
    level: u32,
}

/// One price level: aggregates plus the head (oldest) and tail of its FIFO.
/// 32 bytes.
#[derive(Clone, Copy, Debug)]
struct Level {
    price: Price,
    total: Qty,
    count: u32,
    head: u32,
    tail: u32,
    side: Side,
}

/// True if `a` has better priority than `b` on `side`.
#[inline]
fn better(side: Side, a: i64, b: i64) -> bool {
    match side {
        Side::Buy => a > b,
        Side::Sell => a < b,
    }
}

/// Highest set bit with index `< end`, if any.
#[inline]
fn highest_below(bits: &[u64], end: usize) -> Option<usize> {
    if end == 0 {
        return None;
    }
    let mut w = (end - 1) / 64;
    let r = (end - 1) % 64;
    let mut word = bits[w] & (u64::MAX >> (63 - r));
    loop {
        if word != 0 {
            return Some(w * 64 + 63 - word.leading_zeros() as usize);
        }
        if w == 0 {
            return None;
        }
        w -= 1;
        word = bits[w];
    }
}

/// Lowest set bit with index `>= start`, if any.
#[inline]
fn lowest_from(bits: &[u64], start: usize) -> Option<usize> {
    let mut w = start / 64;
    if w >= bits.len() {
        return None;
    }
    let mut word = bits[w] & (u64::MAX << (start % 64));
    loop {
        if word != 0 {
            return Some(w * 64 + word.trailing_zeros() as usize);
        }
        w += 1;
        if w >= bits.len() {
            return None;
        }
        word = bits[w];
    }
}

/// The price → level index of one side.
///
/// Prices in the window `[base, base + slots.len())` map straight to a slot
/// (`price - base`): finding, opening and closing a level there is O(1)
/// with no search. `bits` mirrors which slots hold a level, so the next
/// best level after the best one empties is a scan of 64 slots per
/// instruction (`leading_zeros` / `trailing_zeros`). Prices outside the
/// window live in `sparse`. The window is re-centred on the best price
/// whenever the best moves outside it, so the touch — where nearly all
/// activity is — is always dense.
#[derive(Clone, Debug)]
struct Ladder {
    side: Side,
    base: i64,
    slots: Vec<u32>,
    bits: Vec<u64>,
    sparse: BTreeMap<i64, u32>,
    /// Best level (arena index), `NIL` when the side is empty.
    best: u32,
    /// Number of levels on this side.
    count: usize,
}

impl Ladder {
    fn new(side: Side, window: usize) -> Ladder {
        Ladder {
            side,
            base: 0,
            slots: vec![NIL; window],
            bits: vec![0; window / 64],
            sparse: BTreeMap::new(),
            best: NIL,
            count: 0,
        }
    }

    #[inline]
    fn window(&self) -> i64 {
        self.slots.len() as i64
    }

    /// The dense slot of `price`, if it is inside the window.
    #[inline]
    fn slot(&self, price: i64) -> Option<usize> {
        let off = price.checked_sub(self.base)?;
        ((off as u64) < self.slots.len() as u64).then_some(off as usize)
    }

    /// The level at `price`, if any. O(1) in the window, O(log S) outside.
    #[inline]
    fn find(&self, price: i64) -> Option<u32> {
        match self.slot(price) {
            Some(s) => {
                let li = self.slots[s];
                (li != NIL).then_some(li)
            }
            None => self.sparse.get(&price).copied(),
        }
    }

    /// Stores level `li` at `price` in the window or the sparse map.
    #[inline]
    fn place(&mut self, price: i64, li: u32) {
        match self.slot(price) {
            Some(s) => {
                self.slots[s] = li;
                self.bits[s / 64] |= 1 << (s % 64);
            }
            None => {
                self.sparse.insert(price, li);
            }
        }
    }

    /// Indexes a new level `li` at `price`; it becomes the best if it beats
    /// the current best (`best_price`). Re-centres first if the new best is
    /// outside the window.
    #[inline]
    fn open(&mut self, price: i64, li: u32, best_price: Option<i64>) {
        let is_best = best_price.is_none_or(|b| better(self.side, price, b));
        if is_best && self.slot(price).is_none() {
            self.recenter(price);
        }
        self.place(price, li);
        if is_best {
            self.best = li;
        }
        self.count += 1;
    }

    /// Un-indexes the (empty) level `li` at `price`. If it was the best,
    /// finds the next best and returns its price for re-centring.
    #[inline]
    fn close(&mut self, price: i64, li: u32) -> Option<i64> {
        match self.slot(price) {
            Some(s) => {
                self.slots[s] = NIL;
                self.bits[s / 64] &= !(1 << (s % 64));
            }
            None => {
                self.sparse.remove(&price);
            }
        }
        self.count -= 1;
        if self.best != li {
            return None;
        }
        let (li, p) = self.next_best(price);
        self.best = li;
        p
    }

    /// The best remaining level (and its price) after the best level at
    /// `old` closed: every remaining level is worse than `old`, so the
    /// dense scan starts at `old`'s slot; the sparse candidate is the
    /// sparse map's best end.
    fn next_best(&self, old: i64) -> (u32, Option<i64>) {
        let w = self.slots.len();
        let off = i128::from(old) - i128::from(self.base);
        let dense = match self.side {
            Side::Buy => highest_below(&self.bits, off.clamp(0, w as i128) as usize),
            Side::Sell => lowest_from(&self.bits, (off + 1).clamp(0, w as i128) as usize),
        }
        .map(|s| (self.slots[s], self.base + s as i64));
        let sparse = match self.side {
            Side::Buy => self.sparse.last_key_value(),
            Side::Sell => self.sparse.first_key_value(),
        }
        .map(|(&p, &li)| (li, p));
        let best = match (dense, sparse) {
            (Some(d), Some(s)) => Some(if better(self.side, s.1, d.1) { s } else { d }),
            (d, s) => d.or(s),
        };
        match best {
            Some((li, p)) => (li, Some(p)),
            None => (NIL, None),
        }
    }

    /// Moves the window so that `center` is in its middle: dense levels
    /// that fall outside go to the sparse map and sparse levels that fall
    /// inside become dense. O(window / 64 + levels moved × log S); rare
    /// (the touch must move half a window first), and it may allocate
    /// B-tree nodes.
    #[cold]
    fn recenter(&mut self, center: i64) {
        let w = self.window();
        let base = center.saturating_sub(w / 2).min(i64::MAX - w);
        if base == self.base {
            return;
        }
        let mut end = self.slots.len();
        while let Some(s) = highest_below(&self.bits, end) {
            self.sparse.insert(self.base + s as i64, self.slots[s]);
            self.slots[s] = NIL;
            end = s;
        }
        self.bits.fill(0);
        self.base = base;
        let mut inside = self.sparse.split_off(&base);
        let mut above = inside.split_off(&(base + w));
        self.sparse.append(&mut above);
        for (price, li) in inside {
            self.place(price, li);
        }
    }

    /// Calls `f(level)` best first until it returns `false`; returns
    /// whether it ran to the end. Order: (bids) sparse above the window,
    /// the window top-down, sparse below — mirrored for asks.
    fn walk(&self, f: &mut dyn FnMut(u32) -> bool) -> bool {
        let top = self.base + self.window();
        match self.side {
            Side::Buy => {
                for (_, &li) in self.sparse.range(top..).rev() {
                    if !f(li) {
                        return false;
                    }
                }
                let mut end = self.slots.len();
                while let Some(s) = highest_below(&self.bits, end) {
                    if !f(self.slots[s]) {
                        return false;
                    }
                    end = s;
                }
                for (_, &li) in self.sparse.range(..self.base).rev() {
                    if !f(li) {
                        return false;
                    }
                }
            }
            Side::Sell => {
                for (_, &li) in self.sparse.range(..self.base) {
                    if !f(li) {
                        return false;
                    }
                }
                let mut start = 0;
                while let Some(s) = lowest_from(&self.bits, start) {
                    if !f(self.slots[s]) {
                        return false;
                    }
                    start = s + 1;
                }
                for (_, &li) in self.sparse.range(top..) {
                    if !f(li) {
                        return false;
                    }
                }
            }
        }
        true
    }

    fn clear(&mut self) {
        self.slots.fill(NIL);
        self.bits.fill(0);
        self.sparse.clear();
        self.best = NIL;
        self.count = 0;
    }
}

impl LadderBook {
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

    /// Unlinks and frees node `ni` (already removed from the id index),
    /// closing its level if it is now empty.
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
            self.close_level(li);
        }
    }

    /// Removes an empty level from its ladder and frees it.
    #[inline]
    fn close_level(&mut self, li: u32) {
        let Level { price, side, .. } = self.levels[li];
        let ladder = &mut self.sides[side_ix(side)];
        if let Some(next) = ladder.close(price.0, li) {
            if ladder.slot(next).is_none() {
                ladder.recenter(next);
            }
        }
        self.levels.remove(li);
    }

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

/// The level for `price` on `side`, opened if needed. Takes the fields it
/// needs so the caller can keep a borrow of the id index alive.
#[inline]
fn level_for(ladder: &mut Ladder, levels: &mut Arena<Level>, side: Side, price: Price) -> u32 {
    if let Some(li) = ladder.find(price.0) {
        return li;
    }
    let best_price = (ladder.best != NIL).then(|| levels[ladder.best].price.0);
    let li = levels.insert(Level {
        price,
        total: Qty::ZERO,
        count: 0,
        head: NIL,
        tail: NIL,
        side,
    });
    ladder.open(price.0, li, best_price);
    li
}
// @student-end

/// A cache-friendly order book: each side keeps a **dense array of price
/// levels** (a "ladder") over a window of prices around the touch, indexed
/// directly by `price - base`, with a **sparse** ordered map for the rare
/// orders priced outside the window.
///
/// Compared with the w06 [`Book`](crate::Book), whose levels are found by a
/// search (a B-tree, or since w26 a binary search over sorted prices),
/// finding the level of an add is one subtraction and one load, and opening
/// or closing a level never moves other levels. The next best price
/// after the best level empties comes from an occupancy bitmap, 64 prices
/// per word. The window follows the market: when the best price leaves it,
/// it is re-centred on the new best (O(window / 64 + levels moved)).
///
/// Same semantics as `Book` (the w28 tests check them differentially).
/// Costs: add, cancel, reduce, get, best, front, fill — O(1) inside the
/// window (closing the best level adds a bitmap scan to the next level);
/// O(log S) for the S sparse levels outside it. Memory: `4 × window` bytes
/// per side plus the bitmap. A warmed-up book allocates only when the
/// window moves or a sparse level opens.
///
/// Prices are in the book's price units: a window of 2048 covers 2048
/// consecutive prices (with a tick size of 5, every fifth slot is usable).
// @student w28
#[derive(Clone, Debug)]
pub struct LadderBook {
    // * `sides`: one `Ladder` per side — `slots[price - base]` holds the
    //   level's arena index (`NIL` when empty), `bits` mirrors occupancy,
    //   `sparse` holds levels outside the window, `best` caches the best
    //   level. Level data lives in the `levels` slab (not inline in the
    //   window) so that moving the window moves 4-byte indices, and nodes
    //   can keep a stable level link.
    // * `nodes` / `levels`: Vec arenas with free lists (as in `Book`); no
    //   generations — this book hands out no keys, and the generation
    //   array would be one more cache line per random cancel.
    // * `index`: OrderId → node, as in `Book`.
    nodes: Arena<Node>,
    levels: Arena<Level>,
    sides: [Ladder; 2],
    index: IdMap<u32>,
}

// @student w28
impl LadderBook {
    /// An empty ladder book whose dense window covers `window` prices per
    /// side (rounded up to a multiple of 64, at least 64), with room for
    /// `orders` resting orders. Allocates the windows and the order storage
    /// now. The window starts wherever the first order on each side lands.
    pub fn new(window: usize, orders: usize) -> LadderBook {
        let window = window.max(64).div_ceil(64) * 64;
        LadderBook {
            nodes: Arena::with_capacity(orders),
            levels: Arena::with_capacity(2 * window),
            sides: [
                Ladder::new(Side::Buy, window),
                Ladder::new(Side::Sell, window),
            ],
            // 2×: see `SlabBook::with_capacity` (in-place tombstone rehash).
            index: IdMap::with_capacity_and_hasher(2 * orders, Default::default()),
        }
    }

    /// Number of prices each side's dense window covers.
    pub fn window(&self) -> usize {
        self.sides[0].slots.len()
    }
}

impl Default for LadderBook {
    /// A 2048-price window per side and room for 1024 orders.
    fn default() -> LadderBook {
        LadderBook::new(2048, 1024)
    }
}

// @student w28
impl OrderBook for LadderBook {
    #[inline]
    fn add(&mut self, order: RestingOrder) -> Result<(), BookError> {
        if order.qty.is_zero() {
            return Err(BookError::ZeroQty);
        }
        let slot = match self.index.entry(order.id) {
            hash_map::Entry::Occupied(_) => return Err(BookError::DuplicateId),
            hash_map::Entry::Vacant(v) => v,
        };
        let li = level_for(
            &mut self.sides[side_ix(order.side)],
            &mut self.levels,
            order.side,
            order.price,
        );
        let tail = self.levels[li].tail;
        let ni = self.nodes.insert(Node {
            id: order.id,
            qty: order.qty,
            prev: tail,
            next: NIL,
            level: li,
        });
        if tail == NIL {
            self.levels[li].head = ni;
        } else {
            self.nodes[tail].next = ni;
        }
        let level = &mut self.levels[li];
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
        let li = self.sides[side_ix(side)].best;
        (li != NIL).then(|| self.level_info(li))
    }

    fn level(&self, side: Side, price: Price) -> Option<LevelInfo> {
        self.sides[side_ix(side)]
            .find(price.0)
            .map(|li| self.level_info(li))
    }

    #[inline]
    fn front(&self, side: Side) -> Option<RestingOrder> {
        let li = self.sides[side_ix(side)].best;
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
        let li = self.sides[side_ix(side)].best;
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
        let id = node.id;
        self.levels[li].total -= qty;
        if remaining.is_zero() {
            self.index.remove(&id);
            self.unlink(ni);
        }
        remaining
    }

    fn visit_levels(&self, side: Side, f: &mut dyn FnMut(LevelInfo) -> bool) {
        self.sides[side_ix(side)].walk(&mut |li| f(self.level_info(li)));
    }

    fn visit_level(&self, side: Side, price: Price, f: &mut dyn FnMut(RestingOrder) -> bool) {
        if let Some(li) = self.sides[side_ix(side)].find(price.0) {
            self.walk_level(li, f);
        }
    }

    fn visit_orders(&self, side: Side, f: &mut dyn FnMut(RestingOrder) -> bool) {
        self.sides[side_ix(side)].walk(&mut |li| self.walk_level(li, f));
    }

    #[inline]
    fn len(&self) -> usize {
        self.index.len()
    }

    #[inline]
    fn level_count(&self, side: Side) -> usize {
        self.sides[side_ix(side)].count
    }

    fn clear(&mut self) {
        self.nodes.clear();
        self.levels.clear();
        self.sides[0].clear();
        self.sides[1].clear();
        self.index.clear();
    }
}
