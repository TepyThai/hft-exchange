//! Week 44 — event-driven backtesting: a recorded command log replayed
//! through the engine, order latency, fill accounting and a report.

use engine::{Engine, EngineConfig};
use marketdata::Level;
use sim::backtest::{BacktestConfig, BacktestReport, Backtester, Recording};
use sim::flow::FlowConfig;
use sim::scenario::{Agent, AgentCtx, Delivery};
use types::{Command, Event, InstrumentSpec, NewOrder, OrderId, Qty, Side, TimeInForce};

/// Never trades; remembers when it was asked to act.
#[derive(Default)]
struct Idle {
    acts: Vec<u64>,
}

impl Agent for Idle {
    fn name(&self) -> &str {
        "idle"
    }
    fn act(&mut self, ctx: &mut AgentCtx<'_>) {
        self.acts.push(ctx.now);
    }
    fn on_event(&mut self, d: &Delivery) {
        panic!("an idle strategy got {d:?}");
    }
}

fn recording(n: usize, seed: u64) -> Recording {
    Recording::synthetic(FlowConfig::default(), n, 1_000, seed)
}

#[test]
fn w44_replaying_alone_reproduces_the_recorded_market() {
    let rec = recording(20_000, 4);
    assert_eq!(rec.len(), 20_000);
    assert!(rec.entries().windows(2).all(|w| w[0].at < w[1].at));
    let mut direct = Engine::with_config(EngineConfig::new(rec.spec()));
    let mut out = Vec::new();
    for t in rec.entries() {
        out.clear();
        direct.process(&t.cmd, &mut out);
    }

    let mut bt = Backtester::new(BacktestConfig::default());
    let mut idle = Idle::default();
    let report = bt.run(&rec, &mut idle);
    assert_eq!(bt.engine().state_hash(), direct.state_hash());
    assert_eq!(report.events_replayed, 20_000);
    assert_eq!(
        (
            report.orders_sent,
            report.fills,
            report.position,
            report.cash
        ),
        (0, 0, 0, 0)
    );
    assert_eq!((report.pnl, report.max_drawdown), (0.0, 0.0));
    assert_eq!(report.end_time, rec.entries().last().unwrap().at);
    // It acted once after every recorded command, at that command's time.
    let times: Vec<u64> = rec.entries().iter().map(|t| t.at).collect();
    assert_eq!(idle.acts, times);
    // Its market-data view is the engine's book.
    for side in [Side::Buy, Side::Sell] {
        let mut levels = Vec::new();
        bt.engine().levels(side, usize::MAX, &mut levels);
        let levels: Vec<Level> = levels
            .iter()
            .map(|l| Level {
                price: l.price,
                qty: l.qty,
            })
            .collect();
        assert_eq!(bt.book().depth(side, usize::MAX), levels);
    }
    // A second run of the same backtester starts from scratch.
    let again = bt.run(&rec, &mut Idle::default());
    assert_eq!(again, report);
}

#[test]
fn w44_same_inputs_same_report() {
    let rec = recording(30_000, 9);
    let run = |latency_ns| {
        let cfg = BacktestConfig {
            latency_ns,
            seed: 3,
            ..BacktestConfig::default()
        };
        let mut bt = Backtester::new(cfg);
        let mut s = Random::default();
        let report = bt.run(&rec, &mut s);
        (report, bt.engine().state_hash(), s.log)
    };
    let (a, ha, la) = run(5_000);
    let (b, hb, lb) = run(5_000);
    assert_eq!((a, ha), (b, hb));
    assert_eq!(la, lb, "the same deliveries in the same order");
    assert!(a.fills > 100 && a.orders_sent > 1_000, "{a:?}");
    let (c, hc, _) = run(50_000);
    assert_ne!(a, c, "latency changes the outcome");
    assert_ne!(ha, hc);
}

/// Snipes the first ask it sees with an IOC and joins the first bid.
#[derive(Default)]
struct Sniper {
    sniped: bool,
    joined: bool,
}

impl Agent for Sniper {
    fn name(&self) -> &str {
        "sniper"
    }
    fn act(&mut self, ctx: &mut AgentCtx<'_>) {
        if let (false, Some(ask)) = (self.sniped, ctx.book.best_ask()) {
            ctx.out.ioc(Side::Buy, ask.price, Qty(2));
            self.sniped = true;
        }
        if let (false, Some(bid)) = (self.joined, ctx.book.best_bid()) {
            ctx.out.limit(Side::Buy, bid.price, Qty(3));
            self.joined = true;
        }
    }
    fn on_event(&mut self, _: &Delivery) {}
}

fn limit(id: u64, side: Side, price: i64, qty: u64) -> Command {
    Command::New(NewOrder::limit(id, side, price, qty))
}

#[test]
fn w44_latency_decides_who_gets_the_fill() {
    let mut rec = Recording::new(InstrumentSpec::default());
    rec.push(0, limit(1, Side::Sell, 101, 5));
    rec.push(0, limit(2, Side::Buy, 99, 5));
    rec.push(
        1_000,
        Command::New(NewOrder::limit(3, Side::Buy, 101, 5).with_tif(TimeInForce::Ioc)),
    );
    rec.push(2_000, limit(4, Side::Buy, 99, 5));
    rec.push(3_000, Command::New(NewOrder::market(5, Side::Sell, 8)));
    rec.push(10_000, limit(6, Side::Sell, 105, 1));

    // (latency, fills, position, cash). The snipe (decided at t=0) beats the
    // recorded buyer at t=1000 only if it arrives first; the bid it joins
    // at t=0 is filled by the market sell at t=3000 only if it queued
    // before order 4 (t=2000). Ties go to the recorded command.
    let cases = [
        (0, 2, 5, -(2 * 101 + 3 * 99)),
        (999, 2, 5, -(2 * 101 + 3 * 99)),
        (1_000, 1, 3, -(3 * 99)),
        (1_999, 1, 3, -(3 * 99)),
        (2_000, 0, 0, 0),
        (50_000, 0, 0, 0), // arrives after the recording ended: still processed
    ];
    let mut last_fills = u64::MAX;
    for (latency_ns, fills, position, cash) in cases {
        let mut bt = Backtester::new(BacktestConfig {
            latency_ns,
            ..BacktestConfig::default()
        });
        let r = bt.run(&rec, &mut Sniper::default());
        assert_eq!(
            (r.fills, r.position, r.cash),
            (fills, position, cash),
            "latency {latency_ns}"
        );
        assert_eq!((r.events_replayed, r.orders_sent), (6, 2));
        assert!(r.fills <= last_fills, "more latency never helps here");
        last_fills = r.fills;
        assert_eq!(r.end_time, 10_000.max(latency_ns));
    }
}

/// Buys one lot with an IOC at the first ask it sees.
#[derive(Default)]
struct BuyOnce(bool);

impl Agent for BuyOnce {
    fn name(&self) -> &str {
        "buy-once"
    }
    fn act(&mut self, ctx: &mut AgentCtx<'_>) {
        if let (false, Some(ask)) = (self.0, ctx.book.best_ask()) {
            ctx.out.ioc(Side::Buy, ask.price, Qty(1));
            self.0 = true;
        }
    }
    fn on_event(&mut self, _: &Delivery) {}
}

#[test]
fn w44_pnl_and_drawdown_on_a_constructed_path() {
    let mut rec = Recording::new(InstrumentSpec::default());
    let mut t = 0;
    let mut at = |cmd| {
        rec.push(t, cmd);
        t += 10;
    };
    at(limit(1, Side::Buy, 99, 10));
    at(limit(2, Side::Sell, 101, 10)); // mid 100: buy 1 at 101 → equity −1
                                       // Up to 109 / 111 (mid 110): equity +9, the peak.
    at(Command::Cancel { id: OrderId(2) });
    at(limit(3, Side::Sell, 111, 10));
    at(limit(4, Side::Buy, 109, 10));
    at(Command::Cancel { id: OrderId(1) });
    // Down to 94 / 96 (mid 95): equity −6, a drawdown of 15 from the peak.
    at(Command::Cancel { id: OrderId(4) });
    at(limit(5, Side::Buy, 94, 10));
    at(Command::Cancel { id: OrderId(3) });
    at(limit(6, Side::Sell, 96, 10));
    // Back up to 104 / 106 (mid 105): equity +4.
    at(Command::Cancel { id: OrderId(6) });
    at(limit(7, Side::Sell, 106, 10));
    at(limit(8, Side::Buy, 104, 10));
    at(Command::Cancel { id: OrderId(5) });

    let mut bt = Backtester::new(BacktestConfig::default());
    let r = bt.run(&rec, &mut BuyOnce::default());
    assert_eq!(
        r,
        BacktestReport {
            events_replayed: 14,
            orders_sent: 1,
            fills: 1,
            filled_qty: 1,
            position: 1,
            cash: -101,
            turnover: 101,
            final_mark: Some(105.0),
            pnl: 4.0,
            max_drawdown: 15.0,
            end_time: 130,
        }
    );
}

/// Trades at random (from the backtester's seeded generator) and logs every
/// delivery and command, for an independent recomputation.
#[derive(Default)]
struct Random {
    log: Vec<Delivery>,
    sent: u64,
    resting: Vec<OrderId>,
}

impl Agent for Random {
    fn name(&self) -> &str {
        "random"
    }
    fn act(&mut self, ctx: &mut AgentCtx<'_>) {
        let r = ctx.rng.range(0, 99);
        let qty = Qty(ctx.rng.range(1, 3));
        let (bid, ask) = (ctx.book.best_bid(), ctx.book.best_ask());
        match (r, bid, ask) {
            (0..=1, _, Some(a)) => {
                ctx.out.ioc(Side::Buy, a.price, qty);
            }
            (2..=3, Some(b), _) => {
                ctx.out.ioc(Side::Sell, b.price, qty);
            }
            (4..=5, Some(b), Some(a)) => {
                let (side, price) = if r == 4 {
                    (Side::Buy, b.price)
                } else {
                    (Side::Sell, a.price)
                };
                self.resting.push(ctx.out.limit(side, price, qty));
            }
            (6..=7, _, _) if !self.resting.is_empty() => {
                let i = ctx.rng.range(0, self.resting.len() as u64 - 1) as usize;
                ctx.out.cancel(self.resting.swap_remove(i));
            }
            _ => {}
        }
        self.sent += ctx.out.len() as u64;
    }
    fn on_event(&mut self, d: &Delivery) {
        self.log.push(*d);
    }
}

#[test]
fn w44_fill_accounting_identities_hold() {
    let rec = recording(20_000, 17);
    let cfg = BacktestConfig {
        latency_ns: 3_000,
        seed: 5,
        ..BacktestConfig::default()
    };
    let mut bt = Backtester::new(cfg);
    let mut s = Random::default();
    let r = bt.run(&rec, &mut s);

    let (mut position, mut cash, mut turnover, mut fills, mut qty) =
        (0i64, 0i128, 0u128, 0u64, 0u64);
    for d in &s.log {
        assert_eq!(d.account, cfg.account);
        assert!(d.order.0 >= cfg.first_id);
        if let Some(leg) = d.fill() {
            let notional = leg.price.0 as i128 * leg.qty.0 as i128;
            let sign = if leg.side == Side::Buy { 1 } else { -1 };
            position += sign * leg.qty.0 as i64;
            cash -= sign as i128 * notional;
            turnover += notional as u128;
            fills += 1;
            qty += leg.qty.0;
        }
    }
    assert!(fills > 200, "{r:?}");
    assert_eq!(
        (r.position, r.cash, r.turnover, r.fills, r.filled_qty),
        (position, cash, turnover, fills, qty)
    );
    assert_eq!(r.orders_sent, s.sent);
    // Every new order was answered (accepted or rejected).
    let answered = s
        .log
        .iter()
        .filter(|d| matches!(d.event, Event::Accepted { .. }))
        .count();
    assert!(answered > 0 && answered as u64 <= s.sent);
    // P&L is cash plus the position at the final mid.
    let (b, a) = (bt.book().best_bid().unwrap(), bt.book().best_ask().unwrap());
    let mid = (b.price.0 + a.price.0) as f64 / 2.0;
    assert_eq!(r.final_mark, Some(mid));
    assert_eq!(r.pnl, cash as f64 + position as f64 * mid);
    assert!(r.max_drawdown >= 0.0 && r.max_drawdown >= -r.pnl);
}
