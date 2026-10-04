//! A live trading session: one engine, simulated flow, and a human trader.
//!
//! Shared by the WebAssembly page (crates/wasm), the native terminal
//! (apps/terminal) and the demo server. Everything is single-threaded and
//! deterministic for a seed except the user's own orders.
//!
//! Infrastructure, not a curriculum exercise; it uses only the engine's
//! public API, so it works with your engine as soon as weeks 6–8 pass.

use crate::flow::{FlowConfig, OrderFlow};
use engine::{Engine, EngineConfig};
use std::collections::{HashMap, VecDeque};
use types::{Command, Event, NewOrder, OrderId, Price, Qty, RejectReason, Side, TimeInForce};

/// User order ids start here so they never collide with the flow's.
pub const USER_ID_BASE: u64 = 1 << 48;
const KEEP_FILLS: usize = 64;
const KEEP_REJECTS: usize = 8;

/// How a user order executes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserKind {
    /// Good-till-cancel limit: matches what it can, rests the rest.
    Limit,
    /// Immediate-or-cancel limit: matches what it can, expires the rest.
    Ioc,
    /// Market: takes liquidity at any price.
    Market,
}

/// A fill of one of the user's orders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UserFill {
    pub id: OrderId,
    pub side: Side,
    pub price: Price,
    pub qty: Qty,
    /// Whether the user's order was resting (maker) or incoming (taker).
    pub maker: bool,
}

/// A user order still resting on the book.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UserOrder {
    pub id: OrderId,
    pub side: Side,
    pub price: Price,
    pub remaining: Qty,
}

/// The human trader's state: open orders, fills, position and cash (in
/// price units × quantity).
#[derive(Clone, Debug, Default)]
pub struct UserState {
    open: HashMap<u64, UserOrder>,
    fills: VecDeque<UserFill>,
    rejects: VecDeque<(OrderId, RejectReason)>,
    pub position: i64,
    pub cash: i64,
}

impl UserState {
    /// Open orders, newest first.
    pub fn open_orders(&self) -> Vec<UserOrder> {
        let mut v: Vec<UserOrder> = self.open.values().copied().collect();
        v.sort_by_key(|o| std::cmp::Reverse(o.id));
        v
    }

    /// Recent fills, newest first.
    pub fn fills(&self) -> impl Iterator<Item = &UserFill> {
        self.fills.iter()
    }

    /// Recent rejections, newest first.
    pub fn rejects(&self) -> impl Iterator<Item = &(OrderId, RejectReason)> {
        self.rejects.iter()
    }

    /// Mark-to-market P&L at `mark`, in price units × quantity.
    pub fn pnl(&self, mark: Option<Price>) -> i64 {
        self.cash + self.position * mark.map_or(0, |p| p.0)
    }

    fn fill(&mut self, f: UserFill) {
        let notional = f.price.0 * f.qty.0 as i64;
        match f.side {
            Side::Buy => {
                self.position += f.qty.0 as i64;
                self.cash -= notional;
            }
            Side::Sell => {
                self.position -= f.qty.0 as i64;
                self.cash += notional;
            }
        }
        if self.fills.len() == KEEP_FILLS {
            self.fills.pop_back();
        }
        self.fills.push_front(f);
    }
}

/// Session-wide counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionStats {
    /// Commands sent to the engine (flow + user).
    pub commands: u64,
    pub trades: u64,
    /// Total traded quantity.
    pub volume: u64,
    /// Events the engine emitted.
    pub events: u64,
}

/// Engine + simulated flow + a human trader; see the module docs.
pub struct LiveSession {
    engine: Engine,
    flow: OrderFlow,
    events: Vec<Event>,
    user: UserState,
    next_user_id: u64,
    stats: SessionStats,
}

impl LiveSession {
    /// A session with the default flow around a fair value of 100.00
    /// (10 000 ticks).
    pub fn new(seed: u64) -> LiveSession {
        LiveSession::with_flow(FlowConfig::default(), seed)
    }

    pub fn with_flow(cfg: FlowConfig, seed: u64) -> LiveSession {
        let config = EngineConfig { tape_capacity: 256, expected_orders: 16_384, ..EngineConfig::new(cfg.spec) };
        LiveSession {
            engine: Engine::with_config(config),
            flow: OrderFlow::new(cfg, seed),
            events: Vec::with_capacity(64),
            user: UserState::default(),
            next_user_id: USER_ID_BASE,
            stats: SessionStats::default(),
        }
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn flow(&self) -> &OrderFlow {
        &self.flow
    }

    pub fn user(&self) -> &UserState {
        &self.user
    }

    pub fn stats(&self) -> SessionStats {
        self.stats
    }

    /// The events the engine emitted for the most recent command (from
    /// [`step`](Self::step), [`submit`](Self::submit) or
    /// [`cancel`](Self::cancel)), in order; empty before the first. This is
    /// how a server feeds the session's output to market data (the demo's
    /// `L2Publisher`). O(1), no allocation; overwritten by the next command.
    pub fn last_events(&self) -> &[Event] {
        &self.events
    }

    /// Process one simulated command; returns the trades it caused.
    pub fn step(&mut self) -> u32 {
        let cmd = self.flow.next_command();
        self.apply(&cmd)
    }

    /// Process `n` simulated commands; returns the trades they caused.
    pub fn run(&mut self, n: usize) -> u32 {
        (0..n).map(|_| self.step()).sum()
    }

    /// Send a user order. `price` is ignored for market orders. Returns the
    /// id; a rejection shows up in [`UserState::rejects`].
    pub fn submit(&mut self, side: Side, kind: UserKind, price: i64, qty: u64) -> OrderId {
        let id = self.next_user_id;
        self.next_user_id += 1;
        let order = match kind {
            UserKind::Market => NewOrder::market(id, side, qty),
            UserKind::Limit => NewOrder::limit(id, side, price, qty),
            UserKind::Ioc => NewOrder::limit(id, side, price, qty).with_tif(TimeInForce::Ioc),
        }
        .with_account(2);
        self.apply(&Command::New(order));
        OrderId(id)
    }

    /// Cancel a resting user order; false if it is no longer resting.
    pub fn cancel(&mut self, id: OrderId) -> bool {
        if !self.user.open.contains_key(&id.0) {
            return false;
        }
        self.apply(&Command::Cancel { id });
        !self.user.open.contains_key(&id.0)
    }

    fn apply(&mut self, cmd: &Command) -> u32 {
        self.events.clear();
        self.engine.process(cmd, &mut self.events);
        self.stats.commands += 1;
        self.stats.events += self.events.len() as u64;
        let mut trades = 0;
        for i in 0..self.events.len() {
            let e = self.events[i];
            self.flow.on_event(&e);
            match e {
                Event::Trade { taker, maker, taker_side, price, qty } => {
                    trades += 1;
                    self.stats.trades += 1;
                    self.stats.volume += qty.0;
                    if taker.0 >= USER_ID_BASE {
                        self.user.fill(UserFill { id: taker, side: taker_side, price, qty, maker: false });
                    }
                    if maker.0 >= USER_ID_BASE {
                        self.user.fill(UserFill { id: maker, side: taker_side.opposite(), price, qty, maker: true });
                        if let Some(o) = self.user.open.get_mut(&maker.0) {
                            o.remaining = Qty(o.remaining.0.saturating_sub(qty.0));
                            if o.remaining.0 == 0 {
                                self.user.open.remove(&maker.0);
                            }
                        }
                    }
                }
                Event::Rested { id, side, price, qty } if id.0 >= USER_ID_BASE => {
                    self.user.open.insert(id.0, UserOrder { id, side, price, remaining: qty });
                }
                Event::Cancelled { id, .. } | Event::Expired { id, .. } if id.0 >= USER_ID_BASE => {
                    self.user.open.remove(&id.0);
                }
                Event::Reduced { id, remaining } if id.0 >= USER_ID_BASE => {
                    if let Some(o) = self.user.open.get_mut(&id.0) {
                        o.remaining = remaining;
                    }
                }
                Event::Rejected { id, reason } if id.0 >= USER_ID_BASE => {
                    if self.user.rejects.len() == KEEP_REJECTS {
                        self.user.rejects.pop_back();
                    }
                    self.user.rejects.push_front((id, reason));
                }
                _ => {}
            }
        }
        trades
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flow_builds_a_two_sided_book_and_trades() {
        let mut s = LiveSession::new(42);
        s.run(20_000);
        let snap = s.engine().snapshot(5);
        assert!(!snap.bids.is_empty() && !snap.asks.is_empty());
        assert!(snap.spread().unwrap() > 0);
        assert!(s.stats().trades > 100);
        assert_eq!(s.stats().commands, 20_000);
    }

    #[test]
    fn user_orders_rest_fill_and_cancel() {
        let mut s = LiveSession::new(7);
        s.run(5_000);
        let snap = s.engine().snapshot(1);
        let bid = snap.best_bid().unwrap().price.0;
        let ask = snap.best_ask().unwrap().price.0;

        // Passive buy below the bid rests and can be cancelled.
        let id = s.submit(Side::Buy, UserKind::Limit, bid - 50, 3);
        assert_eq!(s.user().open_orders().len(), 1);
        assert!(s.cancel(id));
        assert!(s.user().open_orders().is_empty());

        // A marketable buy fills against the ask and moves the position.
        s.submit(Side::Buy, UserKind::Ioc, ask, 1);
        assert_eq!(s.user().position, 1);
        assert_eq!(s.user().cash, -ask);
        assert_eq!(s.user().fills().next().map(|f| f.maker), Some(false));

        // Selling it back at the bid flattens the position.
        s.submit(Side::Sell, UserKind::Market, 0, 1);
        assert_eq!(s.user().position, 0);
        assert!(s.user().open_orders().is_empty());
    }

    #[test]
    fn rejections_are_reported() {
        let mut s = LiveSession::new(1);
        s.submit(Side::Buy, UserKind::Limit, 10_000, 0);
        assert_eq!(s.user().rejects().next().map(|r| r.1), Some(RejectReason::ZeroQuantity));
    }

    #[test]
    fn last_events_are_the_latest_commands_events() {
        let mut s = LiveSession::new(3);
        assert!(s.last_events().is_empty());
        let mut seen = 0u64;
        for _ in 0..2_000 {
            s.step();
            assert!(!s.last_events().is_empty(), "every command emits at least one event");
            seen += s.last_events().len() as u64;
        }
        assert_eq!(seen, s.stats().events);
        s.submit(Side::Buy, UserKind::Limit, 10_000, 0);
        assert!(matches!(s.last_events(), [Event::Rejected { reason: RejectReason::ZeroQuantity, .. }]));
    }
}
