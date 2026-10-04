//! Week 45 — positions, PnL and post-trade limits, updated incrementally on
//! every fill.
//!
//! # Units
//!
//! Prices are integer ticks and quantities integer lots, so money is
//! *ticks × lots*, held in `i128`: an `i64` price times an `i64` position
//! cannot overflow it, and neither can the sums of a realistic book.
//!
//! # Average-cost position keeping
//!
//! A [`Position`] keeps three numbers: the net quantity `qty` (long > 0,
//! short < 0), the **cost basis** `cost` — the total signed cost of the open
//! lots, `Σ price × signed qty`, so negative for a short at positive prices —
//! and the realized PnL. The average entry price is `cost / qty`, but it is
//! never stored: storing a rounded average would leak rounding error into
//! every later fill.
//!
//! A fill of signed quantity `d` (buy > 0, sell < 0) at price `p`:
//!
//! * **Opens or adds** (flat, or same sign as `qty`):
//!   `qty += d`, `cost += d × p`. Nothing is realized; exact.
//! * **Reduces** (opposite sign, `|d| ≤ |qty|`): closes `c = |d|` lots. The
//!   cost released is the pro-rata share of the basis,
//!   `released = cost × c / |qty|`, **truncated toward zero**; then
//!   `realized += −(d × p) − released` and `cost −= released`. A full close
//!   (`c = |qty|`) releases the whole basis, with no division.
//! * **Flips** (opposite sign, `|d| > |qty|`, e.g. long 5, sell 8): first a
//!   full close of `|qty|` lots at `p` as above, then the remaining
//!   `d + qty` lots open a new position at `p` (`cost = (d + qty) × p`).
//!
//! Because the basis is kept as a total, truncation never loses value: the
//! sub-tick remainder of a partial reduction stays in the basis of the lots
//! still open and is realized when they close. Two invariants follow, and
//! the tests check both exactly:
//!
//! * `cost − realized = Σ d × p` over every fill (pure cash flow), so
//!   `realized + unrealized(mark) = −Σ d × p + qty × mark` whatever the
//!   rounding;
//! * a position that returns to flat has realized exactly `−Σ d × p`.
//!
//! The realized PnL of a single partial reduction may differ from the exact
//! rational average-cost value by less than one tick × lot per partial
//! reduction since the position was last flat.
//!
//! Unrealized PnL at mark `m` is `qty × m − cost`.
//!
//! # Accounts, marks and post-trade limits
//!
//! [`Positions`] keeps one `Position` per (account, instrument), one mark
//! per instrument, and per-account aggregates that are updated by deltas, so
//! a fill costs O(1) regardless of how many positions an account holds:
//!
//! * realized PnL `Σ realized`, cost `Σ cost`, net exposure
//!   `Σ qty × mark` (unrealized = net exposure − cost);
//! * gross exposure `Σ |qty| × |mark|`.
//!
//! A new mark for an instrument re-values every position held in it:
//! O(holders). Until [`Positions::set_mark`] is called for an instrument,
//! its mark is the price of its first fill.
//!
//! [`PostTradeLimits`] are checked after every fill and every mark update.
//! A breach is reported (level-triggered: on every update while the account
//! is beyond a limit); the caller trips the account's kill switch:
//!
//! ```ignore
//! for fill in risk.on_event(&event).into_iter().flatten() {
//!     if positions.apply_fill(&fill).is_err() {
//!         risk.kill_account(fill.account);
//!     }
//! }
//! positions.set_mark(instrument, mark, |account, _breach| risk.kill_account(account));
//! ```

use crate::pretrade::Fill;
use types::{AccountId, InstrumentId, Price, Qty, Side};
// Used only by the implementation (unused in the learner template).
#[allow(unused_imports)]
use crate::idmap::IdMap;

/// One account's position in one instrument, under the average-cost method
/// (see the module docs for the exact rules and rounding).
///
/// 48 bytes, `Copy`; every method is O(1) and allocation-free.
// @student w45
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Position {
    /// Net quantity in lots: long > 0, short < 0.
    qty: i64,
    /// Total signed cost of the open lots, Σ price × signed qty.
    cost: i128,
    /// Realized PnL so far.
    realized: i128,
}

// @student w45
impl Position {
    /// A flat position with nothing realized.
    pub fn new() -> Position {
        Position {
            qty: 0,
            cost: 0,
            realized: 0,
        }
    }

    /// Net quantity in lots: long > 0, short < 0.
    #[inline]
    pub fn qty(&self) -> i64 {
        self.qty
    }

    /// Cost basis of the open lots in ticks × lots, signed like the
    /// position (`avg price × qty`, exact). 0 when flat.
    #[inline]
    pub fn cost_basis(&self) -> i128 {
        self.cost
    }

    /// Realized PnL in ticks × lots.
    #[inline]
    pub fn realized(&self) -> i128 {
        self.realized
    }

    /// Average entry price, `cost / qty` truncated toward zero — for
    /// display only; PnL is computed from the exact cost basis. `None` when
    /// flat.
    pub fn avg_price(&self) -> Option<Price> {
        if self.qty == 0 {
            None
        } else {
            Some(Price((self.cost / self.qty as i128) as i64))
        }
    }

    /// Unrealized PnL at `mark`: `qty × mark − cost`.
    #[inline]
    pub fn unrealized(&self, mark: Price) -> i128 {
        self.qty as i128 * mark.0 as i128 - self.cost
    }

    /// Applies a fill of `qty` lots on `side` at `price` and returns the PnL
    /// it realized (0 when it only opens or adds). `qty` and the resulting
    /// position must fit in `i64`. O(1), allocation-free; one division on a
    /// partial reduction (64-bit when `cost × closed` fits in `i64`), none
    /// otherwise.
    #[inline]
    pub fn apply(&mut self, side: Side, price: Price, qty: Qty) -> i128 {
        let p = price.0;
        let d = match side {
            Side::Buy => qty.0 as i64,
            Side::Sell => -(qty.0 as i64),
        };
        let held = self.qty;
        if held == 0 || (held > 0) == (d > 0) {
            // Open or add: exact.
            self.qty = held + d;
            self.cost += wide_mul(d, p);
            return 0;
        }
        // Reduce, close or flip.
        let held_abs = held.unsigned_abs();
        let close = d.unsigned_abs().min(held_abs);
        let released = if close == held_abs {
            self.cost
        } else {
            pro_rata(self.cost, close, held_abs)
        };
        let closed = if d > 0 { close as i64 } else { -(close as i64) };
        let realized = -wide_mul(closed, p) - released;
        self.cost -= released;
        self.realized += realized;
        self.qty = held + d;
        let rest = d - closed;
        if rest != 0 {
            // Flip: the basis is exactly 0 after the full close; the rest
            // of the fill opens the new position at `p`.
            self.cost = wide_mul(rest, p);
        }
        realized
    }
}

/// Post-trade limits for one account.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PostTradeLimits {
    /// Largest tolerated loss in ticks × lots (≥ 0): breached when
    /// realized + unrealized PnL < −`max_loss`.
    pub max_loss: i128,
    /// Largest gross exposure `Σ |qty| × |mark|` in ticks × lots: breached
    /// when exceeded.
    pub max_gross_exposure: u128,
}

impl PostTradeLimits {
    /// Never breached.
    pub const UNLIMITED: PostTradeLimits = PostTradeLimits {
        max_loss: i128::MAX,
        max_gross_exposure: u128::MAX,
    };
}

impl Default for PostTradeLimits {
    fn default() -> Self {
        PostTradeLimits::UNLIMITED
    }
}

/// A post-trade limit an account is beyond.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Breach {
    /// Realized + unrealized PnL below −`max_loss`.
    MaxLoss,
    /// Gross exposure above `max_gross_exposure`.
    MaxGrossExposure,
}

/// An account's PnL and exposure across all its instruments, at the
/// current marks. All values in ticks × lots.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct AccountPnl {
    /// Σ realized PnL.
    pub realized: i128,
    /// Σ (qty × mark − cost).
    pub unrealized: i128,
    /// Σ |qty| × |mark|.
    pub gross_exposure: u128,
    /// Σ qty × mark (long minus short market value).
    pub net_exposure: i128,
}

impl AccountPnl {
    /// Realized + unrealized.
    pub fn total(&self) -> i128 {
        self.realized + self.unrealized
    }
}

// @student-begin w45
/// `a × b` as `i128`: one widening multiply instruction.
#[inline]
fn wide_mul(a: i64, b: i64) -> i128 {
    a as i128 * b as i128
}

/// `cost × part / whole` truncated toward zero, for `part < whole`: in
/// `i64` (one hardware division) when it fits, else in `i128`.
#[inline]
fn pro_rata(cost: i128, part: u64, whole: u64) -> i128 {
    if let (Ok(c), Ok(part), Ok(whole)) = (
        i64::try_from(cost),
        i64::try_from(part),
        i64::try_from(whole),
    ) {
        if let Some(product) = c.checked_mul(part) {
            return (product / whole) as i128;
        }
    }
    cost * part as i128 / whole as i128
}

/// A position plus the dense indices of its account and instrument; one
/// cache line each.
#[derive(Clone, Copy, Debug)]
#[repr(align(64))]
struct PosSlot {
    account: u32,
    instrument: u32,
    pos: Position,
}

/// Per-account limits and aggregates, maintained by deltas (96 bytes: the
/// account id lives in `Positions::account_ids`, off the fill path).
#[derive(Clone, Copy, Debug)]
struct AccountAgg {
    limits: PostTradeLimits,
    /// Σ realized.
    realized: i128,
    /// Σ cost basis.
    cost: i128,
    /// Σ qty × mark.
    net: i128,
    /// Σ |qty| × |mark| (never negative; i128 so deltas can be added).
    gross: i128,
}

impl AccountAgg {
    fn new(limits: PostTradeLimits) -> AccountAgg {
        AccountAgg {
            limits,
            realized: 0,
            cost: 0,
            net: 0,
            gross: 0,
        }
    }

    #[inline]
    fn breach(&self) -> Result<(), Breach> {
        let pnl = self.realized + self.net - self.cost;
        if pnl < -self.limits.max_loss {
            return Err(Breach::MaxLoss);
        }
        if self.gross as u128 > self.limits.max_gross_exposure {
            return Err(Breach::MaxGrossExposure);
        }
        Ok(())
    }

    fn pnl(&self) -> AccountPnl {
        AccountPnl {
            realized: self.realized,
            unrealized: self.net - self.cost,
            gross_exposure: self.gross as u128,
            net_exposure: self.net,
        }
    }
}

/// An instrument's mark and the slots of every position held in it.
#[derive(Clone, Debug)]
struct Instrument {
    mark: i64,
    marked: bool,
    holders: Vec<u32>,
}

#[inline]
fn pos_key(account: AccountId, instrument: InstrumentId) -> u64 {
    (instrument.0 as u64) << 32 | account.0 as u64
}
// @student-end

/// Positions, PnL and post-trade limits for every account and instrument.
///
/// Single-threaded, like [`crate::RiskEngine`]; fed with the [`Fill`]s that
/// `RiskEngine::on_event` returns and with marks from market data.
// @student w45
#[derive(Debug, Default)]
pub struct Positions {
    default_limits: PostTradeLimits,
    slots: Vec<PosSlot>,
    slot_index: IdMap<u64, u32>,
    accounts: Vec<AccountAgg>,
    /// `account_ids[i]` is the id of `accounts[i]`.
    account_ids: Vec<AccountId>,
    account_index: IdMap<AccountId, u32>,
    instruments: Vec<Instrument>,
    instrument_index: IdMap<InstrumentId, u32>,
}

// @student w45
impl Positions {
    /// Empty books; accounts without `set_limits` get `default_limits`.
    pub fn new(default_limits: PostTradeLimits) -> Positions {
        Positions {
            default_limits,
            ..Positions::default()
        }
    }

    /// Sets `account`'s post-trade limits. Does not report a breach
    /// immediately — see [`Positions::check_account`]. O(1) amortized.
    pub fn set_limits(&mut self, account: AccountId, limits: PostTradeLimits) {
        let a = self.account_slot(account);
        self.accounts[a as usize].limits = limits;
    }

    /// Applies one fill to its (account, instrument) position and the
    /// account's aggregates, then checks the account's limits.
    ///
    /// O(1): one hash lookup plus integer arithmetic (one division on a
    /// partial reduction). Allocates only the first time an
    /// (account, instrument) pair trades.
    #[inline]
    pub fn apply_fill(&mut self, fill: &Fill) -> Result<(), Breach> {
        let slot = match self.slot_index.get(&pos_key(fill.account, fill.instrument)) {
            Some(&s) => s,
            None => self.new_slot(fill.account, fill.instrument, fill.price),
        };
        let s = &mut self.slots[slot as usize];
        let mark = self.instruments[s.instrument as usize].mark;
        let (qty0, cost0) = (s.pos.qty, s.pos.cost);
        let realized = s.pos.apply(fill.side, fill.price, fill.qty);
        let qty1 = s.pos.qty;
        let a = &mut self.accounts[s.account as usize];
        a.realized += realized;
        a.cost += s.pos.cost - cost0;
        a.net += wide_mul(qty1 - qty0, mark);
        a.gross += (qty1.unsigned_abs() as i128 - qty0.unsigned_abs() as i128)
            * mark.unsigned_abs() as i128;
        a.breach()
    }

    /// Marks `instrument` at `mark`, re-values every position held in it,
    /// and calls `on_breach` for each holder that is then beyond a limit.
    /// O(holders); never allocates (except for a never-seen instrument).
    pub fn set_mark(
        &mut self,
        instrument: InstrumentId,
        mark: Price,
        on_breach: impl FnMut(AccountId, Breach),
    ) {
        let mut on_breach = on_breach;
        let i = self.instrument_slot(instrument);
        let inst = &mut self.instruments[i as usize];
        let old = inst.mark as i128;
        inst.mark = mark.0;
        inst.marked = true;
        let new = mark.0 as i128;
        let (d_net, d_abs) = (new - old, new.abs() - old.abs());
        for &h in &inst.holders {
            let s = &self.slots[h as usize];
            let a = &mut self.accounts[s.account as usize];
            let qty = s.pos.qty as i128;
            a.net += qty * d_net;
            a.gross += qty.abs() * d_abs;
            if let Err(b) = a.breach() {
                on_breach(self.account_ids[s.account as usize], b);
            }
        }
    }

    /// The current mark of `instrument`, if it has one. O(1).
    pub fn mark(&self, instrument: InstrumentId) -> Option<Price> {
        let &i = self.instrument_index.get(&instrument)?;
        let inst = &self.instruments[i as usize];
        inst.marked.then_some(Price(inst.mark))
    }

    /// `account`'s position in `instrument`, if it ever traded it. O(1).
    pub fn position(&self, account: AccountId, instrument: InstrumentId) -> Option<Position> {
        let &s = self.slot_index.get(&pos_key(account, instrument))?;
        Some(self.slots[s as usize].pos)
    }

    /// `account`'s PnL and exposure across its instruments at the current
    /// marks, if the account is known. O(1) (aggregates are maintained).
    pub fn account(&self, account: AccountId) -> Option<AccountPnl> {
        let &a = self.account_index.get(&account)?;
        Some(self.accounts[a as usize].pnl())
    }

    /// Checks `account` against its limits at the current marks (unknown
    /// accounts pass). O(1).
    pub fn check_account(&self, account: AccountId) -> Result<(), Breach> {
        match self.account_index.get(&account) {
            Some(&a) => self.accounts[a as usize].breach(),
            None => Ok(()),
        }
    }
}

// @student-begin w45
impl Positions {
    /// Dense index of `account`, created with the default limits.
    fn account_slot(&mut self, account: AccountId) -> u32 {
        let (accounts, ids, limits) = (
            &mut self.accounts,
            &mut self.account_ids,
            self.default_limits,
        );
        *self.account_index.entry(account).or_insert_with(|| {
            accounts.push(AccountAgg::new(limits));
            ids.push(account);
            (accounts.len() - 1) as u32
        })
    }

    /// Dense index of `instrument`, created unmarked.
    fn instrument_slot(&mut self, instrument: InstrumentId) -> u32 {
        let instruments = &mut self.instruments;
        *self.instrument_index.entry(instrument).or_insert_with(|| {
            instruments.push(Instrument {
                mark: 0,
                marked: false,
                holders: Vec::new(),
            });
            (instruments.len() - 1) as u32
        })
    }

    /// Creates the (account, instrument) position. An instrument without a
    /// mark is marked at `first_price` (its first fill).
    #[cold]
    fn new_slot(
        &mut self,
        account: AccountId,
        instrument: InstrumentId,
        first_price: Price,
    ) -> u32 {
        let a = self.account_slot(account);
        let i = self.instrument_slot(instrument);
        let slot = self.slots.len() as u32;
        self.slots.push(PosSlot {
            account: a,
            instrument: i,
            pos: Position::new(),
        });
        self.slot_index.insert(pos_key(account, instrument), slot);
        let inst = &mut self.instruments[i as usize];
        if !inst.marked {
            inst.mark = first_price.0;
            inst.marked = true;
        }
        inst.holders.push(slot);
        slot
    }
}
// @student-end
