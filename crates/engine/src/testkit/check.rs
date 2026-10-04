//! Invariant checking over a command / event stream.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
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

// TODO(w12): implement — see the week guide on tepy.dev/hft.

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
#[derive(Clone, Debug, Default)]
pub struct InvariantChecker {
    // TODO(w12): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl InvariantChecker {
    /// A checker for an engine that has processed nothing yet.
    pub fn new() -> InvariantChecker {
        todo!("w12: new")
    }

    /// Checks one command's `events` and the `book` after it.
    pub fn check<B: OrderBook>(
        &mut self,
        cmd: &Command,
        events: &[Event],
        book: &B,
    ) -> Result<(), Violation> {
        todo!("w12: check")
    }

    /// Quantity totals so far.
    pub fn totals(&self) -> Totals {
        todo!("w12: totals")
    }

    /// Number of commands checked so far.
    pub fn commands(&self) -> u64 {
        todo!("w12: commands")
    }
}
