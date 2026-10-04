//! Week 47: the demo exchange as a deployable service — routes, the feed,
//! health, metrics, client limits, shutdown, and the binary's bounded run.

mod common;

use common::{local_config, wait_for, FeedBook, Json};
use demo::client::{http_get, http_request, WsClient};
use demo::{Config, Demo};
use marketdata::L2Publisher;
use sim::LiveSession;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(20);

fn ready(demo: &Demo) {
    wait_for(T, "/health to answer 200", || {
        http_get(demo.http_addr(), "/health")
            .ok()
            .filter(|r| r.status == 200)
    });
}

/// The book an offline replay of the same seed has after feed update `seq`.
fn reference_book(seed: u64, seq: u64) -> FeedBook {
    let mut session = LiveSession::new(seed);
    let mut publisher = L2Publisher::new();
    'outer: while publisher.last_seq().0 < seq {
        session.step();
        for ev in session.last_events() {
            publisher.on_event(ev);
            if publisher.last_seq().0 == seq {
                break 'outer;
            }
        }
    }
    let snap = publisher.snapshot();
    let side = |levels: &[marketdata::Level]| -> BTreeMap<i64, u64> {
        levels.iter().map(|l| (l.price.0, l.qty.0)).collect()
    };
    FeedBook {
        seq,
        bids: side(&snap.bids),
        asks: side(&snap.asks),
    }
}

#[test]
fn w47_http_routes_and_content_types() {
    let demo = Demo::start(local_config(2_000, 42)).unwrap();
    assert_ne!(
        demo.http_addr().port(),
        0,
        "ports are bound before start returns"
    );
    assert_ne!(demo.ws_addr().port(), 0);
    ready(&demo);

    let page = http_get(demo.http_addr(), "/").unwrap();
    assert_eq!(page.status, 200);
    assert_eq!(
        page.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    let html = page.text();
    assert!(html.starts_with("<!doctype html>"));
    assert!(
        html.contains(&format!("\"wsPort\":{}", demo.ws_addr().port())),
        "the page knows the feed port"
    );
    assert!(html.contains("new WebSocket(") && !html.contains("__DEMO_CONFIG__"));
    assert!(
        !html.contains("src=\"http") && !html.contains("href=\"http"),
        "no external assets"
    );
    assert_eq!(
        page.header("content-length"),
        Some(page.body.len().to_string().as_str())
    );

    let health = http_get(demo.http_addr(), "/health").unwrap();
    assert_eq!(health.header("content-type"), Some("application/json"));
    let h = Json::parse(&health.text());
    assert_eq!(h.get("status").str(), "ok");
    assert_eq!(h.get("ready"), &Json::Bool(true));

    let stats = http_get(demo.http_addr(), "/stats").unwrap();
    assert_eq!(
        (stats.status, stats.header("content-type")),
        (200, Some("application/json"))
    );
    let s = Json::parse(&stats.text());
    assert_eq!((s.get("rate").u64(), s.get("seed").u64()), (2_000, 42));
    for key in [
        "commands",
        "trades",
        "feed_seq",
        "ws_clients",
        "best_bid",
        "best_ask",
        "last_trades",
    ] {
        assert!(s.has(key), "/stats has `{key}`: {}", stats.text());
    }

    let metrics = http_get(demo.http_addr(), "/metrics").unwrap();
    assert_eq!(metrics.status, 200);
    assert_eq!(
        metrics.header("content-type"),
        Some("text/plain; version=0.0.4; charset=utf-8")
    );

    assert_eq!(http_get(demo.http_addr(), "/nope").unwrap().status, 404);
    let post = http_request(demo.http_addr(), "POST", "/stats", &[]).unwrap();
    assert_eq!((post.status, post.header("allow")), (405, Some("GET")));
    assert_eq!(
        http_get(demo.http_addr(), "/shutdown").unwrap().status,
        404,
        "no token configured: no route"
    );
    let mut raw = TcpStream::connect(demo.http_addr()).unwrap();
    raw.write_all(b"garbage\r\n\r\n").unwrap();
    let mut answer = String::new();
    raw.read_to_string(&mut answer).unwrap();
    assert!(answer.starts_with("HTTP/1.1 400"), "{answer}");
    assert!(!demo.is_stopping());
    demo.stop();
}

#[test]
fn w47_health_reports_readiness_and_seq_advances() {
    let demo = Demo::start(local_config(5_000, 1)).unwrap();
    ready(&demo);
    let read = || {
        let r = http_get(demo.http_addr(), "/health").unwrap();
        assert_eq!(r.status, 200, "{}", r.text());
        let j = Json::parse(&r.text());
        (
            j.get("seq").u64(),
            j.get("uptime_ms").u64(),
            j.get("commands").u64(),
        )
    };
    let (seq0, up0, cmd0) = read();
    assert!(seq0 > 0, "ready means the feed has published");
    let (seq1, up1, cmd1) = wait_for(T, "the feed seq to advance", || {
        let now = read();
        (now.0 > seq0).then_some(now)
    });
    assert!(seq1 > seq0 && up1 >= up0 && cmd1 > cmd0);

    // The rate is a schedule: ~5000 commands/s, nowhere near flat out.
    std::thread::sleep(Duration::from_millis(300));
    let s = demo.stats();
    let cps = s.commands_per_sec();
    assert!(
        cps > 500.0 && cps < 50_000.0,
        "{cps} commands/s at --rate 5000"
    );
    assert!(s.trades > 0 && s.best_bid.is_some() && s.best_ask.is_some());
    assert!(
        s.best_bid.unwrap().price < s.best_ask.unwrap().price,
        "the book is never crossed"
    );

    demo.shutdown();
    let r = http_get(demo.http_addr(), "/health");
    if let Ok(r) = r {
        assert_eq!(r.status, 503, "a stopping demo is not ready: {}", r.text());
        assert_eq!(Json::parse(&r.text()).get("status").str(), "stopping");
    }
    let last = demo.wait();
    assert!(last.stopping && !last.ready && last.feed_seq >= seq1);
}

#[test]
fn w47_ws_snapshot_then_updates_rebuild_the_seeded_book() {
    let seed = 2024;
    let demo = Demo::start(local_config(20_000, seed)).unwrap();
    let mut ws = WsClient::connect(demo.ws_addr()).unwrap();
    let mut book = FeedBook::default();
    let first = Json::parse(&ws.recv().unwrap());
    assert_eq!(
        book.apply(&first),
        "snapshot",
        "the first message is a snapshot"
    );

    let mut batches = 0;
    let deadline = Instant::now() + Duration::from_secs(1);
    while batches < 20 || Instant::now() < deadline {
        let msg = Json::parse(&ws.recv().unwrap());
        if book.apply(&msg) == "updates" {
            batches += 1;
        }
        if let (Some(bid), Some(ask)) = (book.best_bid(), book.best_ask()) {
            assert!(
                bid < ask,
                "crossed book at feed seq {}: {bid} >= {ask}",
                book.seq
            );
        }
        assert!(batches < 1_000_000);
    }
    assert!(book.seq > first.get("seq").u64());

    // Same seed, same market: the feed is exactly the engine's book.
    assert_eq!(book, reference_book(seed, book.seq));
    ws.close().unwrap();
    demo.stop();
}

/// Checks the Prometheus text exposition format (0.0.4) line by line and
/// returns sample name → value.
fn parse_prometheus(text: &str) -> BTreeMap<String, f64> {
    let mut types: BTreeMap<String, String> = BTreeMap::new();
    let mut samples = BTreeMap::new();
    let valid_name = |n: &str| {
        !n.is_empty()
            && !n.starts_with(|c: char| c.is_ascii_digit())
            && n.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
    };
    assert!(text.ends_with('\n'), "exposition ends with a newline");
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# ") {
            let mut parts = rest.splitn(3, ' ');
            let (kind, name) = (parts.next().unwrap(), parts.next().unwrap_or(""));
            assert!(valid_name(name), "bad metric name in `{line}`");
            if kind == "TYPE" {
                let t = parts.next().unwrap_or("");
                assert!(
                    matches!(t, "counter" | "gauge" | "histogram" | "summary" | "untyped"),
                    "`{line}`"
                );
                assert!(
                    types.insert(name.to_string(), t.to_string()).is_none(),
                    "TYPE twice for {name}"
                );
            } else {
                assert_eq!(kind, "HELP", "`{line}`");
            }
            continue;
        }
        assert!(
            !line.is_empty() && !line.starts_with('#'),
            "unexpected line `{line}`"
        );
        let (series, value) = line
            .rsplit_once(' ')
            .unwrap_or_else(|| panic!("no value in `{line}`"));
        let value: f64 = value
            .parse()
            .unwrap_or_else(|_| panic!("bad value in `{line}`"));
        let name = series.split('{').next().unwrap();
        assert!(valid_name(name), "bad sample name in `{line}`");
        if let Some(labels) = series.strip_prefix(name) {
            if !labels.is_empty() {
                assert!(
                    labels.starts_with('{') && labels.ends_with('}'),
                    "bad labels in `{line}`"
                );
                for pair in labels[1..labels.len() - 1].split("\",") {
                    let (k, v) = pair
                        .split_once("=\"")
                        .unwrap_or_else(|| panic!("bad label in `{line}`"));
                    assert!(valid_name(k) && !v.contains('\n'), "bad label in `{line}`");
                }
            }
        }
        let family = ["_bucket", "_sum", "_count"]
            .iter()
            .find_map(|s| {
                name.strip_suffix(s)
                    .filter(|f| types.get(*f).is_some_and(|t| t == "histogram"))
            })
            .unwrap_or(name);
        assert!(
            types.contains_key(family),
            "sample `{name}` before its TYPE line"
        );
        samples.insert(series.to_string(), value);
    }
    samples
}

#[test]
fn w47_metrics_parse_as_prometheus_text() {
    let demo = Demo::start(local_config(10_000, 9)).unwrap();
    ready(&demo);
    let ws = WsClient::connect(demo.ws_addr()).unwrap();
    wait_for(T, "the client gauge", || {
        (demo.stats().ws_clients == 1).then_some(())
    });
    std::thread::sleep(Duration::from_millis(100));

    let text = http_get(demo.http_addr(), "/metrics").unwrap().text();
    let m = parse_prometheus(&text);
    let get = |k: &str| {
        *m.get(k)
            .unwrap_or_else(|| panic!("missing `{k}` in\n{text}"))
    };
    assert!(get("hft_demo_commands_total") > 0.0);
    assert!(get("hft_demo_trades_total") > 0.0);
    assert!(get("hft_demo_l2_updates_total") > 0.0);
    assert_eq!(get("hft_demo_ws_clients"), 1.0);
    assert_eq!(get("hft_demo_ws_connections_total"), 1.0);
    assert_eq!(get("hft_demo_ready"), 1.0);
    assert!(get("hft_demo_feed_seq") > 0.0);
    assert!(get("hft_demo_uptime_seconds") > 0.0);
    assert!(text.contains("# TYPE hft_demo_publish_latency_ns histogram\n"));
    assert!(text.contains("# TYPE hft_demo_ws_clients gauge\n"));

    // The latency histogram: cumulative buckets, +Inf == count == commands.
    let buckets: Vec<f64> = text
        .lines()
        .filter(|l| l.starts_with("hft_demo_publish_latency_ns_bucket{"))
        .map(|l| l.rsplit_once(' ').unwrap().1.parse().unwrap())
        .collect();
    assert!(
        buckets.len() >= 2 && buckets.windows(2).all(|w| w[0] <= w[1]),
        "{buckets:?}"
    );
    assert!(text.contains("hft_demo_publish_latency_ns_bucket{le=\"+Inf\"}"));
    let count = get("hft_demo_publish_latency_ns_count");
    assert_eq!(*buckets.last().unwrap(), count);
    assert!(count > 0.0);
    assert!(get("hft_demo_publish_latency_ns_sum") > 0.0);
    // Requests are counted by route, with bounded labels.
    assert!(get("hft_demo_http_requests_total{route=\"/health\"}") >= 1.0);

    drop(ws);
    wait_for(T, "the gauge to drop", || {
        (demo.stats().ws_clients == 0).then_some(())
    });
    demo.stop();
}

#[test]
fn w47_feed_caps_clients_and_never_waits_for_slow_ones() {
    let config = Config {
        max_ws_clients: 3,
        ..local_config(200_000, 5)
    };
    let demo = Demo::start(config).unwrap();
    ready(&demo);

    // A client that completes the handshake and then never reads.
    let mut stalled = TcpStream::connect(demo.ws_addr()).unwrap();
    stalled
        .write_all(b"GET / HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n")
        .unwrap();
    let mut a = WsClient::connect(demo.ws_addr()).unwrap();
    let b = WsClient::connect(demo.ws_addr()).unwrap();
    wait_for(T, "three clients", || {
        (demo.stats().ws_clients == 3).then_some(())
    });

    let refused = WsClient::connect(demo.ws_addr())
        .err()
        .expect("a fourth client is refused");
    assert!(refused.to_string().contains("503"), "{refused}");
    assert_eq!(demo.stats().ws_rejected, 1);

    // Seconds of updates pile up behind the stalled client; the engine
    // keeps its pace and the reading clients keep receiving.
    let before = demo.stats();
    std::thread::sleep(Duration::from_millis(1_500));
    let after = demo.stats();
    assert!(
        after.commands - before.commands > 50_000,
        "engine stalled: {} commands in 1.5 s",
        after.commands - before.commands
    );
    assert!(after.feed_seq > before.feed_seq);
    let mut book = FeedBook::default();
    for _ in 0..3 {
        book.apply(&Json::parse(&a.recv().unwrap()));
    }
    assert!(book.seq > 0);

    drop(b);
    drop(stalled);
    wait_for(T, "slots to free up", || {
        (demo.stats().ws_clients <= 1).then_some(())
    });
    let c = WsClient::connect(demo.ws_addr()).expect("a freed slot takes a new client");
    let stats = demo.stop();
    assert_eq!((stats.ws_connections, stats.ws_rejected), (4, 1));
    drop(c);
}

#[test]
fn w47_shutdown_route_needs_the_token_and_drains() {
    let token = "s3cret-token-for-tests";
    let config = Config {
        shutdown_token: Some(token.into()),
        ..local_config(2_000, 3)
    };
    let demo = Demo::start(config).unwrap();
    ready(&demo);
    let mut ws = WsClient::connect(demo.ws_addr()).unwrap();
    ws.recv().unwrap();

    let post = |auth: Option<&str>| {
        let header = auth.map(|t| format!("Bearer {t}"));
        let headers: Vec<(&str, &str)> = header
            .iter()
            .map(|h| ("Authorization", h.as_str()))
            .collect();
        http_request(demo.http_addr(), "POST", "/shutdown", &headers)
            .unwrap()
            .status
    };
    assert_eq!(post(None), 403);
    assert_eq!(post(Some("s3cret-token-for-test")), 403);
    assert_eq!(http_get(demo.http_addr(), "/shutdown").unwrap().status, 405);
    assert!(!demo.is_stopping());
    assert_eq!(post(Some(token)), 202);
    assert!(demo.is_stopping());

    // wait() returns by itself (the route set the flag) within a deadline.
    let http = demo.http_addr();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(demo.wait()).unwrap());
    let stats = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("drained within 10 s");
    assert!(stats.stopping && stats.commands > 0);
    assert_eq!(stats.ws_clients, 0, "every feed client was closed");

    // The feed connection is closed and the HTTP port no longer serves.
    ws.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let ended = loop {
        match ws.recv() {
            Ok(_) => continue,
            Err(e) => break e,
        }
    };
    assert!(
        !matches!(
            ended.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ),
        "{ended}"
    );
    assert!(http_get(http, "/health").is_err());
}

#[test]
fn w47_binary_runs_for_a_bounded_duration() {
    let bin = env!("CARGO_BIN_EXE_demo");
    let bad = Command::new(bin).args(["--rate", "fast"]).output().unwrap();
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("invalid --rate `fast`"));

    let started = Instant::now();
    let mut child = Command::new(bin)
        .args([
            "--duration",
            "2s",
            "--http-addr",
            "127.0.0.1:0",
            "--ws-addr",
            "127.0.0.1:0",
        ])
        .env("HFT_DEMO_RATE", "5000")
        .env("HFT_DEMO_SEED", "7")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let first = lines.next().and_then(Result::ok).unwrap_or_default();
    let mut stderr = String::new();
    if !first.starts_with("http listening on http://") {
        let _ = child.wait();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        panic!("demo did not start: `{first}`\n{stderr}");
    }
    let http: std::net::SocketAddr = first
        .trim_start_matches("http listening on http://")
        .parse()
        .unwrap();
    assert!(lines
        .next()
        .unwrap()
        .unwrap()
        .starts_with("ws listening on ws://"));
    wait_for(T, "the binary's /health", || {
        http_get(http, "/health").ok().filter(|r| r.status == 200)
    });
    assert_eq!(
        demo::client::http_get(http, "/stats")
            .map(|r| Json::parse(&r.text()).get("seed").u64())
            .unwrap(),
        7
    );

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(child.wait_with_output()).unwrap());
    let out = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the demo exits by itself")
        .unwrap();
    let elapsed = started.elapsed();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        elapsed >= Duration::from_secs(2),
        "stopped early: {elapsed:?}"
    );
    let last = lines.map_while(Result::ok).last().unwrap_or_default();
    let summary = Json::parse(
        last.strip_prefix("stopped: ")
            .unwrap_or_else(|| panic!("no summary: `{last}`")),
    );
    assert_eq!(
        (summary.get("rate").u64(), summary.get("seed").u64()),
        (5_000, 7)
    );
    let commands = summary.get("commands").u64();
    assert!(
        (2_000..=40_000).contains(&commands),
        "{commands} commands in ~2 s at 5000/s"
    );
}
