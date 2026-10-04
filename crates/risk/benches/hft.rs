//! Benchmarks for the `risk` crate. Run with `cargo xtask bench --week N`.
//!
//! Each benchmark builds a realistic working set first (1,024 accounts × 4
//! instruments, tens of thousands of open orders or positions), then times
//! the hot-path call over a pre-generated stream of 4,096 inputs, in
//! batches (the calls are cheaper than a timer read).

use benchkit::{black_box, Report, Unit};
use risk::{
    BucketConfig, FakeClock, Fill, Limits, Positions, PostTradeLimits, RiskEngine, SessionId,
    Throttle,
};
use types::{AccountId, InstrumentId, NewOrder, OrderId, Price, Qty, Rng};

const ACCOUNTS: u32 = 1_024;
const INSTRUMENTS: u32 = 4;
const STREAM: usize = 4_096;
const BATCHES: u64 = 200_000;
const BATCH: u64 = 50;

fn main() {
    benchkit::run(&[
        ("w15.risk_check", "Full pre-trade check", bench_risk_check),
        (
            "w17.throttle_check",
            "Token bucket check",
            bench_throttle_check,
        ),
        (
            "w45.fill_update",
            "Apply fill to positions + PnL",
            bench_fill_update,
        ),
    ]);
}

/// A random order: mostly within limits, ~1% each too large or outside the
/// price band (production flow is overwhelmingly valid; a stream of random
/// rejects would mostly measure branch mispredictions).
fn random_order(rng: &mut Rng, id: u64) -> NewOrder {
    let qty = if rng.chance(0.01) {
        rng.range(101, 200)
    } else {
        rng.range(1, 100)
    };
    let mut o = if rng.chance(0.05) {
        NewOrder::market(id, rng.side(), qty)
    } else if rng.chance(0.01) {
        NewOrder::limit(id, rng.side(), rng.range_i64(1_060, 1_100), qty)
    } else {
        NewOrder::limit(id, rng.side(), rng.range_i64(960, 1_040), qty)
    };
    o.account = AccountId(rng.range(0, ACCOUNTS as u64 - 1) as u32);
    o.instrument = InstrumentId(rng.range(1, INSTRUMENTS as u64) as u32);
    o
}

/// `RiskEngine::check` against 1,024 configured accounts with ~60k open
/// orders and filled positions. ~95% of the stream passes (every check
/// runs); the rest hits each reject reason.
fn bench_risk_check() -> Report {
    let reference = Price(1_000);
    let base = Limits {
        max_order_qty: Qty(100),
        max_notional: 100_000,
        max_position: Qty(2_000),
        price_band_ticks: 50,
    };
    let mut r = RiskEngine::with_capacity(base, (ACCOUNTS * INSTRUMENTS) as usize, 65_536);
    let mut rng = Rng::new(15);
    for a in 0..ACCOUNTS {
        let limits = Limits {
            max_position: Qty(rng.range(600, 3_000)),
            ..base
        };
        r.set_limits(AccountId(a), limits);
        for i in 1..=INSTRUMENTS {
            r.set_position(AccountId(a), InstrumentId(i), rng.range_i64(-400, 400));
        }
    }
    for id in 0..60_000 {
        let o = random_order(&mut rng, id);
        if r.check(&o, reference).is_ok() {
            r.on_new(&o);
        }
    }
    for a in (0..ACCOUNTS).step_by(97) {
        r.kill_account(AccountId(a));
    }
    let stream: Vec<NewOrder> = (0..STREAM as u64)
        .map(|i| random_order(&mut rng, 1_000_000 + i))
        .collect();
    let mut i = 0;
    benchkit::latency_batched(
        "w15.risk_check",
        "Full pre-trade check",
        Unit::Ns,
        BATCHES,
        BATCH,
        || {
            let o = &stream[i % STREAM];
            i += 1;
            black_box(r.check(black_box(o), reference)).ok();
        },
    )
}

/// `Throttle::check_at` (session bucket + account bucket) over 4,096
/// sessions of 1,024 accounts, one order every 20ns: each session sends
/// slightly faster than its 12k/s rate, so a few percent are rejected.
fn bench_throttle_check() -> Report {
    let sessions = 4 * ACCOUNTS as u64;
    let mut t = Throttle::new(
        BucketConfig::per_second(12_000, 20),
        BucketConfig::per_second(50_000, 80),
        FakeClock::new(0),
    );
    for s in 0..sessions {
        t.check_at(SessionId(s), AccountId((s % ACCOUNTS as u64) as u32), 0)
            .ok();
    }
    // Every session once per pass over the stream, in random order.
    let mut rng = Rng::new(17);
    let mut order: Vec<u64> = (0..sessions).collect();
    for k in (1..order.len()).rev() {
        order.swap(k, rng.range(0, k as u64) as usize);
    }
    let stream: Vec<(SessionId, AccountId)> = order
        .iter()
        .map(|&s| (SessionId(s), AccountId((s % ACCOUNTS as u64) as u32)))
        .collect();
    let (mut i, mut now) = (0, 0u64);
    benchkit::latency_batched(
        "w17.throttle_check",
        "Token bucket check",
        Unit::Ns,
        BATCHES,
        BATCH,
        || {
            let (s, a) = stream[i % STREAM];
            i += 1;
            now += 20;
            black_box(t.check_at(black_box(s), a, now)).ok();
        },
    )
}

/// `Positions::apply_fill` over 4,096 open (account, instrument) positions
/// with post-trade limits checked on every fill; fills open, add, reduce and
/// flip positions.
fn bench_fill_update() -> Report {
    let limits = PostTradeLimits {
        max_loss: 1_000_000_000_000,
        max_gross_exposure: 1_000_000_000_000_000,
    };
    let mut book = Positions::new(limits);
    let mut rng = Rng::new(45);
    let random_fill = |rng: &mut Rng| Fill {
        order: OrderId(0),
        account: AccountId(rng.range(0, ACCOUNTS as u64 - 1) as u32),
        instrument: InstrumentId(rng.range(1, INSTRUMENTS as u64) as u32),
        side: rng.side(),
        price: Price(rng.range_i64(990, 1_010)),
        qty: Qty(rng.range(1, 50)),
    };
    for a in 0..ACCOUNTS {
        for i in 1..=INSTRUMENTS {
            let f = Fill {
                account: AccountId(a),
                instrument: InstrumentId(i),
                ..random_fill(&mut rng)
            };
            book.apply_fill(&f).ok();
        }
    }
    for i in 1..=INSTRUMENTS {
        book.set_mark(InstrumentId(i), Price(1_000), |_, _| {});
    }
    let stream: Vec<Fill> = (0..STREAM).map(|_| random_fill(&mut rng)).collect();
    let mut i = 0;
    benchkit::latency_batched(
        "w45.fill_update",
        "Apply fill to positions + PnL",
        Unit::Ns,
        BATCHES,
        BATCH,
        || {
            let f = &stream[i % STREAM];
            i += 1;
            black_box(book.apply_fill(black_box(f))).ok();
        },
    )
}
