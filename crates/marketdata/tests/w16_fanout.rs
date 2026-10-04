//! Week 16 — broadcast ring, conflation and the WebSocket feed adapter.

mod common;

use common::{feed, upd, RefBook};
use marketdata::json::{write_batch_json, write_snapshot_json, write_update_json};
use marketdata::ws::{
    accept_key, base64_encode, decode_frame, encode_frame, parse_handshake, server_handshake, sha1,
    Frame, FrameError, Handshake, HandshakeError, Opcode,
};
use marketdata::{
    broadcast, serve_ws, Conflator, L2Snapshot, L2Update, Recv, RingValue, SnapshotCell,
};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use types::{Rng, Seq, Side};

// ---------------------------------------------------------------------------
// Broadcast ring

#[test]
fn w16_ring_single_thread_semantics() {
    let (mut tx, ring) = broadcast::<u64>(4);
    assert_eq!(ring.capacity(), 4);
    let mut a = ring.subscribe();
    assert_eq!(a.try_recv(), Recv::Empty, "nothing published yet");

    for v in 0..3u64 {
        assert_eq!(tx.publish(v * 10), v, "publish returns the sequence number");
    }
    // A new reader starts at the live edge.
    let mut late = ring.subscribe();
    assert_eq!(late.next_seq(), 3);
    assert_eq!(late.try_recv(), Recv::Empty);
    for v in [0, 10, 20] {
        assert_eq!(a.try_recv(), Recv::Item(v));
    }
    assert_eq!(a.try_recv(), Recv::Empty);

    // Exactly `capacity` behind is not a lag.
    for v in 3..7u64 {
        tx.publish(v * 10);
    }
    assert_eq!(late.backlog(), 4);
    let mut b = a.clone();
    for v in 3..7u64 {
        assert_eq!(a.try_recv(), Recv::Item(v * 10));
        assert_eq!(late.try_recv(), Recv::Item(v * 10));
    }
    assert_eq!(a.try_recv(), Recv::Empty);

    // One past capacity: exactly one value lost, then the 4 still in the
    // ring, in order.
    for v in 7..12u64 {
        tx.publish(v * 10);
    }
    assert_eq!(a.try_recv(), Recv::Lagged(1));
    assert_eq!(a.next_seq(), 8);
    for v in 8..12u64 {
        assert_eq!(a.try_recv(), Recv::Item(v * 10));
    }
    assert_eq!(a.try_recv(), Recv::Empty);
    // The clone kept its own cursor (at 3): it missed 3..8.
    assert_eq!(b.try_recv(), Recv::Lagged(5));
    assert_eq!(b.try_recv(), Recv::Item(80));

    // Lapped many times: the count is exact.
    for v in 12..112u64 {
        tx.publish(v);
    }
    assert_eq!(a.try_recv(), Recv::Lagged(96));
    assert_eq!(
        (0..4).map(|_| a.try_recv()).collect::<Vec<_>>(),
        (108..112).map(Recv::Item).collect::<Vec<_>>()
    );
    assert_eq!(a.try_recv(), Recv::Empty);

    // The oldest-reader sees the last `capacity` values.
    let mut oldest = ring.subscribe_oldest();
    assert_eq!(oldest.next_seq(), 108);
    assert_eq!(oldest.try_recv(), Recv::Item(108));
    assert_eq!(ring.published(), 112);
    assert_eq!(tx.next_seq(), 112);
    let mut fresh = tx.subscribe();
    assert_eq!(fresh.try_recv(), Recv::Empty);

    // A ring shorter than capacity: subscribe_oldest starts at 0.
    let (mut tx2, ring2) = broadcast::<u64>(8);
    tx2.publish(5);
    assert_eq!(ring2.subscribe_oldest().try_recv(), Recv::Item(5));

    // L2Update packs into 3 words and round-trips at the extremes.
    assert_eq!(<L2Update as RingValue>::WORDS, 3);
    let mut words = [0u64; 3];
    for u in [
        upd(1, Side::Buy, 100, 5),
        upd((1 << 63) - 1, Side::Sell, i64::MIN, u64::MAX),
        upd(0, Side::Sell, -1, 0),
        upd(42, Side::Buy, i64::MAX, 1),
    ] {
        u.pack(&mut words);
        assert_eq!(L2Update::unpack(&words), u);
    }
    let (mut tx3, ring3) = broadcast::<L2Update>(2);
    let mut r3 = ring3.subscribe();
    let u = upd(9, Side::Sell, -7, 3);
    tx3.publish(u);
    assert_eq!(r3.try_recv(), Recv::Item(u));

    // Invalid capacities are refused.
    for cap in [0, 1, 3, 6, 100] {
        assert!(
            std::panic::catch_unwind(|| broadcast::<u64>(cap)).is_err(),
            "capacity {cap} must be refused"
        );
    }
}

/// A 5-word payload whose words are all derived from the first: a torn
/// read (words from two different publishes) breaks the relation.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Probe([u64; 5]);

impl Probe {
    fn new(v: u64) -> Probe {
        Probe([
            v,
            !v,
            v.rotate_left(17),
            v.wrapping_mul(0x9E37_79B9_7F4A_7C15),
            v ^ 0xA5A5_A5A5_A5A5_A5A5,
        ])
    }

    fn is_consistent(&self) -> bool {
        *self == Probe::new(self.0[0])
    }
}

impl RingValue for Probe {
    const WORDS: usize = 5;
    fn pack(&self, words: &mut [u64]) {
        words.copy_from_slice(&self.0);
    }
    fn unpack(words: &[u64]) -> Self {
        let mut w = [0; 5];
        w.copy_from_slice(words);
        Probe(w)
    }
}

#[test]
fn w16_ring_multi_reader_stress_is_gap_free_or_lagged() {
    const N: u64 = 300_000;
    const CAP: usize = 64;
    let (mut tx, ring) = broadcast::<Probe>(CAP);
    let done = Arc::new(AtomicBool::new(false));

    // Reader kinds: 0–3 spin as fast as they can, 4–5 dawdle now and then
    // (so the writer laps them), 6 does not read until the writer is done.
    let readers: Vec<_> = (0..7)
        .map(|k| {
            let mut r = ring.subscribe();
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                if k == 6 {
                    while !done.load(Ordering::Acquire) {
                        std::thread::yield_now();
                    }
                }
                let (mut expected, mut items, mut lagged) = (0u64, 0u64, 0u64);
                let mut rng = Rng::new(k);
                let deadline = Instant::now() + Duration::from_secs(60);
                loop {
                    match r.try_recv() {
                        Recv::Item(p) => {
                            assert!(p.is_consistent(), "reader {k}: torn read {p:?}");
                            assert_eq!(p.0[0], expected, "reader {k}: out of order");
                            expected += 1;
                            items += 1;
                            if k >= 4 && rng.chance(0.001) {
                                std::thread::sleep(Duration::from_micros(200));
                            }
                        }
                        Recv::Lagged(n) => {
                            assert!(n > 0, "reader {k}: empty lag");
                            expected += n;
                            lagged += n;
                            assert_eq!(r.next_seq(), expected);
                        }
                        Recv::Empty => {
                            if done.load(Ordering::Acquire) && r.next_seq() == N {
                                break;
                            }
                            assert!(Instant::now() < deadline, "reader {k} stuck");
                            std::hint::spin_loop();
                        }
                    }
                }
                assert_eq!(expected, N);
                assert_eq!(
                    items + lagged,
                    N,
                    "reader {k}: every value seen or counted lost"
                );
                (items, lagged)
            })
        })
        .collect();

    for v in 0..N {
        tx.publish(Probe::new(v));
    }
    done.store(true, Ordering::Release);
    let results: Vec<(u64, u64)> = readers.into_iter().map(|h| h.join().unwrap()).collect();
    // The parked reader started N values late in a ring of CAP: it lost
    // exactly N - CAP and read the last CAP.
    assert_eq!(results[6], (CAP as u64, N - CAP as u64));
}

// ---------------------------------------------------------------------------
// Conflation

#[test]
fn w16_conflation_keeps_last_update_per_level() {
    use Side::*;
    let mut c = Conflator::new();
    assert!(c.is_empty());
    assert_eq!(c.pop(), None);
    c.push(upd(1, Buy, 100, 5));
    c.push(upd(2, Sell, 101, 3));
    c.push(upd(3, Buy, 100, 7)); // overwrites in place: stays first
    c.push(upd(4, Buy, 99, 2));
    c.push(upd(5, Sell, 101, 0)); // deletes are values too
    c.push(upd(6, Sell, 100, 1)); // same price, other side: another level
    assert_eq!(c.len(), 4);
    assert_eq!(c.pop(), Some(upd(3, Buy, 100, 7)));
    assert_eq!(c.pop(), Some(upd(5, Sell, 101, 0)));
    // A popped level that changes again goes to the back.
    c.push(upd(7, Buy, 100, 9));
    assert_eq!(c.pop(), Some(upd(4, Buy, 99, 2)));
    assert_eq!(c.pop(), Some(upd(6, Sell, 100, 1)));
    assert_eq!(c.pop(), Some(upd(7, Buy, 100, 9)));
    assert_eq!(c.pop(), None);
    c.push(upd(8, Buy, 1, 1));
    c.clear();
    assert_eq!((c.len(), c.pop()), (0, None));

    // A slow consumer on a real feed: it pops a few levels now and then.
    // At every drain point its book equals the full-stream book as of the
    // highest sequence number it has seen, and the queue never holds more
    // than one entry per level.
    let f = feed(16, 20_000, 2_000, 0);
    let mut rng = Rng::new(16);
    let (mut full, mut slow) = (RefBook::default(), RefBook::default());
    let mut levels = std::collections::HashSet::new();
    let mut drains = 0;
    for u in &f.updates {
        full.apply(u);
        levels.insert((u.side, u.price));
        c.push(*u);
        assert!(c.len() <= levels.len());
        if rng.chance(0.05) {
            for _ in 0..rng.range(1, 8) {
                if let Some(p) = c.pop() {
                    slow.apply(&p);
                }
            }
        }
        if rng.chance(0.002) {
            while let Some(p) = c.pop() {
                slow.apply(&p);
            }
            assert_eq!(slow, full, "book differs at drain point seq {}", u.seq.0);
            drains += 1;
        }
    }
    while let Some(p) = c.pop() {
        slow.apply(&p);
    }
    assert_eq!(slow, full);
    assert_eq!(full.snapshot().bids, f.last.bids);
    assert!(drains > 10);
}

// ---------------------------------------------------------------------------
// WebSocket: hashing, handshake, framing

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn w16_ws_accept_key_matches_rfc6455() {
    // FIPS 180 / RFC 3174 test vectors, including the one- and two-block
    // padding cases.
    assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    assert_eq!(
        hex(&sha1(b"abc")),
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    assert_eq!(
        hex(&sha1(
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
        )),
        "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
    );
    assert_eq!(
        hex(&sha1(&vec![b'a'; 1_000_000])),
        "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
    );
    assert_eq!(
        hex(&sha1(b"The quick brown fox jumps over the lazy dog")),
        "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12"
    );
    // RFC 4648 §10.
    for (input, want) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(base64_encode(input.as_bytes()), want);
    }
    assert_eq!(base64_encode(&[0xFB, 0xFF, 0xBF]), "+/+/");
    // RFC 6455 §1.3.
    assert_eq!(
        accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
        "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    );
}

/// An in-memory stream: reads from `input` in small pieces, records writes.
struct MockStream {
    input: Vec<u8>,
    pos: usize,
    output: Vec<u8>,
}

impl Read for MockStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = buf.len().min(7).min(self.input.len() - self.pos);
        buf[..n].copy_from_slice(&self.input[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Write for MockStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.output.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

const RFC_REQUEST: &str = "GET /chat HTTP/1.1\r\nHost: server.example.com\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nOrigin: http://example.com\r\nSec-WebSocket-Protocol: chat, superchat\r\nSec-WebSocket-Version: 13\r\n\r\n";

#[test]
fn w16_ws_handshake_accepts_valid_and_refuses_invalid_requests() {
    assert_eq!(
        parse_handshake(RFC_REQUEST.as_bytes()),
        Ok(Handshake {
            path: "/chat".into(),
            key: "dGhlIHNhbXBsZSBub25jZQ==".into()
        })
    );
    // Header names are case-insensitive; Connection may list tokens.
    let browser = "GET /feed?x=1 HTTP/1.1\r\nhost: h\r\nconnection: keep-alive, Upgrade\r\nUPGRADE: WebSocket\r\nsec-websocket-version: 13\r\nsec-websocket-key:   AAAAAAAAAAAAAAAAAAAAAA==  \r\n\r\n";
    assert_eq!(
        parse_handshake(browser.as_bytes()).unwrap().path,
        "/feed?x=1"
    );

    let with = |from: &str, to: &str| RFC_REQUEST.replace(from, to);
    for (req, err) in [
        (with("GET", "POST"), HandshakeError::NotGet),
        (
            with("HTTP/1.1\r\nHost", "HTTP/1.0\r\nHost"),
            HandshakeError::Malformed,
        ),
        (
            with("Host: server.example.com", "no colon"),
            HandshakeError::Malformed,
        ),
        (
            with("Upgrade: websocket", "Upgrade: h2c"),
            HandshakeError::NotUpgrade,
        ),
        (
            with("Connection: Upgrade", "Connection: close"),
            HandshakeError::NotUpgrade,
        ),
        (
            with("Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n", ""),
            HandshakeError::BadKey,
        ),
        (
            with("dGhlIHNhbXBsZSBub25jZQ==", "short=="),
            HandshakeError::BadKey,
        ),
        (
            with("dGhlIHNhbXBsZSBub25jZQ==", "dGhlIHNhbXBsZSBub25j!Q=="),
            HandshakeError::BadKey,
        ),
        (
            with("Version: 13", "Version: 8"),
            HandshakeError::UnsupportedVersion,
        ),
    ] {
        assert_eq!(parse_handshake(req.as_bytes()), Err(err), "{req:?}");
    }

    // Server side over a stream that delivers the request 7 bytes at a
    // time, followed by the first bytes of a client frame.
    let mut input = RFC_REQUEST.as_bytes().to_vec();
    input.extend_from_slice(&[0x89, 0x80]);
    let mut s = MockStream {
        input,
        pos: 0,
        output: Vec::new(),
    };
    let (hs, mut rest) = server_handshake(&mut s).unwrap();
    assert_eq!(hs.path, "/chat");
    // Bytes read past the request are handed back, not lost.
    rest.extend_from_slice(&s.input[s.pos..]);
    assert_eq!(rest, vec![0x89, 0x80], "bytes after the request are kept");
    assert_eq!(
        String::from_utf8(s.output).unwrap(),
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n"
    );
    // Refusals answer with an HTTP error before returning it.
    let mut s = MockStream {
        input: with("Version: 13", "Version: 8").into_bytes(),
        pos: 0,
        output: Vec::new(),
    };
    assert!(server_handshake(&mut s).is_err());
    assert!(String::from_utf8(s.output)
        .unwrap()
        .starts_with("HTTP/1.1 426"));
    let mut s = MockStream {
        input: vec![b'a'; 20_000],
        pos: 0,
        output: Vec::new(),
    };
    assert!(server_handshake(&mut s).is_err());
    assert!(String::from_utf8(s.output)
        .unwrap()
        .starts_with("HTTP/1.1 400"));
}

/// A client → server frame: masked with `key`.
fn masked(opcode: u8, payload: &[u8], key: [u8; 4]) -> Vec<u8> {
    let mut v = vec![0x80 | opcode];
    match payload.len() {
        n @ 0..=125 => v.push(0x80 | n as u8),
        n @ 126..=65535 => {
            v.push(0x80 | 126);
            v.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            v.push(0x80 | 127);
            v.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    v.extend_from_slice(&key);
    v.extend(payload.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));
    v
}

#[test]
fn w16_ws_frame_encoding_at_length_boundaries() {
    let mut rng = Rng::new(1606);
    for (len, header) in [
        (0usize, vec![0x82, 0]),
        (125, vec![0x82, 125]),
        (126, vec![0x82, 126, 0x00, 0x7E]),
        (65_535, vec![0x82, 126, 0xFF, 0xFF]),
        (65_536, vec![0x82, 127, 0, 0, 0, 0, 0, 1, 0, 0]),
    ] {
        let payload: Vec<u8> = (0..len).map(|_| rng.next_u64() as u8).collect();
        let mut out = vec![0xEE]; // encoding appends
        encode_frame(Opcode::Binary, &payload, &mut out);
        assert_eq!(out[0], 0xEE);
        let frame = &out[1..];
        assert_eq!(
            &frame[..header.len()],
            &header[..],
            "header for length {len}"
        );
        assert_eq!(&frame[header.len()..], &payload[..]);
        let (decoded, used) = decode_frame(frame, 1 << 20).unwrap().unwrap();
        assert_eq!(used, frame.len());
        assert_eq!(
            decoded,
            Frame {
                fin: true,
                opcode: Opcode::Binary,
                payload: payload.clone()
            }
        );
        // Every proper prefix is "not yet".
        for cut in [0, 1, header.len() - 1, header.len(), frame.len() - 1] {
            if cut < frame.len() {
                assert_eq!(
                    decode_frame(&frame[..cut], 1 << 20),
                    Ok(None),
                    "len {len} cut {cut}"
                );
            }
        }
        // The same payload from a client, masked.
        let m = masked(0x2, &payload, [0x37, 0xFA, 0x21, 0x3D]);
        let (decoded, used) = decode_frame(&m, 1 << 20).unwrap().unwrap();
        assert_eq!((decoded.payload, used), (payload, m.len()));
    }
    // Text frame, then a second frame in the same buffer.
    let mut out = Vec::new();
    encode_frame(Opcode::Text, b"Hello", &mut out);
    assert_eq!(out, [0x81, 0x05, b'H', b'e', b'l', b'l', b'o']);
    // RFC 6455 §5.7: a masked "Hello".
    let rfc = [
        0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58,
    ];
    let mut two = rfc.to_vec();
    two.extend_from_slice(&masked(0x9, b"hi", [1, 2, 3, 4]));
    let (f1, n1) = decode_frame(&two, 125).unwrap().unwrap();
    assert_eq!(
        (f1.opcode, &f1.payload[..], n1),
        (Opcode::Text, &b"Hello"[..], 11)
    );
    let (f2, _) = decode_frame(&two[n1..], 125).unwrap().unwrap();
    assert_eq!((f2.opcode, &f2.payload[..]), (Opcode::Ping, &b"hi"[..]));

    // Protocol violations.
    assert_eq!(
        decode_frame(&[0xC1, 0x00], 10),
        Err(FrameError::ReservedBits)
    );
    assert_eq!(
        decode_frame(&[0x83, 0x00], 10),
        Err(FrameError::UnknownOpcode(3))
    );
    assert_eq!(
        decode_frame(&[0x8B, 0x00], 10),
        Err(FrameError::UnknownOpcode(0xB))
    );
    assert_eq!(
        decode_frame(&[0x09, 0x00], 10),
        Err(FrameError::BadControlFrame)
    );
    assert_eq!(
        decode_frame(&[0x88, 126, 0, 126], 1000),
        Err(FrameError::BadControlFrame)
    );
    assert_eq!(
        decode_frame(&[0x82, 126, 0x01, 0x00], 255),
        Err(FrameError::TooLong(256))
    );
    assert_eq!(
        decode_frame(&[0x82, 127, 0x80, 0, 0, 0, 0, 0, 0, 0], usize::MAX),
        Err(FrameError::TooLong(1 << 63))
    );
}

// ---------------------------------------------------------------------------
// End to end: ring → WebSocket server → loopback client

/// Just enough JSON for the feed messages: objects, arrays, strings
/// without escapes, integers.
#[derive(Clone, Debug, PartialEq)]
enum Json {
    Num(i64),
    Str(String),
    Arr(Vec<Json>),
    Obj(HashMap<String, Json>),
}

impl Json {
    fn parse(s: &str) -> Json {
        let (v, rest) = Json::value(s.as_bytes());
        assert!(rest.is_empty(), "trailing input in {s}");
        v
    }

    fn value(b: &[u8]) -> (Json, &[u8]) {
        match b[0] {
            b'{' => {
                let mut map = HashMap::new();
                let mut b = &b[1..];
                if b[0] == b'}' {
                    return (Json::Obj(map), &b[1..]);
                }
                loop {
                    let (Json::Str(k), rest) = Json::value(b) else {
                        panic!("key")
                    };
                    assert_eq!(rest[0], b':');
                    let (v, rest) = Json::value(&rest[1..]);
                    map.insert(k, v);
                    b = &rest[1..];
                    if rest[0] == b'}' {
                        return (Json::Obj(map), b);
                    }
                }
            }
            b'[' => {
                let mut items = Vec::new();
                let mut b = &b[1..];
                if b[0] == b']' {
                    return (Json::Arr(items), &b[1..]);
                }
                loop {
                    let (v, rest) = Json::value(b);
                    items.push(v);
                    b = &rest[1..];
                    if rest[0] == b']' {
                        return (Json::Arr(items), b);
                    }
                }
            }
            b'"' => {
                let end = b[1..].iter().position(|&c| c == b'"').unwrap() + 1;
                (
                    Json::Str(String::from_utf8(b[1..end].to_vec()).unwrap()),
                    &b[end + 1..],
                )
            }
            _ => {
                let end = b
                    .iter()
                    .position(|&c| !(c == b'-' || c.is_ascii_digit()))
                    .unwrap_or(b.len());
                let n = std::str::from_utf8(&b[..end]).unwrap().parse().unwrap();
                (Json::Num(n), &b[end..])
            }
        }
    }

    fn get(&self, k: &str) -> &Json {
        match self {
            Json::Obj(m) => &m[k],
            _ => panic!("not an object"),
        }
    }

    fn num(&self) -> i64 {
        match self {
            Json::Num(n) => *n,
            _ => panic!("not a number"),
        }
    }

    fn arr(&self) -> &[Json] {
        match self {
            Json::Arr(a) => a,
            _ => panic!("not an array"),
        }
    }

    fn str(&self) -> &str {
        match self {
            Json::Str(s) => s,
            _ => panic!("not a string"),
        }
    }
}

/// A browser stand-in: connects, handshakes, applies snapshots and
/// batches to its own book.
struct Client {
    stream: TcpStream,
    buf: Vec<u8>,
    book: RefBook,
    snapshots: usize,
    batches: usize,
}

impl Client {
    fn connect(addr: std::net::SocketAddr) -> Client {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let key = "x3JJHMbDL1EzLkh9GBhXDw==";
        write!(
            stream,
            "GET /feed HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
        .unwrap();
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        while !buf.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            buf.push(byte[0]);
        }
        let response = String::from_utf8(buf).unwrap();
        assert!(response.starts_with("HTTP/1.1 101"), "{response}");
        assert!(response.contains(&format!("Sec-WebSocket-Accept: {}", accept_key(key))));
        Client {
            stream,
            buf: Vec::new(),
            book: RefBook::default(),
            snapshots: 0,
            batches: 0,
        }
    }

    fn next_frame(&mut self) -> Frame {
        loop {
            if let Some((f, used)) = decode_frame(&self.buf, 1 << 24).unwrap() {
                self.buf.drain(..used);
                return f;
            }
            let mut chunk = [0u8; 65536];
            let n = self.stream.read(&mut chunk).expect("server went quiet");
            assert!(n > 0, "server closed the connection");
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// Applies one message; returns the sequence number the book reflects.
    fn step(&mut self) -> u64 {
        let f = self.next_frame();
        assert_eq!(f.opcode, Opcode::Text);
        let msg = Json::parse(std::str::from_utf8(&f.payload).unwrap());
        let seq = msg.get("seq").num() as u64;
        match msg.get("type").str() {
            "snapshot" => {
                let levels = |k: &str| {
                    msg.get(k)
                        .arr()
                        .iter()
                        .map(|pq| (pq.arr()[0].num(), pq.arr()[1].num() as u64))
                        .collect()
                };
                self.book.bids = levels("bids");
                self.book.asks = levels("asks");
                self.book.seq = seq;
                self.snapshots += 1;
            }
            "updates" => {
                let updates = msg.get("updates").arr();
                assert!(!updates.is_empty());
                for u in updates {
                    let side = match u.get("side").str() {
                        "buy" => Side::Buy,
                        "sell" => Side::Sell,
                        s => panic!("side {s}"),
                    };
                    let useq = u.get("seq").num() as u64;
                    assert!(
                        useq > self.book.seq && useq <= seq,
                        "update {useq} outside batch"
                    );
                    self.book
                        .set(side, u.get("price").num(), u.get("qty").num() as u64);
                }
                self.book.seq = seq;
                self.batches += 1;
            }
            t => panic!("unknown message type {t}"),
        }
        seq
    }
}

#[test]
fn w16_ws_loopback_client_receives_snapshot_then_updates() {
    // The browser format, exactly.
    let mut s = String::new();
    write_update_json(&upd(7, Side::Buy, 10_000, 25), &mut s);
    assert_eq!(s, r#"{"seq":7,"side":"buy","price":10000,"qty":25}"#);
    s.clear();
    write_snapshot_json(
        &L2Snapshot {
            seq: Seq(6),
            bids: vec![common::level(10_000, 15), common::level(9_999, 40)],
            asks: vec![common::level(10_001, 8)],
        },
        &mut s,
    );
    assert_eq!(
        s,
        r#"{"type":"snapshot","seq":6,"bids":[[10000,15],[9999,40]],"asks":[[10001,8]]}"#
    );
    s.clear();
    write_batch_json(
        Seq(9),
        &[upd(8, Side::Sell, -5, 0), upd(9, Side::Buy, 1, 2)],
        &mut s,
    );
    assert_eq!(
        s,
        r#"{"type":"updates","seq":9,"updates":[{"seq":8,"side":"sell","price":-5,"qty":0},{"seq":9,"side":"buy","price":1,"qty":2}]}"#
    );

    let f = feed(1616, 6_000, 1_000, 0);
    let (first, rest) = f.updates.split_at(2_000);
    let (second, tail) = rest.split_at(3_950);
    let (mut tx, ring) = broadcast::<L2Update>(1024);
    let cell = SnapshotCell::default();
    let mut truth = RefBook::default();
    let publish =
        |us: &[L2Update], tx: &mut marketdata::RingPublisher<L2Update>, truth: &mut RefBook| {
            for (i, u) in us.iter().enumerate() {
                tx.publish(*u);
                truth.apply(u);
                if i % 200 == 199 {
                    cell.store(truth.snapshot());
                }
            }
            cell.store(truth.snapshot());
        };
    // Some history before anyone connects: the first client starts from a
    // snapshot plus whatever the ring still holds.
    publish(first, &mut tx, &mut truth);
    assert_eq!(cell.seq(), Seq(2_000));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (r, c) = (Arc::clone(&ring), cell.clone());
    std::thread::spawn(move || serve_ws(listener, r, c));

    let mut client = Client::connect(addr);
    assert_eq!(client.step(), 2_000, "first message: the snapshot");
    assert_eq!(client.snapshots, 1);
    assert_eq!(client.book, truth);

    // A burst the client thread may or may not keep up with: it gets
    // conflated batches, or snapshots after a lag — either way it ends up
    // with the publisher's book.
    publish(second, &mut tx, &mut truth);
    let deadline = Instant::now() + Duration::from_secs(20);
    while client.book.seq < 5_950 {
        assert!(
            Instant::now() < deadline,
            "client stuck at {}",
            client.book.seq
        );
        client.step();
    }
    assert_eq!(client.book, truth, "client book equals the publisher's");
    // A trickle: every update arrives in a batch.
    let snapshots = client.snapshots;
    for u in tail {
        tx.publish(*u);
        truth.apply(u);
        while client.book.seq < u.seq.0 {
            assert!(
                Instant::now() < deadline,
                "client stuck at {}",
                client.book.seq
            );
            client.step();
        }
        assert_eq!(client.book, truth);
    }
    assert_eq!(client.snapshots, snapshots, "no resync without a lag");
    assert!(client.batches >= 1);
    cell.store(truth.snapshot());

    // A second client joining later gets the current book at once.
    let mut late = Client::connect(addr);
    assert_eq!(late.step(), 6_000);
    assert_eq!(late.book, truth);

    // Ping is answered with Pong; Close is echoed and ends the session.
    late.stream
        .write_all(&masked(0x9, b"hb", [9, 8, 7, 6]))
        .unwrap();
    let pong = late.next_frame();
    assert_eq!((pong.opcode, &pong.payload[..]), (Opcode::Pong, &b"hb"[..]));
    late.stream
        .write_all(&masked(0x8, &1000u16.to_be_bytes(), [1, 2, 3, 4]))
        .unwrap();
    let close = late.next_frame();
    assert_eq!(
        (close.opcode, &close.payload[..]),
        (Opcode::Close, &[0x03, 0xE8][..])
    );
    let mut rest = Vec::new();
    late.stream.read_to_end(&mut rest).unwrap();
    assert!(rest.is_empty(), "server closed after the close frame");

    // Bad handshakes get an HTTP error, not a WebSocket.
    let mut bad = TcpStream::connect(addr).unwrap();
    bad.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    bad.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    let mut resp = String::new();
    bad.read_to_string(&mut resp).unwrap();
    assert!(resp.starts_with("HTTP/1.1 400"), "{resp}");
}
