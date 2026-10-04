//! The w06 order book.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
#[allow(unused_imports)] // used by the reference internals
use crate::IdMap;
use crate::{BookError, LevelInfo, OrderBook, RestingOrder};
#[allow(unused_imports)] // used by the reference internals
use std::collections::hash_map;
use types::{OrderId, Price, Qty, Side};

// TODO(w06): implement — see the week guide on tepy.dev/hft.

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
#[derive(Clone, Debug)]
pub struct Book {
    // TODO(w06): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl Book {
    /// An empty book. Allocates nothing until the first order.
    pub fn new() -> Book {
        todo!("w06: new")
    }

    /// An empty book with room for `orders` resting orders and `levels`
    /// price levels before any reallocation.
    pub fn with_capacity(orders: usize, levels: usize) -> Book {
        todo!("w06: with_capacity")
    }
}

impl Default for Book {
    fn default() -> Book {
        Book::new()
    }
}

#[allow(unused_variables)]
impl OrderBook for Book {
    #[inline]
    fn add(&mut self, order: RestingOrder) -> Result<(), BookError> {
        todo!("w06: add")
    }

    #[inline]
    fn cancel(&mut self, id: OrderId) -> Result<RestingOrder, BookError> {
        todo!("w06: cancel")
    }

    #[inline]
    fn reduce(&mut self, id: OrderId, qty: Qty) -> Result<RestingOrder, BookError> {
        todo!("w06: reduce")
    }

    #[inline]
    fn get(&self, id: OrderId) -> Option<RestingOrder> {
        todo!("w06: get")
    }

    #[inline]
    fn contains(&self, id: OrderId) -> bool {
        todo!("w06: contains")
    }

    #[inline]
    fn best(&self, side: Side) -> Option<LevelInfo> {
        todo!("w06: best")
    }

    fn level(&self, side: Side, price: Price) -> Option<LevelInfo> {
        todo!("w06: level")
    }

    #[inline]
    fn front(&self, side: Side) -> Option<RestingOrder> {
        todo!("w06: front")
    }

    #[inline]
    fn fill_front(&mut self, side: Side, qty: Qty) -> Qty {
        todo!("w06: fill_front")
    }

    fn visit_levels(&self, side: Side, f: &mut dyn FnMut(LevelInfo) -> bool) {
        todo!("w06: visit_levels")
    }

    fn visit_level(&self, side: Side, price: Price, f: &mut dyn FnMut(RestingOrder) -> bool) {
        todo!("w06: visit_level")
    }

    fn visit_orders(&self, side: Side, f: &mut dyn FnMut(RestingOrder) -> bool) {
        todo!("w06: visit_orders")
    }

    #[inline]
    fn len(&self) -> usize {
        todo!("w06: len")
    }

    #[inline]
    fn level_count(&self, side: Side) -> usize {
        todo!("w06: level_count")
    }

    fn clear(&mut self) {
        todo!("w06: clear")
    }
}
