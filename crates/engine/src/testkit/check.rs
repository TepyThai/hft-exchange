//! Invariant checking over a command / event stream.

#[allow(unused_imports)] // used by the reference implementation
use book::LevelInfo;
use book::OrderBook;
#[allow(unused_imports)] // used by the reference implementation
use std::collections::HashMap;
#[allow(unused_imports)] // used by the reference implementation
use types::{
    Command, Event, NewOrder, OrderId, OrderKind, Price, Qty, RejectReason, Side, TimeInForce,
};

/// Running quantity totals (in lots × lot size, i.e. raw `Qty` units) for
/// the conservation law.
///
/// Every accepted unit of quantity ends in exactly one place, and a trade
/// of `q` fills `q` of the taker **and** `q` of the maker, so at every
/// point between commands:
///
/// ```text
/// accepted = 2·traded + cancelled + expired + reduced + resting
/// ```
///
/// where `accepted` sums the quantity of every accepted new order, `traded`
/// the quantity of every trade, `cancelled` / `expired` the `remaining` of
/// those events, `reduced` the quantity removed by reduces, and `resting`
/// is the quantity on the book now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub accepted: u64,
    pub traded: u64,
    pub cancelled: u64,
    pub expired: u64,
    pub reduced: u64,
    /// Resting quantity on the book after the last checked command.
    pub resting: u64,
}

impl Totals {
    /// True if the conservation law holds.
    pub fn is_balanced(&self) -> bool {
        self.accepted
            == 2 * self.traded + self.cancelled + self.expired + self.reduced + self.resting
    }
}

/// An invariant that did not hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    /// Zero-based index of the offending command among those checked.
    pub command: u64,
    pub message: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invariant violated at command #{}: {}",
            self.command, self.message
        )
    }
}

impl std::error::Error for Violation {}

// @student-begin w12
/// The checker's view of one open order.
#[derive(Clone, Copy, Debug)]
struct Open {
    side: Side,
    price: Price,
    qty: u64,
    /// Arrival rank on the book (1, 2, …); 0 while it is the active taker.
    rank: u64,
}

/// Priority key of a resting order on its side: smaller is better.
fn priority(side: Side, price: Price, rank: u64) -> (i128, u64) {
    let p = i128::from(price.0);
    (if side == Side::Buy { -p } else { p }, rank)
}

macro_rules! ensure {
    ($cond:expr, $($fmt:tt)+) => {
        if !$cond {
            return Err(format!($($fmt)+));
        }
    };
}

impl InvariantChecker {
    fn check_new<B: OrderBook>(
        &mut self,
        o: &NewOrder,
        events: &[Event],
        book: &B,
    ) -> Result<(), String> {
        let Some(first) = events.first() else {
            return Err("new order produced no events".into());
        };
        match *first {
            Event::Rejected { id, reason } => {
                ensure!(
                    events.len() == 1,
                    "Rejected must be the only event, got {events:?}"
                );
                ensure!(
                    id == o.id,
                    "Rejected id {id:?} is not the order's {:?}",
                    o.id
                );
                match reason {
                    RejectReason::ZeroQuantity => {
                        ensure!(o.qty.is_zero(), "ZeroQuantity for qty {}", o.qty)
                    }
                    RejectReason::DuplicateOrderId => {
                        ensure!(
                            self.open.contains_key(&id),
                            "DuplicateOrderId but {id:?} is not resting"
                        )
                    }
                    RejectReason::NoLiquidity => {
                        ensure!(o.kind == OrderKind::Market, "NoLiquidity on a limit order")
                    }
                    RejectReason::UnknownOrder | RejectReason::InvalidReduce => {
                        return Err(format!("{reason:?} is not a new-order reject"))
                    }
                    _ => {}
                }
                return Ok(());
            }
            Event::Accepted { id } => {
                ensure!(
                    id == o.id,
                    "Accepted id {id:?} is not the order's {:?}",
                    o.id
                );
                ensure!(!o.qty.is_zero(), "accepted a zero-quantity order");
                ensure!(!self.open.contains_key(&id), "accepted duplicate id {id:?}");
                let price = o.limit_price().unwrap_or_default();
                self.open.insert(
                    id,
                    Open {
                        side: o.side,
                        price,
                        qty: o.qty.0,
                        rank: 0,
                    },
                );
                self.totals.accepted += o.qty.0;
            }
            other => {
                return Err(format!(
                    "first event must be Accepted or Rejected, got {other:?}"
                ))
            }
        }

        let maker_side = o.side.opposite();
        // Priority key of the last maker hit, and whether it was exhausted.
        let mut last: Option<((i128, u64), bool)> = None;
        let mut i = 1;
        while let Some(&Event::Trade {
            taker,
            maker,
            taker_side,
            price,
            qty,
        }) = events.get(i)
        {
            ensure!(
                taker == o.id && taker_side == o.side,
                "trade taker/side mismatch: {:?}",
                events[i]
            );
            ensure!(!qty.is_zero(), "zero-quantity trade");
            if let OrderKind::Limit {
                price: limit,
                post_only,
            } = o.kind
            {
                ensure!(!post_only, "post-only order traded");
                ensure!(
                    o.side.crosses(limit, price),
                    "trade at {price} does not satisfy limit {limit}"
                );
            }
            let Some(m) = self.open.get(&maker).copied() else {
                return Err(format!("maker {maker:?} is not a resting order"));
            };
            ensure!(
                m.rank > 0 && m.side == maker_side,
                "maker {maker:?} is not resting on the {maker_side:?} side"
            );
            ensure!(
                price == m.price,
                "trade at {price}, maker rests at {}",
                m.price
            );
            ensure!(
                qty.0 <= m.qty,
                "trade of {qty} exceeds maker remaining {}",
                m.qty
            );
            let key = priority(maker_side, m.price, m.rank);
            if let Some((prev, exhausted)) = last {
                ensure!(
                    exhausted,
                    "moved past a maker that was not filled completely"
                );
                ensure!(
                    key > prev,
                    "maker {maker:?} traded out of price-time priority"
                );
            }
            let t = self.open.get_mut(&o.id).unwrap();
            ensure!(qty.0 <= t.qty, "trades exceed the taker's quantity");
            t.qty -= qty.0;
            let left = m.qty - qty.0;
            if left == 0 {
                self.open.remove(&maker);
            } else {
                self.open.get_mut(&maker).unwrap().qty = left;
            }
            self.totals.traded += qty.0;
            last = Some((key, left == 0));
            i += 1;
        }

        // Makers were taken best-first: whatever is left on the maker side
        // ranks behind the last maker hit (or is that maker, partly filled).
        if let (Some((prev, exhausted)), Some(front)) = (last, book.front(maker_side)) {
            let rank = self.open.get(&front.id).map_or(0, |m| m.rank);
            let key = priority(maker_side, front.price, rank);
            if exhausted {
                ensure!(key > prev, "a better maker {front:?} was skipped");
            } else {
                ensure!(
                    key == prev,
                    "a partly filled maker must stay at the front, found {front:?}"
                );
            }
        }

        let remaining = self.open[&o.id].qty;
        match events.get(i) {
            None => {
                ensure!(
                    remaining == 0,
                    "{remaining} left with neither Rested nor Expired"
                );
                self.open.remove(&o.id);
            }
            Some(&Event::Rested {
                id,
                side,
                price,
                qty,
            }) => {
                ensure!(
                    id == o.id && side == o.side && Some(price) == o.limit_price(),
                    "Rested does not describe the order: {:?}",
                    events[i]
                );
                ensure!(o.tif == TimeInForce::Gtc, "{:?} order rested", o.tif);
                ensure!(
                    qty.0 == remaining && remaining > 0,
                    "Rested qty {qty}, remaining {remaining}"
                );
                self.next_rank += 1;
                self.open.get_mut(&o.id).unwrap().rank = self.next_rank;
            }
            Some(&Event::Expired { id, remaining: r }) => {
                ensure!(id == o.id, "Expired id {id:?} is not the order's");
                ensure!(
                    r.0 == remaining && remaining > 0,
                    "Expired remaining {r}, actual {remaining}"
                );
                ensure!(
                    o.kind == OrderKind::Market || o.tif == TimeInForce::Ioc,
                    "{:?} {:?} order expired",
                    o.kind,
                    o.tif
                );
                self.open.remove(&o.id);
                self.totals.expired += remaining;
            }
            Some(other) => return Err(format!("unexpected event after trades: {other:?}")),
        }
        ensure!(
            i + 1 >= events.len(),
            "events after the final event: {events:?}"
        );
        ensure!(
            o.tif != TimeInForce::Fok || remaining == 0,
            "accepted FOK order was not filled completely"
        );

        // Matching stopped only when nothing more could trade.
        if remaining > 0 {
            if let Some(front) = book.front(maker_side) {
                let tradable = o
                    .limit_price()
                    .is_none_or(|limit| o.side.crosses(limit, front.price));
                ensure!(
                    !tradable,
                    "stopped matching with {remaining} left while {front:?} crosses"
                );
            }
        }
        Ok(())
    }

    fn check_cancel(&mut self, id: OrderId, events: &[Event]) -> Result<(), String> {
        ensure!(
            events.len() == 1,
            "cancel must produce one event, got {events:?}"
        );
        let resting = self.open.get(&id).copied();
        match (events[0], resting) {
            (Event::Cancelled { id: eid, remaining }, Some(o)) => {
                ensure!(
                    eid == id && remaining.0 == o.qty,
                    "Cancelled {:?} but {id:?} had {} left",
                    events[0],
                    o.qty
                );
                self.open.remove(&id);
                self.totals.cancelled += o.qty;
            }
            (
                Event::Rejected {
                    id: eid,
                    reason: RejectReason::UnknownOrder,
                },
                None,
            ) => {
                ensure!(eid == id, "reject for the wrong id");
            }
            (e, r) => return Err(format!("cancel of {id:?} (resting: {r:?}) produced {e:?}")),
        }
        Ok(())
    }

    fn check_reduce(&mut self, id: OrderId, qty: Qty, events: &[Event]) -> Result<(), String> {
        ensure!(
            events.len() == 1,
            "reduce must produce one event, got {events:?}"
        );
        let resting = self.open.get(&id).copied();
        match (events[0], resting) {
            (Event::Reduced { id: eid, remaining }, Some(o)) => {
                ensure!(
                    eid == id && remaining == qty,
                    "Reduced {:?} for a reduce to {qty}",
                    events[0]
                );
                ensure!(
                    qty.0 > 0 && qty.0 < o.qty,
                    "reduce from {} to {qty} accepted",
                    o.qty
                );
                self.totals.reduced += o.qty - qty.0;
                self.open.get_mut(&id).unwrap().qty = qty.0;
            }
            (Event::Rejected { id: eid, reason }, r) => {
                ensure!(eid == id, "reject for the wrong id");
                let consistent = match reason {
                    RejectReason::ZeroQuantity => qty.is_zero(),
                    RejectReason::InvalidLot => true,
                    RejectReason::UnknownOrder => r.is_none(),
                    RejectReason::InvalidReduce => r.is_some_and(|o| qty.0 >= o.qty),
                    _ => false,
                };
                ensure!(
                    consistent,
                    "reduce of {id:?} to {qty} (resting: {r:?}) rejected with {reason:?}"
                );
            }
            (e, r) => return Err(format!("reduce of {id:?} (resting: {r:?}) produced {e:?}")),
        }
        Ok(())
    }

    fn check_book<B: OrderBook>(&mut self, book: &B) -> Result<(), String> {
        let (bid, ask) = (book.best_bid(), book.best_ask());
        if let (Some(b), Some(a)) = (bid, ask) {
            ensure!(
                b.price < a.price,
                "book crossed: bid {} >= ask {}",
                b.price,
                a.price
            );
        }
        let mut count = 0usize;
        let mut resting = 0u64;
        for side in [Side::Buy, Side::Sell] {
            let mut levels: Vec<LevelInfo> = Vec::new();
            book.visit_levels(side, &mut |l| {
                levels.push(l);
                true
            });
            ensure!(
                book.level_count(side) == levels.len(),
                "{side:?} level_count disagrees with the levels"
            );
            ensure!(
                book.best(side) == levels.first().copied(),
                "{side:?} best is not the first level"
            );
            for pair in levels.windows(2) {
                let ok = match side {
                    Side::Buy => pair[0].price > pair[1].price,
                    Side::Sell => pair[0].price < pair[1].price,
                };
                ensure!(
                    ok,
                    "{side:?} levels out of order: {} then {}",
                    pair[0].price,
                    pair[1].price
                );
            }
            for level in &levels {
                ensure!(
                    !level.qty.is_zero() && level.orders > 0,
                    "empty level {level:?}"
                );
                let (mut sum, mut n, mut last_rank) = (0u64, 0u32, 0u64);
                let mut err = None;
                let open = &self.open;
                book.visit_level(side, level.price, &mut |o| {
                    let fail = |m: String| Some(m);
                    err = match open.get(&o.id) {
                        _ if o.qty.is_zero() => fail(format!("zero-quantity resting order {o:?}")),
                        _ if o.side != side || o.price != level.price => {
                            fail(format!("order {o:?} in wrong level"))
                        }
                        None => fail(format!(
                            "phantom order {o:?} (never rested, or already gone)"
                        )),
                        Some(m)
                            if m.rank == 0
                                || m.qty != o.qty.0
                                || m.side != side
                                || m.price != o.price =>
                        {
                            fail(format!("order {o:?} disagrees with its history {m:?}"))
                        }
                        Some(m) if m.rank <= last_rank => fail(format!("FIFO violated at {o:?}")),
                        Some(m) => {
                            last_rank = m.rank;
                            None
                        }
                    };
                    sum += o.qty.0;
                    n += 1;
                    err.is_none()
                });
                if let Some(e) = err {
                    return Err(e);
                }
                ensure!(
                    sum == level.qty.0 && n == level.orders,
                    "level {level:?} aggregates {sum} in {n} orders"
                );
                count += n as usize;
                resting += sum;
            }
        }
        ensure!(
            book.len() == count,
            "book.len() {} but {count} orders visited",
            book.len()
        );
        ensure!(
            self.open.len() == count,
            "{} orders should rest, the book has {count}",
            self.open.len()
        );
        self.totals.resting = resting;
        ensure!(
            self.totals.is_balanced(),
            "quantity not conserved: {:?}",
            self.totals
        );
        Ok(())
    }
}
// @student-end

/// Checks an engine run command by command.
///
/// Feed it each command, the events the engine produced for it and the
/// engine's book afterwards ([`Engine::book`](crate::Engine::book)). It
/// keeps a ledger of every open order (side, price, remaining, arrival
/// rank) built **only from events**, and verifies:
///
/// * **event contract** — the documented event order and shape for each
///   command; rejects are alone; ids, sides and quantities agree;
/// * **matching** — trades at the maker's price, within the taker's limit,
///   in price-time priority, never past a partly filled maker, and matching
///   stops only when nothing crossing is left; post-only never trades; FOK
///   fills completely or not at all; only GTC limits rest, only IOC /
///   market orders expire;
/// * **book** — never crossed; levels strictly ordered with correct
///   aggregates and no empty levels; no zero-quantity orders; FIFO within
///   each level (arrival rank increasing); the book holds exactly the orders
///   the events say are resting, with the quantities the events imply;
/// * **conservation** — [`Totals::is_balanced`] after every command.
///
/// Assumes the engine started empty. O(book size) per command.
// @student w12
#[derive(Clone, Debug, Default)]
pub struct InvariantChecker {
    open: HashMap<OrderId, Open>,
    next_rank: u64,
    totals: Totals,
    commands: u64,
}

// @student w12
impl InvariantChecker {
    /// A checker for an engine that has processed nothing yet.
    pub fn new() -> InvariantChecker {
        InvariantChecker::default()
    }

    /// Checks one command's `events` and the `book` after it.
    pub fn check<B: OrderBook>(
        &mut self,
        cmd: &Command,
        events: &[Event],
        book: &B,
    ) -> Result<(), Violation> {
        let index = self.commands;
        self.commands += 1;
        let result = match *cmd {
            Command::New(ref order) => self.check_new(order, events, book),
            Command::Cancel { id } => self.check_cancel(id, events),
            Command::Reduce { id, qty } => self.check_reduce(id, qty, events),
        }
        .and_then(|()| self.check_book(book));
        result.map_err(|message| Violation {
            command: index,
            message,
        })
    }

    /// Quantity totals so far.
    pub fn totals(&self) -> Totals {
        self.totals
    }

    /// Number of commands checked so far.
    pub fn commands(&self) -> u64 {
        self.commands
    }
}
