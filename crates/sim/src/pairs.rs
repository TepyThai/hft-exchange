//! Week 43 — statistical arbitrage basics: trading the spread between two
//! cointegrated instruments.
//!
//! Two prices can each wander without bound while a combination of them
//! stays put. If `Y ≈ β·X + μ + s` with `s` mean-reverting, then the
//! **spread** `Y − β·X` keeps returning to its mean, and a deviation is a
//! trade: sell the spread (sell `Y`, buy `β` of `X`) when it is rich, buy
//! it when it is cheap, unwind when it is back. The legs hedge each other,
//! so the common random walk of `X` cancels out of the P&L.
//!
//! * [`PairMarket`] (provided) simulates exactly that world: two engines,
//!   fair values `X` (a random walk) and `Y = β·X + μ + s` with `s` an
//!   AR(1) process `s ← φ·s + σ·ε`, and a liquidity ladder around each fair
//!   value, re-quoted every step.
//! * [`RollingStats`] — mean and variance over a sliding window, updated in
//!   O(1) with Welford's method (numerically stable, unlike
//!   `Σx² − n·mean²`, which loses most digits when the variance is small
//!   relative to the mean — exactly the case for prices).
//! * [`PairsTrader`] — z-score of the spread against its rolling window,
//!   entry / exit thresholds with hysteresis, scaling in, position limits,
//!   and IOC orders on both legs.
//!
//! # Signal rules
//!
//! `z = (s − mean) / std` against the *previous* `window` spreads (strictly
//! past data — no look-ahead), then `s` joins the window. With `u` the
//! current position in spread units (long spread = long `Y`, short `X`):
//!
//! ```text
//! units(z) = min(max_units, 1 + floor((|z| − entry) / step))   for |z| ≥ entry
//!
//! flat  (u = 0): z ≥ entry → −units(z);  z ≤ −entry → +units(z);  else 0
//! short (u < 0): z ≤ −entry → +units(z) (flip);  z ≤ exit → 0 (close);
//!                z ≥ entry → −max(|u|, units(z)) (scale in);  else u (hold)
//! long  (u > 0): z ≥ entry → −units(z) (flip);  z ≥ −exit → 0 (close);
//!                z ≤ −entry → max(u, units(z)) (scale in);  else u (hold)
//! ```
//!
//! `step = 0` means a single unit. A position is only ever scaled *in*
//! while the trade is on; it is closed all at once at the exit threshold.

use crate::micro::TopOfBook;
use crate::scenario::Inventory;
use engine::{Engine, EngineConfig};
use types::{AccountId, Command, Event, InstrumentId, InstrumentSpec, OrderId, Rng, Side};
// Used only by the implementation (unused in the learner template).
#[allow(unused_imports)]
use {
    crate::scenario::Outbox,
    types::{Price, Qty},
};

/// Index of the `X` leg (the hedge) in pairs of legs.
pub const X: usize = 0;
/// Index of the `Y` leg (the one quoted in spread units).
pub const Y: usize = 1;

// ---------------------------------------------------------------------------
// Rolling statistics

/// Mean and variance of the last `window` values, updated in O(1) per value
/// with Welford's method; the window is a ring buffer allocated once.
///
/// Adding `x` to `n` values: `d = x − mean; mean += d/n; M2 += d·(x − mean)`.
/// Replacing the oldest value `o` by `x` once full:
/// `mean' = mean + (x − o)/n; M2 += (x − o)·(x − mean' + o − mean)`.
/// The population variance is `M2 / n`. Every `window` replacements the
/// sums are recomputed exactly from the buffer (amortized O(1)), so
/// rounding error cannot accumulate over an unbounded stream.
// @student w43
#[derive(Clone, Debug)]
pub struct RollingStats {
    buf: Vec<f64>,
    window: usize,
    /// Index of the oldest value once full.
    head: usize,
    mean: f64,
    m2: f64,
}

// @student w43
impl RollingStats {
    /// An empty window of `window` values.
    ///
    /// # Panics
    /// If `window` is 0.
    pub fn new(window: usize) -> RollingStats {
        assert!(window > 0, "window must hold at least one value");
        RollingStats {
            buf: Vec::with_capacity(window),
            window,
            head: 0,
            mean: 0.0,
            m2: 0.0,
        }
    }

    /// The window size.
    pub fn window(&self) -> usize {
        self.window
    }

    /// Values currently in the window (≤ `window`).
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// True before the first value.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// True once `window` values have been pushed.
    pub fn is_full(&self) -> bool {
        self.buf.len() == self.window
    }

    /// Adds `x`, evicting the oldest value if the window is full. O(1).
    pub fn push(&mut self, x: f64) {
        if self.buf.len() < self.window {
            self.buf.push(x);
            let n = self.buf.len() as f64;
            let d = x - self.mean;
            self.mean += d / n;
            self.m2 += d * (x - self.mean);
        } else {
            let old = std::mem::replace(&mut self.buf[self.head], x);
            self.head = (self.head + 1) % self.window;
            let n = self.window as f64;
            let mean = self.mean + (x - old) / n;
            self.m2 += (x - old) * (x - mean + old - self.mean);
            self.mean = mean;
            if self.head == 0 {
                self.reanchor();
            }
        }
        // Rounding can push a zero variance a hair below zero.
        self.m2 = self.m2.max(0.0);
    }

    /// Mean of the window; `None` if empty. O(1).
    pub fn mean(&self) -> Option<f64> {
        (!self.buf.is_empty()).then_some(self.mean)
    }

    /// Population variance `M2 / n`; `None` if empty. O(1).
    pub fn variance(&self) -> Option<f64> {
        (!self.buf.is_empty()).then(|| self.m2 / self.buf.len() as f64)
    }

    /// Population standard deviation; `None` if empty. O(1).
    pub fn std_dev(&self) -> Option<f64> {
        self.variance().map(f64::sqrt)
    }

    /// `(x − mean) / std` against the window; `None` with fewer than two
    /// values or a zero standard deviation. O(1).
    pub fn zscore(&self, x: f64) -> Option<f64> {
        let sd = self.std_dev()?;
        (self.buf.len() >= 2 && sd > 0.0).then(|| (x - self.mean) / sd)
    }
}

// @student-begin w43
impl RollingStats {
    /// Recomputes the mean and `M2` exactly (two passes over the window).
    /// Every replacement adds a little rounding error to the running sums
    /// and, unlike growing-only Welford, nothing ever removes it; doing this
    /// once per `window` replacements bounds the drift to one window's
    /// worth of updates at an amortized O(1) per push.
    fn reanchor(&mut self) {
        let n = self.buf.len() as f64;
        self.mean = self.buf.iter().sum::<f64>() / n;
        self.m2 = self
            .buf
            .iter()
            .map(|v| (v - self.mean) * (v - self.mean))
            .sum();
    }
}
// @student-end

// ---------------------------------------------------------------------------
// The simulated pair (provided)

/// Parameters of a [`PairMarket`]. Prices in price units (tick 1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PairConfig {
    /// Initial fair value of `X`.
    pub x0: f64,
    /// The hedge ratio: `Y = beta·X + mu + s`.
    pub beta: f64,
    /// Constant offset of `Y` over `beta·X`.
    pub mu: f64,
    /// Standard deviation of `X`'s random-walk step.
    pub sigma_x: f64,
    /// AR(1) coefficient of the spread `s` (`|phi| < 1`: mean-reverting;
    /// `1`: a random walk, no cointegration).
    pub phi: f64,
    /// Standard deviation of the spread's innovation.
    pub sigma_s: f64,
    /// The ladder's best bid / ask sit this many ticks from the fair value.
    pub half_spread_ticks: i64,
    /// Price levels per side of each ladder (one tick apart).
    pub levels: u32,
    /// Quantity per ladder level.
    pub level_qty: u64,
}

impl Default for PairConfig {
    fn default() -> Self {
        PairConfig {
            x0: 10_000.0,
            beta: 1.5,
            mu: 0.0,
            sigma_x: 1.0,
            phi: 0.95,
            sigma_s: 3.0,
            half_spread_ticks: 1,
            levels: 5,
            level_qty: 20,
        }
    }
}

/// Two instruments, each on its own matching engine, with cointegrated
/// fair values and a liquidity provider re-quoting a ladder around each
/// fair value every [`step`](PairMarket::step). Deterministic for a seed.
///
/// The ladder uses account 0 and ids from `1 << 50`; traders must use
/// other ids. Provided test bed, not an exercise.
pub struct PairMarket {
    cfg: PairConfig,
    rng: Rng,
    engines: [Engine; 2],
    x: f64,
    s: f64,
    ladder: [Vec<OrderId>; 2],
    next_id: u64,
    events: Vec<Event>,
    steps: u64,
}

/// Account of the ladder liquidity provider.
pub const LADDER_ACCOUNT: AccountId = AccountId(0);
const LADDER_FIRST_ID: u64 = 1 << 50;

impl PairMarket {
    /// A market at `x0` with the spread at 0 and both ladders posted.
    pub fn new(cfg: PairConfig, seed: u64) -> PairMarket {
        let engine = |id| {
            let spec = InstrumentSpec {
                id: InstrumentId(id),
                ..InstrumentSpec::default()
            };
            Engine::with_config(EngineConfig::new(spec))
        };
        let mut m = PairMarket {
            cfg,
            rng: Rng::new(seed),
            engines: [engine(1), engine(2)],
            x: cfg.x0,
            s: 0.0,
            ladder: [Vec::new(), Vec::new()],
            next_id: LADDER_FIRST_ID,
            events: Vec::with_capacity(64),
            steps: 0,
        };
        m.requote();
        m
    }

    /// Its parameters.
    pub fn config(&self) -> &PairConfig {
        &self.cfg
    }

    /// Steps taken.
    pub fn steps(&self) -> u64 {
        self.steps
    }

    /// Advances the fair values one step and re-quotes both ladders.
    /// O(levels) engine commands per leg.
    pub fn step(&mut self) {
        let c = self.cfg;
        self.s = c.phi * self.s + c.sigma_s * normal(&mut self.rng);
        self.x += c.sigma_x * normal(&mut self.rng);
        self.steps += 1;
        self.requote();
    }

    /// Fair value of `leg` ([`X`] or [`Y`]).
    pub fn fair(&self, leg: usize) -> f64 {
        match leg {
            X => self.x,
            _ => self.cfg.beta * self.x + self.cfg.mu + self.s,
        }
    }

    /// The true spread `s` (unobservable to a trader; for tests).
    pub fn true_spread(&self) -> f64 {
        self.s
    }

    /// Top of `leg`'s book.
    pub fn top(&self, leg: usize) -> TopOfBook {
        TopOfBook::from_engine(&self.engines[leg])
    }

    /// `leg`'s engine (read-only).
    pub fn engine(&self, leg: usize) -> &Engine {
        &self.engines[leg]
    }

    /// Sends a trader's command to `leg`'s engine and appends its events to
    /// `out`.
    pub fn send(&mut self, leg: usize, cmd: &Command, out: &mut Vec<Event>) {
        self.engines[leg].process(cmd, out);
    }

    fn requote(&mut self) {
        for leg in [X, Y] {
            let engine = &mut self.engines[leg];
            for id in self.ladder[leg].drain(..) {
                self.events.clear();
                engine.process(&Command::Cancel { id }, &mut self.events);
            }
            let fair = match leg {
                X => self.x,
                _ => self.cfg.beta * self.x + self.cfg.mu + self.s,
            };
            let center = fair.round() as i64;
            for k in 0..self.cfg.levels as i64 {
                for (side, price) in [
                    (Side::Buy, center - self.cfg.half_spread_ticks - k),
                    (Side::Sell, center + self.cfg.half_spread_ticks + k),
                ] {
                    let id = OrderId(self.next_id);
                    self.next_id += 1;
                    let order =
                        types::NewOrder::limit(id.0, side, price.max(1), self.cfg.level_qty)
                            .with_account(LADDER_ACCOUNT.0);
                    self.events.clear();
                    engine.process(&Command::New(order), &mut self.events);
                    self.ladder[leg].push(id);
                }
            }
        }
    }
}

/// Approximately standard normal: a sum of 12 uniforms minus 6.
fn normal(rng: &mut Rng) -> f64 {
    (0..12)
        .map(|_| (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64)
        .sum::<f64>()
        - 6.0
}

// ---------------------------------------------------------------------------
// The strategy

/// Parameters of a [`PairsTrader`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PairsConfig {
    /// `β` in the spread `Y − β·X` (and lots of `X` per lot of `Y`).
    pub hedge_ratio: f64,
    /// Spreads in the rolling window.
    pub window: usize,
    /// Open (or flip) when `|z|` reaches this.
    pub entry_z: f64,
    /// Close when `z` is back within this of the mean.
    pub exit_z: f64,
    /// One more unit per this much `|z|` beyond `entry_z` (0: one unit).
    pub step_z: f64,
    /// Position limit, in spread units.
    pub max_units: u32,
    /// Lots of `Y` per spread unit (`X`: `−round(units·unit_qty·β)`).
    pub unit_qty: u64,
    /// IOC orders are priced this many ticks through the touch.
    pub slippage_ticks: i64,
    /// The instruments' tick size.
    pub tick: i64,
    /// The trader's account.
    pub account: AccountId,
}

impl Default for PairsConfig {
    fn default() -> Self {
        PairsConfig {
            hedge_ratio: 1.5,
            window: 100,
            entry_z: 2.0,
            exit_z: 0.5,
            step_z: 1.0,
            max_units: 3,
            unit_qty: 2,
            slippage_ticks: 1,
            tick: 1,
            account: AccountId(2),
        }
    }
}

/// A pairs trader; see the module docs for the signal rules.
///
/// Each [`step`](PairsTrader::step) it reads both mids, updates the z-score
/// and target units, then sends one IOC order per leg for the difference
/// between the wanted and the filled position. Leg positions can never
/// exceed what `max_units` implies: orders are sized to the difference and
/// fills never exceed an order. O(1) per step besides the engines' work.
// @student w43
pub struct PairsTrader {
    cfg: PairsConfig,
    stats: RollingStats,
    units: i32,
    legs: [Inventory; 2],
    outs: [Outbox; 2],
    events: Vec<Event>,
    last_z: Option<f64>,
}

// @student w43
impl PairsTrader {
    /// A flat trader with an empty window.
    pub fn new(cfg: PairsConfig) -> PairsTrader {
        let out = |leg: u32| Outbox::new(cfg.account, InstrumentId(leg + 1), 1 << 40);
        PairsTrader {
            cfg,
            stats: RollingStats::new(cfg.window),
            units: 0,
            legs: [Inventory::default(); 2],
            outs: [out(0), out(1)],
            events: Vec::with_capacity(16),
            last_z: None,
        }
    }

    /// Its parameters.
    pub fn config(&self) -> &PairsConfig {
        &self.cfg
    }

    /// The spread `py − β·px`.
    pub fn spread(&self, px: f64, py: f64) -> f64 {
        py - self.cfg.hedge_ratio * px
    }

    /// The units wanted at `z` when holding `current` (see the module docs).
    /// Pure; O(1).
    pub fn target_units(&self, z: f64, current: i32) -> i32 {
        let c = &self.cfg;
        let size = |z: f64| {
            let extra = if c.step_z > 0.0 {
                ((z.abs() - c.entry_z) / c.step_z).floor() as i64
            } else {
                0
            };
            (1 + extra.max(0)).min(c.max_units as i64) as i32
        };
        // Units the signal alone asks for in the direction of a trade (0
        // when |z| is inside the entry band).
        let fresh = if z.abs() >= c.entry_z { size(z) } else { 0 };
        match current {
            0 if z >= c.entry_z => -fresh,
            0 if z <= -c.entry_z => fresh,
            0 => 0,
            // Short the spread (it was rich).
            u if u < 0 => {
                if z <= -c.entry_z {
                    fresh
                } else if z <= c.exit_z {
                    0
                } else {
                    -(u.abs().max(if z > 0.0 { fresh } else { 0 }))
                }
            }
            // Long the spread (it was cheap).
            u => {
                if z >= c.entry_z {
                    -fresh
                } else if z >= -c.exit_z {
                    0
                } else {
                    u.max(if z < 0.0 { fresh } else { 0 })
                }
            }
        }
    }

    /// Feeds the two mids: computes `z` against the window (if full), updates
    /// the target units, then adds the spread to the window. Returns `z`.
    /// O(1).
    pub fn on_prices(&mut self, px: f64, py: f64) -> Option<f64> {
        let s = self.spread(px, py);
        let z = if self.stats.is_full() {
            self.stats.zscore(s)
        } else {
            None
        };
        self.stats.push(s);
        if let Some(z) = z {
            self.units = self.target_units(z, self.units);
        }
        self.last_z = z;
        z
    }

    /// Current target, in spread units (> 0: long the spread).
    pub fn units(&self) -> i32 {
        self.units
    }

    /// The last z-score computed.
    pub fn last_z(&self) -> Option<f64> {
        self.last_z
    }

    /// Leg positions wanted for the current units: `[X, Y]` in lots.
    pub fn desired_legs(&self) -> [i64; 2] {
        let y = self.units as i64 * self.cfg.unit_qty as i64;
        let x = -(y as f64 * self.cfg.hedge_ratio).round() as i64;
        [x, y]
    }

    /// Fills, position and cash of `leg` ([`X`] or [`Y`]).
    pub fn leg(&self, leg: usize) -> &Inventory {
        &self.legs[leg]
    }

    /// Mark-to-market P&L of both legs at `marks = [X, Y]`.
    pub fn pnl(&self, marks: [f64; 2]) -> f64 {
        self.legs[X].pnl_at(marks[X]) + self.legs[Y].pnl_at(marks[Y])
    }

    /// One trading step against `market`: observe, decide, trade.
    pub fn step(&mut self, market: &mut PairMarket) {
        let tops = [market.top(X), market.top(Y)];
        if let (Some(px), Some(py)) = (tops[X].mid(), tops[Y].mid()) {
            self.on_prices(px, py);
        }
        let want = self.desired_legs();
        for leg in [X, Y] {
            let diff = want[leg] - self.legs[leg].position;
            let side = if diff > 0 { Side::Buy } else { Side::Sell };
            let touch = match side {
                Side::Buy => tops[leg].ask,
                Side::Sell => tops[leg].bid,
            };
            let (Some(level), true) = (touch, diff != 0) else {
                continue;
            };
            let through = self.cfg.slippage_ticks * self.cfg.tick;
            let price = match side {
                Side::Buy => level.price.0 + through,
                Side::Sell => (level.price.0 - through).max(self.cfg.tick),
            };
            let id = self.outs[leg].ioc(side, Price(price), Qty(diff.unsigned_abs()));
            self.events.clear();
            for cmd in self.outs[leg].drain() {
                market.send(leg, &cmd, &mut self.events);
            }
            for e in &self.events {
                if let Event::Trade {
                    taker, price, qty, ..
                } = *e
                {
                    if taker == id {
                        self.legs[leg].apply(side, price, qty);
                    }
                }
            }
        }
    }
}
