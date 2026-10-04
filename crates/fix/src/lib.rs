//! FIX 4.4 parser/encoder and session layer.
//!
//! Curriculum: Week 14 (see <https://tepy.dev/hft>).
//!
//! FIX is the protocol every broker and venue still speaks: ASCII
//! `tag=value` fields separated by SOH (`0x01`), framed by a BodyLength and
//! protected by a CheckSum. This crate parses it without allocating
//! (fields are slices of the input), generates it with exact BodyLength and
//! CheckSum, maps orders to the exchange's [`types`] without floats, and
//! runs the session protocol (logon, sequence numbers, heartbeats, gap
//! recovery) as a sans-IO state machine.
//!
//! | layer | items |
//! |-------|-------|
//! | framing | [`message_len`] (stream → messages), [`checksum`] |
//! | fields | [`FieldIter`], [`Field`], [`parse_uint`] |
//! | validation + header | [`RawMessage::parse`] |
//! | typed messages | [`NewOrderSingle`], [`OrderCancelRequest`], [`ExecutionReport`], [`Logon`], [`Heartbeat`], [`TestRequest`], [`ResendRequest`], [`SequenceReset`], [`Logout`] |
//! | prices | [`Decimal`], [`PriceScale`] |
//! | generation | [`Encoder`], [`FieldWriter`], [`FixBody`] |
//! | session | [`Session`], [`SessionConfig`], [`Received`] |
//!
//! ```
//! use fix::{Encoder, NewOrderSingle, RawMessage, StdHeader, Decimal, OrdType, Tif, PriceScale};
//! use types::{AccountId, InstrumentId, OrderId, Side};
//!
//! let nos = NewOrderSingle {
//!     cl_ord_id: b"ORD-1", account: None, symbol: b"AAPL", side: Side::Buy,
//!     transact_time: b"20261004-12:30:00.000", order_qty: Decimal::parse(b"100").unwrap(),
//!     ord_type: OrdType::Limit, price: Decimal::parse(b"187.25"), time_in_force: Tif::Day,
//!     post_only: false,
//! };
//! let header = StdHeader { sender: b"CLIENT1", target: b"EXCH", seq_num: 2,
//!                          sending_time: b"20261004-12:30:00.000", poss_dup: false };
//! let mut out = Vec::new();
//! Encoder::new().encode(&header, &nos, &mut out);
//!
//! let raw = RawMessage::parse(&out).unwrap();          // BodyLength + CheckSum verified
//! let order = NewOrderSingle::decode(&raw).unwrap()
//!     .to_new_order(OrderId(1), AccountId(7), InstrumentId(3), PriceScale::CENTS)
//!     .unwrap();
//! assert_eq!(order.limit_price(), Some(types::Price(18_725)));
//! ```

#![warn(missing_docs)]

pub mod decimal;
pub mod encode;
pub mod messages;
pub mod parse;
pub mod session;

pub use decimal::{Decimal, PriceScale};
pub use encode::{format_uint, format_utc_timestamp, write_uint, Encoder, FieldWriter, StdHeader};
pub use messages::{
    msg_type, ord_rej_reason, side_code, side_from_code, ExecType, ExecutionReport, FixBody,
    Heartbeat, Logon, Logout, NewOrderSingle, OrdStatus, OrdType, OrderCancelRequest,
    ResendRequest, SequenceReset, TestRequest, Tif,
};
pub use parse::{
    checksum, is_header_tag, message_len, parse_uint, Field, FieldIter, FixError, RawMessage,
    BEGIN_STRING, MAX_BODY_LEN, SOH,
};
pub use session::{
    DisconnectReason, Dropped, Received, Role, Session, SessionConfig, SessionState,
};
