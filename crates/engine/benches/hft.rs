//! Benchmarks for the `engine` crate. Run with `cargo xtask bench --week N`.
//!
//! The w07 / w08 benchmarks build their order flow here, from `types::Rng`
//! and the engine's public API only, so they run as soon as week 7 is done
//! (the w12 generator is not needed).

use benchkit::{black_box, Report, Unit};
use engine::testkit::{FlowConfig, OrderFlow};
use engine::{Engine, OrderBook, Pipeline};
use journal::{
    FileStorage, Handled, Journal, JournalOptions, Primary, SnapshotStore, Standby, SyncPolicy,
    SystemClock, MAX_FRAME_LEN,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use telemetry::{Clock, OpenLoop};
use types::{Command, Event, InstrumentSpec, NewOrder, OrderId, Price, Rng, Side, TimeInForce};

const MID: i64 = 100_000;

/// Resting orders the mixed flow keeps the book around.
const TARGET_RESTING: usize = 1_000;

/// Live order ids, with O(1) random pick and removal.
#[derive(Default)]
struct Live {
    ids: Vec<OrderId>,
    pos: HashMap<OrderId, usize>,
}

impl Live {
    fn insert(&mut self, id: OrderId) {
        if !self.pos.contains_key(&id) {
            self.pos.insert(id, self.ids.len());
            self.ids.push(id);
        }
    }

    fn remove(&mut self, id: OrderId) {
        if let Some(p) = self.pos.remove(&id) {
            self.ids.swap_remove(p);
            if let Some(&moved) = self.ids.get(p) {
                self.pos.insert(moved, p);
            }
        }
    }

    fn pick(&self, rng: &mut Rng) -> Option<OrderId> {
        (!self.ids.is_empty()).then(|| self.ids[rng.range(0, self.ids.len() as u64 - 1) as usize])
    }

    /// Applies one command's events (the bench's view of the book).
    fn observe(&mut self, events: &[Event], book: &impl OrderBook) {
        for e in events {
            match *e {
                Event::Rested { id, .. } => self.insert(id),
                Event::Cancelled { id, .. } => self.remove(id),
                Event::Trade { maker, .. } if !book.contains(maker) => self.remove(maker),
                _ => {}
            }
        }
    }
}

/// Levels away from the touch: 1 + roughly geometric, mostly near the top.
fn depth_ticks(rng: &mut Rng, max: u64) -> i64 {
    1 + rng
        .range(0, max)
        .min(rng.range(0, max))
        .min(rng.range(0, max)) as i64
}

/// A book around `MID` (≈1000 orders over 50 levels a side) and a stream of
/// `n` commands that keeps it there: of the new orders ~70% are passive
/// (priced behind the opposite touch) and ~30% aggressive (limits crossing
/// up to 2 ticks through the touch, IOC or GTC, and some market orders),
/// interleaved with cancels of live orders. The stream is generated against
/// a shadow engine, so replaying it on a clone of the returned engine hits
/// live orders exactly as generated.
fn mixed_flow(n: usize, seed: u64) -> (Engine, Vec<Command>) {
    let mut rng = Rng::new(seed);
    let mut engine = Engine::new(InstrumentSpec::default());
    let mut live = Live::default();
    let mut out = Vec::new();
    let mut next_id = 1u64;
    let mut new_id = || {
        next_id += 1;
        next_id - 1
    };
    for side in [Side::Buy, Side::Sell] {
        for k in 1..=50i64 {
            let price = if side == Side::Buy { MID - k } else { MID + k };
            for _ in 0..rng.range(5, 15) {
                let cmd = Command::New(NewOrder::limit(new_id(), side, price, rng.range(1, 10)));
                out.clear();
                engine.process(&cmd, &mut out);
                live.observe(&out, engine.book());
            }
        }
    }
    let base = engine.clone();

    let mut cmds = Vec::with_capacity(n);
    while cmds.len() < n {
        let book = engine.book();
        let r = rng.range(0, 99);
        let few = live.ids.len() < TARGET_RESTING * 8 / 10;
        let many = live.ids.len() > TARGET_RESTING * 12 / 10;
        let cmd = if (r < 30 && !few) || many {
            match live.pick(&mut rng) {
                Some(id) => Command::Cancel { id },
                None => continue,
            }
        } else if r < 51 && !few {
            // Aggressive (30% of new orders).
            let side = rng.side();
            let qty = rng.range(1, 25);
            match book.best(side.opposite()) {
                None => continue,
                Some(_) if rng.chance(0.15) => Command::New(NewOrder::market(new_id(), side, qty)),
                Some(touch) => {
                    let through = rng.range_i64(0, 2);
                    let price = if side == Side::Buy {
                        touch.price.0 + through
                    } else {
                        touch.price.0 - through
                    };
                    let tif = if rng.chance(0.5) {
                        TimeInForce::Ioc
                    } else {
                        TimeInForce::Gtc
                    };
                    Command::New(NewOrder::limit(new_id(), side, price, qty).with_tif(tif))
                }
            }
        } else {
            // Passive (70% of new orders): behind the opposite touch.
            let side = rng.side();
            let anchor = match (side, book.best(side.opposite())) {
                (_, Some(touch)) => touch.price.0,
                (Side::Buy, None) => MID + 1,
                (Side::Sell, None) => MID - 1,
            };
            let k = depth_ticks(&mut rng, 40);
            let price = if side == Side::Buy {
                anchor - k
            } else {
                anchor + k
            };
            Command::New(NewOrder::limit(new_id(), side, price, rng.range(1, 10)))
        };
        out.clear();
        engine.process(&cmd, &mut out);
        live.observe(&out, engine.book());
        cmds.push(cmd);
    }
    (base, cmds)
}

/// Submit → events latency for one command of the mixed flow.
fn match_latency() -> Report {
    const ITER: u64 = 1_000_000;
    // `latency` warms up with ITER / 10 commands before measuring.
    let (base, cmds) = mixed_flow((ITER + ITER / 10) as usize, 0x0707);
    let mut engine = base.clone();
    let mut out = Vec::with_capacity(64);
    let mut next = cmds.iter();
    benchkit::latency(
        "w07.match_latency",
        "Submit→events, mixed flow (70% passive)",
        Unit::Ns,
        ITER,
        || *next.next().expect("stream long enough"),
        |cmd| {
            engine.process(&cmd, &mut out);
            black_box(&out);
            out.clear();
        },
    )
}

/// Commands per second over the whole mixed flow, single thread.
fn engine_throughput() -> Report {
    let (base, cmds) = mixed_flow(1_000_000, 0x0708);
    let mut out = Vec::with_capacity(64);
    benchkit::throughput(
        "w07.engine_throughput",
        "Orders per second, single thread",
        Unit::OpsPerSec,
        cmds.len() as u64,
        || {
            let mut engine = base.clone();
            for cmd in &cmds {
                engine.process(cmd, &mut out);
                out.clear();
            }
            black_box(engine.seq());
        },
    )
}

/// FOK feasibility on a deep book: 1000 levels a side, 1–5 orders each;
/// each check walks 1–100 levels and about half are feasible.
fn fok_check() -> Report {
    let mut rng = Rng::new(0x0808);
    let mut engine = Engine::new(InstrumentSpec::default());
    let mut out = Vec::new();
    let mut id = 0;
    for side in [Side::Buy, Side::Sell] {
        for k in 1..=1_000i64 {
            let price = if side == Side::Buy { MID - k } else { MID + k };
            for _ in 0..rng.range(1, 5) {
                id += 1;
                engine.process(
                    &Command::New(NewOrder::limit(id, side, price, rng.range(1, 10))),
                    &mut out,
                );
            }
        }
    }
    // Average level holds 3 orders × 5.5 lots.
    let per_level = 16;
    let engine = engine;
    benchkit::latency(
        "w08.fok_check",
        "FOK feasibility check, deep book",
        Unit::Ns,
        1_000_000,
        || {
            let side = rng.side();
            let levels = rng.range_i64(1, 100);
            let limit = if side == Side::Buy {
                MID + levels
            } else {
                MID - levels
            };
            let want = rng.range(1, levels as u64 * per_level * 2);
            (side, Price(limit), types::Qty(want))
        },
        |(side, limit, want)| {
            black_box(engine.fillable(side, Some(limit), want));
        },
    )
}

/// `n` commands of the w12 generator's default flow (adds, cancels,
/// reduces, IOC / FOK / post-only / market orders, malformed commands),
/// fed back through a shadow engine so cancels and reduces hit live orders.
fn w12_stream(seed: u64, n: usize) -> (FlowConfig, Vec<Command>) {
    let cfg = FlowConfig {
        seed,
        ..FlowConfig::default()
    };
    let mut flow = OrderFlow::new(cfg);
    let mut shadow = Engine::with_config(cfg.engine_config());
    let mut out = Vec::new();
    let mut cmds = Vec::with_capacity(n);
    for _ in 0..n {
        let cmd = flow.next_command();
        out.clear();
        shadow.process(&cmd, &mut out);
        flow.observe(&cmd, &out);
        cmds.push(cmd);
    }
    (cfg, cmds)
}

/// Bytes of in-memory journal the w12 pipeline keeps before draining it.
const V1_JOURNAL_BYTES: usize = 256 * 1024;

const V1_LABEL: &str = "Engine v1 end to end (journal + match + md)";

/// The w12 stream through engine v1, the whole [`Pipeline`]: each command
/// is appended to the in-memory journal (sequenced, encoded, checksummed),
/// matched, and its events turned into L2 updates. The journal is drained
/// (cleared — sequence numbers continue) every 256 KiB, as if its frames
/// had been handed to a disk writer, so its write head stays in cache.
fn engine_v1_throughput() -> Report {
    const N: usize = 1_000_000;
    let (cfg, cmds) = w12_stream(0x1212, N);
    benchkit::throughput(
        "w12.engine_v1_throughput",
        V1_LABEL,
        Unit::OpsPerSec,
        N as u64,
        || {
            let journal = Journal::with_capacity(V1_JOURNAL_BYTES);
            let engine = Engine::with_config(cfg.engine_config());
            let mut p = Pipeline::with_journal(journal, engine).expect("both fresh");
            for cmd in &cmds {
                if p.journal().byte_len() + MAX_FRAME_LEN > V1_JOURNAL_BYTES {
                    p.journal_mut().clear();
                }
                let out = p.process(cmd).expect("an in-memory journal never fails");
                black_box(out.updates.len());
            }
            black_box(p.engine().seq());
        },
    )
}

fn main() {
    benchkit::run(&[
        (
            "w07.match_latency",
            "Submit→events, mixed flow (70% passive)",
            match_latency,
        ),
        (
            "w07.engine_throughput",
            "Orders per second, single thread",
            engine_throughput,
        ),
        (
            "w08.fok_check",
            "FOK feasibility check, deep book",
            fok_check,
        ),
        ("w12.engine_v1_throughput", V1_LABEL, engine_v1_throughput),
        ("w18.recovery", RECOVERY_LABEL, recovery),
        ("w20.replication_lag", REPLICATION_LABEL, replication_lag),
        ("w25.engine_open_loop", OPEN_LOOP_LABEL, engine_open_loop),
    ]);
}

// --- Week 18 ----------------------------------------------------------------

const RECOVERY_LABEL: &str = "Recover a 1M-command journal: snapshot at 750k + replay 250k tail \
     (verify whole file, warm page cache)";

/// A directory under the system temp dir, removed on drop (also when a
/// template stub panics).
struct BenchDir(PathBuf);

impl BenchDir {
    fn new(tag: &str) -> BenchDir {
        let path = std::env::temp_dir().join(format!("hft-bench-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp dir");
        BenchDir(path)
    }
}

impl Drop for BenchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Crash recovery of a durable engine: a 1M-command w12 journal (~45 MB)
/// with snapshots every 250k commands. Each sample is a full
/// `engine::recover`: open and verify every frame of the journal (CRC,
/// sequence, rebuilding the index), load the newest snapshot (750k) and
/// replay the 250k-command tail. The file is in the page cache, so this is
/// the CPU cost of recovery, not the disk's.
fn recovery() -> Report {
    const N: usize = 1_000_000;
    const SNAPSHOT_EVERY: usize = 250_000;
    const WARMUP: usize = 2;
    const SAMPLES: usize = 15;
    let (cfg, cmds) = w12_stream(0x1818, N);
    let dir = BenchDir::new("w18-recovery");
    let log = dir.0.join("journal.log");
    let store = SnapshotStore::open(dir.0.join("snapshots")).expect("snapshot dir");
    let opts = JournalOptions::with_sync(SyncPolicy::Never);
    let open = || {
        let storage = FileStorage::open(&log).expect("journal file");
        engine::recover(
            cfg.engine_config(),
            storage,
            Some(&store),
            opts,
            SystemClock::new(),
        )
        .expect("recovery")
    };
    {
        let (mut p, _) = open();
        for (i, cmd) in cmds.iter().enumerate() {
            p.process(cmd).expect("journal append");
            if (i + 1) % SNAPSHOT_EVERY == 0 && i + 1 < N {
                p.checkpoint(&store).expect("snapshot");
            }
        }
        p.journal_mut().sync().expect("sync");
    }
    let mut hist = benchkit::Histogram::new();
    let mut bytes = 0;
    for i in 0..WARMUP + SAMPLES {
        let t = Instant::now();
        let (p, report) = open();
        let elapsed = t.elapsed();
        assert_eq!(
            report.snapshot,
            Some(types::Seq((N - SNAPSHOT_EVERY) as u64))
        );
        assert_eq!(report.replayed, SNAPSHOT_EVERY as u64);
        bytes = p.journal().byte_len();
        black_box(p.engine().state_hash());
        if i >= WARMUP {
            hist.record(elapsed.as_nanos() as u64);
        }
    }
    eprintln!("journal: {N} commands, {bytes} bytes");
    hist.report("w18.recovery", RECOVERY_LABEL, Unit::Us)
}

// --- Week 20 ----------------------------------------------------------------

const REPLICATION_LABEL: &str =
    "Primary append -> standby applied (validated, journaled, matched), \
     in-process, 100k cmds/s, spinning standby";

/// Replication lag to a hot standby: the primary sequences the w12 flow at
/// 100k commands per second (open loop), each through `Primary::append`
/// (journal + send); a standby thread spinning on its channel validates
/// each frame (CRC, sequence), appends it to its journal, applies it to its
/// own engine and acknowledges. Lag = applied on the standby − start of the
/// primary's append, per command (TSC clock shared by both threads).
fn replication_lag() -> Report {
    const N: usize = 200_000;
    const WARMUP: usize = 20_000;
    const INTERVAL_NS: u64 = 10_000;
    let total = N + WARMUP;
    let (cfg, cmds) = w12_stream(0x2020, total);
    let clock = *Clock::global();
    let stamps: Arc<Vec<AtomicU64>> = Arc::new((0..=total).map(|_| AtomicU64::new(0)).collect());
    let lease = Duration::from_secs(3_600);
    let leases = SystemClock::new();
    let (plink, slink) = journal::link();
    let standby = Standby::new(
        Journal::with_capacity(total * MAX_FRAME_LEN),
        Engine::with_config(cfg.engine_config()),
        slink,
        leases,
        lease,
    );
    let seen = Arc::clone(&stamps);
    let standby = std::thread::spawn(move || {
        let mut s = standby;
        let mut hist = benchkit::Histogram::new();
        loop {
            match s.poll_one() {
                Ok(Some(Handled::Applied(seq))) => {
                    let now = clock.now();
                    let sent = seen[seq.0 as usize].load(Ordering::Relaxed);
                    if seq.0 > WARMUP as u64 {
                        hist.record(now.saturating_sub(sent));
                    }
                }
                Ok(Some(other)) => panic!("unexpected {other:?}"),
                Ok(None) => std::hint::spin_loop(),
                // The primary is done and every frame handled.
                Err(_) => return hist,
            }
        }
    });
    let mut primary = Primary::new(
        Journal::with_capacity(total * MAX_FRAME_LEN),
        plink,
        leases,
        lease,
    );
    let start = clock.now();
    for (i, cmd) in cmds.iter().enumerate() {
        let due = start + i as u64 * INTERVAL_NS;
        while clock.now() < due {
            std::hint::spin_loop();
        }
        stamps[i + 1].store(clock.now(), Ordering::Relaxed);
        black_box(primary.append(cmd).expect("the standby granted a lease"));
    }
    drop(primary);
    let hist = standby.join().expect("standby thread");
    hist.report("w20.replication_lag", REPLICATION_LABEL, Unit::Us)
}

// --- Week 25 ----------------------------------------------------------------

const OPEN_LOOP_LABEL: &str = "Engine under open-loop load at 50% utilization (mixed flow), \
     response time from intended start";

/// The real engine under `telemetry::OpenLoop`: the w07 mixed flow is
/// offered at a fixed rate and each command's latency is measured from its
/// *intended* start, so queueing behind a slow command (or an interrupt,
/// or a preemption) is charged to every command that waited.
///
/// The rate is set for 50% utilization of the serving thread: first the
/// same harness runs the flow back to back (every request already due) to
/// measure the cost per command — engine plus the harness's clock reads and
/// histogram records, which share the thread — then the interval is twice
/// that. 1M measured commands after 100k of warm-up.
fn engine_open_loop() -> Report {
    const REQUESTS: u64 = 1_000_000;
    const WARMUP: u64 = 100_000;
    let total = (REQUESTS + WARMUP) as usize;
    let (base, cmds) = mixed_flow(total, 0x2525);
    let mut out = Vec::with_capacity(64);

    // Capacity: the same harness with every request already due (closed
    // loop, back to back) — engine plus measurement cost per command.
    let mut clock = *Clock::global();
    let mut busy = base.clone();
    let flat_out = OpenLoop::at_rate(1_000_000_000, total as u64).run(&mut clock, |_, i| {
        busy.process(&cmds[i as usize], &mut out);
        out.clear();
    });
    black_box(busy.seq());
    let service_ns = flat_out.elapsed_ns as f64 / flat_out.requests as f64;
    let interval_ns = ((2.0 * service_ns).ceil() as u64).max(20);

    let mut engine = base.clone();
    let schedule = OpenLoop::at_rate(1_000_000_000 / interval_ns, REQUESTS).with_warmup(WARMUP);
    let r = schedule.run(&mut clock, |_, i| {
        engine.process(&cmds[i as usize], &mut out);
        out.clear();
    });
    black_box(engine.seq());
    eprintln!(
        "per-command cost (engine + harness) {service_ns:.1} ns -> interval {interval_ns} ns ({:.2}M/s offered, \
         {:.2}M/s achieved), max lag {} ns; service-time view: p50 {} p99 {} p99.9 {} ns",
        r.offered_rate() / 1e6,
        r.achieved_rate() / 1e6,
        r.max_lag_ns,
        r.service.percentile(0.5),
        r.service.percentile(0.99),
        r.service.percentile(0.999),
    );
    let h = &r.response;
    Report {
        id: "w25.engine_open_loop".into(),
        label: OPEN_LOOP_LABEL.into(),
        unit: Unit::Ns,
        value: h.percentile(0.5) as f64,
        p50: Some(h.percentile(0.5) as f64),
        p99: Some(h.percentile(0.99) as f64),
        p999: Some(h.percentile(0.999) as f64),
        max: Some(h.max() as f64),
        samples: h.count(),
    }
}
