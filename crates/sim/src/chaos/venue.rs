//! Scenario 4 — chaos at the edge of the week 23 scenario runner: a
//! [`Venue`] wrapper that damages what the exchange sends back.
//!
//! ```text
//!  Runner ── send ──────────────────────────────► inner Venue (risk → engine → md)
//!    ▲                                                   │ flush
//!    │◄─ market data ── ChaosLink<L2Update> (UDP) ◄─────┤
//!    │◄─ exec reports ── order sessions (TCP)  ◄────────┘
//!         per account: numbered, kept for resend; the plan resets and stalls them
//! ```
//!
//! The two kinds of traffic fail differently, as on a real exchange:
//!
//! * **Market data** is UDP multicast: updates are lost, duplicated,
//!   reordered and delayed one by one ([`ChaosLink`]). The runner's book
//!   builder detects the gap and resyncs from
//!   [`market_snapshot`](Venue::market_snapshot) (week 23).
//! * **Execution reports** travel on an order session — a TCP stream. TCP
//!   never loses, duplicates or reorders a message in the middle of a
//!   stream; its failures are **resets** and **stalls**. So the plan's
//!   faults on [`Link::ExecReports`] become: a lost or damaged report
//!   resets the session (that report and everything after it stay
//!   undelivered until the client reconnects, at the next flush, and the
//!   venue resends from the first report the client lacks); a delay
//!   stalls the stream (head-of-line blocking: everything behind waits); a
//!   duplicate is a resend the client already has, which it drops by
//!   sequence number. Each account's reports therefore reach the runner
//!   **exactly once and in order**, late at worst.
//!
//! What the scenario asserts: agents and the runner survive; after every
//! step each agent has received exactly a prefix of its true reports, and
//! the runner's book, when synced, is the true book at its sequence
//! number; once the faults stop and everything in flight is delivered,
//! positions and cash are conserved across agents and agree with
//! pre-trade risk.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use super::inject::{ChaosLink, Injector};
use super::plan::{FaultEvent, FaultPlan, Link};
use crate::scenario::{Delivery, Venue};
use marketdata::{L2Snapshot, L2Update};
use types::{AccountId, Command};

/// Counters of the order sessions of a [`ChaosVenue`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SessionStats {
    /// Execution reports produced by the inner venue.
    pub reports: u64,
    /// Reports delivered to the runner (each exactly once).
    pub delivered: u64,
    /// Transmissions, including resends and duplicates.
    pub transmissions: u64,
    /// Session resets (a lost or damaged report).
    pub resets: u64,
    /// Stalls (a delayed report).
    pub stalls: u64,
    /// Copies the client already had, dropped by sequence number.
    pub duplicates_dropped: u64,
}

/// A [`Venue`] that injects a [`FaultPlan`] into what `V` sends back (see
/// the [module docs](self)). Its clock is the number of
/// [`flush`](Venue::flush)es so far: flush `k` is step `k` of the plan.
///
/// On flush `k`: the inner venue is flushed; its execution reports are
/// appended, numbered, to their account's session; then every session, in
/// account order, unless it is down or stalled until a later flush,
/// transmits its undelivered reports in order, each judged by the
/// `exec` [`Injector`]:
///
/// * lost or corrupt → **reset**: nothing more is sent on that session
///   this flush; it reconnects at flush `k + 1` and resends from the first
///   report the client has not received;
/// * delayed `d` → **stall**: the report and those behind it wait until
///   flush `k + d`;
/// * otherwise delivered (twice if duplicated: the client drops the copy;
///   a reorder cannot happen inside a stream and is ignored).
///
/// Then the inner venue's market data goes through the `md`
/// [`ChaosLink`], and what is due at `k` is appended.
/// [`send`](Venue::send) and [`market_snapshot`](Venue::market_snapshot)
/// pass straight through.
///
/// Cost per flush: the inner venue's, plus O(log in-flight) per update and
/// O(1) per report transmission; reports are kept until the end of the
/// run (for resends).
pub struct ChaosVenue<V> {
    // TODO(w48): design this type's fields.
    _todo: std::marker::PhantomData<(V,)>,
}

// TODO(w48): implement — see the week guide on tepy.dev/hft.

#[allow(unused_variables)]
impl<V: Venue> ChaosVenue<V> {
    /// Wraps `inner` under `plan`.
    pub fn new(inner: V, plan: &FaultPlan) -> ChaosVenue<V> {
        todo!("w48: new")
    }

    /// The wrapped venue.
    pub fn inner(&self) -> &V {
        todo!("w48: inner")
    }

    /// The wrapped venue, mutably (limits, kill switches).
    pub fn inner_mut(&mut self) -> &mut V {
        todo!("w48: inner_mut")
    }

    /// Flushes so far (the plan's step clock).
    pub fn now(&self) -> u64 {
        todo!("w48: now")
    }

    /// Market-data updates and execution reports produced but not yet
    /// delivered.
    pub fn in_flight(&self) -> usize {
        todo!("w48: in_flight")
    }

    /// Session counters so far.
    pub fn stats(&self) -> SessionStats {
        todo!("w48: stats")
    }

    /// What the plan did so far: market data and execution reports, by
    /// flush.
    pub fn fault_log(&self) -> Vec<FaultEvent> {
        todo!("w48: fault_log")
    }
}

// TODO(w48): implement — see the week guide on tepy.dev/hft.

#[allow(unused_variables)]
impl<V: Venue> Venue for ChaosVenue<V> {
    fn send(&mut self, account: AccountId, cmd: &Command) {
        todo!("w48: send")
    }

    fn flush(&mut self, deliveries: &mut Vec<Delivery>, market_data: &mut Vec<L2Update>) {
        todo!("w48: flush")
    }

    fn market_snapshot(&mut self) -> L2Snapshot {
        todo!("w48: market_snapshot")
    }
}
