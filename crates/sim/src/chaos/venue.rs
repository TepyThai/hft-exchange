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
// @student w48
pub struct ChaosVenue<V> {
    inner: V,
    now: u64,
    md: ChaosLink<L2Update>,
    exec: Injector,
    /// Indexed by account number.
    sessions: Vec<Session>,
    stats: SessionStats,
    deliveries: Vec<Delivery>,
    updates: Vec<L2Update>,
}

// @student-begin w48
/// One account's order session.
#[derive(Clone, Debug, Default)]
struct Session {
    /// Every report for the account; report `n` (from 1) is `sent[n - 1]`.
    sent: Vec<Delivery>,
    /// Reports the client has received (= the next one it expects − 1).
    received: usize,
    /// Nothing is transmitted before this flush (down or stalled).
    blocked_until: u64,
}
// @student-end

// @student w48
impl<V: Venue> ChaosVenue<V> {
    /// Wraps `inner` under `plan`.
    pub fn new(inner: V, plan: &FaultPlan) -> ChaosVenue<V> {
        ChaosVenue {
            inner,
            now: 0,
            md: ChaosLink::new(plan, Link::MarketData),
            exec: Injector::new(plan, Link::ExecReports),
            sessions: Vec::new(),
            stats: SessionStats::default(),
            deliveries: Vec::with_capacity(64),
            updates: Vec::with_capacity(64),
        }
    }

    /// The wrapped venue.
    pub fn inner(&self) -> &V {
        &self.inner
    }

    /// The wrapped venue, mutably (limits, kill switches).
    pub fn inner_mut(&mut self) -> &mut V {
        &mut self.inner
    }

    /// Flushes so far (the plan's step clock).
    pub fn now(&self) -> u64 {
        self.now
    }

    /// Market-data updates and execution reports produced but not yet
    /// delivered.
    pub fn in_flight(&self) -> usize {
        let reports: usize = self
            .sessions
            .iter()
            .map(|s| s.sent.len() - s.received)
            .sum();
        self.md.in_flight() + reports
    }

    /// Session counters so far.
    pub fn stats(&self) -> SessionStats {
        self.stats
    }

    /// What the plan did so far: market data and execution reports, by
    /// flush.
    pub fn fault_log(&self) -> Vec<FaultEvent> {
        let mut log: Vec<FaultEvent> = self
            .md
            .injector()
            .log()
            .iter()
            .chain(self.exec.log())
            .copied()
            .collect();
        log.sort_by_key(FaultEvent::step);
        log
    }
}

// @student-begin w48
impl<V: Venue> ChaosVenue<V> {
    /// Transmits session `i`'s undelivered reports at flush `k`.
    fn transmit(&mut self, i: usize, k: u64, out: &mut Vec<Delivery>) {
        let s = &mut self.sessions[i];
        if k < s.blocked_until {
            return;
        }
        while s.received < s.sent.len() {
            let fate = self.exec.fate(k);
            self.stats.transmissions += 1;
            if fate.is_lost() || fate.corrupt.is_some() {
                self.stats.resets += 1;
                s.blocked_until = k + 1;
                return;
            }
            if fate.delay > 0 {
                self.stats.stalls += 1;
                s.blocked_until = k + fate.delay;
                return;
            }
            out.push(s.sent[s.received]);
            s.received += 1;
            self.stats.delivered += 1;
            if fate.duplicate {
                self.stats.transmissions += 1;
                self.stats.duplicates_dropped += 1;
            }
        }
    }
}
// @student-end

// @student w48
impl<V: Venue> Venue for ChaosVenue<V> {
    fn send(&mut self, account: AccountId, cmd: &Command) {
        self.inner.send(account, cmd);
    }

    fn flush(&mut self, deliveries: &mut Vec<Delivery>, market_data: &mut Vec<L2Update>) {
        let k = self.now;
        self.now += 1;
        self.inner.flush(&mut self.deliveries, &mut self.updates);
        for d in self.deliveries.drain(..) {
            let i = d.account.0 as usize;
            if self.sessions.len() <= i {
                self.sessions.resize_with(i + 1, Session::default);
            }
            self.sessions[i].sent.push(d);
            self.stats.reports += 1;
        }
        for i in 0..self.sessions.len() {
            self.transmit(i, k, deliveries);
        }
        for u in self.updates.drain(..) {
            self.md.send(k, u);
        }
        self.md.recv(k, market_data);
    }

    fn market_snapshot(&mut self) -> L2Snapshot {
        self.inner.market_snapshot()
    }
}
