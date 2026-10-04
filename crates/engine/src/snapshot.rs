//! Read-only views of the engine for market data, the WebAssembly
//! visualizer and tests.
//!
//! Provided (built only on the engine's public accessors).

use crate::tape::TradePrint;
use crate::Engine;
use book::{LevelInfo, OrderBook, RestingOrder};
use types::{Price, Seq, Side};

/// A point-in-time view of the top of the book and the recent trades.
///
/// `seq` says which command the view reflects: a market-data publisher
/// sends it with the snapshot so clients can line it up with incrementals
/// (w10), and a visualizer can tell whether anything changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BookSnapshot {
    /// [`Engine::seq`] when the snapshot was taken.
    pub seq: Seq,
    /// Up to `depth` bid levels, best (highest) first.
    pub bids: Vec<LevelInfo>,
    /// Up to `depth` ask levels, best (lowest) first.
    pub asks: Vec<LevelInfo>,
    /// Recent trades, oldest first (at most the tape capacity).
    pub trades: Vec<TradePrint>,
    /// Last trade price.
    pub last_price: Option<Price>,
}

impl BookSnapshot {
    /// Best bid level in the snapshot.
    pub fn best_bid(&self) -> Option<LevelInfo> {
        self.bids.first().copied()
    }

    /// Best ask level in the snapshot.
    pub fn best_ask(&self) -> Option<LevelInfo> {
        self.asks.first().copied()
    }

    /// `best ask - best bid` in ticks.
    pub fn spread(&self) -> Option<i64> {
        Some(self.best_ask()?.price.0 - self.best_bid()?.price.0)
    }
}

impl<B: OrderBook> Engine<B> {
    /// Clears `out` and fills it with up to `depth` levels of `side`, best
    /// first (L2). O(log L + depth); allocates only if `out` must grow.
    pub fn levels(&self, side: Side, depth: usize, out: &mut Vec<LevelInfo>) {
        self.book().depth(side, depth, out);
    }

    /// Clears `out` and fills it with the orders resting at `price` on
    /// `side` in time priority (L3 view of one level, e.g. to draw queue
    /// position).
    pub fn level_orders(&self, side: Side, price: Price, out: &mut Vec<RestingOrder>) {
        self.book().level_orders(side, price, out);
    }

    /// Clears `out` and fills it with the tape's trades, oldest first.
    pub fn recent_trades(&self, out: &mut Vec<TradePrint>) {
        out.clear();
        out.extend(self.tape().iter().copied());
    }

    /// Top `depth` levels per side plus the recent trades, reusing `out`'s
    /// buffers (no allocation once they have grown).
    pub fn snapshot_into(&self, depth: usize, out: &mut BookSnapshot) {
        out.seq = self.seq();
        self.levels(Side::Buy, depth, &mut out.bids);
        self.levels(Side::Sell, depth, &mut out.asks);
        self.recent_trades(&mut out.trades);
        out.last_price = self.last_trade_price();
    }

    /// Top `depth` levels per side plus the recent trades (allocates the
    /// snapshot's vectors; use [`snapshot_into`](Engine::snapshot_into) on a
    /// hot path).
    pub fn snapshot(&self, depth: usize) -> BookSnapshot {
        let mut s = BookSnapshot::default();
        self.snapshot_into(depth, &mut s);
        s
    }
}
