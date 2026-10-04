//! Seeded random order streams.

use crate::EngineConfig;
#[allow(unused_imports)] // used by the reference implementation
use std::collections::HashMap;
#[allow(unused_imports)] // used by the reference implementation
use types::{
    Command, Event, InstrumentSpec, NewOrder, OrderId, OrderKind, Price, Qty, RejectReason, Rng,
    Side, TimeInForce,
};

/// Shape of a random order stream.
///
/// Prices are generated around a mid that random-walks one tick at a time:
/// passive orders rest 1..=`levels` ticks away from the mid (more of them
/// near the touch), aggressive orders cross it by 0..=3 ticks. Probabilities
/// are per command; whatever is left after cancel, reduce, market and
/// aggressive is a passive limit order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlowConfig {
    /// Seed: the whole stream is a function of the config.
    pub seed: u64,
    /// Tick, lot and band the stream is generated for.
    pub spec: InstrumentSpec,
    /// Starting mid price (a multiple of `spec.tick`).
    pub mid: Price,
    /// Passive orders rest within this many ticks of the mid.
    pub levels: i64,
    /// Order sizes are 1..=`max_lots` lots.
    pub max_lots: u64,
    /// Above this many (believed) live orders, half of all commands become
    /// cancels, so the book reaches a steady state.
    pub target_resting: usize,
    /// Per-command probability that the mid moves one tick (up or down).
    pub drift: f64,
    /// Cancel of a (believed) live order.
    pub p_cancel: f64,
    /// Reduce of a (believed) live order.
    pub p_reduce: f64,
    /// Market order (IOC, or FOK with probability `p_fok`).
    pub p_market: f64,
    /// Limit order priced through the mid.
    pub p_aggressive: f64,
    /// Fraction of limit orders that are IOC.
    pub p_ioc: f64,
    /// Fraction of limit and market orders that are FOK.
    pub p_fok: f64,
    /// Fraction of limit orders that are post-only.
    pub p_post_only: f64,
    /// Malformed command (zero qty, bad tick or lot, outside the band,
    /// duplicate id, unknown cancel, invalid reduce).
    pub p_invalid: f64,
}

impl Default for FlowConfig {
    /// A realistic mix around mid 10_000: ~55% passive adds, 20% cancels
    /// (half of all commands once the book exceeds 1000 orders), 15%
    /// aggressive limits, 3% market orders, 5% reduces, 2% malformed.
    fn default() -> Self {
        FlowConfig {
            seed: 1,
            spec: InstrumentSpec::default(),
            mid: Price(10_000),
            levels: 20,
            max_lots: 10,
            target_resting: 1_000,
            drift: 0.05,
            p_cancel: 0.20,
            p_reduce: 0.05,
            p_market: 0.03,
            p_aggressive: 0.15,
            p_ioc: 0.10,
            p_fok: 0.05,
            p_post_only: 0.10,
            p_invalid: 0.02,
        }
    }
}

impl FlowConfig {
    /// The engine configuration this stream is meant for: same spec, the
    /// starting mid as the band reference price.
    pub fn engine_config(&self) -> EngineConfig {
        EngineConfig::new(self.spec).with_reference_price(self.mid)
    }

    /// A varied configuration derived from `seed`, for differential tests:
    /// tick 1 or 5, lot 1 or 10, band on or off, narrow books (2–8 levels,
    /// so orders pile up on the same prices and FIFO matters), small
    /// resting targets and different order-type mixes.
    pub fn randomized(seed: u64) -> FlowConfig {
        let mut rng = Rng::new(seed ^ 0x5EED_F10E);
        let tick = if rng.chance(0.5) { 1 } else { 5 };
        let lot = if rng.chance(0.5) { 1 } else { 10 };
        let band_ticks = if rng.chance(0.4) {
            rng.range_i64(6, 30)
        } else {
            0
        };
        let pct = |rng: &mut Rng, lo: u64, hi: u64| rng.range(lo, hi) as f64 / 100.0;
        FlowConfig {
            seed,
            spec: InstrumentSpec {
                tick,
                lot,
                band_ticks,
                ..InstrumentSpec::default()
            },
            mid: Price(10_000),
            levels: rng.range_i64(2, 8),
            max_lots: rng.range(1, 8),
            target_resting: rng.range(10, 80) as usize,
            drift: pct(&mut rng, 0, 20),
            p_cancel: pct(&mut rng, 10, 35),
            p_reduce: pct(&mut rng, 0, 10),
            p_market: pct(&mut rng, 0, 8),
            p_aggressive: pct(&mut rng, 5, 30),
            p_ioc: pct(&mut rng, 0, 30),
            p_fok: pct(&mut rng, 0, 20),
            p_post_only: pct(&mut rng, 0, 20),
            p_invalid: pct(&mut rng, 0, 10),
        }
    }
}

// @student-begin w12
impl OrderFlow {
    fn unit(&mut self) -> f64 {
        (self.rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn lots(&mut self) -> Qty {
        Qty(self.rng.range(1, self.cfg.max_lots.max(1)) * self.cfg.spec.lot)
    }

    fn fresh_id(&mut self) -> OrderId {
        let id = OrderId(self.next_id);
        self.next_id += 1;
        id
    }

    fn tif(&mut self) -> TimeInForce {
        if self.rng.chance(self.cfg.p_fok) {
            TimeInForce::Fok
        } else if self.rng.chance(self.cfg.p_ioc) {
            TimeInForce::Ioc
        } else {
            TimeInForce::Gtc
        }
    }

    /// A limit order `ticks` away from the mid, positive = passive side.
    fn limit_at(&mut self, side: Side, ticks: i64) -> Command {
        let tick = self.cfg.spec.tick;
        let price = match side {
            Side::Buy => self.mid - ticks * tick,
            Side::Sell => self.mid + ticks * tick,
        };
        let id = self.fresh_id();
        let qty = self.lots();
        let tif = self.tif();
        let post_only = self.rng.chance(self.cfg.p_post_only);
        let order = NewOrder {
            kind: OrderKind::Limit {
                price: Price(price),
                post_only,
            },
            tif,
            ..NewOrder::limit(id.0, side, price, qty.0)
        };
        if tif == TimeInForce::Gtc {
            // Optimistically live; `observe` corrects fills and rejects.
            self.track(id, qty.0);
        }
        Command::New(order)
    }

    fn passive(&mut self) -> Command {
        let side = self.rng.side();
        let span = self.cfg.levels.max(1) as u64 - 1;
        // Triangular: more orders near the touch.
        let ticks = 1 + self.rng.range(0, span).min(self.rng.range(0, span)) as i64;
        self.limit_at(side, ticks)
    }

    fn aggressive(&mut self) -> Command {
        let side = self.rng.side();
        let ticks = -(self.rng.range_i64(0, 3));
        self.limit_at(side, ticks)
    }

    fn market(&mut self) -> Command {
        let side = self.rng.side();
        let id = self.fresh_id();
        let qty = self.lots();
        let mut order = NewOrder::market(id.0, side, qty.0);
        if self.rng.chance(self.cfg.p_fok) {
            order.tif = TimeInForce::Fok;
        }
        Command::New(order)
    }

    fn random_live(&mut self) -> Option<OrderId> {
        if self.live.is_empty() {
            return None;
        }
        Some(self.live[self.rng.range(0, self.live.len() as u64 - 1) as usize])
    }

    fn cancel(&mut self) -> Option<Command> {
        let id = self.random_live()?;
        self.untrack(id);
        Some(Command::Cancel { id })
    }

    fn reduce(&mut self) -> Option<Command> {
        let id = self.random_live()?;
        let lot = self.cfg.spec.lot;
        let lots = self.open[&id].1 / lot;
        if lots < 2 {
            return None;
        }
        let qty = self.rng.range(1, lots - 1) * lot;
        self.open.get_mut(&id).unwrap().1 = qty;
        Some(Command::Reduce { id, qty: Qty(qty) })
    }

    fn invalid(&mut self) -> Command {
        let spec = self.cfg.spec;
        let side = self.rng.side();
        match self.rng.range(0, 6) {
            0 => {
                let id = self.fresh_id();
                Command::New(NewOrder::limit(id.0, side, self.mid, 0))
            }
            1 if spec.lot > 1 => {
                let id = self.fresh_id();
                let qty = self.lots().0 + 1;
                Command::New(NewOrder::limit(id.0, side, self.mid, qty))
            }
            2 if spec.tick > 1 => {
                let id = self.fresh_id();
                let qty = self.lots();
                Command::New(NewOrder::limit(id.0, side, self.mid + 1, qty.0))
            }
            3 if spec.band_ticks > 0 => {
                let id = self.fresh_id();
                let qty = self.lots();
                let away = (spec.band_ticks + self.rng.range_i64(2, 6)) * spec.tick;
                let price = if side == Side::Buy {
                    self.mid - away
                } else {
                    self.mid + away
                };
                Command::New(NewOrder::limit(id.0, side, price, qty.0))
            }
            4 => match self.random_live() {
                Some(dup) => {
                    let qty = self.lots();
                    Command::New(NewOrder::limit(dup.0, side, self.mid, qty.0))
                }
                None => Command::Cancel {
                    id: OrderId(self.next_id + 1_000_000_000),
                },
            },
            5 => match self.random_live() {
                Some(id) => {
                    let qty = if self.rng.chance(0.5) {
                        0
                    } else {
                        self.open[&id].1 + spec.lot
                    };
                    Command::Reduce { id, qty: Qty(qty) }
                }
                None => Command::Cancel {
                    id: OrderId(self.next_id + 1_000_000_000),
                },
            },
            _ => Command::Cancel {
                id: OrderId(self.next_id + 1_000_000_000),
            },
        }
    }

    fn track(&mut self, id: OrderId, qty: u64) {
        if let Some(entry) = self.open.get_mut(&id) {
            entry.1 = qty;
            return;
        }
        self.open.insert(id, (self.live.len(), qty));
        self.live.push(id);
    }

    fn untrack(&mut self, id: OrderId) {
        if let Some((pos, _)) = self.open.remove(&id) {
            self.live.swap_remove(pos);
            if let Some(&moved) = self.live.get(pos) {
                self.open.get_mut(&moved).unwrap().0 = pos;
            }
        }
    }

    fn consume(&mut self, id: OrderId, qty: u64) {
        if let Some(entry) = self.open.get_mut(&id) {
            entry.1 = entry.1.saturating_sub(qty);
            if entry.1 == 0 {
                self.untrack(id);
            }
        }
    }
}
// @student-end

/// A deterministic stream of [`Command`]s shaped by a [`FlowConfig`].
///
/// The generator keeps its own idea of which orders are live (every GTC
/// limit it issues, minus the ones it cancels) to aim cancels and reduces.
/// Fed the engine's events through [`observe`](OrderFlow::observe), that
/// idea becomes exact (fills, expiries and rejections are accounted);
/// without feedback, some cancels target orders that already traded and are
/// rejected as unknown — also worth testing. The stream is a pure function
/// of the config (plus the observed events, if any).
// @student w12
#[derive(Clone, Debug)]
pub struct OrderFlow {
    cfg: FlowConfig,
    rng: Rng,
    mid: i64,
    next_id: u64,
    /// Believed-live order ids (for O(1) random choice) …
    live: Vec<OrderId>,
    /// … and id → (position in `live`, believed remaining qty).
    open: HashMap<OrderId, (usize, u64)>,
}

// @student w12
impl OrderFlow {
    /// A generator at the start of the stream described by `cfg`.
    pub fn new(cfg: FlowConfig) -> OrderFlow {
        assert!(
            cfg.spec.tick > 0 && cfg.spec.lot > 0,
            "tick and lot must be positive"
        );
        OrderFlow {
            cfg,
            rng: Rng::new(cfg.seed),
            mid: cfg.mid.0 - cfg.mid.0.rem_euclid(cfg.spec.tick),
            next_id: 1,
            live: Vec::new(),
            open: HashMap::new(),
        }
    }

    /// The next command. O(1).
    pub fn next_command(&mut self) -> Command {
        if self.rng.chance(self.cfg.drift) {
            let step = if self.rng.next_u64() & 1 == 0 { 1 } else { -1 };
            self.mid += step * self.cfg.spec.tick;
        }
        if self.rng.chance(self.cfg.p_invalid) {
            return self.invalid();
        }
        let crowded = self.live.len() > self.cfg.target_resting && self.rng.chance(0.5);
        let x = self.unit();
        let c = &self.cfg;
        let (t_cancel, t_reduce) = (c.p_cancel, c.p_cancel + c.p_reduce);
        let (t_market, t_aggressive) = (
            t_reduce + c.p_market,
            t_reduce + c.p_market + c.p_aggressive,
        );
        let picked = if crowded || x < t_cancel {
            self.cancel()
        } else if x < t_reduce {
            self.reduce()
        } else if x < t_market {
            Some(self.market())
        } else if x < t_aggressive {
            Some(self.aggressive())
        } else {
            None
        };
        picked.unwrap_or_else(|| self.passive())
    }

    /// The next `n` commands.
    pub fn take_commands(&mut self, n: usize) -> Vec<Command> {
        (0..n).map(|_| self.next_command()).collect()
    }

    /// Feeds back the events the engine produced for `cmd`, so the
    /// generator's live set matches the book exactly. O(events).
    pub fn observe(&mut self, cmd: &Command, events: &[Event]) {
        for event in events {
            match *event {
                Event::Rested { id, qty, .. } => self.track(id, qty.0),
                Event::Trade {
                    taker, maker, qty, ..
                } => {
                    self.consume(taker, qty.0);
                    self.consume(maker, qty.0);
                }
                Event::Expired { id, .. } | Event::Cancelled { id, .. } => self.untrack(id),
                Event::Reduced { id, remaining } => self.track(id, remaining.0),
                // A rejected new order never rested; rejected cancels and
                // reduces leave a live order live.
                Event::Rejected { id, .. } => {
                    if matches!(cmd, Command::New(o) if o.id == id)
                        && !matches!(
                            event,
                            Event::Rejected {
                                reason: RejectReason::DuplicateOrderId,
                                ..
                            }
                        )
                    {
                        self.untrack(id);
                    }
                }
                Event::Accepted { .. } => {}
            }
        }
    }

    /// The current mid price of the random walk.
    pub fn mid(&self) -> Price {
        Price(self.mid)
    }

    /// How many orders the generator believes are resting.
    pub fn live_orders(&self) -> usize {
        self.live.len()
    }
}

impl Iterator for OrderFlow {
    type Item = Command;

    /// Never ends.
    fn next(&mut self) -> Option<Command> {
        Some(self.next_command())
    }
}
