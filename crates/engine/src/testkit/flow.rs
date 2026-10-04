//! Seeded random order streams.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
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

// TODO(w12): implement — see the week guide on tepy.dev/hft.

/// A deterministic stream of [`Command`]s shaped by a [`FlowConfig`].
///
/// The generator keeps its own idea of which orders are live (every GTC
/// limit it issues, minus the ones it cancels) to aim cancels and reduces.
/// Fed the engine's events through [`observe`](OrderFlow::observe), that
/// idea becomes exact (fills, expiries and rejections are accounted);
/// without feedback, some cancels target orders that already traded and are
/// rejected as unknown — also worth testing. The stream is a pure function
/// of the config (plus the observed events, if any).
#[derive(Clone, Debug)]
pub struct OrderFlow {
    // TODO(w12): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl OrderFlow {
    /// A generator at the start of the stream described by `cfg`.
    pub fn new(cfg: FlowConfig) -> OrderFlow {
        todo!("w12: new")
    }

    /// The next command. O(1).
    pub fn next_command(&mut self) -> Command {
        todo!("w12: next_command")
    }

    /// The next `n` commands.
    pub fn take_commands(&mut self, n: usize) -> Vec<Command> {
        todo!("w12: take_commands")
    }

    /// Feeds back the events the engine produced for `cmd`, so the
    /// generator's live set matches the book exactly. O(events).
    pub fn observe(&mut self, cmd: &Command, events: &[Event]) {
        todo!("w12: observe")
    }

    /// The current mid price of the random walk.
    pub fn mid(&self) -> Price {
        todo!("w12: mid")
    }

    /// How many orders the generator believes are resting.
    pub fn live_orders(&self) -> usize {
        todo!("w12: live_orders")
    }
}

impl Iterator for OrderFlow {
    type Item = Command;

    /// Never ends.
    fn next(&mut self) -> Option<Command> {
        Some(self.next_command())
    }
}
