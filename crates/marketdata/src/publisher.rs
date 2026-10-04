//! Engine events → sequenced L2 incrementals and snapshots.
//!
//! # Why an L2 publisher tracks every order
//!
//! L2 is aggregated by price, but the events that change it are about
//! *orders*, and most of them do not say where the order rests:
//!
//! * `Cancelled { id, remaining }` and `Expired { id, remaining }` carry
//!   neither side nor price — only the order's own record knows which
//!   level to reduce.
//! * `Reduced { id, remaining }` carries the order's *new* quantity, not the
//!   change; the level moves by `old - new`, so the old quantity must be
//!   known.
//! * `Expired` is also emitted for IOC / market remainders that never
//!   rested; those must not touch the book at all, and only a lookup tells
//!   the two cases apart.
//! * A `Trade` names the maker and its price, but whether the maker is now
//!   fully filled (and must be forgotten) depends on its remaining
//!   quantity.
//!
//! So the publisher keeps an L3 view — `order id → (side, price,
//! remaining)` — next to the L2 depth it publishes. It is the same
//! information a consumer of a by-order (L3) feed would have to keep.

use crate::{L2Snapshot, L2Update, Level};
use types::{Event, OrderId, Price, Qty, Seq, Side};

// @student-begin w10
use crate::depth::Depth;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// What the publisher remembers about one resting order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RestingOrder {
    side: Side,
    price: Price,
    qty: Qty,
}

/// Multiplicative (Fx-style) hash for engine-assigned order ids. The
/// default SipHash defends against adversarial keys, which exchange-assigned
/// ids are not; this costs one multiply.
#[derive(Clone, Copy, Debug, Default)]
struct IdHasher(u64);

impl Hasher for IdHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(b as u64);
        }
    }

    #[inline]
    fn write_u64(&mut self, x: u64) {
        self.0 = (self.0.rotate_left(5) ^ x).wrapping_mul(0x517c_c1b7_2722_0a95);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

type OrderMap = HashMap<OrderId, RestingOrder, BuildHasherDefault<IdHasher>>;
// @student-end

/// Turns one instrument's engine [`Event`]s into an L2 feed.
///
/// Feed it every event the engine emits for the instrument, in order, from
/// the start (it cannot see orders that rested before it was created).
/// Memory is O(resting orders + price levels).
// @student w10
#[derive(Clone, Debug)]
pub struct L2Publisher {
    /// L3 view: every resting order.
    orders: OrderMap,
    /// L2 view: aggregate quantity per level.
    depth: Depth,
    /// Sequence number of the last update emitted (0 = none yet).
    last_seq: Seq,
    /// Emit a periodic snapshot every this many updates (0 = never).
    snapshot_interval: u64,
    /// `last_seq` when the last periodic snapshot was taken.
    last_snapshot_seq: Seq,
}

// @student w10
impl L2Publisher {
    /// An empty book with no periodic snapshots.
    pub fn new() -> L2Publisher {
        L2Publisher::with_snapshot_interval(0)
    }

    /// An empty book that offers a full snapshot through
    /// [`periodic_snapshot`](L2Publisher::periodic_snapshot) every
    /// `updates` updates (0 = never).
    pub fn with_snapshot_interval(updates: u64) -> L2Publisher {
        L2Publisher {
            orders: OrderMap::default(),
            depth: Depth::default(),
            last_seq: Seq(0),
            snapshot_interval: updates,
            last_snapshot_seq: Seq(0),
        }
    }

    /// Applies one engine event and returns the level change it causes, if
    /// any — at most one: `Rested`, `Trade` (the maker's level), `Cancelled`,
    /// `Reduced`, and `Expired` of a resting order each change exactly one
    /// level; `Accepted`, `Rejected` and `Expired` of an order that never
    /// rested change none. Events about unknown orders are ignored.
    ///
    /// O(1) expected for the order lookup plus O(log levels) for the level;
    /// allocates only when a new order or level needs room.
    #[inline]
    pub fn on_event(&mut self, ev: &Event) -> Option<L2Update> {
        let (side, price, qty) = match *ev {
            Event::Rested {
                id,
                side,
                price,
                qty,
            } => {
                if qty.is_zero() {
                    return None;
                }
                if let Some(old) = self.orders.insert(id, RestingOrder { side, price, qty }) {
                    // An engine never rests the same id twice; stay
                    // consistent if it does.
                    self.depth.sub(old.side, old.price, old.qty);
                }
                (side, price, self.depth.add(side, price, qty))
            }
            Event::Trade { maker, qty, .. } => {
                let order = self.orders.get_mut(&maker)?;
                let fill = qty.min(order.qty);
                order.qty -= fill;
                let (side, price) = (order.side, order.price);
                if order.qty.is_zero() {
                    self.orders.remove(&maker);
                }
                (side, price, self.depth.sub(side, price, fill))
            }
            Event::Cancelled { id, .. } | Event::Expired { id, .. } => {
                let order = self.orders.remove(&id)?;
                (
                    order.side,
                    order.price,
                    self.depth.sub(order.side, order.price, order.qty),
                )
            }
            Event::Reduced { id, remaining } => {
                let order = self.orders.get_mut(&id)?;
                let old = order.qty;
                if remaining == old {
                    return None;
                }
                order.qty = remaining;
                let (side, price) = (order.side, order.price);
                if remaining.is_zero() {
                    self.orders.remove(&id);
                }
                let level = if remaining < old {
                    self.depth.sub(side, price, old - remaining)
                } else {
                    self.depth.add(side, price, remaining - old)
                };
                (side, price, level)
            }
            Event::Accepted { .. } | Event::Rejected { .. } => return None,
        };
        self.last_seq = Seq(self.last_seq.0 + 1);
        Some(L2Update {
            seq: self.last_seq,
            side,
            price,
            qty,
        })
    }

    /// Sequence number of the last update emitted (`Seq(0)` before the
    /// first).
    pub fn last_seq(&self) -> Seq {
        self.last_seq
    }

    /// The full book as of [`last_seq`](L2Publisher::last_seq). O(levels),
    /// allocates the two level vectors.
    pub fn snapshot(&self) -> L2Snapshot {
        self.depth.snapshot(self.last_seq, usize::MAX)
    }

    /// The best `depth` levels per side as of
    /// [`last_seq`](L2Publisher::last_seq) — for display. A consumer
    /// cannot rebuild a full book from a truncated snapshot.
    /// O(depth), allocates.
    pub fn snapshot_top(&self, depth: usize) -> L2Snapshot {
        self.depth.snapshot(self.last_seq, depth)
    }

    /// A full snapshot if at least `snapshot_interval` updates have been
    /// emitted since the last one this method returned; `None` otherwise
    /// (or if periodic snapshots are off). Call it after each event.
    pub fn periodic_snapshot(&mut self) -> Option<L2Snapshot> {
        let interval = self.snapshot_interval;
        if interval == 0 || self.last_seq.0 - self.last_snapshot_seq.0 < interval {
            return None;
        }
        self.last_snapshot_seq = self.last_seq;
        Some(self.snapshot())
    }

    /// Aggregate quantity at one level (zero if absent). O(log levels).
    pub fn level_qty(&self, side: Side, price: Price) -> Qty {
        self.depth.get(side, price)
    }

    /// Highest bid level. O(log levels).
    pub fn best_bid(&self) -> Option<Level> {
        self.depth.best(Side::Buy)
    }

    /// Lowest ask level. O(log levels).
    pub fn best_ask(&self) -> Option<Level> {
        self.depth.best(Side::Sell)
    }

    /// Number of non-empty levels on one side.
    pub fn level_count(&self, side: Side) -> usize {
        self.depth.level_count(side)
    }

    /// Number of resting orders tracked.
    pub fn order_count(&self) -> usize {
        self.orders.len()
    }

    /// Remaining quantity of a resting order (the L3 view), if it rests.
    pub fn resting_qty(&self, id: OrderId) -> Option<Qty> {
        self.orders.get(&id).map(|o| o.qty)
    }
}

impl Default for L2Publisher {
    fn default() -> Self {
        L2Publisher::new()
    }
}
