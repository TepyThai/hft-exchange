//! Week 46 — market replay from pcap captures.

mod common;

use common::{feed, upd, Feed};
use marketdata::feed::{
    encode_incremental, encode_snapshot, FeedPacket, WireError, HEADER_LEN, MAX_SNAPSHOT_LEVELS,
    MAX_UPDATES_PER_PACKET,
};
use marketdata::pcap::{
    internet_checksum, parse_udp, udp_frame, PacketError, PcapError, PcapReader, PcapWriter,
    PCAP_MAGIC_MICROS,
};
use marketdata::replay::{FEED_SOURCE, INCREMENTAL_GROUP, SNAPSHOT_GROUP};
use marketdata::{
    replay_capture, FeedRecorder, L2Snapshot, LadderDepth, PushOutcome, ReorderBuffer, ReplayStats,
    Replayer,
};
use std::net::Ipv4Addr;
use types::{Rng, Seq, Side};

/// Records a feed the way the publisher sends it: batches of up to
/// `batch` updates, each periodic snapshot after the updates it covers,
/// and the final book at the end. Timestamps 1 µs apart.
fn record(f: &Feed, batch: usize) -> Vec<u8> {
    let mut rec = FeedRecorder::with_batch(Vec::new(), batch).unwrap();
    let mut snaps = f.snapshots.iter().peekable();
    for (i, u) in f.updates.iter().enumerate() {
        rec.record_update(i as u64 * 1_000, u).unwrap();
        while let Some((_, s)) = snaps.next_if(|(at, _)| *at == i + 1) {
            rec.record_snapshot(i as u64 * 1_000, s).unwrap();
        }
    }
    let end = f.updates.len() as u64 * 1_000;
    rec.record_snapshot(end, &f.last).unwrap();
    rec.finish(end).unwrap()
}

// ---------------------------------------------------------------------------
// pcap

#[test]
fn w46_pcap_round_trip() {
    let mut rng = Rng::new(46);
    let mut w = PcapWriter::new(Vec::new()).unwrap();
    let mut sent = Vec::new();
    for i in 0..200u64 {
        let len = [0, 1, 17, 1_400, 9_000][rng.range(0, 4) as usize];
        let payload: Vec<u8> = (0..len).map(|_| rng.next_u64() as u8).collect();
        let ts = 1_700_000_000_000_000_000 + i * 1_234_567 + rng.range(0, 999);
        let dst = if i % 2 == 0 {
            INCREMENTAL_GROUP
        } else {
            SNAPSHOT_GROUP
        };
        w.write_udp(ts, FEED_SOURCE, dst, &payload).unwrap();
        sent.push((ts, dst, payload));
    }
    let bytes = w.into_inner().unwrap();

    // The layout: little-endian nanosecond header, Ethernet frames.
    assert_eq!(&bytes[..4], &[0x4D, 0x3C, 0xB2, 0xA1]);
    let r = PcapReader::new(&bytes).unwrap();
    let h = r.header();
    assert_eq!(
        (h.nanos, h.big_endian, h.version, h.snaplen, h.linktype),
        (true, false, (2, 4), 65_535, 1)
    );
    let records: Vec<_> = r.map(|r| r.unwrap()).collect();
    assert_eq!(records.len(), sent.len());
    for (rec, (ts, dst, payload)) in records.iter().zip(&sent) {
        assert_eq!(rec.ts_ns, *ts);
        assert_eq!(rec.orig_len as usize, rec.data.len());
        assert_eq!(rec.data.len(), 42 + payload.len());
        let d = parse_udp(rec.data).unwrap();
        assert_eq!((d.src, d.dst, d.payload), (FEED_SOURCE, *dst, &payload[..]));
        // Multicast MAC for the group, IPv4 checksum valid.
        assert_eq!(
            &rec.data[..6],
            &[0x01, 0x00, 0x5E, 1, 1, dst.ip().octets()[3]]
        );
        assert_eq!(internet_checksum(&rec.data[14..34]), 0);
    }
    // Through a file, as tools would.
    let path = std::env::temp_dir().join(format!("w46-{}.pcap", std::process::id()));
    std::fs::write(&path, &bytes).unwrap();
    let from_disk = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).ok();
    assert_eq!(PcapReader::new(&from_disk).unwrap().count(), sent.len());

    // A big-endian, microsecond capture (as written on another machine).
    let mut be = Vec::new();
    be.extend_from_slice(&PCAP_MAGIC_MICROS.to_be_bytes());
    be.extend_from_slice(&[0, 2, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0]);
    be.extend_from_slice(&65_535u32.to_be_bytes());
    be.extend_from_slice(&1u32.to_be_bytes());
    let mut frame = Vec::new();
    udp_frame(FEED_SOURCE, INCREMENTAL_GROUP, 7, b"hello", &mut frame);
    for (sec, usec) in [(1u32, 999_999u32), (2, 0)] {
        be.extend_from_slice(&sec.to_be_bytes());
        be.extend_from_slice(&usec.to_be_bytes());
        be.extend_from_slice(&(frame.len() as u32).to_be_bytes());
        be.extend_from_slice(&(frame.len() as u32 + 100).to_be_bytes()); // cut by snaplen
        be.extend_from_slice(&frame);
    }
    let r = PcapReader::new(&be).unwrap();
    assert_eq!((r.header().nanos, r.header().big_endian), (false, true));
    let recs: Vec<_> = r.map(|r| r.unwrap()).collect();
    assert_eq!(recs[0].ts_ns, 1_999_999_000);
    assert_eq!(recs[1].ts_ns, 2_000_000_000);
    assert_eq!(recs[1].orig_len as usize, frame.len() + 100);
    assert_eq!(parse_udp(recs[0].data).unwrap().payload, b"hello");

    // 802.1Q-tagged frames and Ethernet padding past the IP length.
    let mut tagged = frame[..12].to_vec();
    tagged.extend_from_slice(&[0x81, 0x00, 0x00, 0x2A]);
    tagged.extend_from_slice(&frame[12..]);
    tagged.extend_from_slice(&[0; 9]);
    assert_eq!(parse_udp(&tagged).unwrap().payload, b"hello");
}

// ---------------------------------------------------------------------------
// The feed's wire format

#[test]
fn w46_feed_packet_layout_is_exact_and_strict() {
    let mut p = Vec::new();
    encode_incremental(
        &[upd(5, Side::Buy, -2, 7), upd(6, Side::Sell, 258, 0)],
        &mut p,
    );
    #[rustfmt::skip]
    let want = [
        0x4D, 0x44, 1, 1, 2, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0,         // header
        0, 0xFE, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 7, 0, 0, 0, 0, 0, 0, 0,
        1, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    assert_eq!(p, want);
    let Ok(FeedPacket::Incremental(inc)) = FeedPacket::decode(&p) else {
        panic!()
    };
    assert_eq!(
        (inc.first_seq(), inc.len(), inc.is_empty()),
        (Seq(5), 2, false)
    );
    assert_eq!(
        inc.iter().collect::<Vec<_>>(),
        vec![upd(5, Side::Buy, -2, 7), upd(6, Side::Sell, 258, 0)]
    );
    assert_eq!(inc.get(1), upd(6, Side::Sell, 258, 0));

    let snap = L2Snapshot {
        seq: Seq(9),
        bids: vec![common::level(10, 1), common::level(9, 2)],
        asks: vec![common::level(12, 3)],
    };
    let mut s = Vec::new();
    encode_snapshot(&snap, &mut s);
    assert_eq!(
        &s[..HEADER_LEN],
        &[0x4D, 0x44, 1, 2, 2, 0, 1, 0, 9, 0, 0, 0, 0, 0, 0, 0]
    );
    assert_eq!(s.len(), HEADER_LEN + 3 * 16);
    assert_eq!(
        &s[HEADER_LEN..HEADER_LEN + 16],
        &[10, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]
    );
    assert_eq!(
        FeedPacket::decode(&s),
        Ok(FeedPacket::Snapshot(snap.clone()))
    );
    let mut empty = Vec::new();
    encode_snapshot(&L2Snapshot::default(), &mut empty);
    assert_eq!(
        FeedPacket::decode(&empty),
        Ok(FeedPacket::Snapshot(L2Snapshot::default()))
    );
    const { assert!(MAX_UPDATES_PER_PACKET >= 3_000 && MAX_SNAPSHOT_LEVELS >= 4_000) };

    // Strictness: each rule, then every truncation and random damage.
    let with = |base: &[u8], at: usize, v: u8| {
        let mut b = base.to_vec();
        b[at] = v;
        b
    };
    assert_eq!(FeedPacket::decode(&p[..15]), Err(WireError::Truncated));
    assert_eq!(
        FeedPacket::decode(&with(&p, 0, 0)),
        Err(WireError::BadMagic(0x4400))
    );
    assert_eq!(
        FeedPacket::decode(&with(&p, 2, 2)),
        Err(WireError::BadVersion(2))
    );
    assert_eq!(
        FeedPacket::decode(&with(&p, 3, 3)),
        Err(WireError::BadKind(3))
    );
    assert_eq!(
        FeedPacket::decode(&with(&p, 4, 3)),
        Err(WireError::BadLength {
            expected: 67,
            got: 50
        })
    );
    assert_eq!(
        FeedPacket::decode(&with(&p, 6, 1)),
        Err(WireError::BadLength {
            expected: 50,
            got: 50
        })
    );
    assert_eq!(
        FeedPacket::decode(&with(&p, 33, 2)),
        Err(WireError::BadSide { index: 1 })
    );
    assert_eq!(
        FeedPacket::decode(&with(&p, 8, 0)),
        Err(WireError::BadSequence)
    );
    let mut overflow = p.clone();
    overflow[8..16].copy_from_slice(&(u64::MAX - 1).to_le_bytes());
    assert_eq!(FeedPacket::decode(&overflow), Err(WireError::BadSequence));
    assert_eq!(
        FeedPacket::decode(&p[..HEADER_LEN]),
        Err(WireError::BadLength {
            expected: 50,
            got: 16
        })
    );
    let zero_count = [0x4D, 0x44, 1, 1, 0, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0];
    assert_eq!(FeedPacket::decode(&zero_count), Err(WireError::BadSequence));
    let mut extra = p.clone();
    extra.push(0);
    assert!(matches!(
        FeedPacket::decode(&extra),
        Err(WireError::BadLength { .. })
    ));
    // Snapshot levels: out of order, or empty.
    assert_eq!(
        FeedPacket::decode(&with(&s, HEADER_LEN + 16, 11)),
        Err(WireError::BadLevel { index: 1 })
    );
    assert_eq!(
        FeedPacket::decode(&with(&s, HEADER_LEN + 8, 0)),
        Err(WireError::BadLevel { index: 0 })
    );
    assert_eq!(
        FeedPacket::decode(&with(&s, HEADER_LEN + 40, 0)),
        Err(WireError::BadLevel { index: 2 })
    );
    for cut in 0..s.len() {
        assert!(FeedPacket::decode(&s[..cut]).is_err(), "cut at {cut}");
    }
    let mut rng = Rng::new(4646);
    for _ in 0..20_000 {
        let mut b = if rng.chance(0.5) {
            p.clone()
        } else {
            s.clone()
        };
        for _ in 0..rng.range(1, 3) {
            let i = rng.range(0, b.len() as u64 - 1) as usize;
            b[i] = rng.next_u64() as u8;
        }
        let _ = FeedPacket::decode(&b); // never panics
    }
}

// ---------------------------------------------------------------------------
// Reorder buffer

fn drain(rb: &mut ReorderBuffer<u64>) -> Vec<u64> {
    std::iter::from_fn(|| rb.pop())
        .map(|(s, v)| {
            assert_eq!(s.0, v, "item carries its sequence number");
            v
        })
        .collect()
}

#[test]
fn w46_reorder_buffer_converges_under_shuffle_duplicates_and_loss() {
    use PushOutcome::*;
    // The module docs' example.
    let mut rb = ReorderBuffer::new(Seq(7), 8);
    assert_eq!(rb.push(Seq(8), 8), Accepted);
    assert_eq!(rb.push(Seq(10), 10), Accepted);
    assert_eq!(
        (rb.pop(), rb.missing(), rb.buffered()),
        (None, Some(Seq(7)), 2)
    );
    assert_eq!(rb.push(Seq(7), 7), Accepted);
    assert_eq!(drain(&mut rb), vec![7, 8]);
    assert_eq!((rb.next_seq(), rb.missing()), (Seq(9), Some(Seq(9))));
    assert_eq!(rb.push(Seq(8), 8), Duplicate, "already delivered");
    assert_eq!(rb.push(Seq(10), 10), Duplicate, "already buffered");
    assert_eq!(rb.push(Seq(17), 17), Skipped { lost: 1 }, "9 given up");
    assert_eq!(drain(&mut rb), vec![10]);
    assert_eq!(rb.push(Seq(9), 9), Duplicate, "too late: given up");
    for s in 11..=16 {
        assert_eq!(rb.push(Seq(s), s), Accepted);
    }
    assert_eq!(drain(&mut rb), (11..=17).collect::<Vec<_>>());
    assert_eq!((rb.buffered(), rb.missing()), (0, None));
    // Far ahead: everything held is released in order, holes counted.
    rb.push(Seq(19), 19);
    rb.push(Seq(21), 21);
    assert_eq!(
        rb.push(Seq(1_000), 1_000),
        Skipped {
            lost: 1_000 - 18 - 2 - 7
        }
    );
    assert_eq!(drain(&mut rb), vec![19, 21]);
    // A snapshot at 1_000 covers everything up to it.
    rb.push(Seq(1_003), 1_003);
    rb.skip_to(Seq(1_001));
    assert_eq!((rb.next_seq(), rb.buffered()), (Seq(1_001), 1));
    rb.push(Seq(1_001), 1_001);
    rb.push(Seq(1_002), 1_002);
    assert_eq!(drain(&mut rb), vec![1_001, 1_002, 1_003]);
    // Window 1 = strictly in order.
    let mut one = ReorderBuffer::new(Seq(1), 1);
    assert_eq!(one.push(Seq(2), 2), Skipped { lost: 1 });
    assert_eq!(drain(&mut one), vec![2]);

    // Randomized: every message delayed by less than half the window,
    // 5% duplicated (the copy also delayed), 1% dropped. Every message
    // that arrived comes out exactly once, in order; exactly the dropped
    // ones are lost.
    for (seed, window) in [(1u64, 64usize), (2, 256), (3, 1024)] {
        let mut rng = Rng::new(seed);
        let n = 50_000u64;
        // Arrival order: message s at s·16 + a delay below half a window.
        let delay = || 0..(window as u64 / 2) * 16;
        let (mut arrivals, mut expected, mut dropped, mut copies) = (Vec::new(), Vec::new(), 0, 0);
        for s in 1..=n {
            if rng.chance(0.01) {
                dropped += 1;
                continue;
            }
            expected.push(s);
            let d = delay();
            arrivals.push((s * 16 + rng.range(d.start, d.end - 1), s));
            if rng.chance(0.05) {
                copies += 1;
                arrivals.push((s * 16 + rng.range(d.start, d.end - 1), s));
            }
        }
        arrivals.sort();
        let mut rb = ReorderBuffer::new(Seq(1), window);
        let (mut out, mut lost, mut dups) = (Vec::new(), 0, 0);
        for &(_, s) in &arrivals {
            match rb.push(Seq(s), s) {
                Accepted => {}
                Duplicate => dups += 1,
                Skipped { lost: l } => lost += l,
            }
            out.extend(drain(&mut rb));
        }
        // A marker a window past the end pushes the last holes out.
        let end = n + window as u64;
        if let Skipped { lost: l } = rb.push(Seq(end), end) {
            lost += l;
        }
        out.extend(drain(&mut rb));
        assert_eq!(out, expected, "window {window}");
        assert_eq!((lost, dups), (dropped, copies), "window {window}");
    }
}

// ---------------------------------------------------------------------------
// Replay

#[test]
fn w46_replay_rebuilds_books_matching_recorded_snapshots() {
    let f = feed(46, 30_000, 3_000, 1_000);
    let capture = record(&f, 32);
    let mut r = Replayer::new(256);
    replay_capture(&capture, &mut r).unwrap();
    let s = *r.stats();
    let snapshots = f.snapshots.len() as u64 + 1;
    assert_eq!(s.snapshot_packets, snapshots);
    assert_eq!(s.records, s.incremental_packets + s.snapshot_packets);
    assert_eq!(
        (s.malformed, s.updates, s.duplicates, s.lost, s.recoveries),
        (0, 30_000, 0, 0, 0)
    );
    assert_eq!((s.verified, s.mismatches, s.unverified), (snapshots, 0, 0));
    assert_eq!(r.builder().snapshot(), f.last);
    assert!(r.builder().is_synced());

    // Deterministic, and the same on the ladder.
    let mut again = Replayer::with_depth(LadderDepth::new(256), 256);
    replay_capture(&capture, &mut again).unwrap();
    assert_eq!(*again.stats(), s);
    assert_eq!(again.builder().snapshot(), f.last);

    // A wrong snapshot in the capture is caught at its sequence number.
    let bad_at = f.snapshots[7].1.seq;
    let mut damaged = f.snapshots.clone();
    damaged[7].1.bids[2].qty.0 += 1;
    let tampered = Feed {
        updates: f.updates.clone(),
        snapshots: damaged,
        last: f.last.clone(),
    };
    let mut r = Replayer::new(256);
    replay_capture(&record(&tampered, 32), &mut r).unwrap();
    assert_eq!(
        (r.stats().verified, r.stats().mismatches),
        (snapshots - 1, 1)
    );
    assert_eq!(r.first_mismatch(), Some(bad_at));
    assert_eq!(
        r.builder().snapshot(),
        f.last,
        "a bad snapshot is reported, not applied"
    );
}

/// Packets of a feed as `(is_snapshot, payload)`, in publishing order.
fn packets(f: &Feed, batch: usize) -> Vec<(bool, Vec<u8>)> {
    let mut out = Vec::new();
    let mut snaps = f.snapshots.iter().peekable();
    let mut start = 0;
    for i in 0..f.updates.len() {
        let snap_due = snaps.peek().is_some_and(|(at, _)| *at == i + 1);
        if i + 1 - start == batch || snap_due || i + 1 == f.updates.len() {
            let mut p = Vec::new();
            encode_incremental(&f.updates[start..=i], &mut p);
            out.push((false, p));
            start = i + 1;
        }
        while let Some((_, s)) = snaps.next_if(|(at, _)| *at == i + 1) {
            let mut p = Vec::new();
            encode_snapshot(s, &mut p);
            out.push((true, p));
        }
    }
    let mut p = Vec::new();
    encode_snapshot(&f.last, &mut p);
    out.push((true, p));
    out
}

fn write_capture(packets: &[(bool, Vec<u8>)]) -> Vec<u8> {
    let mut w = PcapWriter::new(Vec::new()).unwrap();
    for (i, (snapshot, p)) in packets.iter().enumerate() {
        let dst = if *snapshot {
            SNAPSHOT_GROUP
        } else {
            INCREMENTAL_GROUP
        };
        w.write_udp(i as u64 * 1_000, FEED_SOURCE, dst, p).unwrap();
    }
    w.into_inner().unwrap()
}

#[test]
fn w46_replay_survives_reordered_duplicated_and_dropped_packets() {
    let f = feed(4646, 30_000, 3_000, 500);
    let clean = packets(&f, 16);
    for seed in 0..4u64 {
        let mut rng = Rng::new(seed);
        // Network faults on the incremental channel: packets delayed by up
        // to 6 places, 5% duplicated, 2% lost. Snapshots stay in place.
        let mut keyed: Vec<(u64, &(bool, Vec<u8>))> = Vec::new();
        for (i, p) in clean.iter().enumerate() {
            let at = i as u64 * 10;
            if p.0 {
                keyed.push((at + 9, p));
                continue;
            }
            if rng.chance(0.02) {
                continue;
            }
            keyed.push((at + rng.range(0, 60), p));
            if rng.chance(0.05) {
                keyed.push((at + rng.range(0, 60), p));
            }
        }
        keyed.sort_by_key(|(k, _)| *k);
        let faulty: Vec<(bool, Vec<u8>)> = keyed.into_iter().map(|(_, p)| p.clone()).collect();
        let mut r = Replayer::new(128);
        replay_capture(&write_capture(&faulty), &mut r).unwrap();
        let s = *r.stats();
        assert_eq!(r.builder().snapshot(), f.last, "seed {seed}: {s:?}");
        assert_eq!(s.mismatches, 0, "seed {seed}: {s:?}");
        assert!(
            s.duplicates > 0 && s.recoveries > 0 && s.verified > 0,
            "seed {seed}: {s:?}"
        );
        assert_eq!(s.malformed, 0);
    }

    // A capture that starts mid-feed: the first snapshot gets the book.
    let mid = clean.iter().position(|(snap, _)| *snap).unwrap() + 3;
    let late = write_capture(&clean[mid..]);
    let mut r = Replayer::new(128);
    replay_capture(&late, &mut r).unwrap();
    assert_eq!(r.builder().snapshot(), f.last);
    assert!(r.stats().recoveries >= 1 && r.stats().lost > 0);
}

#[test]
fn w46_corrupt_and_truncated_captures_are_reported() {
    let f = feed(464, 3_000, 500, 200);
    let capture = record(&f, 16);
    let records: Vec<(usize, usize)> = {
        // (start offset, length) of every record.
        let mut v = Vec::new();
        let mut r = PcapReader::new(&capture).unwrap();
        loop {
            let at = r.offset();
            match r.next() {
                Some(Ok(_)) => v.push((at, r.offset() - at)),
                None => break,
                Some(Err(e)) => panic!("{e:?}"),
            }
        }
        v
    };
    let n = records.len();

    // Cut anywhere inside the last two records: the good prefix replays,
    // then the reader reports where the damage starts.
    let (last2, _) = records[n - 2];
    for cut in last2..capture.len() {
        let in_last = cut >= records[n - 1].0;
        let (good, bad_at) = if in_last {
            (n - 1, records[n - 1].0)
        } else {
            (n - 2, last2)
        };
        let mut reader = PcapReader::new(&capture[..cut]).unwrap();
        let ok = reader.by_ref().take_while(|r| r.is_ok()).count();
        if cut == bad_at {
            assert_eq!(ok, good, "a cut exactly between records is a clean end");
            continue;
        }
        assert_eq!(ok, good, "cut at {cut}");
        assert_eq!(reader.next(), None, "the reader stops after an error");
        let mut r = Replayer::new(64);
        assert_eq!(
            replay_capture(&capture[..cut], &mut r),
            Err(PcapError::Truncated { offset: bad_at })
        );
        assert_eq!(r.stats().records as usize, good);
    }

    // Header damage.
    assert_eq!(
        PcapReader::new(&capture[..10]).err(),
        Some(PcapError::Truncated { offset: 0 })
    );
    let mut bad = capture.clone();
    bad[0] = 0;
    assert!(matches!(
        PcapReader::new(&bad).err(),
        Some(PcapError::BadMagic(_))
    ));
    let mut bad = capture.clone();
    bad[20] = 101; // raw IP link type
    assert_eq!(
        PcapReader::new(&bad).err(),
        Some(PcapError::UnsupportedLinkType(101))
    );
    // Record damage: stored > original length, absurd length, bad ns.
    let (first, _) = records[0];
    for (field, value) in [(8usize, 70_000u32), (8, 1 << 30), (4, 1_000_000_000)] {
        let mut bad = capture.clone();
        bad[first + field..first + field + 4].copy_from_slice(&value.to_le_bytes());
        let mut reader = PcapReader::new(&bad).unwrap();
        assert_eq!(
            reader.next(),
            Some(Err(PcapError::BadRecord { offset: first }))
        );
        assert_eq!(reader.next(), None);
    }

    // Frame damage: bad IP checksum, not IPv4, not UDP, fragments, wrong
    // UDP length, garbage feed packets. Each is counted and skipped; the
    // book recovers from the next snapshot.
    let mut frame = Vec::new();
    udp_frame(FEED_SOURCE, INCREMENTAL_GROUP, 1, b"0123456789", &mut frame);
    let damage = |at: usize, v: u8| {
        let mut b = frame.clone();
        b[at] = v;
        b
    };
    assert_eq!(parse_udp(&damage(14 + 8, 1)), Err(PacketError::BadChecksum));
    assert_eq!(
        parse_udp(&damage(12, 0x86)),
        Err(PacketError::NotIpv4(0x8600))
    );
    assert_eq!(parse_udp(&damage(14, 0x65)), Err(PacketError::BadIpHeader));
    assert_eq!(parse_udp(&frame[..30]), Err(PacketError::Truncated));
    assert_eq!(
        parse_udp(&frame[..frame.len() - 1]),
        Err(PacketError::Truncated)
    );
    let reframe = |f: &mut Vec<u8>| {
        f[24] = 0;
        f[25] = 0;
        let c = internet_checksum(&f[14..34]);
        f[24..26].copy_from_slice(&c.to_be_bytes());
    };
    let mut tcp = frame.clone();
    tcp[23] = 6;
    reframe(&mut tcp);
    assert_eq!(parse_udp(&tcp), Err(PacketError::NotUdp(6)));
    let mut frag = frame.clone();
    frag[20] = 0x20; // more fragments
    reframe(&mut frag);
    assert_eq!(parse_udp(&frag), Err(PacketError::Fragmented));
    assert_eq!(
        parse_udp(&damage(14 + 20 + 5, 99)),
        Err(PacketError::BadUdpLength)
    );
    assert_eq!(parse_udp(&frame).unwrap().src, FEED_SOURCE);
    assert_eq!(
        parse_udp(&frame).unwrap().dst.ip(),
        &Ipv4Addr::new(239, 1, 1, 1)
    );

    let mut packets = packets(&f, 16);
    let mut rng = Rng::new(46);
    let mut damaged = 0;
    let mut w = PcapWriter::new(Vec::new()).unwrap();
    for (i, (snap, p)) in packets.iter_mut().enumerate() {
        let mut fr = Vec::new();
        udp_frame(
            FEED_SOURCE,
            if *snap {
                SNAPSHOT_GROUP
            } else {
                INCREMENTAL_GROUP
            },
            0,
            p,
            &mut fr,
        );
        if !*snap && i > 5 && rng.chance(0.03) {
            damaged += 1;
            match rng.range(0, 2) {
                0 => fr[14 + 12] ^= 0x40, // IP header bit flip → checksum
                1 => fr[42] = 0,          // feed magic
                _ => fr[42 + 3] = 9,      // feed kind
            }
        }
        w.write_record(i as u64, &fr).unwrap();
    }
    let mut r = Replayer::new(64);
    replay_capture(&w.into_inner().unwrap(), &mut r).unwrap();
    let s: ReplayStats = *r.stats();
    assert!(damaged > 0);
    assert_eq!(s.malformed, damaged);
    assert!(s.recoveries > 0);
    assert_eq!(r.builder().snapshot(), f.last);
}
