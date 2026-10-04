//! The matching engine: commands in, events out.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
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
#[derive(Clone, Debug)]
pub struct Engine<B = Book> {
    // TODO(w07): design this type's fields.
    _todo: std::marker::PhantomData<(B,)>,
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

#[allow(unused_variables)]
impl<B: OrderBook> Engine<B> {
    /// An engine over `book` (normally empty; a non-empty book is adopted
    /// as-is, e.g. when restoring from a snapshot).
    ///
    /// # Panics
    /// If `config.spec.tick` or `config.spec.lot` is not positive.
    pub fn with_book(config: EngineConfig, book: B) -> Engine<B> {
        todo!("w07: with_book")
    }

    /// Processes one command, **appending** its events to `out` in the
    /// documented order (the caller clears `out` between commands and
    /// reuses it, so steady-state processing does not allocate).
    ///
    /// Cost: O(1) per trade plus the book operation costs (see
    /// [`Book`]); FOK adds a walk of the levels it needs.
    #[inline]
    pub fn process(&mut self, cmd: &Command, out: &mut Vec<Event>) {
        todo!("w07: process")
    }

    /// Handles a new order: validate, then match against the opposite side
    /// in price-time priority, then rest (GTC limit) or expire the rest.
    ///
    /// Events: `Rejected` alone, or `Accepted`, `Trade`*, then `Rested` or
    /// `Expired` if quantity remains.
    ///
    /// Cost: a passive GTC limit (the common case) is one id-index probe
    /// plus the book's add; a taker pays one id probe plus O(1) per trade.
    pub fn submit(&mut self, order: &NewOrder, out: &mut Vec<Event>) {
        todo!("w07: submit")
    }

    /// Cancels a resting order. Events: `Cancelled { remaining }`, or
    /// `Rejected(UnknownOrder)`. O(1) (see [`OrderBook::cancel`]).
    pub fn cancel(&mut self, id: OrderId, out: &mut Vec<Event>) {
        todo!("w07: cancel")
    }

    /// Reduces a resting order's remaining quantity to `qty`, keeping its
    /// time priority. Events: `Reduced { remaining: qty }`, or `Rejected`
    /// with (first failing check) `ZeroQuantity`, `InvalidLot`,
    /// `UnknownOrder`, `InvalidReduce`. O(1).
    pub fn reduce(&mut self, id: OrderId, qty: Qty, out: &mut Vec<Event>) {
        todo!("w07: reduce")
    }

    /// The order book (read-only: all mutation goes through commands).
    #[inline]
    pub fn book(&self) -> &B {
        todo!("w07: book")
    }

    /// The configuration the engine was built with.
    pub fn config(&self) -> &EngineConfig {
        todo!("w07: config")
    }

    /// Number of commands processed so far (rejected ones included) — the
    /// sequence number of the last command. Starts at `Seq(0)`.
    pub fn seq(&self) -> Seq {
        todo!("w07: seq")
    }

    /// Price of the most recent trade, if any.
    pub fn last_trade_price(&self) -> Option<Price> {
        todo!("w07: last_trade_price")
    }

    /// The most recent trades (see [`TradeTape`]).
    pub fn tape(&self) -> &TradeTape {
        todo!("w07: tape")
    }

    /// A deterministic 64-bit hash of the engine state that affects future
    /// output: the sequence number, the last trade price, and every resting
    /// order (side, price, id, remaining qty) in priority order. Two engines
    /// fed the same commands have equal hashes; a journal replay (w09) or a
    /// snapshot restore (w18) must reproduce it. O(resting orders).
    pub fn state_hash(&self) -> u64 {
        todo!("w07: state_hash")
    }
}

#[allow(unused_variables)]
impl<B: OrderBook> Engine<B> {
    /// Checks a new order against the instrument spec and the current book
    /// **without changing anything**. Returns the first failing check, in
    /// the documented order: `ZeroQuantity`, `InvalidLot`, `InvalidTick`,
    /// `PriceBand`, `DuplicateOrderId`, `WouldTake`, `NoLiquidity`,
    /// `FokUnfillable`.
    ///
    /// Cost: O(1) plus one id lookup, plus the FOK walk for FOK orders.
    pub fn validate(&self, order: &NewOrder) -> Result<(), RejectReason> {
        todo!("w08: validate")
    }

    /// The band reference price: the last trade, or the configured
    /// reference before the first trade.
    pub fn reference_price(&self) -> Option<Price> {
        todo!("w08: reference_price")
    }

    /// How much a `side` taker with limit `limit` (`None` = market) could
    /// fill right now, counting at most `want`: the fill-or-kill feasibility
    /// check. Read-only; walks the opposite side from the best level and
    /// stops at the first level that does not cross or once `want` is
    /// reached. O(levels walked), no allocation.
    pub fn fillable(&self, side: Side, limit: Option<Price>, want: Qty) -> Qty {
        todo!("w08: fillable")
    }
}

// TODO(w26): implement — see the week guide on tepy.dev/hft.

// w18 snapshot / restore: a child module, because a restore sets the
// private fields above (`engine/restore.rs`).
mod restore;
