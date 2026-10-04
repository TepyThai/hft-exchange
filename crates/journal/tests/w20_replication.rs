//! Week 20: replication to a hot standby — frame validation, repair of
//! gaps, duplicates and damage, and the leader lease.

use journal::{
    link, Crc32, FrameBuf, FrameError, Handled, Journal, ManualClock, Primary, ReplicationError,
    Standby, StandbyLink, StateMachine, ToPrimary, ToStandby, FRAME_HEADER_LEN,
};
use std::sync::mpsc;
use std::time::Duration;
use types::{Command, NewOrder, OrderId, Qty, Rng, Seq};

/// Commands of all three sizes.
fn commands(seed: u64, n: usize) -> Vec<Command> {
    let mut rng = Rng::new(seed);
    (1..=n as u64)
        .map(|i| match rng.range(0, 5) {
            0 => Command::Cancel {
                id: OrderId(rng.range(1, i)),
            },
            1 => Command::Reduce {
                id: OrderId(rng.range(1, i)),
                qty: Qty(rng.range(1, 50)),
            },
            _ => Command::New(NewOrder::limit(
                i,
                rng.side(),
                rng.range_i64(900, 1100),
                rng.range(1, 100),
            )),
        })
        .collect()
}

/// A toy state machine: an order-sensitive digest of the commands.
#[derive(Clone, Debug, Default, PartialEq)]
struct Digest {
    seq: u64,
    h: u64,
}

impl StateMachine for Digest {
    fn apply(&mut self, cmd: &Command) {
        let x = match *cmd {
            Command::New(o) => o.id.0 ^ (o.qty.0 << 32),
            Command::Cancel { id } => id.0.rotate_left(17),
            Command::Reduce { id, qty } => id.0 ^ qty.0.rotate_left(40),
        };
        self.seq += 1;
        self.h = (self.h ^ x)
            .wrapping_mul(0x0000_0100_0000_01b3)
            .rotate_left(7);
    }

    fn state_hash(&self) -> u64 {
        self.h ^ self.seq.rotate_left(32)
    }
}

fn digest_of(cmds: &[Command]) -> Digest {
    let mut d = Digest::default();
    cmds.iter().for_each(|c| d.apply(c));
    d
}

/// Every frame of `j` as its own byte vector.
fn frames_of(j: &Journal) -> Vec<Vec<u8>> {
    let bytes = j.as_bytes();
    j.iter()
        .map(|f| {
            let f = f.unwrap();
            bytes[f.offset..f.offset + f.frame_len()].to_vec()
        })
        .collect()
}

const LEASE: Duration = Duration::from_micros(1);

#[test]
fn w20_append_frame_checks_crc_and_sequence() {
    let cmds = commands(20, 50);
    let mut src = Journal::new();
    cmds.iter().for_each(|c| {
        src.append(c);
    });
    let frames = frames_of(&src);

    let mut dst = Journal::new();
    for (i, f) in frames[..10].iter().enumerate() {
        assert_eq!(dst.append_frame(f), Ok(Seq(i as u64 + 1)));
    }
    let before = dst.as_bytes().to_vec();
    assert_eq!(
        dst.append_frame(&frames[3]),
        Err(FrameError::Duplicate {
            expected: Seq(11),
            found: Seq(4)
        })
    );
    assert_eq!(
        dst.append_frame(&frames[12]),
        Err(FrameError::Gap {
            expected: Seq(11),
            found: Seq(13)
        })
    );
    // Any single flipped bit is caught (the CRC covers length and seq too).
    for byte in 0..frames[10].len() {
        for bit in 0..8 {
            let mut f = frames[10].clone();
            f[byte] ^= 1 << bit;
            assert!(
                matches!(dst.append_frame(&f), Err(FrameError::Corrupt(_))),
                "byte {byte} bit {bit}"
            );
        }
    }
    let mut long = frames[10].clone();
    long.push(0);
    assert!(matches!(
        dst.append_frame(&long),
        Err(FrameError::TrailingBytes { .. })
    ));
    let short = &frames[10][..frames[10].len() - 1];
    assert!(matches!(
        dst.append_frame(short),
        Err(FrameError::Corrupt(_))
    ));
    assert!(matches!(dst.append_frame(&[]), Err(FrameError::Corrupt(_))));
    // Checksummed but not a command: a writer bug, refused too.
    let mut bogus = vec![1, 0, 0, 0];
    bogus.extend_from_slice(&11u64.to_le_bytes());
    let mut crc = Crc32::new();
    crc.update(&bogus);
    crc.update(&[0xEE]);
    bogus.extend_from_slice(&crc.finish().to_le_bytes());
    bogus.push(0xEE);
    assert!(matches!(
        dst.append_frame(&bogus),
        Err(FrameError::Decode { seq: Seq(11), .. })
    ));
    assert_eq!(dst.as_bytes(), &before[..], "refused frames change nothing");

    for f in &frames[10..] {
        dst.append_frame(f).unwrap();
    }
    assert_eq!(dst.as_bytes(), src.as_bytes());
}

#[test]
fn w20_standby_applies_acknowledges_and_matches_the_primary() {
    let clock = ManualClock::new(0);
    let (plink, slink) = link();
    let mut primary = Primary::new(Journal::new(), plink, clock.clone(), LEASE);
    assert!(
        !primary.is_leader(),
        "no lease before the standby grants one"
    );
    let mut standby = Standby::new(
        Journal::new(),
        Digest::default(),
        slink,
        clock.clone(),
        LEASE,
    );

    let cmds = commands(7, 2_000);
    for (i, c) in cmds.iter().enumerate() {
        let seq = primary.append(c).unwrap();
        assert_eq!(seq, Seq(i as u64 + 1));
        if i % 3 == 0 {
            // Let a few frames queue up now and then.
            assert!(standby.poll().unwrap() >= 1);
        }
    }
    standby.poll().unwrap();
    primary.poll().unwrap();
    assert_eq!(primary.acked_seq(), Seq(2_000));
    assert_eq!(standby.applied_seq(), Seq(2_000));
    assert_eq!(standby.journal().as_bytes(), primary.journal().as_bytes());
    assert_eq!(standby.machine(), &digest_of(&cmds));
    let stats = standby.stats();
    assert_eq!(
        (stats.applied, stats.gaps, stats.duplicates, stats.corrupt),
        (2_000, 0, 0, 0)
    );
}

#[test]
fn w20_gaps_duplicates_and_damage_are_detected_and_repaired() {
    let clock = ManualClock::new(0);
    let (plink, slink) = link();
    // The test is the network between them: it drops, duplicates and
    // damages frames on the way to the standby.
    let net = slink.rx;
    let (deliver, standby_rx) = mpsc::channel();
    let link_end = StandbyLink {
        tx: slink.tx,
        rx: standby_rx,
    };
    let mut standby = Standby::new(
        Journal::new(),
        Digest::default(),
        link_end,
        clock.clone(),
        LEASE,
    );
    let mut primary = Primary::new(Journal::new(), plink, clock.clone(), LEASE);
    let mut rng = Rng::new(2020);
    let mut forward = |chaos: bool, standby: &mut Standby<Digest, ManualClock>| {
        while let Ok(msg) = net.try_recv() {
            match msg {
                ToStandby::Frame(mut f) if chaos => match rng.range(0, 99) {
                    0..=3 => {} // lost
                    4..=7 => {
                        deliver.send(ToStandby::Frame(f)).unwrap();
                        deliver.send(ToStandby::Frame(f)).unwrap();
                    }
                    8..=10 => {
                        let i = rng.range(0, f.as_bytes().len() as u64 - 1) as usize;
                        f.as_bytes_mut()[i] ^= 1 << rng.range(0, 7);
                        deliver.send(ToStandby::Frame(f)).unwrap();
                    }
                    _ => deliver.send(ToStandby::Frame(f)).unwrap(),
                },
                other => deliver.send(other).unwrap(),
            }
        }
        standby.poll().unwrap();
    };

    let cmds = commands(99, 3_000);
    for c in &cmds {
        primary.append(c).unwrap();
        forward(true, &mut standby);
    }
    // Quiet network: heartbeats expose a lost tail; resends fill holes.
    for _ in 0..100 {
        if primary.acked_seq() == Seq(3_000) {
            break;
        }
        primary.heartbeat().unwrap();
        forward(false, &mut standby);
        primary.poll().unwrap();
        forward(false, &mut standby);
    }
    assert_eq!(primary.acked_seq(), Seq(3_000));
    assert_eq!(standby.journal().as_bytes(), primary.journal().as_bytes());
    assert_eq!(standby.machine(), &digest_of(&cmds));
    let s = standby.stats();
    assert!(s.gaps > 0 && s.duplicates > 0 && s.corrupt > 0, "{s:?}");
    assert!(s.resend_requests > 0 && primary.resent() > 0);
    assert_eq!(s.applied, 3_000, "every command applied exactly once");

    // Handled reports each case.
    let mut src = Journal::new();
    let frames: Vec<_> = (0..3)
        .map(|i| {
            src.append(&cmds[i]);
            journal::FrameBuf::copy_from(&frames_of(&src)[i]).unwrap()
        })
        .collect();
    let (_, slink) = link();
    let mut s = Standby::new(Journal::new(), Digest::default(), slink, clock, LEASE);
    assert_eq!(
        s.handle(ToStandby::Frame(frames[0])),
        Handled::Applied(Seq(1))
    );
    assert_eq!(
        s.handle(ToStandby::Frame(frames[0])),
        Handled::Duplicate(Seq(1))
    );
    assert!(matches!(
        s.handle(ToStandby::Frame(frames[2])),
        Handled::Rejected(FrameError::Gap { .. })
    ));
    let mut bad = frames[1];
    bad.as_bytes_mut()[FRAME_HEADER_LEN] ^= 0x40;
    assert!(matches!(
        s.handle(ToStandby::Frame(bad)),
        Handled::Rejected(FrameError::Corrupt(_))
    ));
    assert_eq!(
        s.handle(ToStandby::Frame(frames[1])),
        Handled::Applied(Seq(2))
    );
}

#[test]
fn w20_lease_decides_who_leads() {
    let clock = ManualClock::new(0);
    let lease = Duration::from_nanos(1_000);
    let (plink, slink) = link();
    let mut primary = Primary::new(Journal::new(), plink, clock.clone(), lease);
    let cmd = commands(1, 1)[0];
    assert!(matches!(
        primary.append(&cmd),
        Err(ReplicationError::NotLeader { .. })
    ));
    // The standby's creation grants the first lease: [0, 1000).
    let mut standby = Standby::new(
        Journal::new(),
        Digest::default(),
        slink,
        clock.clone(),
        lease,
    );
    assert_eq!(primary.append(&cmd), Ok(Seq(1)));
    assert_eq!(primary.lease_until(), 1_000);

    // Each acknowledgement renews it from the standby's clock.
    clock.set(100);
    standby.poll().unwrap();
    primary.poll().unwrap();
    assert_eq!(
        (primary.lease_until(), standby.lease_expires_at()),
        (1_100, 1_100)
    );
    clock.set(1_099);
    assert!(primary.is_leader() && !standby.can_promote());
    let mut standby = *standby.promote().expect_err("lease still current");

    // The instant the lease ends, the primary stops and the standby may go.
    clock.set(1_100);
    assert!(!primary.is_leader() && standby.can_promote());
    assert!(matches!(
        primary.append(&cmd),
        Err(ReplicationError::NotLeader {
            now: 1_100,
            lease_until: 1_100
        })
    ));

    // A heartbeat answered before the standby promoted renews the lease.
    primary.heartbeat().unwrap();
    standby.poll().unwrap();
    primary.poll().unwrap();
    assert_eq!(primary.lease_until(), 2_100);
    assert_eq!(primary.append(&cmd), Ok(Seq(2)));
    standby.poll().unwrap();

    // The primary dies: the standby notices the silence only through the
    // lease, and takes over exactly when it ends — with every command.
    drop(primary);
    assert_eq!(standby.poll(), Err(ReplicationError::Disconnected));
    clock.set(2_099);
    let standby = *standby.promote().expect_err("lease still current");
    clock.set(2_100);
    let (j, m) = standby.promote().expect("lease over");
    assert_eq!(j.last_seq(), Some(Seq(2)));
    assert_eq!(m, digest_of(&[cmd, cmd]));
}

#[test]
fn w20_lost_resend_request_is_repeated_under_load() {
    // The test plays the primary and the network: it sees every request
    // and decides what reaches the standby.
    let lease = Duration::from_micros(400);
    let quarter = (lease.as_nanos() / 4) as u64;
    let clock = ManualClock::new(0);
    let (plink, slink) = link();
    let mut standby = Standby::new(
        Journal::new(),
        Digest::default(),
        slink,
        clock.clone(),
        lease,
    );
    let cmds = commands(48, 6);
    let mut source = Journal::new();
    cmds.iter().for_each(|c| {
        source.append(c);
    });
    let frames = frames_of(&source);
    let frame = |i: usize| ToStandby::Frame(FrameBuf::copy_from(&frames[i]).unwrap());
    let resends = |rx: &std::sync::mpsc::Receiver<ToPrimary>| -> Vec<Seq> {
        rx.try_iter()
            .filter_map(|m| match m {
                ToPrimary::Resend { from } => Some(from),
                _ => None,
            })
            .collect()
    };

    assert_eq!(standby.handle(frame(0)), Handled::Applied(Seq(1)));
    // Frame 2 is lost; frame 3 reveals the gap and the standby asks once.
    assert!(matches!(standby.handle(frame(2)), Handled::Rejected(_)));
    assert_eq!(resends(&plink.rx), vec![Seq(2)]);
    // That request is lost too. More early frames within a quarter lease
    // must not flood the primary with duplicates…
    clock.advance(quarter - 1);
    assert!(matches!(standby.handle(frame(3)), Handled::Rejected(_)));
    assert!(resends(&plink.rx).is_empty());
    // …but once a quarter lease passes without the resend, it asks again,
    // with no heartbeat needed.
    clock.advance(1);
    assert!(matches!(standby.handle(frame(4)), Handled::Rejected(_)));
    assert_eq!(resends(&plink.rx), vec![Seq(2)]);
    // The resend arrives and replication catches up.
    for i in 1..frames.len() {
        assert_eq!(
            standby.handle(frame(i)),
            Handled::Applied(Seq(i as u64 + 1))
        );
    }
    assert_eq!(standby.applied_seq(), Seq(6));
    clock.advance(lease.as_nanos() as u64 + 1);
    let (journal, machine) = standby
        .promote()
        .unwrap_or_else(|_| panic!("the lease has expired"));
    assert_eq!(journal.as_bytes(), source.as_bytes());
    assert_eq!(machine, digest_of(&cmds));
}
