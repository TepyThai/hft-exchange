//! Week 12: generator, invariants, differential testing and shrinking.

use engine::testkit::{
    differential, shrink, FlowConfig, InvariantChecker, Matcher, NaiveEngine, OrderFlow,
};
use engine::{Book, Engine, EngineConfig, OrderBook, RestingOrder};
use types::{
    Command, Event, InstrumentSpec, NewOrder, OrderId, OrderKind, Price, Qty, RejectReason, Side,
    TimeInForce,
};

fn engine_for(cfg: &FlowConfig) -> Engine {
    Engine::with_config(cfg.engine_config())
}

/// Runs `cmds` through a fresh engine with the invariant checker.
fn check_stream(cfg: &FlowConfig, cmds: &[Command]) -> Result<(), String> {
    let mut e = engine_for(cfg);
    let mut checker = InvariantChecker::new();
    let mut out = Vec::new();
    for cmd in cmds {
        out.clear();
        e.process(cmd, &mut out);
        checker
            .check(cmd, &out, e.book())
            .map_err(|v| v.to_string())?;
    }
    Ok(())
}

fn diff_fails(cfg: &FlowConfig, cmds: &[Command]) -> bool {
    let mut a = engine_for(cfg);
    let mut b = NaiveEngine::new(cfg.engine_config());
    differential(&mut a, &mut b, cmds).is_err()
}

#[test]
fn w12_flow_is_deterministic_and_covers_every_command_kind() {
    let cfg = FlowConfig {
        seed: 42,
        ..FlowConfig::default()
    };
    let a = OrderFlow::new(cfg).take_commands(20_000);
    assert_eq!(
        a,
        OrderFlow::new(cfg).take_commands(20_000),
        "same seed, same stream"
    );
    assert_ne!(
        a,
        OrderFlow::new(FlowConfig { seed: 43, ..cfg }).take_commands(20_000)
    );

    let (mut gtc, mut ioc, mut fok, mut post, mut market, mut cancel, mut reduce, mut zero) =
        (0, 0, 0, 0, 0, 0, 0, 0);
    for cmd in &a {
        match cmd {
            Command::New(o) if o.qty.is_zero() => zero += 1,
            Command::New(NewOrder {
                kind: OrderKind::Market,
                ..
            }) => market += 1,
            Command::New(o) => {
                match o.tif {
                    TimeInForce::Gtc => gtc += 1,
                    TimeInForce::Ioc => ioc += 1,
                    TimeInForce::Fok => fok += 1,
                }
                post += matches!(
                    o.kind,
                    OrderKind::Limit {
                        post_only: true,
                        ..
                    }
                ) as u32;
            }
            Command::Cancel { .. } => cancel += 1,
            Command::Reduce { .. } => reduce += 1,
        }
    }
    for (name, n) in [
        ("gtc", gtc),
        ("ioc", ioc),
        ("fok", fok),
        ("post-only", post),
        ("market", market),
        ("cancel", cancel),
        ("reduce", reduce),
        ("zero qty", zero),
    ] {
        assert!(n > 20, "{name}: only {n} in 20k commands");
    }

    // With feedback, cancels and reduces target orders that really rest,
    // and the book settles around the configured size.
    let mut flow = OrderFlow::new(cfg);
    let mut e = engine_for(&cfg);
    let (mut out, mut cancels, mut hits) = (Vec::new(), 0, 0);
    for _ in 0..20_000 {
        let cmd = flow.next_command();
        out.clear();
        e.process(&cmd, &mut out);
        flow.observe(&cmd, &out);
        if matches!(cmd, Command::Cancel { .. }) {
            cancels += 1;
            hits += matches!(out[0], Event::Cancelled { .. }) as u32;
        }
    }
    assert!(
        hits * 100 >= cancels * 90,
        "only {hits}/{cancels} cancels hit a live order"
    );
    let size = e.book().len();
    assert!(
        size > cfg.target_resting / 2 && size < 2 * cfg.target_resting,
        "book size {size}"
    );
    assert!(
        e.book().len().abs_diff(flow.live_orders()) <= 10,
        "generator lost track of the book"
    );
}

#[test]
fn w12_invariants_hold_on_long_random_streams() {
    for seed in 0..6 {
        let cfg = FlowConfig {
            seed,
            ..FlowConfig::default()
        };
        let mut flow = OrderFlow::new(cfg);
        let mut e = engine_for(&cfg);
        let mut checker = InvariantChecker::new();
        let mut out = Vec::new();
        for _ in 0..20_000 {
            let cmd = flow.next_command();
            out.clear();
            e.process(&cmd, &mut out);
            if let Err(v) = checker.check(&cmd, &out, e.book()) {
                panic!("seed {seed}: {v}\ncommand {cmd:?}\nevents {out:?}");
            }
            flow.observe(&cmd, &out);
        }
        let t = checker.totals();
        assert!(t.is_balanced(), "{t:?}");
        assert!(
            t.traded > 0 && t.cancelled > 0 && t.expired > 0 && t.reduced > 0 && t.resting > 0,
            "{t:?}"
        );
        assert_eq!(checker.commands(), 20_000);
    }
}

/// Feeds a hand-written (possibly wrong) engine run to the checker. The
/// book is maintained by hand to match what the "engine" claims.
struct Scenario {
    checker: InvariantChecker,
    book: Book,
}

impl Scenario {
    fn new() -> Self {
        Scenario {
            checker: InvariantChecker::new(),
            book: Book::new(),
        }
    }

    fn rest(&mut self, id: u64, side: Side, price: i64, qty: u64) {
        self.book
            .add(RestingOrder::new(id, side, price, qty))
            .unwrap();
        let cmd = Command::New(NewOrder::limit(id, side, price, qty));
        let events = [
            Event::Accepted { id: OrderId(id) },
            Event::Rested {
                id: OrderId(id),
                side,
                price: Price(price),
                qty: Qty(qty),
            },
        ];
        self.checker
            .check(&cmd, &events, &self.book)
            .expect("valid setup");
    }

    fn feed(&mut self, cmd: Command, events: &[Event]) -> String {
        match self.checker.check(&cmd, events, &self.book) {
            Ok(()) => panic!("checker accepted a broken run: {cmd:?} → {events:?}"),
            Err(v) => v.message,
        }
    }
}

#[test]
fn w12_checker_catches_broken_engines() {
    let buy = |id, price, qty| Command::New(NewOrder::limit(id, Side::Buy, price, qty));
    let acc = |id| Event::Accepted { id: OrderId(id) };
    let trade = |taker, maker, price, qty| Event::Trade {
        taker: OrderId(taker),
        maker: OrderId(maker),
        taker_side: Side::Buy,
        price: Price(price),
        qty: Qty(qty),
    };

    // Trade at the taker's price instead of the maker's.
    let mut s = Scenario::new();
    s.rest(1, Side::Sell, 100, 5);
    s.book.fill_front(Side::Sell, Qty(5));
    assert!(s
        .feed(buy(2, 101, 5), &[acc(2), trade(2, 1, 101, 5)])
        .contains("maker rests at 100"));

    // FIFO broken: the younger maker traded first.
    let mut s = Scenario::new();
    s.rest(1, Side::Sell, 100, 5);
    s.rest(2, Side::Sell, 100, 5);
    s.book.cancel(OrderId(2)).unwrap();
    assert!(s
        .feed(buy(3, 100, 5), &[acc(3), trade(3, 2, 100, 5)])
        .contains("skipped"));

    // Remainder silently dropped (no Rested / Expired).
    let mut s = Scenario::new();
    assert!(s
        .feed(buy(1, 90, 5), &[acc(1)])
        .contains("neither Rested nor Expired"));

    // Matching stopped while a crossing maker was left.
    let mut s = Scenario::new();
    s.rest(1, Side::Sell, 100, 5);
    let ioc = Command::New(NewOrder::limit(2, Side::Buy, 100, 3).with_tif(TimeInForce::Ioc));
    assert!(s
        .feed(
            ioc,
            &[
                acc(2),
                Event::Expired {
                    id: OrderId(2),
                    remaining: Qty(3)
                }
            ]
        )
        .contains("stopped matching"));

    // A book left crossed (the engine rested instead of matching).
    let mut s = Scenario::new();
    s.rest(1, Side::Sell, 100, 5);
    s.book.add(RestingOrder::new(2, Side::Buy, 101, 5)).unwrap();
    let rested = Event::Rested {
        id: OrderId(2),
        side: Side::Buy,
        price: Price(101),
        qty: Qty(5),
    };
    assert!(s
        .feed(buy(2, 101, 5), &[acc(2), rested])
        .contains("stopped matching"));

    // Quantity not conserved: the book lost 2 lots on a cancel.
    let mut s = Scenario::new();
    s.rest(1, Side::Sell, 100, 5);
    s.book.cancel(OrderId(1)).unwrap();
    let wrong = Event::Cancelled {
        id: OrderId(1),
        remaining: Qty(3),
    };
    assert!(s
        .feed(Command::Cancel { id: OrderId(1) }, &[wrong])
        .contains("had 5 left"));

    // A reject that is not alone.
    let mut s = Scenario::new();
    let both = [
        Event::Rejected {
            id: OrderId(1),
            reason: RejectReason::ZeroQuantity,
        },
        acc(1),
    ];
    assert!(s.feed(buy(1, 100, 0), &both).contains("only event"));

    // A phantom order on the book.
    let mut s = Scenario::new();
    s.book.add(RestingOrder::new(7, Side::Buy, 50, 1)).unwrap();
    let unknown = Event::Rejected {
        id: OrderId(9),
        reason: RejectReason::UnknownOrder,
    };
    assert!(s
        .feed(Command::Cancel { id: OrderId(9) }, &[unknown])
        .contains("phantom"));
}

#[test]
fn w12_naive_reference_follows_the_event_contract() {
    let spec = InstrumentSpec {
        tick: 1,
        lot: 1,
        band_ticks: 50,
        ..InstrumentSpec::default()
    };
    let config = EngineConfig::new(spec).with_reference_price(Price(100));
    let mut naive = NaiveEngine::new(config);
    let mut out = Vec::new();
    let mut run = |n: &mut NaiveEngine, cmd: Command| {
        out.clear();
        n.process(&cmd, &mut out);
        out.clone()
    };
    for (id, price, qty) in [(1, 101, 2), (2, 100, 3), (3, 100, 4), (4, 102, 5)] {
        run(
            &mut naive,
            Command::New(NewOrder::limit(id, Side::Sell, price, qty)),
        );
    }
    run(
        &mut naive,
        Command::Reduce {
            id: OrderId(2),
            qty: Qty(1),
        },
    );
    let t = |maker, price, qty| Event::Trade {
        taker: OrderId(9),
        maker: OrderId(maker),
        taker_side: Side::Buy,
        price: Price(price),
        qty: Qty(qty),
    };
    assert_eq!(
        run(
            &mut naive,
            Command::New(NewOrder::limit(9, Side::Buy, 101, 9))
        ),
        vec![
            Event::Accepted { id: OrderId(9) },
            t(2, 100, 1),
            t(3, 100, 4),
            t(1, 101, 2),
            Event::Rested {
                id: OrderId(9),
                side: Side::Buy,
                price: Price(101),
                qty: Qty(2)
            },
        ]
    );
    assert_eq!(naive.last_trade_price(), Some(Price(101)));
    let mut book = Vec::new();
    naive.resting(&mut book);
    assert_eq!(
        book,
        vec![
            RestingOrder::new(9, Side::Buy, 101, 2),
            RestingOrder::new(4, Side::Sell, 102, 5)
        ]
    );
    let reject = |id, reason| {
        vec![Event::Rejected {
            id: OrderId(id),
            reason,
        }]
    };
    assert_eq!(
        run(
            &mut naive,
            Command::New(NewOrder::limit(10, Side::Buy, 200, 1))
        ),
        reject(10, RejectReason::PriceBand)
    );
    assert_eq!(
        run(
            &mut naive,
            Command::New(NewOrder::market(11, Side::Buy, 6).with_tif(TimeInForce::Fok))
        ),
        reject(11, RejectReason::FokUnfillable)
    );
    assert_eq!(
        run(
            &mut naive,
            Command::New(NewOrder::limit(12, Side::Sell, 101, 1).post_only())
        ),
        reject(12, RejectReason::WouldTake)
    );
    assert_eq!(
        run(&mut naive, Command::Cancel { id: OrderId(1) }),
        reject(1, RejectReason::UnknownOrder)
    );

    // The reference matcher also satisfies every invariant (its book is
    // mirrored into a `Book` so the checker can inspect it).
    let cfg = FlowConfig::randomized(3);
    let mut naive = NaiveEngine::new(cfg.engine_config());
    let mut checker = InvariantChecker::new();
    let (mut events, mut orders) = (Vec::new(), Vec::new());
    for cmd in OrderFlow::new(cfg).take(3_000) {
        events.clear();
        naive.process(&cmd, &mut events);
        Matcher::resting(&naive, &mut orders);
        let mut mirror = Book::new();
        orders.iter().for_each(|&o| mirror.add(o).unwrap());
        checker.check(&cmd, &events, &mirror).unwrap();
    }
}

#[test]
fn w12_engine_matches_naive_on_thousands_of_random_streams() {
    let mut commands = 0;
    for seed in 0..2_000 {
        let cfg = FlowConfig::randomized(seed);
        let stream = OrderFlow::new(cfg).take_commands(250);
        commands += stream.len();
        if diff_fails(&cfg, &stream) {
            let minimal = shrink(&stream, |cmds| diff_fails(&cfg, cmds));
            let mut a = engine_for(&cfg);
            let mut b = NaiveEngine::new(cfg.engine_config());
            let why = differential(&mut a, &mut b, &minimal).unwrap_err();
            panic!("seed {seed} ({cfg:?}): engine and naive disagree.\nminimal stream: {minimal:#?}\n{why}");
        }
        if let Err(v) = check_stream(&cfg, &stream) {
            let minimal = shrink(&stream, |cmds| check_stream(&cfg, cmds).is_err());
            panic!("seed {seed}: {v}\nminimal stream: {minimal:#?}");
        }
    }
    assert_eq!(commands, 500_000);
}

#[test]
fn w12_shrink_finds_a_one_minimal_stream() {
    let cfg = FlowConfig {
        seed: 12,
        levels: 4,
        ..FlowConfig::default()
    };
    let stream = OrderFlow::new(cfg).take_commands(400);
    // Property under test: "some command trades with two makers".
    let sweeps = |cmds: &[Command]| {
        let mut e = engine_for(&cfg);
        let mut out = Vec::new();
        cmds.iter().any(|cmd| {
            out.clear();
            e.process(cmd, &mut out);
            out.iter()
                .filter(|ev| matches!(ev, Event::Trade { .. }))
                .count()
                >= 2
        })
    };
    assert!(sweeps(&stream), "the stream should contain a sweep");
    let mut calls = 0;
    let minimal = shrink(&stream, |cmds| {
        calls += 1;
        sweeps(cmds)
    });
    assert!(sweeps(&minimal));
    assert_eq!(minimal.len(), 3, "two makers and one taker: {minimal:?}");
    for skip in 0..minimal.len() {
        let mut fewer = minimal.clone();
        fewer.remove(skip);
        assert!(!sweeps(&fewer), "not 1-minimal: command {skip} can go");
    }
    assert!(calls < 400, "{calls} predicate calls");

    // A subsequence of the input, in order.
    let mut rest = stream.iter();
    assert!(minimal.iter().all(|c| rest.any(|s| s == c)));
    // A passing stream comes back unchanged.
    let calm = &stream[..1];
    assert_eq!(shrink(calm, sweeps), calm.to_vec());
}

/// An engine with a planted bug: market orders only trade at the best
/// price level (as if converted to an IOC limit at the touch).
struct TouchOnlyMarkets(Engine);

impl Matcher for TouchOnlyMarkets {
    fn process(&mut self, cmd: &Command, out: &mut Vec<Event>) {
        let mut cmd = *cmd;
        if let Command::New(o) = &mut cmd {
            if let (OrderKind::Market, Some(best)) = (o.kind, self.0.book().best(o.side.opposite()))
            {
                o.kind = OrderKind::Limit {
                    price: best.price,
                    post_only: false,
                };
                if o.tif == TimeInForce::Gtc {
                    o.tif = TimeInForce::Ioc;
                }
            }
        }
        self.0.process(&cmd, out);
    }

    fn resting(&self, out: &mut Vec<RestingOrder>) {
        Matcher::resting(&self.0, out);
    }
}

#[test]
fn w12_differential_catches_and_shrinks_a_planted_bug() {
    let cfg = FlowConfig {
        seed: 1212,
        levels: 3,
        p_market: 0.1,
        ..FlowConfig::default()
    };
    let stream = OrderFlow::new(cfg).take_commands(2_000);
    let fails = |cmds: &[Command]| {
        let mut buggy = TouchOnlyMarkets(engine_for(&cfg));
        let mut naive = NaiveEngine::new(cfg.engine_config());
        differential(&mut buggy, &mut naive, cmds).is_err()
    };
    assert!(fails(&stream), "the planted bug must be found");
    let minimal = shrink(&stream, fails);
    // Two resting orders at different prices and a market order that
    // needs both levels.
    assert_eq!(minimal.len(), 3, "{minimal:#?}");
    assert!(matches!(
        minimal[2],
        Command::New(NewOrder {
            kind: OrderKind::Market,
            ..
        })
    ));
    let mut buggy = TouchOnlyMarkets(engine_for(&cfg));
    let mut naive = NaiveEngine::new(cfg.engine_config());
    let mismatch = differential(&mut buggy, &mut naive, &minimal).unwrap_err();
    assert_eq!(mismatch.index, 2);
    assert!(mismatch.to_string().contains("diverge at command #2"));
}
