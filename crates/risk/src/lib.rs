//! Pre-trade risk checks, throttles, positions and PnL.
//!
//! Curriculum: Weeks 15, 17, 45 (see <https://tepy.dev/hft>).
//!
//! | Week | Module | What |
//! | --- | --- | --- |
//! | 15 | [`pretrade`] | [`RiskEngine`]: per-account [`Limits`] (order size, notional, projected position, price band), open-order and position tracking from engine events, global and per-account kill switches |
//! | 17 | [`throttle`] | [`TokenBucket`], [`SlidingWindow`] and the per-session / per-account [`Throttle`], driven by an injected [`Clock`] |
//! | 45 | [`pnl`] | [`Position`] (average cost, flips, realized / unrealized PnL), per-account exposure in [`Positions`], post-trade limits |
//!
//! Everything here is single-threaded and runs on the gateway's engine-side
//! thread; only the global [`KillSwitch`] and [`FakeClock`] are shared
//! handles. Hot-path calls (`RiskEngine::check`, `Throttle::check_at`,
//! `Positions::apply_fill`) never allocate and never lock.
//!
//! Prices are ticks, quantities lots; money is ticks × lots in `u128` /
//! `i128`. There are no floats anywhere in this crate.
//!
//! # Wiring (gateway / engine side)
//!
//! ```ignore
//! // per inbound NewOrder, timestamped once on receipt:
//! throttle.check_at(session, order.account, rx_ns).map_err(RejectReason::Risk)?;
//! risk.check(&order, reference_price)?;          // Err(RejectReason) → Event::Rejected
//! risk.on_new(&order);
//! engine.submit(Command::New(order));
//!
//! // per engine event:
//! for fill in risk.on_event(&event).into_iter().flatten() {
//!     if positions.apply_fill(&fill).is_err() {
//!         risk.kill_account(fill.account);           // then cancel risk.open_orders(fill.account)
//!     }
//! }
//! ```

mod idmap;
pub mod pnl;
pub mod pretrade;
pub mod throttle;

pub use idmap::{IdHasher, IdMap};
pub use pnl::{AccountPnl, Breach, Position, Positions, PostTradeLimits};
pub use pretrade::{Exposure, Fill, KillSwitch, Limits, RiskEngine};
pub use throttle::{
    BucketConfig, Clock, FakeClock, MonotonicClock, SessionId, SlidingWindow, Throttle, TokenBucket,
};
