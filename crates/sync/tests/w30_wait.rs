//! Week 30: wait strategies and thread pinning.

mod common;

use common::{within, Rng};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use sync::{spsc, BusySpin, SpinPark, SpinYield, WaitStrategy};

fn check_ready_and_deadline<S: WaitStrategy>(name: &str, s: &S) {
    // Already ready: returns at once, even with a deadline in the past.
    assert!(s.wait_for(|| true, Some(Instant::now())), "{name}");
    assert!(s.wait_for(|| true, None), "{name}");
    // Never ready: gives up at the deadline, not much later.
    let t = Instant::now();
    assert!(
        !s.wait_for(|| false, Some(t + Duration::from_millis(20))),
        "{name}"
    );
    let waited = t.elapsed();
    assert!(
        waited >= Duration::from_millis(20),
        "{name}: returned early ({waited:?})"
    );
    assert!(
        waited < Duration::from_secs(2),
        "{name}: overshot the deadline ({waited:?})"
    );
}

#[test]
fn w30_strategies_honour_ready_and_deadline() {
    check_ready_and_deadline("BusySpin", &BusySpin);
    check_ready_and_deadline("SpinYield", &SpinYield::new(100));
    check_ready_and_deadline("SpinPark", &SpinPark::default());
    // A deadline shorter than max_park must still be honoured.
    check_ready_and_deadline(
        "SpinPark(max_park=10s)",
        &SpinPark::new(10, Duration::from_secs(10)),
    );
}

fn ping<S: WaitStrategy + 'static>(strategy: S, rounds: u64) {
    let strategy = Arc::new(strategy);
    let flag = Arc::new(AtomicU64::new(0));
    let waiter = {
        let (strategy, flag) = (Arc::clone(&strategy), Arc::clone(&flag));
        std::thread::spawn(move || {
            for round in 1..=rounds {
                strategy.wait_until(|| flag.load(Ordering::Acquire) >= round);
                assert!(flag.load(Ordering::Acquire) >= round);
            }
        })
    };
    let mut rng = Rng::new(rounds);
    for round in 1..=rounds {
        if rng.chance(0.3) {
            std::thread::sleep(Duration::from_micros(rng.range(10, 300)));
        }
        flag.store(round, Ordering::Release);
        strategy.notify();
    }
    waiter.join().unwrap();
}

#[test]
fn w30_every_strategy_wakes_on_a_cross_thread_signal() {
    within(60, || {
        ping(BusySpin, 2_000);
        ping(SpinYield::new(50), 2_000);
        // max_park far beyond the test's patience: only `notify` can wake it.
        ping(SpinPark::new(50, Duration::from_secs(30)), 2_000);
    });
}

#[test]
fn w30_spin_park_parks_and_notify_unparks() {
    within(30, || {
        let strategy = Arc::new(SpinPark::new(100, Duration::from_secs(30)));
        let flag = Arc::new(AtomicU64::new(0));
        let waiter = {
            let (strategy, flag) = (Arc::clone(&strategy), Arc::clone(&flag));
            std::thread::spawn(move || {
                strategy.wait_until(|| flag.load(Ordering::Acquire) == 1);
                Instant::now()
            })
        };
        // The waiter spins briefly, then parks.
        while !strategy.is_parked() {
            std::thread::yield_now();
        }
        std::thread::sleep(Duration::from_millis(10));
        // Still parked (allowing for a spurious wakeup that re-parks).
        let t = Instant::now();
        while !strategy.is_parked() {
            assert!(
                t.elapsed() < Duration::from_millis(100),
                "a waiter with nothing to do must stay parked"
            );
            std::thread::yield_now();
        }
        let sent = Instant::now();
        flag.store(1, Ordering::Release);
        strategy.notify();
        let woke = waiter.join().unwrap();
        assert!(
            woke.duration_since(sent) < Duration::from_secs(5),
            "notify did not unpark the waiter"
        );
        assert!(!strategy.is_parked());
    });
}

#[test]
fn w30_spin_park_never_loses_a_wakeup() {
    // A consumer that parks after a few polls, a producer that pauses at
    // random: if any notify were lost, the consumer would sleep for
    // max_park (30s) and the watchdog would fire.
    const N: u64 = 200_000;
    within(60, || {
        let (mut tx, mut rx) = spsc::channel::<u64>(64);
        let strategy = Arc::new(SpinPark::new(20, Duration::from_secs(30)));
        let consumer = {
            let strategy = Arc::clone(&strategy);
            std::thread::spawn(move || {
                let mut expected = 0;
                while expected < N {
                    strategy.wait_until(|| !rx.is_empty());
                    while let Some(v) = rx.try_pop() {
                        assert_eq!(v, expected);
                        expected += 1;
                    }
                }
            })
        };
        let mut rng = Rng::new(30);
        let mut i = 0;
        while i < N {
            if tx.try_push(i).is_ok() {
                i += 1;
                strategy.notify();
            } else {
                std::thread::yield_now();
            }
            if rng.chance(0.0005) {
                std::thread::sleep(Duration::from_micros(rng.range(1, 200)));
            }
        }
        consumer.join().unwrap();
    });
}

#[test]
fn w30_pin_current_thread_sets_affinity() {
    let result = std::thread::spawn(|| {
        let allowed = sync::current_affinity().unwrap();
        assert!(!allowed.is_empty());
        let cpu = *allowed.last().unwrap();
        sync::pin_current_thread(cpu).unwrap();
        if cfg!(target_os = "linux") {
            assert_eq!(
                sync::current_affinity().unwrap(),
                vec![cpu],
                "thread is not pinned to cpu {cpu}"
            );
            assert!(
                sync::pin_current_thread(1 << 20).is_err(),
                "a nonexistent CPU must be an error"
            );
            // A failed call leaves the previous mask in place.
            assert_eq!(sync::current_affinity().unwrap(), vec![cpu]);
        }
    })
    .join();
    assert!(result.is_ok());
}
