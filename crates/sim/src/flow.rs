//! Synthetic order flow for demos, the live WASM page and the terminal.
//!
//! [`OrderFlow`] is a zero-intelligence trader population around a fair
//! value that random-walks with mean reversion: mostly passive limit orders
//! near the touch, some aggressive limits, IOC and market orders, and
//! cancels of its own resting orders. Feed every engine event back through
//! [`OrderFlow::on_event`] so cancels only target orders that still rest.
//!
//! It is infrastructure, not a curriculum exercise: deterministic for a
//! seed, no allocation per command once warmed up, no clock.

use std::collections::HashMap;
use types::{Command, Event, InstrumentSpec, NewOrder, OrderId, Rng, Side, TimeInForce};

/// Shape of the simulated flow. Prices and distances are in ticks.
#[derive(Clone, Copy, Debug)]
pub struct FlowConfig {
    pub spec: InstrumentSpec,
    /// Initial (and long-run mean) fair value, in price units (multiple of the tick).
    pub fair_value: i64,
    /// Standard deviation of the fair-value step per command, in ticks.
    pub volatility: f64,
    /// Strength of the pull back to the initial fair value per command (0..1).
    pub mean_reversion: f64,
    /// Passive orders rest up to this many ticks behind the fair value.
    pub depth_ticks: i64,
    /// Share of commands that cancel a resting order.
    pub p_cancel: f64,
    /// Share of new orders that are market orders.
    pub p_market: f64,
    /// Share of new orders that are IOC limits.
    pub p_ioc: f64,
    /// Share of limit orders priced through the fair value (aggressive).
    pub p_aggressive: f64,
    /// Order size in lots is 1..=max_lots, skewed small.
    pub max_lots: u64,
    /// Account id stamped on generated orders.
    pub account: u32,
    /// First order id; ids increase by one per new order.
    pub first_id: u64,
}

impl Default for FlowConfig {
    fn default() -> Self {
        FlowConfig {
            spec: InstrumentSpec::default(),
            fair_value: 10_000,
            volatility: 0.35,
            mean_reversion: 0.0005,
            depth_ticks: 25,
            p_cancel: 0.35,
            p_market: 0.04,
            p_ioc: 0.08,
            p_aggressive: 0.10,
            max_lots: 50,
            account: 1,
            first_id: 1,
        }
    }
}

/// Generates commands one at a time; see the module docs.
pub struct OrderFlow {
    cfg: FlowConfig,
    rng: Rng,
    fair: f64,
    next_id: u64,
    /// Resting orders this flow owns: id -> index in `resting_ids`.
    resting: HashMap<u64, usize>,
    resting_ids: Vec<u64>,
}

impl OrderFlow {
    pub fn new(cfg: FlowConfig, seed: u64) -> OrderFlow {
        OrderFlow {
            fair: cfg.fair_value as f64,
            next_id: cfg.first_id,
            rng: Rng::new(seed),
            resting: HashMap::with_capacity(4096),
            resting_ids: Vec::with_capacity(4096),
            cfg,
        }
    }

    pub fn config(&self) -> &FlowConfig {
        &self.cfg
    }

    /// Current fair value, rounded to the tick.
    pub fn fair_value(&self) -> i64 {
        self.round_tick(self.fair)
    }

    /// Orders of this flow currently resting (as far as events reported).
    pub fn resting(&self) -> usize {
        self.resting_ids.len()
    }

    /// Whether `id` was issued by this flow.
    pub fn owns(&self, id: OrderId) -> bool {
        id.0 >= self.cfg.first_id && id.0 < self.next_id
    }

    /// The next command to send.
    pub fn next_command(&mut self) -> Command {
        self.step_fair_value();
        if !self.resting_ids.is_empty() && self.rng.chance(self.cfg.p_cancel) {
            let i = self.rng.range(0, self.resting_ids.len() as u64 - 1) as usize;
            let id = self.resting_ids[i];
            self.forget(id);
            return Command::Cancel { id: OrderId(id) };
        }
        Command::New(self.next_order())
    }

    /// Track resting orders from engine events (all events, any owner).
    pub fn on_event(&mut self, event: &Event) {
        match *event {
            Event::Rested { id, .. } if self.owns(id) => {
                if !self.resting.contains_key(&id.0) {
                    self.resting.insert(id.0, self.resting_ids.len());
                    self.resting_ids.push(id.0);
                }
            }
            Event::Cancelled { id, .. } | Event::Expired { id, .. } => self.forget(id.0),
            Event::Reduced { id, remaining } if remaining.0 == 0 => self.forget(id.0),
            _ => {}
        }
    }

    /// A maker was fully filled: callers that see fills (book snapshots,
    /// L2 deltas) can report it so cancels stop targeting it. Cancelling a
    /// filled order is harmless (the engine rejects it), only wasteful.
    pub fn on_filled(&mut self, id: OrderId) {
        self.forget(id.0);
    }

    fn forget(&mut self, id: u64) {
        if let Some(i) = self.resting.remove(&id) {
            self.resting_ids.swap_remove(i);
            if let Some(&moved) = self.resting_ids.get(i) {
                self.resting.insert(moved, i);
            }
        }
    }

    fn next_order(&mut self) -> NewOrder {
        let id = self.next_id;
        self.next_id += 1;
        let side = self.rng.side();
        let lots = self.size_lots();
        let qty = lots * self.cfg.spec.lot;
        let order = if self.rng.chance(self.cfg.p_market) {
            NewOrder::market(id, side, qty)
        } else {
            let tick = self.cfg.spec.tick;
            let aggressive = self.rng.chance(self.cfg.p_aggressive);
            // Passive: behind the fair value, skewed toward the touch.
            // Aggressive: a few ticks through it.
            let ticks = if aggressive {
                -(self.rng.range_i64(1, 4))
            } else {
                let u = (self.rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
                1 + (u * u * self.cfg.depth_ticks as f64) as i64
            };
            let fair = self.fair_value();
            let price = match side {
                Side::Buy => fair - ticks * tick,
                Side::Sell => fair + ticks * tick,
            }
            .max(tick);
            let o = NewOrder::limit(id, side, price, qty);
            if self.rng.chance(self.cfg.p_ioc) {
                o.with_tif(TimeInForce::Ioc)
            } else {
                o
            }
        };
        order.with_account(self.cfg.account)
    }

    fn size_lots(&mut self) -> u64 {
        // Mostly small, occasionally large: 1 + max * u^3.
        let u = (self.rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        1 + ((self.cfg.max_lots - 1) as f64 * u * u * u) as u64
    }

    fn step_fair_value(&mut self) {
        // Sum of uniforms ~ normal(0, 1).
        let mut z = -6.0;
        for _ in 0..12 {
            z += (self.rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        }
        let tick = self.cfg.spec.tick as f64;
        let pull = (self.cfg.fair_value as f64 - self.fair) * self.cfg.mean_reversion;
        self.fair = (self.fair + z * self.cfg.volatility * tick + pull).max(tick * 10.0);
    }

    fn round_tick(&self, v: f64) -> i64 {
        let tick = self.cfg.spec.tick;
        ((v / tick as f64).round() as i64).max(1) * tick
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_valid() {
        let cfg = FlowConfig::default();
        let mut a = OrderFlow::new(cfg, 7);
        let mut b = OrderFlow::new(cfg, 7);
        for _ in 0..10_000 {
            let (ca, cb) = (a.next_command(), b.next_command());
            assert_eq!(ca, cb);
            if let Command::New(o) = ca {
                assert!(o.qty.0 > 0 && o.qty.0 % cfg.spec.lot == 0);
                if let Some(p) = o.limit_price() {
                    assert!(p.0 > 0 && p.0 % cfg.spec.tick == 0);
                }
                // Pretend every limit rests so cancels get exercised.
                if let Some(p) = o.limit_price() {
                    a.on_event(&Event::Rested { id: o.id, side: o.side, price: p, qty: o.qty });
                    b.on_event(&Event::Rested { id: o.id, side: o.side, price: p, qty: o.qty });
                }
            }
        }
        assert!(a.resting() > 0);
    }

    #[test]
    fn cancels_target_only_resting_orders() {
        let mut f = OrderFlow::new(FlowConfig { p_cancel: 0.5, ..FlowConfig::default() }, 3);
        let mut live = std::collections::HashSet::new();
        for _ in 0..5_000 {
            match f.next_command() {
                Command::New(o) => {
                    if let Some(p) = o.limit_price() {
                        live.insert(o.id);
                        f.on_event(&Event::Rested { id: o.id, side: o.side, price: p, qty: o.qty });
                    }
                }
                Command::Cancel { id } => assert!(live.remove(&id), "cancelled {id:?} which never rested"),
                Command::Reduce { .. } => unreachable!(),
            }
        }
    }
}
