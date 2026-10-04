//! Week 4: cache padding, padded counters, and the refitted SPSC ring.

mod common;

use common::{backoff, within};
use std::mem::{align_of, align_of_val, size_of};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use sync::{spsc, CachePadded, PaddedCounters, CACHE_LINE};

#[test]
fn w04_cache_padded_alignment_size_and_access() {
    assert!(CACHE_LINE >= 64 && CACHE_LINE.is_power_of_two());

    let mut small = CachePadded::new(7u8);
    assert_eq!(align_of_val(&small), CACHE_LINE);
    assert_eq!(size_of::<CachePadded<u8>>(), CACHE_LINE);
    assert_eq!(
        &*small as *const u8 as usize % CACHE_LINE,
        0,
        "value must start on a line boundary"
    );
    *small += 1; // DerefMut
    assert_eq!(*small, 8); // Deref
    assert_eq!(small.into_inner(), 8);

    let big = CachePadded::new([3u8; 200]);
    assert_eq!(
        size_of::<CachePadded<[u8; 200]>>() % CACHE_LINE,
        0,
        "size is a whole number of lines"
    );
    assert!(size_of::<CachePadded<[u8; 200]>>() >= 200);
    assert_eq!(big.iter().map(|&b| b as u32).sum::<u32>(), 600);
    assert_eq!(align_of::<CachePadded<AtomicU64>>(), CACHE_LINE);
}

#[test]
fn w04_padded_values_never_share_a_line() {
    let v: Vec<CachePadded<AtomicU64>> = (0..16)
        .map(|i| CachePadded::new(AtomicU64::new(i)))
        .collect();
    let addrs: Vec<usize> = v
        .iter()
        .map(|c| &**c as *const AtomicU64 as usize)
        .collect();
    for (i, w) in addrs.windows(2).enumerate() {
        assert!(
            w[1] - w[0] >= CACHE_LINE,
            "elements {i} and {} are {} bytes apart",
            i + 1,
            w[1] - w[0]
        );
    }
    assert!(addrs.iter().all(|a| a % CACHE_LINE == 0));
    assert_eq!(v[5].load(Ordering::Relaxed), 5);
}

#[test]
fn w04_padded_counters_count_exactly_under_8_threads() {
    const THREADS: usize = 8;
    const ADDS: u64 = 100_000;
    let counters = Arc::new(PaddedCounters::new(THREADS));
    assert_eq!(counters.len(), THREADS);
    within(60, {
        let counters = Arc::clone(&counters);
        move || {
            std::thread::scope(|s| {
                for t in 0..THREADS {
                    let counters = &counters;
                    s.spawn(move || {
                        for _ in 0..ADDS {
                            counters.add(t, 1);
                        }
                        // Shared slot too: adds from any thread are atomic.
                        for _ in 0..1_000 {
                            counters.add(0, 2);
                        }
                    });
                }
            });
        }
    });
    assert_eq!(counters.get(0), ADDS + THREADS as u64 * 2_000);
    for t in 1..THREADS {
        assert_eq!(counters.get(t), ADDS, "slot {t}");
    }
    assert_eq!(
        counters.sum(),
        THREADS as u64 * ADDS + THREADS as u64 * 2_000
    );
}

#[test]
fn w04_padded_counters_edge_cases() {
    let none = PaddedCounters::new(0);
    assert!(none.is_empty());
    assert_eq!(none.sum(), 0);

    let one = PaddedCounters::new(1);
    assert!(!one.is_empty());
    one.add(0, u64::MAX - 1);
    one.add(0, 1);
    assert_eq!(one.get(0), u64::MAX);

    let three = PaddedCounters::new(3);
    let out_of_range = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| three.add(3, 1)));
    assert!(
        out_of_range.is_err(),
        "slot >= len must panic, not corrupt memory"
    );
}

#[test]
fn w04_spsc_indices_on_separate_cache_lines() {
    fn check<T>(what: &str) {
        let (head, tail) = spsc::index_offsets::<T>();
        assert!(
            head.abs_diff(tail) >= 64,
            "{what}: head at {head}, tail at {tail} share a cache line"
        );
    }
    check::<u8>("u8");
    check::<u64>("u64");
    check::<String>("String");
    check::<[u64; 16]>("[u64; 16]");
}

#[test]
fn w04_spsc_refit_still_delivers_in_order() {
    // Capacity 4: producer and consumer constantly meet at the boundary,
    // which is where index-publication bugs show.
    const N: u64 = 1_000_000;
    within(60, || {
        let (mut tx, mut rx) = spsc::channel::<u64>(4);
        let producer = std::thread::spawn(move || {
            let (mut i, mut spins) = (0, 0);
            while i < N {
                match tx.try_push(i * 3) {
                    Ok(()) => i += 1,
                    Err(_) => backoff(&mut spins),
                }
            }
        });
        let (mut expected, mut spins) = (0, 0);
        while expected < N {
            match rx.try_pop() {
                Some(v) => {
                    assert_eq!(v, expected * 3);
                    expected += 1;
                }
                None => backoff(&mut spins),
            }
        }
        producer.join().unwrap();
    });
}
