//! Benchmarks for the `sync` crate. Run with `cargo xtask bench --week N`.
//!
//! Multi-threaded benchmarks pin their threads to distinct physical cores,
//! packed into as few L3 domains as possible (and avoiding core 0 when
//! there are enough cores), so results don't depend on where the scheduler
//! happens to put the threads. Override with `HFT_BENCH_CPUS=2,3,4` (a
//! comma-separated list used in order) or disable with
//! `HFT_BENCH_CPUS=none`. Pinning is done here rather than with
//! `sync::pin_current_thread` so early weeks' benchmarks don't depend on
//! week 30's code.

use benchkit::{black_box, Histogram, Report, Unit};
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};
use sync::{
    mpsc, spmc, spsc, BusySpin, CancelToken, PaddedCounters, SeqLock, SpinPark, UnpaddedCounters,
    WaitStrategy,
};

fn main() {
    benchkit::run(&[
        (
            "w01.spsc_throughput",
            "SPSC ring, producer→consumer across two threads",
            bench_spsc_throughput,
        ),
        (
            "w01.mutex_channel_throughput",
            "Mutex channel baseline, two threads",
            bench_mutex_channel_throughput,
        ),
        (
            "w03.cancel_fanout",
            "Cancel 1,000 child tokens, observed by all",
            bench_cancel_fanout,
        ),
        (
            "w04.padded_counters",
            "8 threads incrementing padded counters",
            bench_padded_counters,
        ),
        (
            "w04.unpadded_counters",
            "8 threads incrementing adjacent counters (baseline)",
            bench_unpadded_counters,
        ),
        (
            "w05.spsc_batched_throughput",
            "SPSC ring with batching, two threads",
            bench_spsc_batched_throughput,
        ),
        (
            "w05.mpsc_throughput",
            "MPSC, 4 producers → 1 consumer",
            bench_mpsc_throughput,
        ),
        (
            "w30.wakeup_busy",
            "Cross-thread wakeup, busy polling",
            bench_wakeup_busy,
        ),
        (
            "w30.wakeup_park",
            "Cross-thread wakeup, park/unpark",
            bench_wakeup_park,
        ),
        (
            "w34.seqlock_read",
            "Seqlock read under a concurrent writer",
            bench_seqlock_read,
        ),
    ]);
}

// ---------------------------------------------------------------------------
// Week 1

fn bench_spsc_throughput() -> Report {
    const N: u64 = 4_000_000;
    let cpus = bench_cpus(2);
    pin(cpus[0]);
    benchkit::throughput(
        "w01.spsc_throughput",
        "SPSC ring, producer→consumer across two threads",
        Unit::OpsPerSec,
        N,
        || {
            let (mut tx, mut rx) = spsc::channel::<u64>(1024);
            std::thread::scope(|s| {
                s.spawn(|| {
                    pin(cpus[1]);
                    for i in 0..N {
                        let mut v = i;
                        while let Err(back) = tx.try_push(v) {
                            v = back;
                            std::hint::spin_loop();
                        }
                    }
                });
                let mut sum = 0u64;
                let mut got = 0;
                while got < N {
                    match rx.try_pop() {
                        Some(v) => {
                            sum = sum.wrapping_add(v);
                            got += 1;
                        }
                        None => std::hint::spin_loop(),
                    }
                }
                assert_eq!(sum, N * (N - 1) / 2);
            });
        },
    )
}

fn bench_mutex_channel_throughput() -> Report {
    const N: u64 = 1_000_000;
    let cpus = &bench_cpus(2);
    pin(cpus[0]);
    benchkit::throughput(
        "w01.mutex_channel_throughput",
        "Mutex channel baseline, two threads",
        Unit::OpsPerSec,
        N,
        || {
            let (tx, rx) = spmc::channel::<u64>();
            std::thread::scope(|s| {
                s.spawn(move || {
                    pin(cpus[1]);
                    for i in 0..N {
                        tx.send(i).unwrap();
                    }
                });
                let mut sum = 0u64;
                while let Some(v) = rx.recv() {
                    sum = sum.wrapping_add(v);
                }
                assert_eq!(sum, N * (N - 1) / 2);
            });
        },
    )
}

// ---------------------------------------------------------------------------
// Week 3

fn bench_cancel_fanout() -> Report {
    const CHILDREN: usize = 1_000;
    // Trees are dropped in `setup` (untimed), not at the end of the timed op.
    let graveyard = RefCell::new(Vec::new());
    benchkit::latency(
        "w03.cancel_fanout",
        "Cancel 1,000 child tokens, observed by all",
        Unit::Us,
        5_000,
        || {
            graveyard.borrow_mut().clear();
            let root = CancelToken::new();
            let children: Vec<CancelToken> = (0..CHILDREN).map(|_| root.child()).collect();
            (root, children)
        },
        |(root, children)| {
            root.cancel();
            assert!(children.iter().all(|c| c.is_cancelled()));
            graveyard.borrow_mut().push((root, children));
        },
    )
}

// ---------------------------------------------------------------------------
// Week 4

trait Counters: Sync {
    fn add(&self, slot: usize, n: u64);
    fn sum(&self) -> u64;
}

impl Counters for PaddedCounters {
    #[inline]
    fn add(&self, slot: usize, n: u64) {
        PaddedCounters::add(self, slot, n)
    }
    fn sum(&self) -> u64 {
        PaddedCounters::sum(self)
    }
}

impl Counters for UnpaddedCounters {
    #[inline]
    fn add(&self, slot: usize, n: u64) {
        UnpaddedCounters::add(self, slot, n)
    }
    fn sum(&self) -> u64 {
        UnpaddedCounters::sum(self)
    }
}

fn counters_bench(id: &str, label: &str, counters: &impl Counters, per_thread: u64) -> Report {
    const THREADS: usize = 8;
    let cpus = bench_cpus(THREADS);
    let mut expected = 0;
    let report = benchkit::throughput(
        id,
        label,
        Unit::OpsPerSec,
        THREADS as u64 * per_thread,
        || {
            let start = Barrier::new(THREADS);
            std::thread::scope(|s| {
                for (t, &cpu) in cpus.iter().enumerate() {
                    let start = &start;
                    s.spawn(move || {
                        pin(cpu);
                        start.wait();
                        for _ in 0..per_thread {
                            counters.add(t, 1);
                        }
                    });
                }
            });
            expected += THREADS as u64 * per_thread;
        },
    );
    assert_eq!(counters.sum(), expected);
    report
}

fn bench_padded_counters() -> Report {
    let counters = PaddedCounters::new(8);
    counters_bench(
        "w04.padded_counters",
        "8 threads incrementing padded counters",
        &counters,
        20_000_000,
    )
}

fn bench_unpadded_counters() -> Report {
    let counters = UnpaddedCounters::new(8);
    counters_bench(
        "w04.unpadded_counters",
        "8 threads incrementing adjacent counters (baseline)",
        &counters,
        1_000_000,
    )
}

// ---------------------------------------------------------------------------
// Week 5

fn bench_spsc_batched_throughput() -> Report {
    const N: u64 = 64_000_000;
    const BATCH: usize = 256;
    let cpus = &bench_cpus(2);
    pin(cpus[0]);
    let src: Vec<u64> = (0..BATCH as u64).collect();
    benchkit::throughput(
        "w05.spsc_batched_throughput",
        "SPSC ring with batching, two threads",
        Unit::OpsPerSec,
        N,
        || {
            let (mut tx, mut rx) = spsc::channel::<u64>(4096);
            std::thread::scope(|s| {
                let src = &src;
                s.spawn(move || {
                    pin(cpus[1]);
                    let mut sent = 0u64;
                    while sent < N {
                        let want = ((N - sent) as usize).min(BATCH);
                        let n = tx.push_slice(&src[..want]);
                        if n == 0 {
                            std::hint::spin_loop();
                        }
                        sent += n as u64;
                    }
                });
                let mut out = [0u64; BATCH];
                let mut got = 0u64;
                let mut sum = 0u64;
                while got < N {
                    let n = rx.pop_into(&mut out);
                    if n == 0 {
                        std::hint::spin_loop();
                        continue;
                    }
                    sum = sum.wrapping_add(out[n - 1]);
                    got += n as u64;
                }
                black_box(sum);
            });
        },
    )
}

fn bench_mpsc_throughput() -> Report {
    const PRODUCERS: usize = 4;
    const PER: u64 = 1_000_000;
    let cpus = bench_cpus(PRODUCERS + 1);
    pin(cpus[0]);
    benchkit::throughput(
        "w05.mpsc_throughput",
        "MPSC, 4 producers → 1 consumer",
        Unit::OpsPerSec,
        PRODUCERS as u64 * PER,
        || {
            let (tx, mut rx) = mpsc::channel::<u64>(4096);
            std::thread::scope(|s| {
                for &cpu in &cpus[1..] {
                    let tx = tx.clone();
                    s.spawn(move || {
                        pin(cpu);
                        for i in 0..PER {
                            tx.push(i).unwrap();
                        }
                    });
                }
                let total = PRODUCERS as u64 * PER;
                let mut got = 0;
                let mut sum = 0u64;
                while got < total {
                    match rx.try_pop() {
                        Some(v) => {
                            sum = sum.wrapping_add(v);
                            got += 1;
                        }
                        None => std::hint::spin_loop(),
                    }
                }
                assert_eq!(sum, PRODUCERS as u64 * PER * (PER - 1) / 2);
            });
        },
    )
}

// ---------------------------------------------------------------------------
// Week 30

/// Keeps a value on its own 128-byte line (local, so this benchmark does
/// not depend on week 4's `CachePadded`).
#[repr(align(128))]
struct Line<T>(T);

/// One-way wakeup latency: the main thread stamps the time, publishes a
/// round number and calls `notify`; the waiter (pinned to another core)
/// records `now - stamp` when its `wait_until` returns, then acks.
/// `before_send` runs before each round (to let the waiter go idle).
fn wakeup_bench<S: WaitStrategy + 'static>(
    id: &str,
    label: &str,
    unit: Unit,
    strategy: S,
    rounds: u64,
    before_send: impl Fn(&S),
) -> Report {
    let warmup = rounds / 10;
    let total = rounds + warmup;
    let cpus = bench_cpus(2);
    let strategy = Arc::new(strategy);
    let round = Arc::new(Line(AtomicU64::new(0)));
    let stamp = Arc::new(Line(AtomicU64::new(0)));
    let ack = Arc::new(Line(AtomicU64::new(0)));
    let base = Instant::now();
    let waiter = {
        let (strategy, round, stamp, ack) = (
            Arc::clone(&strategy),
            Arc::clone(&round),
            Arc::clone(&stamp),
            Arc::clone(&ack),
        );
        let cpu = cpus[1];
        std::thread::spawn(move || {
            pin(cpu);
            let mut hist = Histogram::new();
            for r in 1..=total {
                strategy.wait_until(|| round.0.load(Ordering::Acquire) >= r);
                let now = base.elapsed().as_nanos() as u64;
                if r > warmup {
                    hist.record(now.saturating_sub(stamp.0.load(Ordering::Relaxed)));
                }
                ack.0.store(r, Ordering::Release);
            }
            hist
        })
    };
    pin(cpus[0]);
    for r in 1..=total {
        while ack.0.load(Ordering::Acquire) < r - 1 {
            std::hint::spin_loop();
        }
        before_send(&strategy);
        stamp
            .0
            .store(base.elapsed().as_nanos() as u64, Ordering::Relaxed);
        round.0.store(r, Ordering::Release);
        strategy.notify();
    }
    waiter.join().unwrap().report(id, label, unit)
}

fn spin_for(d: Duration) {
    let t = Instant::now();
    while t.elapsed() < d {
        std::hint::spin_loop();
    }
}

fn bench_wakeup_busy() -> Report {
    wakeup_bench(
        "w30.wakeup_busy",
        "Cross-thread wakeup, busy polling",
        Unit::Ns,
        BusySpin,
        200_000,
        |_| {
            // Let the waiter settle into its polling loop.
            spin_for(Duration::from_micros(1));
        },
    )
}

fn bench_wakeup_park() -> Report {
    let strategy = SpinPark::new(1_000, Duration::from_millis(100));
    wakeup_bench(
        "w30.wakeup_park",
        "Cross-thread wakeup, park/unpark",
        Unit::Us,
        strategy,
        20_000,
        |s| {
            // Measure the parked path: wait until the waiter has given up
            // spinning, then give it time to actually block in the kernel.
            while !s.is_parked() {
                std::hint::spin_loop();
            }
            spin_for(Duration::from_micros(30));
        },
    )
}

// ---------------------------------------------------------------------------
// Week 34

#[derive(Clone, Copy, Default)]
struct Quote {
    bid: i64,
    ask: i64,
    bid_qty: u64,
    ask_qty: u64,
    seq: u64,
}

fn bench_seqlock_read() -> Report {
    let cpus = bench_cpus(2);
    let lock = SeqLock::new(Quote::default());
    let stop = AtomicBool::new(false);
    std::thread::scope(|s| {
        let writer = s.spawn(|| {
            pin(cpus[1]);
            let mut k = 0u64;
            // ~2M updates/s: a busy instrument's top of book.
            while !stop.load(Ordering::Relaxed) {
                k += 1;
                let px = 10_000 + (k % 7) as i64;
                lock.write(Quote {
                    bid: px,
                    ask: px + 1,
                    bid_qty: k % 100,
                    ask_qty: k % 50,
                    seq: k,
                });
                spin_for(Duration::from_nanos(500));
            }
            k
        });
        pin(cpus[0]);
        let report = benchkit::latency_batched(
            "w34.seqlock_read",
            "Seqlock read under a concurrent writer",
            Unit::Ns,
            200_000,
            64,
            || {
                let q = lock.read();
                black_box(q.bid + q.ask + (q.bid_qty + q.ask_qty + q.seq) as i64);
            },
        );
        stop.store(true, Ordering::Relaxed);
        let writes = writer.join().unwrap();
        assert!(writes > 0);
        report
    })
}

// ---------------------------------------------------------------------------
// CPU selection and pinning (bench-local, Linux only)

/// `n` CPUs to pin `n` threads to (`None` = don't pin).
fn bench_cpus(n: usize) -> Vec<Option<usize>> {
    let unpinned = vec![None; n];
    match std::env::var("HFT_BENCH_CPUS") {
        Ok(v) if v.trim() == "none" => return unpinned,
        Ok(v) if v.trim().is_empty() => {}
        Ok(v) => {
            let list: Vec<usize> = v.split(',').filter_map(|c| c.trim().parse().ok()).collect();
            if list.len() >= n {
                return list[..n].iter().map(|&c| Some(c)).collect();
            }
            eprintln!("HFT_BENCH_CPUS lists fewer than {n} CPUs; picking automatically");
        }
        Err(_) => {}
    }
    match topology_cpus(n) {
        Some(cpus) => cpus.into_iter().map(Some).collect(),
        None => unpinned,
    }
}

/// One CPU per physical core, packed into the fewest L3 domains, skipping
/// core 0 when there are enough cores. `None` if topology is unavailable or
/// there are fewer than `n` cores.
fn topology_cpus(n: usize) -> Option<Vec<usize>> {
    let allowed = affinity::allowed()?;
    let read = |cpu: usize, file: &str| {
        std::fs::read_to_string(format!("/sys/devices/system/cpu/cpu{cpu}/{file}"))
            .ok()
            .map(|s| s.trim().to_string())
    };
    // (l3 domain, core) → lowest CPU of that core.
    let mut cores: Vec<(String, usize)> = Vec::new();
    let mut seen_cores = std::collections::HashSet::new();
    for &cpu in &allowed {
        let siblings =
            read(cpu, "topology/thread_siblings_list").unwrap_or_else(|| cpu.to_string());
        if seen_cores.insert(siblings) {
            let l3 = read(cpu, "cache/index3/shared_cpu_list").unwrap_or_default();
            cores.push((l3, cpu));
        }
    }
    if cores.len() > n {
        cores.retain(|&(_, cpu)| cpu != 0);
    }
    if cores.len() < n {
        return None;
    }
    // Group by L3 in order of first appearance; prefer one group that fits.
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (l3, cpu) in cores {
        match groups.iter_mut().find(|(g, _)| *g == l3) {
            Some((_, v)) => v.push(cpu),
            None => groups.push((l3, vec![cpu])),
        }
    }
    if let Some((_, v)) = groups.iter().find(|(_, v)| v.len() >= n) {
        return Some(v[..n].to_vec());
    }
    Some(groups.into_iter().flat_map(|(_, v)| v).take(n).collect())
}

fn pin(cpu: Option<usize>) {
    if let Some(cpu) = cpu {
        affinity::pin(cpu);
    }
}

#[cfg(target_os = "linux")]
mod affinity {
    type CpuSet = [u64; 16];

    extern "C" {
        fn sched_setaffinity(pid: i32, cpusetsize: usize, mask: *const CpuSet) -> i32;
        fn sched_getaffinity(pid: i32, cpusetsize: usize, mask: *mut CpuSet) -> i32;
    }

    pub fn pin(cpu: usize) {
        if cpu >= 1024 {
            return;
        }
        let mut set: CpuSet = [0; 16];
        set[cpu / 64] |= 1 << (cpu % 64);
        // SAFETY: pid 0 is the calling thread; `set` is an initialized
        // cpu_set_t-sized buffer passed with its exact size.
        let rc = unsafe { sched_setaffinity(0, std::mem::size_of::<CpuSet>(), &set) };
        if rc != 0 {
            eprintln!(
                "warning: could not pin to cpu {cpu}: {}",
                std::io::Error::last_os_error()
            );
        }
    }

    pub fn allowed() -> Option<Vec<usize>> {
        let mut set: CpuSet = [0; 16];
        // SAFETY: as above; the kernel writes at most `size_of::<CpuSet>()`
        // bytes into `set`.
        let rc = unsafe { sched_getaffinity(0, std::mem::size_of::<CpuSet>(), &mut set) };
        (rc == 0).then(|| {
            (0..1024)
                .filter(|&c| set[c / 64] & (1 << (c % 64)) != 0)
                .collect()
        })
    }
}

#[cfg(not(target_os = "linux"))]
mod affinity {
    pub fn pin(_cpu: usize) {}

    pub fn allowed() -> Option<Vec<usize>> {
        None
    }
}
