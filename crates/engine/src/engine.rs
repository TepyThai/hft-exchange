//! The matching engine: commands in, events out.

use crate::tape::TradeTape;
use book::{Book, OrderBook};
#[allow(unused_imports)] // used by the reference implementation
use types::{
    Command, Event, InstrumentSpec, NewOrder, OrderId, OrderKind, Price, Qty, RejectReason, Seq,
    Side, TimeInForce,
};
#[allow(unused_imports)] // used by the reference implementation
use {crate::hash::StateHasher, crate::tape::TradePrint, book::RestingOrder};

/// Static configuration of one engine (one instrument).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineConfig {
    /// Tick size, lot size and price band the engine validates against.
    pub spec: InstrumentSpec,
    /// Reference price for the price band until the first trade (after
    /// that, the last trade price is the reference). `None`: no band check
    /// before the first trade.
    pub reference_price: Option<Price>,
    /// How many recent trades the [`TradeTape`] keeps for snapshots.
    pub tape_capacity: usize,
    /// Capacity hint: resting orders to pre-allocate room for.
    pub expected_orders: usize,
}

impl EngineConfig {
    /// Defaults for `spec`: no reference price, a 64-trade tape, room for
    /// 1024 resting orders.
    pub fn new(spec: InstrumentSpec) -> EngineConfig {
        EngineConfig {
            spec,
            reference_price: None,
            tape_capacity: 64,
            expected_orders: 1024,
        }
    }

    /// Sets the band reference price used until the first trade.
    pub fn with_reference_price(mut self, price: Price) -> EngineConfig {
        self.reference_price = Some(price);
        self
    }
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig::new(InstrumentSpec::default())
    }
}

/// A single-instrument, single-threaded matching engine in price-time
/// priority, generic over its order book.
///
/// The engine is a deterministic state machine: the same sequence of
/// [`Command`]s always produces the same [`Event`]s and the same
/// [`state_hash`](Engine::state_hash). It reads no clock, draws no random
/// numbers and iterates no randomly seeded hash map, which is what lets the
/// journal (w09) rebuild it by replay.
///
/// See the [crate docs](crate) for the event-order contract and the
/// validation order.
///
/// `B` is any [`OrderBook`]: the w06 [`Book`] by default, the w27 slab book
/// or the w28 ladder book via [`Engine::with_book`], or
/// `Box<dyn OrderBook>` for dynamic dispatch (w37).
// @student w07
#[derive(Clone, Debug)]
pub struct Engine<B = Book> {
    book: B,
    config: EngineConfig,
    last_trade: Option<Price>,
    seq: u64,
    tape: TradeTape,
}

impl Engine<Book> {
    /// An engine over a fresh [`Book`] with default settings for `spec`.
    pub fn new(spec: InstrumentSpec) -> Engine<Book> {
        Engine::with_config(EngineConfig::new(spec))
    }

    /// An engine over a fresh [`Book`] sized for `config.expected_orders`.
    pub fn with_config(config: EngineConfig) -> Engine<Book> {
        Engine::with_book(config, Book::with_capacity(config.expected_orders, 256))
    }
}

impl Default for Engine<Book> {
    fn default() -> Self {
        Engine::new(InstrumentSpec::default())
    }
}

// @student w07
impl<B: OrderBook> Engine<B> {
    /// An engine over `book` (normally empty; a non-empty book is adopted
    /// as-is, e.g. when restoring from a snapshot).
    ///
    /// # Panics
    /// If `config.spec.tick` or `config.spec.lot` is not positive.
    pub fn with_book(config: EngineConfig, book: B) -> Engine<B> {
        assert!(config.spec.tick > 0, "tick size must be positive");
        assert!(config.spec.lot > 0, "lot size must be positive");
        Engine {
            book,
            config,
            last_trade: None,
            seq: 0,
            tape: TradeTape::new(config.tape_capacity),
        }
    }

    /// Processes one command, **appending** its events to `out` in the
    /// documented order (the caller clears `out` between commands and
    /// reuses it, so steady-state processing does not allocate).
    ///
    /// Cost: O(1) per trade plus the book operation costs (see
    /// [`Book`]); FOK adds a walk of the levels it needs.
    #[inline]
    pub fn process(&mut self, cmd: &Command, out: &mut Vec<Event>) {
        match *cmd {
            Command::New(ref order) => self.submit(order, out),
            Command::Cancel { id } => self.cancel(id, out),
            Command::Reduce { id, qty } => self.reduce(id, qty, out),
        }
    }

    /// Handles a new order: validate, then match against the opposite side
    /// in price-time priority, then rest (GTC limit) or expire the rest.
    ///
    /// Events: `Rejected` alone, or `Accepted`, `Trade`*, then `Rested` or
    /// `Expired` if quantity remains.
    pub fn submit(&mut self, order: &NewOrder, out: &mut Vec<Event>) {
        self.seq += 1;
        if let Err(reason) = self.validate(order) {
            out.push(Event::Rejected {
                id: order.id,
                reason,
            });
            return;
        }
        out.push(Event::Accepted { id: order.id });

        let maker_side = order.side.opposite();
        let limit = order.limit_price();
        let mut remaining = order.qty;
        while !remaining.is_zero() {
            let Some(maker) = self.book.front(maker_side) else {
                break;
            };
            if let Some(limit) = limit {
                if !order.side.crosses(limit, maker.price) {
                    break;
                }
            }
            let qty = remaining.min(maker.qty);
            self.book.fill_front(maker_side, qty);
            remaining -= qty;
            self.last_trade = Some(maker.price);
            self.tape.push(TradePrint {
                seq: Seq(self.seq),
                taker: order.id,
                maker: maker.id,
                taker_side: order.side,
                price: maker.price,
                qty,
            });
            out.push(Event::Trade {
                taker: order.id,
                maker: maker.id,
                taker_side: order.side,
                price: maker.price,
                qty,
            });
        }
        if remaining.is_zero() {
            return;
        }
        match (order.kind, order.tif) {
            (OrderKind::Limit { price, .. }, TimeInForce::Gtc) => {
                let rest = RestingOrder {
                    id: order.id,
                    side: order.side,
                    price,
                    qty: remaining,
                };
                // Validation already ruled out a duplicate id and zero qty.
                self.book.add(rest).expect("validated order must rest");
                out.push(Event::Rested {
                    id: order.id,
                    side: order.side,
                    price,
                    qty: remaining,
                });
            }
            _ => out.push(Event::Expired {
                id: order.id,
                remaining,
            }),
        }
    }

    /// Cancels a resting order. Events: `Cancelled { remaining }`, or
    /// `Rejected(UnknownOrder)`. O(1) (see [`OrderBook::cancel`]).
    pub fn cancel(&mut self, id: OrderId, out: &mut Vec<Event>) {
        self.seq += 1;
        match self.book.cancel(id) {
            Ok(order) => out.push(Event::Cancelled {
                id,
                remaining: order.qty,
            }),
            Err(e) => out.push(Event::Rejected {
                id,
                reason: e.into(),
            }),
        }
    }

    /// Reduces a resting order's remaining quantity to `qty`, keeping its
    /// time priority. Events: `Reduced { remaining: qty }`, or `Rejected`
    /// with (first failing check) `ZeroQuantity`, `InvalidLot`,
    /// `UnknownOrder`, `InvalidReduce`. O(1).
    pub fn reduce(&mut self, id: OrderId, qty: Qty, out: &mut Vec<Event>) {
        self.seq += 1;
        let reason = if qty.is_zero() {
            RejectReason::ZeroQuantity
        } else if !qty.0.is_multiple_of(self.config.spec.lot) {
            RejectReason::InvalidLot
        } else {
            match self.book.reduce(id, qty) {
                Ok(order) => {
                    out.push(Event::Reduced {
                        id,
                        remaining: order.qty,
                    });
                    return;
                }
                Err(e) => e.into(),
            }
        };
        out.push(Event::Rejected { id, reason });
    }

    /// The order book (read-only: all mutation goes through commands).
    #[inline]
    pub fn book(&self) -> &B {
        &self.book
    }

    /// The configuration the engine was built with.
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// Number of commands processed so far (rejected ones included) — the
    /// sequence number of the last command. Starts at `Seq(0)`.
    pub fn seq(&self) -> Seq {
        Seq(self.seq)
    }

    /// Price of the most recent trade, if any.
    pub fn last_trade_price(&self) -> Option<Price> {
        self.last_trade
    }

    /// The most recent trades (see [`TradeTape`]).
    pub fn tape(&self) -> &TradeTape {
        &self.tape
    }

    /// A deterministic 64-bit hash of the engine state that affects future
    /// output: the sequence number, the last trade price, and every resting
    /// order (side, price, id, remaining qty) in priority order. Two engines
    /// fed the same commands have equal hashes; a journal replay (w09) or a
    /// snapshot restore (w18) must reproduce it. O(resting orders).
    pub fn state_hash(&self) -> u64 {
        let mut h = StateHasher::new();
        h.write_u64(self.seq);
        match self.last_trade {
            Some(p) => {
                h.write_u64(1);
                h.write_i64(p.0);
            }
            None => h.write_u64(0),
        }
        for side in [Side::Buy, Side::Sell] {
            h.write_u64(self.book.level_count(side) as u64);
            self.book.visit_orders(side, &mut |o| {
                h.write_i64(o.price.0);
                h.write_u64(o.id.0);
                h.write_u64(o.qty.0);
                true
            });
        }
        h.write_u64(self.book.len() as u64);
        h.finish()
    }
}

// @student w08
impl<B: OrderBook> Engine<B> {
    /// Checks a new order against the instrument spec and the current book
    /// **without changing anything**. Returns the first failing check, in
    /// the documented order: `ZeroQuantity`, `InvalidLot`, `InvalidTick`,
    /// `PriceBand`, `DuplicateOrderId`, `WouldTake`, `NoLiquidity`,
    /// `FokUnfillable`.
    ///
    /// Cost: O(1) plus one id lookup, plus the FOK walk for FOK orders.
    pub fn validate(&self, order: &NewOrder) -> Result<(), RejectReason> {
        let spec = &self.config.spec;
        if order.qty.is_zero() {
            return Err(RejectReason::ZeroQuantity);
        }
        if !order.qty.0.is_multiple_of(spec.lot) {
            return Err(RejectReason::InvalidLot);
        }
        if let OrderKind::Limit { price, .. } = order.kind {
            if price.0.rem_euclid(spec.tick) != 0 {
                return Err(RejectReason::InvalidTick);
            }
            if spec.band_ticks > 0 {
                if let Some(reference) = self.reference_price() {
                    let max = (spec.band_ticks as u64).saturating_mul(spec.tick as u64);
                    if price.0.abs_diff(reference.0) > max {
                        return Err(RejectReason::PriceBand);
                    }
                }
            }
        }
        if self.book.contains(order.id) {
            return Err(RejectReason::DuplicateOrderId);
        }
        let opposite = self.book.best(order.side.opposite());
        match order.kind {
            OrderKind::Limit {
                price,
                post_only: true,
            } => {
                if opposite.is_some_and(|best| order.side.crosses(price, best.price)) {
                    return Err(RejectReason::WouldTake);
                }
            }
            OrderKind::Market => {
                if opposite.is_none() {
                    return Err(RejectReason::NoLiquidity);
                }
            }
            OrderKind::Limit { .. } => {}
        }
        if order.tif == TimeInForce::Fok
            && self.fillable(order.side, order.limit_price(), order.qty) < order.qty
        {
            return Err(RejectReason::FokUnfillable);
        }
        Ok(())
    }

    /// The band reference price: the last trade, or the configured
    /// reference before the first trade.
    pub fn reference_price(&self) -> Option<Price> {
        self.last_trade.or(self.config.reference_price)
    }

    /// How much a `side` taker with limit `limit` (`None` = market) could
    /// fill right now, counting at most `want`: the fill-or-kill feasibility
    /// check. Read-only; walks the opposite side from the best level and
    /// stops at the first level that does not cross or once `want` is
    /// reached. O(levels walked), no allocation.
    pub fn fillable(&self, side: Side, limit: Option<Price>, want: Qty) -> Qty {
        let mut got = Qty::ZERO;
        if want.is_zero() {
            return got;
        }
        self.book.visit_levels(side.opposite(), &mut |level| {
            if let Some(limit) = limit {
                if !side.crosses(limit, level.price) {
                    return false;
                }
            }
            got += level.qty;
            got < want
        });
        got.min(want)
    }
}
