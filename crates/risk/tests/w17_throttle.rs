//! Week 17 — token buckets, sliding windows and the per-session /
//! per-account throttle, all driven by an injected clock.

mod common;

use risk::{
    BucketConfig, Clock, FakeClock, MonotonicClock, SessionId, SlidingWindow, Throttle, TokenBucket,
};
use types::{AccountId, RiskReason, Rng};

const LIMITED: Result<(), RiskReason> = Err(RiskReason::RateLimit);

#[test]
fn w17_bucket_allows_burst_then_one_token_per_interval() {
    let cfg = BucketConfig::per_second(1_000, 5);
    assert_eq!(
        cfg,
        BucketConfig {
            burst: 5,
            ns_per_token: 1_000_000
        }
    );
    assert_eq!(
        BucketConfig::per_second(3, 1).ns_per_token,
        333_333_334,
        "rounded up: never faster than asked"
    );

    let mut b = TokenBucket::new(cfg, 0);
    assert_eq!(b.available(0), 5);
    for _ in 0..5 {
        assert!(b.try_acquire(0));
    }
    assert!(!b.try_acquire(0), "burst exhausted");
    assert!(!b.try_acquire(999_999), "one ns short of a token");
    assert!(b.try_acquire(1_000_000));
    assert!(!b.try_acquire(1_000_000));
    assert_eq!(b.available(3_500_000), 2, "2.5 tokens earned, 2 whole");
    assert!(b.try_acquire(3_500_000) && b.try_acquire(3_500_000));
    assert!(!b.try_acquire(3_999_999));
    assert!(b.try_acquire(4_000_000), "the half token was kept");

    // Edge configurations.
    let mut unlimited = TokenBucket::new(BucketConfig::UNLIMITED, 0);
    assert!((0..1_000).all(|_| unlimited.try_acquire(0)));
    assert_eq!(unlimited.available(0), u64::MAX);
    let mut closed = TokenBucket::new(
        BucketConfig {
            burst: 0,
            ns_per_token: 10,
        },
        0,
    );
    assert!(!closed.try_acquire(0) && !closed.try_acquire(1_000_000));
}

#[test]
fn w17_refill_is_exact_under_irregular_calls() {
    // Integer credit: however irregular the calls, a client that always
    // wants a token gets exactly burst + floor(elapsed / ns_per_token).
    for (seed, ns_per_token, burst) in [(1, 7, 3), (2, 1_000, 2), (3, 333, 10)] {
        let mut rng = Rng::new(seed);
        let mut b = TokenBucket::new(
            BucketConfig {
                burst,
                ns_per_token,
            },
            0,
        );
        let (mut now, mut granted) = (0u64, 0u64);
        while b.try_acquire(0) {
            granted += 1;
        }
        assert_eq!(granted, burst);
        while now < 5_000_000 {
            // Gaps shorter than one token (and burst ≥ 2) keep the drained
            // bucket below its cap, so no credit is ever discarded.
            now += rng.range(0, ns_per_token - 1);
            while b.try_acquire(now) {
                granted += 1;
            }
            assert_eq!(granted, burst + now / ns_per_token, "seed {seed} at {now}");
        }
    }
}

#[test]
fn w17_long_idle_caps_at_capacity_and_time_never_runs_backwards() {
    let mut b = TokenBucket::new(
        BucketConfig {
            burst: 5,
            ns_per_token: 100,
        },
        0,
    );
    while b.try_acquire(0) {}
    // An hour of idling earns only `burst` tokens.
    let hour = 3_600_000_000_000;
    assert_eq!(b.available(hour), 5);
    for _ in 0..5 {
        assert!(b.try_acquire(hour));
    }
    assert!(!b.try_acquire(hour));

    // A timestamp from the past adds nothing and does not rewind time.
    assert!(!b.try_acquire(hour - 1_000_000));
    assert!(!b.try_acquire(hour + 99));
    assert!(b.try_acquire(hour + 100));

    // Extreme values saturate instead of overflowing.
    let mut big = TokenBucket::new(
        BucketConfig {
            burst: u64::MAX,
            ns_per_token: u64::MAX,
        },
        u64::MAX - 10,
    );
    assert!(big.try_acquire(u64::MAX));
    assert!(!big.try_acquire(u64::MAX));
    let mut late = TokenBucket::new(
        BucketConfig {
            burst: 2,
            ns_per_token: 1,
        },
        u64::MAX - 1,
    );
    assert!(late.try_acquire(u64::MAX - 1) && late.try_acquire(u64::MAX - 1));
    assert!(!late.try_acquire(u64::MAX - 1));
    assert!(late.try_acquire(u64::MAX), "one ns, one token");
    assert!(!late.try_acquire(u64::MAX));
}

#[test]
fn w17_sessions_are_isolated_and_account_caps_their_sum() {
    let clock = FakeClock::new(0);
    let per_session = BucketConfig {
        burst: 10,
        ns_per_token: 1_000,
    };
    let per_account = BucketConfig {
        burst: 15,
        ns_per_token: 500,
    };
    let mut t = Throttle::new(per_session, per_account, clock.clone());
    let (a, b) = (AccountId(1), AccountId(2));

    // A flooding session gets its own burst only...
    let flood = (0..100)
        .filter(|_| t.check(SessionId(1), a).is_ok())
        .count();
    assert_eq!(flood, 10);
    // ...and does not touch a session of another account,
    assert_eq!(
        (0..100)
            .filter(|_| t.check(SessionId(2), b).is_ok())
            .count(),
        10
    );
    // while a second session of the same account shares the account's 15.
    assert_eq!(
        (0..100)
            .filter(|_| t.check(SessionId(3), a).is_ok())
            .count(),
        5
    );
    assert_eq!(t.account_tokens(a, 0), Some(0));

    // Sustained: session 1 floods (20 orders per µs) while session 2 sends
    // at exactly its rate (1 per µs). Session 2 never loses an order;
    // session 1 is held to its own rate.
    let (mut s1, mut s2) = (0, 0);
    for _ in 0..1_000 {
        clock.advance(1_000);
        s1 += (0..20).filter(|_| t.check(SessionId(1), a).is_ok()).count();
        s2 += t.check(SessionId(2), b).is_ok() as usize;
    }
    assert_eq!(s2, 1_000);
    assert_eq!(s1, 1_000, "one token per µs, no more");
}

#[test]
fn w17_rejection_charges_neither_bucket() {
    let clock = FakeClock::new(0);
    let mut t = Throttle::new(
        BucketConfig {
            burst: 3,
            ns_per_token: 1_000,
        },
        BucketConfig {
            burst: 3,
            ns_per_token: 1_000,
        },
        &clock,
    );
    let a = AccountId(7);
    // Session 1 drains the account.
    for _ in 0..3 {
        assert_eq!(t.check(SessionId(1), a), Ok(()));
    }
    // Session 2 has tokens but the account has none: session 2 keeps them.
    for _ in 0..10 {
        assert_eq!(t.check(SessionId(2), a), LIMITED);
    }
    assert_eq!(t.session_tokens(SessionId(2), 0), Some(3));
    // Session 1 is empty while the account refills: the account keeps it.
    clock.advance(1_000);
    assert_eq!(t.check(SessionId(1), a), Ok(()));
    assert_eq!(t.check(SessionId(1), a), LIMITED);
    assert_eq!(t.account_tokens(a, 1_000), Some(0));
    clock.advance(2_000);
    assert_eq!(t.account_tokens(a, 3_000), Some(2));
    assert_eq!(t.check(SessionId(1), a), Ok(()));
    assert_eq!(t.check(SessionId(1), a), Ok(()));
    assert_eq!(t.check(SessionId(2), a), LIMITED, "account empty again");
    assert_eq!(t.session_tokens(SessionId(2), 3_000), Some(3));
    assert_eq!(t.session_tokens(SessionId(9), 3_000), None);
}

#[test]
fn w17_sliding_window_weights_previous_window() {
    let mut w = SlidingWindow::new(10, 1_000, 0);
    assert_eq!((0..20).filter(|_| w.try_acquire(0)).count(), 10);
    assert_eq!(w.estimate(999), 10);
    // New window, but the previous one still counts fully at its start...
    assert!(!w.try_acquire(1_000));
    // ...half at mid-window: 10 × 0.5 + 5 = 10.
    assert_eq!((0..20).filter(|_| w.try_acquire(1_500)).count(), 5);
    assert_eq!(w.estimate(1_500), 10);
    // 10 × 0.1 + 9 = 10.
    assert_eq!((0..20).filter(|_| w.try_acquire(1_900)).count(), 4);
    // Next window: the previous one admitted 9.
    assert_eq!((0..20).filter(|_| w.try_acquire(2_000)).count(), 1);
    // After a gap of more than one window, nothing carries over.
    assert_eq!((0..20).filter(|_| w.try_acquire(5_000)).count(), 10);
    assert_eq!(w.estimate(5_999), 10);
    assert_eq!(w.estimate(7_000), 0);
    // A timestamp from the past counts as the current window's start.
    let mut back = SlidingWindow::new(2, 100, 1_000);
    assert!(back.try_acquire(500) && back.try_acquire(1_000));
    assert!(!back.try_acquire(1_050));
}

/// Reference token bucket: credit added one nanosecond at a time.
struct TickBucket {
    credit: u64,
    cap: u64,
    cost: u64,
    now: u64,
}

impl TickBucket {
    fn try_acquire(&mut self, now: u64) -> bool {
        while self.now < now {
            self.now += 1;
            if self.credit < self.cap {
                self.credit += 1;
            }
        }
        if self.credit >= self.cost {
            self.credit -= self.cost;
            true
        } else {
            false
        }
    }
}

/// Reference sliding window: decides from the full log of admitted times.
fn window_allows(log: &[u64], origin: u64, window: u64, limit: u64, now: u64) -> bool {
    let start = origin + (now - origin) / window * window;
    let curr = log.iter().filter(|&&t| t >= start).count() as u64;
    let prev = if start >= origin + window {
        log.iter()
            .filter(|&&t| t >= start - window && t < start)
            .count() as u64
    } else {
        0
    };
    let elapsed = now - start;
    prev as u128 * (window - elapsed) as u128 + (curr + 1) as u128 * window as u128
        <= limit as u128 * window as u128
}

#[test]
fn w17_randomized_limiters_match_reference_models() {
    let mut rng = Rng::new(17);
    for round in 0..20 {
        let (burst, cost) = (rng.range(1, 6), rng.range(1, 40));
        let mut fast = TokenBucket::new(
            BucketConfig {
                burst,
                ns_per_token: cost,
            },
            0,
        );
        let mut slow = TickBucket {
            credit: burst * cost,
            cap: burst * cost,
            cost,
            now: 0,
        };
        let (limit, window, origin) = (rng.range(1, 8), rng.range(10, 200), rng.range(0, 1_000));
        let mut sw = SlidingWindow::new(limit, window, origin);
        let mut log = Vec::new();
        let mut now = 0;
        for i in 0..3_000 {
            now += rng.range(0, 3 * cost);
            assert_eq!(
                fast.try_acquire(now),
                slow.try_acquire(now),
                "round {round} call {i} at {now}"
            );
            let t = origin + now;
            let want = window_allows(&log, origin, window, limit, t);
            assert_eq!(sw.try_acquire(t), want, "round {round} call {i} at {t}");
            if want {
                log.push(t);
            }
        }
        // Never more than `limit` in one fixed window.
        let mut per_window = std::collections::HashMap::new();
        for t in &log {
            *per_window.entry((t - origin) / window).or_insert(0u64) += 1;
        }
        assert!(per_window.values().all(|&n| n <= limit));
    }
}

#[test]
fn w17_throttle_reads_injected_clock_and_never_allocates() {
    let real = MonotonicClock::new();
    let (t0, t1) = (real.now_ns(), real.now_ns());
    assert!(t1 >= t0);

    let clock = FakeClock::new(1_000);
    let mut t = Throttle::new(
        BucketConfig {
            burst: 2,
            ns_per_token: 100,
        },
        BucketConfig::UNLIMITED,
        clock.clone(),
    );
    assert_eq!(t.clock().now_ns(), 1_000);
    let (s, a) = (SessionId(1), AccountId(1));
    assert_eq!(t.check(s, a), Ok(()));
    assert_eq!(t.check(s, a), Ok(()));
    assert_eq!(t.check(s, a), LIMITED);
    clock.advance(100);
    assert_eq!(t.check(s, a), Ok(()), "check() reads the injected clock");
    // A per-account override, and a session that logs out and back in:
    // its bucket restarts, the account's does not.
    t.set_account_limit(
        a,
        BucketConfig {
            burst: 1,
            ns_per_token: 1_000_000,
        },
    );
    assert_eq!(t.check(s, a), LIMITED, "session empty");
    t.remove_session(s);
    assert_eq!(t.check(s, a), Ok(()));
    assert_eq!(
        t.check(SessionId(2), a),
        LIMITED,
        "account override: burst 1"
    );

    // After the first order of each session/account, checks never allocate.
    let mut t = Throttle::new(
        BucketConfig::per_second(1_000_000, 50),
        BucketConfig::per_second(2_000_000, 100),
        FakeClock::new(0),
    );
    for i in 0..64 {
        let _ = t.check_at(SessionId(i), AccountId(i as u32 % 8), 0);
    }
    let mut rng = Rng::new(3);
    let mut ok = 0;
    let n = common::allocations_in(|| {
        for now in 0..100_000u64 {
            let i = rng.range(0, 63);
            ok += t
                .check_at(SessionId(i), AccountId(i as u32 % 8), now * 50)
                .is_ok() as u32;
        }
    });
    assert_eq!(n, 0, "check_at allocated");
    assert!(ok > 1_000 && ok < 100_000, "{ok}");
}
