//! Week 32: one submission/completion interface, two backends (epoll and a
//! simulated io_uring), the same echo session code on both.

use gateway::io::{
    echo_loop, Completion, ConnId, EchoLoad, EchoOptions, EchoStats, IoBackend, PollBackend,
    RingBackend, Submission, EBADF, EBUSY, ECANCELED, EINVAL,
};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;
use types::Rng;

/// Every backend this platform has, freshly created (epoll first on
/// Linux).
fn backends() -> Vec<(&'static str, Box<dyn IoBackend>)> {
    #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
    let mut v: Vec<(&'static str, Box<dyn IoBackend>)> = vec![
        ("poll", Box::new(PollBackend::scan(256))),
        ("ring", Box::new(RingBackend::new(256).unwrap())),
        ("ring+scan", Box::new(RingBackend::scan(256).unwrap())),
    ];
    #[cfg(target_os = "linux")]
    v.insert(
        0,
        (
            "epoll",
            Box::new(gateway::io::EpollBackend::with_poller(
                gateway::io::EpollPoller::new().unwrap(),
                256,
                "epoll",
            )),
        ),
    );
    v
}

/// Submits and drains until `n` completions are in, sorted by tag:
/// `(user_data, result, bytes)`.
fn complete(io: &mut dyn IoBackend, n: usize) -> Vec<(u64, i64, Vec<u8>)> {
    let mut got: Vec<Completion> = Vec::new();
    while got.len() < n {
        io.submit_and_wait(n - got.len(), Some(Duration::from_secs(10)))
            .unwrap();
        let before = got.len();
        io.drain(&mut got);
        assert!(
            got.len() > before || got.len() >= n,
            "{}: timed out with {} of {n} completions",
            io.name(),
            got.len()
        );
    }
    let mut v: Vec<(u64, i64, Vec<u8>)> = got
        .into_iter()
        .map(|c| (c.user_data, c.result, c.buf.unwrap_or_default()))
        .collect();
    v.sort_by_key(|c| c.0);
    v
}

fn script(io: &mut dyn IoBackend) -> Vec<Vec<(u64, i64, Vec<u8>)>> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    io.listen(listener).unwrap();
    let mut peers: Vec<TcpStream> = (0..3).map(|_| TcpStream::connect(addr).unwrap()).collect();
    let c = ConnId;
    let mut steps = Vec::new();

    // Accept the three in arrival order.
    for ud in 1..=3 {
        io.push(Submission::Accept { user_data: ud }).unwrap();
    }
    steps.push(complete(io, 3));

    // Reads of data already sent.
    for (i, p) in peers.iter_mut().enumerate() {
        p.write_all(&vec![b'a' + i as u8; 10 * (i + 1)]).unwrap();
    }
    for i in 0..3 {
        io.push(Submission::Read {
            conn: c(i),
            buf: vec![0; 4096],
            user_data: 10 + i as u64,
        })
        .unwrap();
    }
    steps.push(complete(io, 3));

    // Writes; the peers receive exactly them.
    for i in 0..3 {
        let buf: Vec<u8> = (0..100 * (i + 1)).map(|k| (k * 7 + i) as u8).collect();
        io.push(Submission::Write {
            conn: c(i as u32),
            buf,
            user_data: 20 + i as u64,
        })
        .unwrap();
    }
    steps.push(complete(io, 3));
    for (i, p) in peers.iter_mut().enumerate() {
        let mut got = vec![0; 100 * (i + 1)];
        p.read_exact(&mut got).unwrap();
        assert!(got.iter().enumerate().all(|(k, &b)| b == (k * 7 + i) as u8));
    }

    // Errors: a second read in flight, an unknown connection, an empty
    // buffer, and a close that cancels the pending read.
    io.push(Submission::Read {
        conn: c(0),
        buf: vec![0; 64],
        user_data: 40,
    })
    .unwrap();
    io.push(Submission::Read {
        conn: c(0),
        buf: vec![0; 64],
        user_data: 41,
    })
    .unwrap();
    io.push(Submission::Read {
        conn: c(99),
        buf: vec![0; 64],
        user_data: 42,
    })
    .unwrap();
    io.push(Submission::Read {
        conn: c(1),
        buf: Vec::new(),
        user_data: 43,
    })
    .unwrap();
    io.push(Submission::Close {
        conn: c(0),
        user_data: 44,
    })
    .unwrap();
    io.push(Submission::Write {
        conn: c(0),
        buf: vec![1],
        user_data: 45,
    })
    .unwrap();
    steps.push(complete(io, 6));

    // End of stream: peer 1 closes; peer 2 sends its last bytes, then
    // closes.
    drop(peers.remove(1));
    peers[1].write_all(b"bye").unwrap();
    drop(peers.remove(1));
    io.push(Submission::Read {
        conn: c(1),
        buf: vec![0; 64],
        user_data: 50,
    })
    .unwrap();
    io.push(Submission::Read {
        conn: c(2),
        buf: vec![0; 64],
        user_data: 51,
    })
    .unwrap();
    steps.push(complete(io, 2));
    io.push(Submission::Read {
        conn: c(2),
        buf: vec![0; 64],
        user_data: 52,
    })
    .unwrap();
    steps.push(complete(io, 1));
    steps
}

#[test]
fn w32_backends_complete_a_scripted_workload_identically() {
    let mut runs = Vec::new();
    for (name, mut io) in backends() {
        let steps = script(io.as_mut());
        runs.push((name, steps, io.stats()));
    }
    let (_, reference, _) = &runs[0];
    let s = |ud: u64, res: i64, bytes: &[u8]| (ud, res, bytes.to_vec());
    assert_eq!(reference[0], vec![s(1, 0, b""), s(2, 1, b""), s(3, 2, b"")]);
    assert_eq!(
        reference[1],
        vec![
            s(10, 10, &[b'a'; 10]),
            s(11, 20, &[b'b'; 20]),
            s(12, 30, &[b'c'; 30])
        ]
    );
    assert_eq!(
        reference[2].iter().map(|c| c.1).collect::<Vec<_>>(),
        vec![100, 200, 300]
    );
    assert_eq!(
        reference[3].iter().map(|c| (c.0, c.1)).collect::<Vec<_>>(),
        vec![
            (40, -ECANCELED),
            (41, -EBUSY),
            (42, -EBADF),
            (43, -EINVAL),
            (44, 0),
            (45, -EBADF)
        ]
    );
    assert_eq!(reference[4], vec![s(50, 0, b""), s(51, 3, b"bye")]);
    assert_eq!(reference[5], vec![s(52, 0, b"")]);
    for (name, steps, stats) in &runs {
        assert_eq!(steps, reference, "{name} differs from {}", runs[0].0);
        assert_eq!(stats.submitted, 3 + 3 + 3 + 6 + 3, "{name}");
        assert_eq!(stats.completed, stats.submitted, "{name}");
    }
}

/// `peers` connections, each having sent `frames` frames and half-closed
/// before the loop starts (everything is ready at the first wakeup); they
/// read their echoes afterwards.
fn preloaded_echo(
    io: &mut dyn IoBackend,
    peers: usize,
    frames: usize,
    opts: &EchoOptions,
) -> EchoStats {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let mut clients = Vec::new();
    let mut conns = Vec::new();
    let frame = [6u8, 0, b'p', b'a', b'y', b'l', b'o', b'd'];
    for _ in 0..peers {
        let mut c = TcpStream::connect(addr).unwrap();
        c.write_all(&frame.repeat(frames)).unwrap();
        c.shutdown(std::net::Shutdown::Write).unwrap();
        let (s, _) = listener.accept().unwrap();
        conns.push(io.register(s).unwrap());
        clients.push(c);
    }
    let readers = std::thread::spawn(move || {
        for mut c in clients {
            let mut echo = vec![0u8; frame.len() * frames];
            c.read_exact(&mut echo).unwrap();
            assert_eq!(echo, frame.repeat(frames));
        }
    });
    // Let every byte land before the first wakeup.
    std::thread::sleep(Duration::from_millis(20));
    let stats = echo_loop(io, &conns, opts).unwrap();
    readers.join().unwrap();
    stats
}

#[test]
fn w32_batching_reduces_wakeups() {
    const PEERS: usize = 32;
    const FRAMES: usize = 4;
    let natural = EchoOptions::default();
    let one_at_a_time = EchoOptions {
        max_batch: 1,
        ..EchoOptions::default()
    };
    let mut user_syscalls = Vec::new();
    for (name, mut io) in backends() {
        let batched = preloaded_echo(io.as_mut(), PEERS, FRAMES, &natural);
        let (_, mut io1) = backends().into_iter().find(|(n, _)| *n == name).unwrap();
        let single = preloaded_echo(io1.as_mut(), PEERS, FRAMES, &one_at_a_time);
        let total = (PEERS * FRAMES) as u64;
        assert_eq!(
            (batched.messages, single.messages),
            (total, total),
            "{name}"
        );
        // One completion per wakeup needs a wakeup per read, write and end
        // of stream; natural batching takes whatever is ready.
        assert!(single.wakeups >= 3 * PEERS as u64, "{name}: {single:?}");
        assert!(
            batched.wakeups * 4 <= single.wakeups,
            "{name}: batched {} vs one-at-a-time {} wakeups",
            batched.wakeups,
            single.wakeups
        );
        assert!(
            batched.per_wakeup() >= 8.0,
            "{name}: {:.1} messages per wakeup",
            batched.per_wakeup()
        );
        user_syscalls.push((name, batched.io.syscalls));
    }
    // The ring backend's caller makes far fewer syscalls than a readiness
    // loop doing the same work itself (one per read/write plus the waits).
    let get = |n: &str| {
        user_syscalls
            .iter()
            .find(|(name, _)| *name == n)
            .map(|x| x.1)
    };
    let readiness = get("epoll").or(get("poll")).unwrap();
    let ring = get("ring").unwrap();
    assert!(readiness >= 3 * PEERS as u64, "{user_syscalls:?}");
    assert!(ring * 2 < readiness, "{user_syscalls:?}");
}

#[test]
fn w32_short_reads_and_writes_are_handled() {
    for (name, mut io) in backends() {
        let io = io.as_mut();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        peer.set_nodelay(true).unwrap();
        let (s, _) = listener.accept().unwrap();
        // A send buffer far smaller than the write forces partial writes
        // (kept above two loopback segments so delayed ACKs don't stall).
        let tuning = gateway::SocketTuning {
            send_buffer: Some(256 * 1024),
            ..gateway::SocketTuning::latency()
        };
        tuning.apply(&s).unwrap();
        let conn = io.register(s).unwrap();

        // One 4 MiB write (far more than the socket takes at once), read by a
        // slow peer in random small chunks.
        let big: Vec<u8> = (0..4 << 20).map(|i: u32| (i % 251) as u8).collect();
        let expect = big.clone();
        let mut reader_peer = peer.try_clone().unwrap();
        let reader = std::thread::spawn(move || {
            let mut rng = Rng::new(7);
            let mut got = Vec::with_capacity(expect.len());
            let mut buf = vec![0u8; 4096];
            while got.len() < expect.len() {
                let want = rng.range(1, 4096) as usize;
                let n = reader_peer.read(&mut buf[..want]).unwrap();
                assert!(n > 0, "unexpected end of stream");
                got.extend_from_slice(&buf[..n]);
                if rng.chance(0.01) {
                    std::thread::sleep(Duration::from_micros(200));
                }
            }
            assert!(got == expect, "the written stream is corrupted");
        });
        io.push(Submission::Write {
            conn,
            buf: big,
            user_data: 1,
        })
        .unwrap();
        let done = complete(io, 1);
        assert_eq!((done[0].0, done[0].1), (1, 4 << 20), "{name}");
        reader.join().unwrap();
        let st = io.stats();
        assert!(
            st.syscalls + st.kernel_syscalls > 16,
            "{name}: the 4 MiB went out in pieces: {st:?}"
        );

        // 200 KB arriving in random pieces, read with random buffer sizes.
        let mut rng = Rng::new(32);
        let data: Vec<u8> = (0..200_000).map(|_| rng.next_u64() as u8).collect();
        let to_send = data.clone();
        let writer = std::thread::spawn(move || {
            let mut rng = Rng::new(33);
            let mut off = 0;
            while off < to_send.len() {
                let n = (rng.range(1, 3000) as usize).min(to_send.len() - off);
                peer.write_all(&to_send[off..off + n]).unwrap();
                off += n;
                if rng.chance(0.05) {
                    std::thread::sleep(Duration::from_micros(100));
                }
            }
            peer
        });
        let mut got = Vec::new();
        let (mut reads, mut short) = (0, 0);
        while got.len() < data.len() {
            let size = rng.range(1, 4096) as usize;
            io.push(Submission::Read {
                conn,
                buf: vec![0; size],
                user_data: 2,
            })
            .unwrap();
            let c = complete(io, 1).remove(0);
            assert!(c.1 > 0, "{name}: read returned {}", c.1);
            assert_eq!(c.1 as usize, c.2.len());
            reads += 1;
            short += usize::from(c.2.len() < size);
            got.extend_from_slice(&c.2);
        }
        assert!(got == data, "{name}: the read stream is corrupted");
        assert!(
            short > 0 && reads > 50,
            "{name}: {reads} reads, {short} short"
        );
        drop(writer.join().unwrap());
    }
}

#[test]
fn w32_echo_under_load_on_every_backend() {
    let load = EchoLoad {
        clients: 4,
        window: 16,
        frames: 2_000,
        max_payload: 64,
        seed: 32,
    };
    for (name, mut io) in backends() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let clients = load.start(listener.local_addr().unwrap()).unwrap();
        let conns: Vec<ConnId> = (0..load.clients)
            .map(|_| io.register(listener.accept().unwrap().0).unwrap())
            .collect();
        let stats = echo_loop(io.as_mut(), &conns, &EchoOptions::default()).unwrap();
        for c in clients {
            assert_eq!(c.join().unwrap().unwrap(), load.frames, "{name}");
        }
        assert_eq!(stats.messages, load.total(), "{name}");
        assert!(
            stats.wakeups > 0 && stats.completions >= 2 * conns.len() as u64,
            "{name}: {stats:?}"
        );
    }
}
