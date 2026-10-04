//! Week 48 — chaos engineering: a deterministic, seeded fault plan, the
//! injector, and scenarios that must recover without loss — checked after
//! every step against an independent model of the truth.

use engine::OrderBook;
use marketdata::feed::encode_incremental;
use marketdata::{BookBuilder, L2Publisher, L2Snapshot, L2Update, Level, SnapshotCell};
use risk::{Fill, Positions, PostTradeLimits};
use sim::chaos::wire::{seal_heartbeat, seal_packet};
use sim::chaos::{
    draw, shrink_plan, ChaosLink, ChaosVenue, FailoverConfig, FailoverSim, FanoutConfig, FanoutSim,
    Fate, Fault, FaultEvent, FaultPlan, FeedClient, FeedConfig, FeedSim, FeedStatus, Injector,
    Leader, Link, RingConsumer,
};
use sim::flow::{FlowConfig, OrderFlow};
use sim::scenario::{
    Agent, AgentCtx, Delivery, InProcess, MakerAgent, MakerConfig, MomentumAgent, MomentumConfig,
    NoiseAgent, NoiseConfig, Runner, ScenarioConfig, ScenarioReport, Venue,
};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::mpsc;
use types::{AccountId, Command, InstrumentId, Price, Qty, Seq, Side};

// ---------------------------------------------------------------------------
// An independent model of the truth: the publisher's book after a prefix of
// its updates, kept in two ordered maps.

#[derive(Default)]
struct Truth {
    bids: BTreeMap<i64, u64>,
    asks: BTreeMap<i64, u64>,
    applied: usize,
}

impl Truth {
    /// Advances to the book after update `seq` (never backwards).
    fn advance(&mut self, published: &[L2Update], seq: Seq) {
        assert!(
            seq.0 as usize >= self.applied,
            "the truth only moves forward"
        );
        while self.applied < seq.0 as usize {
            let u = published[self.applied];
            assert_eq!(u.seq.0 as usize, self.applied + 1, "dense feed sequence");
            let side = match u.side {
                Side::Buy => &mut self.bids,
                Side::Sell => &mut self.asks,
            };
            if u.qty.0 == 0 {
                side.remove(&u.price.0);
            } else {
                side.insert(u.price.0, u.qty.0);
            }
            self.applied += 1;
        }
    }

    fn levels(&self, side: Side) -> Vec<Level> {
        let level = |(&p, &q): (&i64, &u64)| Level {
            price: Price(p),
            qty: Qty(q),
        };
        match side {
            Side::Buy => self.bids.iter().rev().map(level).collect(),
            Side::Sell => self.asks.iter().map(level).collect(),
        }
    }

    fn snapshot(&self) -> L2Snapshot {
        L2Snapshot {
            seq: Seq(self.applied as u64),
            bids: self.levels(Side::Buy),
            asks: self.levels(Side::Sell),
        }
    }

    /// True if `book` holds exactly the truth's levels.
    fn matches(&self, book: &BookBuilder) -> bool {
        book.depth(Side::Buy, usize::MAX) == self.levels(Side::Buy)
            && book.depth(Side::Sell, usize::MAX) == self.levels(Side::Sell)
    }
}

/// Checks a consumer after a step: if it says Live, its book is the truth
/// at its sequence number. Returns whether it was Live.
fn check_live(truth: &mut Truth, published: &[L2Update], book: Option<&BookBuilder>) -> bool {
    let Some(book) = book else { return false };
    let seq = book.last_seq().unwrap_or_default();
    assert!(
        seq.0 as usize <= published.len(),
        "Live beyond the publisher"
    );
    truth.advance(published, seq);
    assert!(truth.matches(book), "Live with a wrong book at {seq:?}");
    true
}

fn md(f: impl Fn(Link) -> Fault) -> Fault {
    f(Link::MarketData)
}

/// Every kind of feed damage at once, over `0..until`.
fn heavy_feed(seed: u64, until: u64) -> FaultPlan {
    from_text(&format!(
        "seed={seed} drop(md,100)@0..{until} corrupt(md,40)@0..{until} dup(md,60)@0..{until} \
         reorder(md,60)@0..{until} delay(md,80,6)@0..{until}"
    ))
}

/// A plan from its text form — the line a failing run reports.
fn from_text(text: &str) -> FaultPlan {
    text.parse().unwrap_or_else(|e| panic!("{e}"))
}

// ---------------------------------------------------------------------------
// 1. The plan is data, and its decisions are pure functions.

/// The documented composition rule, written independently from the
/// injector: what fault `i` does to packet `n` depends only on
/// `draw(plan.stream(i), n)`.
fn model_fate(plan: &FaultPlan, link: Link, step: u64, n: u64) -> Fate {
    let mut fate = Fate::CLEAN;
    let active: Vec<usize> = (0..plan.faults.len())
        .filter(|&i| plan.faults[i].fault.acts_on(link) && plan.faults[i].is_active(step))
        .collect();
    if active
        .iter()
        .any(|&i| plan.faults[i].fault == Fault::Partition)
    {
        fate.partitioned = true;
        return fate;
    }
    for i in active {
        let r = draw(plan.stream(i), n);
        let fault = plan.faults[i].fault;
        if r % 1000 >= fault.per_mille().unwrap() as u64 {
            continue;
        }
        match fault {
            Fault::Drop { .. } => {
                fate.dropped = true;
                return fate;
            }
            Fault::Corrupt { .. } => fate.corrupt = Some((r >> 32) as u32),
            Fault::Duplicate { .. } => fate.duplicate = true,
            Fault::Delay { max_steps, .. } => fate.delay += 1 + (r >> 32) % max_steps as u64,
            Fault::Reorder { .. } => fate.reorder = true,
            _ => unreachable!(),
        }
    }
    fate
}

#[test]
fn w48_fault_plan_round_trips_and_its_decisions_are_pure() {
    let plan = FaultPlan::new(42)
        .with(
            md(|link| Fault::Drop {
                link,
                per_mille: 200,
            }),
            0..1_000,
        )
        .with(
            md(|link| Fault::Corrupt {
                link,
                per_mille: 100,
            }),
            0..1_000,
        )
        .with(
            md(|link| Fault::Duplicate {
                link,
                per_mille: 150,
            }),
            0..1_000,
        )
        .with(
            md(|link| Fault::Delay {
                link,
                per_mille: 120,
                max_steps: 5,
            }),
            500..1_000,
        )
        .with(
            md(|link| Fault::Reorder {
                link,
                per_mille: 80,
            }),
            0..1_000,
        )
        .with(
            Fault::Drop {
                link: Link::ToPrimary,
                per_mille: 1000,
            },
            0..10,
        )
        .with(Fault::Partition, 100..200)
        .with(Fault::Stall { consumer: 2 }, 5..9)
        .with(Fault::CrashPrimary, 300..999);

    // The plan as text, and back.
    let text = "seed=42 drop(md,200)@0..1000 corrupt(md,100)@0..1000 dup(md,150)@0..1000 \
                delay(md,120,5)@500..1000 reorder(md,80)@0..1000 drop(to-primary,1000)@0..10 \
                partition@100..200 stall(2)@5..9 crash@300";
    assert_eq!(plan.to_string(), text);
    assert_eq!(text.parse::<FaultPlan>(), Ok(plan.clone()));
    assert_eq!("seed=5".parse::<FaultPlan>(), Ok(FaultPlan::new(5)));
    for bad in [
        "",
        "drop(md,5)@0..9",
        "seed=x",
        "seed=1 drop(xx,5)@0..1",
        "seed=1 drop(md,1001)@0..1",
        "seed=1 drop(md)@0..1",
        "seed=1 delay(md,5,0)@0..1",
        "seed=1 drop(md,5)@9..1",
        "seed=1 drop(md,5)",
        "seed=1 explode@1..2",
        "seed=1 crash@1..2",
    ] {
        assert!(bad.parse::<FaultPlan>().is_err(), "{bad:?} parsed");
    }

    // Process faults are queries on the plan.
    assert_eq!(plan.crash_at(), Some(300));
    assert!(plan.partitioned(100) && plan.partitioned(199) && !plan.partitioned(200));
    assert!(plan.stalled(2, 5) && !plan.stalled(2, 9) && !plan.stalled(1, 6));
    assert_eq!(plan.quiet_from(), 1_000);

    // Every decision is exactly the documented rule.
    let mut inj = Injector::new(&plan, Link::MarketData);
    let (mut dropped, mut corrupt, mut dup, mut delayed, mut held, mut late) = (0, 0, 0, 0, 0, 0);
    let mut fates = Vec::new();
    for n in 0..20_000u64 {
        let step = n / 20;
        let fate = inj.fate(step);
        assert_eq!(
            fate,
            model_fate(&plan, Link::MarketData, step, n),
            "packet {n}"
        );
        assert!(!fate.partitioned, "a partition does not touch market data");
        dropped += fate.dropped as u32;
        corrupt += fate.corrupt.is_some() as u32;
        dup += fate.duplicate as u32;
        held += fate.reorder as u32;
        if fate.delay > 0 {
            delayed += 1;
            assert!(step >= 500 && fate.delay <= 5);
            late += (n >= 10_000 && !fate.dropped) as u32;
        }
        fates.push(fate);
    }
    assert_eq!(inj.packets(), 20_000);
    let in_rate = |count: u32, n: u32, per_mille: u32| {
        let expect = n * per_mille / 1000;
        count.abs_diff(expect) <= expect / 8 + 10
    };
    let survivors = 20_000 - dropped;
    assert!(in_rate(dropped, 20_000, 200), "{dropped}");
    assert!(in_rate(corrupt, survivors, 100), "{corrupt}");
    assert!(in_rate(dup, survivors, 150), "{dup}");
    assert!(in_rate(held, survivors, 80), "{held}");
    assert!(in_rate(delayed, survivors / 2, 120), "{delayed}");
    assert_eq!(late, delayed);
    // The log is exactly the fates that were not clean.
    let logged: Vec<_> = (0..20_000u64)
        .filter(|&n| !fates[n as usize].is_clean())
        .map(|n| FaultEvent::Packet {
            step: n / 20,
            link: Link::MarketData,
            packet: n,
            fate: fates[n as usize],
        })
        .collect();
    assert_eq!(inj.log(), &logged[..]);

    // Other links: the partition, and a link-specific drop.
    let mut to_standby = Injector::new(&plan, Link::ToStandby);
    let mut to_primary = Injector::new(&plan, Link::ToPrimary);
    for step in 0..300 {
        let (s, p) = (to_standby.fate(step), to_primary.fate(step));
        let cut = (100..200).contains(&step);
        assert_eq!(s.partitioned, cut);
        assert_eq!(s.is_clean(), !cut);
        assert_eq!(p.is_lost(), cut || step < 10);
        assert_eq!(p.dropped, step < 10);
    }
    assert!(Injector::new(&plan, Link::ExecReports).fate(0).is_clean());

    // Editing other faults leaves a fault's decisions alone: removing the
    // drop only reveals the packets it was hiding; a lower rate hits a
    // subset of the same packets; another seed gives other faults.
    let without_drop = plan.without(0);
    let lower_dup = {
        let mut p = plan.clone();
        p.faults[2].fault = md(|link| Fault::Duplicate {
            link,
            per_mille: 50,
        });
        p
    };
    let mut a = Injector::new(&without_drop, Link::MarketData);
    let mut b = Injector::new(&lower_dup, Link::MarketData);
    let mut other_seed = Injector::new(&plan.reseeded(43), Link::MarketData);
    let mut differ = 0;
    for (n, &fate) in fates.iter().enumerate() {
        let step = n as u64 / 20;
        let fa = a.fate(step);
        if !fate.dropped {
            assert_eq!(fa, fate, "packet {n}");
        }
        let fb = b.fate(step);
        assert!(!fb.duplicate || fate.duplicate, "packet {n}");
        assert_eq!(
            Fate {
                duplicate: false,
                ..fb
            },
            Fate {
                duplicate: false,
                ..fate
            }
        );
        differ += (other_seed.fate(step) != fate) as u32;
    }
    assert!(differ > 5_000, "{differ}");
}

// ---------------------------------------------------------------------------
// 2. The link delivers exactly what the plan says.

#[test]
fn w48_chaos_link_delivers_exactly_as_planned() {
    let plan = from_text(
        "seed=9 dup(md,1000)@2..3 delay(md,1000,1)@4..5 reorder(md,1000)@6..7 \
         drop(md,1000)@8..9 delay(md,1000,1)@10..11 reorder(md,1000)@12..13 \
         corrupt(md,1000)@14..15",
    );
    let mut link = ChaosLink::<u64>::new(&plan, Link::MarketData);
    let mut got = Vec::new();
    let mut per_step = Vec::new();
    for step in 0..17u64 {
        if step <= 14 && step != 13 {
            link.send(step, step);
        }
        got.clear();
        link.recv(step, &mut got);
        per_step.push(got.clone());
        if step == 4 || step == 6 {
            assert_eq!(link.in_flight(), 1, "step {step}");
        }
    }
    let expect: Vec<Vec<u64>> = vec![
        vec![0],
        vec![1],
        vec![2, 2],
        vec![3],
        vec![],
        vec![4, 5],
        vec![],
        vec![7, 6],
        vec![],
        vec![9],
        vec![],
        vec![10, 11],
        vec![],
        vec![12], // nothing followed it: released one step late
        vec![],   // 14: a u64 cannot be damaged: lost instead
        vec![],
        vec![],
    ];
    assert_eq!(per_step, expect);
    assert_eq!(link.in_flight(), 0);
    let steps: Vec<u64> = link.injector().log().iter().map(FaultEvent::step).collect();
    assert_eq!(steps, [2, 4, 6, 8, 10, 12, 14]);

    // A datagram is damaged in exactly one bit, the one the fate selects.
    let plan = from_text("seed=3 corrupt(md,1000)@0..64");
    let mut link = ChaosLink::<Vec<u8>>::new(&plan, Link::MarketData);
    for step in 0..64 {
        let sent: Vec<u8> = (0..16).map(|i| (i * 7 + step) as u8).collect();
        link.send(step, sent.clone());
        let mut out = Vec::new();
        link.recv(step, &mut out);
        assert_eq!(out.len(), 1);
        let flipped: Vec<usize> = (0..128)
            .filter(|&b| (sent[b / 8] ^ out[0][b / 8]) >> (b % 8) & 1 == 1)
            .collect();
        let Some(FaultEvent::Packet { fate, .. }) = link.injector().log().last() else {
            panic!("not logged");
        };
        assert_eq!(flipped, [fate.corrupt.unwrap() as usize % 128]);
    }
}

// ---------------------------------------------------------------------------
// 3. The feed client, one fault at a time, exactly.

/// Update `seq` of a hand-made feed: alternating sides, new levels, and
/// update 13 removes the level update 1 created.
fn update(seq: u64) -> L2Update {
    if seq == 13 {
        return L2Update {
            seq: Seq(13),
            side: Side::Buy,
            price: Price(99),
            qty: Qty(0),
        };
    }
    let (side, price) = if seq % 2 == 1 {
        (Side::Buy, 100 - seq as i64)
    } else {
        (Side::Sell, 200 + seq as i64)
    };
    L2Update {
        seq: Seq(seq),
        side,
        price: Price(price),
        qty: Qty(seq * 10),
    }
}

fn datagram(seqs: std::ops::RangeInclusive<u64>) -> Vec<u8> {
    let updates: Vec<L2Update> = seqs.map(update).collect();
    let mut packet = Vec::new();
    encode_incremental(&updates, &mut packet);
    let mut out = Vec::new();
    seal_packet(&packet, &mut out);
    out
}

fn heartbeat(next: u64) -> Vec<u8> {
    let mut out = Vec::new();
    seal_heartbeat(Seq(next), &mut out);
    out
}

fn truth_at(seq: u64) -> (Truth, Vec<L2Update>) {
    let published: Vec<L2Update> = (1..=16).map(update).collect();
    let mut t = Truth::default();
    t.advance(&published, Seq(seq));
    (t, published)
}

fn assert_live_at(c: &FeedClient, seq: u64) {
    assert_eq!(c.status(), FeedStatus::Live);
    let book = c.book().expect("Live serves its book");
    assert_eq!(book.last_seq(), Some(Seq(seq)));
    assert!(truth_at(seq).0.matches(book), "wrong book at {seq}");
}

fn assert_recovering(c: &FeedClient) {
    assert_eq!(c.status(), FeedStatus::Recovering);
    assert!(c.book().is_none(), "a Recovering client serves no book");
}

#[test]
fn w48_feed_client_recovers_from_each_fault_exactly() {
    let patience = 3;
    let mut c = FeedClient::new(4, patience);
    assert_live_at(&c, 0);

    // In order, then a duplicate packet: ignored.
    c.on_datagram(0, &datagram(1..=2));
    assert_live_at(&c, 2);
    c.on_datagram(0, &datagram(1..=2));
    assert_live_at(&c, 2);
    assert_eq!(c.stats().duplicates, 2);

    // A damaged datagram is a loss, never applied; the next packet exposes
    // the hole; the client waits `patience` steps for it, then asks.
    let mut damaged = datagram(3..=3);
    damaged[9] ^= 0x10; // a bit of the sequence number field
    c.on_datagram(1, &damaged);
    assert_eq!(c.stats().corrupt, 1);
    assert_live_at(&c, 2);
    c.on_datagram(2, &datagram(4..=4));
    assert_recovering(&c);
    assert_eq!(c.horizon(), Seq(4));
    for t in 2..2 + patience {
        assert!(!c.wants_snapshot(t), "step {t}");
    }
    assert!(c.wants_snapshot(2 + patience));
    assert!(!c.wants_snapshot(3 + patience), "one request at a time");
    c.on_snapshot(3 + patience, &truth_at(4).0.snapshot());
    assert_live_at(&c, 4);
    assert!(!c.wants_snapshot(4 + patience));

    // Reordered within the window: put back in order, no snapshot.
    c.on_datagram(10, &datagram(6..=6));
    assert_recovering(&c);
    c.on_datagram(10, &datagram(5..=5));
    assert_live_at(&c, 6);
    assert!(!c.wants_snapshot(10));

    // Tail loss: update 7 is lost before a lull; a heartbeat reveals it.
    c.on_datagram(20, &heartbeat(7));
    assert_live_at(&c, 6);
    c.on_datagram(21, &heartbeat(8));
    assert_recovering(&c);
    assert!(!c.wants_snapshot(21));
    assert!(c.wants_snapshot(21 + patience));
    c.on_snapshot(21 + patience, &truth_at(7).0.snapshot());
    assert_live_at(&c, 7);

    // A loss the reorder buffer gives up on puts the book out of sync: ask
    // at once. A snapshot that does not close the hole (update 11 lost
    // too) leaves it Recovering and asking again; the next one recovers.
    c.on_datagram(30, &datagram(12..=12));
    c.on_datagram(30, &datagram(16..=16));
    assert_eq!(c.stats().lost, 4, "8 given up for 12, 9..=11 for 16");
    assert_recovering(&c);
    assert!(c.wants_snapshot(30), "out of sync: no patience");
    c.on_snapshot(30, &truth_at(10).0.snapshot());
    assert_recovering(&c);
    assert!(c.wants_snapshot(30), "the hole is still open: ask again");
    c.on_snapshot(30, &truth_at(16).0.snapshot());
    assert_live_at(&c, 16);

    // A late, stale snapshot is ignored.
    c.on_snapshot(30, &truth_at(9).0.snapshot());
    assert_live_at(&c, 16);

    // Silence: nothing at all for more than `patience` steps, not even a
    // heartbeat — the client cannot vouch for its book; anything heard
    // again ends it.
    assert!(!c.wants_snapshot(30 + patience));
    assert_live_at(&c, 16);
    assert!(!c.wants_snapshot(31 + patience));
    assert_recovering(&c);
    c.on_datagram(32 + patience, &heartbeat(17));
    assert_live_at(&c, 16);

    let s = c.stats();
    assert_eq!(s.stale_snapshots, 1);
    assert_eq!(s.snapshots, 4);
    assert_eq!(s.snapshot_requests, 4);
    assert_eq!((s.heartbeats, s.datagrams), (3, 11));
    assert_eq!(s.silences, 2, "also while waiting for the first snapshot");
}

// ---------------------------------------------------------------------------
// 4. The feed under heavy faults: never Live with a wrong book.

#[test]
fn w48_feed_is_never_live_with_a_wrong_book() {
    // Liveness: a hole is closed within the client's patience plus the
    // snapshot round trip, however bad the line.
    let base = FeedConfig::default();
    let cfg = FeedConfig {
        live_within: Some(base.patience + base.snapshot_latency),
        ..base
    };
    let mut totals = sim::chaos::FeedClientStats::default();
    for seed in 1..=4u64 {
        // Everything at once, then a total blackout just before the lull
        // (tail loss), then quiet.
        let plan = heavy_feed(seed, cfg.steps).with(
            md(|link| Fault::Drop {
                link,
                per_mille: 1000,
            }),
            cfg.steps - 15..cfg.steps,
        );
        let mut sim = FeedSim::new(
            FeedConfig {
                flow_seed: seed,
                ..cfg
            },
            &plan,
        );
        let mut truth = Truth::default();
        let mut live = 0;
        while !sim.is_done() {
            sim.step().unwrap_or_else(|v| panic!("seed {seed}: {v:?}"));
            live += check_live(&mut truth, sim.published(), sim.client().book()) as u32;
        }
        // Converged once the faults stopped.
        let c = sim.client();
        assert_eq!(c.status(), FeedStatus::Live, "seed {seed}");
        assert_eq!(c.last_seq(), sim.publisher().last_seq());
        assert_eq!(
            c.book().unwrap().snapshot(),
            sim.publisher().snapshot(),
            "seed {seed}"
        );
        assert!(live > 300, "seed {seed}: live {live} steps");
        let s = c.stats();
        for (total, n) in [
            (&mut totals.corrupt, s.corrupt),
            (&mut totals.duplicates, s.duplicates),
            (&mut totals.lost, s.lost),
            (&mut totals.gaps, s.gaps),
            (&mut totals.snapshots, s.snapshots),
            (&mut totals.heartbeats, s.heartbeats),
        ] {
            *total += n;
        }
        assert!(s.snapshots >= 5 && s.silences >= 1, "seed {seed}: {s:?}");
        // The run checker agrees.
        let outcome = FeedSim::run(
            FeedConfig {
                flow_seed: seed,
                ..cfg
            },
            &plan,
        )
        .unwrap();
        assert_eq!(outcome.client, s);
        assert_eq!(outcome.fault_log, sim.fault_log());
    }
    let t = totals;
    assert!(
        t.corrupt > 40 && t.duplicates > 100 && t.lost > 100 && t.gaps > 20 && t.heartbeats > 100,
        "{t:?}"
    );
}

// ---------------------------------------------------------------------------
// 5. Slow consumers on the fan-out ring.

#[test]
fn w48_slow_consumer_is_lapped_and_resyncs_without_blocking_the_publisher() {
    let cfg = FanoutConfig::default();
    let plan = FaultPlan::new(5)
        .with(Fault::Stall { consumer: 1 }, 100..250)
        .with(Fault::Stall { consumer: 1 }, 300..380)
        .with(Fault::Stall { consumer: 2 }, 200..203);
    let mut sim = FanoutSim::new(cfg, &plan);
    let mut truths: Vec<Truth> = (0..3).map(|_| Truth::default()).collect();
    let mut live = [0u32; 3];
    while !sim.is_done() {
        sim.step().unwrap();
        for (i, truth) in truths.iter_mut().enumerate() {
            live[i] += check_live(truth, sim.published(), sim.consumer(i).book()) as u32;
        }
        // The writer never waits: everything is in the ring at once.
        assert_eq!(sim.ring().published(), sim.published().len() as u64);
    }
    let published = sim.published().len() as u64;
    let head = sim.publisher().snapshot();
    assert!(published > 1_000, "{published}");
    for i in 0..3 {
        let c = sim.consumer(i);
        assert_eq!(
            c.book().map(BookBuilder::snapshot),
            Some(head.clone()),
            "consumer {i}"
        );
        assert_eq!(c.backlog(), 0);
    }
    let [healthy, slow, blip] = [0, 1, 2].map(|i| sim.consumer(i).stats());
    // Healthy readers are never disturbed.
    assert_eq!((healthy.lags, healthy.gaps, healthy.resyncs), (0, 0, 0));
    assert_eq!(healthy.read, published);
    assert_eq!(live[0] as u64, sim.now());
    // A short stall stays within the ring: it just catches up.
    assert_eq!((blip.lags, blip.gaps, blip.resyncs), (0, 0, 0));
    assert_eq!(blip.read, published);
    // The long stalls are lapped twice and resync from the cell each time.
    assert_eq!(slow.lags, 2, "{slow:?}");
    assert!(slow.missed > 0 && slow.resyncs >= 2, "{slow:?}");
    assert_eq!(slow.read + slow.missed, published, "{slow:?}");
    assert_eq!(
        sim.fault_log(),
        [
            FaultEvent::Stall {
                step: 100,
                consumer: 1
            },
            FaultEvent::Stall {
                step: 200,
                consumer: 2
            },
            FaultEvent::Resume {
                step: 203,
                consumer: 2
            },
            FaultEvent::Resume {
                step: 250,
                consumer: 1
            },
            FaultEvent::Stall {
                step: 300,
                consumer: 1
            },
            FaultEvent::Resume {
                step: 380,
                consumer: 1
            },
        ]
    );
    assert_eq!(FanoutSim::run(cfg, &plan).unwrap(), sim.outcome());

    // On real threads: a reader parked before the first publish cannot slow
    // the publisher down — it publishes 10 rings' worth while the reader
    // sleeps. Woken, the reader is lapped exactly once and resyncs to the
    // publisher's final book. (The reader only reads after the publisher
    // is done, so the outcome does not depend on scheduling.)
    let capacity = 256usize;
    let (mut tx, ring) = marketdata::broadcast::<L2Update>(capacity);
    let cell = SnapshotCell::default();
    let consumer = RingConsumer::new(&ring, &cell);
    let (wake, parked) = mpsc::channel::<()>();
    let reader_cell = cell.clone();
    let reader = std::thread::spawn(move || {
        let mut consumer = consumer;
        parked.recv().unwrap();
        while consumer.poll(&reader_cell, 64) > 0 || consumer.status() != FeedStatus::Live {}
        consumer
    });
    let mut flow = OrderFlow::new(FlowConfig::default(), 77);
    let mut engine =
        engine::Engine::with_config(engine::EngineConfig::new(FlowConfig::default().spec));
    let mut publisher = L2Publisher::with_snapshot_interval(capacity as u64 / 4);
    let mut events = Vec::new();
    while tx.next_seq() < 10 * capacity as u64 {
        let cmd = flow.next_command();
        events.clear();
        engine.process(&cmd, &mut events);
        for ev in &events {
            flow.on_event(ev);
            if let Some(u) = publisher.on_event(ev) {
                tx.publish(u);
                if let Some(s) = publisher.periodic_snapshot() {
                    cell.store(s);
                }
            }
        }
    }
    cell.store(publisher.snapshot());
    wake.send(()).unwrap();
    let consumer = reader.join().unwrap();
    let s = consumer.stats();
    assert_eq!(s.lags, 1);
    assert_eq!(s.missed, tx.next_seq() - capacity as u64);
    assert_eq!(s.read, capacity as u64);
    assert_eq!(consumer.book().unwrap().snapshot(), publisher.snapshot());
}

// ---------------------------------------------------------------------------
// 6. Failover: crash or partition the primary mid-stream.

/// Steps the simulation to the end, checking after every step that the
/// surviving journal holds every acknowledged command, unchanged, and that
/// a deposed primary never leads again.
fn run_failover(cfg: FailoverConfig, plan: &FaultPlan) -> (FailoverSim, Vec<Leader>) {
    let mut sim = FailoverSim::new(cfg, plan);
    let mut leaders = Vec::new();
    let mut verified = 0u64;
    while !sim.is_done() {
        assert!(sim.now() < cfg.max_steps, "{plan}: stuck");
        sim.step().unwrap_or_else(|v| panic!("{plan}: {v:?}"));
        let leader = sim.leader();
        if leaders.contains(&Leader::Promoted) {
            assert_eq!(leader, Leader::Promoted, "{plan}: leadership went back");
        }
        leaders.push(leader);
        let journal = sim.surviving_journal().unwrap();
        let acked = sim.acked();
        assert!(
            journal.last_seq().unwrap_or_default() >= acked,
            "{plan}: acked lost"
        );
        for frame in journal
            .iter_from(Seq(verified + 1))
            .take((acked.0 - verified) as usize)
        {
            let frame = frame.unwrap();
            assert_eq!(
                frame.command().unwrap(),
                sim.commands()[frame.seq.0 as usize - 1]
            );
        }
        verified = verified.max(acked.0);
    }
    (sim, leaders)
}

fn lossy_replication(seed: u64) -> FaultPlan {
    from_text(&format!(
        "seed={seed} drop(to-standby,50)@0..2000 dup(to-standby,30)@0..2000 \
         corrupt(to-standby,30)@0..2000 delay(to-standby,60,4)@0..2000 \
         drop(to-primary,50)@0..2000 delay(to-primary,60,4)@0..2000"
    ))
}

#[test]
fn w48_failover_loses_no_acknowledged_command() {
    let cfg = FailoverConfig::default();
    let n = Seq(cfg.commands as u64);
    // The primary crashes at a seeded step, under lossy replication.
    for seed in 1..=6u64 {
        let crash = types::Rng::new(seed).range(60, 400);
        let plan = lossy_replication(seed).with(Fault::CrashPrimary, crash..crash + 1);
        let (sim, leaders) = run_failover(cfg, &plan);
        let out = sim.outcome();
        assert_eq!(out.crashed_at, Some(crash), "{plan}");
        let promoted = out.promoted_at.expect("the standby took over");
        assert!(promoted >= crash, "{plan}");
        assert!(
            out.failover_steps.unwrap() <= cfg.lease_steps + 6,
            "{plan}: {out:?}"
        );
        assert!(out.acked_at_failover <= out.survived, "{plan}: {out:?}");
        assert!(
            out.survived.0 >= (crash - 8) * 2,
            "{plan}: little replicated: {out:?}"
        );
        // Nobody led between the crash and the promotion.
        assert!(leaders[crash as usize - 1..promoted as usize - 1]
            .iter()
            .all(|&l| l == Leader::None));
        assert_eq!(
            (out.final_seq, out.final_hash),
            (n, sim.expected_hash(n)),
            "{plan}"
        );
        let st = out.standby;
        assert!(
            st.gaps + st.corrupt > 0 && st.duplicates > 0,
            "{plan}: {st:?}"
        );
        // The run checker agrees, and replays the same.
        assert_eq!(FailoverSim::run(cfg, &plan).unwrap(), out);
    }

    // A partition longer than the lease: the primary steps down before the
    // standby takes over, and stays down after the network heals.
    let plan = FaultPlan::new(7).with(Fault::Partition, 150..260);
    let (sim, leaders) = run_failover(cfg, &plan);
    let out = sim.outcome();
    let promoted = out.promoted_at.unwrap() as usize;
    assert!(
        promoted > 150 && promoted < 150 + 2 * cfg.lease_steps as usize,
        "{out:?}"
    );
    assert_eq!(leaders[140], Leader::Primary);
    assert_eq!(
        leaders[promoted - 2],
        Leader::None,
        "a gap between the two leases"
    );
    assert!(
        out.resent > 0,
        "the old primary sequenced commands that did not survive"
    );
    assert_eq!((out.final_seq, out.final_hash), (n, sim.expected_hash(n)));

    // A short partition and an asymmetric one (the standby hears the
    // primary, not the reverse): no failover; order entry just pauses.
    for plan in [
        FaultPlan::new(8).with(Fault::Partition, 150..160),
        FaultPlan::new(9).with(
            Fault::Drop {
                link: Link::ToPrimary,
                per_mille: 1000,
            },
            150..230,
        ),
    ] {
        let (sim, leaders) = run_failover(cfg, &plan);
        let out = sim.outcome();
        assert_eq!(out.promoted_at, None, "{plan}");
        assert!(
            leaders.contains(&Leader::None) || plan.faults[0].to == 160,
            "{plan}"
        );
        assert_eq!((out.final_seq, out.final_hash), (n, sim.expected_hash(n)));
    }
}

// ---------------------------------------------------------------------------
// 7. A faulty venue under the w23 scenario runner.

type Shared<T> = Rc<RefCell<T>>;

/// The exchange, with a record of everything it really sent.
struct Tap {
    inner: InProcess,
    reports: Shared<Vec<Delivery>>,
    updates: Shared<Vec<L2Update>>,
}

impl Venue for Tap {
    fn send(&mut self, account: AccountId, cmd: &Command) {
        self.inner.send(account, cmd);
    }
    fn flush(&mut self, deliveries: &mut Vec<Delivery>, market_data: &mut Vec<L2Update>) {
        let (d0, m0) = (deliveries.len(), market_data.len());
        self.inner.flush(deliveries, market_data);
        self.reports
            .borrow_mut()
            .extend_from_slice(&deliveries[d0..]);
        self.updates
            .borrow_mut()
            .extend_from_slice(&market_data[m0..]);
    }
    fn market_snapshot(&mut self) -> L2Snapshot {
        self.inner.market_snapshot()
    }
}

/// An agent, with a record of every report it received.
struct Recorded {
    agent: Box<dyn Agent>,
    got: Shared<Vec<Delivery>>,
}

impl Agent for Recorded {
    fn name(&self) -> &str {
        self.agent.name()
    }
    fn act(&mut self, ctx: &mut AgentCtx<'_>) {
        self.agent.act(ctx);
    }
    fn on_event(&mut self, d: &Delivery) {
        self.got.borrow_mut().push(*d);
        self.agent.on_event(d);
    }
}

struct Market {
    runner: Runner<ChaosVenue<Tap>>,
    reports: Shared<Vec<Delivery>>,
    updates: Shared<Vec<L2Update>>,
    received: Vec<Shared<Vec<Delivery>>>,
}

fn chaos_market(seed: u64, plan: &FaultPlan) -> Market {
    let cfg = ScenarioConfig {
        seed,
        ..ScenarioConfig::default()
    };
    let (reports, updates) = (Shared::default(), Shared::default());
    let tap = Tap {
        inner: InProcess::new(&cfg),
        reports: reports.clone(),
        updates: updates.clone(),
    };
    let mut runner = Runner::with_venue(ChaosVenue::new(tap, plan), &cfg);
    let maker = |half_spread, qty| -> Box<dyn Agent> {
        Box::new(MakerAgent::new(MakerConfig {
            half_spread_ticks: half_spread,
            qty: Qty(qty),
            lots_per_tick: 4,
            max_position: 60,
            ..MakerConfig::default()
        }))
    };
    let mut agents: Vec<Box<dyn Agent>> = vec![maker(2, 10), maker(3, 25)];
    for slippage in [None, Some(0), Some(2)] {
        agents.push(Box::new(NoiseAgent::new(NoiseConfig {
            p_trade: 0.6,
            max_qty: 8,
            slippage_ticks: slippage,
            ..NoiseConfig::default()
        })));
    }
    agents.push(Box::new(MomentumAgent::new(MomentumConfig::default())));
    let mut received = Vec::new();
    for agent in agents {
        let got = Shared::default();
        received.push(Rc::clone(&got));
        runner.add_agent(Box::new(Recorded { agent, got }));
    }
    Market {
        runner,
        reports,
        updates,
        received,
    }
}

/// Every kind of venue damage, over the first `until` flushes.
fn venue_faults(seed: u64, until: u64) -> FaultPlan {
    from_text(&format!(
        "{} drop(exec,30)@0..{until} corrupt(exec,10)@0..{until} dup(exec,40)@0..{until} \
         delay(exec,40,8)@0..{until}",
        heavy_feed(seed, until)
    ))
}

/// Runs `steps` steps, checking after each that every agent has received
/// exactly a prefix of its true reports and that the runner's book, when
/// synced, is the true book at its sequence number.
fn run_checked(m: &mut Market, steps: u64, truth: &mut Truth) {
    for _ in 0..steps {
        m.runner.step();
        let reports = m.reports.borrow();
        for (i, got) in m.received.iter().enumerate() {
            let account = AccountId(i as u32 + 1);
            let mine = reports.iter().filter(|d| d.account == account);
            let got = got.borrow();
            assert!(
                mine.take(got.len()).eq(got.iter()),
                "account {account:?}: not a prefix of its reports"
            );
        }
        let book = m.runner.book();
        if book.is_synced() {
            check_live(truth, &m.updates.borrow(), Some(book));
        }
    }
}

#[test]
fn w48_chaos_venue_keeps_agents_consistent() {
    for seed in [3u64, 11] {
        let agents = 6;
        let (faulty, clean) = (600u64, 200u64);
        let plan = venue_faults(seed, faulty * agents);
        let mut m = chaos_market(seed, &plan);
        let mut truth = Truth::default();
        run_checked(&mut m, faulty, &mut truth);
        run_checked(&mut m, clean, &mut truth);

        let venue = m.runner.venue();
        assert_eq!(venue.in_flight(), 0, "seed {seed}: everything delivered");
        let s = venue.stats();
        assert_eq!(s.delivered, s.reports);
        assert!(
            s.resets > 5 && s.stalls > 5 && s.duplicates_dropped > 5,
            "{s:?}"
        );
        // Every report reached its owner, exactly once, in order.
        let reports = m.reports.borrow();
        for (i, got) in m.received.iter().enumerate() {
            let account = AccountId(i as u32 + 1);
            let mine: Vec<Delivery> = reports
                .iter()
                .filter(|d| d.account == account)
                .copied()
                .collect();
            assert_eq!(*got.borrow(), mine, "seed {seed} account {account:?}");
        }
        // Inventories are conserved and agree with pre-trade risk.
        let rep = m.runner.report();
        let (mut pos, mut cash) = (0i64, 0i128);
        for a in &rep.agents {
            pos += a.inventory.position;
            cash += a.inventory.cash;
            let exposure = venue
                .inner()
                .inner
                .risk()
                .exposure(a.account, InstrumentId(1));
            assert_eq!(exposure.position, a.inventory.position, "seed {seed} {a:?}");
        }
        assert_eq!((pos, cash), (0, 0), "seed {seed}");
        assert!(rep.trades > 300, "seed {seed}: {rep:?}");
        // Week 45's books, kept from what each agent received, agree: same
        // positions, and the PnL of a closed market sums to zero.
        let mut books = Positions::new(PostTradeLimits::UNLIMITED);
        for (i, got) in m.received.iter().enumerate() {
            for d in got.borrow().iter() {
                if let Some(leg) = d.fill() {
                    let fill = Fill {
                        order: leg.order,
                        account: AccountId(i as u32 + 1),
                        instrument: InstrumentId(1),
                        side: leg.side,
                        price: leg.price,
                        qty: leg.qty,
                    };
                    books.apply_fill(&fill).unwrap();
                }
            }
        }
        let mark = venue.inner().inner.engine().last_trade_price().unwrap();
        books.set_mark(InstrumentId(1), mark, |_, _| {});
        let mut total = 0i128;
        for a in &rep.agents {
            let p = books
                .position(a.account, InstrumentId(1))
                .unwrap_or_default();
            assert_eq!(p.qty(), a.inventory.position, "seed {seed} {a:?}");
            let pnl = books.account(a.account).map_or(0, |x| x.total());
            assert_eq!(pnl, a.inventory.pnl(mark), "seed {seed} {a:?}");
            total += pnl;
        }
        assert_eq!(total, 0, "seed {seed}");
        // The book recovered from its gaps and equals the engine's.
        let book = m.runner.book();
        assert!(
            book.is_synced() && book.gaps() > 10,
            "seed {seed}: {}",
            book.gaps()
        );
        let engine = venue.inner().inner.engine();
        for side in [Side::Buy, Side::Sell] {
            let mut levels = Vec::new();
            engine.levels(side, usize::MAX, &mut levels);
            let levels: Vec<Level> = levels
                .iter()
                .map(|l| Level {
                    price: l.price,
                    qty: l.qty,
                })
                .collect();
            assert_eq!(book.depth(side, usize::MAX), levels, "seed {seed}");
        }
        assert!(!engine.book().is_crossed());
    }
}

// ---------------------------------------------------------------------------
// 8. Same seed, same run; a failing seed replays exactly and shrinks.

fn venue_run(seed: u64, plan: &FaultPlan) -> (ScenarioReport, Vec<FaultEvent>, u64) {
    let mut m = chaos_market(seed, plan);
    m.runner.run(400);
    let v = m.runner.venue();
    (
        m.runner.report(),
        v.fault_log(),
        v.inner().inner.engine().state_hash(),
    )
}

#[test]
fn w48_same_seed_same_run_and_a_failing_seed_replays_and_shrinks() {
    // Same plan, same outcome and fault log; another seed, other faults.
    let feed = FeedConfig::default();
    let plan = heavy_feed(21, feed.steps);
    let a = FeedSim::run(feed, &plan).unwrap();
    assert_eq!(FeedSim::run(feed, &plan).unwrap(), a);
    let b = FeedSim::run(feed, &plan.reseeded(22)).unwrap();
    assert_ne!(a.fault_log, b.fault_log);
    assert_eq!(
        (a.updates, a.book.clone()),
        (b.updates, b.book.clone()),
        "same market"
    );

    let fanout = FanoutConfig::default();
    let plan = FaultPlan::new(1).with(Fault::Stall { consumer: 0 }, 50..200);
    assert_eq!(
        FanoutSim::run(fanout, &plan).unwrap(),
        FanoutSim::run(fanout, &plan).unwrap()
    );

    let failover = FailoverConfig::default();
    let plan = lossy_replication(31).with(Fault::CrashPrimary, 222..223);
    let a = FailoverSim::run(failover, &plan).unwrap();
    assert_eq!(FailoverSim::run(failover, &plan).unwrap(), a);
    let b = FailoverSim::run(failover, &plan.reseeded(32)).unwrap();
    assert_ne!(a.fault_log, b.fault_log);
    assert_ne!((a.survived, a.standby), (b.survived, b.standby));

    let plan = venue_faults(41, 2_400);
    assert_eq!(venue_run(5, &plan), venue_run(5, &plan));
    assert_ne!(venue_run(5, &plan).1, venue_run(5, &plan.reseeded(42)).1);

    // The workflow. Say the failover SLO is one lease: the standby must
    // lead within `lease_steps` of the crash. Some seeds break it. Sweep,
    // report the first failure as text, replay it from the text, shrink it.
    let slo = failover.lease_steps;
    let schedule = lossy_replication(0).with(Fault::CrashPrimary, 222..223);
    let fails = |p: &FaultPlan| match FailoverSim::run(failover, p) {
        Ok(out) => out.failover_steps.is_some_and(|steps| steps > slo),
        Err(_) => true,
    };
    let failing = (1..=40)
        .map(|seed| schedule.reseeded(seed))
        .find(|p| fails(p))
        .expect("some seed breaks a one-lease failover SLO");
    assert!(
        (1..=40).any(|seed| !fails(&schedule.reseeded(seed))),
        "the SLO should hold for some seeds"
    );
    let report = failing.to_string();
    let replayed: FaultPlan = report.parse().unwrap();
    assert_eq!(replayed, failing);
    let original = FailoverSim::run(failover, &failing).unwrap();
    assert_eq!(FailoverSim::run(failover, &replayed).unwrap(), original);

    let minimal = shrink_plan(&replayed, fails);
    assert!(fails(&minimal));
    for i in 0..minimal.faults.len() {
        assert!(!fails(&minimal.without(i)), "{minimal} is not 1-minimal");
    }
    // What it takes: the crash, and a frame the primary sent before dying
    // that arrives late — the standby takes it as a sign of life and
    // renews its promise not to take over.
    assert!(minimal
        .faults
        .iter()
        .any(|s| s.fault == Fault::CrashPrimary));
    assert!(
        minimal.faults.iter().any(|s| matches!(
            s.fault,
            Fault::Delay {
                link: Link::ToStandby,
                ..
            }
        )),
        "{minimal}"
    );
    let span = |p: &FaultPlan| p.faults.iter().map(|s| s.to - s.from).sum::<u64>();
    assert!(span(&minimal) < span(&failing) / 10, "{minimal}");
    println!(
        "failing plan: {report}\nfailover took {:?} steps\nminimal plan: {minimal}",
        original.failover_steps
    );
}
