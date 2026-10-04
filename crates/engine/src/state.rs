//! Week 18: the engine's complete state as a value, and its snapshot
//! encoding.
//!
//! Everything that affects the engine's future output is the sequence
//! number, the last trade price (the price-band reference) and the resting
//! orders *in priority order* — the same things [`Engine::state_hash`]
//! covers. The trade tape is a display cache and is not part of it.
//! [`Engine::snapshot_state`] captures an [`EngineState`],
//! [`Engine::restore`] rebuilds an engine from one, and the
//! [`Snapshottable`] impl lets [`journal::recover`] do both through a
//! [`SnapshotStore`](journal::SnapshotStore).

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::Engine;
use book::{OrderBook, RestingOrder};
use journal::{SnapshotError, Snapshottable};
use std::fmt;
use types::{OrderId, Price, Seq};

/// The engine's state at one sequence number.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineState {
    /// [`Engine::seq`]: commands processed.
    pub seq: Seq,
    /// [`Engine::last_trade_price`].
    pub last_trade: Option<Price>,
    /// Every resting order: all bids, best price first and oldest first
    /// within a price, then all asks the same way. Restoring adds them in
    /// this order, which recreates the time priority.
    pub orders: Vec<RestingOrder>,
}

/// Why bytes are not an encoded [`EngineState`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateError {
    /// Unknown format version byte.
    Version(u8),
    /// The length does not match the order count.
    Length { expected: usize, actual: usize },
    /// A side byte other than 0 (buy) or 1 (sell), at order `index`.
    Side { index: usize, byte: u8 },
    /// The last-trade flag is neither 0 nor 1.
    Flag(u8),
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            StateError::Version(v) => write!(f, "unknown engine state version {v}"),
            StateError::Length { expected, actual } => {
                write!(f, "engine state needs {expected} bytes, got {actual}")
            }
            StateError::Side { index, byte } => write!(f, "order {index}: bad side byte {byte}"),
            StateError::Flag(b) => write!(f, "bad last-trade flag {b}"),
        }
    }
}

impl std::error::Error for StateError {}

/// Why [`Engine::restore`] refused a state: it could not have come from an
/// engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreError {
    /// Order `index` breaks the priority order (a bid after an ask, or a
    /// price worse-then-better within a side).
    OutOfOrder { index: usize },
    /// The best bid is at or above the best ask.
    Crossed,
    /// An order with zero quantity.
    ZeroQty(OrderId),
    /// Two orders with one id.
    DuplicateId(OrderId),
}

impl fmt::Display for RestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            RestoreError::OutOfOrder { index } => {
                write!(f, "order {index} is out of priority order")
            }
            RestoreError::Crossed => write!(f, "restored book would be crossed"),
            RestoreError::ZeroQty(id) => write!(f, "order {} has zero quantity", id.0),
            RestoreError::DuplicateId(id) => write!(f, "order id {} appears twice", id.0),
        }
    }
}

impl std::error::Error for RestoreError {}

impl EngineState {
    /// Format version written by [`encode`](EngineState::encode).
    pub const VERSION: u8 = 1;
    /// Encoded size of the fixed part: version, seq, flag, last trade,
    /// order count.
    pub const HEADER_LEN: usize = 1 + 8 + 1 + 8 + 4;
    /// Encoded size of one order: side, id, price, qty.
    pub const ORDER_LEN: usize = 1 + 8 + 8 + 8;
}

#[allow(unused_variables)]
impl EngineState {
    /// Appends the binary encoding to `out` (little-endian, no padding):
    ///
    /// ```text
    /// version u8 = 1 | seq u64 | has_last u8 | last i64 (0 if none) | count u32
    /// count × ( side u8 (0 buy, 1 sell) | id u64 | price i64 | qty u64 )
    /// ```
    ///
    /// O(orders); 22 + 25 bytes per order.
    pub fn encode(&self, out: &mut Vec<u8>) {
        todo!("w18: encode")
    }

    /// Decodes what [`encode`](EngineState::encode) wrote. Strict: exact
    /// length, known version, valid side and flag bytes. Does not check
    /// that the orders make a valid book — [`Engine::restore`] does.
    /// O(orders).
    pub fn decode(bytes: &[u8]) -> Result<EngineState, StateError> {
        todo!("w18: decode")
    }
}

/// The engine as a snapshottable journal state machine: its snapshot is
/// the encoded [`EngineState`].
#[allow(unused_variables)]
impl<B: OrderBook> Snapshottable for Engine<B> {
    fn applied_seq(&self) -> Seq {
        todo!("w18: applied_seq")
    }

    fn save_snapshot(&self, out: &mut Vec<u8>) {
        todo!("w18: save_snapshot")
    }

    fn load_snapshot(&mut self, bytes: &[u8]) -> Result<(), SnapshotError> {
        todo!("w18: load_snapshot")
    }
}
