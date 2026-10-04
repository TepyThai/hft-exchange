//! Load the demo: N WebSocket clients on loopback for a few seconds, then
//! report the engine's sustained rate and what each client received.
//!
//! ```sh
//! cargo run --release -p demo --example fanout -- --clients 50 --secs 10 --rate 0
//! ```
//!
//! `--rate 0` runs the engine flat out. Demo numbers on whatever machine
//! runs it: compare runs on the same machine only.

use demo::client::WsClient;
use demo::{Config, Demo};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Default)]
struct ClientReport {
    messages: u64,
    snapshots: u64,
    updates: u64,
    bytes: u64,
    last_seq: u64,
}

fn arg(args: &[String], name: &str, default: u64) -> u64 {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(|v| {
            v.parse()
                .unwrap_or_else(|_| panic!("{name} needs a number"))
        })
        .unwrap_or(default)
}

/// `"seq":N` right after the message type.
fn message_seq(msg: &str) -> u64 {
    let at = msg.find("\"seq\":").map(|i| i + 6).unwrap_or(0);
    msg[at..]
        .bytes()
        .take_while(u8::is_ascii_digit)
        .fold(0, |n, d| n * 10 + u64::from(d - b'0'))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let clients = arg(&args, "--clients", 50) as usize;
    let secs = arg(&args, "--secs", 10);
    let rate = arg(&args, "--rate", 0);
    let config = Config {
        http_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        rate,
        max_ws_clients: clients.max(1),
        ..Config::default()
    };
    let demo = Demo::start(config).expect("start the demo");
    let ws = demo.ws_addr();
    let stop = Arc::new(AtomicBool::new(false));
    let threads: Vec<_> = (0..clients)
        .map(|_| {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut c = WsClient::connect(ws).expect("connect");
                c.set_read_timeout(Some(Duration::from_millis(100)))
                    .unwrap();
                let mut r = ClientReport::default();
                while !stop.load(Ordering::Relaxed) {
                    let Ok(msg) = c.recv() else { continue };
                    r.messages += 1;
                    r.bytes += msg.len() as u64;
                    r.last_seq = message_seq(&msg);
                    if msg.starts_with("{\"type\":\"snapshot\"") {
                        r.snapshots += 1;
                    } else {
                        r.updates += msg.matches("\"side\"").count() as u64;
                    }
                }
                r
            })
        })
        .collect();

    // Let every client connect and get its first snapshot.
    while demo.stats().ws_clients < clients as u64 {
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(500));
    let (t0, s0) = (Instant::now(), demo.stats());
    std::thread::sleep(Duration::from_secs(secs));
    let (elapsed, s1) = (t0.elapsed().as_secs_f64(), demo.stats());
    stop.store(true, Ordering::Relaxed);
    let reports: Vec<ClientReport> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    let snapshot = demo.registry().collect();
    let end = demo.stop();

    let cps = (s1.commands - s0.commands) as f64 / elapsed;
    let ups = (s1.feed_seq - s0.feed_seq) as f64 / elapsed;
    println!(
        "clients {clients}, rate {}, {secs} s",
        if rate == 0 {
            "unthrottled".into()
        } else {
            rate.to_string()
        }
    );
    println!(
        "engine: {cps:.0} commands/s, {ups:.0} L2 updates/s published, {} trades total",
        end.trades
    );
    if let Some(h) = snapshot.histogram("hft_demo_publish_latency_ns", &[]) {
        println!(
            "publish latency per command (engine + L2 + ring): p50 {} ns, p99 {} ns, p99.9 {} ns over {} commands",
            h.percentile(0.5),
            h.percentile(0.99),
            h.percentile(0.999),
            h.count()
        );
    }
    if reports.is_empty() {
        return;
    }
    let total_secs = elapsed + 0.5; // clients also read during the warm-up
    let mut per_client: Vec<f64> = reports
        .iter()
        .map(|r| r.updates as f64 / total_secs)
        .collect();
    per_client.sort_by(f64::total_cmp);
    let msgs: u64 = reports.iter().map(|r| r.messages).sum();
    let bytes: u64 = reports.iter().map(|r| r.bytes).sum();
    let resyncs: u64 = reports.iter().map(|r| r.snapshots.saturating_sub(1)).sum();
    let mut lag: Vec<u64> = reports
        .iter()
        .map(|r| s1.feed_seq.saturating_sub(r.last_seq))
        .collect();
    lag.sort_unstable();
    println!(
        "fan-out: {:.0} messages/s and {:.1} MB/s across all clients; per client {:.0} (min) / {:.0} (median) updates/s delivered after conflation",
        msgs as f64 / total_secs,
        bytes as f64 / total_secs / 1e6,
        per_client[0],
        per_client[per_client.len() / 2],
    );
    println!(
        "resyncs (snapshots after the first): {resyncs}; final lag behind the feed: median {} / max {} updates",
        lag[lag.len() / 2],
        lag[lag.len() - 1]
    );
}
