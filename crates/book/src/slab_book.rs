//! Week 27: the slab-backed order book.

use crate::slab::SlabKey;
#[allow(unused_imports)] // used by the reference internals
use crate::IdMap;
use crate::{BookError, LevelInfo, OrderBook, RestingOrder};
#[allow(unused_imports)] // used by the reference internals
use std::collections::hash_map;
use types::{OrderId, Price, Qty, Side};

// @student-begin w27
use crate::book::{side_ix, PriceIndex, NIL};
use crate::slab::Slab;

/// One resting order: a node in its level's intrusive FIFO. Links are raw
/// slot indices (live by construction); generations guard only the keys
/// handed out. 32 bytes.
#[derive(Clone, Copy, Debug)]
struct Node {
    id: OrderId,
    qty: Qty,
    prev: u32,
    next: u32,
    level: u32,
}

/// One price level: aggregates plus the head (oldest) and tail of its FIFO.
#[derive(Clone, Copy, Debug)]
struct Level {
    price: Price,
    total: Qty,
    count: u32,
    head: u32,
    tail: u32,
    side: Side,
}

/// The level for `price` on `side`, opened (from the level slab) if needed.
/// Takes the fields it needs so the caller can keep a borrow of the id
/// index alive.
#[inline]
fn level_for(index: &mut PriceIndex, levels: &mut Slab<Level>, side: Side, price: Price) -> u32 {
    let key = PriceIndex::key(side, price);
    match index.find(key) {
        Ok(i) => index.levels[i],
        Err(i) => {
            let li = levels
                .insert(Level {
                    price,
                    total: Qty::ZERO,
                    count: 0,
                    head: NIL,
                    tail: NIL,
                    side,
                })
                .index();
            index.insert(i, key, li);
            li
        }
    }
}

impl SlabBook {
    #[inline]
    fn order_at(&self, ni: u32) -> RestingOrder {
        let n = self.nodes.at(ni);
        let l = self.levels.at(n.level);
        RestingOrder {
            id: n.id,
            side: l.side,
            price: l.price,
            qty: n.qty,
        }
    }

    #[inline]
    fn level_info(&self, li: u32) -> LevelInfo {
        let l = self.levels.at(li);
        LevelInfo {
            price: l.price,
            qty: l.total,
            orders: l.count,
        }
    }

    /// Unlinks and frees node `key` (already removed from the id index),
    /// closing its level if it is now empty.
    #[inline]
    fn unlink(&mut self, key: SlabKey) {
        let Node {
            prev,
            next,
            level: li,
            qty,
            ..
        } = self.nodes.remove(key).expect("indexed node is live");
        if prev == NIL {
            self.levels.at_mut(li).head = next;
        } else {
            self.nodes.at_mut(prev).next = next;
        }
        if next == NIL {
            self.levels.at_mut(li).tail = prev;
        } else {
            self.nodes.at_mut(next).prev = prev;
        }
        let level = self.levels.at_mut(li);
        level.total -= qty;
        level.count -= 1;
        if level.count == 0 {
            let (price, side) = (level.price, level.side);
            self.sides[side_ix(side)].remove(PriceIndex::key(side, price));
            let lk = self.levels.key_at(li).expect("level is live");
            self.levels.remove(lk);
        }
    }

    #[inline]
    fn walk_level(&self, li: u32, f: &mut dyn FnMut(RestingOrder) -> bool) -> bool {
        let level = self.levels.at(li);
        let mut ni = level.head;
        while ni != NIL {
            let n = self.nodes.at(ni);
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

/// A slab-backed order book: every node, level and index entry lives in
/// storage reserved at construction, so a book sized for its peak never
/// allocates — not on the first order, not when levels open and close.
///
/// It has the same price-time semantics as the w06 [`Book`](crate::Book)
/// (the w27 tests check them differentially through `Box<dyn OrderBook>`),
/// plus **generational handles**: [`SlabBook::key_of`] returns a
/// [`SlabKey`] for a resting order, and [`SlabBook::get_by_key`] /
/// [`SlabBook::cancel_by_key`] use it without hashing the order id. A key
/// kept after its order was filled or cancelled is *stale* — its slot may
/// already hold a newer order — and is rejected, never misapplied.
///
/// Costs are those of `Book`: O(1) cancel / reduce / get / best / front /
/// fill; O(log L) to find a level away from the touch; opening or closing a
/// level is a short memmove near the touch. Allocation: none while at most
/// `orders` orders and `levels` levels per side are live at once; beyond
/// that, the slabs grow like `Vec`s.
// @student w27
#[derive(Clone, Debug)]
pub struct SlabBook {
    // * `nodes` / `levels`: generational slabs (w27 `Slab`). Internal links
    //   (FIFO prev/next, node → level, price index → level) are raw slot
    //   indices: they are live by construction, so they skip the generation
    //   check. Keys given out (`key_of`) carry the generation.
    // * `sides`: the w26 sorted price index, best at the back (reserved for
    //   `levels` per side).
    // * `index`: OrderId → node key, reserved for 2 × `orders` entries
    //   (see `with_capacity`).
    nodes: Slab<Node>,
    levels: Slab<Level>,
    sides: [PriceIndex; 2],
    index: IdMap<SlabKey>,
}

// @student w27
impl SlabBook {
    /// An empty book with storage for `orders` resting orders and `levels`
    /// price levels per side, all allocated now.
    pub fn with_capacity(orders: usize, levels: usize) -> SlabBook {
        SlabBook {
            nodes: Slab::with_capacity(orders),
            levels: Slab::with_capacity(2 * levels),
            sides: [
                PriceIndex::with_capacity(levels),
                PriceIndex::with_capacity(levels),
            ],
            // Twice the live orders: a hashbrown table rehashes its
            // tombstones in place (no allocation) only while it is at most
            // half full, so this keeps a churning steady state allocation-free.
            index: IdMap::with_capacity_and_hasher(2 * orders, Default::default()),
        }
    }

    /// The generational handle of a resting order, or `None`. O(1) (one
    /// hash probe).
    pub fn key_of(&self, id: OrderId) -> Option<SlabKey> {
        self.index.get(&id).copied()
    }

    /// The order behind `key`, or `None` if the key is stale (its order was
    /// filled, cancelled or the book cleared). O(1), no hashing.
    pub fn get_by_key(&self, key: SlabKey) -> Option<RestingOrder> {
        self.nodes.get(key)?;
        Some(self.order_at(key.index()))
    }

    /// Cancels the order behind `key`. Errors: [`BookError::UnknownOrder`]
    /// if the key is stale (nothing changes). O(1).
    pub fn cancel_by_key(&mut self, key: SlabKey) -> Result<RestingOrder, BookError> {
        let order = self.get_by_key(key).ok_or(BookError::UnknownOrder)?;
        self.index.remove(&order.id);
        self.unlink(key);
        Ok(order)
    }

    /// How many resting orders fit before the book allocates.
    pub fn order_capacity(&self) -> usize {
        self.nodes.capacity()
    }
}

impl Default for SlabBook {
    /// A book with room for 1024 orders and 256 levels per side.
    fn default() -> SlabBook {
        SlabBook::with_capacity(1024, 256)
    }
}

// @student w27
impl OrderBook for SlabBook {
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
        let li = level_for(
            &mut self.sides[side_ix(order.side)],
            &mut self.levels,
            order.side,
            order.price,
        );
        let tail = self.levels.at(li).tail;
        let key = self.nodes.insert(Node {
            id: order.id,
            qty: order.qty,
            prev: tail,
            next: NIL,
            level: li,
        });
        let ni = key.index();
        if tail == NIL {
            self.levels.at_mut(li).head = ni;
        } else {
            self.nodes.at_mut(tail).next = ni;
        }
        let level = self.levels.at_mut(li);
        level.tail = ni;
        level.total += order.qty;
        level.count += 1;
        slot.insert(key);
        Ok(())
    }

    #[inline]
    fn cancel(&mut self, id: OrderId) -> Result<RestingOrder, BookError> {
        let key = self.index.remove(&id).ok_or(BookError::UnknownOrder)?;
        let order = self.order_at(key.index());
        self.unlink(key);
        Ok(order)
    }

    #[inline]
    fn reduce(&mut self, id: OrderId, qty: Qty) -> Result<RestingOrder, BookError> {
        let key = *self.index.get(&id).ok_or(BookError::UnknownOrder)?;
        if qty.is_zero() {
            return Err(BookError::ZeroQty);
        }
        let node = self.nodes.at_mut(key.index());
        if qty >= node.qty {
            return Err(BookError::InvalidReduce);
        }
        let delta = node.qty - qty;
        node.qty = qty;
        let li = node.level;
        self.levels.at_mut(li).total -= delta;
        Ok(self.order_at(key.index()))
    }

    #[inline]
    fn get(&self, id: OrderId) -> Option<RestingOrder> {
        self.index.get(&id).map(|k| self.order_at(k.index()))
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
        let level = self.levels.at(li);
        let n = self.nodes.at(level.head);
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
        let ni = self.levels.at(li).head;
        let node = self.nodes.at_mut(ni);
        assert!(
            !qty.is_zero() && qty <= node.qty,
            "fill_front: qty {qty} not in 1..={}",
            node.qty
        );
        node.qty -= qty;
        let remaining = node.qty;
        self.levels.at_mut(li).total -= qty;
        if remaining.is_zero() {
            let id = self.nodes.at(ni).id;
            let key = self.index.remove(&id).expect("front order is indexed");
            self.unlink(key);
        }
        remaining
    }

    fn visit_levels(&self, side: Side, f: &mut dyn FnMut(LevelInfo) -> bool) {
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
