//! Week 27: the slab-backed order book.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::slab::SlabKey;
#[allow(unused_imports)] // used by the reference internals
use crate::IdMap;
use crate::{BookError, LevelInfo, OrderBook, RestingOrder};
#[allow(unused_imports)] // used by the reference internals
use std::collections::hash_map;
use types::{OrderId, Price, Qty, Side};

// TODO(w27): implement — see the week guide on tepy.dev/hft.

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
#[derive(Clone, Debug)]
pub struct SlabBook {
    // TODO(w27): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl SlabBook {
    /// An empty book with storage for `orders` resting orders and `levels`
    /// price levels per side, all allocated now.
    pub fn with_capacity(orders: usize, levels: usize) -> SlabBook {
        todo!("w27: with_capacity")
    }

    /// The generational handle of a resting order, or `None`. O(1) (one
    /// hash probe).
    pub fn key_of(&self, id: OrderId) -> Option<SlabKey> {
        todo!("w27: key_of")
    }

    /// The order behind `key`, or `None` if the key is stale (its order was
    /// filled, cancelled or the book cleared). O(1), no hashing.
    pub fn get_by_key(&self, key: SlabKey) -> Option<RestingOrder> {
        todo!("w27: get_by_key")
    }

    /// Cancels the order behind `key`. Errors: [`BookError::UnknownOrder`]
    /// if the key is stale (nothing changes). O(1).
    pub fn cancel_by_key(&mut self, key: SlabKey) -> Result<RestingOrder, BookError> {
        todo!("w27: cancel_by_key")
    }

    /// How many resting orders fit before the book allocates.
    pub fn order_capacity(&self) -> usize {
        todo!("w27: order_capacity")
    }
}

impl Default for SlabBook {
    /// A book with room for 1024 orders and 256 levels per side.
    fn default() -> SlabBook {
        SlabBook::with_capacity(1024, 256)
    }
}

#[allow(unused_variables)]
impl OrderBook for SlabBook {
    #[inline]
    fn add(&mut self, order: RestingOrder) -> Result<(), BookError> {
        todo!("w27: add")
    }

    #[inline]
    fn cancel(&mut self, id: OrderId) -> Result<RestingOrder, BookError> {
        todo!("w27: cancel")
    }

    #[inline]
    fn reduce(&mut self, id: OrderId, qty: Qty) -> Result<RestingOrder, BookError> {
        todo!("w27: reduce")
    }

    #[inline]
    fn get(&self, id: OrderId) -> Option<RestingOrder> {
        todo!("w27: get")
    }

    #[inline]
    fn contains(&self, id: OrderId) -> bool {
        todo!("w27: contains")
    }

    #[inline]
    fn best(&self, side: Side) -> Option<LevelInfo> {
        todo!("w27: best")
    }

    fn level(&self, side: Side, price: Price) -> Option<LevelInfo> {
        todo!("w27: level")
    }

    #[inline]
    fn front(&self, side: Side) -> Option<RestingOrder> {
        todo!("w27: front")
    }

    #[inline]
    fn fill_front(&mut self, side: Side, qty: Qty) -> Qty {
        todo!("w27: fill_front")
    }

    fn visit_levels(&self, side: Side, f: &mut dyn FnMut(LevelInfo) -> bool) {
        todo!("w27: visit_levels")
    }

    fn visit_level(&self, side: Side, price: Price, f: &mut dyn FnMut(RestingOrder) -> bool) {
        todo!("w27: visit_level")
    }

    fn visit_orders(&self, side: Side, f: &mut dyn FnMut(RestingOrder) -> bool) {
        todo!("w27: visit_orders")
    }

    #[inline]
    fn len(&self) -> usize {
        todo!("w27: len")
    }

    #[inline]
    fn level_count(&self, side: Side) -> usize {
        todo!("w27: level_count")
    }

    fn clear(&mut self) {
        todo!("w27: clear")
    }
}
