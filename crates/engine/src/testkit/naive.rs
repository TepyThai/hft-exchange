//! A deliberately naive reference matcher.

use crate::EngineConfig;
use book::RestingOrder;
#[allow(unused_imports)] // used by the reference implementation
use types::{
    Command, Event, NewOrder, OrderId, OrderKind, Price, Qty, RejectReason, Side, TimeInForce,
};

// @student-begin w12
#[derive(Clone, Copy, Debug)]
struct NaiveOrder {
    id: OrderId,
    side: Side,
    price: Price,
    qty: u64,
    /// Arrival stamp: lower = earlier = better time priority.
    time: u64,
}

impl NaiveOrder {
    /// True if `self` has better priority than `other` (same side).
    fn beats(&self, other: &NaiveOrder) -> bool {
        if self.price != other.price {
            return match self.side {
                Side::Buy => self.price > other.price,
                Side::Sell => self.price < other.price,
            };
        }
        self.time < other.time
    }
}

impl NaiveEngine {
    /// Index of the best resting order on `side` that a taker with `limit`
    /// would trade with: a full scan, every time.
    fn best_maker(&self, side: Side, limit: Option<Price>) -> Option<usize> {
        let mut best: Option<usize> = None;
        for (i, o) in self.orders.iter().enumerate() {
            if o.side != side {
                continue;
            }
            if let Some(limit) = limit {
                if !side.opposite().crosses(limit, o.price) {
                    continue;
                }
            }
            if best.is_none_or(|b| o.beats(&self.orders[b])) {
                best = Some(i);
            }
        }
        best
    }

    fn position(&self, id: OrderId) -> Option<usize> {
        self.orders.iter().position(|o| o.id == id)
    }

    fn check(&self, order: &NewOrder) -> Result<(), RejectReason> {
        let spec = self.config.spec;
        if order.qty.0 == 0 {
            return Err(RejectReason::ZeroQuantity);
        }
        if !order.qty.0.is_multiple_of(spec.lot) {
            return Err(RejectReason::InvalidLot);
        }
        if let Some(price) = order.limit_price() {
            if price.0.rem_euclid(spec.tick) != 0 {
                return Err(RejectReason::InvalidTick);
            }
            let reference = self.last_trade.or(self.config.reference_price);
            if let (true, Some(r)) = (spec.band_ticks > 0, reference) {
                if (i128::from(price.0) - i128::from(r.0)).abs()
                    > i128::from(spec.band_ticks) * i128::from(spec.tick)
                {
                    return Err(RejectReason::PriceBand);
                }
            }
        }
        if self.position(order.id).is_some() {
            return Err(RejectReason::DuplicateOrderId);
        }
        let maker_side = order.side.opposite();
        let crossing: u64 = self
            .orders
            .iter()
            .filter(|o| {
                o.side == maker_side
                    && order
                        .limit_price()
                        .is_none_or(|l| order.side.crosses(l, o.price))
            })
            .map(|o| o.qty)
            .sum();
        if let OrderKind::Limit {
            post_only: true, ..
        } = order.kind
        {
            if crossing > 0 {
                return Err(RejectReason::WouldTake);
            }
        }
        if order.kind == OrderKind::Market && !self.orders.iter().any(|o| o.side == maker_side) {
            return Err(RejectReason::NoLiquidity);
        }
        if order.tif == TimeInForce::Fok && crossing < order.qty.0 {
            return Err(RejectReason::FokUnfillable);
        }
        Ok(())
    }

    fn new_order(&mut self, order: &NewOrder, out: &mut Vec<Event>) {
        if let Err(reason) = self.check(order) {
            out.push(Event::Rejected {
                id: order.id,
                reason,
            });
            return;
        }
        out.push(Event::Accepted { id: order.id });
        let mut remaining = order.qty.0;
        while remaining > 0 {
            let Some(i) = self.best_maker(order.side.opposite(), order.limit_price()) else {
                break;
            };
            let maker = self.orders[i];
            let qty = remaining.min(maker.qty);
            remaining -= qty;
            self.orders[i].qty -= qty;
            if self.orders[i].qty == 0 {
                self.orders.remove(i);
            }
            self.last_trade = Some(maker.price);
            out.push(Event::Trade {
                taker: order.id,
                maker: maker.id,
                taker_side: order.side,
                price: maker.price,
                qty: Qty(qty),
            });
        }
        if remaining == 0 {
            return;
        }
        match (order.kind, order.tif) {
            (OrderKind::Limit { price, .. }, TimeInForce::Gtc) => {
                self.clock += 1;
                self.orders.push(NaiveOrder {
                    id: order.id,
                    side: order.side,
                    price,
                    qty: remaining,
                    time: self.clock,
                });
                out.push(Event::Rested {
                    id: order.id,
                    side: order.side,
                    price,
                    qty: Qty(remaining),
                });
            }
            _ => out.push(Event::Expired {
                id: order.id,
                remaining: Qty(remaining),
            }),
        }
    }
}
// @student-end

/// The slowest correct matcher we could write: every resting order in one
/// `Vec`, scanned in full for every match step. Same semantics, event order
/// and validation order as [`Engine`](crate::Engine) — written
/// independently, so that agreement between the two over thousands of
/// random streams means something. O(n) per operation; for tests only.
// @student w12
#[derive(Clone, Debug)]
pub struct NaiveEngine {
    config: EngineConfig,
    last_trade: Option<Price>,
    orders: Vec<NaiveOrder>,
    clock: u64,
}

// @student w12
impl NaiveEngine {
    /// An empty reference matcher with the engine's configuration (spec and
    /// band reference price).
    pub fn new(config: EngineConfig) -> NaiveEngine {
        NaiveEngine {
            config,
            last_trade: None,
            orders: Vec::new(),
            clock: 0,
        }
    }

    /// Processes one command, appending its events to `out` exactly as
    /// [`Engine::process`](crate::Engine::process) documents.
    pub fn process(&mut self, cmd: &Command, out: &mut Vec<Event>) {
        match *cmd {
            Command::New(ref order) => self.new_order(order, out),
            Command::Cancel { id } => match self.position(id) {
                Some(i) => {
                    let o = self.orders.remove(i);
                    out.push(Event::Cancelled {
                        id,
                        remaining: Qty(o.qty),
                    });
                }
                None => out.push(Event::Rejected {
                    id,
                    reason: RejectReason::UnknownOrder,
                }),
            },
            Command::Reduce { id, qty } => {
                let reason = if qty.0 == 0 {
                    RejectReason::ZeroQuantity
                } else if !qty.0.is_multiple_of(self.config.spec.lot) {
                    RejectReason::InvalidLot
                } else {
                    match self.position(id) {
                        None => RejectReason::UnknownOrder,
                        Some(i) if qty.0 >= self.orders[i].qty => RejectReason::InvalidReduce,
                        Some(i) => {
                            self.orders[i].qty = qty.0;
                            out.push(Event::Reduced { id, remaining: qty });
                            return;
                        }
                    }
                };
                out.push(Event::Rejected { id, reason });
            }
        }
    }

    /// Clears `out` and fills it with every resting order: bids best first,
    /// then asks best first, oldest first within a price.
    pub fn resting(&self, out: &mut Vec<RestingOrder>) {
        out.clear();
        for side in [Side::Buy, Side::Sell] {
            let mut mine: Vec<&NaiveOrder> =
                self.orders.iter().filter(|o| o.side == side).collect();
            mine.sort_by_key(|o| {
                let p = i128::from(o.price.0);
                (if side == Side::Buy { -p } else { p }, o.time)
            });
            out.extend(mine.into_iter().map(|o| RestingOrder {
                id: o.id,
                side,
                price: o.price,
                qty: Qty(o.qty),
            }));
        }
    }

    /// Price of the most recent trade, if any.
    pub fn last_trade_price(&self) -> Option<Price> {
        self.last_trade
    }
}
