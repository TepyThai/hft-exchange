//! Benchmarks for the `fix` crate. Run with `cargo xtask bench --week N`.

use benchkit::{black_box, Report, Unit};
use fix::{Decimal, Encoder, NewOrderSingle, OrdType, PriceScale, RawMessage, StdHeader, Tif};
use types::{AccountId, InstrumentId, OrderId, Side};

fn main() {
    benchkit::run(&[("w14.parse_nos", "Parse NewOrderSingle", bench_parse_nos)]);
}

/// A typical 162-byte limit NewOrderSingle: validate BodyLength and
/// CheckSum, parse the standard header, decode the typed body, and map it
/// to a `types::NewOrder` (price string → ticks).
fn bench_parse_nos() -> Report {
    let nos = NewOrderSingle {
        cl_ord_id: b"ORD-0001",
        account: Some(b"ACC42"),
        symbol: b"AAPL",
        side: Side::Buy,
        transact_time: b"20261004-12:30:00.000",
        order_qty: Decimal::parse(b"100").unwrap(),
        ord_type: OrdType::Limit,
        price: Decimal::parse(b"187.25"),
        time_in_force: Tif::Day,
        post_only: false,
    };
    let header = StdHeader {
        sender: b"CLIENT1",
        target: b"EXCH",
        seq_num: 2,
        sending_time: b"20261004-12:30:00.000",
        poss_dup: false,
    };
    let mut msg = Vec::new();
    Encoder::new().encode(&header, &nos, &mut msg);
    assert_eq!(msg.len(), 162);
    benchkit::latency_batched(
        "w14.parse_nos",
        "Parse NewOrderSingle",
        Unit::Ns,
        20_000,
        200,
        || {
            let raw = RawMessage::parse(black_box(&msg[..])).unwrap();
            let order = NewOrderSingle::decode(&raw)
                .unwrap()
                .to_new_order(OrderId(1), AccountId(7), InstrumentId(3), PriceScale::CENTS)
                .unwrap();
            black_box(order);
        },
    )
}
