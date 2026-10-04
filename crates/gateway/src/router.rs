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
// @student w13
#[derive(Debug)]
pub struct OrderRouter {
    next_id: u64,
    by_client: HashMap<(SessionId, OrderId), OrderId>,
    by_exchange: IdMap<OrderId, Route>,
}

// @student w13
impl OrderRouter {
    /// An empty router; the first exchange id is `1`.
    pub fn new() -> OrderRouter {
        OrderRouter::starting_at(1)
    }

    /// An empty router whose first exchange id is `first` (after a restart,
    /// one past the last id the journal assigned).
    pub fn starting_at(first: u64) -> OrderRouter {
        OrderRouter {
            next_id: first,
            by_client: HashMap::new(),
            by_exchange: IdMap::default(),
        }
    }

    /// The exchange id the next [`insert`](Self::insert) will assign.
    pub fn next_id(&self) -> OrderId {
        OrderId(self.next_id)
    }

    /// The exchange id of `session`'s live order `client_id`. O(1).
    pub fn lookup(&self, session: SessionId, client_id: OrderId) -> Option<OrderId> {
        self.by_client.get(&(session, client_id)).copied()
    }

    /// Owner and state of a live order, by exchange id. O(1).
    pub fn route(&self, id: OrderId) -> Option<&Route> {
        self.by_exchange.get(&id)
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
        let key = (session, client_id);
        if self.by_client.contains_key(&key) {
            return Err(RejectReason::DuplicateOrderId);
        }
        let id = OrderId(self.next_id);
        self.next_id += 1;
        self.by_client.insert(key, id);
        self.by_exchange.insert(
            id,
            Route {
                session,
                client_id,
                instrument,
                remaining: qty,
            },
        );
        Ok(id)
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
        let new_id = match cmd {
            Command::New(o) => Some(o.id),
            _ => None,
        };
        for ev in events {
            match *ev {
                Event::Accepted { id } => {
                    if let Some(r) = self.by_exchange.get(&id) {
                        deliver(r.session, Event::Accepted { id: r.client_id });
                    }
                }
                Event::Rejected { id, reason } => {
                    if let Some(r) = self.by_exchange.get(&id).copied() {
                        deliver(
                            r.session,
                            Event::Rejected {
                                id: r.client_id,
                                reason,
                            },
                        );
                        // A rejected new order never lived; a cancel/reduce
                        // reject leaves the order alone unless the engine
                        // says it is gone.
                        if new_id == Some(id) || reason == RejectReason::UnknownOrder {
                            self.forget(id);
                        }
                    }
                }
                Event::Trade {
                    taker,
                    maker,
                    taker_side,
                    price,
                    qty,
                } => {
                    let t = self.fill(taker, qty);
                    let m = self.fill(maker, qty);
                    match (t, m) {
                        (Some(t), Some(m)) if t.session == m.session => deliver(
                            t.session,
                            Event::Trade {
                                taker: t.client_id,
                                maker: m.client_id,
                                taker_side,
                                price,
                                qty,
                            },
                        ),
                        (t, m) => {
                            if let Some(t) = t {
                                deliver(
                                    t.session,
                                    Event::Trade {
                                        taker: t.client_id,
                                        maker: OrderId(0),
                                        taker_side,
                                        price,
                                        qty,
                                    },
                                );
                            }
                            if let Some(m) = m {
                                deliver(
                                    m.session,
                                    Event::Trade {
                                        taker: OrderId(0),
                                        maker: m.client_id,
                                        taker_side,
                                        price,
                                        qty,
                                    },
                                );
                            }
                        }
                    }
                    for id in [taker, maker] {
                        if self
                            .by_exchange
                            .get(&id)
                            .is_some_and(|r| r.remaining.is_zero())
                        {
                            self.forget(id);
                        }
                    }
                }
                Event::Rested {
                    id,
                    side,
                    price,
                    qty,
                } => {
                    if let Some(r) = self.by_exchange.get_mut(&id) {
                        r.remaining = qty;
                        deliver(
                            r.session,
                            Event::Rested {
                                id: r.client_id,
                                side,
                                price,
                                qty,
                            },
                        );
                    }
                }
                Event::Cancelled { id, remaining } => {
                    if let Some(r) = self.forget(id) {
                        deliver(
                            r.session,
                            Event::Cancelled {
                                id: r.client_id,
                                remaining,
                            },
                        );
                    }
                }
                Event::Expired { id, remaining } => {
                    if let Some(r) = self.forget(id) {
                        deliver(
                            r.session,
                            Event::Expired {
                                id: r.client_id,
                                remaining,
                            },
                        );
                    }
                }
                Event::Reduced { id, remaining } => {
                    if let Some(r) = self.by_exchange.get_mut(&id) {
                        r.remaining = remaining;
                        let r = *r;
                        deliver(
                            r.session,
                            Event::Reduced {
                                id: r.client_id,
                                remaining,
                            },
                        );
                        if remaining.is_zero() {
                            self.forget(id);
                        }
                    }
                }
            }
        }
    }

    /// Exchange ids of `session`'s live orders, ascending (the order in
    /// which cancel-on-disconnect cancels them, so a replay cancels in the
    /// same order). O(live orders); allocates the result.
    pub fn open_orders(&self, session: SessionId) -> Vec<OrderId> {
        let mut ids: Vec<OrderId> = self
            .by_exchange
            .iter()
            .filter(|(_, r)| r.session == session)
            .map(|(&id, _)| id)
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Number of live orders.
    pub fn len(&self) -> usize {
        self.by_exchange.len()
    }

    /// True if no order is live.
    pub fn is_empty(&self) -> bool {
        self.by_exchange.is_empty()
    }
}

// @student-begin w13
impl OrderRouter {
    /// Charges a fill to a live order; returns its (updated) route.
    fn fill(&mut self, id: OrderId, qty: Qty) -> Option<Route> {
        let r = self.by_exchange.get_mut(&id)?;
        r.remaining = r.remaining.saturating_sub(qty);
        Some(*r)
    }

    /// Removes a live order from both maps.
    fn forget(&mut self, id: OrderId) -> Option<Route> {
        let r = self.by_exchange.remove(&id)?;
        self.by_client.remove(&(r.session, r.client_id));
        Some(r)
    }
}
// @student-end

impl Default for OrderRouter {
    fn default() -> Self {
        OrderRouter::new()
    }
}
