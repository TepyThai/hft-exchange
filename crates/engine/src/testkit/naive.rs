//! A deliberately naive reference matcher.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::EngineConfig;
use book::RestingOrder;
#[allow(unused_imports)] // used by the reference implementation
use types::{
    Command, Event, NewOrder, OrderId, OrderKind, Price, Qty, RejectReason, Side, TimeInForce,
};

// TODO(w12): implement — see the week guide on tepy.dev/hft.

/// The slowest correct matcher we could write: every resting order in one
/// `Vec`, scanned in full for every match step. Same semantics, event order
/// and validation order as [`Engine`](crate::Engine) — written
/// independently, so that agreement between the two over thousands of
/// random streams means something. O(n) per operation; for tests only.
#[derive(Clone, Debug)]
pub struct NaiveEngine {
    // TODO(w12): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl NaiveEngine {
    /// An empty reference matcher with the engine's configuration (spec and
    /// band reference price).
    pub fn new(config: EngineConfig) -> NaiveEngine {
        todo!("w12: new")
    }

    /// Processes one command, appending its events to `out` exactly as
    /// [`Engine::process`](crate::Engine::process) documents.
    pub fn process(&mut self, cmd: &Command, out: &mut Vec<Event>) {
        todo!("w12: process")
    }

    /// Clears `out` and fills it with every resting order: bids best first,
    /// then asks best first, oldest first within a price.
    pub fn resting(&self, out: &mut Vec<RestingOrder>) {
        todo!("w12: resting")
    }

    /// Price of the most recent trade, if any.
    pub fn last_trade_price(&self) -> Option<Price> {
        todo!("w12: last_trade_price")
    }
}
