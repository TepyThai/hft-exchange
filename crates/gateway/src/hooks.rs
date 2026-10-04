//! Extension points of the engine thread: pre-trade checks (week 13 hook,
//! week 17 [`RiskGate`]) and pipeline stages (the journal and market-data
//! fan-out that week 24 inserts between the engine and the sessions).
//!
//! Both run on the engine thread, in sequence order, so they see one
//! consistent history and need no locks:
//!
//! ```text
//! session reader ─▶ MPSC ─▶ engine thread:
//!     id remap → PreTrade::check ─(reject)─────────────────────────┐
//!              → Stage::on_command → Engine::process               │
//!              → Stage::on_events → PreTrade::on_event             │
//!              → OrderRouter (client ids) ─▶ batch outbox ◀────────┘
//!     end of batch: Stage::flush ─▶ per-session queues ─▶ writers
//! ```

use crate::session::SessionId;
#[allow(unused_imports)] // used by the implementation the learner writes
use risk::{BucketConfig, Limits, MonotonicClock, RiskEngine, Throttle};
use types::{Command, Event, InstrumentId, NewOrder, Price, RejectReason};

/// A pre-trade check run on the engine thread before an order is
/// sequenced. Orders it rejects never reach the engine (or the journal);
/// the client gets `Rejected` with the returned reason.
pub trait PreTrade: Send {
    /// Checks `order` (exchange order id already assigned, account set by
    /// the session) received on `session` at `rx_ns` (nanoseconds on the
    /// gateway's monotonic clock). `reference` is the engine's reference
    /// price for the instrument (last trade, else the configured one).
    ///
    /// `Ok` means the order **will** be sent to the engine: record it as
    /// open (check-and-reserve), so the next check sees it.
    fn check(
        &mut self,
        session: SessionId,
        order: &NewOrder,
        rx_ns: u64,
        reference: Option<Price>,
    ) -> Result<(), RejectReason>;

    /// Every engine event, in exchange ids and engine order (fills, open
    /// orders released by cancels, expiries and rejects).
    fn on_event(&mut self, event: &Event) {
        let _ = event;
    }

    /// `session` disconnected (its throttle state can go).
    fn on_close(&mut self, session: SessionId) {
        let _ = session;
    }
}

/// Accepts every order: the week-13 default.
#[derive(Clone, Copy, Debug, Default)]
pub struct AllowAll;

impl PreTrade for AllowAll {
    fn check(
        &mut self,
        _: SessionId,
        _: &NewOrder,
        _: u64,
        _: Option<Price>,
    ) -> Result<(), RejectReason> {
        Ok(())
    }
}

/// A pipeline stage on the engine thread, between matching and the
/// sessions. Week 24 implements it with the journal (write-ahead in
/// [`on_command`](Stage::on_command), group commit in
/// [`flush`](Stage::flush)) and market-data fan-out (in
/// [`on_events`](Stage::on_events)).
pub trait Stage: Send {
    /// A command about to be applied to `instrument`'s engine, in sequence
    /// order, with exchange order ids. Pre-trade rejects never get here.
    fn on_command(&mut self, instrument: InstrumentId, cmd: &Command) {
        let _ = (instrument, cmd);
    }

    /// The events the engine produced for `cmd`, before they are routed to
    /// sessions.
    fn on_events(&mut self, instrument: InstrumentId, cmd: &Command, events: &[Event]) {
        let _ = (instrument, cmd, events);
    }

    /// End of an engine-thread batch: the batch's execution reports are
    /// released to the sessions when this returns (fsync here for
    /// durable-before-ack).
    fn flush(&mut self) {}
}

/// No stage: reports go straight to the sessions.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoStage;

impl Stage for NoStage {}

/// The engine thread's hooks. `Hooks::default()` is [`AllowAll`] +
/// [`NoStage`].
pub struct Hooks {
    /// Pre-trade check.
    pub pretrade: Box<dyn PreTrade>,
    /// Pipeline stage.
    pub stage: Box<dyn Stage>,
}

impl Default for Hooks {
    fn default() -> Self {
        Hooks {
            pretrade: Box::new(AllowAll),
            stage: Box::new(NoStage),
        }
    }
}

impl std::fmt::Debug for Hooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hooks").finish_non_exhaustive()
    }
}

/// Week 17: the exchange's pre-trade gate — per-session and per-account
/// token-bucket throttles ([`Throttle`]), then the week-15 [`RiskEngine`]
/// limits — as a [`PreTrade`] hook.
///
/// Throttle first: a client that floods is rejected with
/// `Risk(RateLimit)` before any limit arithmetic, and a throttled order
/// charges nothing. The throttle runs on the receive timestamp (`rx_ns`),
/// not the time the engine thread gets to the order, so queueing inside the
/// gateway never costs a client its budget.
// @student w17
pub struct RiskGate {
    risk: RiskEngine,
    throttle: Throttle<MonotonicClock>,
}

// @student w17
impl RiskGate {
    /// A gate applying `limits` to every account, `per_session` to each
    /// session and `per_account` to each account's sessions combined.
    pub fn new(limits: Limits, per_session: BucketConfig, per_account: BucketConfig) -> RiskGate {
        RiskGate {
            risk: RiskEngine::new(limits),
            throttle: Throttle::new(per_session, per_account, MonotonicClock::new()),
        }
    }

    /// The risk engine (per-account limits, kill switches, exposures).
    pub fn risk(&self) -> &RiskEngine {
        &self.risk
    }

    /// Mutable access to the risk engine (set limits, kill an account).
    pub fn risk_mut(&mut self) -> &mut RiskEngine {
        &mut self.risk
    }

    /// Mutable access to the throttle (per-account limits).
    pub fn throttle_mut(&mut self) -> &mut Throttle<MonotonicClock> {
        &mut self.throttle
    }
}

// @student w17
impl PreTrade for RiskGate {
    /// Throttle, then limits, then record the order as open. A market
    /// order with no reference price is valued at 0 (only the quantity and
    /// position checks bind until the first trade). O(1), no allocation on
    /// the hot path.
    fn check(
        &mut self,
        session: SessionId,
        order: &NewOrder,
        rx_ns: u64,
        reference: Option<Price>,
    ) -> Result<(), RejectReason> {
        self.throttle
            .check_at(session, order.account, rx_ns)
            .map_err(RejectReason::Risk)?;
        let reference = reference.or(order.limit_price()).unwrap_or(Price(0));
        self.risk.check(order, reference)?;
        self.risk.on_new(order);
        Ok(())
    }

    fn on_event(&mut self, event: &Event) {
        self.risk.on_event(event);
    }

    fn on_close(&mut self, session: SessionId) {
        self.throttle.remove_session(session);
    }
}
