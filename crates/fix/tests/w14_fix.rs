//! Week 14: FIX 4.4 — field iterator, BodyLength/CheckSum, typed messages,
//! float-free price mapping, stream framing, and the session layer.
//!
//! Golden messages were generated independently (a 10-line Python script
//! summing bytes), so the encoder is checked against an outside source.

use fix::{
    checksum, format_utc_timestamp, message_len, parse_uint, Decimal, DisconnectReason, Encoder,
    ExecType, ExecutionReport, Field, FieldIter, FixError, Heartbeat, Logon, Logout,
    NewOrderSingle, OrdStatus, OrdType, OrderCancelRequest, PriceScale, RawMessage, Received,
    ResendRequest, Role, SequenceReset, Session, SessionConfig, SessionState, StdHeader,
    TestRequest, Tif,
};
use types::{
    AccountId, Command, InstrumentId, OrderId, OrderKind, Price, Qty, Rng, Side, TimeInForce,
};

/// A widely circulated NewOrderSingle from real QuickFIX logs (market
/// order, Currency and HandlInst fields we do not use). 9=148, 10=092.
const REAL_NOS: &[u8] = b"8=FIX.4.4\x019=148\x0135=D\x0134=1080\x0149=TESTBUY1\x0152=20180920-18:14:19.508\x0156=TESTSELL1\x0111=636730640278898634\x0115=USD\x0121=2\x0138=7000\x0140=1\x0154=1\x0155=MSFT\x0160=20180920-18:14:19.492\x0110=092\x01";

const NOS: &[u8] = b"8=FIX.4.4\x019=139\x0135=D\x0149=CLIENT1\x0156=EXCH\x0134=2\x0152=20261004-12:30:00.000\x0111=ORD-0001\x011=ACC42\x0155=AAPL\x0154=1\x0160=20261004-12:30:00.000\x0138=100\x0140=2\x0144=187.25\x0159=0\x0110=069\x01";
const ER: &[u8] = b"8=FIX.4.4\x019=161\x0135=8\x0149=EXCH\x0156=CLIENT1\x0134=7\x0152=20261004-12:30:00.000\x0137=1001\x0111=ORD-0001\x0117=5001\x01150=F\x0139=1\x0155=AAPL\x0154=1\x0138=100\x0144=187.25\x0132=40\x0131=187.25\x01151=60\x0114=40\x016=187.25\x0110=201\x01";
const LOGON: &[u8] = b"8=FIX.4.4\x019=72\x0135=A\x0149=CLIENT1\x0156=EXCH\x0134=1\x0152=20261004-12:30:00.000\x0198=0\x01108=30\x01141=Y\x0110=041\x01";
const HEARTBEAT: &[u8] = b"8=FIX.4.4\x019=65\x0135=0\x0149=EXCH\x0156=CLIENT1\x0134=12\x0152=20261004-12:30:00.000\x01112=TEST1\x0110=073\x01";
const RESEND: &[u8] = b"8=FIX.4.4\x019=64\x0135=2\x0149=EXCH\x0156=CLIENT1\x0134=13\x0152=20261004-12:30:00.000\x017=5\x0116=0\x0110=135\x01";
const OCR: &[u8] = b"8=FIX.4.4\x019=124\x0135=F\x0149=CLIENT1\x0156=EXCH\x0134=3\x0152=20261004-12:30:00.000\x0141=ORD-0001\x0111=ORD-0002\x0137=1001\x0155=AAPL\x0154=1\x0160=20261004-12:30:00.000\x0110=194\x01";

const TIME: &[u8] = b"20261004-12:30:00.000";
/// 2026-10-04 12:30:00 UTC in ns since the epoch.
const T0: u64 = 1_791_117_000_000_000_000;
const SEC: u64 = 1_000_000_000;

fn header<'a>(sender: &'a [u8], target: &'a [u8], seq: u64) -> StdHeader<'a> {
    StdHeader {
        sender,
        target,
        seq_num: seq,
        sending_time: TIME,
        poss_dup: false,
    }
}

fn nos() -> NewOrderSingle<'static> {
    NewOrderSingle {
        cl_ord_id: b"ORD-0001",
        account: Some(b"ACC42"),
        symbol: b"AAPL",
        side: Side::Buy,
        transact_time: TIME,
        order_qty: Decimal::parse(b"100").unwrap(),
        ord_type: OrdType::Limit,
        price: Decimal::parse(b"187.25"),
        time_in_force: Tif::Day,
        post_only: false,
    }
}

/// Builds a message from `35=..|49=..|...` text with a correct BodyLength
/// and CheckSum, for structural edge cases the encoder would never produce.
fn build(body: &str) -> Vec<u8> {
    let body = body.replace('|', "\x01");
    let mut m = format!("8=FIX.4.4\x019={}\x01{}", body.len(), body).into_bytes();
    let sum = checksum(&m);
    m.extend_from_slice(format!("10={sum:03}\x01").as_bytes());
    m
}

/// Splits a buffer of concatenated messages.
fn split(mut buf: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    while !buf.is_empty() {
        let n = message_len(buf).unwrap().expect("complete message");
        out.push(&buf[..n]);
        buf = &buf[n..];
    }
    out
}

fn field(raw: &RawMessage<'_>, tag: u32) -> Option<Vec<u8>> {
    raw.fields()
        .map(Result::unwrap)
        .find(|f| f.tag == tag)
        .map(|f| f.value.to_vec())
}

#[test]
fn w14_field_iterator_and_checksum() {
    let msg = b"8=FIX.4.4\x019=5\x0135=0\x01";
    let fields: Vec<Field> = FieldIter::new(msg).map(Result::unwrap).collect();
    assert_eq!(
        fields,
        [
            Field {
                tag: 8,
                value: b"FIX.4.4"
            },
            Field {
                tag: 9,
                value: b"5"
            },
            Field {
                tag: 35,
                value: b"0"
            },
        ]
    );
    // Zero copy: values are slices of the input.
    let range = msg.as_ptr_range();
    assert!(fields.iter().all(|f| range.contains(&f.value.as_ptr())));

    for (bad, offset) in [
        (&b"=5\x01"[..], 0),      // empty tag
        (b"35=\x01", 3),          // empty value
        (b"35=D", 3),             // no SOH
        (b"3a=D\x01", 1),         // non-digit tag
        (b"035=D\x01", 0),        // leading zero
        (b"35\x01", 2),           // no '='
        (b"1234567890=x\x01", 9), // tag too long
    ] {
        let mut it = FieldIter::new(bad);
        assert_eq!(
            it.next(),
            Some(Err(FixError::Malformed { offset })),
            "{bad:?}"
        );
        assert_eq!(it.next(), None, "iteration stops after an error");
    }
    let mut it = FieldIter::new(b"35=D\x0149=X\x01oops");
    assert_eq!(it.next().unwrap().unwrap().tag, 35);
    assert_eq!(it.offset(), 5);
    assert_eq!(it.next().unwrap().unwrap().tag, 49);
    assert_eq!(it.next(), Some(Err(FixError::Malformed { offset: 10 })));

    // CheckSum: byte sum mod 256 (golden values from the Python generator).
    let body_end = |m: &[u8]| m.len() - 7;
    assert_eq!(checksum(&REAL_NOS[..body_end(REAL_NOS)]), 92);
    assert_eq!(checksum(&NOS[..body_end(NOS)]), 69);
    assert_eq!(checksum(&ER[..body_end(ER)]), 201);
    assert_eq!(checksum(b""), 0);
    assert_eq!(checksum(&[1u8; 256]), 0);
    assert_eq!(checksum(&[0xffu8; 3]), 0xfd);

    assert_eq!(parse_uint(b"1080"), Some(1080));
    assert_eq!(parse_uint(b"18446744073709551615"), None, "20 digits");
    assert_eq!(
        parse_uint(b"9999999999999999999"),
        Some(9_999_999_999_999_999_999)
    );
    for bad in [&b""[..], b"-1", b"1.0", b" 1", b"+1"] {
        assert_eq!(parse_uint(bad), None);
    }
}

#[test]
fn w14_validates_real_world_samples() {
    let raw = RawMessage::parse(REAL_NOS).unwrap();
    assert_eq!(raw.msg_type, b"D");
    assert_eq!(raw.seq_num, 1080);
    assert_eq!(
        (raw.sender, raw.target),
        (&b"TESTBUY1"[..], &b"TESTSELL1"[..])
    );
    assert_eq!(raw.sending_time, b"20180920-18:14:19.508");
    assert!(!raw.poss_dup);
    assert!(
        raw.body.starts_with(b"11=636730640278898634\x01"),
        "body starts after the header"
    );
    assert_eq!(raw.bytes, REAL_NOS);

    for (msg, ty, seq) in [
        (NOS, "D", 2),
        (ER, "8", 7),
        (LOGON, "A", 1),
        (HEARTBEAT, "0", 12),
        (RESEND, "2", 13),
        (OCR, "F", 3),
    ] {
        let raw = RawMessage::parse(msg).unwrap_or_else(|e| panic!("{ty}: {e}"));
        assert_eq!((raw.msg_type, raw.seq_num), (ty.as_bytes(), seq));
        assert_eq!(message_len(msg), Ok(Some(msg.len())));
    }

    // Tampering is caught.
    let mut flipped = NOS.to_vec();
    let at = flipped.windows(4).position(|w| w == b"AAPL").unwrap();
    flipped[at] = b'B';
    assert_eq!(
        RawMessage::parse(&flipped),
        Err(FixError::BadChecksum {
            declared: 69,
            computed: 70
        })
    );
    let longer = String::from_utf8(NOS.to_vec())
        .unwrap()
        .replacen("9=139", "9=140", 1);
    assert_eq!(
        RawMessage::parse(longer.as_bytes()),
        Err(FixError::BadBodyLength)
    );
    let shorter = String::from_utf8(NOS.to_vec())
        .unwrap()
        .replacen("9=139", "9=138", 1);
    assert_eq!(
        RawMessage::parse(shorter.as_bytes()),
        Err(FixError::BadBodyLength)
    );
    let fix42 = String::from_utf8(NOS.to_vec())
        .unwrap()
        .replacen("FIX.4.4", "FIX.4.2", 1);
    assert_eq!(
        RawMessage::parse(fix42.as_bytes()),
        Err(FixError::BadBeginString)
    );
    assert_eq!(
        RawMessage::parse(&NOS[..NOS.len() - 1]),
        Err(FixError::BadBodyLength)
    );
    let mut trailing = NOS.to_vec();
    trailing.push(b'x');
    assert_eq!(RawMessage::parse(&trailing), Err(FixError::BadBodyLength));
    let mut bad_sum = NOS.to_vec();
    let n = bad_sum.len();
    bad_sum[n - 4..n - 1].copy_from_slice(b"9x9");
    assert_eq!(RawMessage::parse(&bad_sum), Err(FixError::InvalidValue(10)));

    // Header rules.
    assert_eq!(
        RawMessage::parse(&build("49=A|35=0|56=B|34=1|52=x|")),
        Err(FixError::MissingField(35))
    );
    assert_eq!(
        RawMessage::parse(&build("35=0|49=A|56=B|52=x|")),
        Err(FixError::MissingField(34))
    );
    assert_eq!(
        RawMessage::parse(&build("35=0|49=A|56=B|34=1|")),
        Err(FixError::MissingField(52))
    );
    assert_eq!(
        RawMessage::parse(&build("35=0|49=A|56=B|34=x|52=x|")),
        Err(FixError::InvalidValue(34))
    );
    // Body syntax errors surface when the body is iterated (typed decoders
    // propagate them), not in the header pass.
    let lazy = build("35=0|49=A|56=B|34=1|52=x|58=a|=b|");
    let raw = RawMessage::parse(&lazy).unwrap();
    let body: Vec<_> = raw.fields().collect();
    assert_eq!(
        body[0],
        Ok(Field {
            tag: 58,
            value: b"a"
        })
    );
    assert!(matches!(body[1], Err(FixError::Malformed { .. })));
    // Right after the header it is caught by the header pass, with the
    // offset counted from the start of the message (15 + 25).
    assert_eq!(
        RawMessage::parse(&build("35=5|49=A|56=B|34=1|52=x|=b|")),
        Err(FixError::Malformed { offset: 40 })
    );
    let dup = build("35=0|49=A|56=B|34=9|43=Y|122=x|52=x|112=T|");
    let raw = RawMessage::parse(&dup).unwrap();
    assert!(raw.poss_dup);
    assert_eq!(raw.body, b"112=T\x01");
}

#[test]
fn w14_encodes_and_decodes_golden_messages() {
    let mut enc = Encoder::new();
    let mut out = Vec::new();
    let check = |golden: &[u8], range: std::ops::Range<usize>, out: &Vec<u8>| {
        assert_eq!(
            String::from_utf8_lossy(&out[range]).replace('\x01', "|"),
            String::from_utf8_lossy(golden).replace('\x01', "|")
        );
    };

    let r = enc.encode(&header(b"CLIENT1", b"EXCH", 2), &nos(), &mut out);
    check(NOS, r, &out);
    assert_eq!(
        NewOrderSingle::decode(&RawMessage::parse(NOS).unwrap()),
        Ok(nos())
    );

    let er = ExecutionReport {
        order_id: 1001,
        cl_ord_id: b"ORD-0001",
        orig_cl_ord_id: None,
        exec_id: 5001,
        exec_type: ExecType::Trade,
        ord_status: OrdStatus::PartiallyFilled,
        ord_rej_reason: None,
        symbol: b"AAPL",
        side: Side::Buy,
        order_qty: Some(100),
        price: Decimal::parse(b"187.25"),
        last_qty: Some(40),
        last_px: Some(Decimal::from_ticks(18_725, PriceScale::CENTS)),
        leaves_qty: 60,
        cum_qty: 40,
        avg_px: Decimal::from_ticks(18_725, PriceScale::CENTS),
        text: None,
    };
    out.clear();
    let r = enc.encode(&header(b"EXCH", b"CLIENT1", 7), &er, &mut out);
    check(ER, r, &out);
    assert_eq!(
        ExecutionReport::decode(&RawMessage::parse(ER).unwrap()),
        Ok(er)
    );

    let logon = Logon {
        heart_bt_int: 30,
        reset_seq_num: true,
        username: None,
        password: None,
    };
    let hb = Heartbeat {
        test_req_id: Some(b"TEST1"),
    };
    let rr = ResendRequest {
        begin_seq_no: 5,
        end_seq_no: 0,
    };
    let ocr = OrderCancelRequest {
        orig_cl_ord_id: b"ORD-0001",
        cl_ord_id: b"ORD-0002",
        order_id: Some(1001),
        symbol: b"AAPL",
        side: Side::Buy,
        transact_time: TIME,
    };
    // Several messages appended to one buffer, as a session does.
    out.clear();
    let ranges = [
        enc.encode(&header(b"CLIENT1", b"EXCH", 1), &logon, &mut out),
        enc.encode(&header(b"EXCH", b"CLIENT1", 12), &hb, &mut out),
        enc.encode(&header(b"EXCH", b"CLIENT1", 13), &rr, &mut out),
        enc.encode(&header(b"CLIENT1", b"EXCH", 3), &ocr, &mut out),
    ];
    for (golden, r) in [LOGON, HEARTBEAT, RESEND, OCR].into_iter().zip(ranges) {
        check(golden, r, &out);
    }
    assert_eq!(Logon::decode(&RawMessage::parse(LOGON).unwrap()), Ok(logon));
    assert_eq!(
        Heartbeat::decode(&RawMessage::parse(HEARTBEAT).unwrap()),
        Ok(hb)
    );
    assert_eq!(
        ResendRequest::decode(&RawMessage::parse(RESEND).unwrap()),
        Ok(rr)
    );
    let raw_ocr = RawMessage::parse(OCR).unwrap();
    assert_eq!(OrderCancelRequest::decode(&raw_ocr), Ok(ocr));
    assert_eq!(
        ocr.to_command(),
        Some(Command::Cancel { id: OrderId(1001) })
    );

    // Every typed message round-trips through the encoder.
    out.clear();
    let tr = TestRequest {
        test_req_id: b"abc",
    };
    let sr = SequenceReset {
        new_seq_no: 42,
        gap_fill: true,
    };
    let lo = Logout { text: Some(b"bye") };
    let r1 = enc.encode(&header(b"A", b"B", 1), &tr, &mut out);
    let r2 = enc.encode(&header(b"A", b"B", 2), &sr, &mut out);
    let r3 = enc.encode(&header(b"A", b"B", 3), &lo, &mut out);
    assert_eq!(
        TestRequest::decode(&RawMessage::parse(&out[r1]).unwrap()),
        Ok(tr)
    );
    assert_eq!(
        SequenceReset::decode(&RawMessage::parse(&out[r2.clone()]).unwrap()),
        Ok(sr)
    );
    assert_eq!(
        Logout::decode(&RawMessage::parse(&out[r3]).unwrap()),
        Ok(lo)
    );
    // A typed decoder refuses another message type.
    assert_eq!(
        Logout::decode(&RawMessage::parse(&out[r2]).unwrap()),
        Err(FixError::UnexpectedMsgType)
    );

    // UTCTimestamp formatting.
    let mut ts = [0u8; 21];
    for (nanos, want) in [
        (T0, "20261004-12:30:00.000"),
        (951_868_799_999_000_000, "20000229-23:59:59.999"),
        (0, "19700101-00:00:00.000"),
        (4_107_542_400_001_000_000, "21000301-00:00:00.001"),
    ] {
        format_utc_timestamp(nanos, &mut ts);
        assert_eq!(std::str::from_utf8(&ts).unwrap(), want);
    }
}

#[test]
fn w14_new_order_single_maps_to_types() {
    let ids = (OrderId(77), AccountId(7), InstrumentId(3));
    let map = |msg: &[u8], scale| {
        let raw = RawMessage::parse(msg)?;
        NewOrderSingle::decode(&raw)?.to_new_order(ids.0, ids.1, ids.2, scale)
    };

    // Real-world sample: market order, Day (59 absent) → engine GTC.
    let o = map(REAL_NOS, PriceScale::CENTS).unwrap();
    assert_eq!(o.kind, OrderKind::Market);
    assert_eq!(
        (o.qty, o.side, o.tif),
        (Qty(7000), Side::Buy, TimeInForce::Gtc)
    );
    assert_eq!((o.id, o.account, o.instrument), ids);

    // 187.25 at different scales, exactly.
    assert_eq!(
        map(NOS, PriceScale::CENTS).unwrap().limit_price(),
        Some(Price(18_725))
    );
    assert_eq!(
        map(NOS, PriceScale::new(4)).unwrap().limit_price(),
        Some(Price(1_872_500))
    );
    assert_eq!(
        map(NOS, PriceScale::new(1)),
        Err(FixError::InvalidValue(44)),
        "not on a 0.1 tick"
    );
    assert_eq!(map(NOS, PriceScale::UNITS), Err(FixError::InvalidValue(44)));

    let base = "35=D|49=C|56=E|34=2|52=t|11=X|55=AAPL|60=t|";
    let one = |extra: &str| map(&build(&format!("{base}{extra}")), PriceScale::CENTS);
    let limit = one("54=2|38=5|40=2|44=0.10|59=3|").unwrap();
    assert_eq!(
        (limit.side, limit.tif, limit.limit_price()),
        (Side::Sell, TimeInForce::Ioc, Some(Price(10)))
    );
    assert_eq!(
        one("54=1|38=5|40=2|44=1|59=4|").unwrap().tif,
        TimeInForce::Fok
    );
    assert_eq!(
        one("54=1|38=5|40=2|44=1|59=1|").unwrap().tif,
        TimeInForce::Gtc
    );
    let post = one("54=1|38=5|40=2|44=99.5|18=6|").unwrap();
    assert_eq!(
        post.kind,
        OrderKind::Limit {
            price: Price(9_950),
            post_only: true
        }
    );
    assert!(matches!(
        one("54=1|38=5|40=2|44=1|18=E 6|").unwrap().kind,
        OrderKind::Limit {
            post_only: true,
            ..
        }
    ));
    assert!(matches!(
        one("54=1|38=5|40=2|44=1|18=E|").unwrap().kind,
        OrderKind::Limit {
            post_only: false,
            ..
        }
    ));
    assert_eq!(
        one("54=1|38=5.000|40=2|44=-3.5|").unwrap().limit_price(),
        Some(Price(-350))
    );
    assert_eq!(
        one("54=1|38=5.5|40=2|44=1|"),
        Err(FixError::InvalidValue(38)),
        "fractional lots"
    );
    assert_eq!(
        one("54=1|38=5|40=1|18=6|"),
        Err(FixError::InvalidValue(18)),
        "post-only market"
    );
    assert_eq!(one("54=1|38=5|40=2|"), Err(FixError::MissingField(44)));
    assert_eq!(one("54=5|38=5|40=1|"), Err(FixError::InvalidValue(54)));
    assert_eq!(one("54=1|38=5|40=3|"), Err(FixError::InvalidValue(40)));
    assert_eq!(one("54=1|38=5|40=1|59=6|"), Err(FixError::InvalidValue(59)));
    assert_eq!(
        one("54=1|38=5|40=2|44=1.2.3|"),
        Err(FixError::InvalidValue(44))
    );
    assert_eq!(one("54=1|40=1|"), Err(FixError::MissingField(38)));
    let no_clordid = build("35=D|49=C|56=E|34=2|52=t|55=AAPL|60=t|54=1|38=5|40=1|");
    assert_eq!(
        map(&no_clordid, PriceScale::CENTS),
        Err(FixError::MissingField(11))
    );
    assert_eq!(map(ER, PriceScale::CENTS), Err(FixError::UnexpectedMsgType));
}

#[test]
fn w14_decimal_prices_without_floats() {
    for (text, mantissa, scale) in [
        ("187.25", 18_725, 2),
        ("-0.5", -5, 1),
        (".5", 5, 1),
        ("5.", 5, 0),
        ("007", 7, 0),
        ("0", 0, 0),
        ("187.2500", 1_872_500, 4),
        ("9223372036854775807", i64::MAX, 0),
        ("0.000000000000000001", 1, 18),
    ] {
        assert_eq!(
            Decimal::parse(text.as_bytes()),
            Some(Decimal { mantissa, scale }),
            "{text}"
        );
    }
    for bad in [
        "",
        "-",
        ".",
        "1.2.3",
        "1e5",
        "+1",
        "1,5",
        "9223372036854775808",
        "0.0000000000000000001",
        " 1",
    ] {
        assert_eq!(Decimal::parse(bad.as_bytes()), None, "{bad:?}");
    }
    let d = |s: &str| Decimal::parse(s.as_bytes()).unwrap();
    assert_eq!(
        d("187.2500").to_ticks(PriceScale::CENTS),
        Some(18_725),
        "trailing zeros are fine"
    );
    assert_eq!(d("187.255").to_ticks(PriceScale::CENTS), None, "sub-tick");
    assert_eq!(
        d("92233720368547758.07").to_ticks(PriceScale::new(3)),
        None,
        "overflow"
    );
    assert_eq!(
        d("-1").to_ticks(PriceScale::new(18)),
        Some(-1_000_000_000_000_000_000)
    );

    let text = |d: Decimal| {
        let mut v = Vec::new();
        d.write(&mut v);
        String::from_utf8(v).unwrap()
    };
    assert_eq!(
        text(Decimal {
            mantissa: 5,
            scale: 2
        }),
        "0.05"
    );
    assert_eq!(
        text(Decimal {
            mantissa: -18_725,
            scale: 2
        }),
        "-187.25"
    );
    assert_eq!(
        text(Decimal {
            mantissa: 0,
            scale: 3
        }),
        "0.000"
    );
    assert_eq!(
        text(Decimal {
            mantissa: i64::MIN,
            scale: 0
        }),
        "-9223372036854775808"
    );

    // Property: ticks → text → ticks is the identity at every scale.
    let mut rng = Rng::new(14);
    for _ in 0..50_000 {
        let scale = PriceScale::new(rng.range(0, 9) as u8);
        let ticks = match rng.range(0, 3) {
            0 => rng.range_i64(-1_000, 1_000),
            1 => rng.range_i64(-10_000_000_000, 10_000_000_000),
            _ => rng.next_u64() as i64,
        };
        let s = text(Decimal::from_ticks(ticks, scale));
        let back = Decimal::parse(s.as_bytes()).unwrap_or_else(|| panic!("{s}"));
        assert_eq!(back.to_ticks(scale), Some(ticks), "{s}");
        // And at a coarser scale it is either exact or refused, never rounded.
        if scale.decimals() > 0 {
            let coarse = PriceScale::new(scale.decimals() - 1);
            match back.to_ticks(coarse) {
                Some(c) => assert_eq!(c * 10, ticks),
                None => assert_ne!(ticks % 10, 0),
            }
        }
    }
}

#[test]
fn w14_message_len_frames_partial_reads() {
    // A stream of encoded messages, delivered in random chunks.
    let mut rng = Rng::new(1414);
    let mut enc = Encoder::new();
    let mut stream = Vec::new();
    let mut sent = Vec::new();
    for seq in 1..=500u64 {
        let start = stream.len();
        let qty = Decimal::from_ticks(rng.range(1, 1_000_000) as i64, PriceScale::UNITS);
        let price = Decimal::from_ticks(
            rng.range_i64(1, 10_000_000),
            PriceScale::new(rng.range(0, 6) as u8),
        );
        let cl = format!("C{}", rng.next_u64());
        let order = NewOrderSingle {
            cl_ord_id: cl.as_bytes(),
            order_qty: qty,
            price: Some(price),
            ..nos()
        };
        if rng.chance(0.5) {
            enc.encode(&header(b"CLIENT1", b"EXCH", seq), &order, &mut stream);
        } else {
            enc.encode(
                &header(b"CLIENT1", b"EXCH", seq),
                &Heartbeat { test_req_id: None },
                &mut stream,
            );
        }
        sent.push(stream[start..].to_vec());
    }
    let mut got: Vec<Vec<u8>> = Vec::new();
    let mut buf: Vec<u8> = Vec::new();
    let mut pos = 0;
    while pos < stream.len() {
        let n = (rng.range(1, 150) as usize).min(stream.len() - pos);
        buf.extend_from_slice(&stream[pos..pos + n]);
        pos += n;
        let mut start = 0;
        while let Some(len) = message_len(&buf[start..]).unwrap() {
            let msg = &buf[start..start + len];
            RawMessage::parse(msg).unwrap();
            got.push(msg.to_vec());
            start += len;
        }
        buf.drain(..start);
    }
    assert!(buf.is_empty());
    assert_eq!(got, sent);

    // Every proper prefix is "need more"; nonsense is an error.
    for n in 0..NOS.len() {
        assert_eq!(message_len(&NOS[..n]), Ok(None), "prefix {n}");
    }
    assert_eq!(
        message_len(b"8=FIX.4.2\x019=5\x01"),
        Err(FixError::BadBeginString)
    );
    assert_eq!(
        message_len(b"GET / HTTP/1.1\r\n"),
        Err(FixError::BadBeginString)
    );
    assert_eq!(
        message_len(b"8=FIX.4.4\x019=12a"),
        Err(FixError::BadBodyLength)
    );
    assert_eq!(
        message_len(b"8=FIX.4.4\x019=\x01"),
        Err(FixError::BadBodyLength)
    );
    assert_eq!(
        message_len(b"8=FIX.4.4\x019=1234567\x01"),
        Err(FixError::BadBodyLength)
    );
    assert_eq!(
        message_len(b"8=FIX.4.4\x019=999999\x01"),
        Err(FixError::TooLarge)
    );
}

fn pair(reset: bool) -> (Session, Session) {
    let client = SessionConfig {
        reset_seq_num: reset,
        ..SessionConfig::new(Role::Initiator, "CLIENT1", "EXCH")
    };
    let exch = SessionConfig::new(Role::Acceptor, "EXCH", "CLIENT1");
    (Session::new(client), Session::new(exch))
}

/// Delivers every message in `out` to `to`, returning what happened.
fn deliver(out: &mut Vec<u8>, to: &mut Session, now: u64, reply: &mut Vec<u8>) -> Vec<String> {
    let msgs: Vec<Vec<u8>> = split(out).into_iter().map(<[u8]>::to_vec).collect();
    out.clear();
    msgs.iter()
        .map(|m| match to.on_message(m, now, reply) {
            Received::App(raw) => format!("App({})", String::from_utf8_lossy(raw.msg_type)),
            other => format!("{other:?}"),
        })
        .collect()
}

fn types_of(out: &[u8]) -> Vec<String> {
    split(out)
        .iter()
        .map(|m| String::from_utf8_lossy(RawMessage::parse(m).unwrap().msg_type).into_owned())
        .collect()
}

/// Logs a pair on at `T0`.
fn logged_on(reset: bool) -> (Session, Session, Vec<u8>, Vec<u8>) {
    let (mut client, mut exch) = pair(reset);
    let (mut c_out, mut e_out) = (Vec::new(), Vec::new());
    exch.start(T0, &mut e_out);
    assert!(e_out.is_empty());
    assert_eq!(exch.state(), SessionState::AwaitingLogon);
    client.start(T0, &mut c_out);
    assert_eq!(client.state(), SessionState::LogonSent);
    assert_eq!(types_of(&c_out), ["A"]);
    assert_eq!(deliver(&mut c_out, &mut exch, T0, &mut e_out), ["Admin"]);
    assert_eq!(exch.state(), SessionState::Active);
    assert_eq!(types_of(&e_out), ["A"], "acceptor answers the Logon");
    assert_eq!(deliver(&mut e_out, &mut client, T0, &mut c_out), ["Admin"]);
    assert_eq!(client.state(), SessionState::Active);
    assert_eq!(
        (client.heartbeat_interval_s(), exch.heartbeat_interval_s()),
        (30, 30)
    );
    (client, exch, c_out, e_out)
}

#[test]
fn w14_session_logon_heartbeats_and_test_requests() {
    let (mut client, mut exch, mut c_out, mut e_out) = logged_on(false);
    assert_eq!(
        (client.next_outbound_seq(), exch.next_inbound_seq()),
        (2, 2)
    );

    // Application messages flow and are numbered.
    assert_eq!(client.send(&nos(), T0, &mut c_out), Some(2));
    let msgs = split(&c_out)
        .into_iter()
        .map(<[u8]>::to_vec)
        .collect::<Vec<_>>();
    match exch.on_message(&msgs[0], T0, &mut e_out) {
        Received::App(raw) => {
            assert_eq!(raw.seq_num, 2);
            assert_eq!(NewOrderSingle::decode(&raw), Ok(nos()));
        }
        other => panic!("{other:?}"),
    }
    c_out.clear();

    // Quiet for HeartBtInt: both sides send a Heartbeat, not before.
    assert_eq!(exch.poll(T0 + 29 * SEC, &mut e_out), None);
    assert!(e_out.is_empty());
    let t1 = T0 + 30 * SEC;
    assert_eq!(
        (client.poll(t1, &mut c_out), exch.poll(t1, &mut e_out)),
        (None, None)
    );
    assert_eq!(
        (types_of(&c_out), types_of(&e_out)),
        (vec!["0".to_string()], vec!["0".to_string()])
    );
    assert_eq!(deliver(&mut c_out, &mut exch, t1, &mut e_out), ["Admin"]);
    assert_eq!(deliver(&mut e_out, &mut client, t1, &mut c_out), ["Admin"]);

    // Silence for HeartBtInt + 20%: a TestRequest, answered by a Heartbeat
    // echoing its TestReqID.
    let t2 = t1 + 36 * SEC;
    assert_eq!(exch.poll(t1 + 30 * SEC, &mut e_out), None);
    assert_eq!(types_of(&e_out), ["0"], "its own Heartbeat is still due");
    assert_eq!(
        deliver(&mut e_out, &mut client, t1 + 30 * SEC, &mut c_out),
        ["Admin"]
    );
    assert!(c_out.is_empty(), "the client stays silent (not polled)");
    assert_eq!(exch.poll(t2 - SEC, &mut e_out), None);
    assert!(e_out.is_empty());
    assert_eq!(exch.poll(t2, &mut e_out), None);
    assert_eq!(types_of(&e_out), ["1"]);
    let id = field(&RawMessage::parse(&e_out).unwrap(), 112).unwrap();
    assert_eq!(deliver(&mut e_out, &mut client, t2, &mut c_out), ["Admin"]);
    let hb = c_out.clone();
    assert_eq!(field(&RawMessage::parse(&hb).unwrap(), 112), Some(id));
    assert_eq!(deliver(&mut c_out, &mut exch, t2, &mut e_out), ["Admin"]);

    // Unanswered TestRequest: logout and disconnect one interval later.
    let t3 = t2 + 36 * SEC;
    assert_eq!(exch.poll(t3, &mut e_out), None);
    assert_eq!(types_of(&e_out), ["1"]);
    e_out.clear();
    assert_eq!(
        exch.poll(t3 + 30 * SEC, &mut e_out),
        Some(DisconnectReason::HeartbeatTimeout)
    );
    assert_eq!(types_of(&e_out), ["5"]);
    assert_eq!(
        field(&RawMessage::parse(&e_out).unwrap(), 58),
        Some(b"Heartbeat timeout".to_vec())
    );
    assert_eq!(exch.state(), SessionState::Disconnected);
    assert_eq!(
        exch.send(&nos(), t3, &mut e_out),
        None,
        "nothing is sent after disconnect"
    );

    // Graceful logout from a fresh pair.
    let (mut client, mut exch, mut c_out, mut e_out) = logged_on(false);
    client.logout(Some(b"done"), T0, &mut c_out);
    assert_eq!(client.state(), SessionState::LogoutSent);
    assert_eq!(
        deliver(&mut c_out, &mut exch, T0, &mut e_out),
        ["Disconnect(PeerLogout)"]
    );
    assert_eq!(
        deliver(&mut e_out, &mut client, T0, &mut c_out),
        ["Disconnect(LogoutComplete)"]
    );

    // Handshake failures.
    let (mut client, mut exch) = pair(false);
    let (mut c_out, mut e_out) = (Vec::new(), Vec::new());
    exch.start(T0, &mut e_out);
    assert_eq!(
        exch.poll(T0 + 10 * SEC, &mut e_out),
        Some(DisconnectReason::LogonTimeout)
    );
    let (_, mut exch) = pair(false);
    exch.start(T0, &mut e_out);
    let mut enc = Encoder::new();
    enc.encode(&header(b"CLIENT1", b"EXCH", 1), &nos(), &mut c_out);
    assert_eq!(
        deliver(&mut c_out, &mut exch, T0, &mut e_out),
        ["Disconnect(NotLoggedOn)"]
    );
    assert!(e_out.is_empty(), "no Logout before a Logon");
    let (_, mut exch) = pair(false);
    exch.start(T0, &mut e_out);
    client.start(T0, &mut c_out);
    let wrong = String::from_utf8(c_out.clone())
        .unwrap()
        .replace("CLIENT1", "CLIENT9");
    let wrong = build(&wrong[wrong.find("35=").unwrap()..wrong.find("10=").unwrap()]);
    assert!(matches!(
        exch.on_message(&wrong, T0, &mut e_out),
        Received::Disconnect(DisconnectReason::CompIdMismatch)
    ));
}

#[test]
fn w14_session_sequence_gaps_and_too_low() {
    let (mut client, mut exch, mut c_out, mut e_out) = logged_on(true);
    // ResetSeqNumFlag was echoed and both sides count from 1.
    assert_eq!((exch.next_inbound_seq(), exch.next_outbound_seq()), (2, 2));

    // Lose seq 2: seq 3 opens a gap → one ResendRequest(2, 0).
    client.send(&nos(), T0, &mut c_out);
    c_out.clear();
    client.send(&nos(), T0, &mut c_out);
    assert_eq!(
        deliver(&mut c_out, &mut exch, T0, &mut e_out),
        ["Dropped(Gap { expected: 2, received: 3 })"]
    );
    assert_eq!(types_of(&e_out), ["2"]);
    let rr = RawMessage::parse(&e_out).unwrap();
    assert_eq!(
        ResendRequest::decode(&rr),
        Ok(ResendRequest {
            begin_seq_no: 2,
            end_seq_no: 0
        })
    );
    // Further messages during the gap do not repeat the request.
    let mut pending = std::mem::take(&mut e_out);
    client.send(&nos(), T0, &mut c_out);
    assert_eq!(
        deliver(&mut c_out, &mut exch, T0, &mut e_out),
        ["Dropped(Gap { expected: 2, received: 4 })"]
    );
    assert!(e_out.is_empty());
    assert_eq!(exch.next_inbound_seq(), 2);

    // The client answers with a gap fill: MsgSeqNum 2, PossDup, NewSeqNo 5.
    assert_eq!(
        deliver(&mut pending, &mut client, T0, &mut c_out),
        ["Admin"]
    );
    let fill = RawMessage::parse(&c_out).unwrap();
    assert_eq!(
        (fill.msg_type, fill.seq_num, fill.poss_dup),
        (&b"4"[..], 2, true)
    );
    assert_eq!(
        SequenceReset::decode(&fill),
        Ok(SequenceReset {
            new_seq_no: 5,
            gap_fill: true
        })
    );
    assert_eq!(
        client.next_outbound_seq(),
        5,
        "a gap fill does not consume a number"
    );
    assert_eq!(deliver(&mut c_out, &mut exch, T0, &mut e_out), ["Admin"]);
    assert_eq!(exch.next_inbound_seq(), 5);
    client.send(&nos(), T0, &mut c_out);
    assert_eq!(deliver(&mut c_out, &mut exch, T0, &mut e_out), ["App(D)"]);

    // Garbled messages are dropped without consuming a number.
    let mut enc = Encoder::new();
    enc.encode(&header(b"CLIENT1", b"EXCH", 6), &nos(), &mut c_out);
    let n = c_out.len();
    c_out[n - 3] = b'9';
    let got = deliver(&mut c_out, &mut exch, T0, &mut e_out);
    assert!(got[0].starts_with("Dropped(Garbled(BadChecksum"), "{got:?}");
    assert_eq!(exch.next_inbound_seq(), 6);

    // A possible duplicate of an old number is ignored.
    let dup = StdHeader {
        poss_dup: true,
        ..header(b"CLIENT1", b"EXCH", 3)
    };
    enc.encode(&dup, &nos(), &mut c_out);
    assert_eq!(
        deliver(&mut c_out, &mut exch, T0, &mut e_out),
        ["Dropped(Duplicate { seq: 3 })"]
    );

    // Reset mode forces the expected number, whatever MsgSeqNum says.
    enc.encode(
        &header(b"CLIENT1", b"EXCH", 1),
        &SequenceReset {
            new_seq_no: 20,
            gap_fill: false,
        },
        &mut c_out,
    );
    assert_eq!(deliver(&mut c_out, &mut exch, T0, &mut e_out), ["Admin"]);
    assert_eq!(exch.next_inbound_seq(), 20);

    // Too low without PossDup: Logout with the standard text, then disconnect.
    assert!(e_out.is_empty());
    enc.encode(&header(b"CLIENT1", b"EXCH", 7), &nos(), &mut c_out);
    assert_eq!(
        deliver(&mut c_out, &mut exch, T0, &mut e_out),
        ["Disconnect(SeqTooLow { expected: 20, received: 7 })"]
    );
    assert_eq!(types_of(&e_out), ["5"]);
    assert_eq!(
        field(&RawMessage::parse(&e_out).unwrap(), 58),
        Some(b"MsgSeqNum too low, expecting 20 but received 7".to_vec())
    );
    assert_eq!(exch.state(), SessionState::Disconnected);
    assert_eq!(
        exch.disconnect_reason(),
        Some(DisconnectReason::SeqTooLow {
            expected: 20,
            received: 7
        })
    );

    // A Logon ahead of the expected number is accepted, then the gap requested.
    let (mut client, mut exch) = pair(false);
    let (mut c_out, mut e_out) = (Vec::new(), Vec::new());
    exch.start(T0, &mut e_out);
    let logon = Logon {
        heart_bt_int: 10,
        reset_seq_num: false,
        username: Some(b"u"),
        password: Some(b"p"),
    };
    enc.encode(&header(b"CLIENT1", b"EXCH", 4), &logon, &mut c_out);
    assert_eq!(deliver(&mut c_out, &mut exch, T0, &mut e_out), ["Admin"]);
    assert_eq!(
        types_of(&e_out),
        ["A", "2"],
        "Logon answered, then ResendRequest(1, 0)"
    );
    assert_eq!(exch.state(), SessionState::Active);
    assert_eq!(exch.heartbeat_interval_s(), 10);
    client.start(T0, &mut c_out);
    assert_eq!(client.next_outbound_seq(), 2);
}
