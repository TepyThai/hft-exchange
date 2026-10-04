//! Matching engine: price-time priority, order types, validation, property tests, sharding, specialization.
//!
//! Curriculum: Weeks 7, 8, 12, 26, 36, 37, 38, 40 (see https://tepy.dev/hft).
//!
//! [`Engine`] turns [`Command`](types::Command)s into
//! [`Event`](types::Event)s for one instrument, over any
//! [`OrderBook`](book::OrderBook). It is a deterministic state machine:
//! same commands in, same events and same [`Engine::state_hash`] out.
//!
//! ```
//! use engine::Engine;
//! use types::{Command, Event, InstrumentSpec, NewOrder, Side};
//!
//! let mut engine = Engine::new(InstrumentSpec::default());
//! let mut events = Vec::new();
//! engine.process(&Command::New(NewOrder::limit(1, Side::Sell, 101, 5)), &mut events);
//! events.clear();
//! engine.process(&Command::New(NewOrder::limit(2, Side::Buy, 102, 3)), &mut events);
//! assert!(matches!(events[1], Event::Trade { qty, .. } if qty.0 == 3));
//! ```
//!
//! # Event-order contract
//!
//! [`Engine::process`] **appends** the events of one command to the
//! caller's buffer, always in this order:
//!
//! | command | outcome | events, in order |
//! | --- | --- | --- |
//! | `New` | fails validation | `Rejected { id, reason }` — the only event; the book is unchanged |
//! | `New` | accepted | `Accepted { id }`, then zero or more `Trade`, then at most one `Rested` **or** `Expired` |
//! | `Cancel` | resting | `Cancelled { id, remaining }` |
//! | `Cancel` | not resting | `Rejected { id, UnknownOrder }` |
//! | `Reduce` | valid | `Reduced { id, remaining }` (`remaining` = the requested qty) |
//! | `Reduce` | invalid | `Rejected { id, reason }` |
//!
//! For an accepted new order:
//!
//! * **Trades** are emitted in execution order: best opposite price first,
//!   and oldest order first within a price. Each trade has
//!   `taker` = the incoming order, `maker` = the resting order,
//!   `taker_side` = the incoming side, `price` = **the maker's** resting
//!   price (the taker gets any price improvement), and
//!   `qty` = min(taker remaining, maker remaining).
//! * If nothing remains after the trades, there is **no further event**: the
//!   last `Trade` completes the order.
//! * If quantity remains: a GTC limit order rests —
//!   `Rested { id, side, price: its limit, qty: remaining }`; an IOC limit
//!   order or any market order ends with `Expired { id, remaining }`. (FOK
//!   never expires: it is rejected up front unless it fills completely.)
//! * Makers get no event of their own: a maker is done when its trades add
//!   up to its resting quantity (consumers track this from `Rested` and
//!   `Trade`; [`book::OrderBook::get`] shows what is left).
//! * Market orders never rest, whatever their time in force: any remainder
//!   is `Expired` (FOK market orders are all-or-nothing).
//! * Post-only + IOC is accepted and then expires untouched; post-only +
//!   FOK is always rejected (`WouldTake` or `FokUnfillable`).
//!
//! # Validation order
//!
//! A new order is rejected with the **first** failing check, in this order
//! (static checks on the message first, then checks against book state):
//!
//! 1. `ZeroQuantity` — `qty == 0`
//! 2. `InvalidLot` — `qty` is not a multiple of `spec.lot`
//! 3. `InvalidTick` — a limit price that is not a multiple of `spec.tick`
//! 4. `PriceBand` — a limit price more than `band_ticks * tick` away from
//!    the reference price (the last trade, else
//!    [`EngineConfig::reference_price`]); skipped when `band_ticks == 0` or
//!    there is no reference yet. Market orders have no price to check.
//! 5. `DuplicateOrderId` — an order with this id is resting. (Ids of orders
//!    that are no longer live are not remembered — that would need
//!    unbounded memory; the sequencer guarantees uniqueness.)
//! 6. `WouldTake` — a post-only limit that crosses the best opposite price
//! 7. `NoLiquidity` — a market order with an empty opposite side
//! 8. `FokUnfillable` — a FOK order whose limit cannot fill its full
//!    quantity now (checked by [`Engine::fillable`], without touching the
//!    book)
//!
//! `Reduce` checks `ZeroQuantity`, `InvalidLot`, `UnknownOrder`, then
//! `InvalidReduce` (new qty not below the remaining qty). `Cancel` checks
//! `UnknownOrder`. `Risk(..)` and `Halted` come from layers in front of the
//! engine (w15). Prices are in integer price units; `spec.tick` is the
//! instrument's tick size in those units.
//!
//! # Allocation
//!
//! Events go into the caller's `Vec` (reuse it and clear it between
//! commands); the trade tape is a fixed ring. Once the book has grown to its
//! peak size, a command allocates nothing, except a `BTreeMap` node when the
//! w06 book opens a new price level (w27 removes that).
//!
//! # Weeks
//!
//! * w07 — [`Engine`]: matching, events, cancel, reduce, state hash.
//! * w08 — [`Engine::validate`], [`Engine::fillable`]: TIF, post-only,
//!   tick/lot/band validation.
//! * w12 — [`testkit`]: order-stream generator, invariant checker, naive
//!   reference matcher, differential testing and shrinking.

mod engine;
mod hash;
mod snapshot;
mod tape;
pub mod testkit;

pub use engine::{Engine, EngineConfig};
pub use hash::StateHasher;
pub use snapshot::BookSnapshot;
pub use tape::{TradePrint, TradeTape};

pub use book::{Book, LevelInfo, OrderBook, RestingOrder};
