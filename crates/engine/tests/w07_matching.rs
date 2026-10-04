//! Week 7: price-time priority matching and the event-order contract.

use engine::{Book, Engine, EngineConfig, LevelInfo, OrderBook, RestingOrder};
use types::{
    Command, Event, InstrumentSpec, NewOrder, OrderId, Price, Qty, RejectReason, Rng, Seq, Side,
};

use Side::{Buy, Sell};

fn engine() -> Engine {
    Engine::new(InstrumentSpec::default())
}

fn run(e: &mut Engine, cmd: Command) -> Vec<Event> {
    let mut out = Vec::new();
    e.process(&cmd, &mut out);
    out
}

fn limit(id: u64, side: Side, price: i64, qty: u64) -> Command {
    Command::New(NewOrder::limit(id, side, price, qty))
}

fn market(id: u64, side: Side, qty: u64) -> Command {
    Command::New(NewOrder::market(id, side, qty))
}

fn accepted(id: u64) -> Event {
    Event::Accepted { id: OrderId(id) }
}

fn rested(id: u64, side: Side, price: i64, qty: u64) -> Event {
    Event::Rested {
        id: OrderId(id),
        side,
        price: Price(price),
        qty: Qty(qty),
    }
}

fn trade(taker: u64, maker: u64, taker_side: Side, price: i64, qty: u64) -> Event {
    Event::Trade {
        taker: OrderId(taker),
        maker: OrderId(maker),
        taker_side,
        price: Price(price),
        qty: Qty(qty),
    }
}

fn rejected(id: u64, reason: RejectReason) -> Event {
    Event::Rejected {
        id: OrderId(id),
        reason,
    }
}

fn resting(e: &Engine) -> Vec<RestingOrder> {
    let mut out = Vec::new();
    for side in [Buy, Sell] {
        e.book().visit_orders(side, &mut |o| {
            out.push(o);
            true
        });
    }
    out
}

#[test]
fn w07_non_crossing_limits_are_accepted_then_rested() {
    let mut e = engine();
    assert_eq!(
        run(&mut e, limit(1, Buy, 100, 5)),
        vec![accepted(1), rested(1, Buy, 100, 5)]
    );
    assert_eq!(
        run(&mut e, limit(2, Sell, 101, 7)),
        vec![accepted(2), rested(2, Sell, 101, 7)]
    );
    assert_eq!(
        run(&mut e, limit(3, Buy, 100, 1)),
        vec![accepted(3), rested(3, Buy, 100, 1)]
    );
    let b = e.book();
    assert_eq!(
        b.best_bid(),
        Some(LevelInfo {
            price: Price(100),
            qty: Qty(6),
            orders: 2
        })
    );
    assert_eq!(
        b.best_ask(),
        Some(LevelInfo {
            price: Price(101),
            qty: Qty(7),
            orders: 1
        })
    );
    assert_eq!(e.seq(), Seq(3));
    assert_eq!(e.last_trade_price(), None);

    // `process` appends: the caller owns (and clears) the buffer.
    let mut out = vec![accepted(99)];
    e.process(&limit(4, Sell, 105, 1), &mut out);
    assert_eq!(
        out,
        vec![accepted(99), accepted(4), rested(4, Sell, 105, 1)]
    );
}

#[test]
fn w07_crossing_limit_trades_at_the_makers_price() {
    let mut e = engine();
    run(&mut e, limit(1, Sell, 101, 5));
    run(&mut e, limit(2, Buy, 99, 5));
    // Buyer willing to pay 105 gets the resting 101: price improvement.
    assert_eq!(
        run(&mut e, limit(3, Buy, 105, 3)),
        vec![accepted(3), trade(3, 1, Buy, 101, 3)]
    );
    assert_eq!(e.book().get(OrderId(1)).map(|o| o.qty), Some(Qty(2)));
    assert!(
        !e.book().contains(OrderId(3)),
        "a fully filled taker does not rest"
    );
    // Seller willing to go down to 90 gets the resting bid at 99.
    assert_eq!(
        run(&mut e, limit(4, Sell, 90, 5)),
        vec![accepted(4), trade(4, 2, Sell, 99, 5)]
    );
    assert_eq!(e.last_trade_price(), Some(Price(99)));
    // Exactly at the touch also crosses.
    assert_eq!(
        run(&mut e, limit(5, Buy, 101, 2)),
        vec![accepted(5), trade(5, 1, Buy, 101, 2)]
    );
    assert!(e.book().is_empty());
}

#[test]
fn w07_sweep_follows_price_then_time_priority() {
    let mut e = engine();
    run(&mut e, limit(1, Sell, 101, 2));
    run(&mut e, limit(2, Sell, 100, 3));
    run(&mut e, limit(3, Sell, 100, 4));
    run(&mut e, limit(4, Sell, 102, 5));
    run(&mut e, limit(5, Sell, 100, 1));
    let events = run(&mut e, limit(6, Buy, 101, 9));
    assert_eq!(
        events,
        vec![
            accepted(6),
            trade(6, 2, Buy, 100, 3), // best price, oldest first
            trade(6, 3, Buy, 100, 4),
            trade(6, 5, Buy, 100, 1),
            trade(6, 1, Buy, 101, 1), // then the next level; maker 1 partly filled
        ]
    );
    assert_eq!(
        resting(&e),
        vec![
            RestingOrder::new(1, Sell, 101, 1),
            RestingOrder::new(4, Sell, 102, 5)
        ],
        "the 102 level is beyond the limit and untouched"
    );
    let tape: Vec<(u64, i64)> = e.tape().iter().map(|t| (t.maker.0, t.price.0)).collect();
    assert_eq!(tape, vec![(2, 100), (3, 100), (5, 100), (1, 101)]);
    assert!(e
        .tape()
        .iter()
        .all(|t| t.seq == Seq(6) && t.taker == OrderId(6)));
}

#[test]
fn w07_partial_fill_rests_the_remainder_at_the_limit() {
    let mut e = engine();
    run(&mut e, limit(1, Sell, 100, 3));
    assert_eq!(
        run(&mut e, limit(2, Buy, 101, 10)),
        vec![
            accepted(2),
            trade(2, 1, Buy, 100, 3),
            rested(2, Buy, 101, 7)
        ]
    );
    assert_eq!(
        e.book().best_bid(),
        Some(LevelInfo {
            price: Price(101),
            qty: Qty(7),
            orders: 1
        })
    );
    assert_eq!(e.book().best_ask(), None);
    // The rested remainder is now a maker, trading at its own price.
    assert_eq!(
        run(&mut e, limit(3, Sell, 95, 2)),
        vec![accepted(3), trade(3, 2, Sell, 101, 2)]
    );
    assert_eq!(
        e.book().get(OrderId(2)),
        Some(RestingOrder::new(2, Buy, 101, 5))
    );
    // A partly filled maker can be cancelled for what is left.
    assert_eq!(
        run(&mut e, Command::Cancel { id: OrderId(2) }),
        vec![Event::Cancelled {
            id: OrderId(2),
            remaining: Qty(5)
        }]
    );
}

#[test]
fn w07_market_orders_sweep_and_expire_never_rest() {
    let mut e = engine();
    assert_eq!(
        run(&mut e, market(1, Buy, 5)),
        vec![rejected(1, RejectReason::NoLiquidity)]
    );
    run(&mut e, limit(2, Sell, 100, 2));
    run(&mut e, limit(3, Sell, 103, 2));
    run(&mut e, limit(4, Buy, 90, 1)); // own side is irrelevant
    assert_eq!(
        run(&mut e, market(5, Buy, 10)),
        vec![
            accepted(5),
            trade(5, 2, Buy, 100, 2),
            trade(5, 3, Buy, 103, 2),
            Event::Expired {
                id: OrderId(5),
                remaining: Qty(6)
            },
        ]
    );
    assert_eq!(e.book().best_ask(), None);
    assert_eq!(resting(&e), vec![RestingOrder::new(4, Buy, 90, 1)]);
    assert_eq!(e.last_trade_price(), Some(Price(103)));
    // Fully filled market order: no Expired.
    run(&mut e, limit(6, Sell, 104, 5));
    assert_eq!(
        run(&mut e, market(7, Buy, 5)),
        vec![accepted(7), trade(7, 6, Buy, 104, 5)]
    );
}

#[test]
fn w07_cancel_and_reduce_emit_single_events_and_keep_priority() {
    let mut e = engine();
    for id in 1..=3 {
        run(&mut e, limit(id, Buy, 50, 10));
    }
    assert_eq!(
        run(
            &mut e,
            Command::Reduce {
                id: OrderId(1),
                qty: Qty(4)
            }
        ),
        vec![Event::Reduced {
            id: OrderId(1),
            remaining: Qty(4)
        }]
    );
    assert_eq!(
        run(&mut e, Command::Cancel { id: OrderId(2) }),
        vec![Event::Cancelled {
            id: OrderId(2),
            remaining: Qty(10)
        }]
    );
    assert_eq!(
        run(&mut e, Command::Cancel { id: OrderId(2) }),
        vec![rejected(2, RejectReason::UnknownOrder)]
    );
    assert_eq!(
        run(
            &mut e,
            Command::Reduce {
                id: OrderId(9),
                qty: Qty(1)
            }
        ),
        vec![rejected(9, RejectReason::UnknownOrder)]
    );
    assert_eq!(
        run(
            &mut e,
            Command::Reduce {
                id: OrderId(1),
                qty: Qty(4)
            }
        ),
        vec![rejected(1, RejectReason::InvalidReduce)],
        "reduce must lower the quantity"
    );
    assert_eq!(
        run(
            &mut e,
            Command::Reduce {
                id: OrderId(1),
                qty: Qty(0)
            }
        ),
        vec![rejected(1, RejectReason::ZeroQuantity)],
        "reduce-to-zero is a cancel's job"
    );
    // Order 1 kept its place ahead of order 3 despite the reduce.
    assert_eq!(
        run(&mut e, limit(4, Sell, 50, 6)),
        vec![
            accepted(4),
            trade(4, 1, Sell, 50, 4),
            trade(4, 3, Sell, 50, 2)
        ]
    );
    assert_eq!(resting(&e), vec![RestingOrder::new(3, Buy, 50, 8)]);
    assert_eq!(e.seq(), Seq(10), "every command counts, rejected ones too");
}

#[test]
fn w07_duplicate_live_ids_are_rejected() {
    let mut e = engine();
    run(&mut e, limit(1, Buy, 100, 5));
    let before = resting(&e);
    assert_eq!(
        run(&mut e, limit(1, Buy, 99, 1)),
        vec![rejected(1, RejectReason::DuplicateOrderId)]
    );
    // Same id on the other side would even trade against itself: rejected first.
    assert_eq!(
        run(&mut e, limit(1, Sell, 100, 5)),
        vec![rejected(1, RejectReason::DuplicateOrderId)]
    );
    assert_eq!(
        run(&mut e, market(1, Sell, 5)),
        vec![rejected(1, RejectReason::DuplicateOrderId)]
    );
    assert_eq!(resting(&e), before, "rejects leave the book unchanged");
    // Once the order is gone, the id is no longer live (the sequencer
    // guarantees uniqueness; the engine only guards live ids).
    run(&mut e, Command::Cancel { id: OrderId(1) });
    assert_eq!(
        run(&mut e, limit(1, Sell, 100, 1)),
        vec![accepted(1), rested(1, Sell, 100, 1)]
    );
}

#[test]
fn w07_same_commands_same_events_and_state_hash() {
    let mut rng = Rng::new(7);
    let mut cmds = Vec::new();
    for id in 1..=3_000u64 {
        let cmd = match rng.range(0, 9) {
            0..=5 => limit(id, rng.side(), rng.range_i64(95, 105), rng.range(1, 10)),
            6 => market(id, rng.side(), rng.range(1, 10)),
            _ => Command::Cancel {
                id: OrderId(rng.range(1, id)),
            },
        };
        cmds.push(cmd);
    }
    let (mut a, mut b) = (engine(), engine());
    // The engine is generic over the book: a boxed trait object works too.
    let mut dynamic: Engine<Box<dyn OrderBook>> =
        Engine::with_book(EngineConfig::default(), Box::new(Book::new()));
    let (mut ea, mut eb, mut ed) = (Vec::new(), Vec::new(), Vec::new());
    for cmd in &cmds {
        a.process(cmd, &mut ea);
        b.process(cmd, &mut eb);
        dynamic.process(cmd, &mut ed);
        assert!(!a.book().is_crossed(), "book crossed after {cmd:?}");
    }
    assert_eq!(ea, eb);
    assert_eq!(ea, ed);
    assert_eq!(dynamic.state_hash(), a.state_hash());
    assert!(ea.iter().any(|e| matches!(e, Event::Trade { .. })));
    assert_eq!(a.seq(), Seq(3_000));
    assert_eq!(a.state_hash(), b.state_hash());
    assert_eq!(a.state_hash(), a.clone().state_hash(), "hashing is pure");

    // Any further command — even a rejected one — changes the hash.
    let h = a.state_hash();
    let mut out = Vec::new();
    a.process(
        &Command::Cancel {
            id: OrderId(u64::MAX),
        },
        &mut out,
    );
    assert_ne!(a.state_hash(), h);
    // Different book, different hash.
    b.process(&limit(9_999, Buy, 1, 1), &mut out);
    assert_ne!(b.state_hash(), a.state_hash());

    // Read API for market data / the visualizer.
    let snap = a.snapshot(3);
    assert_eq!(snap.seq, a.seq());
    assert!(snap.bids.len() <= 3 && snap.asks.len() <= 3);
    assert_eq!(snap.best_bid(), a.book().best_bid());
    assert_eq!(snap.best_ask(), a.book().best_ask());
    assert!(snap.bids.windows(2).all(|w| w[0].price > w[1].price));
    assert!(snap.asks.windows(2).all(|w| w[0].price < w[1].price));
    assert_eq!(snap.last_price, a.last_trade_price());
    assert_eq!(snap.trades.len(), a.tape().len());
    assert_eq!(snap.trades.last(), a.tape().last());
    assert_eq!(snap.trades.last().map(|t| t.price), a.last_trade_price());
}
