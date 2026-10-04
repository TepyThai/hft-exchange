//! Benchmarks for the `wire` crate. Run with `cargo xtask bench --week N`.

use benchkit::{black_box, Report, Unit};
use types::{
    AccountId, Command, Event, InstrumentId, NewOrder, OrderId, OrderKind, Price, Qty,
    RejectReason, Rng, Side, TimeInForce,
};
use wire::schema::{OrdType, Version};
use wire::{decode, encode_frame, encode_into, FrameDecoder, MarketData, Message, MAX_MSG_LEN};

fn main() {
    benchkit::run(&[
        (
            "w11.decode",
            "Decode one 40-byte order message",
            bench_decode,
        ),
        (
            "w11.decode_throughput",
            "Decode stream of mixed messages",
            bench_decode_throughput,
        ),
        (
            "w35.decode",
            "Generated decoder, one message",
            bench_schema_decode,
        ),
    ]);
}

fn order() -> NewOrder {
    NewOrder {
        id: OrderId(0x1234_5678),
        account: AccountId(17),
        instrument: InstrumentId(3),
        side: Side::Sell,
        qty: Qty(250),
        kind: OrderKind::Limit {
            price: Price(10_050),
            post_only: false,
        },
        tif: TimeInForce::Ioc,
    }
}

/// `wire::decode` of one 40-byte `NewOrder`: tag dispatch, length and field
/// validation, materializing a `Message`.
fn bench_decode() -> Report {
    let mut buf = [0u8; MAX_MSG_LEN];
    let n = encode_into(&Message::Command(Command::New(order())), &mut buf);
    assert_eq!(n, 40);
    benchkit::latency_batched(
        "w11.decode",
        "Decode one 40-byte order message",
        Unit::Ns,
        20_000,
        1_000,
        || {
            let msg = decode(black_box(&buf[..])).unwrap();
            black_box(msg);
        },
    )
}

/// A plausible order-entry + market-data mix: mostly orders, cancels and
/// their acks, some fills and book updates.
fn mixed_message(rng: &mut Rng, i: u64) -> Message {
    let id = OrderId(i);
    let price = Price(10_000 + rng.range_i64(-50, 50));
    let qty = Qty(rng.range(1, 500));
    let instrument = InstrumentId(rng.range(1, 8) as u32);
    match rng.range(0, 99) {
        0..=24 => Message::Command(Command::New(NewOrder {
            id,
            account: AccountId(rng.range(1, 100) as u32),
            instrument,
            side: rng.side(),
            qty,
            kind: OrderKind::Limit {
                price,
                post_only: rng.chance(0.1),
            },
            tif: TimeInForce::Gtc,
        })),
        25..=39 => Message::Command(Command::Cancel { id }),
        40..=44 => Message::Command(Command::Reduce { id, qty }),
        45..=59 => Message::Event(Event::Accepted { id }),
        60..=61 => Message::Event(Event::Rejected {
            id,
            reason: RejectReason::PriceBand,
        }),
        62..=69 => Message::Event(Event::Trade {
            taker: id,
            maker: OrderId(i / 2),
            taker_side: rng.side(),
            price,
            qty,
        }),
        70..=77 => Message::Event(Event::Rested {
            id,
            side: rng.side(),
            price,
            qty,
        }),
        78..=83 => Message::Event(Event::Cancelled { id, remaining: qty }),
        84..=87 => Message::MarketData(MarketData::AddOrder {
            instrument,
            id,
            side: rng.side(),
            price,
            qty,
        }),
        88..=91 => Message::MarketData(MarketData::OrderDelete { instrument, id }),
        _ => Message::MarketData(MarketData::L2Update {
            instrument,
            side: rng.side(),
            price,
            qty,
        }),
    }
}

/// 100k framed messages arriving in 64 KiB reads (so a frame straddles
/// every read boundary): `FrameDecoder` + `decode` per message.
fn bench_decode_throughput() -> Report {
    const N: u64 = 100_000;
    const READ: usize = 64 * 1024;
    let mut rng = Rng::new(11);
    let mut stream = vec![0u8; N as usize * (2 + MAX_MSG_LEN)];
    let mut len = 0;
    for i in 0..N {
        len += encode_frame(&mixed_message(&mut rng, i), &mut stream[len..]);
    }
    stream.truncate(len);
    let mut dec = FrameDecoder::default();
    benchkit::throughput(
        "w11.decode_throughput",
        "Decode stream of mixed messages",
        Unit::MsgsPerSec,
        N,
        || {
            let mut count = 0u64;
            for chunk in black_box(&stream[..]).chunks(READ) {
                let mut input = chunk;
                while let Some(frame) = dec.next_frame(&mut input).unwrap() {
                    black_box(decode(frame).unwrap());
                    count += 1;
                }
            }
            assert_eq!(count, N);
        },
    )
}

/// The generated decoder: header, template and block checks, enumerated
/// field validation, and an owned copy of a 55-byte v2 `NewOrder`.
fn bench_schema_decode() -> Report {
    let msg = wire::schema::NewOrder {
        account: 17,
        id: 0x1234_5678,
        qty: 250,
        price: 10_050,
        instrument: 3,
        side: Side::Sell,
        ord_type: OrdType::Limit,
        tif: TimeInForce::Ioc,
        client_ts: Some(1_790_000_000_000_000_000),
        client_tag: None,
    };
    let mut buf = [0u8; 64];
    let n = msg.encode_into(Version::V2, &mut buf);
    assert_eq!(n, 55);
    benchkit::latency_batched(
        "w35.decode",
        "Generated decoder, one message",
        Unit::Ns,
        20_000,
        1_000,
        || {
            let m = wire::schema::NewOrder::decode(black_box(&buf[..n])).unwrap();
            black_box(m);
        },
    )
}
