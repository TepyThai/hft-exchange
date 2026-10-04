//! Week 18: snapshot and restore of the engine's state.
//!
//! A child module of `engine` so it can set the private fields a restore
//! has to set (sequence number, last trade) without widening the engine's
//! public API.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use super::Engine;
use crate::state::{EngineState, RestoreError};
use crate::EngineConfig;
use book::{Book, OrderBook};

// TODO(w18): implement — see the week guide on tepy.dev/hft.

#[allow(unused_variables)]
impl<B: OrderBook> Engine<B> {
    /// The engine's complete state (see [`EngineState`]): sequence number,
    /// last trade price, and every resting order in priority order.
    /// O(resting orders); allocates the order vector.
    pub fn snapshot_state(&self) -> EngineState {
        todo!("w18: snapshot_state")
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
        todo!("w18: restore_state")
    }
}

#[allow(unused_variables)]
impl Engine<Book> {
    /// A new engine for `config` in `state` (see
    /// [`restore_state`](Engine::restore_state)).
    pub fn restore(
        config: EngineConfig,
        state: &EngineState,
    ) -> Result<Engine<Book>, RestoreError> {
        todo!("w18: restore")
    }
}
