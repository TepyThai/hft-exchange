//! Week 38: hot path / cold path — the matching engine restructured so the
//! common case is a short straight line and everything rare is out of line.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::policy::{NoRisk, RiskPolicy, TapeRing};
#[allow(unused_imports)] // used by the reference implementation
use crate::tape::TradePrint;
use crate::EngineConfig;
#[allow(unused_imports)] // used by the reference implementation
use crate::StateHasher;
#[allow(unused_imports)] // used by the reference implementation
use book::RestingOrder;
use book::{Book, OrderBook};
#[allow(unused_imports)] // used by the reference implementation
use types::{
    Command, Event, NewOrder, OrderId, OrderKind, Price, Qty, RejectReason, Seq, Side, TimeInForce,
};

/// Number of distinct reject reasons [`FastEngine::reject_count`] tracks
/// (every [`RejectReason`]; all `Risk(_)` reasons share one counter).
pub const REJECT_KINDS: usize = 12;

/// The counter slot of a reject reason (`0..REJECT_KINDS`).
pub fn reject_slot(reason: RejectReason) -> usize {
    match reason {
        RejectReason::DuplicateOrderId => 0,
        RejectReason::UnknownOrder => 1,
        RejectReason::ZeroQuantity => 2,
        RejectReason::InvalidTick => 3,
        RejectReason::InvalidLot => 4,
        RejectReason::PriceBand => 5,
        RejectReason::WouldTake => 6,
        RejectReason::FokUnfillable => 7,
        RejectReason::NoLiquidity => 8,
        RejectReason::InvalidReduce => 9,
        RejectReason::Risk(_) => 10,
        RejectReason::Halted => 11,
    }
}

/// The w07/w08 engine with a pre-trade [`RiskPolicy`] built in,
/// restructured around its hot path:
///
/// * **Hot** (inlined, straight-line): a valid passive GTC limit — one
///   combined spec check, one look at the opposite best, one book `add`
///   (which is also the duplicate check); a valid aggressive limit, IOC or
///   market order — one duplicate probe, the matching loop, rest or expire;
///   a valid cancel or reduce.
/// * **Cold** (`#[cold]`, `#[inline(never)]`, out of the hot function's
///   instruction stream): every reject — the precise reason is worked out
///   only once we know there is one — fill-or-kill (the depth walk),
///   post-only orders that would take, market orders with no liquidity, and
///   the reject counters (the "logging" of this engine, see
///   [`reject_count`](FastEngine::reject_count)).
///
/// Behaviour is exactly that of [`Guarded<B, R>`](crate::Guarded) — itself
/// [`Engine`](crate::Engine) plus the policy — event for event; the w38
/// tests check it differentially. The trade tape is a
/// [`TapeRing<TAPE>`](TapeRing) (const-generic capacity, w37). Costs are the
/// engine's; nothing allocates except `out` growing.
#[derive(Clone, Debug)]
pub struct FastEngine<B: OrderBook = Book, R: RiskPolicy = NoRisk, const TAPE: usize = 64> {
    // TODO(w38): design this type's fields.
    _todo: std::marker::PhantomData<(B, R,)>,
}

#[allow(unused_variables)]
impl<B: OrderBook, R: RiskPolicy, const TAPE: usize> FastEngine<B, R, TAPE> {
    /// An engine over `book` (normally empty) behind `risk`.
    ///
    /// # Panics
    /// If `config.spec.tick` or `config.spec.lot` is not positive.
    pub fn new(config: EngineConfig, book: B, risk: R) -> FastEngine<B, R, TAPE> {
        todo!("w38: new")
    }

    /// Processes one command, **appending** its events to `out` — the same
    /// events, in the same order, as [`Guarded::process`](crate::Guarded::process).
    #[inline]
    pub fn process(&mut self, cmd: &Command, out: &mut Vec<Event>) {
        todo!("w38: process")
    }

    #[inline]
    fn submit(&mut self, order: &NewOrder, out: &mut Vec<Event>) {
        todo!("w38: submit")
    }

    /// The passive fast path: a GTC limit that does not cross rests at once;
    /// `add` is also the duplicate check (one id probe).
    #[inline]
    fn rest_passive(&mut self, order: &NewOrder, price: Price, out: &mut Vec<Event>) {
        todo!("w38: rest_passive")
    }

    /// Matches `order` against the opposite side up to `limit` (`None`:
    /// market), appending trades; returns the unfilled quantity.
    #[inline]
    fn take(&mut self, order: &NewOrder, limit: Option<Price>, out: &mut Vec<Event>) -> Qty {
        todo!("w38: take")
    }

    /// After matching a limit order: rest a GTC remainder, expire an IOC one.
    #[inline]
    fn finish(&mut self, order: &NewOrder, price: Price, remaining: Qty, out: &mut Vec<Event>) {
        todo!("w38: finish")
    }

    /// True if the order passes every message-only check (lot, tick, band,
    /// non-zero) — one combined test, no reason computed.
    #[inline]
    fn spec_ok(&self, order: &NewOrder) -> bool {
        todo!("w38: spec_ok")
    }

    #[inline]
    fn in_band(&self, price: Price) -> bool {
        todo!("w38: in_band")
    }

    // ---------------------------------------------------------------------
    // Cold paths.

    /// Records and emits a reject (the only place the counters are touched).
    #[cold]
    #[inline(never)]
    fn reject(&mut self, id: OrderId, reason: RejectReason, out: &mut Vec<Event>) {
        todo!("w38: reject")
    }

    /// The order failed `spec_ok`: find the first failing check, in the
    /// documented order.
    #[cold]
    #[inline(never)]
    fn reject_spec(&mut self, order: &NewOrder, out: &mut Vec<Event>) {
        todo!("w38: reject_spec")
    }

    /// Everything rare: fill-or-kill orders and post-only orders that would
    /// take. Runs the full book validation (in the documented order), then
    /// matches like the hot path.
    #[cold]
    #[inline(never)]
    fn submit_rare(&mut self, order: &NewOrder, out: &mut Vec<Event>) {
        todo!("w38: submit_rare")
    }

    /// The fill-or-kill feasibility walk (as [`Engine::fillable`](crate::Engine::fillable)).
    #[cold]
    #[inline(never)]
    fn fillable(&self, side: Side, limit: Option<Price>, want: Qty) -> Qty {
        todo!("w38: fillable")
    }

    #[inline]
    fn cancel(&mut self, id: OrderId, out: &mut Vec<Event>) {
        todo!("w38: cancel")
    }

    #[inline]
    fn reduce(&mut self, id: OrderId, qty: Qty, out: &mut Vec<Event>) {
        todo!("w38: reduce")
    }

    /// The band reference: last trade, else the configured reference.
    #[inline]
    pub fn reference_price(&self) -> Option<Price> {
        todo!("w38: reference_price")
    }

    /// How many commands were rejected with `reason` so far (risk rejects
    /// all count under any `Risk(_)`). O(1).
    pub fn reject_count(&self, reason: RejectReason) -> u64 {
        todo!("w38: reject_count")
    }

    /// The order book (read-only).
    pub fn book(&self) -> &B {
        todo!("w38: book")
    }

    /// The risk policy.
    pub fn risk(&self) -> &R {
        todo!("w38: risk")
    }

    /// The configuration.
    pub fn config(&self) -> &EngineConfig {
        todo!("w38: config")
    }

    /// Commands the engine sequenced (risk rejects excluded, like
    /// [`Guarded`](crate::Guarded)).
    pub fn seq(&self) -> Seq {
        todo!("w38: seq")
    }

    /// Price of the most recent trade.
    pub fn last_trade_price(&self) -> Option<Price> {
        todo!("w38: last_trade_price")
    }

    /// The last `TAPE` trades.
    pub fn tape(&self) -> &TapeRing<TAPE> {
        todo!("w38: tape")
    }

    /// The same hash as [`Engine::state_hash`](crate::Engine::state_hash)
    /// for the same state. O(resting orders).
    pub fn state_hash(&self) -> u64 {
        todo!("w38: state_hash")
    }
}

impl<B: OrderBook, R: RiskPolicy, const TAPE: usize> crate::testkit::Matcher
    for FastEngine<B, R, TAPE>
{
    fn process(&mut self, cmd: &Command, out: &mut Vec<Event>) {
        FastEngine::process(self, cmd, out);
    }

    fn resting(&self, out: &mut Vec<RestingOrder>) {
        out.clear();
        for side in [Side::Buy, Side::Sell] {
            self.book().visit_orders(side, &mut |o| {
                out.push(o);
                true
            });
        }
    }
}
