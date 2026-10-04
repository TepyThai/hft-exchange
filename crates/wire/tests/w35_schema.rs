//! Week 35: schema-driven codecs — conformance vectors, versioning, forward
//! compatibility, and the generated layout.

use types::{Rng, Side, TimeInForce};
use wire::schema::{
    decode_message, Cancel, CancelView, FieldInfo, Header, NewOrder, NewOrderView, OrdType, Reduce,
    SchemaMessage, Trade, TradeView, Version, HEADER_LEN,
};
use wire::{write_frame, DecodeError, FrameDecoder};

fn order() -> NewOrder {
    NewOrder {
        account: 7,
        id: 42,
        qty: 100,
        price: 10_050,
        instrument: 3,
        side: Side::Buy,
        ord_type: OrdType::PostOnly,
        tif: TimeInForce::Gtc,
        client_ts: Some(0x1122_3344_5566_7788),
        client_tag: None,
    }
}

fn encode(msg: &SchemaMessage, version: Version) -> Vec<u8> {
    let mut buf = [0u8; 128];
    let n = msg.encode_into(version, &mut buf);
    buf[..n].to_vec()
}

#[rustfmt::skip]
const NEW_ORDER_V1: [u8; 39] = [
    1, 1, 35, 0,                       // template 1, version 1, block 35
    7, 0, 0, 0,                        // account
    42, 0, 0, 0, 0, 0, 0, 0,           // id
    100, 0, 0, 0, 0, 0, 0, 0,          // qty
    0x42, 0x27, 0, 0, 0, 0, 0, 0,      // price 10_050
    3, 0, 0, 0,                        // instrument
    b'B', b'P', b'G',                  // side, ord_type, tif
];

#[rustfmt::skip]
const NEW_ORDER_V2: [u8; 55] = [
    1, 2, 51, 0,                       // template 1, version 2, block 51
    7, 0, 0, 0,
    42, 0, 0, 0, 0, 0, 0, 0,
    100, 0, 0, 0, 0, 0, 0, 0,
    0x42, 0x27, 0, 0, 0, 0, 0, 0,
    3, 0, 0, 0,
    b'B', b'P', b'G',
    0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11,   // client_ts
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,   // client_tag: null
];

#[test]
fn w35_golden_vectors_v1_and_v2() {
    let msg = SchemaMessage::NewOrder(order());
    assert_eq!(encode(&msg, Version::V1), NEW_ORDER_V1);
    assert_eq!(encode(&msg, Version::V2), NEW_ORDER_V2);
    assert_eq!(NewOrder::encoded_len(Version::V1), 39);
    assert_eq!(NewOrder::encoded_len(Version::V2), 55);

    #[rustfmt::skip]
    let cases: Vec<(SchemaMessage, Version, Vec<u8>)> = vec![
        (
            SchemaMessage::Cancel(Cancel { id: 0x0102, client_ts: Some(5) }),
            Version::V1,
            vec![2, 1, 8, 0, 0x02, 0x01, 0, 0, 0, 0, 0, 0],
        ),
        (
            SchemaMessage::Cancel(Cancel { id: 0x0102, client_ts: Some(5) }),
            Version::V2,
            vec![2, 2, 16, 0, 0x02, 0x01, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0],
        ),
        (
            SchemaMessage::Reduce(Reduce { id: 9, qty: 4, client_ts: None }),
            Version::V2,
            [&[3, 2, 24, 0][..], &[9, 0, 0, 0, 0, 0, 0, 0], &[4, 0, 0, 0, 0, 0, 0, 0], &[0xff; 8]].concat(),
        ),
        (
            SchemaMessage::Trade(Trade {
                taker: 1, maker: 2, price: -3, qty: 4, taker_side: Side::Sell,
                match_id: Some(6), exec_ts: Some(7),
            }),
            Version::V2,
            [
                &[4, 2, 49, 0][..],
                &[1, 0, 0, 0, 0, 0, 0, 0],
                &[2, 0, 0, 0, 0, 0, 0, 0],
                &[0xfd, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
                &[4, 0, 0, 0, 0, 0, 0, 0],
                b"S",
                &[6, 0, 0, 0, 0, 0, 0, 0],
                &[7, 0, 0, 0, 0, 0, 0, 0],
            ].concat(),
        ),
    ];
    for (msg, version, golden) in cases {
        assert_eq!(encode(&msg, version), golden, "{msg:?} as {version:?}");
    }
}

#[test]
fn w35_decoder_accepts_v1_and_v2() {
    // v1: the v2 fields are absent.
    let v1 = NewOrderView::new(&NEW_ORDER_V1).unwrap();
    assert_eq!(v1.version(), 1);
    assert_eq!(v1.message_len(), 39);
    assert_eq!(
        (v1.account(), v1.id(), v1.qty(), v1.price()),
        (7, 42, 100, 10_050)
    );
    assert_eq!(
        (v1.instrument(), v1.side(), v1.ord_type(), v1.tif()),
        (3, Side::Buy, OrdType::PostOnly, TimeInForce::Gtc)
    );
    assert_eq!((v1.client_ts(), v1.client_tag()), (None, None));
    let want_v1 = NewOrder {
        client_ts: None,
        client_tag: None,
        ..order()
    };
    assert_eq!(NewOrder::decode(&NEW_ORDER_V1), Ok(want_v1));

    // v2: present, and null reads as None.
    let v2 = NewOrderView::new(&NEW_ORDER_V2).unwrap();
    assert_eq!(v2.version(), 2);
    assert_eq!(v2.message_len(), 55);
    assert_eq!(v2.client_ts(), Some(0x1122_3344_5566_7788));
    assert_eq!(v2.client_tag(), None);
    assert_eq!(NewOrder::decode(&NEW_ORDER_V2), Ok(order()));

    // The generic decoder dispatches on the template and reports the length.
    assert_eq!(
        decode_message(&NEW_ORDER_V1),
        Ok((SchemaMessage::NewOrder(want_v1), 39))
    );
    assert_eq!(
        decode_message(&NEW_ORDER_V2),
        Ok((SchemaMessage::NewOrder(order()), 55))
    );

    // Both map to the same engine command.
    let cmd = SchemaMessage::NewOrder(order()).to_command().unwrap();
    assert_eq!(SchemaMessage::NewOrder(want_v1).to_command(), Some(cmd));
    let types::Command::New(o) = cmd else {
        panic!()
    };
    assert_eq!(o.limit_price(), Some(types::Price(10_050)));
    assert_eq!(
        o.kind,
        types::OrderKind::Limit {
            price: types::Price(10_050),
            post_only: true
        }
    );
}

#[test]
fn w35_newer_versions_are_skipped_not_rejected() {
    // A v3 sender appended a u8 and a u64: version 3, block 51 + 9.
    let mut v3 = NEW_ORDER_V2.to_vec();
    v3[1] = 3;
    v3[2] = 60;
    v3.push(0xAB);
    v3.extend_from_slice(&[0x5A; 8]);
    let view = NewOrderView::new(&v3).unwrap();
    assert_eq!(view.version(), 3);
    assert_eq!(view.message_len(), 64);
    assert_eq!(view.to_msg(), order());

    // A stream of concatenated messages of mixed versions is walked with the
    // lengths from the headers — including the fields this decoder does not know.
    let cancel = encode(
        &SchemaMessage::Cancel(Cancel {
            id: 5,
            client_ts: None,
        }),
        Version::V1,
    );
    let trade_msg = Trade {
        taker: 1,
        maker: 2,
        price: 3,
        qty: 4,
        taker_side: Side::Buy,
        match_id: Some(9),
        exec_ts: None,
    };
    let trade = encode(&SchemaMessage::Trade(trade_msg), Version::V2);
    let stream = [&v3[..], &cancel, &trade, &NEW_ORDER_V1].concat();
    let mut pos = 0;
    let mut got = Vec::new();
    while pos < stream.len() {
        let (msg, used) = decode_message(&stream[pos..]).unwrap();
        got.push(msg);
        pos += used;
    }
    assert_eq!(got.len(), 4);
    assert_eq!(got[0], SchemaMessage::NewOrder(order()));
    assert_eq!(
        got[1],
        SchemaMessage::Cancel(Cancel {
            id: 5,
            client_ts: None
        })
    );
    assert_eq!(got[2], SchemaMessage::Trade(trade_msg));
    assert!(matches!(
        got[3],
        SchemaMessage::NewOrder(NewOrder {
            client_ts: None,
            ..
        })
    ));
    assert_eq!(
        TradeView::new(&trade).unwrap().to_msg().to_event(),
        trade_msg.to_event()
    );
}

#[test]
fn w35_rejects_bad_headers_and_fields() {
    assert_eq!(
        decode_message(&[]),
        Err(DecodeError::Truncated {
            need: HEADER_LEN,
            have: 0
        })
    );
    assert_eq!(
        decode_message(&[1, 1, 35]),
        Err(DecodeError::Truncated {
            need: HEADER_LEN,
            have: 3
        })
    );
    // Version 0 is never valid.
    let mut v0 = NEW_ORDER_V1;
    v0[1] = 0;
    assert_eq!(decode_message(&v0), Err(DecodeError::UnsupportedVersion(0)));
    // Unknown template.
    let mut unknown = NEW_ORDER_V1;
    unknown[0] = 99;
    assert_eq!(decode_message(&unknown), Err(DecodeError::UnknownType(99)));
    // The header promises more bytes than there are.
    assert_eq!(
        decode_message(&NEW_ORDER_V2[..54]),
        Err(DecodeError::Truncated { need: 55, have: 54 })
    );
    // Version 2 needs the v2 block.
    let mut short = NEW_ORDER_V1.to_vec();
    short[1] = 2;
    assert_eq!(
        NewOrder::decode(&short),
        Err(DecodeError::BlockTooShort { need: 51, have: 35 })
    );
    let mut tiny = NEW_ORDER_V1.to_vec();
    tiny[2] = 34;
    assert_eq!(
        NewOrder::decode(&tiny),
        Err(DecodeError::BlockTooShort { need: 35, have: 34 })
    );
    // Wrong template for a typed view.
    assert_eq!(
        CancelView::new(&NEW_ORDER_V1).unwrap_err(),
        DecodeError::UnexpectedType {
            expected: Cancel::TEMPLATE,
            found: NewOrder::TEMPLATE
        }
    );
    // Enumerated fields are validated once, in `new`.
    for (at, field) in [(36, "side"), (37, "ord_type"), (38, "tif")] {
        let mut bad = NEW_ORDER_V2;
        bad[at] = b'?';
        assert_eq!(
            NewOrder::decode(&bad),
            Err(DecodeError::InvalidField {
                field,
                value: b'?' as u64
            })
        );
    }
    // A header round-trips.
    let mut h = [0u8; 4];
    Header {
        template: 4,
        version: 2,
        block_len: 0x0131,
    }
    .write(&mut h);
    assert_eq!(h, [4, 2, 0x31, 0x01]);
}

/// A random message with random optional fields (never the null value).
fn random_message(rng: &mut Rng) -> SchemaMessage {
    let opt = |rng: &mut Rng| {
        if rng.chance(0.5) {
            Some(rng.next_u64() % u64::MAX)
        } else {
            None
        }
    };
    match rng.range(0, 3) {
        0 => SchemaMessage::NewOrder(NewOrder {
            account: rng.next_u64() as u32,
            id: rng.next_u64(),
            qty: rng.next_u64(),
            price: rng.next_u64() as i64,
            instrument: rng.next_u64() as u32,
            side: rng.side(),
            ord_type: [OrdType::Limit, OrdType::Market, OrdType::PostOnly]
                [rng.range(0, 2) as usize],
            tif: [TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok][rng.range(0, 2) as usize],
            client_ts: opt(rng),
            client_tag: opt(rng),
        }),
        1 => SchemaMessage::Cancel(Cancel {
            id: rng.next_u64(),
            client_ts: opt(rng),
        }),
        2 => SchemaMessage::Reduce(Reduce {
            id: rng.next_u64(),
            qty: rng.next_u64(),
            client_ts: opt(rng),
        }),
        _ => SchemaMessage::Trade(Trade {
            taker: rng.next_u64(),
            maker: rng.next_u64(),
            price: rng.next_u64() as i64,
            qty: rng.next_u64(),
            taker_side: rng.side(),
            match_id: opt(rng),
            exec_ts: opt(rng),
        }),
    }
}

/// What a decoder sees from a message sent as v1: the v2 fields are gone.
fn as_v1(msg: SchemaMessage) -> SchemaMessage {
    match msg {
        SchemaMessage::NewOrder(m) => SchemaMessage::NewOrder(NewOrder {
            client_ts: None,
            client_tag: None,
            ..m
        }),
        SchemaMessage::Cancel(m) => SchemaMessage::Cancel(Cancel {
            client_ts: None,
            ..m
        }),
        SchemaMessage::Reduce(m) => SchemaMessage::Reduce(Reduce {
            client_ts: None,
            ..m
        }),
        SchemaMessage::Trade(m) => SchemaMessage::Trade(Trade {
            match_id: None,
            exec_ts: None,
            ..m
        }),
    }
}

#[test]
fn w35_roundtrip_random_messages_through_frames() {
    let mut rng = Rng::new(35);
    let mut stream = Vec::new();
    let mut expected = Vec::new();
    for _ in 0..10_000 {
        let msg = random_message(&mut rng);
        let version = if rng.chance(0.5) {
            Version::V1
        } else {
            Version::V2
        };
        let bytes = encode(&msg, version);
        let want = if version == Version::V1 {
            as_v1(msg)
        } else {
            msg
        };
        assert_eq!(decode_message(&bytes), Ok((want, bytes.len())));
        // Re-encoding what was decoded gives the same bytes.
        assert_eq!(encode(&want, version), bytes);
        // Carry it over week 11's framing too.
        let mut frame = [0u8; 130];
        let n = write_frame(&bytes, &mut frame);
        stream.extend_from_slice(&frame[..n]);
        expected.push(want);
    }
    let mut dec = FrameDecoder::default();
    let mut got = Vec::new();
    for chunk in stream.chunks(rng.range(1, 300) as usize) {
        let mut input = chunk;
        while let Some(frame) = dec.next_frame(&mut input).unwrap() {
            let (msg, used) = decode_message(frame).unwrap();
            assert_eq!(used, frame.len());
            got.push(msg);
        }
    }
    assert!(got == expected);
}

#[test]
fn w35_generated_layout_matches_the_schema() {
    let f = |name, offset, size, since| FieldInfo {
        name,
        offset,
        size,
        since,
    };
    assert_eq!(
        NewOrder::FIELDS,
        &[
            f("account", 4, 4, 1),
            f("id", 8, 8, 1),
            f("qty", 16, 8, 1),
            f("price", 24, 8, 1),
            f("instrument", 32, 4, 1),
            f("side", 36, 1, 1),
            f("ord_type", 37, 1, 1),
            f("tif", 38, 1, 1),
            f("client_ts", 39, 8, 2),
            f("client_tag", 47, 8, 2),
        ]
    );
    assert_eq!((NewOrder::V1_BLOCK, NewOrder::V2_BLOCK), (35, 51));
    assert_eq!((Cancel::V1_BLOCK, Cancel::V2_BLOCK), (8, 16));
    assert_eq!((Reduce::V1_BLOCK, Reduce::V2_BLOCK), (16, 24));
    assert_eq!((Trade::V1_BLOCK, Trade::V2_BLOCK), (33, 49));
    assert_eq!(
        Trade::FIELDS.last().map(|f| (f.name, f.offset)),
        Some(("exec_ts", 45))
    );

    // The generated accessors read exactly where FIELDS says: give every
    // u64 field a distinct value and look for it at the advertised offset.
    let mut rng = Rng::new(99);
    for _ in 0..100 {
        let m = Trade {
            taker: rng.next_u64(),
            maker: rng.next_u64(),
            price: rng.next_u64() as i64,
            qty: rng.next_u64(),
            taker_side: rng.side(),
            match_id: Some(rng.next_u64() >> 1),
            exec_ts: Some(rng.next_u64() >> 1),
        };
        let bytes = encode(&SchemaMessage::Trade(m), Version::V2);
        let at = |name: &str| {
            Trade::FIELDS
                .iter()
                .find(|f| f.name == name)
                .unwrap()
                .offset
        };
        let u64_at = |o: usize| u64::from_le_bytes(bytes[o..o + 8].try_into().unwrap());
        let view = TradeView::new(&bytes).unwrap();
        assert_eq!(u64_at(at("taker")), view.taker());
        assert_eq!(u64_at(at("maker")), view.maker());
        assert_eq!(u64_at(at("price")) as i64, view.price());
        assert_eq!(u64_at(at("qty")), view.qty());
        assert_eq!(
            bytes[at("taker_side")],
            if view.taker_side() == Side::Buy {
                b'B'
            } else {
                b'S'
            }
        );
        assert_eq!(Some(u64_at(at("match_id"))), view.match_id());
        assert_eq!(Some(u64_at(at("exec_ts"))), view.exec_ts());
    }
}

#[test]
fn w35_null_value_means_absent() {
    let m = Reduce {
        id: 1,
        qty: 2,
        client_ts: None,
    };
    let bytes = encode(&SchemaMessage::Reduce(m), Version::V2);
    assert_eq!(
        &bytes[20..28],
        &[0xff; 8],
        "None is written as the null value"
    );
    assert_eq!(Reduce::decode(&bytes), Ok(m));
    // The null value itself cannot be carried: Some(NULL) reads back as None.
    let edge = Reduce {
        client_ts: Some(u64::MAX),
        ..m
    };
    assert_eq!(
        Reduce::decode(&encode(&SchemaMessage::Reduce(edge), Version::V2)),
        Ok(m)
    );
    // The largest non-null value survives.
    let max = Reduce {
        client_ts: Some(u64::MAX - 1),
        ..m
    };
    assert_eq!(
        Reduce::decode(&encode(&SchemaMessage::Reduce(max), Version::V2)),
        Ok(max)
    );
    // Signed null is i64::MIN: a negative price is not null.
    let t = Trade {
        taker: 1,
        maker: 2,
        price: -1,
        qty: 1,
        taker_side: Side::Buy,
        match_id: None,
        exec_ts: None,
    };
    assert_eq!(
        Trade::decode(&encode(&SchemaMessage::Trade(t), Version::V2)),
        Ok(t)
    );

    // Engine orders survive the schema both ways.
    let mut rng = Rng::new(5);
    for i in 0..1_000u64 {
        let o = match i % 3 {
            0 => types::NewOrder::limit(
                i,
                rng.side(),
                rng.range_i64(-1000, 1000),
                rng.range(1, 1000),
            ),
            1 => types::NewOrder::limit(i, rng.side(), 100, 5)
                .post_only()
                .with_tif(TimeInForce::Ioc),
            _ => types::NewOrder::market(i, rng.side(), rng.range(1, 50)),
        }
        .with_account(rng.next_u64() as u32);
        let wire = NewOrder::from_order(&o);
        assert_eq!(wire.to_order(), o);
        let bytes = encode(&SchemaMessage::NewOrder(wire), Version::CURRENT);
        assert_eq!(NewOrder::decode(&bytes).map(|m| m.to_order()), Ok(o));
    }
}
