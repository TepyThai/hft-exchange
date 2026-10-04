//! Week 13: the TCP gateway end to end, over loopback, with the blocking
//! test client.

use engine::EngineConfig;
use gateway::client::Client;
use gateway::{Auth, Gateway, GatewayConfig, Hooks, NoStage, PreTrade, Sequencer, SessionId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use types::{
    AccountId, Command, Event, InstrumentSpec, NewOrder, OrderId, OrderKind, Price, Qty,
    RejectReason, RiskReason, Rng, Side, TimeInForce,
};
use wire::{Logon, Message};

fn start(config: GatewayConfig) -> Gateway {
    Gateway::start(config).expect("gateway starts")
}

fn logged_on(gw: &Gateway, account: u32) -> (Client, SessionId) {
    let mut c = Client::connect(gw.local_addr()).expect("connect");
    let ack = c.logon(account, 0).expect("logon");
    assert_eq!(ack.account, AccountId(account));
    (c, SessionId(ack.session))
}

fn accepted(id: u64) -> Event {
    Event::Accepted { id: OrderId(id) }
}

fn rejected(id: u64, reason: RejectReason) -> Event {
    Event::Rejected {
        id: OrderId(id),
        reason,
    }
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn w13_logon_required() {
    let gw = start(GatewayConfig {
        auth: Auth::Tokens(vec![(AccountId(7), 42), (AccountId(8), 43)]),
        logon_timeout: Duration::from_millis(200),
        ..GatewayConfig::default()
    });

    // An order before logon: the connection is closed, nothing is sent back.
    let mut c = Client::connect(gw.local_addr()).unwrap();
    c.send_order(&NewOrder::limit(1, Side::Buy, 100, 1))
        .unwrap();
    assert_eq!(c.read_to_close().unwrap(), vec![]);

    // Wrong token, then an unknown account.
    for (account, token) in [(7, 41), (9, 42)] {
        let mut c = Client::connect(gw.local_addr()).unwrap();
        c.send(Message::Logon(Logon {
            account: AccountId(account),
            token,
        }))
        .unwrap();
        assert_eq!(
            c.read_to_close().unwrap(),
            vec![],
            "account {account} token {token}"
        );
    }

    // Good credentials: LogonAck with the account and a fresh session id.
    let mut a = Client::connect(gw.local_addr()).unwrap();
    let ack_a = a.logon(7, 42).unwrap();
    let mut b = Client::connect(gw.local_addr()).unwrap();
    let ack_b = b.logon(8, 43).unwrap();
    assert_eq!((ack_a.account, ack_b.account), (AccountId(7), AccountId(8)));
    assert_ne!(ack_a.session, ack_b.session);
    a.send_order(&NewOrder::limit(1, Side::Buy, 100, 1))
        .unwrap();
    assert_eq!(a.recv_event().unwrap(), accepted(1));

    // A second logon on an active session closes it.
    b.send(Message::Logon(Logon {
        account: AccountId(8),
        token: 43,
    }))
    .unwrap();
    assert_eq!(b.read_to_close().unwrap(), vec![]);

    // Silence: closed after the logon timeout.
    let mut quiet = Client::connect(gw.local_addr()).unwrap();
    let t = Instant::now();
    assert_eq!(quiet.read_to_close().unwrap(), vec![]);
    assert!(
        t.elapsed() >= Duration::from_millis(150),
        "closed too early: {:?}",
        t.elapsed()
    );

    let stats = gw.stats();
    assert_eq!(stats.logons, 2);
    assert_eq!(stats.protocol_errors, 5);
    assert_eq!(
        stats.sequenced, 1,
        "only the logged-on order reached the engine"
    );
    assert!(gw.shutdown(Duration::from_secs(10)).is_clean());
}

#[test]
fn w13_order_ack_and_fill_reach_the_right_sessions() {
    let gw = start(GatewayConfig::default());
    let (mut a, _) = logged_on(&gw, 1);
    let (mut b, _) = logged_on(&gw, 2);

    a.send_order(&NewOrder::limit(1, Side::Sell, 100, 10))
        .unwrap();
    assert_eq!(
        a.recv_events(2).unwrap(),
        vec![
            accepted(1),
            Event::Rested {
                id: OrderId(1),
                side: Side::Sell,
                price: Price(100),
                qty: Qty(10)
            },
        ]
    );

    // B lifts 4: B sees its own id as taker, A its own id as maker; neither
    // sees the other's.
    let trade = |taker: u64, maker: u64, qty: u64| Event::Trade {
        taker: OrderId(taker),
        maker: OrderId(maker),
        taker_side: Side::Buy,
        price: Price(100),
        qty: Qty(qty),
    };
    b.send_order(&NewOrder::limit(1, Side::Buy, 100, 4))
        .unwrap();
    assert_eq!(b.recv_events(2).unwrap(), vec![accepted(1), trade(1, 0, 4)]);
    assert_eq!(a.recv_event().unwrap(), trade(0, 1, 4));

    // An IOC that takes the rest.
    b.send_order(&NewOrder::limit(2, Side::Buy, 101, 6).with_tif(TimeInForce::Ioc))
        .unwrap();
    assert_eq!(b.recv_events(2).unwrap(), vec![accepted(2), trade(2, 0, 6)]);
    assert_eq!(a.recv_event().unwrap(), trade(0, 1, 6));

    // A's order is fully filled, so the router forgot it.
    a.cancel(1).unwrap();
    assert_eq!(
        a.recv_event().unwrap(),
        rejected(1, RejectReason::UnknownOrder)
    );
    // Nothing else is queued for either session.
    for c in [&mut a, &mut b] {
        c.cancel(99).unwrap();
        assert_eq!(
            c.recv_event().unwrap(),
            rejected(99, RejectReason::UnknownOrder)
        );
    }
    assert!(gw.shutdown(Duration::from_secs(10)).is_clean());
}

#[test]
fn w13_client_ids_are_remapped_per_session() {
    let gw = start(GatewayConfig::default());
    let (mut a, _) = logged_on(&gw, 1);
    let (mut b, _) = logged_on(&gw, 2);
    let (mut c, _) = logged_on(&gw, 3);

    // Both sessions use client id 5.
    a.send_order(&NewOrder::limit(5, Side::Buy, 90, 1)).unwrap();
    assert_eq!(a.recv_event().unwrap(), accepted(5));
    a.recv_event().unwrap(); // Rested
    b.send_order(&NewOrder::limit(5, Side::Buy, 91, 2)).unwrap();
    assert_eq!(b.recv_event().unwrap(), accepted(5));
    b.recv_event().unwrap();

    // Within a session a live id is a duplicate.
    a.send_order(&NewOrder::limit(5, Side::Buy, 92, 1)).unwrap();
    assert_eq!(
        a.recv_event().unwrap(),
        rejected(5, RejectReason::DuplicateOrderId)
    );

    // A's cancel of "5" is A's order, not B's.
    a.cancel(5).unwrap();
    assert_eq!(
        a.recv_event().unwrap(),
        Event::Cancelled {
            id: OrderId(5),
            remaining: Qty(1)
        }
    );
    c.send_order(&NewOrder::limit(1, Side::Sell, 91, 2))
        .unwrap();
    assert_eq!(c.recv_event().unwrap(), accepted(1));
    let fill = c.recv_event().unwrap();
    assert!(
        matches!(
            fill,
            Event::Trade {
                taker: OrderId(1),
                maker: OrderId(0),
                qty: Qty(2),
                ..
            }
        ),
        "{fill:?}"
    );
    let fill = b.recv_event().unwrap();
    assert!(
        matches!(
            fill,
            Event::Trade {
                taker: OrderId(0),
                maker: OrderId(5),
                price: Price(91),
                ..
            }
        ),
        "{fill:?}"
    );

    // Once done, an id can be reused; cancels and reduces of ids the
    // session never used are rejected without reaching the engine.
    a.send_order(&NewOrder::limit(5, Side::Buy, 89, 3)).unwrap();
    assert_eq!(a.recv_event().unwrap(), accepted(5));
    a.recv_event().unwrap();
    a.reduce(5, 1).unwrap();
    assert_eq!(
        a.recv_event().unwrap(),
        Event::Reduced {
            id: OrderId(5),
            remaining: Qty(1)
        }
    );
    let before = gw.stats().sequenced;
    a.cancel(77).unwrap();
    assert_eq!(
        a.recv_event().unwrap(),
        rejected(77, RejectReason::UnknownOrder)
    );
    b.reduce(77, 1).unwrap();
    assert_eq!(
        b.recv_event().unwrap(),
        rejected(77, RejectReason::UnknownOrder)
    );
    assert_eq!(gw.stats().sequenced, before);
    assert!(gw.shutdown(Duration::from_secs(10)).is_clean());

    // The same traffic through a sequencer: the two "5"s got distinct
    // exchange ids, assigned 1, 2, … in sequence order.
    let mut seq = Sequencer::new(&[EngineConfig::default()], Hooks::default(), true);
    let mut sink = |_: SessionId, _: Message| {};
    for (s, price) in [(1, 90), (2, 91)] {
        seq.command(
            SessionId(s),
            0,
            &Command::New(NewOrder::limit(5, Side::Buy, price, 1)),
            &mut sink,
        );
    }
    assert_eq!(
        seq.router().lookup(SessionId(1), OrderId(5)),
        Some(OrderId(1))
    );
    assert_eq!(
        seq.router().lookup(SessionId(2), OrderId(5)),
        Some(OrderId(2))
    );
    assert_eq!(seq.router().next_id(), OrderId(3));
}

#[test]
fn w13_malformed_frame_closes_only_that_session() {
    let gw = start(GatewayConfig {
        cancel_on_disconnect: true,
        ..GatewayConfig::default()
    });
    let (mut a, _) = logged_on(&gw, 1);
    a.send_order(&NewOrder::limit(1, Side::Sell, 100, 1))
        .unwrap();
    a.recv_events(2).unwrap();

    let garbage: [&[u8]; 4] = [
        &[0, 0],                   // zero-length frame
        &[0xff, 0xff, 1, 2, 3],    // longer than the decoder's limit
        &[1, 0, b'Z'],             // unknown message type
        &[5, 0, b'N', 0, 0, 0, 0], // truncated NewOrder
    ];
    for bytes in garbage {
        let (mut x, _) = logged_on(&gw, 9);
        x.send_order(&NewOrder::limit(1, Side::Buy, 50, 3)).unwrap();
        x.send_raw(bytes).unwrap();
        let mut got = x.read_to_close().unwrap();
        // Its resting order is cancelled on disconnect, through the engine.
        assert_eq!(
            got.pop(),
            Some(Message::Event(Event::Cancelled {
                id: OrderId(1),
                remaining: Qty(3)
            })),
            "{bytes:?}"
        );
        assert_eq!(got.len(), 2, "Accepted + Rested before the cancel: {got:?}");
    }

    // A was never disturbed: its order is still there to trade with.
    let (mut b, _) = logged_on(&gw, 2);
    b.send_order(&NewOrder::limit(1, Side::Buy, 100, 1))
        .unwrap();
    assert_eq!(b.recv_event().unwrap(), accepted(1));
    assert!(matches!(
        b.recv_event().unwrap(),
        Event::Trade {
            taker: OrderId(1),
            ..
        }
    ));
    assert!(matches!(
        a.recv_event().unwrap(),
        Event::Trade {
            maker: OrderId(1),
            ..
        }
    ));
    assert_eq!(gw.stats().protocol_errors, 4);
    assert!(gw.shutdown(Duration::from_secs(10)).is_clean());
}

/// A pre-trade check for the test: orders above `max_qty` are fat-finger
/// rejects, and each session may send `burst` orders in total.
struct TestGate {
    max_qty: u64,
    burst: u32,
    sent: HashMap<SessionId, u32>,
    log: Arc<Mutex<GateLog>>,
}

#[derive(Default)]
struct GateLog {
    checked: Vec<(SessionId, NewOrder)>,
    events: Vec<Event>,
    closed: Vec<SessionId>,
}

impl PreTrade for TestGate {
    fn check(
        &mut self,
        session: SessionId,
        order: &NewOrder,
        _rx_ns: u64,
        _reference: Option<Price>,
    ) -> Result<(), RejectReason> {
        self.log.lock().unwrap().checked.push((session, *order));
        if order.qty.0 > self.max_qty {
            return Err(RejectReason::Risk(RiskReason::MaxOrderQty));
        }
        let n = self.sent.entry(session).or_default();
        if *n >= self.burst {
            return Err(RejectReason::Risk(RiskReason::RateLimit));
        }
        *n += 1;
        Ok(())
    }

    fn on_event(&mut self, event: &Event) {
        self.log.lock().unwrap().events.push(*event);
    }

    fn on_close(&mut self, session: SessionId) {
        self.log.lock().unwrap().closed.push(session);
    }
}

#[test]
fn w13_pretrade_rejects_come_back_as_rejected() {
    let log = Arc::new(Mutex::new(GateLog::default()));
    let gate = TestGate {
        max_qty: 100,
        burst: 3,
        sent: HashMap::new(),
        log: Arc::clone(&log),
    };
    let gw = Gateway::start_with(
        GatewayConfig::default(),
        Hooks {
            pretrade: Box::new(gate),
            stage: Box::new(NoStage),
        },
    )
    .unwrap();
    let (mut a, sa) = logged_on(&gw, 7);

    // The client claims account 999; the session trades for account 7.
    let order = |id: u64, qty: u64| NewOrder::limit(id, Side::Buy, 100, qty).with_account(999);
    a.send_order(&order(100, 500)).unwrap();
    assert_eq!(
        a.recv_event().unwrap(),
        rejected(100, RejectReason::Risk(RiskReason::MaxOrderQty))
    );
    for id in 101..=103 {
        a.send_order(&order(id, 10)).unwrap();
        assert_eq!(a.recv_events(2).unwrap()[0], accepted(id));
    }
    a.send_order(&order(104, 10)).unwrap();
    assert_eq!(
        a.recv_event().unwrap(),
        rejected(104, RejectReason::Risk(RiskReason::RateLimit))
    );
    // A rejected id was never live: it can be sent again (and is rejected
    // again, by the throttle this time).
    a.send_order(&order(100, 1)).unwrap();
    assert_eq!(
        a.recv_event().unwrap(),
        rejected(100, RejectReason::Risk(RiskReason::RateLimit))
    );

    // Rejects never reached the engine.
    assert_eq!(gw.stats().sequenced, 3);
    drop(a);
    wait_for("the session to close", || gw.stats().closed == 1);
    assert!(gw.shutdown(Duration::from_secs(10)).is_clean());

    let log = log.lock().unwrap();
    // The hook saw exchange ids (the next id to assign), the session's
    // account and every order, in order.
    let ids: Vec<u64> = log.checked.iter().map(|(_, o)| o.id.0).collect();
    assert_eq!(ids, vec![1, 1, 2, 3, 4, 4]);
    assert!(log
        .checked
        .iter()
        .all(|(s, o)| *s == sa && o.account == AccountId(7)));
    // ... and the engine's events for the accepted orders, in exchange ids.
    let acks: Vec<Event> = log
        .events
        .iter()
        .filter(|e| matches!(e, Event::Accepted { .. }))
        .copied()
        .collect();
    assert_eq!(acks, vec![accepted(1), accepted(2), accepted(3)]);
    // Cancel-on-disconnect cancelled the three resting orders, then the
    // hook heard the session close.
    assert_eq!(
        log.events
            .iter()
            .filter(|e| matches!(e, Event::Cancelled { .. }))
            .count(),
        3
    );
    assert_eq!(log.closed, vec![sa]);
}

#[test]
fn w13_shutdown_drains() {
    const SESSIONS: usize = 3;
    const ORDERS: u64 = 1500;
    let gw = start(GatewayConfig {
        cancel_on_disconnect: false,
        inbound_capacity: 256,
        outbound_capacity: 1 << 14,
        ..GatewayConfig::default()
    });
    let mut readers = Vec::new();
    for s in 0..SESSIONS {
        let (mut c, _) = logged_on(&gw, s as u32 + 1);
        // Pipelined: every order is sent before any report is read.
        let orders: Vec<Message> = (1..=ORDERS)
            .map(|id| Command::New(NewOrder::limit(id, Side::Buy, 50 + s as i64, 1)).into())
            .collect();
        for chunk in orders.chunks(100) {
            c.send_all(chunk).unwrap();
        }
        readers.push(std::thread::spawn(move || c.read_to_close().unwrap()));
    }
    let total = SESSIONS as u64 * ORDERS;
    wait_for("every order to be read", || gw.stats().commands_in == total);

    let report = gw.shutdown(Duration::from_secs(20));
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(report.sequenced, total);
    assert_eq!(report.sessions_closed, SESSIONS);

    // Every order read before the shutdown got both its reports, in order,
    // then a clean close.
    for (s, r) in readers.into_iter().enumerate() {
        let got = r.join().unwrap();
        assert_eq!(got.len(), 2 * ORDERS as usize, "session {s}");
        for (k, pair) in got.chunks(2).enumerate() {
            let id = OrderId(k as u64 + 1);
            assert_eq!(pair[0], Message::Event(Event::Accepted { id }));
            assert!(matches!(pair[1], Message::Event(Event::Rested { id: i, .. }) if i == id));
        }
    }
}

/// Random command for `session`'s client, using client ids it has used
/// before (live or not) some of the time.
fn random_command(rng: &mut Rng, used: &mut Vec<u64>, next: &mut u64) -> Command {
    let pick_used = |rng: &mut Rng| -> u64 {
        if used.is_empty() || rng.chance(0.1) {
            1000 + rng.range(0, 50)
        } else {
            used[rng.range(0, used.len() as u64 - 1) as usize]
        }
    };
    let roll = rng.range(0, 99);
    if roll < 60 {
        let id = if rng.chance(0.1) && !used.is_empty() {
            pick_used(rng)
        } else {
            *next += 1;
            used.push(*next);
            *next
        };
        let side = rng.side();
        let qty = rng.range(1, 10);
        let mut o = if roll < 52 {
            NewOrder::limit(id, side, rng.range_i64(95, 105), qty)
        } else {
            NewOrder::market(id, side, qty)
        };
        if matches!(o.kind, OrderKind::Limit { .. }) {
            o.tif = match rng.range(0, 9) {
                0 => TimeInForce::Ioc,
                1 => TimeInForce::Fok,
                _ => TimeInForce::Gtc,
            };
            if rng.chance(0.05) {
                o = o.post_only();
            }
        }
        Command::New(o)
    } else if roll < 88 {
        Command::Cancel {
            id: OrderId(pick_used(rng)),
        }
    } else {
        Command::Reduce {
            id: OrderId(pick_used(rng)),
            qty: Qty(rng.range(0, 6)),
        }
    }
}

#[test]
fn w13_randomized_reports_match_an_in_process_sequencer() {
    const SESSIONS: usize = 4;
    let spec = InstrumentSpec::default();
    let gw = start(GatewayConfig {
        instruments: vec![EngineConfig::new(spec)],
        cancel_on_disconnect: true,
        ..GatewayConfig::default()
    });
    let mut model = Sequencer::new(&[EngineConfig::new(spec)], Hooks::default(), true);

    let mut clients = Vec::new();
    let mut ids = HashMap::new();
    for s in 0..SESSIONS {
        let account = 10 + s as u32;
        let (c, sid) = logged_on(&gw, account);
        model.open(sid, AccountId(account), &mut |_, _| {});
        ids.insert(sid, s);
        clients.push((c, sid, AccountId(account), Vec::new(), 0u64));
    }

    let mut rng = Rng::new(0x13);
    let mut kinds: HashMap<&'static str, usize> = HashMap::new();
    for _ in 0..3000 {
        let s = rng.range(0, SESSIONS as u64 - 1) as usize;
        let (client, sid, account, used, next) = &mut clients[s];
        let mut cmd = random_command(&mut rng, used, next);
        client.send(cmd).unwrap();
        if let Command::New(o) = &mut cmd {
            o.account = *account; // what the session forwards
        }
        let sid = *sid;
        let mut expected: Vec<(SessionId, Message)> = Vec::new();
        model.command(sid, 0, &cmd, &mut |to, m| expected.push((to, m)));
        for (to, want) in expected {
            let got = clients[ids[&to]].0.recv().unwrap();
            assert_eq!(got, Some(want), "report for session {to:?} after {cmd:?}");
            if let Message::Event(e) = want {
                *kinds.entry(event_kind(&e)).or_default() += 1;
            }
        }
    }
    // The stream exercised every kind of report.
    for k in [
        "accepted",
        "rejected",
        "trade",
        "rested",
        "cancelled",
        "expired",
        "reduced",
    ] {
        assert!(kinds.get(k).copied().unwrap_or(0) >= 10, "{k}: {kinds:?}");
    }
    assert_eq!(gw.stats().sequenced, model.sequenced());
    assert!(gw.shutdown(Duration::from_secs(10)).is_clean());
}

fn event_kind(e: &Event) -> &'static str {
    match e {
        Event::Accepted { .. } => "accepted",
        Event::Rejected { .. } => "rejected",
        Event::Trade { .. } => "trade",
        Event::Rested { .. } => "rested",
        Event::Cancelled { .. } => "cancelled",
        Event::Expired { .. } => "expired",
        Event::Reduced { .. } => "reduced",
    }
}
