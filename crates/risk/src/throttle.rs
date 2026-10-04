//! Week 17 — rate limiting: token buckets and sliding windows driven by an
//! injected monotonic clock, combined per session and per account.
//!
//! # Time
//!
//! Every limiter takes the current time as an argument (`now_ns`, from a
//! [`Clock`]) instead of reading a clock itself. That makes them
//! deterministic — tests drive them with a [`FakeClock`] — and lets the
//! gateway read the clock once per message (or per batch) and reuse the
//! timestamp for every check. Time is a `u64` of nanoseconds since an
//! arbitrary origin; a timestamp earlier than one already seen is treated as
//! "no time passed", never as negative time.
//!
//! # Integer token bucket
//!
//! A [`TokenBucket`] does not store fractional tokens. It stores *credit in
//! nanoseconds*: one token costs `ns_per_token` ns of credit, elapsed time
//! adds credit 1:1, and the bucket holds at most `burst × ns_per_token` ns.
//! Refill is therefore one subtraction, one add and one `min` — no division,
//! no rounding, no drift, however irregular the calls. (This is the same
//! arithmetic as GCRA, the "generic cell rate algorithm".)
//!
//! # Combining limits
//!
//! [`Throttle`] gives each session and each account its own bucket. An order
//! passes only if *both* have a token, and then takes one from each; if
//! either is empty, neither is charged. One noisy session therefore cannot
//! use up another session's budget (fairness between sessions), while the
//! account bucket caps the total across all of an account's sessions (no
//! escaping the limit by opening more connections).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use types::{AccountId, RiskReason};
// Used only by the implementation (unused in the learner template).
#[allow(unused_imports)]
use crate::idmap::IdMap;

/// A monotonic time source in nanoseconds.
pub trait Clock {
    /// Nanoseconds since an arbitrary fixed origin. Never decreases.
    fn now_ns(&self) -> u64;
}

impl<C: Clock + ?Sized> Clock for &C {
    #[inline]
    fn now_ns(&self) -> u64 {
        (**self).now_ns()
    }
}

/// The real clock: nanoseconds since the clock was created, from
/// [`Instant`] (`CLOCK_MONOTONIC` on Linux, ~20ns per read via the vDSO).
#[derive(Clone, Copy, Debug)]
pub struct MonotonicClock {
    origin: Instant,
}

impl MonotonicClock {
    /// A clock whose origin is now.
    pub fn new() -> MonotonicClock {
        MonotonicClock {
            origin: Instant::now(),
        }
    }
}

impl Default for MonotonicClock {
    fn default() -> Self {
        MonotonicClock::new()
    }
}

impl Clock for MonotonicClock {
    #[inline]
    fn now_ns(&self) -> u64 {
        Instant::now().duration_since(self.origin).as_nanos() as u64
    }
}

/// A manually driven clock for deterministic tests and simulations.
///
/// Clones share the same time (an `Arc<AtomicU64>`), so a test can hand one
/// clone to a [`Throttle`] and advance time through another. Each method is
/// one atomic operation.
#[derive(Clone, Debug, Default)]
pub struct FakeClock {
    now: Arc<AtomicU64>,
}

impl FakeClock {
    /// A clock reading `start_ns`.
    pub fn new(start_ns: u64) -> FakeClock {
        FakeClock {
            now: Arc::new(AtomicU64::new(start_ns)),
        }
    }

    /// Moves time forward by `ns`.
    pub fn advance(&self, ns: u64) {
        self.now.fetch_add(ns, Ordering::Relaxed);
    }

    /// Sets the time. Tests may move it backwards to check that limiters
    /// treat that as "no time passed".
    pub fn set(&self, now_ns: u64) {
        self.now.store(now_ns, Ordering::Relaxed);
    }
}

impl Clock for FakeClock {
    #[inline]
    fn now_ns(&self) -> u64 {
        self.now.load(Ordering::Relaxed)
    }
}

/// Token-bucket parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BucketConfig {
    /// Capacity in tokens: the largest burst allowed after a long idle
    /// period. `0` blocks everything (unless `ns_per_token` is 0).
    pub burst: u64,
    /// One token refills every `ns_per_token` nanoseconds. `0` means
    /// unlimited: every request passes.
    pub ns_per_token: u64,
}

impl BucketConfig {
    /// No limit at all.
    pub const UNLIMITED: BucketConfig = BucketConfig {
        burst: 1,
        ns_per_token: 0,
    };

    /// `rate` tokens per second with a burst of `burst` tokens. The
    /// interval is rounded *up* (`ceil(1e9 / rate)` ns), so the effective
    /// rate never exceeds `rate`. Panics if `rate` is 0.
    pub const fn per_second(rate: u64, burst: u64) -> BucketConfig {
        assert!(rate > 0, "rate must be positive");
        BucketConfig {
            burst,
            ns_per_token: 1_000_000_000u64.div_ceil(rate),
        }
    }
}

/// A token bucket with integer nanosecond credit (see the module docs).
///
/// 32 bytes, `Copy`; every operation is O(1) and allocation-free, and
/// `try_acquire` (the hot path) has no division and no branch on the
/// outcome.
// @student w17
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenBucket {
    /// Credit in ns, `0..=capacity`.
    credit: u64,
    /// `burst × ns_per_token`, saturating.
    capacity: u64,
    /// Credit one token costs (`ns_per_token`).
    cost: u64,
    /// Latest time seen.
    last: u64,
}

// @student w17
impl TokenBucket {
    /// A full bucket (`burst` tokens available) at time `now_ns`. The
    /// capacity `burst × ns_per_token` saturates at `u64::MAX` ns (~584
    /// years of credit).
    pub fn new(config: BucketConfig, now_ns: u64) -> TokenBucket {
        let capacity = config.burst.saturating_mul(config.ns_per_token);
        TokenBucket {
            credit: capacity,
            capacity,
            cost: config.ns_per_token,
            last: now_ns,
        }
    }

    /// Takes one token if one is available at `now_ns`; returns whether it
    /// did. O(1), no division.
    #[inline]
    pub fn try_acquire(&mut self, now_ns: u64) -> bool {
        self.refill(now_ns);
        let ok = self.has_token();
        self.take_if(ok);
        ok
    }

    /// Whole tokens available at `now_ns` (`u64::MAX` when unlimited).
    /// Refills, so it advances the bucket's notion of time. O(1).
    pub fn available(&mut self, now_ns: u64) -> u64 {
        self.refill(now_ns);
        self.credit.checked_div(self.cost).unwrap_or(u64::MAX)
    }
}

// @student-begin w17
impl TokenBucket {
    /// Adds the credit earned since the last call, capped at capacity.
    /// Earlier timestamps add nothing and do not move time back.
    #[inline]
    fn refill(&mut self, now_ns: u64) {
        let elapsed = now_ns.saturating_sub(self.last);
        self.last = self.last.max(now_ns);
        self.credit = self.credit.saturating_add(elapsed).min(self.capacity);
    }

    /// True if one token can be taken right now (after `refill`).
    #[inline]
    fn has_token(&self) -> bool {
        self.credit >= self.cost
    }

    /// Takes one token if `ok` (which must imply `has_token`). Branch-free:
    /// whether an order passes is data-dependent and mispredicts often.
    #[inline]
    fn take_if(&mut self, ok: bool) {
        self.credit -= self.cost * ok as u64;
    }
}
// @student-end

/// A sliding-window counter: at most `limit` requests per `window_ns`,
/// estimated from two fixed windows.
///
/// Time is cut into fixed windows of `window_ns`. The count in the sliding
/// window that ends now is estimated as the current window's count plus the
/// previous window's count weighted by how much of it the sliding window
/// still overlaps:
///
/// ```text
/// estimate = prev × (window − elapsed) / window + curr     (elapsed = now − current window start)
/// allow iff  prev × (window − elapsed) + (curr + 1) × window ≤ limit × window
/// ```
///
/// The comparison is multiplied out (in `u128`), so there is no division
/// and no rounding. Memory is O(1), unlike an exact log of timestamps; the
/// price is that the estimate assumes the previous window's requests were
/// spread evenly. It never admits more than `limit` within one fixed window.
// @student w17
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlidingWindow {
    limit: u64,
    window: u64,
    /// Start of the current fixed window.
    start: u64,
    /// Requests admitted in the previous fixed window.
    prev: u64,
    /// Requests admitted in the current fixed window.
    curr: u64,
}

// @student w17
impl SlidingWindow {
    /// At most `limit` requests per `window_ns`; the first fixed window
    /// starts at `now_ns`. Panics if `window_ns` is 0.
    pub fn new(limit: u64, window_ns: u64, now_ns: u64) -> SlidingWindow {
        assert!(window_ns > 0, "window must be positive");
        SlidingWindow {
            limit,
            window: window_ns,
            start: now_ns,
            prev: 0,
            curr: 0,
        }
    }

    /// Admits one request at `now_ns` if the estimate allows it. O(1); one
    /// division only when time crosses a window boundary.
    #[inline]
    pub fn try_acquire(&mut self, now_ns: u64) -> bool {
        let elapsed = self.roll(now_ns);
        let w = self.window as u128;
        let weighted = self.prev as u128 * (w - elapsed as u128) + (self.curr as u128 + 1) * w;
        if weighted <= self.limit as u128 * w {
            self.curr += 1;
            true
        } else {
            false
        }
    }

    /// The estimated number of requests in the sliding window ending at
    /// `now_ns`, rounded down. O(1).
    pub fn estimate(&mut self, now_ns: u64) -> u64 {
        let elapsed = self.roll(now_ns);
        let w = self.window as u128;
        (self.prev as u128 * (w - elapsed as u128) / w) as u64 + self.curr
    }
}

// @student-begin w17
impl SlidingWindow {
    /// Moves to the fixed window containing `now_ns` and returns the time
    /// elapsed in it (`< window`). Earlier timestamps count as `start`.
    #[inline]
    fn roll(&mut self, now_ns: u64) -> u64 {
        let since = now_ns.saturating_sub(self.start);
        if since < self.window {
            return since;
        }
        let windows = since / self.window;
        self.prev = if windows == 1 { self.curr } else { 0 };
        self.curr = 0;
        self.start += windows * self.window;
        since - windows * self.window
    }
}
// @student-end

/// Identifies a client session (one logged-on connection) at the gateway.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct SessionId(pub u64);

// @student-begin w17
/// A session's bucket, plus the dense index of the account bucket of the
/// last account it sent for — so the hot path needs one hash lookup.
#[derive(Clone, Copy, Debug)]
struct SessionBucket {
    bucket: TokenBucket,
    account: AccountId,
    account_idx: u32,
}

/// Charges one token to both buckets if both have one at `now_ns`;
/// otherwise charges neither.
#[inline]
fn charge(
    session: &mut TokenBucket,
    account: &mut TokenBucket,
    now_ns: u64,
) -> Result<(), RiskReason> {
    session.refill(now_ns);
    account.refill(now_ns);
    let ok = session.has_token() & account.has_token();
    session.take_if(ok);
    account.take_if(ok);
    if ok {
        Ok(())
    } else {
        Err(RiskReason::RateLimit)
    }
}

/// Index of `account`'s bucket in `buckets`, created full at `now_ns`.
fn account_bucket(
    index: &mut IdMap<AccountId, u32>,
    buckets: &mut Vec<TokenBucket>,
    account: AccountId,
    config: BucketConfig,
    now_ns: u64,
) -> u32 {
    *index.entry(account).or_insert_with(|| {
        buckets.push(TokenBucket::new(config, now_ns));
        (buckets.len() - 1) as u32
    })
}
// @student-end

/// Per-session and per-account order-rate throttle.
///
/// Every session and every account gets its own [`TokenBucket`], created
/// full on first use (or replaced by [`Throttle::set_account_limit`]). An
/// order passes only if both buckets have a token; it then takes one from
/// each, and a rejected order takes none.
// @student w17
#[derive(Debug)]
pub struct Throttle<C: Clock> {
    clock: C,
    session_config: BucketConfig,
    account_config: BucketConfig,
    sessions: IdMap<SessionId, SessionBucket>,
    /// Account id → index into `account_buckets`.
    account_index: IdMap<AccountId, u32>,
    account_buckets: Vec<TokenBucket>,
}

// @student w17
impl<C: Clock> Throttle<C> {
    /// A throttle giving every session `per_session` and every account
    /// `per_account`, reading time from `clock`.
    pub fn new(per_session: BucketConfig, per_account: BucketConfig, clock: C) -> Throttle<C> {
        Throttle {
            clock,
            session_config: per_session,
            account_config: per_account,
            sessions: IdMap::default(),
            account_index: IdMap::default(),
            account_buckets: Vec::new(),
        }
    }

    /// The injected clock.
    pub fn clock(&self) -> &C {
        &self.clock
    }

    /// [`Throttle::check_at`] at the clock's current time (one clock read).
    #[inline]
    pub fn check(&mut self, session: SessionId, account: AccountId) -> Result<(), RiskReason> {
        let now = self.clock.now_ns();
        self.check_at(session, account, now)
    }

    /// May one more order from `account` on `session` pass at `now_ns`?
    /// `Err(RiskReason::RateLimit)` if either bucket is empty, in which case
    /// neither is charged.
    ///
    /// O(1) with no clock read: one hash lookup (the session, which caches
    /// its account's bucket index; a second when the session switches
    /// accounts) and integer arithmetic. Never allocates once both the
    /// session and the account have been seen.
    #[inline]
    pub fn check_at(
        &mut self,
        session: SessionId,
        account: AccountId,
        now_ns: u64,
    ) -> Result<(), RiskReason> {
        // Fast path: a known session sending for the account it used last
        // (`get_mut` is ~3ns cheaper than `entry` on a hit).
        if let Some(s) = self.sessions.get_mut(&session) {
            if s.account == account {
                return charge(
                    &mut s.bucket,
                    &mut self.account_buckets[s.account_idx as usize],
                    now_ns,
                );
            }
        }
        self.check_slow(session, account, now_ns)
    }

    /// Gives `account` its own limit, starting from a full bucket now.
    /// O(1) amortized.
    pub fn set_account_limit(&mut self, account: AccountId, config: BucketConfig) {
        let now = self.clock.now_ns();
        let a = account_bucket(
            &mut self.account_index,
            &mut self.account_buckets,
            account,
            config,
            now,
        );
        self.account_buckets[a as usize] = TokenBucket::new(config, now);
    }

    /// Forgets a session's bucket (call on logout). The account bucket
    /// stays, so reconnecting does not reset the account's budget. O(1).
    pub fn remove_session(&mut self, session: SessionId) {
        self.sessions.remove(&session);
    }

    /// Whole tokens left in `session`'s bucket at `now_ns`, or `None` if
    /// the session has no bucket yet. O(1).
    pub fn session_tokens(&mut self, session: SessionId, now_ns: u64) -> Option<u64> {
        self.sessions
            .get_mut(&session)
            .map(|s| s.bucket.available(now_ns))
    }

    /// Whole tokens left in `account`'s bucket at `now_ns`, or `None` if
    /// the account has no bucket yet. O(1).
    pub fn account_tokens(&mut self, account: AccountId, now_ns: u64) -> Option<u64> {
        let &a = self.account_index.get(&account)?;
        Some(self.account_buckets[a as usize].available(now_ns))
    }
}

// @student-begin w17
impl<C: Clock> Throttle<C> {
    /// New session, or a session switching accounts: (re)resolve its
    /// account bucket, then charge.
    #[cold]
    #[inline(never)]
    fn check_slow(
        &mut self,
        session: SessionId,
        account: AccountId,
        now_ns: u64,
    ) -> Result<(), RiskReason> {
        let (index, buckets) = (&mut self.account_index, &mut self.account_buckets);
        let account_idx = account_bucket(index, buckets, account, self.account_config, now_ns);
        let session_config = self.session_config;
        let s = self
            .sessions
            .entry(session)
            .or_insert_with(|| SessionBucket {
                bucket: TokenBucket::new(session_config, now_ns),
                account,
                account_idx,
            });
        s.account = account;
        s.account_idx = account_idx;
        charge(&mut s.bucket, &mut buckets[account_idx as usize], now_ns)
    }
}
// @student-end
