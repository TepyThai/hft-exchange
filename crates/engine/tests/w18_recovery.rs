//! Week 18: a durable engine — snapshots, restore and crash recovery.

use engine::testkit::{FlowConfig, OrderFlow};
use engine::{recover, Engine, EngineState, OrderBook, RestingOrder, RestoreError, StateError};
use journal::{
    FileStorage, JournalOptions, ManualClock, MemStorage, SnapshotStore, Snapshottable, SyncPolicy,
    SystemClock,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use types::{Command, OrderId, Price, Seq, Side};

/// A fresh directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "hft-w18-engine-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn flow_cfg(seed: u64) -> FlowConfig {
    FlowConfig {
        seed,
        p_invalid: 0.05,
        ..FlowConfig::default()
    }
}

/// `n` commands of the w12 generator, fed back through a shadow engine so
/// cancels and reduces hit live orders.
fn stream(cfg: FlowConfig, n: usize) -> Vec<Command> {
    let mut flow = OrderFlow::new(cfg);
    let mut shadow = Engine::with_config(cfg.engine_config());
    let mut out = Vec::new();
    (0..n)
        .map(|_| {
            let cmd = flow.next_command();
            out.clear();
            shadow.process(&cmd, &mut out);
            flow.observe(&cmd, &out);
            cmd
        })
        .collect()
}

/// State hash after each prefix of `cmds` (`hashes[k]` = after `k`).
fn hashes(cfg: &FlowConfig, cmds: &[Command]) -> Vec<u64> {
    let mut e = Engine::with_config(cfg.engine_config());
    let mut out = Vec::new();
    let mut v = vec![e.state_hash()];
    for c in cmds {
        out.clear();
        e.process(c, &mut out);
        v.push(e.state_hash());
    }
    v
}

#[test]
fn w18_recovery_equals_live_state_hash() {
    let dir = TempDir::new("live");
    let store = SnapshotStore::open(dir.join("snapshots")).unwrap();
    let log = dir.join("journal.log");
    let cfg = flow_cfg(18);
    let cmds = stream(cfg, 40_000);
    let expect = hashes(&cfg, &cmds);
    let opts = JournalOptions {
        buffer_bytes: 4 * 1024,
        ..JournalOptions::with_sync(SyncPolicy::EveryN(32))
    };
    let open = || {
        let storage = FileStorage::open(&log).unwrap();
        recover(
            cfg.engine_config(),
            storage,
            Some(&store),
            opts,
            SystemClock::new(),
        )
        .unwrap()
    };

    // First start: nothing on disk.
    let (mut live, report) = open();
    assert_eq!(
        (report.snapshot, report.replayed, report.applied),
        (None, 0, Seq(0))
    );
    for (i, c) in cmds[..30_000].iter().enumerate() {
        live.process(c).unwrap();
        if i + 1 == 10_000 || i + 1 == 20_000 {
            assert_eq!(live.checkpoint(&store).unwrap(), Seq(i as u64 + 1));
        }
    }
    let durable = live.journal().durable_seq();
    assert!(durable.0 >= 30_000 - 32);
    // Crash: the process dies, the write buffer with it.
    drop(live);

    let (mut back, report) = open();
    let at = report.applied;
    assert!(
        at >= durable && at <= Seq(30_000),
        "recovered to {at:?}, durable {durable:?}"
    );
    assert_eq!(report.snapshot, Some(Seq(20_000)));
    assert_eq!(report.replayed, at.0 - 20_000);
    assert_eq!(back.engine().state_hash(), expect[at.0 as usize]);
    assert_eq!(back.journal().next_seq(), Seq(at.0 + 1));

    // Carry on from where it recovered: same commands, same state.
    for c in &cmds[at.0 as usize..] {
        back.process(c).unwrap();
    }
    assert_eq!(back.engine().state_hash(), expect[40_000]);
    back.journal_mut().sync().unwrap();
    drop(back);
    let (again, report) = open();
    assert_eq!(report.applied, Seq(40_000));
    assert_eq!(again.engine().state_hash(), expect[40_000]);

    // Power loss (journal in memory storage): exactly the synced commands
    // come back.
    let storage = MemStorage::new();
    let mem_store = SnapshotStore::open(dir.join("mem-snapshots")).unwrap();
    let opts = JournalOptions::with_sync(SyncPolicy::EveryN(100));
    let clock = ManualClock::new(0);
    let (mut live, _) = recover(
        cfg.engine_config(),
        storage.clone(),
        Some(&mem_store),
        opts,
        clock.clone(),
    )
    .unwrap();
    for (i, c) in cmds[..12_345].iter().enumerate() {
        live.process(c).unwrap();
        if i + 1 == 5_000 {
            live.checkpoint(&mem_store).unwrap();
        }
    }
    let durable = live.journal().durable_seq();
    assert_eq!(durable, Seq(12_300));
    let (back, report) = recover(
        cfg.engine_config(),
        storage.power_loss(),
        Some(&mem_store),
        opts,
        clock,
    )
    .unwrap();
    assert_eq!(
        (report.snapshot, report.applied),
        (Some(Seq(5_000)), durable)
    );
    assert_eq!(back.engine().state_hash(), expect[12_300]);
}

#[test]
fn w18_snapshot_plus_tail_equals_full_replay() {
    let cfg = FlowConfig::randomized(1818);
    let cmds = stream(cfg, 20_000);
    let expect = hashes(&cfg, &cmds);

    // Restore at many points: same hash, same bytes, same future events
    // (time priority survived).
    let mut live = Engine::with_config(cfg.engine_config());
    let mut out = Vec::new();
    let mut snapshots = Vec::new();
    for (i, c) in cmds.iter().enumerate() {
        out.clear();
        live.process(c, &mut out);
        let seq = i + 1;
        if seq % 2_500 == 0 || seq == 1 || seq == 777 {
            let state = live.snapshot_state();
            assert_eq!(state.seq, Seq(seq as u64));
            assert_eq!(state.orders.len(), live.book().len());
            let mut bytes = Vec::new();
            state.encode(&mut bytes);
            assert_eq!(EngineState::decode(&bytes).unwrap(), state);
            let restored = Engine::restore(cfg.engine_config(), &state).unwrap();
            assert_eq!(restored.state_hash(), live.state_hash(), "seq {seq}");
            assert_eq!(restored.snapshot_state(), state);
            if seq < 19_000 {
                let (mut a, mut b) = (live.clone(), restored);
                let (mut ea, mut eb) = (Vec::new(), Vec::new());
                for c in &cmds[seq..seq + 1_000] {
                    ea.clear();
                    eb.clear();
                    a.process(c, &mut ea);
                    b.process(c, &mut eb);
                    assert_eq!(ea, eb, "events diverge after restoring at {seq}");
                }
            }
            snapshots.push((Seq(seq as u64), bytes));
        }
    }

    // Recovery from a journal file with exactly one of those snapshots (or
    // none) equals the full replay.
    let dir = TempDir::new("tail");
    let log = dir.join("journal.log");
    let opts = JournalOptions {
        index_stride: 256,
        ..JournalOptions::with_sync(SyncPolicy::Never)
    };
    {
        let (mut p, _) = recover(
            cfg.engine_config(),
            FileStorage::open(&log).unwrap(),
            None,
            opts,
            SystemClock::new(),
        )
        .unwrap();
        for c in &cmds {
            p.process(c).unwrap();
        }
        p.journal_mut().sync().unwrap();
    }
    let (full, report) = recover(
        cfg.engine_config(),
        FileStorage::open(&log).unwrap(),
        None,
        opts,
        SystemClock::new(),
    )
    .unwrap();
    assert_eq!((report.snapshot, report.replayed), (None, 20_000));
    assert_eq!(full.engine().state_hash(), expect[20_000]);
    for (k, (seq, bytes)) in snapshots.iter().enumerate() {
        let store = SnapshotStore::open(dir.join(&format!("snapshots-{k}"))).unwrap();
        store.save(*seq, bytes).unwrap();
        let (p, report) = recover(
            cfg.engine_config(),
            FileStorage::open(&log).unwrap(),
            Some(&store),
            opts,
            SystemClock::new(),
        )
        .unwrap();
        assert_eq!(report.snapshot, Some(*seq));
        assert_eq!(report.replayed, 20_000 - seq.0);
        assert_eq!(
            p.engine().state_hash(),
            full.engine().state_hash(),
            "snapshot at {seq:?}"
        );
    }

    // States no engine could be in are refused; so are malformed bytes.
    let o = RestingOrder::new;
    let state = |orders: Vec<RestingOrder>| EngineState {
        seq: Seq(9),
        last_trade: Some(Price(100)),
        orders,
    };
    let c = cfg.engine_config();
    let bad = [
        (
            vec![o(1, Side::Buy, 99, 1), o(2, Side::Buy, 100, 1)],
            RestoreError::OutOfOrder { index: 1 },
        ),
        (
            vec![o(1, Side::Sell, 101, 1), o(2, Side::Buy, 99, 1)],
            RestoreError::OutOfOrder { index: 1 },
        ),
        (
            vec![o(1, Side::Buy, 101, 1), o(2, Side::Sell, 101, 1)],
            RestoreError::Crossed,
        ),
        (
            vec![o(1, Side::Buy, 99, 1), o(1, Side::Sell, 101, 1)],
            RestoreError::DuplicateId(OrderId(1)),
        ),
        (
            vec![o(1, Side::Buy, 99, 0)],
            RestoreError::ZeroQty(OrderId(1)),
        ),
    ];
    for (orders, err) in bad {
        assert_eq!(Engine::restore(c, &state(orders)).unwrap_err(), err);
    }
    let good = state(vec![
        o(1, Side::Buy, 99, 1),
        o(2, Side::Buy, 99, 2),
        o(3, Side::Sell, 101, 1),
    ]);
    let mut e = Engine::restore(c, &good).unwrap();
    assert_eq!(
        (e.seq(), e.last_trade_price(), e.book().len()),
        (Seq(9), Some(Price(100)), 3)
    );
    let mut bytes = Vec::new();
    good.encode(&mut bytes);
    assert!(matches!(
        EngineState::decode(&bytes[..bytes.len() - 1]),
        Err(StateError::Length { .. })
    ));
    bytes[0] = 7;
    assert_eq!(EngineState::decode(&bytes), Err(StateError::Version(7)));
    assert!(e.load_snapshot(&bytes).is_err());
}
