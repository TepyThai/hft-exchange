//! Week 24: exchange v2 end to end — sessions → risk → journal → engine →
//! market data → execution reports, over real sockets and threads.

use engine::{Engine, EngineConfig};
use gateway::client::Client;
use gateway::exchange::{journal_path, PipelineStats, TradeTick};
use gateway::loadgen::{self, LoadConfig};
use gateway::{
    AllowAll, EngineIdle, ExchangeV2, Fsync, GatewayConfig, Hooks, JournalBackend, MemJournals,
    PreTrade, Sequencer, SessionId, V2Config, BUSY,
};
use journal::{FileStorage, Frames, JournalOptions, SystemClock, FIRST_SEQ};
use marketdata::{ws, BookBuilder, L2Publisher, L2Snapshot, L2Update, Recv};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use telemetry::Stage as Stamp;
use types::{
    AccountId, Command, Event, InstrumentId, InstrumentSpec, NewOrder, OrderId, Price, Qty, Rng,
    Seq, Side, TimeInForce,
};
use wire::Message;

const INSTRUMENT: InstrumentId = InstrumentId(1);
const TIMEOUT: Duration = Duration::from_secs(10);

/// A temporary directory removed on drop (also when the test fails).
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let p = std::env::temp_dir().join(format!("hft-w24-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// In-memory journals; per-session queues deep enough that a test client
/// pipelining thousands of orders before it reads is not cut as a slow
/// consumer.
fn in_memory(journals: &MemJournals) -> V2Config {
    V2Config::in_memory(
        GatewayConfig {
            outbound_capacity: 1 << 16,
            ..GatewayConfig::default()
        },
        journals.clone(),
    )
}

fn start(config: V2Config) -> ExchangeV2 {
    ExchangeV2::start(config, AllowAll).expect("exchange v2 starts")
}

fn logged_on(ex: &ExchangeV2, account: u32) -> (Client, SessionId) {
    let mut c = Client::connect(ex.local_addr()).unwrap();
    let ack = c.logon(account, 0).unwrap();
    (c, SessionId(ack.session))
}

fn limit(id: u64, side: Side, price: i64, qty: u64) -> Message {
    Command::New(NewOrder::limit(id, side, price, qty)).into()
}

/// Polls `f` until it returns `Some`, failing after [`TIMEOUT`].
fn eventually<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_micros(200));
    }
}

/// The next value of a ring reader (waiting for it).
fn next<T: marketdata::RingValue + std::fmt::Debug>(r: &mut marketdata::Reader<T>) -> T {
    eventually("a market-data value", || match r.try_recv() {
        Recv::Item(v) => Some(v),
        Recv::Empty => None,
        Recv::Lagged(n) => panic!("reader lagged by {n}"),
    })
}

/// Commands in a journal image, in sequence order.
fn journaled(bytes: &[u8]) -> Vec<Command> {
    Frames::new(bytes, FIRST_SEQ)
        .map(|f| f.expect("journal frame").command().expect("command"))
        .collect()
}

/// What the journal says happened: replays it through a fresh engine and
/// returns the trade tape and the final L2 book.
fn replay_truth(bytes: &[u8], cfg: EngineConfig) -> (Engine, Vec<TradeTick>, L2Snapshot) {
    let mut engine = Engine::with_config(cfg);
    let mut publisher = L2Publisher::new();
    let (mut events, mut tape) = (Vec::new(), Vec::new());
    for cmd in journaled(bytes) {
        events.clear();
        engine.process(&cmd, &mut events);
        for ev in &events {
            publisher.on_event(ev);
            if let Event::Trade {
                taker_side,
                price,
                qty,
                ..
            } = *ev
            {
                tape.push(TradeTick {
                    seq: Seq(tape.len() as u64 + 1),
                    taker_side,
                    price,
                    qty,
                });
            }
        }
    }
    let book = publisher.snapshot();
    (engine, tape, book)
}

/// `(price, qty)` per level, best first.
type Levels = Vec<(i64, u64)>;

/// A snapshot's bids and asks as plain pairs.
fn levels(s: &L2Snapshot) -> (Levels, Levels) {
    let f = |ls: &[marketdata::Level]| ls.iter().map(|l| (l.price.0, l.qty.0)).collect();
    (f(&s.bids), f(&s.asks))
}

/// A minimal WebSocket client for week 16's feed: handshake, then text
/// frames.
struct WsClient {
    stream: TcpStream,
    buf: Vec<u8>,
}

impl WsClient {
    fn connect(addr: SocketAddr) -> WsClient {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        write!(
            stream,
            "GET /feed HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
        .unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        let head = String::from_utf8(head).unwrap();
        assert!(head.starts_with("HTTP/1.1 101"), "{head}");
        assert!(head.contains(&ws::accept_key(key)));
        WsClient {
            stream,
            buf: Vec::new(),
        }
    }

    fn next_text(&mut self) -> String {
        loop {
            if let Some((f, used)) = ws::decode_frame(&self.buf, 1 << 24).unwrap() {
                self.buf.drain(..used);
                if f.opcode == ws::Opcode::Text {
                    return String::from_utf8(f.payload).unwrap();
                }
                continue;
            }
            let mut chunk = [0u8; 65536];
            let n = self.stream.read(&mut chunk).expect("feed went quiet");
            assert!(n > 0, "feed closed");
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// Reads messages until one satisfies `f`; returns how many it read.
    fn until(&mut self, mut f: impl FnMut(&str) -> bool) -> usize {
        let mut n = 0;
        loop {
            n += 1;
            if f(&self.next_text()) {
                return n;
            }
        }
    }
}

/// The `"seq":N` of a feed message.
fn json_seq(msg: &str) -> u64 {
    let at = msg.find("\"seq\":").expect("seq") + 6;
    msg[at..]
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn w24_order_ack_and_md_update_end_to_end() {
    let journals = MemJournals::new();
    let mut config = in_memory(&journals);
    config.feed.listen = Some("127.0.0.1:0".parse().unwrap());
    let ex = start(config);
    let feed = ex
        .feed(INSTRUMENT)
        .expect("instrument 1 has a feed")
        .clone();
    let (mut l2, mut tape) = (feed.l2.subscribe(), feed.trades.subscribe());
    let md = ex.md_addrs();
    assert_eq!(md.len(), 1);
    let mut browser = WsClient::connect(md[0].1);
    let first = browser.next_text();
    assert!(
        first.starts_with("{\"type\":\"snapshot\",\"seq\":0,\"bids\":[],\"asks\":[]"),
        "{first}"
    );

    // A resting sell: ack, then the level appears on every feed.
    let (mut a, _) = logged_on(&ex, 1);
    a.send(limit(1, Side::Sell, 101, 5)).unwrap();
    assert_eq!(a.recv_event().unwrap(), Event::Accepted { id: OrderId(1) });
    assert!(matches!(a.recv_event().unwrap(), Event::Rested { .. }));
    let level = L2Update {
        seq: Seq(1),
        side: Side::Sell,
        price: Price(101),
        qty: Qty(5),
    };
    assert_eq!(next(&mut l2), level);
    browser.until(|m| m.contains("\"side\":\"sell\",\"price\":101,\"qty\":5"));

    // A crossing buy from another session: both sides hear of the trade,
    // the tape prints it, the level shrinks.
    let (mut b, _) = logged_on(&ex, 2);
    b.send(limit(7, Side::Buy, 102, 3)).unwrap();
    assert_eq!(b.recv_event().unwrap(), Event::Accepted { id: OrderId(7) });
    let print = |taker, maker| Event::Trade {
        taker: OrderId(taker),
        maker: OrderId(maker),
        taker_side: Side::Buy,
        price: Price(101),
        qty: Qty(3),
    };
    assert_eq!(b.recv_event().unwrap(), print(7, 0));
    assert_eq!(a.recv_event().unwrap(), print(0, 1));
    assert_eq!(
        next(&mut tape),
        TradeTick {
            seq: Seq(1),
            taker_side: Side::Buy,
            price: Price(101),
            qty: Qty(3)
        }
    );
    assert_eq!(
        next(&mut l2),
        L2Update {
            seq: Seq(2),
            qty: Qty(2),
            ..level
        }
    );
    browser.until(|m| m.contains("\"price\":101,\"qty\":2"));

    // Both orders were journaled (and synced) before anyone heard of them,
    // and both carry the full set of stage stamps, in pipeline order.
    let storage = journals.storage(INSTRUMENT);
    assert_eq!(journaled(&storage.durable_bytes()).len(), 2);
    assert!(storage.syncs() >= 2);
    let samples = ex.stage_log().samples();
    assert_eq!(samples.len(), 2);
    let path = [
        Stamp::Ingress,
        Stamp::Gateway,
        Stamp::Risk,
        Stamp::Match,
        Stamp::Journal,
        Stamp::Publish,
        Stamp::Ack,
    ];
    for s in &samples {
        let stamps: Vec<_> = s.iter().collect();
        assert_eq!(stamps.iter().map(|x| x.0).collect::<Vec<_>>(), path);
        // One process-wide clock; one thread from `Gateway` on.
        assert!(
            stamps[1..].windows(2).all(|w| w[0].1 <= w[1].1),
            "{stamps:?}"
        );
        assert!(s.total() > 0 && s.total() < TIMEOUT.as_nanos() as u64);
    }
    let report = ex.shutdown(TIMEOUT);
    assert!(report.is_clean(), "{report:?}");
    // Shutdown disconnected `a`, whose remaining 2 were cancelled through
    // the journal (cancel on disconnect) and left the feed.
    assert_eq!(
        (
            report.pipeline.journaled,
            report.pipeline.trades,
            report.pipeline.md_updates
        ),
        (3, 1, 3)
    );
}

/// Pipelines `n` resting buys at distinct prices and checks, as each ack
/// and each L2 update arrives, how much of the journal is durable.
/// Returns (acks that arrived before their command was durable, stats).
fn acks_vs_durability(fsync: Fsync, n: u64) -> (u64, PipelineStats) {
    let journals = MemJournals::new();
    let mut config = in_memory(&journals);
    config.fsync = fsync;
    config.gateway.cancel_on_disconnect = false;
    let ex = start(config);
    let storage = journals.storage(INSTRUMENT);
    let mut l2 = ex.feed(INSTRUMENT).unwrap().l2.subscribe();
    let (mut c, _) = logged_on(&ex, 1);
    let orders: Vec<Message> = (1..=n)
        .map(|k| limit(k, Side::Buy, 100_000 - k as i64, 1))
        .collect();
    c.send_all(&orders).unwrap();
    // Order k is the k-th command, so its journal frame is the k-th; every
    // frame has the same length.
    let frame_len = journal::FRAME_HEADER_LEN
        + journal::encoded_len(&Command::New(NewOrder::limit(1, Side::Buy, 1, 1)));
    let mut early = 0;
    let mut acked = 0;
    while acked < n {
        match c.recv_event().unwrap() {
            Event::Accepted { id } => {
                acked += 1;
                assert_eq!(id.0, acked, "acks in order");
                let durable = storage.durable_bytes().len() / frame_len;
                early += (durable < id.0 as usize) as u64;
                // Market data is gated the same way: update u is order u's
                // new level.
                while let Recv::Item(u) = l2.try_recv() {
                    let durable = storage.durable_bytes().len() / frame_len;
                    if fsync == Fsync::Always {
                        assert!(
                            durable as u64 >= u.seq.0,
                            "update {} before its order was durable",
                            u.seq.0
                        );
                    }
                }
            }
            Event::Rested { .. } => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    if let Fsync::IntervalMs(_) = fsync {
        // The flow has stopped: the interval fsync must still happen, on
        // the idle engine thread, without another order to trigger it.
        eventually("the idle interval fsync", || {
            (storage.durable_bytes().len() == storage.bytes().len()).then_some(())
        });
    }
    let report = ex.shutdown(TIMEOUT);
    assert!(report.is_clean());
    // Whatever the policy, a clean shutdown leaves everything synced.
    assert_eq!(storage.durable_bytes(), storage.bytes());
    assert_eq!(journaled(&storage.bytes()).len() as u64, n);
    (early, report.pipeline)
}

#[test]
fn w24_no_ack_or_market_data_before_the_command_is_durable() {
    const N: u64 = 2_000;
    let (early, stats) = acks_vs_durability(Fsync::Always, N);
    assert_eq!(early, 0, "an ack arrived before its command was synced");
    // Group commit: one fsync per engine batch, never more than one per
    // order (pipelined orders share batches).
    assert!(
        stats.syncs >= 1 && stats.syncs <= stats.batches,
        "{stats:?}"
    );
    assert!(stats.syncs <= N, "{stats:?}");
    assert_eq!(stats.journaled, N);

    // The control: without fsync, acks run ahead of the disk — the check
    // above can fail.
    let (early, stats) = acks_vs_durability(Fsync::Never, N);
    assert_eq!(early, N, "nothing is synced before shutdown under `never`");
    assert_eq!(stats.syncs, 1, "only the final sync at shutdown");

    // In between: at most one fsync per interval — and one once the flow
    // stops (checked inside), so the tail is not left unsynced.
    let (_, stats) = acks_vs_durability(Fsync::IntervalMs(5), N);
    assert!(stats.syncs >= 1 && stats.syncs < stats.batches, "{stats:?}");
}

/// One random command for session-local client ids `1..next`.
fn random_command(rng: &mut Rng, next: &mut u64) -> Command {
    match rng.range(0, 9) {
        0..=5 => {
            *next += 1;
            let side = rng.side();
            let price = rng.range_i64(95, 105);
            let o = NewOrder::limit(*next, side, price, rng.range(1, 10));
            Command::New(if rng.chance(0.15) {
                o.with_tif(TimeInForce::Ioc)
            } else {
                o
            })
        }
        6 | 7 => Command::Cancel {
            id: OrderId(rng.range(1, (*next).max(1))),
        },
        _ => Command::Reduce {
            id: OrderId(rng.range(1, (*next).max(1))),
            qty: Qty(rng.range(0, 5)),
        },
    }
}

#[test]
fn w24_journal_replay_reproduces_the_engine_state() {
    const COMMANDS: usize = 3_000;
    let dir = TempDir::new("replay");
    let base = dir.0.join("journal.bin");
    let spec = InstrumentSpec::default();
    let config = || V2Config {
        gateway: GatewayConfig {
            cancel_on_disconnect: false,
            outbound_capacity: 1 << 16,
            ..GatewayConfig::default()
        },
        journal: JournalBackend::File(base.clone()),
        fsync: Fsync::Always,
        ..in_memory(&MemJournals::new())
    };

    // Live: one session, random flow; an in-process sequencer is the model
    // (same input order → same ids, reports and engine state).
    let ex = start(config());
    let mut model = Sequencer::new(&[EngineConfig::new(spec)], Hooks::default(), false);
    let (mut c, sid) = logged_on(&ex, 3);
    model.open(sid, AccountId(3), &mut |_, _| {});
    let mut rng = Rng::new(24);
    let mut next = 0;
    let mut expected = Vec::new();
    let mut msgs = Vec::new();
    for _ in 0..COMMANDS {
        let cmd = random_command(&mut rng, &mut next);
        let cmd = match cmd {
            Command::New(o) => Command::New(o.with_account(3)),
            other => other,
        };
        model.command(sid, 0, &cmd, &mut |s, m| {
            assert_eq!(s, sid);
            expected.push(m);
        });
        msgs.push(cmd.into());
    }
    for chunk in msgs.chunks(100) {
        c.send_all(chunk).unwrap();
    }
    let got: Vec<Message> = (0..expected.len())
        .map(|_| c.recv().unwrap().expect("report"))
        .collect();
    assert_eq!(got, expected, "live reports equal the model's");
    let report = ex.shutdown(TIMEOUT);
    assert!(report.is_clean());
    let live_hash = model.engine(INSTRUMENT).unwrap().state_hash();

    // Recovery straight from the file (engine::recover) equals the live
    // engine, command for command.
    let file = journal_path(&base, INSTRUMENT);
    assert_eq!(file, dir.0.join("journal.1.bin"));
    let (recovered, rec) = engine::recover(
        EngineConfig::new(spec),
        FileStorage::open(&file).unwrap(),
        None,
        JournalOptions::default(),
        SystemClock::new(),
    )
    .unwrap();
    assert_eq!(rec.replayed, model.sequenced());
    assert_eq!(recovered.engine().state_hash(), live_hash);
    drop(recovered);

    // Restarting exchange v2 on the same journal recovers the same state,
    // republishes the book and continues the exchange ids.
    let ex = start(config());
    let r = &ex.recovery()[0];
    assert_eq!((r.replayed, r.state_hash), (model.sequenced(), live_hash));
    let last_id = model.router().next_id().0 - 1;
    assert_eq!(r.max_order_id, OrderId(last_id));
    let model_book = {
        let bytes = std::fs::read(&file).unwrap();
        replay_truth(&bytes, EngineConfig::new(spec)).2
    };
    assert_eq!(
        levels(&ex.feed(INSTRUMENT).unwrap().snapshots.load()),
        levels(&model_book),
        "the restarted feed starts from the recovered book"
    );
    let best_bid = model_book.bids.first().expect("resting bids survive").price;
    let (mut c, _) = logged_on(&ex, 4);
    c.send(Command::New(
        NewOrder::limit(1, Side::Sell, best_bid.0, 1).with_tif(TimeInForce::Ioc),
    ))
    .unwrap();
    assert_eq!(c.recv_event().unwrap(), Event::Accepted { id: OrderId(1) });
    assert!(
        matches!(c.recv_event().unwrap(), Event::Trade { taker: OrderId(1), maker: OrderId(0), price, .. } if price == best_bid),
        "trades against an order restored from the journal"
    );
    assert!(ex.shutdown(TIMEOUT).is_clean());
    let commands = journaled(&std::fs::read(&file).unwrap());
    assert_eq!(commands.len() as u64, model.sequenced() + 1);
    assert!(
        matches!(commands.last(), Some(Command::New(o)) if o.id.0 == last_id + 1),
        "exchange ids continue after a restart"
    );
}

#[test]
fn w24_every_trade_reaches_the_tape_in_order() {
    const SESSIONS: usize = 3;
    const ORDERS: u64 = 1_500;
    let journals = MemJournals::new();
    let ex = start(in_memory(&journals));
    let feed = ex.feed(INSTRUMENT).unwrap().clone();
    let (mut l2, mut tape) = (feed.l2.subscribe_oldest(), feed.trades.subscribe_oldest());

    // Three sessions trade against each other concurrently, each waiting
    // only for its own acks.
    let threads: Vec<_> = (0..SESSIONS)
        .map(|s| {
            let (mut c, _) = logged_on(&ex, 10 + s as u32);
            std::thread::spawn(move || {
                let mut rng = Rng::new(100 + s as u64);
                let orders: Vec<Message> = (1..=ORDERS)
                    .map(|id| {
                        let side = rng.side();
                        let price = 100 + rng.range_i64(-3, 3);
                        limit(id, side, price, rng.range(1, 9))
                    })
                    .collect();
                for chunk in orders.chunks(50) {
                    c.send_all(chunk).unwrap();
                }
                let (mut acks, mut taker_prints) = (0, Vec::new());
                while acks < ORDERS {
                    match c.recv_event().unwrap() {
                        Event::Accepted { .. } => acks += 1,
                        Event::Trade {
                            taker,
                            taker_side,
                            price,
                            qty,
                            ..
                        } if taker.0 != 0 => taker_prints.push((taker_side, price, qty)),
                        _ => {}
                    }
                }
                (c, taker_prints)
            })
        })
        .collect();
    let mut clients = Vec::new();
    let mut prints = 0;
    for t in threads {
        let (c, p) = t.join().unwrap();
        prints += p.len();
        clients.push(c);
    }
    assert!(ex.shutdown(TIMEOUT).is_clean());
    drop(clients);

    // The journal is the truth: replay it and compare the tape and the
    // book a subscriber built from the L2 feed.
    let bytes = journals.storage(INSTRUMENT).bytes();
    let (_, truth_tape, truth_book) =
        replay_truth(&bytes, EngineConfig::new(InstrumentSpec::default()));
    assert!(
        truth_tape.len() > 500,
        "the flow trades: {}",
        truth_tape.len()
    );
    let mut seen = Vec::new();
    loop {
        match tape.try_recv() {
            Recv::Item(t) => seen.push(t),
            Recv::Empty => break,
            Recv::Lagged(n) => panic!("tape reader lagged by {n}"),
        }
    }
    assert_eq!(seen, truth_tape, "every trade, in execution order");
    // Every print the takers heard about made the tape (makers may still
    // have reports in flight when a taker's acks are all in).
    assert!(prints <= seen.len());

    let mut book = BookBuilder::new();
    book.apply_snapshot(&L2Snapshot::default());
    loop {
        match l2.try_recv() {
            Recv::Item(u) => assert_eq!(book.apply_update(&u), marketdata::UpdateOutcome::Applied),
            Recv::Empty => break,
            Recv::Lagged(n) => panic!("L2 reader lagged by {n}"),
        }
    }
    assert_eq!(levels(&book.snapshot()), levels(&truth_book));
}

#[test]
fn w24_a_slow_md_consumer_never_blocks_matching_or_acks() {
    const ORDERS: u64 = 5_000;
    let journals = MemJournals::new();
    let mut config = in_memory(&journals);
    config.feed.ring_capacity = 64;
    config.feed.snapshot_interval = 32;
    config.gateway.cancel_on_disconnect = false;
    config.feed.listen = Some("127.0.0.1:0".parse().unwrap());
    let ex = start(config);
    let feed = ex.feed(INSTRUMENT).unwrap().clone();
    // Two consumers that do not read while the market moves: an
    // in-process reader and a browser that has stopped draining its socket.
    let mut slow = feed.l2.subscribe();
    let mut browser = WsClient::connect(ex.md_addrs()[0].1);
    assert!(browser.next_text().contains("snapshot"));

    let (mut c, _) = logged_on(&ex, 1);
    let mut rng = Rng::new(16);
    let orders: Vec<Message> = (1..=ORDERS)
        .map(|id| limit(id, rng.side(), 90 + rng.range_i64(0, 20), rng.range(1, 5)))
        .collect();
    let t = Instant::now();
    for chunk in orders.chunks(250) {
        c.send_all(chunk).unwrap();
    }
    let mut acks = 0;
    while acks < ORDERS {
        if let Event::Accepted { .. } = c.recv_event().unwrap() {
            acks += 1;
        }
    }
    // Generous bound: what matters is that the run finishes at all while
    // nobody drains the market data.
    assert!(t.elapsed() < TIMEOUT, "acks took {:?}", t.elapsed());
    assert_eq!(ex.stats().sequenced, ORDERS);
    let published = feed.l2.published();
    assert!(published > 10 * 64, "{published} updates");

    // The slow reader finds out exactly how much it missed and resyncs
    // from the snapshot cell.
    let Recv::Lagged(missed) = slow.try_recv() else {
        panic!("a reader {published} updates behind a 64-slot ring must lag")
    };
    assert!(missed > 0 && missed < published);
    let mut book = BookBuilder::new();
    let snap = feed.snapshots.load();
    book.apply_snapshot(&snap);
    while let Recv::Item(u) = slow.try_recv() {
        book.apply_update(&u);
    }
    let last = book.last_seq().expect("synced from a snapshot");
    assert!(book.is_synced(), "snapshot + ring tail leave no gap");
    assert!(last.0 >= snap.seq.0);

    // The browser gets conflated batches (or a fresh snapshot) and catches
    // up to the live edge without having been sent every update.
    let messages = browser.until(|m| json_seq(m) >= published);
    assert!(
        (messages as u64) < published,
        "{messages} messages for {published} updates"
    );

    let report = ex.shutdown(TIMEOUT);
    assert!(report.is_clean());
    let (_, _, truth) = replay_truth(
        &journals.storage(INSTRUMENT).bytes(),
        EngineConfig::new(InstrumentSpec::default()),
    );
    // Market data of a batch is out before its acks: the resynced book is
    // the final one.
    assert_eq!(last.0, published);
    assert_eq!(levels(&book.snapshot()), levels(&truth));
}

#[test]
fn w24_shutdown_drains_and_syncs() {
    const SESSIONS: u32 = 3;
    const ORDERS: u64 = 1_500;
    let journals = MemJournals::new();
    let mut config = in_memory(&journals);
    // `never`: nothing is fsynced while running, so the sync seen below is
    // the one shutdown does.
    config.fsync = Fsync::Never;
    config.gateway.cancel_on_disconnect = false;
    let ex = start(config);
    let mut readers = Vec::new();
    for s in 0..SESSIONS {
        let (mut c, _) = logged_on(&ex, s + 1);
        let orders: Vec<Message> = (1..=ORDERS)
            .map(|id| limit(id, Side::Buy, 50 + s as i64, 1))
            .collect();
        c.send_all(&orders).unwrap();
        // Read everything up to the close, concurrently with shutdown.
        readers.push(std::thread::spawn(move || c.read_to_close().unwrap()));
    }
    // Every session's orders are in the gateway before shutdown begins.
    let deadline = Instant::now() + TIMEOUT;
    while ex.stats().commands_in < SESSIONS as u64 * ORDERS {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let storage = journals.storage(INSTRUMENT);
    let report = ex.shutdown(TIMEOUT);
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(report.gateway.sequenced, SESSIONS as u64 * ORDERS);
    for r in readers {
        let msgs = r.join().unwrap();
        let acks = msgs
            .iter()
            .filter(|m| matches!(m, Message::Event(Event::Accepted { .. })))
            .count();
        assert_eq!(acks as u64, ORDERS, "every order answered before the close");
    }
    let bytes = storage.bytes();
    assert_eq!(journaled(&bytes).len() as u64, report.gateway.sequenced);
    assert_eq!(report.pipeline.journaled, report.gateway.sequenced);
    assert_eq!(
        storage.durable_bytes(),
        bytes,
        "shutdown synced the journal"
    );
    assert_eq!(report.pipeline.syncs, 1);
}

/// A pre-trade check that blocks while `stalled` is set — an engine thread
/// stuck on one order (a slow disk, a huge sweep) — and says so in `stuck`.
struct Stall {
    stalled: Arc<AtomicBool>,
    stuck: Arc<AtomicBool>,
}

impl PreTrade for Stall {
    fn check(
        &mut self,
        _: SessionId,
        _: &NewOrder,
        _: u64,
        _: Option<Price>,
    ) -> Result<(), types::RejectReason> {
        while self.stalled.load(Ordering::Acquire) {
            self.stuck.store(true, Ordering::Release);
            std::thread::sleep(Duration::from_micros(100));
        }
        self.stuck.store(false, Ordering::Release);
        Ok(())
    }
}

#[test]
fn w24_busy_rejects_at_the_edge_while_the_engine_is_stalled() {
    const CAPACITY: usize = 16;
    const BURST: u64 = 300;
    let stalled = Arc::new(AtomicBool::new(false));
    let stuck = Arc::new(AtomicBool::new(false));
    let mut config = in_memory(&MemJournals::new());
    config.gateway = GatewayConfig {
        busy_reject: true,
        inbound_capacity: CAPACITY,
        engine_idle: EngineIdle::BusyPoll,
        ..config.gateway
    };
    let gw = ExchangeV2::start(
        config,
        Stall {
            stalled: Arc::clone(&stalled),
            stuck: Arc::clone(&stuck),
        },
    )
    .unwrap();
    let mut c = Client::connect(gw.local_addr()).unwrap();
    c.logon(1, 0).unwrap();
    c.send(limit(1, Side::Buy, 90, 1)).unwrap();
    assert_eq!(c.recv_event().unwrap(), Event::Accepted { id: OrderId(1) });
    c.recv_event().unwrap(); // Rested

    // Stall the engine thread inside a batch.
    stalled.store(true, Ordering::Release);
    c.send(limit(2, Side::Buy, 90, 1)).unwrap();
    let deadline = Instant::now() + TIMEOUT;
    while !stuck.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline, "the engine never stalled");
        std::thread::sleep(Duration::from_millis(1));
    }
    // A burst: the queue takes what fits, the edge answers the rest at
    // once, while the engine is still stuck.
    let burst: Vec<Message> = (3..3 + BURST)
        .map(|id| limit(id, Side::Buy, 90, 1))
        .collect();
    c.send_all(&burst).unwrap();
    let mut busy = Vec::new();
    while gw.stats().busy_rejects + (CAPACITY as u64) < BURST {
        assert!(
            Instant::now() < deadline,
            "with `busy_reject`, a session must answer a new order that finds the engine \
             queue full with `Rejected {{ BUSY }}` at once: {:?}",
            gw.stats()
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let refused = gw.stats().busy_rejects;
    assert!(refused >= BURST - CAPACITY as u64, "{refused}");
    for _ in 0..refused {
        match c.recv_event().unwrap() {
            Event::Rejected { id, reason } => {
                assert_eq!(reason, BUSY);
                busy.push(id.0);
            }
            other => panic!("only busy rejects while stalled, got {other:?}"),
        }
    }
    assert!(
        stalled.load(Ordering::Acquire),
        "all of that happened during the stall"
    );
    // A cancel is never refused for load: it waits for room.
    c.cancel(1).unwrap();

    // Release: everything that was queued is matched and acknowledged,
    // the cancel too; nothing else was dropped.
    stalled.store(false, Ordering::Release);
    let queued = BURST - refused;
    let mut accepted = Vec::new();
    let mut cancelled = false;
    while accepted.len() < 1 + queued as usize || !cancelled {
        match c.recv_event().unwrap() {
            Event::Accepted { id } => accepted.push(id.0),
            Event::Cancelled { id, .. } => {
                assert_eq!(id, OrderId(1));
                cancelled = true;
            }
            Event::Rested { .. } => {}
            Event::Rejected { reason, .. } => assert_eq!(reason, BUSY, "a late busy reject"),
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(accepted[0], 2);
    let mut all: Vec<u64> = accepted.iter().chain(&busy).copied().collect();
    all.sort_unstable();
    assert_eq!(
        all,
        (2..3 + BURST).collect::<Vec<_>>(),
        "each order answered once"
    );
    assert!(accepted.len() <= 1 + CAPACITY, "queue depth stayed bounded");

    // Fully recovered: bursts that fit the queue are accepted again.
    for burst in 0..5u64 {
        let after: Vec<Message> = (0..10)
            .map(|k| limit(1000 + 10 * burst + k, Side::Buy, 90, 1))
            .collect();
        c.send_all(&after).unwrap();
        let mut ok = 0;
        while ok < 10 {
            match c.recv_event().unwrap() {
                Event::Accepted { .. } => ok += 1,
                Event::Rested { .. } => {}
                other => panic!("unexpected {other:?}"),
            }
        }
    }
    assert_eq!(gw.stats().busy_rejects, refused);
    assert!(gw.shutdown(TIMEOUT).is_clean());
}

#[test]
fn w24_load_test_reports_stages_and_end_to_end() {
    let journals = MemJournals::new();
    let mut config = in_memory(&journals);
    config.gateway.busy_reject = true;
    let ex = start(config);
    let load = LoadConfig {
        warmup: 1_000,
        ..LoadConfig::new(20_000, 6_000)
    };
    let r = loadgen::run(ex.local_addr(), &load).unwrap();
    assert_eq!(r.unanswered, 0, "{}", r.summary());
    assert!(r.orders > 3_000, "{}", r.summary());
    assert_eq!(r.accepted + r.rejected + r.busy, r.orders);
    assert_eq!(r.e2e.count(), r.accepted + r.rejected);
    assert_eq!(r.sent, load.commands);
    assert!(r.e2e.percentile(0.5) > 0 && r.e2e.percentile(0.5) <= r.e2e.percentile(0.99));

    // The server's view of the same orders: every stage, every sample.
    let samples = ex.stage_log().samples_since(r.measure_start_ns);
    let measured = r.accepted + r.rejected;
    assert!(
        samples.len() as u64 >= measured * 9 / 10 && samples.len() as u64 <= measured + 100,
        "{} samples for {measured} orders",
        samples.len()
    );
    // Every sample carries every stage (week 21's stamps; week 39 turns
    // them into attribution), and the server's end to end is part of the
    // client's.
    let mut server_e2e = telemetry::Histogram::new();
    for s in &samples {
        for stage in [
            Stamp::Gateway,
            Stamp::Risk,
            Stamp::Match,
            Stamp::Journal,
            Stamp::Publish,
            Stamp::Ack,
        ] {
            assert!(s.interval(stage).is_some(), "{stage:?} missing");
        }
        server_e2e.record(s.total());
    }
    assert!(server_e2e.percentile(0.5) <= r.e2e.percentile(0.5));
    assert_eq!(ex.stats().busy_rejects, r.busy);
    assert!(ex.shutdown(TIMEOUT).is_clean());
}
