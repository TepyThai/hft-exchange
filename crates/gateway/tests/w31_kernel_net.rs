//! Week 31: socket options (applied, then read back from the kernel),
//! vectored writes against writers that accept random short writes, the
//! loopback ping-pong, and a tuned gateway shedding a slow consumer.

use gateway::client::Client;
use gateway::net::{
    socket_options, write_all_vectored, write_all_vectored_async, FrameBatch, SocketOptions,
};
use gateway::pingpong::{EchoServer, PingClient, RecvMode, MAX_PING};
use gateway::{Gateway, GatewayConfig, SocketTuning};
use std::io::{self, IoSlice, Write};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use types::{Command, Event, NewOrder, OrderId, Price, Qty, RejectReason, Rng, Side};
use wire::{decode, FrameDecoder, Message};

fn assert_applied(got: SocketOptions, want: &SocketTuning) {
    assert_eq!(got.nodelay, want.nodelay, "{got:?}");
    assert_eq!(got.keepalive, want.keepalive_idle.is_some(), "{got:?}");
    if let Some(idle) = want.keepalive_idle {
        assert_eq!(got.keepalive_idle, idle, "{got:?}");
    }
    assert_eq!(got.linger, None, "linger must stay off: {got:?}");
    for (requested, reported) in [
        (want.recv_buffer, got.recv_buffer),
        (want.send_buffer, got.send_buffer),
    ] {
        if let Some(n) = requested {
            // Linux doubles the request for its bookkeeping (other kernels
            // report it as set).
            assert!(
                reported >= n && reported <= 2 * n,
                "requested {n}, kernel reports {reported}"
            );
            if cfg!(target_os = "linux") {
                assert_eq!(reported, 2 * n);
            }
        }
    }
}

#[test]
fn w31_socket_options_are_applied_and_read_back() {
    let tuning = SocketTuning {
        nodelay: true,
        recv_buffer: Some(96 * 1024),
        send_buffer: Some(48 * 1024),
        keepalive_idle: Some(Duration::from_secs(17)),
        busy_poll_us: None,
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    // Before connecting, through tokio::net::TcpSocket.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .unwrap();
    let client = rt
        .block_on(async {
            let socket = tokio::net::TcpSocket::new_v4()?;
            tuning.apply_to_socket(&socket)?;
            socket.connect(addr).await
        })
        .unwrap();
    assert_applied(socket_options(&client).unwrap(), &tuning);

    // On an accepted socket, explicitly (not inherited from the listener).
    let (accepted, _) = listener.accept().unwrap();
    let before = socket_options(&accepted).unwrap();
    assert!(
        !before.nodelay && !before.keepalive,
        "kernel defaults: {before:?}"
    );
    tuning.apply(&accepted).unwrap();
    assert_applied(socket_options(&accepted).unwrap(), &tuning);

    // Turning options off is applied too.
    let off = SocketTuning {
        nodelay: false,
        keepalive_idle: None,
        ..tuning
    };
    off.apply(&accepted).unwrap();
    assert_applied(socket_options(&accepted).unwrap(), &off);

    // Busy polling needs CAP_NET_ADMIN to raise: either it is set, or the
    // kernel says why not — never silently ignored.
    let busy = SocketTuning {
        busy_poll_us: Some(50),
        ..tuning
    };
    match busy.apply(&accepted) {
        Ok(()) => {}
        Err(e) => assert!(
            matches!(
                e.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
            ),
            "{e:?}"
        ),
    }
    drop(rt);
}

/// A writer that accepts a random number of bytes per call (stopping
/// anywhere, mid-slice included) and sometimes reports `Interrupted`.
struct ShortWriter {
    rng: Rng,
    max: usize,
    interrupt: f64,
    /// Accept a random 1..=max bytes (else exactly `max`, or all).
    random: bool,
    out: Vec<u8>,
    calls: usize,
}

impl ShortWriter {
    fn new(seed: u64, max: usize) -> ShortWriter {
        ShortWriter {
            rng: Rng::new(seed),
            max,
            interrupt: 0.1,
            random: true,
            out: Vec::new(),
            calls: 0,
        }
    }
}

impl Write for ShortWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_vectored(&[IoSlice::new(buf)])
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        self.calls += 1;
        let total: usize = bufs.iter().map(|b| b.len()).sum();
        assert!(total > 0, "write_vectored called with nothing to write");
        if self.rng.chance(self.interrupt) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let mut take = if self.random {
            self.rng.range(1, total.min(self.max) as u64) as usize
        } else {
            total.min(self.max)
        };
        let n = take;
        for b in bufs {
            let k = take.min(b.len());
            self.out.extend_from_slice(&b[..k]);
            take -= k;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// [`ShortWriter`] as a Tokio writer that is also randomly not ready.
struct AsyncShort(ShortWriter);

impl tokio::io::AsyncWrite for AsyncShort {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.poll_write_vectored(cx, &[IoSlice::new(buf)])
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        if self.0.rng.chance(0.3) {
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        Poll::Ready(self.0.write_vectored(bufs))
    }

    fn is_write_vectored(&self) -> bool {
        true
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

fn random_slices(rng: &mut Rng) -> Vec<Vec<u8>> {
    (0..rng.range(0, 20))
        .map(|_| {
            let len = if rng.chance(0.2) {
                0
            } else {
                rng.range(1, 60) as usize
            };
            (0..len).map(|_| rng.next_u64() as u8).collect()
        })
        .collect()
}

#[test]
fn w31_vectored_writes_survive_random_short_writes() {
    let mut rng = Rng::new(31);
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    for round in 0..500u64 {
        let parts = random_slices(&mut rng);
        let want: Vec<u8> = parts.concat();
        let max = rng.range(1, 80) as usize;

        let mut w = ShortWriter::new(round, max);
        let mut iov: Vec<IoSlice<'_>> = parts.iter().map(|p| IoSlice::new(p)).collect();
        assert_eq!(write_all_vectored(&mut w, &mut iov).unwrap(), want.len());
        assert_eq!(w.out, want, "round {round}");
        if want.is_empty() {
            assert_eq!(w.calls, 0, "nothing to write: no call");
        }

        let mut w = AsyncShort(ShortWriter::new(round, max));
        let mut iov: Vec<IoSlice<'_>> = parts.iter().map(|p| IoSlice::new(p)).collect();
        let n = rt
            .block_on(write_all_vectored_async(&mut w, &mut iov))
            .unwrap();
        assert_eq!((n, &w.0.out), (want.len(), &want), "async round {round}");
    }

    // One call per partial write, never one per buffer.
    let parts: Vec<Vec<u8>> = (0..20).map(|i| vec![i as u8; 10]).collect();
    for (max, calls) in [(usize::MAX, 1), (35, 6)] {
        let mut w = ShortWriter {
            interrupt: 0.0,
            random: false,
            ..ShortWriter::new(1, max)
        };
        let mut iov: Vec<IoSlice<'_>> = parts.iter().map(|p| IoSlice::new(p)).collect();
        write_all_vectored(&mut w, &mut iov).unwrap();
        assert_eq!(
            w.calls, calls,
            "20 slices of 10 bytes, {max} bytes per call"
        );
        assert_eq!(w.out, parts.concat());
    }

    // A writer that accepts nothing is an error, not a spin.
    struct Stuck;
    impl Write for Stuck {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Ok(0)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let data = [1u8; 8];
    let err = write_all_vectored(&mut Stuck, &mut [IoSlice::new(&data)]).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::WriteZero);

    // A batch of reports: one frame per slot, decodable after any pattern
    // of short writes.
    let mut batch = FrameBatch::new(64);
    let mut sent = Vec::new();
    for i in 0..100u64 {
        let ev = match i % 3 {
            0 => Event::Accepted { id: OrderId(i) },
            1 => Event::Rejected {
                id: OrderId(i),
                reason: RejectReason::PriceBand,
            },
            _ => Event::Trade {
                taker: OrderId(i),
                maker: OrderId(0),
                taker_side: Side::Sell,
                price: Price(-7),
                qty: Qty(i),
            },
        };
        if batch.push(&Message::Event(ev)) {
            sent.push(Message::Event(ev));
        }
    }
    assert_eq!((batch.len(), sent.len()), (64, 64), "full at capacity");
    let mut w = ShortWriter::new(9, 7);
    assert_eq!(batch.write_blocking(&mut w).unwrap(), batch.bytes());
    let mut w2 = AsyncShort(ShortWriter::new(10, 50));
    assert_eq!(rt.block_on(batch.write_to(&mut w2)).unwrap(), batch.bytes());
    for out in [w.out, w2.0.out] {
        let mut dec = FrameDecoder::default();
        let mut input = &out[..];
        let mut got = Vec::new();
        while let Some(f) = dec.next_frame(&mut input).unwrap() {
            got.push(decode(f).unwrap());
        }
        assert_eq!(got, sent);
    }
}

#[test]
fn w31_pingpong_echoes_every_frame() {
    let t = SocketTuning::latency();
    let servers = [
        (
            EchoServer::spawn(RecvMode::Blocking, t, None).unwrap(),
            RecvMode::Blocking,
        ),
        (
            EchoServer::spawn(RecvMode::BusyPoll, t, None).unwrap(),
            RecvMode::BusyPoll,
        ),
        (
            EchoServer::spawn_tokio(t, None).unwrap(),
            RecvMode::Blocking,
        ),
    ];
    let mut rng = Rng::new(0x31);
    for (server, mode) in servers {
        let mut client = PingClient::connect(server.addr(), mode, t).unwrap();
        let mut echo = Vec::new();
        for _ in 0..300 {
            let len = rng.range(1, MAX_PING as u64) as usize;
            let payload: Vec<u8> = (0..len).map(|_| rng.next_u64() as u8).collect();
            client.ping(&payload, &mut echo).unwrap();
            assert_eq!(echo, payload);
        }
        drop(client);
        assert_eq!(server.join().unwrap(), 300);
    }
}

#[test]
fn w31_tuned_gateway_disconnects_a_slow_consumer() {
    let small = SocketTuning {
        send_buffer: Some(4096),
        recv_buffer: Some(4096),
        ..SocketTuning::latency()
    };
    let gw = Gateway::start(GatewayConfig {
        tuning: Some(small),
        vectored_writes: true,
        outbound_capacity: 64,
        ..GatewayConfig::default()
    })
    .unwrap();

    // A well-behaved session, on vectored writes.
    let mut b = Client::connect(gw.local_addr()).unwrap();
    b.logon(1, 0).unwrap();
    b.send_order(&NewOrder::limit(1, Side::Buy, 50, 1)).unwrap();
    assert_eq!(
        b.recv_events(2).unwrap()[0],
        Event::Accepted { id: OrderId(1) }
    );

    // A session that floods orders and never reads its reports.
    let mut x = Client::connect(gw.local_addr()).unwrap();
    small.apply(x.stream()).unwrap();
    x.logon(2, 0).unwrap();
    let flood = std::thread::spawn(move || {
        let mut id = 0;
        for _ in 0..400 {
            let orders: Vec<Message> = (0..100)
                .map(|_| {
                    id += 1;
                    Command::New(NewOrder::limit(id, Side::Buy, 1 + (id % 40) as i64, 1)).into()
                })
                .collect();
            if x.send_all(&orders).is_err() {
                break; // the gateway closed us
            }
        }
        x
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    while gw.stats().slow_consumers == 0 {
        assert!(
            Instant::now() < deadline,
            "no slow consumer detected: {:?}",
            gw.stats()
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    let mut x = flood.join().unwrap();
    // Its connection is closed (cleanly or by reset), not left hanging.
    x.set_timeout(Some(Duration::from_secs(10))).unwrap();
    let _ = x.read_to_close();

    // The engine never blocked: the other session trades as before.
    b.send_order(&NewOrder::limit(2, Side::Sell, 50, 1))
        .unwrap();
    assert_eq!(b.recv_event().unwrap(), Event::Accepted { id: OrderId(2) });
    let fill = b.recv_event().unwrap();
    assert!(
        matches!(
            fill,
            Event::Trade {
                taker: OrderId(2),
                maker: OrderId(1),
                ..
            }
        ),
        "{fill:?}"
    );
    assert_eq!(gw.stats().slow_consumers, 1);
    let report = gw.shutdown(Duration::from_secs(10));
    assert!(report.is_clean(), "{report:?}");
}
