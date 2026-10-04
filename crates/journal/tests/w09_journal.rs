//! Week 9 — sequenced, checksummed journal and deterministic replay.

use journal::codec::{CANCEL_LEN, NEW_ORDER_LEN, REDUCE_LEN, TAG_CANCEL, TAG_NEW};
use journal::{
    crc32, decode_command, encode_command, encoded_len, replay, replay_frames, Corruption,
    CorruptionKind, Crc32, DecodeError, Frames, Journal, ReplayError, Replayed, StateHasher,
    StateMachine, FIRST_SEQ, FRAME_HEADER_LEN, MAX_COMMAND_LEN, MAX_PAYLOAD_LEN,
};
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use types::{
    AccountId, Command, InstrumentId, NewOrder, OrderId, OrderKind, Price, Qty, Rng, Seq, Side,
    TimeInForce,
};

// ---------------------------------------------------------------------------
// Helpers

/// Bit-at-a-time CRC-32/IEEE: slow, obviously correct, the test oracle.
fn crc32_bitwise(bytes: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in bytes {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

fn random_command(rng: &mut Rng, next_id: &mut u64) -> Command {
    match rng.range(0, 9) {
        0..=5 => {
            *next_id += 1;
            let side = rng.side();
            let qty = rng.range(1, 500);
            let price = rng.range_i64(900, 1100);
            let mut o = if rng.chance(0.05) {
                NewOrder::market(*next_id, side, qty)
            } else {
                NewOrder::limit(*next_id, side, price, qty)
            };
            if rng.chance(0.1) {
                o = o.post_only();
            }
            if o.limit_price().is_some() {
                o = o.with_tif(match rng.range(0, 9) {
                    0 => TimeInForce::Ioc,
                    1 => TimeInForce::Fok,
                    _ => TimeInForce::Gtc,
                });
            }
            Command::New(o.with_account(rng.range(1, 8) as u32))
        }
        6..=7 => Command::Cancel {
            id: OrderId(rng.range(1, (*next_id).max(1))),
        },
        _ => Command::Reduce {
            id: OrderId(rng.range(1, (*next_id).max(1))),
            qty: Qty(rng.range(0, 300)),
        },
    }
}

fn random_commands(seed: u64, n: usize) -> Vec<Command> {
    let mut rng = Rng::new(seed);
    let mut next_id = 0;
    (0..n)
        .map(|_| random_command(&mut rng, &mut next_id))
        .collect()
}

fn encode(cmd: &Command) -> Vec<u8> {
    let mut buf = [0u8; MAX_COMMAND_LEN];
    let n = encode_command(cmd, &mut buf);
    buf[..n].to_vec()
}

/// Builds a frame by hand from the documented layout.
fn raw_frame(seq: u64, payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::new();
    f.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    f.extend_from_slice(&seq.to_le_bytes());
    let mut crc = Crc32::new();
    crc.update(&f[0..12]);
    crc.update(payload);
    f.extend_from_slice(&crc.finish().to_le_bytes());
    f.extend_from_slice(payload);
    f
}

/// Toy state machine: the resting quantity of each order id, as a naive
/// book would track it (new adds, cancel removes, reduce lowers).
#[derive(Default, Clone)]
struct Resting {
    orders: BTreeMap<u64, u64>,
    applied: u64,
}

impl StateMachine for Resting {
    fn apply(&mut self, cmd: &Command) {
        self.applied += 1;
        match *cmd {
            Command::New(o) => {
                if o.limit_price().is_some() && o.tif == TimeInForce::Gtc {
                    self.orders.entry(o.id.0).or_insert(o.qty.0);
                }
            }
            Command::Cancel { id } => {
                self.orders.remove(&id.0);
            }
            Command::Reduce { id, qty } => {
                if let Some(q) = self.orders.get_mut(&id.0) {
                    if qty.0 == 0 {
                        self.orders.remove(&id.0);
                    } else if qty.0 < *q {
                        *q = qty.0;
                    }
                }
            }
        }
    }

    fn state_hash(&self) -> u64 {
        let mut h = StateHasher::new();
        self.applied.hash(&mut h);
        for (id, qty) in &self.orders {
            (id, qty).hash(&mut h);
        }
        h.finish()
    }
}

fn checkpoint_3000(cmds: &[Command]) -> Resting {
    let mut r = Resting::default();
    cmds[..3_000].iter().for_each(|c| r.apply(c));
    r
}

fn collect_ok(j: &Journal, from: Seq) -> Vec<(Seq, Command)> {
    j.iter_from(from)
        .map(|f| f.unwrap())
        .map(|f| (f.seq, f.command().unwrap()))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests

#[test]
fn w09_crc32_matches_reference_vectors() {
    assert_eq!(crc32(b""), 0);
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(
        crc32(b"The quick brown fox jumps over the lazy dog"),
        0x414F_A339
    );
    assert_eq!(crc32(&[0u8; 32]), 0x190A_55AD);
    assert_eq!(crc32(&[0xFFu8; 32]), 0xFF6C_AB0B);

    // Differential against the bitwise oracle, every length 0..=300
    // (exercises every remainder of an 8-byte stride), plus chunked
    // streaming at every split point.
    let mut rng = Rng::new(9);
    let data: Vec<u8> = (0..300).map(|_| rng.next_u64() as u8).collect();
    for n in 0..=data.len() {
        assert_eq!(crc32(&data[..n]), crc32_bitwise(&data[..n]), "len {n}");
    }
    let whole = crc32(&data[..77]);
    for split in 0..=77 {
        let mut c = Crc32::new();
        c.update(&data[..split]);
        c.update(&[]);
        c.update(&data[split..77]);
        assert_eq!(c.finish(), whole, "split {split}");
    }
    assert_eq!(Crc32::default().finish(), 0);
}

#[test]
fn w09_command_codec_roundtrips_and_rejects_garbage() {
    // Documented sizes and tag-first layout.
    let new = Command::New(
        NewOrder::limit(7, Side::Sell, -5, 9)
            .post_only()
            .with_tif(TimeInForce::Fok),
    );
    let bytes = encode(&new);
    assert_eq!(bytes.len(), NEW_ORDER_LEN);
    assert_eq!(bytes.len(), encoded_len(&new));
    assert_eq!(bytes[0], TAG_NEW);
    assert_eq!(bytes[1], 0b0001_0101, "sell | post_only | tif=Fok");
    assert_eq!(&bytes[2..10], &7u64.to_le_bytes());
    assert_eq!(&bytes[18..26], &9u64.to_le_bytes());
    assert_eq!(&bytes[26..34], &(-5i64).to_le_bytes());
    let cancel = Command::Cancel {
        id: OrderId(u64::MAX),
    };
    assert_eq!(
        encode(&cancel),
        [&[TAG_CANCEL][..], &u64::MAX.to_le_bytes()].concat()
    );
    assert_eq!(
        encode(&Command::Reduce {
            id: OrderId(1),
            qty: Qty(2)
        })
        .len(),
        REDUCE_LEN
    );
    assert_eq!(CANCEL_LEN, 9);

    // Boundary values survive the round trip.
    let extremes = [
        Command::New(NewOrder {
            id: OrderId(u64::MAX),
            account: AccountId(u32::MAX),
            instrument: InstrumentId(u32::MAX),
            side: Side::Buy,
            qty: Qty(u64::MAX),
            kind: OrderKind::Limit {
                price: Price(i64::MIN),
                post_only: false,
            },
            tif: TimeInForce::Ioc,
        }),
        Command::New(NewOrder::limit(0, Side::Sell, i64::MAX, 0)),
        Command::New(NewOrder::market(3, Side::Sell, 1)),
        Command::Reduce {
            id: OrderId(0),
            qty: Qty(u64::MAX),
        },
    ];
    for c in extremes.iter().chain(random_commands(1, 5_000).iter()) {
        let b = encode(c);
        assert_eq!(decode_command(&b), Ok(*c));
    }

    // Strict decoding.
    assert_eq!(decode_command(&[]), Err(DecodeError::Empty));
    assert_eq!(
        decode_command(&[0x7F, 0, 0]),
        Err(DecodeError::UnknownTag(0x7F))
    );
    assert_eq!(
        decode_command(&bytes[..NEW_ORDER_LEN - 1]),
        Err(DecodeError::BadLength {
            tag: TAG_NEW,
            expected: NEW_ORDER_LEN,
            actual: NEW_ORDER_LEN - 1
        })
    );
    let mut long = encode(&cancel);
    long.push(0);
    assert!(matches!(
        decode_command(&long),
        Err(DecodeError::BadLength { .. })
    ));
    for bad_flags in [
        0b0010_0000u8,
        0b1000_0000,
        0b0001_1000, /* tif = 3 */
        0b0000_0110, /* market + post-only */
    ] {
        let mut b = bytes.clone();
        b[1] = bad_flags;
        b[26..34].copy_from_slice(&0i64.to_le_bytes());
        assert_eq!(
            decode_command(&b),
            Err(DecodeError::BadFlags(bad_flags)),
            "{bad_flags:#010b}"
        );
    }
    let mut market = encode(&Command::New(NewOrder::market(3, Side::Buy, 1)));
    market[26] = 1;
    assert_eq!(
        decode_command(&market),
        Err(DecodeError::MarketWithPrice(1))
    );
}

#[test]
fn w09_append_assigns_dense_sequence_numbers() {
    let mut j = Journal::new();
    assert!(j.is_empty());
    assert_eq!((j.len(), j.byte_len(), j.last_seq()), (0, 0, None));
    assert_eq!((j.first_seq(), j.next_seq()), (FIRST_SEQ, Seq(1)));
    assert_eq!(j.iter().count(), 0);

    let cmds = random_commands(2, 1_000);
    for (i, c) in cmds.iter().enumerate() {
        assert_eq!(j.append(c), Seq(i as u64 + 1));
    }
    assert_eq!(
        (j.len(), j.last_seq(), j.next_seq()),
        (1_000, Some(Seq(1_000)), Seq(1_001))
    );
    let expected_bytes: usize = cmds.iter().map(|c| FRAME_HEADER_LEN + encoded_len(c)).sum();
    assert_eq!(j.byte_len(), expected_bytes);

    // Full iteration and iteration from every kind of starting point.
    let all = collect_ok(&j, Seq(0));
    assert_eq!(all.len(), 1_000);
    for (i, (seq, c)) in all.iter().enumerate() {
        assert_eq!((*seq, c), (Seq(i as u64 + 1), &cmds[i]));
    }
    assert_eq!(collect_ok(&j, Seq(1)), all);
    assert_eq!(collect_ok(&j, Seq(600)), all[599..]);
    assert_eq!(collect_ok(&j, Seq(1_000)), all[999..]);
    assert!(collect_ok(&j, Seq(1_001)).is_empty());
    assert!(collect_ok(&j, Seq(u64::MAX)).is_empty());

    // The first frame is laid out exactly as documented.
    let b = j.as_bytes();
    let first = encode(&cmds[0]);
    assert_eq!(&b[0..4], &(first.len() as u32).to_le_bytes());
    assert_eq!(&b[4..12], &1u64.to_le_bytes());
    let mut crc = Crc32::new();
    crc.update(&b[0..12]);
    crc.update(&first);
    assert_eq!(&b[12..16], &crc.finish().to_le_bytes());
    assert_eq!(&b[16..16 + first.len()], &first[..]);
    assert_eq!(
        &b[..FRAME_HEADER_LEN + first.len()],
        &raw_frame(1, &first)[..]
    );

    // Encoding is deterministic: same commands, same bytes.
    let mut again = Journal::with_capacity(expected_bytes);
    cmds.iter().for_each(|c| {
        again.append(c);
    });
    assert_eq!(again.as_bytes(), j.as_bytes());

    // A tail journal and clear(): sequence numbers are never reused.
    let mut tail = Journal::starting_at(Seq(501));
    assert_eq!((tail.first_seq(), tail.last_seq()), (Seq(501), None));
    assert_eq!(tail.append(&cmds[0]), Seq(501));
    tail.clear();
    assert!(tail.is_empty());
    assert_eq!((tail.first_seq(), tail.byte_len()), (Seq(502), 0));
    assert_eq!(tail.append(&cmds[1]), Seq(502));
    assert_eq!(collect_ok(&tail, Seq(0)), vec![(Seq(502), cmds[1])]);
}

#[test]
fn w09_corruption_stops_iteration_at_last_good_frame() {
    let cmds = random_commands(3, 200);
    let mut j = Journal::new();
    let mut offsets = Vec::new();
    for c in &cmds {
        offsets.push(j.byte_len());
        j.append(c);
    }
    let good = j.into_bytes();

    // Flip one bit in every byte position of frame #120 (seq 121): header
    // (len, seq, crc) or payload, the damage is caught at that frame.
    let (start, end) = (offsets[120], offsets[121]);
    for pos in start..end {
        for bit in [0u8, 3, 7] {
            let mut bytes = good.clone();
            bytes[pos] ^= 1 << bit;
            let (j2, damage) = Journal::recover(bytes.clone(), FIRST_SEQ);
            let damage = damage.expect("bit flip must be detected");
            assert_eq!(damage.offset, start, "byte {pos} bit {bit}");
            assert!(
                !damage.is_torn_tail() || pos < start + 4,
                "byte {pos}: {damage}"
            );
            assert_eq!(
                (j2.len(), j2.last_seq(), j2.byte_len()),
                (120, Some(Seq(120)), start)
            );

            // The iterator itself: 120 good frames, one error, then nothing.
            let mut it = Frames::new(&bytes, FIRST_SEQ);
            assert_eq!(it.by_ref().take(120).filter(|f| f.is_ok()).count(), 120);
            assert!(matches!(it.next(), Some(Err(c)) if c == damage));
            assert_eq!(it.next(), None);
            assert_eq!((it.offset(), it.next_seq()), (start, Seq(121)));
        }
    }

    // Payload damage is specifically a CRC mismatch.
    let mut bytes = good.clone();
    bytes[start + FRAME_HEADER_LEN] ^= 0x10;
    let (_, d) = Journal::recover(bytes, FIRST_SEQ);
    assert!(matches!(
        d,
        Some(Corruption {
            kind: CorruptionKind::BadCrc { .. },
            ..
        })
    ));

    // An absurd length field is reported as such, not as a truncation.
    let mut bytes = good.clone();
    bytes[start + 3] = 0xFF;
    let (_, d) = Journal::recover(bytes, FIRST_SEQ);
    let len = u32::from_le_bytes(good[start..start + 4].try_into().unwrap()) | 0xFF00_0000;
    assert!(len > MAX_PAYLOAD_LEN);
    assert_eq!(d.map(|c| c.kind), Some(CorruptionKind::BadLength { len }));

    // An intact frame with the wrong sequence number (a gap or a splice).
    let mut spliced = good[..offsets[50]].to_vec();
    spliced.extend_from_slice(&raw_frame(52, &encode(&cmds[50])));
    let (j3, d) = Journal::recover(spliced, FIRST_SEQ);
    assert_eq!(j3.len(), 50);
    assert_eq!(
        d.map(|c| c.kind),
        Some(CorruptionKind::SeqMismatch {
            expected: Seq(51),
            found: Seq(52)
        })
    );

    // Bytes that should start at a later sequence number.
    let (j4, d) = Journal::recover(good.clone(), Seq(10));
    assert!(j4.is_empty() && j4.next_seq() == Seq(10));
    assert_eq!(
        d,
        Some(Corruption {
            offset: 0,
            kind: CorruptionKind::SeqMismatch {
                expected: Seq(10),
                found: Seq(1)
            }
        })
    );
}

#[test]
fn w09_torn_tail_is_truncated_and_appends_resume() {
    let cmds = random_commands(4, 50);
    let mut j = Journal::new();
    for c in &cmds[..49] {
        j.append(c);
    }
    let before_last = j.byte_len();
    j.append(&cmds[49]);
    let good = j.into_bytes();

    // Cut the last frame at every possible length: a torn write.
    for cut in before_last..good.len() {
        let (mut r, damage) = Journal::recover(good[..cut].to_vec(), FIRST_SEQ);
        if cut == before_last {
            assert_eq!(damage, None, "a clean frame boundary is not damage");
        } else {
            let d = damage.expect("torn tail detected");
            assert!(d.is_torn_tail(), "cut {cut}: {d}");
            assert_eq!(d.offset, before_last);
            let available = cut - before_last;
            let needed = if available < FRAME_HEADER_LEN {
                FRAME_HEADER_LEN
            } else {
                good.len() - before_last
            };
            assert_eq!(d.kind, CorruptionKind::Truncated { needed, available });
        }
        assert_eq!(
            (r.len(), r.last_seq(), r.byte_len()),
            (49, Some(Seq(49)), before_last)
        );
        // Appending after recovery continues the sequence and restores the
        // original bytes.
        assert_eq!(r.append(&cmds[49]), Seq(50));
        assert_eq!(r.as_bytes(), &good[..]);
    }

    // Degenerate inputs.
    let (e, d) = Journal::recover(Vec::new(), FIRST_SEQ);
    assert!(e.is_empty() && d.is_none());
    let (e, d) = Journal::recover(vec![1, 2, 3], FIRST_SEQ);
    assert!(e.is_empty());
    assert_eq!(
        d.map(|c| c.kind),
        Some(CorruptionKind::Truncated {
            needed: FRAME_HEADER_LEN,
            available: 3
        })
    );
}

#[test]
fn w09_replay_reproduces_live_state_hash() {
    for seed in [5, 6, 7] {
        let cmds = random_commands(seed, 20_000);
        // Live: journal first, then apply — the sequencer's contract.
        let mut j = Journal::new();
        let mut live = Resting::default();
        for c in &cmds {
            j.append(c);
            live.apply(c);
        }
        assert!(
            !live.orders.is_empty(),
            "toy stream should leave orders resting"
        );

        // Replay into a fresh machine: identical state, identical hash.
        let mut replayed = Resting::default();
        let done = replay(&j, j.first_seq(), &mut replayed).unwrap();
        assert_eq!(
            done,
            Replayed {
                applied: 20_000,
                last_seq: Some(Seq(20_000))
            }
        );
        assert_eq!(replayed.state_hash(), live.state_hash());
        assert_eq!(replayed.orders, live.orders);

        // Replaying the bytes after a round trip through raw storage.
        let (reloaded, damage) = Journal::recover(j.as_bytes().to_vec(), FIRST_SEQ);
        assert!(damage.is_none());
        let mut again = Resting::default();
        replay(&reloaded, Seq(1), &mut again).unwrap();
        assert_eq!(again.state_hash(), live.state_hash());

        // A different history gives a different hash (sanity: the hash
        // actually covers the state).
        let mut shorter = Resting::default();
        cmds[..19_999].iter().for_each(|c| shorter.apply(c));
        assert_ne!(shorter.state_hash(), live.state_hash());
        let mut tweaked = live.clone();
        let (&id, &qty) = tweaked.orders.iter().next().unwrap();
        tweaked.orders.insert(id, qty + 1);
        assert_ne!(tweaked.state_hash(), live.state_hash());
    }
}

#[test]
fn w09_replay_from_mid_sequence_and_partial_journals() {
    let cmds = random_commands(8, 5_000);
    let mut j = Journal::new();
    let mut live = Resting::default();
    let mut checkpoint = None;
    for (i, c) in cmds.iter().enumerate() {
        if i == 3_000 {
            checkpoint = Some(live.clone()); // state after seq 3000
        }
        j.append(c);
        live.apply(c);
    }

    // Snapshot (state at 3000) + replay from 3001 == full state.
    let mut resumed = checkpoint.unwrap();
    let done = replay(&j, Seq(3_001), &mut resumed).unwrap();
    assert_eq!(
        done,
        Replayed {
            applied: 2_000,
            last_seq: Some(Seq(5_000))
        }
    );
    assert_eq!(resumed.state_hash(), live.state_hash());

    // Same, with the tail held in its own journal.
    let mut tail = Journal::starting_at(Seq(3_001));
    cmds[3_000..].iter().for_each(|c| {
        tail.append(c);
    });
    let mut resumed = Resting::default();
    cmds[..3_000].iter().for_each(|c| resumed.apply(c));
    replay(&tail, tail.first_seq(), &mut resumed).unwrap();
    assert_eq!(resumed.state_hash(), live.state_hash());
    // Commands before the tail are missing: refuse rather than silently skip.
    let mut fresh = Resting::default();
    assert_eq!(
        replay(&tail, Seq(1), &mut fresh),
        Err(ReplayError::BeforeStart {
            from: Seq(1),
            first: Seq(3_001)
        })
    );
    assert_eq!(fresh.applied, 0);
    // Past the end: nothing to do.
    assert_eq!(replay(&j, Seq(5_001), &mut fresh), Ok(Replayed::default()));

    // A corrupted frame in raw bytes: replay applies exactly the good
    // prefix and reports where it stopped.
    let mut bytes = j.as_bytes().to_vec();
    let offset_of_4001: usize = cmds[..4_000]
        .iter()
        .map(|c| FRAME_HEADER_LEN + encoded_len(c))
        .sum();
    bytes[offset_of_4001 + FRAME_HEADER_LEN] ^= 0xFF;
    let mut prefix_live = Resting::default();
    cmds[..4_000].iter().for_each(|c| prefix_live.apply(c));
    let mut partial = Resting::default();
    let err = replay_frames(Frames::new(&bytes, FIRST_SEQ), &mut partial).unwrap_err();
    let ReplayError::Corrupt { at, progress } = err else {
        panic!("expected corruption, got {err:?}")
    };
    assert_eq!(at.offset, offset_of_4001);
    assert!(matches!(at.kind, CorruptionKind::BadCrc { .. }));
    assert_eq!(
        progress,
        Replayed {
            applied: 4_000,
            last_seq: Some(Seq(4_000))
        }
    );
    assert_eq!(partial.state_hash(), prefix_live.state_hash());
    // Recovering the bytes keeps that same prefix, which replays cleanly.
    let (damaged, d) = Journal::recover(bytes, FIRST_SEQ);
    assert_eq!(d.map(|c| c.offset), Some(offset_of_4001));
    let mut partial = Resting::default();
    assert_eq!(
        replay(&damaged, Seq(1), &mut partial),
        Ok(Replayed {
            applied: 4_000,
            last_seq: Some(Seq(4_000))
        })
    );
    assert_eq!(partial.state_hash(), prefix_live.state_hash());
    // Raw replay from the middle of the bytes: skipped frames are still
    // verified, so the result is the same as replaying the journal.
    let mut resumed = checkpoint_3000(&cmds);
    replay_frames(
        Frames::new(j.as_bytes(), FIRST_SEQ).skip_to(Seq(3_001)),
        &mut resumed,
    )
    .unwrap();
    assert_eq!(resumed.state_hash(), live.state_hash());

    // A frame that passes its CRC but is not a command (a writer bug).
    let mut good_then_bad = j.as_bytes()[..offset_of_4001].to_vec();
    good_then_bad.extend_from_slice(&raw_frame(4_001, &[0xEE, 1, 2]));
    let (bad, d) = Journal::recover(good_then_bad, FIRST_SEQ);
    assert_eq!((d, bad.len()), (None, 4_001));
    let mut partial = Resting::default();
    assert_eq!(
        replay(&bad, Seq(1), &mut partial),
        Err(ReplayError::Decode {
            seq: Seq(4_001),
            error: DecodeError::UnknownTag(0xEE),
            progress: Replayed {
                applied: 4_000,
                last_seq: Some(Seq(4_000))
            },
        })
    );
    assert_eq!(partial.state_hash(), prefix_live.state_hash());
}
