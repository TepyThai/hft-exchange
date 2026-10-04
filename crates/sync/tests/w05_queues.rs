//! Week 5: batched SPSC operations with cached indices, and the bounded
//! MPSC queue.

mod common;

use common::{backoff, within, Rng, Tracked};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use sync::{mpsc, spsc};

#[test]
fn w05_batches_match_vecdeque_model() {
    let mut rng = Rng::new(0xBA7C4);
    for cap in [1usize, 2, 8, 16] {
        let (mut tx, mut rx) = spsc::channel::<u32>(cap);
        let mut model = VecDeque::new();
        let mut next = 0u32;
        let mut out = [0u32; 24];
        for _ in 0..20_000 {
            match rng.range(0, 3) {
                0 => {
                    let n = rng.range(0, 20) as usize;
                    let batch: Vec<u32> = (next..next + n as u32).collect();
                    let pushed = tx.push_slice(&batch);
                    assert_eq!(
                        pushed,
                        n.min(cap - model.len()),
                        "push_slice must push all that fit"
                    );
                    model.extend(&batch[..pushed]);
                    next += pushed as u32;
                }
                1 => {
                    let n = rng.range(0, 24) as usize;
                    let popped = rx.pop_into(&mut out[..n]);
                    assert_eq!(popped, n.min(model.len()));
                    for &v in &out[..popped] {
                        assert_eq!(Some(v), model.pop_front());
                    }
                }
                2 => {
                    if tx.try_push(next).is_ok() {
                        model.push_back(next);
                        next += 1;
                    } else {
                        assert_eq!(model.len(), cap);
                    }
                }
                _ => assert_eq!(rx.try_pop(), model.pop_front()),
            }
            assert_eq!(tx.len(), model.len());
        }
    }
}

#[test]
fn w05_batch_edges() {
    let (mut tx, mut rx) = spsc::channel::<u8>(8);
    assert_eq!(tx.push_slice(&[]), 0);
    assert_eq!(rx.pop_into(&mut []), 0);
    assert_eq!(rx.pop_into(&mut [0; 4]), 0, "empty ring pops nothing");

    // More than fits: a partial push.
    let src: Vec<u8> = (0..12).collect();
    assert_eq!(tx.push_slice(&src), 8);
    assert_eq!(tx.push_slice(&src), 0, "full ring pushes nothing");

    // Pop 5, push 5 more: the second batch wraps around the end.
    let mut out = [0u8; 5];
    assert_eq!(rx.pop_into(&mut out), 5);
    assert_eq!(out, [0, 1, 2, 3, 4]);
    assert_eq!(tx.push_slice(&[100, 101, 102, 103, 104]), 5);
    let mut all = [0u8; 16];
    assert_eq!(rx.pop_into(&mut all), 8);
    assert_eq!(all[..8], [5, 6, 7, 100, 101, 102, 103, 104]);
    assert!(rx.is_empty());
}

#[test]
fn w05_spsc_batched_stress_in_order() {
    const N: u64 = 2_000_000;
    within(60, || {
        let (mut tx, mut rx) = spsc::channel::<u64>(256);
        let producer = std::thread::spawn(move || {
            let mut rng = Rng::new(1);
            let mut next = 0u64;
            let mut buf = Vec::with_capacity(64);
            let mut spins = 0;
            while next < N {
                let want = rng.range(1, 64).min(N - next);
                buf.clear();
                buf.extend(next..next + want);
                let mut sent = 0;
                while sent < buf.len() {
                    let n = tx.push_slice(&buf[sent..]);
                    if n == 0 {
                        backoff(&mut spins);
                    }
                    sent += n;
                }
                next += want;
            }
        });
        let mut rng = Rng::new(2);
        let mut expected = 0u64;
        let mut out = vec![0u64; 100];
        let mut owned = Vec::new();
        let mut spins = 0;
        while expected < N {
            let got: &[u64] = if rng.chance(0.2) {
                owned.clear();
                let n = rx.pop_batch(&mut owned, rng.range(1, 100) as usize);
                &owned[..n]
            } else {
                let n = rng.range(1, 100) as usize;
                let k = rx.pop_into(&mut out[..n]);
                &out[..k]
            };
            if got.is_empty() {
                backoff(&mut spins);
            }
            for &v in got {
                assert_eq!(v, expected, "batched transfer out of order");
                expected += 1;
            }
        }
        producer.join().unwrap();
        assert_eq!(rx.try_pop(), None);
    });
}

#[test]
fn w05_pop_batch_moves_owned_values() {
    let (mut tx, mut rx) = spsc::channel::<String>(4);
    for s in ["a", "b", "c"] {
        tx.try_push(s.to_string()).unwrap();
    }
    let mut out = vec!["kept".to_string()];
    assert_eq!(rx.pop_batch(&mut out, 2), 2);
    assert_eq!(out, ["kept", "a", "b"]);
    tx.try_push("d".into()).unwrap();
    tx.try_push("e".into()).unwrap();
    assert_eq!(rx.pop_batch(&mut out, 10), 3);
    assert_eq!(out, ["kept", "a", "b", "c", "d", "e"]);
    assert_eq!(rx.pop_batch(&mut out, 10), 0);
    assert_eq!(rx.pop_batch(&mut out, 0), 0);
}

#[test]
fn w05_mpsc_full_and_empty_edges() {
    let (tx, mut rx) = mpsc::channel::<u32>(0);
    assert_eq!(tx.capacity(), 2, "minimum capacity is 2");
    assert_eq!(rx.try_pop(), None);
    drop(tx);

    let (tx, mut rx) = mpsc::channel::<u32>(5);
    assert_eq!(tx.capacity(), 8);
    assert!(tx.is_empty() && rx.is_empty());
    let tx2 = tx.clone();
    for i in 0..8 {
        let producer = if i % 2 == 0 { &tx } else { &tx2 };
        producer.try_push(i).unwrap();
    }
    assert_eq!(tx.try_push(8), Err(8), "a full queue hands the value back");
    assert_eq!(tx2.try_push(9), Err(9));
    assert_eq!(rx.len(), 8);
    assert_eq!(rx.try_pop(), Some(0));
    tx2.try_push(8).unwrap();
    for i in 1..=8 {
        assert_eq!(rx.try_pop(), Some(i));
    }
    assert_eq!(rx.try_pop(), None);
    assert!(tx.is_empty());
}

#[test]
fn w05_mpsc_four_producers_exactly_once_in_per_producer_order() {
    // Two producers use `try_push`, two use `push`: the claim styles mix.
    const PRODUCERS: u64 = 4;
    const PER: u64 = 250_000;
    within(60, || {
        let (tx, mut rx) = mpsc::channel::<u64>(64);
        let producers: Vec<_> = (0..PRODUCERS)
            .map(|p| {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let mut spins = 0;
                    for seq in 0..PER {
                        let mut v = (p << 32) | seq;
                        if p % 2 == 0 {
                            // Even producers: non-blocking CAS claims.
                            while let Err(back) = tx.try_push(v) {
                                v = back;
                                backoff(&mut spins);
                            }
                        } else {
                            // Odd producers: fetch_add claims, waiting
                            // while the queue is full.
                            tx.push(v).unwrap();
                        }
                    }
                })
            })
            .collect();
        drop(tx);
        let mut next = [0u64; PRODUCERS as usize];
        let mut received = 0;
        let mut spins = 0;
        while received < PRODUCERS * PER {
            match rx.try_pop() {
                Some(v) => {
                    let (p, seq) = ((v >> 32) as usize, v & 0xFFFF_FFFF);
                    assert_eq!(
                        seq, next[p],
                        "producer {p}: out of order, lost or duplicated"
                    );
                    next[p] += 1;
                    received += 1;
                }
                None => backoff(&mut spins),
            }
        }
        for p in producers {
            p.join().unwrap();
        }
        assert_eq!(rx.try_pop(), None, "an extra item appeared");
        assert!(next.iter().all(|&n| n == PER));
    });
}

#[test]
fn w05_mpsc_drops_unconsumed_items_once() {
    let drops = Arc::new(AtomicUsize::new(0));
    let (tx, mut rx) = mpsc::channel::<Tracked>(16);
    let tx2 = tx.clone();
    for id in 0..10 {
        let t = if id % 2 == 0 { &tx } else { &tx2 };
        assert!(t.try_push(Tracked::new(id, &drops)).is_ok());
    }
    for id in 0..4 {
        assert_eq!(rx.try_pop().map(|t| t.id), Some(id));
    }
    assert_eq!(drops.load(Ordering::SeqCst), 4);
    drop(rx);
    // Producers still alive: the queued items must not be freed yet.
    assert_eq!(drops.load(Ordering::SeqCst), 4);
    drop(tx);
    drop(tx2);
    assert_eq!(drops.load(Ordering::SeqCst), 10);
}

#[test]
fn w05_mpsc_push_waits_for_space_and_gives_up_without_consumer() {
    within(30, || {
        let drops = Arc::new(AtomicUsize::new(0));
        let (tx, mut rx) = mpsc::channel::<Tracked>(2);
        tx.push(Tracked::new(0, &drops)).ok().unwrap();
        tx.push(Tracked::new(1, &drops)).ok().unwrap();
        assert!(tx.try_push(Tracked::new(9, &drops)).is_err());
        assert_eq!(drops.load(Ordering::SeqCst), 1);

        // A push into a full queue waits until the consumer makes room.
        let waiting = {
            let (tx, drops) = (tx.clone(), Arc::clone(&drops));
            std::thread::spawn(move || tx.push(Tracked::new(2, &drops)).is_ok())
        };
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(
            !waiting.is_finished(),
            "push must wait while the queue is full"
        );
        assert_eq!(rx.try_pop().map(|t| t.id), Some(0));
        assert!(waiting.join().unwrap());
        assert_eq!(rx.try_pop().map(|t| t.id), Some(1));
        assert_eq!(rx.try_pop().map(|t| t.id), Some(2));
        assert_eq!(drops.load(Ordering::SeqCst), 4);

        // Fill it again, then drop the consumer under a waiting push: the
        // push gives the value back instead of spinning forever.
        tx.push(Tracked::new(3, &drops)).ok().unwrap();
        tx.push(Tracked::new(4, &drops)).ok().unwrap();
        let stuck = {
            let (tx, drops) = (tx.clone(), Arc::clone(&drops));
            std::thread::spawn(move || tx.push(Tracked::new(5, &drops)).err().map(|t| t.id))
        };
        std::thread::sleep(std::time::Duration::from_millis(20));
        drop(rx);
        assert_eq!(stuck.join().unwrap(), Some(5));
        assert_eq!(drops.load(Ordering::SeqCst), 5);
        // The abandoned claim is not mistaken for a value: exactly the two
        // queued items are dropped with the queue.
        drop(tx);
        assert_eq!(drops.load(Ordering::SeqCst), 7);
    });
}
