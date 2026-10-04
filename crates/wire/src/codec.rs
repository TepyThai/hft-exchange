//! Week 11: the fixed-layout protocol — message types, enumerated codes,
//! little-endian field helpers, and the [`encode_into`] / [`decode`] pair.
//!
//! Every layout is documented in a table in the [crate docs](crate). A
//! message's length is decided by its one-byte type tag ([`MsgType::wire_len`]),
//! so a decoder knows how many bytes to expect after reading one byte.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
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

/// Reads a little-endian `u16` at byte offset `at`. Panics if out of bounds.
#[inline]
#[allow(unused_variables)]
pub fn read_u16(buf: &[u8], at: usize) -> u16 {
    todo!("w11: read_u16")
}

/// Reads a little-endian `u32` at byte offset `at`. Panics if out of bounds.
#[inline]
#[allow(unused_variables)]
pub fn read_u32(buf: &[u8], at: usize) -> u32 {
    todo!("w11: read_u32")
}

/// Reads a little-endian `u64` at byte offset `at`. Panics if out of bounds.
#[inline]
#[allow(unused_variables)]
pub fn read_u64(buf: &[u8], at: usize) -> u64 {
    todo!("w11: read_u64")
}

/// Reads a little-endian `i64` at byte offset `at`. Panics if out of bounds.
#[inline]
#[allow(unused_variables)]
pub fn read_i64(buf: &[u8], at: usize) -> i64 {
    todo!("w11: read_i64")
}

/// Writes `v` little-endian at byte offset `at`. Panics if out of bounds.
#[inline]
#[allow(unused_variables)]
pub fn write_u16(buf: &mut [u8], at: usize, v: u16) {
    todo!("w11: write_u16")
}

/// Writes `v` little-endian at byte offset `at`. Panics if out of bounds.
#[inline]
#[allow(unused_variables)]
pub fn write_u32(buf: &mut [u8], at: usize, v: u32) {
    todo!("w11: write_u32")
}

/// Writes `v` little-endian at byte offset `at`. Panics if out of bounds.
#[inline]
#[allow(unused_variables)]
pub fn write_u64(buf: &mut [u8], at: usize, v: u64) {
    todo!("w11: write_u64")
}

/// Writes `v` little-endian at byte offset `at`. Panics if out of bounds.
#[inline]
#[allow(unused_variables)]
pub fn write_i64(buf: &mut [u8], at: usize, v: i64) {
    todo!("w11: write_i64")
}

// TODO(w11): implement — see the week guide on tepy.dev/hft.

/// Encodes `msg` at the start of `buf` and returns the number of bytes
/// written (always `msg.wire_len()`). Reserved bytes are written as zero.
///
/// O(1), no allocation. Panics if `buf` is shorter than `msg.wire_len()`
/// (any buffer of [`MAX_MSG_LEN`] bytes is big enough).
#[allow(unused_variables)]
pub fn encode_into(msg: &Message, buf: &mut [u8]) -> usize {
    todo!("w11: encode_into")
}

/// Decodes the message at the start of `buf` (normally exactly one frame's
/// payload). Bytes beyond the type's fixed length are ignored, which is
/// what lets a later protocol revision append fields.
///
/// O(1), no allocation. Fails on an empty or short buffer, an unknown tag,
/// or an out-of-domain enumerated field (side, order type, time in force,
/// flags, reject code, protocol version).
#[allow(unused_variables)]
pub fn decode(buf: &[u8]) -> Result<Message, DecodeError> {
    todo!("w11: decode")
}
