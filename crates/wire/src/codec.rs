//! Week 11: the fixed-layout protocol — message types, enumerated codes,
//! little-endian field helpers, and the [`encode_into`] / [`decode`] pair.
//!
//! Every layout is documented in a table in the [crate docs](crate). A
//! message's length is decided by its one-byte type tag ([`MsgType::wire_len`]),
//! so a decoder knows how many bytes to expect after reading one byte.

#[allow(unused_imports)] // used by the implementation the learner writes
use crate::view::{CancelView, LogonView, NewOrderView, ReduceView};
use std::fmt;
#[allow(unused_imports)]
use types::{
    AccountId, Command, Event, InstrumentId, OrderId, OrderKind, Price, Qty, RejectReason,
    RiskReason, Side, TimeInForce,
};

/// Length of the longest message (`NewOrder`, `Trade`), in bytes: a buffer
/// this big holds any message.
pub const MAX_MSG_LEN: usize = 40;

/// Protocol version carried in `Logon` / `LogonAck` (byte 1).
pub const PROTOCOL_VERSION: u8 = 1;

/// Wire value of [`Side::Buy`].
pub const SIDE_BUY: u8 = b'B';
/// Wire value of [`Side::Sell`].
pub const SIDE_SELL: u8 = b'S';
/// `NewOrder` order type: limit.
pub const ORD_LIMIT: u8 = b'L';
/// `NewOrder` order type: market.
pub const ORD_MARKET: u8 = b'M';
/// Wire value of [`TimeInForce::Gtc`].
pub const TIF_GTC: u8 = b'G';
/// Wire value of [`TimeInForce::Ioc`].
pub const TIF_IOC: u8 = b'I';
/// Wire value of [`TimeInForce::Fok`].
pub const TIF_FOK: u8 = b'F';
/// `NewOrder` flags bit 0: post-only (limit orders only). Other bits are
/// reserved and must be zero.
pub const FLAG_POST_ONLY: u8 = 0x01;

/// The one-byte type tag that starts every message. Tags are unique across
/// all three families (order entry in, order entry out, market data), so
/// one decoder handles any stream. Upper case = client→exchange and market
/// data; lower case = exchange→client replies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MsgType {
    /// Client → exchange: start a session.
    Logon = b'L',
    /// Client → exchange: enter an order ([`Command::New`]).
    NewOrder = b'N',
    /// Client → exchange: cancel an order ([`Command::Cancel`]).
    Cancel = b'X',
    /// Client → exchange: reduce an order ([`Command::Reduce`]).
    Reduce = b'R',
    /// Exchange → client: logon accepted.
    LogonAck = b'l',
    /// Exchange → client: [`Event::Accepted`].
    Accepted = b'a',
    /// Exchange → client: [`Event::Rejected`].
    Rejected = b'j',
    /// Exchange → client: [`Event::Trade`].
    Trade = b't',
    /// Exchange → client: [`Event::Rested`].
    Rested = b'r',
    /// Exchange → client: [`Event::Cancelled`].
    Cancelled = b'c',
    /// Exchange → client: [`Event::Expired`].
    Expired = b'e',
    /// Exchange → client: [`Event::Reduced`].
    Reduced = b'd',
    /// Market data: an order was added to the book.
    AddOrder = b'A',
    /// Market data: a resting order was (partly) executed.
    OrderExecuted = b'E',
    /// Market data: an order left the book.
    OrderDelete = b'D',
    /// Market data: aggregated quantity at a price level changed.
    L2Update = b'P',
}

impl MsgType {
    /// Every message type, in tag-table order.
    pub const ALL: [MsgType; 16] = [
        MsgType::Logon,
        MsgType::NewOrder,
        MsgType::Cancel,
        MsgType::Reduce,
        MsgType::LogonAck,
        MsgType::Accepted,
        MsgType::Rejected,
        MsgType::Trade,
        MsgType::Rested,
        MsgType::Cancelled,
        MsgType::Expired,
        MsgType::Reduced,
        MsgType::AddOrder,
        MsgType::OrderExecuted,
        MsgType::OrderDelete,
        MsgType::L2Update,
    ];

    /// The message type for a tag byte, or `None` for an unknown tag. O(1).
    #[inline]
    pub const fn from_tag(tag: u8) -> Option<MsgType> {
        Some(match tag {
            b'L' => MsgType::Logon,
            b'N' => MsgType::NewOrder,
            b'X' => MsgType::Cancel,
            b'R' => MsgType::Reduce,
            b'l' => MsgType::LogonAck,
            b'a' => MsgType::Accepted,
            b'j' => MsgType::Rejected,
            b't' => MsgType::Trade,
            b'r' => MsgType::Rested,
            b'c' => MsgType::Cancelled,
            b'e' => MsgType::Expired,
            b'd' => MsgType::Reduced,
            b'A' => MsgType::AddOrder,
            b'E' => MsgType::OrderExecuted,
            b'D' => MsgType::OrderDelete,
            b'P' => MsgType::L2Update,
            _ => return None,
        })
    }

    /// The tag byte (byte 0 of the message).
    #[inline]
    pub const fn tag(self) -> u8 {
        self as u8
    }

    /// Fixed encoded length of this message type in bytes (tag included).
    #[inline]
    pub const fn wire_len(self) -> usize {
        match self {
            MsgType::Logon | MsgType::LogonAck => 16,
            MsgType::NewOrder | MsgType::Trade => 40,
            MsgType::Cancel | MsgType::Accepted | MsgType::Rejected | MsgType::OrderDelete => 16,
            MsgType::Reduce
            | MsgType::Cancelled
            | MsgType::Expired
            | MsgType::Reduced
            | MsgType::L2Update => 24,
            MsgType::Rested | MsgType::AddOrder | MsgType::OrderExecuted => 32,
        }
    }
}

/// Client → exchange session start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Logon {
    /// The account this session trades for.
    pub account: AccountId,
    /// Opaque credential checked by the gateway.
    pub token: u64,
}

/// Exchange → client: the logon was accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LogonAck {
    /// The account the session is bound to.
    pub account: AccountId,
    /// Gateway-assigned session id (for logs and support).
    pub session: u64,
}

/// Order-by-order (L3) and aggregated (L2) market data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MarketData {
    /// An order now rests on the book.
    AddOrder {
        /// Instrument of the book.
        instrument: InstrumentId,
        /// The resting order.
        id: OrderId,
        /// Book side.
        side: Side,
        /// Limit price in ticks.
        price: Price,
        /// Displayed quantity.
        qty: Qty,
    },
    /// A resting order traded `qty` at `price`.
    OrderExecuted {
        /// Instrument of the book.
        instrument: InstrumentId,
        /// The resting (maker) order.
        id: OrderId,
        /// Execution price in ticks.
        price: Price,
        /// Executed quantity.
        qty: Qty,
    },
    /// An order left the book (cancelled, expired or fully reduced).
    OrderDelete {
        /// Instrument of the book.
        instrument: InstrumentId,
        /// The order that left.
        id: OrderId,
    },
    /// Total quantity at one price level changed (`qty == 0`: level gone).
    L2Update {
        /// Instrument of the book.
        instrument: InstrumentId,
        /// Book side.
        side: Side,
        /// Level price in ticks.
        price: Price,
        /// New total quantity at the level.
        qty: Qty,
    },
}

/// Any message of the week-11 protocol. Commands and events are the shared
/// [`types`], so the mapping to the engine is `Message::Command(cmd)` /
/// `Message::Event(ev)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Message {
    /// Client → exchange: `L`.
    Logon(Logon),
    /// Exchange → client: `l`.
    LogonAck(LogonAck),
    /// Client → exchange: `N`, `X`, `R`.
    Command(Command),
    /// Exchange → client: `a`, `j`, `t`, `r`, `c`, `e`, `d`.
    Event(Event),
    /// Market data: `A`, `E`, `D`, `P`.
    MarketData(MarketData),
}

impl Message {
    /// The wire type of this message. O(1).
    #[inline]
    pub const fn msg_type(&self) -> MsgType {
        match self {
            Message::Logon(_) => MsgType::Logon,
            Message::LogonAck(_) => MsgType::LogonAck,
            Message::Command(Command::New(_)) => MsgType::NewOrder,
            Message::Command(Command::Cancel { .. }) => MsgType::Cancel,
            Message::Command(Command::Reduce { .. }) => MsgType::Reduce,
            Message::Event(Event::Accepted { .. }) => MsgType::Accepted,
            Message::Event(Event::Rejected { .. }) => MsgType::Rejected,
            Message::Event(Event::Trade { .. }) => MsgType::Trade,
            Message::Event(Event::Rested { .. }) => MsgType::Rested,
            Message::Event(Event::Cancelled { .. }) => MsgType::Cancelled,
            Message::Event(Event::Expired { .. }) => MsgType::Expired,
            Message::Event(Event::Reduced { .. }) => MsgType::Reduced,
            Message::MarketData(MarketData::AddOrder { .. }) => MsgType::AddOrder,
            Message::MarketData(MarketData::OrderExecuted { .. }) => MsgType::OrderExecuted,
            Message::MarketData(MarketData::OrderDelete { .. }) => MsgType::OrderDelete,
            Message::MarketData(MarketData::L2Update { .. }) => MsgType::L2Update,
        }
    }

    /// Encoded length in bytes (see [`MsgType::wire_len`]).
    #[inline]
    pub const fn wire_len(&self) -> usize {
        self.msg_type().wire_len()
    }
}

impl From<Command> for Message {
    fn from(c: Command) -> Message {
        Message::Command(c)
    }
}

impl From<Event> for Message {
    fn from(e: Event) -> Message {
        Message::Event(e)
    }
}

impl From<MarketData> for Message {
    fn from(m: MarketData) -> Message {
        Message::MarketData(m)
    }
}

/// Why bytes could not be decoded. Shared by the week-11 codec, the views,
/// and the week-35 schema codecs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The buffer holds fewer bytes than the message (or header) needs.
    Truncated {
        /// Bytes required.
        need: usize,
        /// Bytes available.
        have: usize,
    },
    /// Unknown type tag (week 11) or template id (week 35).
    UnknownType(u8),
    /// A view or typed decoder was handed a different message type.
    UnexpectedType {
        /// The tag / template the decoder handles.
        expected: u8,
        /// The tag / template found in byte 0.
        found: u8,
    },
    /// An enumerated or constrained field holds a value outside its domain
    /// (a side byte that is not `B`/`S`, a reserved flag bit, ...).
    InvalidField {
        /// Field name, as in the layout table.
        field: &'static str,
        /// The offending value.
        value: u64,
    },
    /// The protocol or schema version is not supported (e.g. version 0).
    UnsupportedVersion(u8),
    /// Week 35: the header's block length is shorter than its version
    /// requires.
    BlockTooShort {
        /// Block length the version requires.
        need: usize,
        /// Block length in the header.
        have: usize,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Truncated { need, have } => {
                write!(f, "truncated message: need {need} bytes, have {have}")
            }
            DecodeError::UnknownType(t) => write!(f, "unknown message type 0x{t:02x}"),
            DecodeError::UnexpectedType { expected, found } => {
                write!(
                    f,
                    "expected message type 0x{expected:02x}, found 0x{found:02x}"
                )
            }
            DecodeError::InvalidField { field, value } => {
                write!(f, "invalid value {value} for field `{field}`")
            }
            DecodeError::UnsupportedVersion(v) => write!(f, "unsupported version {v}"),
            DecodeError::BlockTooShort { need, have } => {
                write!(
                    f,
                    "block length {have} is shorter than the {need} bytes its version needs"
                )
            }
        }
    }
}

impl std::error::Error for DecodeError {}

// ---------------------------------------------------------------------------
// Enumerated codes (part of the protocol contract: see the code tables in
// the crate docs).

/// Wire byte for a side. O(1).
#[inline]
pub const fn side_byte(side: Side) -> u8 {
    match side {
        Side::Buy => SIDE_BUY,
        Side::Sell => SIDE_SELL,
    }
}

/// Side for a wire byte, `None` if it is neither `B` nor `S`. O(1).
#[inline]
pub const fn side_from_byte(b: u8) -> Option<Side> {
    match b {
        SIDE_BUY => Some(Side::Buy),
        SIDE_SELL => Some(Side::Sell),
        _ => None,
    }
}

/// Wire byte for a time in force. O(1).
#[inline]
pub const fn tif_byte(tif: TimeInForce) -> u8 {
    match tif {
        TimeInForce::Gtc => TIF_GTC,
        TimeInForce::Ioc => TIF_IOC,
        TimeInForce::Fok => TIF_FOK,
    }
}

/// Time in force for a wire byte, `None` if unknown. O(1).
#[inline]
pub const fn tif_from_byte(b: u8) -> Option<TimeInForce> {
    match b {
        TIF_GTC => Some(TimeInForce::Gtc),
        TIF_IOC => Some(TimeInForce::Ioc),
        TIF_FOK => Some(TimeInForce::Fok),
        _ => None,
    }
}

/// Reject reason code carried in byte 1 of `Rejected`. O(1).
#[inline]
pub const fn reject_code(reason: RejectReason) -> u8 {
    match reason {
        RejectReason::DuplicateOrderId => 1,
        RejectReason::UnknownOrder => 2,
        RejectReason::ZeroQuantity => 3,
        RejectReason::InvalidTick => 4,
        RejectReason::InvalidLot => 5,
        RejectReason::PriceBand => 6,
        RejectReason::WouldTake => 7,
        RejectReason::FokUnfillable => 8,
        RejectReason::NoLiquidity => 9,
        RejectReason::InvalidReduce => 10,
        RejectReason::Halted => 11,
        RejectReason::Risk(RiskReason::MaxOrderQty) => 32,
        RejectReason::Risk(RiskReason::MaxNotional) => 33,
        RejectReason::Risk(RiskReason::PositionLimit) => 34,
        RejectReason::Risk(RiskReason::RateLimit) => 35,
        RejectReason::Risk(RiskReason::KillSwitch) => 36,
    }
}

/// Reject reason for a code, `None` if the code is unassigned. O(1).
#[inline]
pub const fn reject_from_code(code: u8) -> Option<RejectReason> {
    Some(match code {
        1 => RejectReason::DuplicateOrderId,
        2 => RejectReason::UnknownOrder,
        3 => RejectReason::ZeroQuantity,
        4 => RejectReason::InvalidTick,
        5 => RejectReason::InvalidLot,
        6 => RejectReason::PriceBand,
        7 => RejectReason::WouldTake,
        8 => RejectReason::FokUnfillable,
        9 => RejectReason::NoLiquidity,
        10 => RejectReason::InvalidReduce,
        11 => RejectReason::Halted,
        32 => RejectReason::Risk(RiskReason::MaxOrderQty),
        33 => RejectReason::Risk(RiskReason::MaxNotional),
        34 => RejectReason::Risk(RiskReason::PositionLimit),
        35 => RejectReason::Risk(RiskReason::RateLimit),
        36 => RejectReason::Risk(RiskReason::KillSwitch),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Little-endian field helpers. `buf[at..at + N].try_into().unwrap()` turns a
// slice into a `[u8; N]` without copying through memory; when the caller
// holds a `&[u8; LEN]` and `at` is a constant, every bounds check folds away
// at compile time and each read is a single unaligned load.

// @student w11
/// Reads a little-endian `u16` at byte offset `at`. Panics if out of bounds.
#[inline]
pub fn read_u16(buf: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(buf[at..at + 2].try_into().unwrap())
}

// @student w11
/// Reads a little-endian `u32` at byte offset `at`. Panics if out of bounds.
#[inline]
pub fn read_u32(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(buf[at..at + 4].try_into().unwrap())
}

// @student w11
/// Reads a little-endian `u64` at byte offset `at`. Panics if out of bounds.
#[inline]
pub fn read_u64(buf: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(buf[at..at + 8].try_into().unwrap())
}

// @student w11
/// Reads a little-endian `i64` at byte offset `at`. Panics if out of bounds.
#[inline]
pub fn read_i64(buf: &[u8], at: usize) -> i64 {
    i64::from_le_bytes(buf[at..at + 8].try_into().unwrap())
}

// @student w11
/// Writes `v` little-endian at byte offset `at`. Panics if out of bounds.
#[inline]
pub fn write_u16(buf: &mut [u8], at: usize, v: u16) {
    buf[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

// @student w11
/// Writes `v` little-endian at byte offset `at`. Panics if out of bounds.
#[inline]
pub fn write_u32(buf: &mut [u8], at: usize, v: u32) {
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

// @student w11
/// Writes `v` little-endian at byte offset `at`. Panics if out of bounds.
#[inline]
pub fn write_u64(buf: &mut [u8], at: usize, v: u64) {
    buf[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

// @student w11
/// Writes `v` little-endian at byte offset `at`. Panics if out of bounds.
#[inline]
pub fn write_i64(buf: &mut [u8], at: usize, v: i64) {
    buf[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

// @student-begin w11
/// Field offsets, one module per layout (the tables in the crate docs).
pub(crate) mod at {
    /// `Logon` / `LogonAck`.
    pub mod logon {
        pub const VERSION: usize = 1;
        pub const ACCOUNT: usize = 4;
        /// `token` in Logon, `session` in LogonAck.
        pub const TOKEN: usize = 8;
    }
    /// `NewOrder`.
    pub mod new_order {
        pub const SIDE: usize = 1;
        pub const ORD_TYPE: usize = 2;
        pub const TIF: usize = 3;
        pub const FLAGS: usize = 4;
        pub const ID: usize = 8;
        pub const ACCOUNT: usize = 16;
        pub const INSTRUMENT: usize = 20;
        pub const QTY: usize = 24;
        pub const PRICE: usize = 32;
    }
    /// `Cancel`, `Reduce`, `Accepted`, `Rejected`, `Cancelled`, `Expired`,
    /// `Reduced`: an order id at 8, maybe a code at 1 and a quantity at 16.
    pub mod order {
        pub const CODE: usize = 1;
        pub const ID: usize = 8;
        pub const QTY: usize = 16;
    }
    /// `Trade`.
    pub mod trade {
        pub const SIDE: usize = 1;
        pub const TAKER: usize = 8;
        pub const MAKER: usize = 16;
        pub const PRICE: usize = 24;
        pub const QTY: usize = 32;
    }
    /// `Rested`.
    pub mod rested {
        pub const SIDE: usize = 1;
        pub const ID: usize = 8;
        pub const PRICE: usize = 16;
        pub const QTY: usize = 24;
    }
    /// `AddOrder`, `OrderExecuted`, `OrderDelete`.
    pub mod md_order {
        pub const SIDE: usize = 1;
        pub const INSTRUMENT: usize = 4;
        pub const ID: usize = 8;
        pub const PRICE: usize = 16;
        pub const QTY: usize = 24;
    }
    /// `L2Update`.
    pub mod l2 {
        pub const SIDE: usize = 1;
        pub const INSTRUMENT: usize = 4;
        pub const PRICE: usize = 8;
        pub const QTY: usize = 16;
    }
}

/// Checks the tag and length of a fixed-size message and returns its bytes
/// as an array reference: the type now carries the length, so reads at
/// constant offsets need no further bounds checks.
#[inline]
pub(crate) fn fixed<const N: usize>(buf: &[u8], ty: MsgType) -> Result<&[u8; N], DecodeError> {
    debug_assert_eq!(N, ty.wire_len());
    let Some(&tag) = buf.first() else {
        return Err(DecodeError::Truncated { need: N, have: 0 });
    };
    if tag != ty.tag() {
        return Err(DecodeError::UnexpectedType {
            expected: ty.tag(),
            found: tag,
        });
    }
    match buf.get(..N) {
        Some(b) => Ok(b.try_into().unwrap()),
        None => Err(DecodeError::Truncated {
            need: N,
            have: buf.len(),
        }),
    }
}

/// Decodes a side byte or reports which field was wrong.
#[inline]
pub(crate) fn side_at(b: u8, field: &'static str) -> Result<Side, DecodeError> {
    side_from_byte(b).ok_or(DecodeError::InvalidField {
        field,
        value: b as u64,
    })
}
// @student-end

// @student w11
/// Encodes `msg` at the start of `buf` and returns the number of bytes
/// written (always `msg.wire_len()`). Reserved bytes are written as zero.
///
/// O(1), no allocation. Panics if `buf` is shorter than `msg.wire_len()`
/// (any buffer of [`MAX_MSG_LEN`] bytes is big enough).
pub fn encode_into(msg: &Message, buf: &mut [u8]) -> usize {
    use at::*;
    let ty = msg.msg_type();
    let len = ty.wire_len();
    assert!(
        buf.len() >= len,
        "encode_into: {ty:?} needs {len} bytes, buffer has {}",
        buf.len()
    );
    let b = &mut buf[..len];
    b.fill(0);
    b[0] = ty.tag();
    match *msg {
        Message::Logon(Logon { account, token }) => {
            b[logon::VERSION] = PROTOCOL_VERSION;
            write_u32(b, logon::ACCOUNT, account.0);
            write_u64(b, logon::TOKEN, token);
        }
        Message::LogonAck(LogonAck { account, session }) => {
            b[logon::VERSION] = PROTOCOL_VERSION;
            write_u32(b, logon::ACCOUNT, account.0);
            write_u64(b, logon::TOKEN, session);
        }
        Message::Command(Command::New(o)) => {
            let (ord_type, flags, price) = match o.kind {
                OrderKind::Limit { price, post_only } => (
                    ORD_LIMIT,
                    if post_only { FLAG_POST_ONLY } else { 0 },
                    price.0,
                ),
                OrderKind::Market => (ORD_MARKET, 0, 0),
            };
            b[new_order::SIDE] = side_byte(o.side);
            b[new_order::ORD_TYPE] = ord_type;
            b[new_order::TIF] = tif_byte(o.tif);
            b[new_order::FLAGS] = flags;
            write_u64(b, new_order::ID, o.id.0);
            write_u32(b, new_order::ACCOUNT, o.account.0);
            write_u32(b, new_order::INSTRUMENT, o.instrument.0);
            write_u64(b, new_order::QTY, o.qty.0);
            write_i64(b, new_order::PRICE, price);
        }
        Message::Command(Command::Cancel { id }) | Message::Event(Event::Accepted { id }) => {
            write_u64(b, order::ID, id.0);
        }
        Message::Command(Command::Reduce { id, qty: q })
        | Message::Event(Event::Cancelled { id, remaining: q })
        | Message::Event(Event::Expired { id, remaining: q })
        | Message::Event(Event::Reduced { id, remaining: q }) => {
            write_u64(b, order::ID, id.0);
            write_u64(b, order::QTY, q.0);
        }
        Message::Event(Event::Rejected { id, reason }) => {
            b[order::CODE] = reject_code(reason);
            write_u64(b, order::ID, id.0);
        }
        Message::Event(Event::Trade {
            taker,
            maker,
            taker_side,
            price,
            qty,
        }) => {
            b[trade::SIDE] = side_byte(taker_side);
            write_u64(b, trade::TAKER, taker.0);
            write_u64(b, trade::MAKER, maker.0);
            write_i64(b, trade::PRICE, price.0);
            write_u64(b, trade::QTY, qty.0);
        }
        Message::Event(Event::Rested {
            id,
            side,
            price,
            qty,
        }) => {
            b[rested::SIDE] = side_byte(side);
            write_u64(b, rested::ID, id.0);
            write_i64(b, rested::PRICE, price.0);
            write_u64(b, rested::QTY, qty.0);
        }
        Message::MarketData(md) => match md {
            MarketData::AddOrder {
                instrument,
                id,
                side,
                price,
                qty,
            } => {
                b[md_order::SIDE] = side_byte(side);
                write_u32(b, md_order::INSTRUMENT, instrument.0);
                write_u64(b, md_order::ID, id.0);
                write_i64(b, md_order::PRICE, price.0);
                write_u64(b, md_order::QTY, qty.0);
            }
            MarketData::OrderExecuted {
                instrument,
                id,
                price,
                qty,
            } => {
                write_u32(b, md_order::INSTRUMENT, instrument.0);
                write_u64(b, md_order::ID, id.0);
                write_i64(b, md_order::PRICE, price.0);
                write_u64(b, md_order::QTY, qty.0);
            }
            MarketData::OrderDelete { instrument, id } => {
                write_u32(b, md_order::INSTRUMENT, instrument.0);
                write_u64(b, md_order::ID, id.0);
            }
            MarketData::L2Update {
                instrument,
                side,
                price,
                qty,
            } => {
                b[l2::SIDE] = side_byte(side);
                write_u32(b, l2::INSTRUMENT, instrument.0);
                write_i64(b, l2::PRICE, price.0);
                write_u64(b, l2::QTY, qty.0);
            }
        },
    }
    len
}

// @student w11
/// Decodes the message at the start of `buf` (normally exactly one frame's
/// payload). Bytes beyond the type's fixed length are ignored, which is
/// what lets a later protocol revision append fields.
///
/// O(1), no allocation. Fails on an empty or short buffer, an unknown tag,
/// or an out-of-domain enumerated field (side, order type, time in force,
/// flags, reject code, protocol version).
pub fn decode(buf: &[u8]) -> Result<Message, DecodeError> {
    use at::*;
    let Some(&tag) = buf.first() else {
        return Err(DecodeError::Truncated { need: 1, have: 0 });
    };
    let ty = MsgType::from_tag(tag).ok_or(DecodeError::UnknownType(tag))?;
    let id = |b: &[u8]| OrderId(read_u64(b, order::ID));
    let qty = |b: &[u8]| Qty(read_u64(b, order::QTY));
    Ok(match ty {
        MsgType::Logon => Message::Logon(LogonView::new(buf)?.to_logon()),
        MsgType::NewOrder => Message::Command(Command::New(NewOrderView::new(buf)?.to_new_order())),
        MsgType::Cancel => Message::Command(CancelView::new(buf)?.to_command()),
        MsgType::Reduce => Message::Command(ReduceView::new(buf)?.to_command()),
        MsgType::LogonAck => {
            let b = fixed::<16>(buf, ty)?;
            if b[logon::VERSION] != PROTOCOL_VERSION {
                return Err(DecodeError::UnsupportedVersion(b[logon::VERSION]));
            }
            Message::LogonAck(LogonAck {
                account: AccountId(read_u32(b, logon::ACCOUNT)),
                session: read_u64(b, logon::TOKEN),
            })
        }
        MsgType::Accepted => Message::Event(Event::Accepted {
            id: id(fixed::<16>(buf, ty)?),
        }),
        MsgType::Rejected => {
            let b = fixed::<16>(buf, ty)?;
            let code = b[order::CODE];
            let reason = reject_from_code(code).ok_or(DecodeError::InvalidField {
                field: "reason",
                value: code as u64,
            })?;
            Message::Event(Event::Rejected { id: id(b), reason })
        }
        MsgType::Trade => {
            let b = fixed::<40>(buf, ty)?;
            Message::Event(Event::Trade {
                taker: OrderId(read_u64(b, trade::TAKER)),
                maker: OrderId(read_u64(b, trade::MAKER)),
                taker_side: side_at(b[trade::SIDE], "taker_side")?,
                price: Price(read_i64(b, trade::PRICE)),
                qty: Qty(read_u64(b, trade::QTY)),
            })
        }
        MsgType::Rested => {
            let b = fixed::<32>(buf, ty)?;
            Message::Event(Event::Rested {
                id: OrderId(read_u64(b, rested::ID)),
                side: side_at(b[rested::SIDE], "side")?,
                price: Price(read_i64(b, rested::PRICE)),
                qty: Qty(read_u64(b, rested::QTY)),
            })
        }
        MsgType::Cancelled => {
            let b = fixed::<24>(buf, ty)?;
            Message::Event(Event::Cancelled {
                id: id(b),
                remaining: qty(b),
            })
        }
        MsgType::Expired => {
            let b = fixed::<24>(buf, ty)?;
            Message::Event(Event::Expired {
                id: id(b),
                remaining: qty(b),
            })
        }
        MsgType::Reduced => {
            let b = fixed::<24>(buf, ty)?;
            Message::Event(Event::Reduced {
                id: id(b),
                remaining: qty(b),
            })
        }
        MsgType::AddOrder => {
            let b = fixed::<32>(buf, ty)?;
            Message::MarketData(MarketData::AddOrder {
                instrument: InstrumentId(read_u32(b, md_order::INSTRUMENT)),
                id: OrderId(read_u64(b, md_order::ID)),
                side: side_at(b[md_order::SIDE], "side")?,
                price: Price(read_i64(b, md_order::PRICE)),
                qty: Qty(read_u64(b, md_order::QTY)),
            })
        }
        MsgType::OrderExecuted => {
            let b = fixed::<32>(buf, ty)?;
            Message::MarketData(MarketData::OrderExecuted {
                instrument: InstrumentId(read_u32(b, md_order::INSTRUMENT)),
                id: OrderId(read_u64(b, md_order::ID)),
                price: Price(read_i64(b, md_order::PRICE)),
                qty: Qty(read_u64(b, md_order::QTY)),
            })
        }
        MsgType::OrderDelete => {
            let b = fixed::<16>(buf, ty)?;
            Message::MarketData(MarketData::OrderDelete {
                instrument: InstrumentId(read_u32(b, md_order::INSTRUMENT)),
                id: OrderId(read_u64(b, md_order::ID)),
            })
        }
        MsgType::L2Update => {
            let b = fixed::<24>(buf, ty)?;
            Message::MarketData(MarketData::L2Update {
                instrument: InstrumentId(read_u32(b, l2::INSTRUMENT)),
                side: side_at(b[l2::SIDE], "side")?,
                price: Price(read_i64(b, l2::PRICE)),
                qty: Qty(read_u64(b, l2::QTY)),
            })
        }
    })
}
