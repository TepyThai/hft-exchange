//! Week 15 — pre-trade risk: per-account limits, exposure tracking and kill
//! switches, checked before an order reaches the matching engine.
//!
//! # Where it sits
//!
//! [`RiskEngine`] is a single-threaded component owned by the thread that
//! sequences orders into the engine (the gateway's engine-side loop):
//!
//! ```text
//! NewOrder ─► risk.check(&o, reference) ─► Ok  ─► risk.on_new(&o) ─► engine
//!                                       └► Err ─► Event::Rejected { reason }
//! engine events ─► risk.on_event(&ev) ─► fills (for positions / PnL, w45)
//! ```
//!
//! `check` takes `&self`, never allocates and never locks: one atomic load,
//! one hash lookup and a handful of integer comparisons. All bookkeeping
//! happens in `on_new` / `on_event`.
//!
//! # Exposure and the projected-position rule
//!
//! For every (account, instrument) the engine keeps an [`Exposure`]: the net
//! filled `position` (long > 0, short < 0) and the total remaining quantity
//! of the account's open orders on each side (`open_buy`, `open_sell`). An
//! order counts as open from `on_new` (so orders in flight to the engine are
//! included) until a `Cancelled`, `Expired`, `Rejected` or final `Trade`
//! event closes it.
//!
//! The position limit assumes the worst case: every open order on the side
//! that *increases* exposure in the order's direction fills, plus the new
//! order. Opposite-side open orders are ignored — they could be cancelled, so
//! they are never credited.
//!
//! ```text
//! buy  qty q:  projected = position + open_buy  + q   reject if projected >  max_position
//! sell qty q:  projected = position − open_sell − q   reject if projected < −max_position
//! ```
//!
//! Consequences: an order that only reduces the position (sell while long,
//! with `position − open_sell − q ≥ −max_position`) always passes, even if
//! the account is over its limit; and an account cannot "pre-load" both
//! sides past the limit, because each side is bounded on its own. The
//! arithmetic is done in `i128`, so no combination of `i64`/`u64` inputs can
//! overflow.
//!
//! # Check order
//!
//! The first failing check decides the reason:
//!
//! 1. global kill switch → `Risk(KillSwitch)`
//! 2. account kill switch → `Risk(KillSwitch)`
//! 3. `qty > max_order_qty` → `Risk(MaxOrderQty)`
//! 4. limit price more than `price_band_ticks` from the reference →
//!    [`RejectReason::PriceBand`] (market orders have no price and skip it)
//! 5. `|price| × qty > max_notional` (market orders are valued at the
//!    reference price) → `Risk(MaxNotional)`
//! 6. projected position beyond `max_position` → `Risk(PositionLimit)`
//!
//! `check` returns a [`RejectReason`] rather than a bare [`RiskReason`]
//! because a price-band violation has its own variant in the shared
//! contract; every other failure is `RejectReason::Risk(_)`, so the gateway
//! can put the result straight into `Event::Rejected`.

use types::{AccountId, Event, InstrumentId, NewOrder, OrderId, Price, Qty, RejectReason, Side};
// Used only by the implementation (unused in the learner template).
#[allow(unused_imports)]
use {
    crate::idmap::IdMap,
    std::sync::atomic::{AtomicBool, Ordering},
    std::sync::Arc,
    types::{OrderKind, RiskReason},
};

/// Pre-trade limits for one account. Position limits apply to each
/// instrument separately.
///
/// Start from [`Limits::UNLIMITED`] and override what you need:
///
/// ```
/// # use risk::Limits; use types::Qty;
/// let l = Limits { max_order_qty: Qty(1_000), price_band_ticks: 50, ..Limits::UNLIMITED };
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Limits {
    /// Largest quantity a single order may have (fat-finger check).
    pub max_order_qty: Qty,
    /// Largest notional of a single order, `|price| × qty` in ticks × lots.
    /// `u128` so that no `i64` price times `u64` quantity can overflow.
    pub max_notional: u128,
    /// Largest absolute projected net position per instrument, in lots
    /// (see the module docs for the projection rule).
    pub max_position: Qty,
    /// Largest allowed distance of a limit price from the reference price,
    /// in ticks. `0` disables the band (as `InstrumentSpec::band_ticks`).
    pub price_band_ticks: u64,
}

impl Limits {
    /// No limits at all: every order passes (unless a kill switch is on).
    pub const UNLIMITED: Limits = Limits {
        max_order_qty: Qty(u64::MAX),
        max_notional: u128::MAX,
        max_position: Qty(u64::MAX),
        price_band_ticks: 0,
    };

    /// Blocks every order with `MaxOrderQty` — a safe default for accounts
    /// that must be configured explicitly before trading.
    pub const BLOCKED: Limits = Limits {
        max_order_qty: Qty(0),
        max_notional: 0,
        max_position: Qty(0),
        price_band_ticks: 0,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Limits::UNLIMITED
    }
}

/// What pre-trade risk knows about one account in one instrument.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Exposure {
    /// Net filled position in lots: long > 0, short < 0.
    pub position: i64,
    /// Total remaining quantity of the account's open buy orders.
    pub open_buy: Qty,
    /// Total remaining quantity of the account's open sell orders.
    pub open_sell: Qty,
}

/// One side of a trade, attributed to a tracked order: what
/// [`RiskEngine::on_event`] returns for every `Event::Trade` leg it knows,
/// and what position keeping (w45) consumes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Fill {
    /// The tracked order that filled.
    pub order: OrderId,
    pub account: AccountId,
    pub instrument: InstrumentId,
    /// The side of *this* order (the maker's side is the taker's opposite).
    pub side: Side,
    /// Trade price (the maker's price).
    pub price: Price,
    pub qty: Qty,
}

/// A global kill switch that any thread can flip.
///
/// Cloning shares the same switch (it is an `Arc<AtomicBool>`), so an
/// operator thread can hold a handle while the engine thread checks it on
/// every order. Engaging blocks new orders only; cancels are never blocked.
/// Every method is one atomic operation — no locks, no allocation (except
/// [`KillSwitch::new`], which allocates the shared flag once).
// @student w15
#[derive(Clone, Debug, Default)]
pub struct KillSwitch {
    engaged: Arc<AtomicBool>,
}

// @student w15
impl KillSwitch {
    /// A new, disengaged switch.
    pub fn new() -> KillSwitch {
        KillSwitch {
            engaged: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Blocks all new orders. Visible to every handle promptly (the flag
    /// guards no other data, so `Relaxed` is enough: there is nothing to
    /// synchronize-with, only a value to observe).
    pub fn engage(&self) {
        self.engaged.store(true, Ordering::Relaxed);
    }

    /// Allows new orders again.
    pub fn disengage(&self) {
        self.engaged.store(false, Ordering::Relaxed);
    }

    /// True while engaged. One relaxed load (~1ns, a plain `mov` on x86).
    #[inline]
    pub fn is_engaged(&self) -> bool {
        self.engaged.load(Ordering::Relaxed)
    }
}

// @student-begin w15
/// Per-account configuration: limits and the account kill switch.
#[derive(Clone, Copy, Debug)]
struct AccountRisk {
    limits: Limits,
    killed: bool,
}

/// One (account, instrument) exposure record, stored in a slab so that
/// order events reach it with a direct index. It also holds the dense
/// index of its account, so `check` needs one hash lookup, not two.
#[derive(Clone, Copy, Debug)]
struct Slot {
    account: AccountId,
    /// Index into `RiskEngine::accounts`.
    account_idx: u32,
    instrument: InstrumentId,
    exposure: Exposure,
}

/// A tracked open order.
#[derive(Clone, Copy, Debug)]
struct OpenOrder {
    /// Index of the order's (account, instrument) slot.
    slot: u32,
    side: Side,
    /// Remaining (open) quantity: counted in the slot's `open_buy`/`open_sell`.
    remaining: u64,
    /// The engine has accepted the order (Accepted, Rested or a Trade seen).
    acked: bool,
}

/// Key of an (account, instrument) pair: the account id in the low half,
/// where most of the variation is.
#[inline]
fn slot_key(account: AccountId, instrument: InstrumentId) -> u64 {
    (instrument.0 as u64) << 32 | account.0 as u64
}

/// `|price| × qty` in ticks × lots, exact.
#[inline]
fn notional(price: Price, qty: Qty) -> u128 {
    price.0.unsigned_abs() as u128 * qty.0 as u128
}

/// The projected-position rule (module docs): may `exposure` take `qty`
/// more on `side` within `max_position`?
#[inline]
fn within_position_limit(exposure: &Exposure, side: Side, qty: Qty, max_position: Qty) -> bool {
    let position = exposure.position as i128;
    let qty = qty.0 as i128;
    let max = max_position.0 as i128;
    match side {
        Side::Buy => position + exposure.open_buy.0 as i128 + qty <= max,
        Side::Sell => position - exposure.open_sell.0 as i128 - qty >= -max,
    }
}

/// The limit checks after the kill switches, in the documented order.
#[inline]
fn check_limits(
    limits: &Limits,
    exposure: &Exposure,
    order: &NewOrder,
    reference: Price,
) -> Result<(), RejectReason> {
    if order.qty > limits.max_order_qty {
        return Err(RejectReason::Risk(RiskReason::MaxOrderQty));
    }
    let price = match order.kind {
        OrderKind::Limit { price, .. } => {
            if limits.price_band_ticks != 0 {
                let distance = (price.0 as i128 - reference.0 as i128).unsigned_abs();
                if distance > limits.price_band_ticks as u128 {
                    return Err(RejectReason::PriceBand);
                }
            }
            price
        }
        OrderKind::Market => reference,
    };
    if notional(price, order.qty) > limits.max_notional {
        return Err(RejectReason::Risk(RiskReason::MaxNotional));
    }
    if !within_position_limit(exposure, order.side, order.qty, limits.max_position) {
        return Err(RejectReason::Risk(RiskReason::PositionLimit));
    }
    Ok(())
}

/// Adds `delta` to the open quantity of `side`.
#[inline]
fn open_add(exposure: &mut Exposure, side: Side, delta: u64) {
    match side {
        Side::Buy => exposure.open_buy.0 += delta,
        Side::Sell => exposure.open_sell.0 += delta,
    }
}

/// Removes `delta` from the open quantity of `side`.
#[inline]
fn open_sub(exposure: &mut Exposure, side: Side, delta: u64) {
    match side {
        Side::Buy => exposure.open_buy.0 = exposure.open_buy.0.saturating_sub(delta),
        Side::Sell => exposure.open_sell.0 = exposure.open_sell.0.saturating_sub(delta),
    }
}
// @student-end

/// The pre-trade risk engine: limits, kill switches, and the open-order /
/// position bookkeeping the limits are checked against.
///
/// Single-threaded by design (it lives on the engine-side thread); only the
/// global [`KillSwitch`] is shared across threads.
// @student w15
#[derive(Debug)]
pub struct RiskEngine {
    /// Limits for accounts without an explicit `set_limits`.
    default_limits: Limits,
    /// Dense per-account configuration, and account id → index into it.
    accounts: Vec<AccountRisk>,
    account_index: IdMap<AccountId, u32>,
    /// (account, instrument) → index into `slots`.
    slot_index: IdMap<u64, u32>,
    slots: Vec<Slot>,
    /// Open orders: id → slot, side, remaining.
    orders: IdMap<OrderId, OpenOrder>,
    global: KillSwitch,
}

// @student w15
impl RiskEngine {
    /// An engine where every account without explicit limits uses
    /// `default_limits` (use [`Limits::BLOCKED`] to require configuration).
    /// Allocates the kill-switch flag; the tables start empty.
    pub fn new(default_limits: Limits) -> RiskEngine {
        RiskEngine::with_capacity(default_limits, 0, 0)
    }

    /// Like [`RiskEngine::new`], but pre-sizes the tables for `pairs`
    /// (account, instrument) pairs and `open_orders` concurrently open
    /// orders, so steady-state `on_new` never reallocates.
    pub fn with_capacity(default_limits: Limits, pairs: usize, open_orders: usize) -> RiskEngine {
        RiskEngine {
            default_limits,
            accounts: Vec::new(),
            account_index: IdMap::default(),
            slot_index: IdMap::with_capacity_and_hasher(pairs, Default::default()),
            slots: Vec::with_capacity(pairs),
            orders: IdMap::with_capacity_and_hasher(open_orders, Default::default()),
            global: KillSwitch::new(),
        }
    }

    /// Sets `account`'s limits (keeps its kill-switch state). Tighter limits
    /// apply to new orders only; open orders are not touched. O(1) amortized.
    pub fn set_limits(&mut self, account: AccountId, limits: Limits) {
        let a = self.account_for(account);
        self.accounts[a as usize].limits = limits;
    }

    /// The limits that apply to `account` (its own or the default). O(1).
    pub fn limits(&self, account: AccountId) -> Limits {
        self.account(account)
            .map_or(self.default_limits, |a| a.limits)
    }

    /// A handle to the global kill switch, for an operator thread. O(1).
    pub fn global_kill_switch(&self) -> KillSwitch {
        self.global.clone()
    }

    /// Engages `account`'s kill switch: its new orders are rejected with
    /// `KillSwitch` until [`RiskEngine::revive_account`]. Open orders stay on
    /// the book — cancel them with [`RiskEngine::open_orders`]. O(1)
    /// amortized.
    pub fn kill_account(&mut self, account: AccountId) {
        let a = self.account_for(account);
        self.accounts[a as usize].killed = true;
    }

    /// Disengages `account`'s kill switch. O(1).
    pub fn revive_account(&mut self, account: AccountId) {
        if let Some(&a) = self.account_index.get(&account) {
            self.accounts[a as usize].killed = false;
        }
    }

    /// True if new orders from `account` are blocked by a kill switch
    /// (its own or the global one). O(1).
    pub fn is_killed(&self, account: AccountId) -> bool {
        self.global.is_engaged() || self.account(account).is_some_and(|a| a.killed)
    }

    /// The pre-trade check: may `order` go to the engine, given the current
    /// `reference` price (last trade, mid or previous close — the caller's
    /// choice)? See the module docs for the order of checks and the
    /// projected-position rule.
    ///
    /// Read-only: it does not record the order (call
    /// [`RiskEngine::on_new`] when you forward it). O(1): one atomic load
    /// and one hash lookup (two for an account's first order in an
    /// instrument); never allocates, never locks.
    #[inline]
    pub fn check(&self, order: &NewOrder, reference: Price) -> Result<(), RejectReason> {
        if self.global.is_engaged() {
            return Err(RejectReason::Risk(RiskReason::KillSwitch));
        }
        let (account, exposure) = match self
            .slot_index
            .get(&slot_key(order.account, order.instrument))
        {
            Some(&i) => {
                let slot = &self.slots[i as usize];
                (
                    Some(&self.accounts[slot.account_idx as usize]),
                    slot.exposure,
                )
            }
            None => (self.account(order.account), Exposure::default()),
        };
        let limits = match account {
            Some(a) if a.killed => return Err(RejectReason::Risk(RiskReason::KillSwitch)),
            Some(a) => &a.limits,
            None => &self.default_limits,
        };
        check_limits(limits, &exposure, order, reference)
    }

    /// Records `order` as open (call it after a successful `check`, when the
    /// order is forwarded to the engine): its quantity counts towards the
    /// account's open quantity on its side from now on, so orders still in
    /// flight to the engine are part of the next check.
    ///
    /// An id that is already open is ignored (the engine will reject the
    /// duplicate). O(1) amortized; allocates only when a table grows past
    /// its capacity (new account/instrument pair, more open orders).
    pub fn on_new(&mut self, order: &NewOrder) {
        if self.orders.contains_key(&order.id) {
            return;
        }
        let slot = self.slot_for(order.account, order.instrument);
        open_add(
            &mut self.slots[slot as usize].exposure,
            order.side,
            order.qty.0,
        );
        self.orders.insert(
            order.id,
            OpenOrder {
                slot,
                side: order.side,
                remaining: order.qty.0,
                acked: false,
            },
        );
    }

    /// Applies an engine event to the exposure of the orders it concerns.
    /// Events for orders this engine never saw in `on_new` are ignored.
    ///
    /// * `Accepted` marks the order acknowledged.
    /// * `Trade` moves the filled quantity from open to position for each
    ///   tracked leg (taker and maker — both may be tracked, even by the
    ///   same account) and returns one [`Fill`] per tracked leg:
    ///   `[taker, maker]`. A fully filled order stops being tracked.
    /// * `Rested` / `Reduced` set the order's remaining quantity.
    /// * `Cancelled` / `Expired` release the remaining quantity.
    /// * `Rejected` releases the order — except a `DuplicateOrderId`
    ///   rejection of an id that is already acknowledged: that reject is for
    ///   a second order reusing a live id, not for the order we track.
    ///
    /// Returns `[None, None]` for every event but `Trade`. O(1): one or two
    /// hash lookups; never allocates.
    pub fn on_event(&mut self, event: &Event) -> [Option<Fill>; 2] {
        match *event {
            Event::Accepted { id } => {
                if let Some(o) = self.orders.get_mut(&id) {
                    o.acked = true;
                }
            }
            Event::Rejected { id, reason } => {
                let release = match self.orders.get(&id) {
                    Some(o) => reason != RejectReason::DuplicateOrderId || !o.acked,
                    None => false,
                };
                if release {
                    self.close(id);
                }
            }
            Event::Trade {
                taker,
                maker,
                price,
                qty,
                ..
            } => {
                return [self.fill(taker, price, qty), self.fill(maker, price, qty)];
            }
            Event::Rested { id, qty, .. } | Event::Reduced { id, remaining: qty } => {
                self.set_remaining(id, qty.0);
            }
            Event::Cancelled { id, .. } | Event::Expired { id, .. } => self.close(id),
        }
        [None, None]
    }

    /// Current exposure of `account` in `instrument` (zero if never seen).
    /// O(1).
    pub fn exposure(&self, account: AccountId, instrument: InstrumentId) -> Exposure {
        match self.slot_index.get(&slot_key(account, instrument)) {
            Some(&i) => self.slots[i as usize].exposure,
            None => Exposure::default(),
        }
    }

    /// Overrides the filled position of `account` in `instrument` — for
    /// start-of-day positions or reconciliation. O(1) amortized.
    pub fn set_position(&mut self, account: AccountId, instrument: InstrumentId, position: i64) {
        let slot = self.slot_for(account, instrument);
        self.slots[slot as usize].exposure.position = position;
    }

    /// Ids of `account`'s open orders, ascending — what to cancel after a
    /// kill. O(open orders) and allocates the result: not for the hot path.
    pub fn open_orders(&self, account: AccountId) -> Vec<OrderId> {
        let mut ids: Vec<OrderId> = self
            .orders
            .iter()
            .filter(|(_, o)| self.slots[o.slot as usize].account == account)
            .map(|(&id, _)| id)
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Number of orders currently tracked as open. O(1).
    pub fn open_order_count(&self) -> usize {
        self.orders.len()
    }
}

// @student-begin w15
impl RiskEngine {
    /// `account`'s configuration, if it has one.
    #[inline]
    fn account(&self, account: AccountId) -> Option<&AccountRisk> {
        self.account_index
            .get(&account)
            .map(|&a| &self.accounts[a as usize])
    }

    /// Index of `account`'s configuration, created with the default limits.
    fn account_for(&mut self, account: AccountId) -> u32 {
        let (accounts, limits) = (&mut self.accounts, self.default_limits);
        *self.account_index.entry(account).or_insert_with(|| {
            accounts.push(AccountRisk {
                limits,
                killed: false,
            });
            (accounts.len() - 1) as u32
        })
    }

    /// The slot of (account, instrument), created on first use.
    fn slot_for(&mut self, account: AccountId, instrument: InstrumentId) -> u32 {
        if let Some(&s) = self.slot_index.get(&slot_key(account, instrument)) {
            return s;
        }
        let account_idx = self.account_for(account);
        let s = self.slots.len() as u32;
        self.slots.push(Slot {
            account,
            account_idx,
            instrument,
            exposure: Exposure::default(),
        });
        self.slot_index.insert(slot_key(account, instrument), s);
        s
    }

    /// One trade leg: open → position. Returns the fill if `id` is tracked.
    fn fill(&mut self, id: OrderId, price: Price, qty: Qty) -> Option<Fill> {
        let o = self.orders.get_mut(&id)?;
        o.acked = true;
        let filled = qty.0.min(o.remaining);
        o.remaining -= filled;
        let (slot, side, done) = (o.slot, o.side, o.remaining == 0);
        if done {
            self.orders.remove(&id);
        }
        let s = &mut self.slots[slot as usize];
        open_sub(&mut s.exposure, side, filled);
        let signed = qty.0 as i64;
        match side {
            Side::Buy => s.exposure.position += signed,
            Side::Sell => s.exposure.position -= signed,
        }
        Some(Fill {
            order: id,
            account: s.account,
            instrument: s.instrument,
            side,
            price,
            qty,
        })
    }

    /// Sets a tracked order's remaining quantity (Rested / Reduced).
    fn set_remaining(&mut self, id: OrderId, remaining: u64) {
        let Some(o) = self.orders.get_mut(&id) else {
            return;
        };
        o.acked = true;
        let (slot, side, old) = (o.slot, o.side, o.remaining);
        o.remaining = remaining;
        if remaining == 0 {
            self.orders.remove(&id);
        }
        let e = &mut self.slots[slot as usize].exposure;
        if remaining < old {
            open_sub(e, side, old - remaining);
        } else {
            open_add(e, side, remaining - old);
        }
    }

    /// Stops tracking `id`, releasing its remaining quantity.
    fn close(&mut self, id: OrderId) {
        if let Some(o) = self.orders.remove(&id) {
            open_sub(
                &mut self.slots[o.slot as usize].exposure,
                o.side,
                o.remaining,
            );
        }
    }
}
// @student-end
