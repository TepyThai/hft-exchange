//! Week 18: snapshot and restore of the engine's state.
//!
//! A child module of `engine` so it can set the private fields a restore
//! has to set (sequence number, last trade) without widening the engine's
//! public API.

use super::Engine;
use crate::state::{EngineState, RestoreError};
use crate::EngineConfig;
use book::{Book, OrderBook};

// @student-begin w18
use types::{Seq, Side};
// @student-end

// @student w18
impl<B: OrderBook> Engine<B> {
    /// The engine's complete state (see [`EngineState`]): sequence number,
    /// last trade price, and every resting order in priority order.
    /// O(resting orders); allocates the order vector.
    pub fn snapshot_state(&self) -> EngineState {
        let mut orders = Vec::with_capacity(self.book.len());
        for side in [Side::Buy, Side::Sell] {
            self.book.visit_orders(side, &mut |o| {
                orders.push(o);
                true
            });
        }
        EngineState {
            seq: Seq(self.seq),
            last_trade: self.last_trade,
            orders,
        }
    }

    /// Replaces this engine's state with `state`, keeping its configuration
    /// and book allocation: afterwards it behaves — and
    /// [`state_hash`](Engine::state_hash)es — exactly like the engine the
    /// state was taken from. The trade tape starts empty.
    ///
    /// Checks that `state` could have come from an engine (sides grouped,
    /// prices in priority order, not crossed, no zero quantities or
    /// duplicate ids); on error the engine is left empty at `Seq(0)`.
    /// O(orders) book insertions.
    pub fn restore_state(&mut self, state: &EngineState) -> Result<(), RestoreError> {
        self.book.clear();
        self.tape.clear();
        self.seq = 0;
        self.last_trade = None;
        let mut prev: Option<(Side, types::Price)> = None;
        for (index, o) in state.orders.iter().enumerate() {
            if o.qty.is_zero() {
                self.book.clear();
                return Err(RestoreError::ZeroQty(o.id));
            }
            let in_order = match (prev, o.side) {
                (None, _) => true,
                (Some((Side::Buy, p)), Side::Buy) => o.price <= p,
                (Some((Side::Sell, p)), Side::Sell) => o.price >= p,
                (Some((Side::Buy, _)), Side::Sell) => true,
                (Some((Side::Sell, _)), Side::Buy) => false,
            };
            if !in_order {
                self.book.clear();
                return Err(RestoreError::OutOfOrder { index });
            }
            if self.book.add(*o).is_err() {
                self.book.clear();
                return Err(RestoreError::DuplicateId(o.id));
            }
            prev = Some((o.side, o.price));
        }
        if self.book.is_crossed() {
            self.book.clear();
            return Err(RestoreError::Crossed);
        }
        self.seq = state.seq.0;
        self.last_trade = state.last_trade;
        Ok(())
    }
}

// @student w18
impl Engine<Book> {
    /// A new engine for `config` in `state` (see
    /// [`restore_state`](Engine::restore_state)).
    pub fn restore(
        config: EngineConfig,
        state: &EngineState,
    ) -> Result<Engine<Book>, RestoreError> {
        let mut engine = Engine::with_config(config);
        engine.restore_state(state)?;
        Ok(engine)
    }
}
