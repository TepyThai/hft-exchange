//! Benchmarks for the `gateway` crate. Run with `cargo xtask bench --week N`.

use benchkit::{Histogram, Report, Unit};
use gateway::client::Client;
use gateway::io::{echo_loop, EchoLoad, EchoOptions, EchoStats, IoBackend, RingBackend};
use gateway::pingpong::{EchoServer, PingClient, RecvMode};
use gateway::SocketTuning;
use gateway::{EngineIdle, Gateway, GatewayConfig};
use std::time::{Duration, Instant};
use types::{Event, NewOrder, OrderId, Side, TimeInForce};

fn main() {
    benchkit::run(&[
        (
            "w13.roundtrip",
            "Client order → ack over loopback TCP",
            bench_roundtrip,
        ),
        (
            "w31.tcp_pingpong",
            "TCP ping-pong, loopback",
            bench_pingpong,
        ),
        (
            "w32.batched_io",
            "Messages per wakeup-batched loop",
            bench_batched_io,
        ),
    ]);
}

/// One logged-on client sends an IOC order into an empty book and waits
/// for its `Accepted` (timed), then reads the `Expired` that follows
/// (untimed), 20 000 times after 2 000 warm-up round trips. The engine
/// thread busy-polls; one Tokio worker serves the session.
fn bench_roundtrip() -> Report {
    const WARMUP: u64 = 2_000;
    const ROUNDS: u64 = 20_000;
    let gw = Gateway::start(GatewayConfig {
        io_threads: 1,
        engine_idle: EngineIdle::BusyPoll,
        ..GatewayConfig::default()
    })
    .expect("gateway starts");
    let mut client = Client::connect(gw.local_addr()).expect("connect");
    client.logon(1, 0).expect("logon");
    let mut hist = Histogram::new();
    for i in 0..WARMUP + ROUNDS {
        let order = NewOrder::limit(i + 1, Side::Buy, 100, 1).with_tif(TimeInForce::Ioc);
        let t = Instant::now();
        client.send_order(&order).expect("send");
        let ack = client.recv_event().expect("ack");
        let rtt = t.elapsed();
        assert_eq!(ack, Event::Accepted { id: OrderId(i + 1) });
        assert!(matches!(
            client.recv_event().expect("expired"),
            Event::Expired { .. }
        ));
        if i >= WARMUP {
            hist.record(rtt.as_nanos() as u64);
        }
    }
    drop(client);
    gw.shutdown(Duration::from_secs(5));
    hist.report(
        "w13.roundtrip",
        "Client order → ack over loopback TCP",
        Unit::Us,
    )
}

fn pingpong_hist(server: EchoServer, mode: RecvMode, rounds: u64) -> Histogram {
    let mut client =
        PingClient::connect(server.addr(), mode, SocketTuning::latency()).expect("connect");
    let payload = [7u8; 40];
    let mut echo = Vec::new();
    let mut hist = Histogram::new();
    for i in 0..rounds + rounds / 10 {
        let t = Instant::now();
        client.ping(&payload, &mut echo).expect("ping");
        let rtt = t.elapsed();
        if i >= rounds / 10 {
            hist.record(rtt.as_nanos() as u64);
        }
    }
    drop(client);
    server.join().expect("echo server");
    hist
}

/// A 40-byte frame echoed at the framing layer over loopback TCP, 20 000
/// round trips after 2 000 warm-up ones, with the gateway's socket tuning
/// (`TCP_NODELAY`, modest buffers). The reported value is the plain-thread
/// server with blocking receives on both ends (std, faster than the Tokio
/// server on the reference machine); the busy-polling and Tokio variants
/// are printed for comparison.
fn bench_pingpong() -> Report {
    const ROUNDS: u64 = 20_000;
    let t = SocketTuning::latency();
    let reported = pingpong_hist(
        EchoServer::spawn(RecvMode::Blocking, t, None).expect("server"),
        RecvMode::Blocking,
        ROUNDS,
    );
    let others = [
        ("std busy-poll", RecvMode::BusyPoll, false),
        ("tokio server, blocking client", RecvMode::Blocking, true),
    ];
    let us = |h: &Histogram, q: f64| h.quantile(q) as f64 / 1e3;
    eprintln!(
        "std blocking (reported): p50 {:.1} us, p99 {:.1} us, p99.9 {:.1} us",
        us(&reported, 0.5),
        us(&reported, 0.99),
        us(&reported, 0.999)
    );
    for (name, mode, tokio) in others {
        let server = if tokio {
            EchoServer::spawn_tokio(t, None)
        } else {
            EchoServer::spawn(mode, t, None)
        };
        let h = pingpong_hist(server.expect("server"), mode, ROUNDS);
        eprintln!(
            "{name}: p50 {:.1} us, p99 {:.1} us, p99.9 {:.1} us",
            us(&h, 0.5),
            us(&h, 0.99),
            us(&h, 0.999)
        );
    }
    reported.report("w31.tcp_pingpong", "TCP ping-pong, loopback", Unit::Us)
}

fn echo_run(io: &mut dyn IoBackend, load: EchoLoad) -> EchoStats {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let clients = load
        .start(listener.local_addr().expect("addr"))
        .expect("clients");
    let conns: Vec<_> = (0..load.clients)
        .map(|_| {
            io.register(listener.accept().expect("accept").0)
                .expect("register")
        })
        .collect();
    let stats = echo_loop(io, &conns, &EchoOptions::default()).expect("echo loop");
    for c in clients {
        assert_eq!(
            c.join().expect("client thread").expect("client"),
            load.frames
        );
    }
    stats
}

fn readiness_backend() -> Box<dyn IoBackend> {
    #[cfg(target_os = "linux")]
    {
        Box::new(gateway::io::EpollBackend::with_poller(
            gateway::io::EpollPoller::new().expect("epoll"),
            1024,
            "epoll",
        ))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Box::new(gateway::io::PollBackend::scan(1024))
    }
}

/// 8 client connections, each keeping 64 frames (1–32 byte payloads) in
/// flight, echoed by one event loop; the value is the ring backend's
/// messages per second (median of 3 runs). The readiness (epoll) backend
/// and a low-load run (one frame in flight per client) are printed for
/// comparison, with messages per wakeup.
fn bench_batched_io() -> Report {
    let high = EchoLoad {
        clients: 8,
        window: 64,
        frames: 50_000,
        max_payload: 32,
        seed: 32,
    };
    let low = EchoLoad {
        window: 1,
        frames: 5_000,
        ..high
    };
    let mut ring_rates = Vec::new();
    type Make = fn() -> Box<dyn IoBackend>;
    let backends: [(&str, Make); 2] = [
        ("ring", || Box::new(RingBackend::new(1024).expect("ring"))),
        ("epoll", readiness_backend),
    ];
    for (name, make) in backends {
        let mut rates = Vec::new();
        let mut per_wakeup = Vec::new();
        for _ in 0..3 {
            let s = echo_run(make().as_mut(), high);
            rates.push(s.msgs_per_sec());
            per_wakeup.push(s.per_wakeup());
        }
        rates.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let lo = echo_run(make().as_mut(), low);
        eprintln!(
            "{name:>5}: high load {:.2}M msgs/s ({:.1} msgs/wakeup) | low load {:.2}M msgs/s ({:.2} msgs/wakeup, {} caller syscalls for {} msgs)",
            rates[1] / 1e6,
            per_wakeup[1],
            lo.msgs_per_sec() / 1e6,
            lo.per_wakeup(),
            lo.io.syscalls,
            lo.messages
        );
        if name == "ring" {
            ring_rates = rates;
        }
    }
    Report {
        id: "w32.batched_io".into(),
        label: "Messages per wakeup-batched loop".into(),
        unit: Unit::MsgsPerSec,
        value: ring_rates[1],
        p50: None,
        p99: None,
        p999: None,
        max: None,
        samples: ring_rates.len() as u64,
    }
}
