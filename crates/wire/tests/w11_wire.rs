//! Week 11: fixed-layout codec, zero-copy views, and framing over partial
//! reads.

use types::{
    AccountId, Command, Event, InstrumentId, NewOrder, OrderId, OrderKind, Price, Qty,
    RejectReason, RiskReason, Rng, Side, TimeInForce,
};
use wire::{
    decode, encode_frame, encode_into, DecodeError, FrameDecoder, FrameError, InboundView, Logon,
    LogonAck, MarketData, Message, MsgType, NewOrderView, MAX_MSG_LEN,
};

// ---------------------------------------------------------------------------
// Helpers

const REASONS: [RejectReason; 16] = [
    RejectReason::DuplicateOrderId,
    RejectReason::UnknownOrder,
    RejectReason::ZeroQuantity,
    RejectReason::InvalidTick,
    RejectReason::InvalidLot,
    RejectReason::PriceBand,
    RejectReason::WouldTake,
    RejectReason::FokUnfillable,
    RejectReason::NoLiquidity,
    RejectReason::InvalidReduce,
    RejectReason::Halted,
    RejectReason::Risk(RiskReason::MaxOrderQty),
    RejectReason::Risk(RiskReason::MaxNotional),
    RejectReason::Risk(RiskReason::PositionLimit),
    RejectReason::Risk(RiskReason::RateLimit),
    RejectReason::Risk(RiskReason::KillSwitch),
];

fn tif(rng: &mut Rng) -> TimeInForce {
    [TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok][rng.range(0, 2) as usize]
}

/// A random valid message of a random type, with full-range field values.
fn random_message(rng: &mut Rng) -> Message {
    let id = OrderId(rng.next_u64());
    let qty = Qty(rng.next_u64());
    let price = Price(rng.next_u64() as i64);
    let instrument = InstrumentId(rng.next_u64() as u32);
    match rng.range(0, 15) {
        0 => Message::Logon(Logon {
            account: AccountId(rng.next_u64() as u32),
            token: rng.next_u64(),
        }),
        1 => Message::LogonAck(LogonAck {
            account: AccountId(rng.next_u64() as u32),
            session: rng.next_u64(),
        }),
        2 => {
            let kind = if rng.chance(0.2) {
                OrderKind::Market
            } else {
                OrderKind::Limit {
                    price,
                    post_only: rng.chance(0.3),
                }
            };
            Message::Command(Command::New(NewOrder {
                id,
                account: AccountId(rng.next_u64() as u32),
                instrument,
                side: rng.side(),
                qty,
                kind,
                tif: tif(rng),
            }))
        }
        3 => Message::Command(Command::Cancel { id }),
        4 => Message::Command(Command::Reduce { id, qty }),
        5 => Message::Event(Event::Accepted { id }),
        6 => Message::Event(Event::Rejected {
            id,
            reason: REASONS[rng.range(0, 15) as usize],
        }),
        7 => Message::Event(Event::Trade {
            taker: id,
            maker: OrderId(rng.next_u64()),
            taker_side: rng.side(),
            price,
            qty,
        }),
        8 => Message::Event(Event::Rested {
            id,
            side: rng.side(),
            price,
            qty,
        }),
        9 => Message::Event(Event::Cancelled { id, remaining: qty }),
        10 => Message::Event(Event::Expired { id, remaining: qty }),
        11 => Message::Event(Event::Reduced { id, remaining: qty }),
        12 => Message::MarketData(MarketData::AddOrder {
            instrument,
            id,
            side: rng.side(),
            price,
            qty,
        }),
        13 => Message::MarketData(MarketData::OrderExecuted {
            instrument,
            id,
            price,
            qty,
        }),
        14 => Message::MarketData(MarketData::OrderDelete { instrument, id }),
        _ => Message::MarketData(MarketData::L2Update {
            instrument,
            side: rng.side(),
            price,
            qty,
        }),
    }
}

fn encode(msg: &Message) -> Vec<u8> {
    let mut buf = [0u8; MAX_MSG_LEN];
    let n = encode_into(msg, &mut buf);
    buf[..n].to_vec()
}

/// `n` random messages and the framed byte stream carrying them.
fn random_stream(rng: &mut Rng, n: usize) -> (Vec<Message>, Vec<u8>) {
    let msgs: Vec<Message> = (0..n).map(|_| random_message(rng)).collect();
    let mut stream = vec![0u8; n * (2 + MAX_MSG_LEN)];
    let mut len = 0;
    for m in &msgs {
        len += encode_frame(m, &mut stream[len..]);
    }
    stream.truncate(len);
    (msgs, stream)
}

fn post_only_order() -> NewOrder {
    NewOrder {
        id: OrderId(42),
        account: AccountId(7),
        instrument: InstrumentId(3),
        side: Side::Buy,
        qty: Qty(100),
        kind: OrderKind::Limit {
            price: Price(10_050),
            post_only: true,
        },
        tif: TimeInForce::Gtc,
    }
}

#[rustfmt::skip]
const NEW_ORDER_GOLDEN: [u8; 40] = [
    b'N', b'B', b'L', b'G', 0x01, 0, 0, 0,          // tag, side, ord type, tif, flags, res
    0x2a, 0, 0, 0, 0, 0, 0, 0,                       // id 42
    0x07, 0, 0, 0,                                   // account 7
    0x03, 0, 0, 0,                                   // instrument 3
    0x64, 0, 0, 0, 0, 0, 0, 0,                       // qty 100
    0x42, 0x27, 0, 0, 0, 0, 0, 0,                    // price 10_050
];

// ---------------------------------------------------------------------------
// Tests

#[test]
fn w11_golden_new_order_bytes() {
    let msg = Message::Command(Command::New(post_only_order()));
    assert_eq!(msg.wire_len(), 40);
    assert_eq!(encode(&msg), NEW_ORDER_GOLDEN);
    assert_eq!(decode(&NEW_ORDER_GOLDEN), Ok(msg));

    // A market order: type `M`, no flags, price 0.
    let market = NewOrder::market(5, Side::Sell, 9).with_tif(TimeInForce::Fok);
    let bytes = encode(&Message::Command(Command::New(market)));
    assert_eq!(&bytes[..8], &[b'N', b'S', b'M', b'F', 0, 0, 0, 0]);
    assert_eq!(&bytes[32..40], &[0; 8]);
    assert_eq!(decode(&bytes), Ok(Message::Command(Command::New(market))));

    // Encoding overwrites whatever was in the buffer, reserved bytes included.
    let mut dirty = [0xAAu8; 64];
    assert_eq!(encode_into(&msg, &mut dirty), 40);
    assert_eq!(dirty[..40], NEW_ORDER_GOLDEN);
    assert_eq!(dirty[40], 0xAA, "must not write past the message");
}

#[test]
fn w11_golden_event_and_market_data_bytes() {
    #[rustfmt::skip]
    let cases: Vec<(Message, Vec<u8>)> = vec![
        (
            Message::Event(Event::Trade {
                taker: OrderId(9), maker: OrderId(4), taker_side: Side::Sell, price: Price(-5), qty: Qty(300),
            }),
            [
                &[b't', b'S', 0, 0, 0, 0, 0, 0][..],
                &[9, 0, 0, 0, 0, 0, 0, 0],
                &[4, 0, 0, 0, 0, 0, 0, 0],
                &[0xfb, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
                &[0x2c, 0x01, 0, 0, 0, 0, 0, 0],
            ].concat(),
        ),
        (
            Message::Event(Event::Rejected { id: OrderId(77), reason: RejectReason::Risk(RiskReason::RateLimit) }),
            vec![b'j', 35, 0, 0, 0, 0, 0, 0, 77, 0, 0, 0, 0, 0, 0, 0],
        ),
        (
            Message::Event(Event::Cancelled { id: OrderId(0x0102_0304_0506_0708), remaining: Qty(1) }),
            vec![b'c', 0, 0, 0, 0, 0, 0, 0, 8, 7, 6, 5, 4, 3, 2, 1, 1, 0, 0, 0, 0, 0, 0, 0],
        ),
        (
            Message::MarketData(MarketData::L2Update {
                instrument: InstrumentId(2), side: Side::Sell, price: Price(10_000), qty: Qty(0),
            }),
            vec![b'P', b'S', 0, 0, 2, 0, 0, 0, 0x10, 0x27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        ),
        (
            Message::Logon(Logon { account: AccountId(0x0a0b), token: 0xdead_beef }),
            vec![b'L', 1, 0, 0, 0x0b, 0x0a, 0, 0, 0xef, 0xbe, 0xad, 0xde, 0, 0, 0, 0],
        ),
    ];
    for (msg, golden) in cases {
        assert_eq!(encode(&msg), golden, "encode {msg:?}");
        assert_eq!(decode(&golden), Ok(msg), "decode {msg:?}");
    }

    // Framing: u16 LE payload length, then the payload.
    let mut buf = [0u8; 2 + MAX_MSG_LEN];
    let n = encode_frame(
        &Message::Command(Command::Cancel { id: OrderId(1) }),
        &mut buf,
    );
    assert_eq!(
        buf[..n],
        [16, 0, b'X', 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]
    );
}

#[test]
fn w11_roundtrip_random_messages() {
    let mut rng = Rng::new(11);
    let mut seen = std::collections::HashSet::new();
    for _ in 0..20_000 {
        let msg = random_message(&mut rng);
        let bytes = encode(&msg);
        assert_eq!(bytes.len(), msg.msg_type().wire_len());
        assert_eq!(bytes[0], msg.msg_type().tag());
        assert_eq!(MsgType::from_tag(bytes[0]), Some(msg.msg_type()));
        let back = decode(&bytes).unwrap_or_else(|e| panic!("{msg:?}: {e}"));
        assert_eq!(back, msg);
        // Bytes past the fixed length are ignored (forward compatibility).
        let mut longer = bytes.clone();
        longer.extend_from_slice(&[0xEE; 7]);
        assert_eq!(decode(&longer), Ok(msg));
        seen.insert(msg.msg_type());
    }
    assert_eq!(
        seen.len(),
        MsgType::ALL.len(),
        "every message type exercised"
    );
}

#[test]
fn w11_views_read_fields_in_place() {
    let order = post_only_order();
    let bytes = encode(&Message::Command(Command::New(order)));
    let v = NewOrderView::new(&bytes).unwrap();
    assert_eq!(v.id(), OrderId(42));
    assert_eq!(v.account(), AccountId(7));
    assert_eq!(v.instrument(), InstrumentId(3));
    assert_eq!(v.side(), Side::Buy);
    assert_eq!(v.tif(), TimeInForce::Gtc);
    assert_eq!(v.qty(), Qty(100));
    assert_eq!(v.price(), Price(10_050));
    assert_eq!(
        v.kind(),
        OrderKind::Limit {
            price: Price(10_050),
            post_only: true
        }
    );
    assert_eq!(v.to_new_order(), order);
    // Zero copy: the view reads the caller's bytes.
    assert_eq!(v.as_bytes().as_ptr(), bytes.as_ptr());

    // InboundView dispatches every client→exchange message.
    let mut rng = Rng::new(7);
    for _ in 0..2_000 {
        let msg = random_message(&mut rng);
        let bytes = encode(&msg);
        match (msg, InboundView::parse(&bytes)) {
            (Message::Command(cmd), Ok(view)) => assert_eq!(view.to_command(), Some(cmd)),
            (Message::Logon(l), Ok(InboundView::Logon(v))) => {
                assert_eq!(v.to_logon(), l);
                assert_eq!((v.account(), v.token()), (l.account, l.token));
            }
            (Message::Command(_) | Message::Logon(_), other) => panic!("{msg:?} → {other:?}"),
            // Exchange→client and market data are not valid inbound.
            (_, res) => assert_eq!(res.unwrap_err(), DecodeError::UnknownType(bytes[0])),
        }
    }
    // A view of the wrong type says so.
    let cancel = encode(&Message::Command(Command::Cancel { id: OrderId(1) }));
    assert_eq!(
        NewOrderView::new(&cancel).unwrap_err(),
        DecodeError::UnexpectedType {
            expected: b'N',
            found: b'X'
        }
    );
}

#[test]
fn w11_decode_rejects_malformed_input() {
    assert_eq!(
        decode(&[]),
        Err(DecodeError::Truncated { need: 1, have: 0 })
    );
    for n in 1..40 {
        assert_eq!(
            decode(&NEW_ORDER_GOLDEN[..n]),
            Err(DecodeError::Truncated { need: 40, have: n }),
            "prefix of {n} bytes"
        );
        assert!(NewOrderView::new(&NEW_ORDER_GOLDEN[..n]).is_err());
    }
    assert_eq!(decode(&[0xFF; 40]), Err(DecodeError::UnknownType(0xFF)));
    assert_eq!(decode(&[0; 40]), Err(DecodeError::UnknownType(0)));

    let corrupt = |at: usize, v: u8| {
        let mut b = NEW_ORDER_GOLDEN;
        b[at] = v;
        decode(&b)
    };
    assert_eq!(
        corrupt(1, b'X'),
        Err(DecodeError::InvalidField {
            field: "side",
            value: b'X' as u64
        })
    );
    assert_eq!(
        corrupt(2, b'Q'),
        Err(DecodeError::InvalidField {
            field: "ord_type",
            value: b'Q' as u64
        })
    );
    assert_eq!(
        corrupt(3, 0),
        Err(DecodeError::InvalidField {
            field: "tif",
            value: 0
        })
    );
    assert_eq!(
        corrupt(4, 0x03),
        Err(DecodeError::InvalidField {
            field: "flags",
            value: 3
        })
    );
    // Reserved bytes are ignored.
    assert!(corrupt(5, 0x99).is_ok());
    // A market order may not carry a price or flags.
    assert!(matches!(
        corrupt(2, b'M'),
        Err(DecodeError::InvalidField { field: "flags", .. })
    ));
    let mut market_with_price = NEW_ORDER_GOLDEN;
    market_with_price[2] = b'M';
    market_with_price[4] = 0;
    assert!(matches!(
        decode(&market_with_price),
        Err(DecodeError::InvalidField { field: "price", .. })
    ));

    // Unknown reject code, bad trade side, wrong protocol version.
    let rejected = encode(&Message::Event(Event::Rejected {
        id: OrderId(1),
        reason: RejectReason::Halted,
    }));
    let mut bad = rejected.clone();
    bad[1] = 0;
    assert_eq!(
        decode(&bad),
        Err(DecodeError::InvalidField {
            field: "reason",
            value: 0
        })
    );
    let mut trade = encode(&Message::Event(Event::Trade {
        taker: OrderId(1),
        maker: OrderId(2),
        taker_side: Side::Buy,
        price: Price(1),
        qty: Qty(1),
    }));
    trade[1] = b'b';
    assert!(matches!(
        decode(&trade),
        Err(DecodeError::InvalidField {
            field: "taker_side",
            ..
        })
    ));
    let mut logon = encode(&Message::Logon(Logon {
        account: AccountId(1),
        token: 1,
    }));
    logon[1] = 2;
    assert_eq!(decode(&logon), Err(DecodeError::UnsupportedVersion(2)));
}

#[test]
fn w11_frame_decoder_handles_random_chunking() {
    for seed in 0..12u64 {
        let mut rng = Rng::new(1100 + seed);
        let (msgs, stream) = random_stream(&mut rng, 3_000);
        // Chunk sizes from 1 byte (every header split) to several frames.
        let max_chunk = [1u64, 2, 3, 7, 17, 41, 64, 200, 1000, 5000, 4, 97][seed as usize];
        let mut dec = FrameDecoder::default();
        let mut got = Vec::with_capacity(msgs.len());
        let mut pos = 0;
        while pos < stream.len() {
            let n = (rng.range(1, max_chunk) as usize).min(stream.len() - pos);
            let mut input = &stream[pos..pos + n];
            while let Some(frame) = dec.next_frame(&mut input).unwrap() {
                got.push(decode(frame).unwrap());
            }
            assert!(
                input.is_empty(),
                "next_frame returns None only when input is used up"
            );
            pos += n;
        }
        assert_eq!(dec.buffered(), 0, "stream ended on a frame boundary");
        assert_eq!(got.len(), msgs.len(), "seed {seed}");
        assert!(
            got == msgs,
            "seed {seed}: frames decoded out of order or corrupted"
        );
    }

    // An empty read is harmless at any point.
    let mut dec = FrameDecoder::default();
    let mut empty: &[u8] = &[];
    assert_eq!(dec.next_frame(&mut empty), Ok(None));
}

#[test]
fn w11_frames_inside_a_read_are_not_copied() {
    let mut rng = Rng::new(3);
    let (msgs, stream) = random_stream(&mut rng, 100);
    let range = stream.as_ptr_range();

    // Whole stream in one read: every frame is a slice of the input.
    let mut dec = FrameDecoder::default();
    let mut input = &stream[..];
    let mut n = 0;
    while let Some(frame) = dec.next_frame(&mut input).unwrap() {
        assert!(range.contains(&frame.as_ptr()), "frame {n} was copied");
        assert_eq!(decode(frame).unwrap(), msgs[n]);
        n += 1;
    }
    assert_eq!(n, msgs.len());

    // Split in the middle of the second frame: only that frame is copied.
    let first = 2 + msgs[0].wire_len();
    let cut = first + 5;
    let (a, b) = stream.split_at(cut);
    let mut dec = FrameDecoder::default();
    let mut input = a;
    let f0 = dec.next_frame(&mut input).unwrap().unwrap();
    assert!(a.as_ptr_range().contains(&f0.as_ptr()));
    assert_eq!(dec.next_frame(&mut input), Ok(None));
    assert_eq!(dec.buffered(), 5, "partial frame held");
    let mut input = b;
    let f1 = dec.next_frame(&mut input).unwrap().unwrap();
    assert!(
        !b.as_ptr_range().contains(&f1.as_ptr()),
        "boundary frame comes from the decoder"
    );
    assert_eq!(decode(f1).unwrap(), msgs[1]);
    let f2 = dec.next_frame(&mut input).unwrap().unwrap();
    assert!(
        b.as_ptr_range().contains(&f2.as_ptr()),
        "frames after the boundary are zero-copy again"
    );
    assert_eq!(decode(f2).unwrap(), msgs[2]);
}

#[test]
fn w11_frame_decoder_rejects_bad_lengths() {
    let mut dec = FrameDecoder::default();
    let mut input: &[u8] = &[0, 0, b'X'];
    assert_eq!(dec.next_frame(&mut input), Err(FrameError::ZeroLength));

    let mut dec = FrameDecoder::new(40);
    let mut input: &[u8] = &[41, 0];
    assert_eq!(
        dec.next_frame(&mut input),
        Err(FrameError::TooLong { len: 41, max: 40 })
    );

    // Detected even when the header arrives one byte at a time.
    let mut dec = FrameDecoder::new(1024);
    let mut a: &[u8] = &[0xff];
    assert_eq!(dec.next_frame(&mut a), Ok(None));
    let mut b: &[u8] = &[0xff, 1, 2, 3];
    assert_eq!(
        dec.next_frame(&mut b),
        Err(FrameError::TooLong {
            len: 0xffff,
            max: 1024
        })
    );

    // The largest allowed frame passes, through the copy path too.
    let mut dec = FrameDecoder::new(40);
    let mut frame = vec![40, 0];
    frame.extend_from_slice(&NEW_ORDER_GOLDEN);
    let (x, y) = frame.split_at(17);
    let (mut x, mut y) = (x, y);
    assert_eq!(dec.next_frame(&mut x), Ok(None));
    assert_eq!(dec.next_frame(&mut y), Ok(Some(&NEW_ORDER_GOLDEN[..])));
}
