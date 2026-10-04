//! Week 12: engine v1 wiring — sequencer (journal) → engine → L2 market
//! data.

use engine::testkit::{FlowConfig, OrderFlow};
use engine::{Engine, EngineConfig, LevelInfo, OrderBook, Pipeline, SeqMismatch};
use journal::{replay, Journal, StateMachine, FIRST_SEQ};
use marketdata::{BookBuilder, L2Snapshot, L2Update, Level, UpdateOutcome};
use types::{Command, Event, NewOrder, OrderId, Price, Qty, RejectReason, Seq, Side};

/// A flow with plenty of rejections (malformed orders, unknown cancels).
fn noisy(seed: u64) -> FlowConfig {
    FlowConfig {
        seed,
        p_invalid: 0.10,
        ..FlowConfig::default()
    }
}

fn levels(e: &Engine, side: Side) -> Vec<Level> {
    let mut v: Vec<LevelInfo> = Vec::new();
    e.levels(side, usize::MAX, &mut v);
    v.iter()
        .map(|l| Level {
            price: l.price,
            qty: l.qty,
        })
        .collect()
}

#[test]
fn w12_pipeline_journals_every_command_including_rejected() {
    // Exact output of a few hand-written commands.
    let mut p = Pipeline::new(EngineConfig::default());
    let out = p
        .process(&Command::New(NewOrder::limit(1, Side::Sell, 101, 5)))
        .unwrap();
    assert_eq!(out.seq, Seq(1));
    assert_eq!(
        out.events,
        &[
            Event::Accepted { id: OrderId(1) },
            Event::Rested {
                id: OrderId(1),
                side: Side::Sell,
                price: Price(101),
                qty: Qty(5)
            }
        ]
    );
    assert_eq!(
        out.updates,
        &[L2Update {
            seq: Seq(1),
            side: Side::Sell,
            price: Price(101),
            qty: Qty(5)
        }]
    );
    let out = p
        .process(&Command::New(NewOrder::limit(2, Side::Buy, 102, 3)))
        .unwrap();
    assert_eq!(out.seq, Seq(2));
    assert_eq!(out.events.len(), 2, "accepted + one trade, fully filled");
    assert_eq!(
        out.updates,
        &[L2Update {
            seq: Seq(2),
            side: Side::Sell,
            price: Price(101),
            qty: Qty(2)
        }]
    );
    let out = p.process(&Command::Cancel { id: OrderId(9) }).unwrap();
    assert_eq!(
        out.seq,
        Seq(3),
        "a rejected command still takes a sequence number"
    );
    assert_eq!(
        out.events,
        &[Event::Rejected {
            id: OrderId(9),
            reason: RejectReason::UnknownOrder
        }]
    );
    assert!(out.updates.is_empty());
    let out = p
        .process(&Command::New(NewOrder::limit(3, Side::Buy, 100, 0)))
        .unwrap();
    assert_eq!((out.seq, out.updates.len()), (Seq(4), 0));

    // A long noisy stream: every command is in the journal, in order, with
    // the sequence number the pipeline reported — rejected ones included.
    let mut p = Pipeline::new(noisy(12).engine_config());
    let mut flow = OrderFlow::new(noisy(12));
    let (mut sent, mut rejected) = (Vec::new(), 0);
    for i in 0..20_000u64 {
        let cmd = flow.next_command();
        let out = p.process(&cmd).unwrap();
        assert_eq!(out.seq, Seq(i + 1));
        rejected += out
            .events
            .iter()
            .filter(|e| matches!(e, Event::Rejected { .. }))
            .count();
        flow.observe(&cmd, out.events);
        sent.push(cmd);
    }
    assert!(
        rejected > 1_000,
        "the stream must exercise rejections ({rejected})"
    );
    let j = p.journal();
    assert_eq!((j.len(), j.last_seq()), (20_000, Some(Seq(20_000))));
    assert_eq!(p.engine().seq(), Seq(20_000));
    let journaled: Vec<(Seq, Command)> = j
        .iter()
        .map(|f| {
            let f = f.unwrap();
            (f.seq, f.command().unwrap())
        })
        .collect();
    let expected: Vec<(Seq, Command)> = sent
        .iter()
        .enumerate()
        .map(|(i, c)| (Seq(i as u64 + 1), *c))
        .collect();
    assert!(
        journaled == expected,
        "journal differs from the commands sent"
    );
}

#[test]
fn w12_journal_replay_reproduces_engine_state_hash() {
    for seed in [1u64, 7, 31] {
        let cfg = if seed == 1 {
            noisy(seed)
        } else {
            FlowConfig::randomized(seed)
        };
        let mut p = Pipeline::new(cfg.engine_config());
        let mut flow = OrderFlow::new(cfg);
        let mut hashes = vec![p.engine().state_hash()];
        for _ in 0..15_000 {
            let cmd = flow.next_command();
            let out = p.process(&cmd).unwrap();
            flow.observe(&cmd, out.events);
            hashes.push(p.engine().state_hash());
        }

        // Full replay into a fresh engine.
        let mut fresh = Engine::with_config(cfg.engine_config());
        let done = replay(p.journal(), FIRST_SEQ, &mut fresh).unwrap();
        assert_eq!(done.applied, 15_000);
        assert_eq!(
            StateMachine::state_hash(&fresh),
            p.engine().state_hash(),
            "seed {seed}"
        );

        // Every prefix: the replayed engine matches the live one after each
        // command, not just at the end.
        let mut step = Engine::with_config(cfg.engine_config());
        for frame in p.journal().iter() {
            let frame = frame.unwrap();
            step.apply(&frame.command().unwrap());
            assert_eq!(
                step.state_hash(),
                hashes[frame.seq.0 as usize],
                "seed {seed}, seq {}",
                frame.seq.0
            );
        }

        // The raw journal bytes alone are enough.
        let (copy, damage) = Journal::recover(p.journal().as_bytes().to_vec(), FIRST_SEQ);
        assert!(damage.is_none());
        let mut from_bytes = Engine::with_config(cfg.engine_config());
        replay(&copy, FIRST_SEQ, &mut from_bytes).unwrap();
        assert_eq!(from_bytes.state_hash(), p.engine().state_hash());
    }
}

#[test]
fn w12_l2_from_publisher_matches_engine_book() {
    for seed in [3u64, 12, 99] {
        let cfg = FlowConfig::randomized(seed);
        let mut p = Pipeline::new(cfg.engine_config());
        let mut flow = OrderFlow::new(cfg);
        let mut client = BookBuilder::new();
        client.apply_snapshot(&L2Snapshot::default());
        let mut last_feed_seq = 0;
        for i in 0..10_000 {
            let cmd = flow.next_command();
            let out = p.process(&cmd).unwrap();
            flow.observe(&cmd, out.events);
            for u in out.updates {
                assert_eq!(u.seq.0, last_feed_seq + 1, "feed sequence is dense");
                last_feed_seq = u.seq.0;
                assert_eq!(client.apply_update(u), UpdateOutcome::Applied);
            }
            if i % 97 == 0 || i == 9_999 {
                let e = p.engine();
                for side in [Side::Buy, Side::Sell] {
                    let book = levels(e, side);
                    assert_eq!(client.depth(side, usize::MAX), book, "seed {seed} cmd {i}");
                    assert_eq!(p.publisher().snapshot().levels(side), &book[..]);
                }
            }
        }
        assert!(
            p.engine().book().len() > 0,
            "the book must not be trivially empty"
        );
    }

    // A pipeline wired onto an engine that already holds orders starts its
    // publisher from that book.
    let mut e = Engine::with_config(EngineConfig::default());
    let mut out = Vec::new();
    for (id, side, price) in [(1, Side::Buy, 99), (2, Side::Buy, 99), (3, Side::Sell, 103)] {
        e.process(&Command::New(NewOrder::limit(id, side, price, 4)), &mut out);
    }
    assert_eq!(
        Pipeline::with_journal(Journal::new(), e.clone()).unwrap_err(),
        SeqMismatch {
            journal_next: Seq(1),
            engine_seq: Seq(3)
        }
    );
    let p = Pipeline::with_journal(Journal::starting_at(Seq(4)), e).unwrap();
    let snap = p.publisher().snapshot();
    assert_eq!(snap.levels(Side::Buy), &levels(p.engine(), Side::Buy)[..]);
    assert_eq!(snap.levels(Side::Sell), &levels(p.engine(), Side::Sell)[..]);
    assert_eq!(snap.best_bid().unwrap().qty, Qty(8));
}
