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

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::pretrade::Fill;
use types::{AccountId, InstrumentId, Price, Qty, Side};
// Used only by the implementation (unused in the learner template).
#[allow(unused_imports)]
use crate::idmap::IdMap;

/// One account's position in one instrument, under the average-cost method
/// (see the module docs for the exact rules and rounding).
///
/// 48 bytes, `Copy`; every method is O(1) and allocation-free.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Position {
    // TODO(w45): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl Position {
    /// A flat position with nothing realized.
    pub fn new() -> Position {
        todo!("w45: new")
    }

    /// Net quantity in lots: long > 0, short < 0.
    #[inline]
    pub fn qty(&self) -> i64 {
        todo!("w45: qty")
    }

    /// Cost basis of the open lots in ticks × lots, signed like the
    /// position (`avg price × qty`, exact). 0 when flat.
    #[inline]
    pub fn cost_basis(&self) -> i128 {
        todo!("w45: cost_basis")
    }

    /// Realized PnL in ticks × lots.
    #[inline]
    pub fn realized(&self) -> i128 {
        todo!("w45: realized")
    }

    /// Average entry price, `cost / qty` truncated toward zero — for
    /// display only; PnL is computed from the exact cost basis. `None` when
    /// flat.
    pub fn avg_price(&self) -> Option<Price> {
        todo!("w45: avg_price")
    }

    /// Unrealized PnL at `mark`: `qty × mark − cost`.
    #[inline]
    pub fn unrealized(&self, mark: Price) -> i128 {
        todo!("w45: unrealized")
    }

    /// Applies a fill of `qty` lots on `side` at `price` and returns the PnL
    /// it realized (0 when it only opens or adds). `qty` and the resulting
    /// position must fit in `i64`. O(1), allocation-free; one division on a
    /// partial reduction (64-bit when `cost × closed` fits in `i64`), none
    /// otherwise.
    #[inline]
    pub fn apply(&mut self, side: Side, price: Price, qty: Qty) -> i128 {
        todo!("w45: apply")
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

// TODO(w45): implement — see the week guide on tepy.dev/hft.

/// Positions, PnL and post-trade limits for every account and instrument.
///
/// Single-threaded, like [`crate::RiskEngine`]; fed with the [`Fill`]s that
/// `RiskEngine::on_event` returns and with marks from market data.
#[derive(Debug, Default)]
pub struct Positions {
    // TODO(w45): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl Positions {
    /// Empty books; accounts without `set_limits` get `default_limits`.
    pub fn new(default_limits: PostTradeLimits) -> Positions {
        todo!("w45: new")
    }

    /// Sets `account`'s post-trade limits. Does not report a breach
    /// immediately — see [`Positions::check_account`]. O(1) amortized.
    pub fn set_limits(&mut self, account: AccountId, limits: PostTradeLimits) {
        todo!("w45: set_limits")
    }

    /// Applies one fill to its (account, instrument) position and the
    /// account's aggregates, then checks the account's limits.
    ///
    /// O(1): one hash lookup plus integer arithmetic (one division on a
    /// partial reduction). Allocates only the first time an
    /// (account, instrument) pair trades.
    #[inline]
    pub fn apply_fill(&mut self, fill: &Fill) -> Result<(), Breach> {
        todo!("w45: apply_fill")
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
        todo!("w45: set_mark")
    }

    /// The current mark of `instrument`, if it has one. O(1).
    pub fn mark(&self, instrument: InstrumentId) -> Option<Price> {
        todo!("w45: mark")
    }

    /// `account`'s position in `instrument`, if it ever traded it. O(1).
    pub fn position(&self, account: AccountId, instrument: InstrumentId) -> Option<Position> {
        todo!("w45: position")
    }

    /// `account`'s PnL and exposure across its instruments at the current
    /// marks, if the account is known. O(1) (aggregates are maintained).
    pub fn account(&self, account: AccountId) -> Option<AccountPnl> {
        todo!("w45: account")
    }

    /// Checks `account` against its limits at the current marks (unknown
    /// accounts pass). O(1).
    pub fn check_account(&self, account: AccountId) -> Result<(), Breach> {
        todo!("w45: check_account")
    }
}

// TODO(w45): implement — see the week guide on tepy.dev/hft.
