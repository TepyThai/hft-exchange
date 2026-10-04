//! Week 1: the blocking SPMC channel and the lock-free SPSC ring.

mod common;

use common::{backoff, within, Rng, Tracked};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use sync::{spmc, spsc};

#[test]
fn w01_spmc_fifo_and_disconnect() {
    let (tx, rx) = spmc::channel::<u32>();
    assert_eq!(rx.try_recv(), Err(spmc::TryRecvError::Empty));
    for i in 0..1000 {
        tx.send(i).unwrap();
    }
    assert_eq!(rx.len(), 1000);
    drop(tx);
    // Queued messages survive the sender: drain them in order, then None.
    for i in 0..1000 {
        assert_eq!(rx.recv(), Some(i));
    }
    assert_eq!(rx.recv(), None);
    assert_eq!(rx.try_recv(), Err(spmc::TryRecvError::Disconnected));

    // Sending with every receiver gone hands the value back.
    let (tx, rx) = spmc::channel::<String>();
    let rx2 = rx.clone();
    drop(rx);
    tx.send("still one receiver".into()).unwrap();
    drop(rx2);
    let err = tx.send("nobody listens".into()).unwrap_err();
    assert_eq!(err.0, "nobody listens");
}

#[test]
fn w01_spmc_blocked_receivers_wake_when_sender_drops() {
    within(20, || {
        let (tx, rx) = spmc::channel::<u64>();
        let waiters: Vec<_> = (0..4)
            .map(|_| {
                let rx = rx.clone();
                std::thread::spawn(move || rx.recv())
            })
            .collect();
        // Let them block in recv, then hang up: every one must wake.
        std::thread::sleep(Duration::from_millis(20));
        drop(tx);
        for w in waiters {
            assert_eq!(w.join().unwrap(), None);
        }
    });
}

#[test]
fn w01_spmc_each_message_delivered_exactly_once() {
    const N: u64 = 200_000;
    let received = within(60, || {
        let (tx, rx) = spmc::channel::<u64>();
        let consumers: Vec<_> = (0..4)
            .map(|_| {
                let rx = rx.clone();
                std::thread::spawn(move || {
                    let mut got = Vec::new();
                    while let Some(v) = rx.recv() {
                        got.push(v);
                    }
                    got
                })
            })
            .collect();
        drop(rx);
        for i in 0..N {
            tx.send(i).unwrap();
        }
        drop(tx);
        consumers
            .into_iter()
            .map(|c| c.join().unwrap())
            .collect::<Vec<_>>()
    });
    for got in &received {
        // One queue: each consumer sees its share in send order.
        assert!(
            got.windows(2).all(|w| w[0] < w[1]),
            "a consumer saw messages out of order"
        );
    }
    let mut all: Vec<u64> = received.into_iter().flatten().collect();
    all.sort_unstable();
    assert_eq!(all.len() as u64, N, "lost or duplicated messages");
    assert!(all.iter().enumerate().all(|(i, &v)| v == i as u64));
}

#[test]
fn w01_spsc_capacity_full_and_empty_edges() {
    let (tx, mut rx) = spsc::channel::<u8>(0);
    assert_eq!(tx.capacity(), 1, "capacity rounds up to at least 1");
    assert_eq!(rx.try_pop(), None);
    drop(tx);

    let (mut tx, mut rx) = spsc::channel::<u32>(5);
    assert_eq!(tx.capacity(), 8, "capacity rounds up to a power of two");
    assert_eq!(rx.capacity(), 8);
    assert!(tx.is_empty() && rx.is_empty());
    for i in 0..8 {
        tx.try_push(i).unwrap();
    }
    assert!(tx.is_full());
    assert_eq!(tx.len(), 8);
    assert_eq!(tx.try_push(99), Err(99), "a full ring hands the value back");
    assert_eq!(rx.try_pop(), Some(0));
    tx.try_push(8).unwrap();
    assert_eq!(tx.try_push(100), Err(100));
    for i in 1..=8 {
        assert_eq!(rx.try_pop(), Some(i));
    }
    assert_eq!(rx.try_pop(), None);
    assert_eq!(rx.len(), 0);
}

#[test]
fn w01_spsc_matches_vecdeque_model() {
    // Randomized differential test, single-threaded: many laps around a
    // small ring, every result compared with a VecDeque.
    let mut rng = Rng::new(0x5_95C);
    for cap in [1usize, 2, 4, 8, 16] {
        let (mut tx, mut rx) = spsc::channel::<u64>(cap);
        let mut model = VecDeque::new();
        let mut next = 0u64;
        for _ in 0..50_000 {
            if rng.chance(0.55) {
                let r = tx.try_push(next);
                if model.len() < cap {
                    assert_eq!(r, Ok(()));
                    model.push_back(next);
                } else {
                    assert_eq!(r, Err(next));
                }
                next += 1;
            } else {
                assert_eq!(rx.try_pop(), model.pop_front());
            }
            assert_eq!(rx.len(), model.len());
        }
    }
}

#[test]
fn w01_spsc_cross_thread_1m_in_order() {
    const N: u64 = 1_000_000;
    within(60, || {
        let (mut tx, mut rx) = spsc::channel::<u64>(1024);
        let producer = std::thread::spawn(move || {
            let (mut i, mut spins) = (0, 0);
            while i < N {
                match tx.try_push(i) {
                    Ok(()) => i += 1,
                    Err(_) => backoff(&mut spins),
                }
            }
        });
        let (mut expected, mut spins) = (0, 0);
        while expected < N {
            match rx.try_pop() {
                Some(v) => {
                    assert_eq!(v, expected, "out of order, lost or duplicated");
                    expected += 1;
                }
                None => backoff(&mut spins),
            }
        }
        producer.join().unwrap();
        assert_eq!(rx.try_pop(), None);
    });
}

#[test]
fn w01_spsc_moves_owned_values_and_drops_leftovers() {
    let drops = Arc::new(AtomicUsize::new(0));
    let (mut tx, mut rx) = spsc::channel::<Tracked>(4);
    for id in 0..4 {
        assert!(tx.try_push(Tracked::new(id, &drops)).is_ok());
    }
    // A rejected push gives the value back (and does not drop it).
    let back = tx.try_push(Tracked::new(9, &drops)).err().unwrap();
    assert_eq!(back.id, 9);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(back);
    let first = rx.try_pop().unwrap();
    assert_eq!(first.id, 0);
    drop(first);
    assert_eq!(drops.load(Ordering::SeqCst), 2);
    // Three items still queued: dropping both ends drops each exactly once.
    drop(tx);
    assert_eq!(
        drops.load(Ordering::SeqCst),
        2,
        "items must live while the consumer exists"
    );
    drop(rx);
    assert_eq!(drops.load(Ordering::SeqCst), 5);

    // Heap-owning values cross threads intact.
    let (mut tx, mut rx) = spsc::channel::<String>(2);
    let t = std::thread::spawn(move || {
        for i in 0..10_000 {
            let mut s = format!("msg-{i}");
            while let Err(back) = tx.try_push(s) {
                s = back;
                std::thread::yield_now();
            }
        }
    });
    let mut i = 0;
    while i < 10_000 {
        if let Some(s) = rx.try_pop() {
            assert_eq!(s, format!("msg-{i}"));
            i += 1;
        } else {
            std::thread::yield_now();
        }
    }
    t.join().unwrap();
}
