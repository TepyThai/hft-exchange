//! Week 38: hot path / cold path — the matching engine restructured so the
//! common case is a short straight line and everything rare is out of line.

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
// @student w38
#[derive(Clone, Debug)]
pub struct FastEngine<B: OrderBook = Book, R: RiskPolicy = NoRisk, const TAPE: usize = 64> {
    book: B,
    config: EngineConfig,
    risk: R,
    seq: u64,
    last_trade: Option<Price>,
    tape: TapeRing<TAPE>,
    /// Hot-path shortcuts: with a tick or lot of 1 the spec check skips
    /// the division.
    unit_tick: bool,
    unit_lot: bool,
    rejects: [u64; REJECT_KINDS],
}

// @student w38
impl<B: OrderBook, R: RiskPolicy, const TAPE: usize> FastEngine<B, R, TAPE> {
    /// An engine over `book` (normally empty) behind `risk`.
    ///
    /// # Panics
    /// If `config.spec.tick` or `config.spec.lot` is not positive.
    pub fn new(config: EngineConfig, book: B, risk: R) -> FastEngine<B, R, TAPE> {
        assert!(config.spec.tick > 0, "tick size must be positive");
        assert!(config.spec.lot > 0, "lot size must be positive");
        FastEngine {
            book,
            config,
            risk,
            seq: 0,
            last_trade: None,
            tape: TapeRing::new(),
            unit_tick: config.spec.tick == 1,
            unit_lot: config.spec.lot == 1,
            rejects: [0; REJECT_KINDS],
        }
    }

    /// Processes one command, **appending** its events to `out` — the same
    /// events, in the same order, as [`Guarded::process`](crate::Guarded::process).
    #[inline]
    pub fn process(&mut self, cmd: &Command, out: &mut Vec<Event>) {
        let start = out.len();
        match *cmd {
            Command::New(ref order) => {
                let reference = self
                    .reference_price()
                    .or(order.limit_price())
                    .unwrap_or_default();
                if let Err(reason) = self.risk.check(order, reference) {
                    // Never sequenced, and the policy is not shown its own reject.
                    return self.reject(order.id, reason, out);
                }
                self.risk.on_accept(order);
                self.submit(order, out);
            }
            Command::Cancel { id } => self.cancel(id, out),
            Command::Reduce { id, qty } => self.reduce(id, qty, out),
        }
        for event in &out[start..] {
            self.risk.on_event(event);
        }
    }

    #[inline]
    fn submit(&mut self, order: &NewOrder, out: &mut Vec<Event>) {
        self.seq += 1;
        if !self.spec_ok(order) {
            return self.reject_spec(order, out);
        }
        let opposite = self.book.best(order.side.opposite());
        match (order.kind, order.tif) {
            (OrderKind::Limit { price, post_only }, tif) if tif != TimeInForce::Fok => {
                let crosses = opposite.is_some_and(|b| order.side.crosses(price, b.price));
                if !crosses {
                    if tif == TimeInForce::Gtc {
                        return self.rest_passive(order, price, out);
                    }
                } else if post_only {
                    return self.submit_rare(order, out);
                }
                if self.book.contains(order.id) {
                    return self.reject(order.id, RejectReason::DuplicateOrderId, out);
                }
                out.push(Event::Accepted { id: order.id });
                let remaining = self.take(order, Some(price), out);
                self.finish(order, price, remaining, out);
            }
            (OrderKind::Market, tif) if tif != TimeInForce::Fok => {
                if self.book.contains(order.id) {
                    return self.reject(order.id, RejectReason::DuplicateOrderId, out);
                }
                if opposite.is_none() {
                    return self.reject(order.id, RejectReason::NoLiquidity, out);
                }
                out.push(Event::Accepted { id: order.id });
                let remaining = self.take(order, None, out);
                if !remaining.is_zero() {
                    out.push(Event::Expired {
                        id: order.id,
                        remaining,
                    });
                }
            }
            _ => self.submit_rare(order, out),
        }
    }

    /// The passive fast path: a GTC limit that does not cross rests at once;
    /// `add` is also the duplicate check (one id probe).
    #[inline]
    fn rest_passive(&mut self, order: &NewOrder, price: Price, out: &mut Vec<Event>) {
        let rest = RestingOrder {
            id: order.id,
            side: order.side,
            price,
            qty: order.qty,
        };
        if self.book.add(rest).is_err() {
            return self.reject(order.id, RejectReason::DuplicateOrderId, out);
        }
        out.push(Event::Accepted { id: order.id });
        out.push(Event::Rested {
            id: order.id,
            side: order.side,
            price,
            qty: order.qty,
        });
    }

    /// Matches `order` against the opposite side up to `limit` (`None`:
    /// market), appending trades; returns the unfilled quantity.
    #[inline]
    fn take(&mut self, order: &NewOrder, limit: Option<Price>, out: &mut Vec<Event>) -> Qty {
        let maker_side = order.side.opposite();
        let mut remaining = order.qty;
        while !remaining.is_zero() {
            let Some(maker) = self.book.front(maker_side) else {
                break;
            };
            if limit.is_some_and(|l| !order.side.crosses(l, maker.price)) {
                break;
            }
            let qty = remaining.min(maker.qty);
            self.book.fill_front(maker_side, qty);
            remaining -= qty;
            self.last_trade = Some(maker.price);
            self.tape.push(TradePrint {
                seq: Seq(self.seq),
                taker: order.id,
                maker: maker.id,
                taker_side: order.side,
                price: maker.price,
                qty,
            });
            out.push(Event::Trade {
                taker: order.id,
                maker: maker.id,
                taker_side: order.side,
                price: maker.price,
                qty,
            });
        }
        remaining
    }

    /// After matching a limit order: rest a GTC remainder, expire an IOC one.
    #[inline]
    fn finish(&mut self, order: &NewOrder, price: Price, remaining: Qty, out: &mut Vec<Event>) {
        if remaining.is_zero() {
            return;
        }
        if order.tif == TimeInForce::Gtc {
            let rest = RestingOrder {
                id: order.id,
                side: order.side,
                price,
                qty: remaining,
            };
            self.book.add(rest).expect("validated order must rest");
            out.push(Event::Rested {
                id: order.id,
                side: order.side,
                price,
                qty: remaining,
            });
        } else {
            out.push(Event::Expired {
                id: order.id,
                remaining,
            });
        }
    }

    /// True if the order passes every message-only check (lot, tick, band,
    /// non-zero) — one combined test, no reason computed.
    #[inline]
    fn spec_ok(&self, order: &NewOrder) -> bool {
        let spec = &self.config.spec;
        let lot_ok = self.unit_lot || order.qty.0.is_multiple_of(spec.lot);
        let price_ok = match order.kind {
            OrderKind::Limit { price, .. } => {
                (self.unit_tick || price.0.rem_euclid(spec.tick) == 0)
                    && (spec.band_ticks == 0 || self.in_band(price))
            }
            OrderKind::Market => true,
        };
        !order.qty.is_zero() && lot_ok && price_ok
    }

    #[inline]
    fn in_band(&self, price: Price) -> bool {
        let spec = &self.config.spec;
        match self.reference_price() {
            Some(reference) => {
                let max = (spec.band_ticks as u64).saturating_mul(spec.tick as u64);
                price.0.abs_diff(reference.0) <= max
            }
            None => true,
        }
    }

    // ---------------------------------------------------------------------
    // Cold paths.

    /// Records and emits a reject (the only place the counters are touched).
    #[cold]
    #[inline(never)]
    fn reject(&mut self, id: OrderId, reason: RejectReason, out: &mut Vec<Event>) {
        self.rejects[reject_slot(reason)] += 1;
        out.push(Event::Rejected { id, reason });
    }

    /// The order failed `spec_ok`: find the first failing check, in the
    /// documented order.
    #[cold]
    #[inline(never)]
    fn reject_spec(&mut self, order: &NewOrder, out: &mut Vec<Event>) {
        let spec = self.config.spec;
        let reason = if order.qty.is_zero() {
            RejectReason::ZeroQuantity
        } else if !order.qty.0.is_multiple_of(spec.lot) {
            RejectReason::InvalidLot
        } else {
            match order.kind {
                OrderKind::Limit { price, .. } if price.0.rem_euclid(spec.tick) != 0 => {
                    RejectReason::InvalidTick
                }
                _ => RejectReason::PriceBand,
            }
        };
        self.reject(order.id, reason, out);
    }

    /// Everything rare: fill-or-kill orders and post-only orders that would
    /// take. Runs the full book validation (in the documented order), then
    /// matches like the hot path.
    #[cold]
    #[inline(never)]
    fn submit_rare(&mut self, order: &NewOrder, out: &mut Vec<Event>) {
        let opposite = self.book.best(order.side.opposite());
        let reason = if self.book.contains(order.id) {
            Some(RejectReason::DuplicateOrderId)
        } else {
            match order.kind {
                OrderKind::Limit {
                    price,
                    post_only: true,
                } if opposite.is_some_and(|b| order.side.crosses(price, b.price)) => {
                    Some(RejectReason::WouldTake)
                }
                OrderKind::Market if opposite.is_none() => Some(RejectReason::NoLiquidity),
                _ if order.tif == TimeInForce::Fok
                    && self.fillable(order.side, order.limit_price(), order.qty) < order.qty =>
                {
                    Some(RejectReason::FokUnfillable)
                }
                _ => None,
            }
        };
        if let Some(reason) = reason {
            return self.reject(order.id, reason, out);
        }
        out.push(Event::Accepted { id: order.id });
        let limit = order.limit_price();
        let remaining = self.take(order, limit, out);
        match limit {
            Some(price) => self.finish(order, price, remaining, out),
            None if !remaining.is_zero() => out.push(Event::Expired {
                id: order.id,
                remaining,
            }),
            None => {}
        }
    }

    /// The fill-or-kill feasibility walk (as [`Engine::fillable`](crate::Engine::fillable)).
    #[cold]
    #[inline(never)]
    fn fillable(&self, side: Side, limit: Option<Price>, want: Qty) -> Qty {
        let mut got = Qty::ZERO;
        self.book.visit_levels(side.opposite(), &mut |level| {
            if limit.is_some_and(|l| !side.crosses(l, level.price)) {
                return false;
            }
            // Saturate: hostile quantities must not wrap the FOK check.
            got = Qty(got.0.saturating_add(level.qty.0));
            got < want
        });
        got.min(want)
    }

    #[inline]
    fn cancel(&mut self, id: OrderId, out: &mut Vec<Event>) {
        self.seq += 1;
        match self.book.cancel(id) {
            Ok(order) => out.push(Event::Cancelled {
                id,
                remaining: order.qty,
            }),
            Err(_) => self.reject(id, RejectReason::UnknownOrder, out),
        }
    }

    #[inline]
    fn reduce(&mut self, id: OrderId, qty: Qty, out: &mut Vec<Event>) {
        self.seq += 1;
        if qty.is_zero() || !(self.unit_lot || qty.0.is_multiple_of(self.config.spec.lot)) {
            let reason = if qty.is_zero() {
                RejectReason::ZeroQuantity
            } else {
                RejectReason::InvalidLot
            };
            return self.reject(id, reason, out);
        }
        match self.book.reduce(id, qty) {
            Ok(order) => out.push(Event::Reduced {
                id,
                remaining: order.qty,
            }),
            Err(e) => self.reject(id, e.into(), out),
        }
    }

    /// The band reference: last trade, else the configured reference.
    #[inline]
    pub fn reference_price(&self) -> Option<Price> {
        self.last_trade.or(self.config.reference_price)
    }

    /// How many commands were rejected with `reason` so far (risk rejects
    /// all count under any `Risk(_)`). O(1).
    pub fn reject_count(&self, reason: RejectReason) -> u64 {
        self.rejects[reject_slot(reason)]
    }

    /// The order book (read-only).
    pub fn book(&self) -> &B {
        &self.book
    }

    /// The risk policy.
    pub fn risk(&self) -> &R {
        &self.risk
    }

    /// The configuration.
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// Commands the engine sequenced (risk rejects excluded, like
    /// [`Guarded`](crate::Guarded)).
    pub fn seq(&self) -> Seq {
        Seq(self.seq)
    }

    /// Price of the most recent trade.
    pub fn last_trade_price(&self) -> Option<Price> {
        self.last_trade
    }

    /// The last `TAPE` trades.
    pub fn tape(&self) -> &TapeRing<TAPE> {
        &self.tape
    }

    /// The same hash as [`Engine::state_hash`](crate::Engine::state_hash)
    /// for the same state. O(resting orders).
    pub fn state_hash(&self) -> u64 {
        let mut h = StateHasher::new();
        h.write_u64(self.seq);
        match self.last_trade {
            Some(p) => {
                h.write_u64(1);
                h.write_i64(p.0);
            }
            None => h.write_u64(0),
        }
        for side in [Side::Buy, Side::Sell] {
            h.write_u64(self.book.level_count(side) as u64);
            self.book.visit_orders(side, &mut |o| {
                h.write_i64(o.price.0);
                h.write_u64(o.id.0);
                h.write_u64(o.qty.0);
                true
            });
        }
        h.write_u64(self.book.len() as u64);
        h.finish()
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
