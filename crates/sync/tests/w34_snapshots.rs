//! Week 34: the seqlock and the epoch-reclaimed snapshot cell.

mod common;

use common::{within, Tracked};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use sync::{SeqLock, SnapshotCell};

/// A top-of-book-like value whose fields must always agree: a torn read
/// shows up as mismatched fields.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
struct Quote {
    words: [u64; 8],
}

impl Quote {
    fn new(k: u64) -> Quote {
        Quote { words: [k; 8] }
    }

    fn check(&self) -> u64 {
        assert!(
            self.words.iter().all(|&w| w == self.words[0]),
            "torn read: {:?}",
            self.words
        );
        self.words[0]
    }
}

#[test]
fn w34_seqlock_read_write_and_version() {
    let lock = SeqLock::new(Quote::new(1));
    assert_eq!(lock.version(), 0);
    assert_eq!(lock.read(), Quote::new(1));
    assert_eq!(lock.try_read(), Some(Quote::new(1)));
    for k in 2..=10 {
        lock.write(Quote::new(k));
        assert_eq!(lock.read().check(), k);
    }
    assert_eq!(lock.version(), 9);
    let mut lock = lock;
    lock.get_mut().words[0] = 0;
    assert_eq!(lock.into_inner().words[..2], [0, 10]);
}

fn hammer(writers: u64, writes_each: u64, readers: usize) -> u64 {
    let lock = Arc::new(SeqLock::new(Quote::new(0)));
    let done = Arc::new(AtomicBool::new(false));
    // Everyone starts together, so reads really overlap writes.
    let start = Arc::new(Barrier::new(readers + writers as usize));
    let reader_threads: Vec<_> = (0..readers)
        .map(|_| {
            let (lock, done, start) = (Arc::clone(&lock), Arc::clone(&done), Arc::clone(&start));
            std::thread::spawn(move || {
                start.wait();
                let mut reads = 0u64;
                let mut last = 0;
                while !done.load(Ordering::Acquire) || reads < 1_000 {
                    let k = lock.read().check();
                    if writers == 1 {
                        assert!(k >= last, "a reader went back in time: {k} after {last}");
                    }
                    last = k;
                    reads += 1;
                }
                reads
            })
        })
        .collect();
    let writer_threads: Vec<_> = (0..writers)
        .map(|w| {
            let (lock, start) = (Arc::clone(&lock), Arc::clone(&start));
            std::thread::spawn(move || {
                start.wait();
                for i in 1..=writes_each {
                    lock.write(Quote::new(i * writers + w));
                }
            })
        })
        .collect();
    for w in writer_threads {
        w.join().unwrap();
    }
    done.store(true, Ordering::Release);
    let reads: u64 = reader_threads.into_iter().map(|r| r.join().unwrap()).sum();
    assert!(reads >= readers as u64 * 1_000);
    lock.version() as u64
}

#[test]
fn w34_seqlock_readers_never_see_torn_values() {
    let version = within(60, || hammer(1, 300_000, 3));
    assert_eq!(version, 300_000);
}

#[test]
fn w34_seqlock_concurrent_writers_are_serialized() {
    let version = within(60, || hammer(2, 100_000, 2));
    assert_eq!(version, 200_000, "a write was lost: writers overlapped");
}

#[test]
fn w34_snapshot_publish_load_and_hold() {
    let cell = SnapshotCell::new(vec![1u32, 2, 3]);
    assert_eq!(cell.version(), 0);
    let mut reader = cell.reader();
    assert_eq!(reader.read(|v| v.clone()), vec![1, 2, 3]);
    let held = reader.load();
    for k in 0..100u32 {
        cell.publish(vec![k; k as usize]);
    }
    assert_eq!(cell.version(), 100);
    assert_eq!(
        *held,
        vec![1, 2, 3],
        "a loaded Arc must survive later publishes"
    );
    assert_eq!(reader.read(|v| v.len()), 99);
    assert_eq!(*cell.load(), vec![99; 99]);
    cell.publish_arc(Arc::new(vec![7]));
    assert_eq!(*reader.load(), vec![7]);
    assert_eq!(
        cell.pending_reclaim(),
        0,
        "no reader was pinned, so nothing is pending"
    );
}

#[test]
fn w34_snapshot_concurrent_readers_see_whole_monotonic_versions() {
    const PUBLISHES: u64 = 20_000;
    within(60, || {
        let cell = Arc::new(SnapshotCell::new(vec![0u64; 16]));
        let done = Arc::new(AtomicBool::new(false));
        let start = Arc::new(Barrier::new(5));
        let readers: Vec<_> = (0..4)
            .map(|r| {
                let mut reader = cell.reader();
                let (done, start) = (Arc::clone(&done), Arc::clone(&start));
                std::thread::spawn(move || {
                    let mut last = 0;
                    let mut reads = 0u64;
                    start.wait();
                    while !done.load(Ordering::Acquire) || reads < 100 {
                        let k = if r % 2 == 0 {
                            reader.read(|v| {
                                assert!(v.iter().all(|&x| x == v[0]), "inconsistent snapshot");
                                v[0]
                            })
                        } else {
                            let v = reader.load();
                            assert!(v.iter().all(|&x| x == v[0]));
                            v[0]
                        };
                        assert!(k >= last, "went back from version {last} to {k}");
                        last = k;
                        reads += 1;
                    }
                    reads
                })
            })
            .collect();
        start.wait();
        for k in 1..=PUBLISHES {
            cell.publish(vec![k; 16]);
        }
        done.store(true, Ordering::Release);
        for r in readers {
            assert!(r.join().unwrap() > 0);
        }
        assert_eq!(cell.version(), PUBLISHES);
        assert_eq!(
            cell.reclaim(),
            0,
            "with every reader idle, everything is reclaimable"
        );
    });
}

#[test]
fn w34_snapshot_reclaims_every_version_exactly_once() {
    let drops = Arc::new(AtomicUsize::new(0));
    let made = Arc::new(AtomicUsize::new(1));
    let cell = Arc::new(SnapshotCell::new(Tracked::new(0, &drops)));
    // No readers: each publish frees the previous version immediately.
    for id in 1..=1_000 {
        cell.publish(Tracked::new(id, &drops));
        made.fetch_add(1, Ordering::SeqCst);
    }
    assert_eq!(drops.load(Ordering::SeqCst), 1_000);
    assert_eq!(cell.pending_reclaim(), 0);

    // With readers racing the writer, versions may wait — but none is
    // freed twice, and none leaks.
    within(60, {
        let (cell, drops, made) = (Arc::clone(&cell), Arc::clone(&drops), Arc::clone(&made));
        move || {
            let done = Arc::new(AtomicBool::new(false));
            let start = Arc::new(Barrier::new(5));
            let readers: Vec<_> = (0..4)
                .map(|_| {
                    let mut reader = cell.reader();
                    let (done, start) = (Arc::clone(&done), Arc::clone(&start));
                    std::thread::spawn(move || {
                        start.wait();
                        while !done.load(Ordering::Acquire) {
                            reader.read(|t| assert!(t.id >= 1_000));
                        }
                    })
                })
                .collect();
            start.wait();
            for id in 1_001..=20_000 {
                cell.publish(Tracked::new(id, &drops));
                made.fetch_add(1, Ordering::SeqCst);
            }
            done.store(true, Ordering::Release);
            for r in readers {
                r.join().unwrap();
            }
        }
    });
    assert_eq!(cell.reclaim(), 0);
    assert_eq!(
        drops.load(Ordering::SeqCst),
        made.load(Ordering::SeqCst) - 1,
        "all but the current version are freed"
    );
    drop(cell);
    assert_eq!(
        drops.load(Ordering::SeqCst),
        made.load(Ordering::SeqCst),
        "dropping the cell frees the current version"
    );
}

#[test]
fn w34_pinned_reader_blocks_reclamation_until_unpinned() {
    let drops = Arc::new(AtomicUsize::new(0));
    let cell = SnapshotCell::new(Tracked::new(0, &drops));
    let mut reader = cell.reader();
    reader.read(|pinned| {
        for id in 1..=10 {
            cell.publish(Tracked::new(id, &drops));
        }
        // The version this reader is looking at must still be alive.
        assert_eq!(
            drops.load(Ordering::SeqCst),
            0,
            "freed a version a reader was using"
        );
        assert_eq!(pinned.id, 0);
        assert_eq!(cell.pending_reclaim(), 10);
    });
    // Unpinned: the next reclamation frees all ten old versions.
    assert_eq!(cell.reclaim(), 0);
    assert_eq!(drops.load(Ordering::SeqCst), 10);
    assert_eq!(reader.read(|t| t.id), 10);
    drop(reader);
    drop(cell);
    assert_eq!(drops.load(Ordering::SeqCst), 11);
}
