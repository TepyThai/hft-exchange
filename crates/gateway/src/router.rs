//! Week 13: client order ids ↔ exchange order ids, and routing execution
//! reports back to the session that owns each order.
//!
//! Clients choose their own order ids (like FIX's `ClOrdID`), unique only
//! within their session: two sessions may both send order `1`. The engine
//! needs ids unique across the whole exchange. The [`OrderRouter`] is the one
//! place that maps between the two:
//!
//! * a new order gets the next exchange id (`1, 2, 3, …` in sequence order,
//!   so a replay of the same commands assigns the same ids);
//! * a cancel or reduce names the client id, which the router resolves
//!   within the sending session only — a session cannot touch another
//!   session's orders, whatever ids it sends;
//! * every engine event is translated back to the owner's client id and
//!   delivered to the owner's session. For a trade, each side learns only
//!   its own order id: the counterparty's slot in the report is `0`
//!   (exchanges don't disclose who you traded with), unless both orders
//!   belong to the same session.
//!
//! The router tracks each order's remaining quantity from the events, so it
//! forgets an order exactly when the order is done (fully filled,
//! cancelled, expired or rejected); the client may then reuse its id.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::session::SessionId;
#[allow(unused_imports)] // used by the implementation the learner writes
use risk::IdMap;
#[allow(unused_imports)] // used by the implementation the learner writes
use std::collections::HashMap;
#[allow(unused_imports)] // used by the implementation the learner writes
use types::{Command, Event, InstrumentId, OrderId, Qty, RejectReason};

/// Where a live order came from, as the router tracks it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Route {
    /// The session that sent the order (and receives its reports).
    pub session: SessionId,
    /// The id the client chose.
    pub client_id: OrderId,
    /// The instrument (engine) the order lives in.
    pub instrument: InstrumentId,
    /// Quantity not yet filled, cancelled or expired.
    pub remaining: Qty,
}

/// Maps `(session, client id)` ↔ exchange id for every live order.
///
/// The client-keyed map uses std's SipHash: its keys are chosen by clients,
/// and a fast non-keyed hash would let one client craft colliding ids. The
/// exchange-keyed map uses [`risk::IdMap`] (ids the exchange assigns itself).
#[derive(Debug)]
pub struct OrderRouter {
    // TODO(w13): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl OrderRouter {
    /// An empty router; the first exchange id is `1`.
    pub fn new() -> OrderRouter {
        todo!("w13: new")
    }

    /// An empty router whose first exchange id is `first` (after a restart,
    /// one past the last id the journal assigned).
    pub fn starting_at(first: u64) -> OrderRouter {
        todo!("w13: starting_at")
    }

    /// The exchange id the next [`insert`](Self::insert) will assign.
    pub fn next_id(&self) -> OrderId {
        todo!("w13: next_id")
    }

    /// The exchange id of `session`'s live order `client_id`. O(1).
    pub fn lookup(&self, session: SessionId, client_id: OrderId) -> Option<OrderId> {
        todo!("w13: lookup")
    }

    /// Owner and state of a live order, by exchange id. O(1).
    pub fn route(&self, id: OrderId) -> Option<&Route> {
        todo!("w13: route")
    }

    /// Registers a new order and returns its exchange id (the current
    /// [`next_id`](Self::next_id)). `Err(DuplicateOrderId)` if the session
    /// already has a live order with this client id; nothing changes then.
    /// O(1) amortized; allocates only when a map grows.
    pub fn insert(
        &mut self,
        session: SessionId,
        client_id: OrderId,
        instrument: InstrumentId,
        qty: Qty,
    ) -> Result<OrderId, RejectReason> {
        todo!("w13: insert")
    }

    /// Applies the events the engine produced for `cmd` (both in exchange
    /// ids) and calls `deliver(owner, event)` for each report, translated to
    /// the owner's client ids, in engine order. A trade between two
    /// sessions produces one report for each. Orders that reached a
    /// terminal state are forgotten. Events about unknown ids are skipped.
    /// O(events), no allocation.
    pub fn route_events<F: FnMut(SessionId, Event)>(
        &mut self,
        cmd: &Command,
        events: &[Event],
        deliver: &mut F,
    ) {
        todo!("w13: route_events")
    }

    /// Exchange ids of `session`'s live orders, ascending (the order in
    /// which cancel-on-disconnect cancels them, so a replay cancels in the
    /// same order). O(live orders); allocates the result.
    pub fn open_orders(&self, session: SessionId) -> Vec<OrderId> {
        todo!("w13: open_orders")
    }

    /// Number of live orders.
    pub fn len(&self) -> usize {
        todo!("w13: len")
    }

    /// True if no order is live.
    pub fn is_empty(&self) -> bool {
        todo!("w13: is_empty")
    }
}

// TODO(w13): implement — see the week guide on tepy.dev/hft.

impl Default for OrderRouter {
    fn default() -> Self {
        OrderRouter::new()
    }
}
