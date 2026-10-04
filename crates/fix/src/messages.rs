//! Typed FIX 4.4 messages: decode from a [`RawMessage`], encode through a
//! [`FieldWriter`], and map to the exchange's [`types`].
//!
//! | MsgType | message              | fields (required **bold**)                                      |
//! |---------|----------------------|-----------------------------------------------------------------|
//! | `D`     | NewOrderSingle       | **11** ClOrdID, 1 Account, **55** Symbol, **54** Side, **60** TransactTime, **38** OrderQty, **40** OrdType, 44 Price (**limit**), 59 TimeInForce (default Day), 18 ExecInst (`6` = post-only) |
//! | `F`     | OrderCancelRequest   | **41** OrigClOrdID, **11** ClOrdID, 37 OrderID, **55** Symbol, **54** Side, **60** TransactTime |
//! | `8`     | ExecutionReport      | **37** OrderID, **11** ClOrdID, 41 OrigClOrdID, **17** ExecID, **150** ExecType, **39** OrdStatus, 103 OrdRejReason, **55** Symbol, **54** Side, 38 OrderQty, 44 Price, 32 LastQty, 31 LastPx, **151** LeavesQty, **14** CumQty, **6** AvgPx, 58 Text |
//! | `A`     | Logon                | **98** EncryptMethod (0), **108** HeartBtInt, 141 ResetSeqNumFlag, 553 Username, 554 Password |
//! | `0`     | Heartbeat            | 112 TestReqID                                                   |
//! | `1`     | TestRequest          | **112** TestReqID                                               |
//! | `2`     | ResendRequest        | **7** BeginSeqNo, **16** EndSeqNo (0 = to the end)              |
//! | `4`     | SequenceReset        | 123 GapFillFlag, **36** NewSeqNo                                |
//! | `5`     | Logout               | 58 Text                                                         |
//!
//! Unknown body tags are ignored. Exchange-generated ids (OrderID 37,
//! ExecID 17) are decimal integers.

use crate::decimal::{Decimal, PriceScale};
use crate::encode::FieldWriter;
#[allow(unused_imports)] // used by the implementation the learner writes
use crate::parse::{parse_uint, Field, FixError, RawMessage};
#[allow(unused_imports)]
use types::{
    AccountId, Command, Event, InstrumentId, NewOrder, OrderId, OrderKind, Qty, RejectReason, Side,
    TimeInForce,
};

/// MsgType (35) values.
pub mod msg_type {
    /// Heartbeat.
    pub const HEARTBEAT: &[u8] = b"0";
    /// TestRequest.
    pub const TEST_REQUEST: &[u8] = b"1";
    /// ResendRequest.
    pub const RESEND_REQUEST: &[u8] = b"2";
    /// Reject (session-level).
    pub const REJECT: &[u8] = b"3";
    /// SequenceReset.
    pub const SEQUENCE_RESET: &[u8] = b"4";
    /// Logout.
    pub const LOGOUT: &[u8] = b"5";
    /// ExecutionReport.
    pub const EXECUTION_REPORT: &[u8] = b"8";
    /// Logon.
    pub const LOGON: &[u8] = b"A";
    /// NewOrderSingle.
    pub const NEW_ORDER_SINGLE: &[u8] = b"D";
    /// OrderCancelRequest.
    pub const ORDER_CANCEL_REQUEST: &[u8] = b"F";
}

/// A message body that can be encoded: its MsgType and its fields.
pub trait FixBody {
    /// MsgType (35).
    const MSG_TYPE: &'static [u8];
    /// Writes the body fields (everything after the standard header).
    fn write_body(&self, w: &mut FieldWriter<'_>);
}

// ---------------------------------------------------------------------------
// Code tables

/// Side (54) code: `1` buy, `2` sell.
pub const fn side_code(side: Side) -> u8 {
    match side {
        Side::Buy => b'1',
        Side::Sell => b'2',
    }
}

/// Side for a (54) code; other sides (sell short, ...) are not supported.
pub const fn side_from_code(c: u8) -> Option<Side> {
    match c {
        b'1' => Some(Side::Buy),
        b'2' => Some(Side::Sell),
        _ => None,
    }
}

/// OrdType (40).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OrdType {
    /// `1`
    Market,
    /// `2`
    Limit,
}

impl OrdType {
    /// Wire code.
    pub const fn code(self) -> u8 {
        match self {
            OrdType::Market => b'1',
            OrdType::Limit => b'2',
        }
    }
    /// From a wire code (stop orders etc. are not supported).
    pub const fn from_code(c: u8) -> Option<OrdType> {
        match c {
            b'1' => Some(OrdType::Market),
            b'2' => Some(OrdType::Limit),
            _ => None,
        }
    }
}

/// TimeInForce (59). Day and GTC both map to the engine's GTC (the
/// exchange has no session end).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Tif {
    /// `0` (the default when 59 is absent).
    #[default]
    Day,
    /// `1`
    Gtc,
    /// `3`
    Ioc,
    /// `4`
    Fok,
}

impl Tif {
    /// Wire code.
    pub const fn code(self) -> u8 {
        match self {
            Tif::Day => b'0',
            Tif::Gtc => b'1',
            Tif::Ioc => b'3',
            Tif::Fok => b'4',
        }
    }
    /// From a wire code.
    pub const fn from_code(c: u8) -> Option<Tif> {
        match c {
            b'0' => Some(Tif::Day),
            b'1' => Some(Tif::Gtc),
            b'3' => Some(Tif::Ioc),
            b'4' => Some(Tif::Fok),
            _ => None,
        }
    }
    /// The engine's time in force.
    pub const fn to_types(self) -> TimeInForce {
        match self {
            Tif::Day | Tif::Gtc => TimeInForce::Gtc,
            Tif::Ioc => TimeInForce::Ioc,
            Tif::Fok => TimeInForce::Fok,
        }
    }
}

/// ExecType (150).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExecType {
    /// `0` order accepted.
    New,
    /// `4` cancelled.
    Canceled,
    /// `5` replaced (our Reduce).
    Replaced,
    /// `8` rejected.
    Rejected,
    /// `C` expired (IOC / market remainder).
    Expired,
    /// `F` fill.
    Trade,
}

impl ExecType {
    /// Wire code.
    pub const fn code(self) -> u8 {
        match self {
            ExecType::New => b'0',
            ExecType::Canceled => b'4',
            ExecType::Replaced => b'5',
            ExecType::Rejected => b'8',
            ExecType::Expired => b'C',
            ExecType::Trade => b'F',
        }
    }
    /// From a wire code.
    pub const fn from_code(c: u8) -> Option<ExecType> {
        match c {
            b'0' => Some(ExecType::New),
            b'4' => Some(ExecType::Canceled),
            b'5' => Some(ExecType::Replaced),
            b'8' => Some(ExecType::Rejected),
            b'C' => Some(ExecType::Expired),
            b'F' => Some(ExecType::Trade),
            _ => None,
        }
    }
    /// The execution report an engine event produces, if any (`Rested`
    /// produces none: the order was already acknowledged by `Accepted`).
    pub const fn for_event(event: &Event) -> Option<ExecType> {
        match event {
            Event::Accepted { .. } => Some(ExecType::New),
            Event::Rejected { .. } => Some(ExecType::Rejected),
            Event::Trade { .. } => Some(ExecType::Trade),
            Event::Rested { .. } => None,
            Event::Cancelled { .. } => Some(ExecType::Canceled),
            Event::Expired { .. } => Some(ExecType::Expired),
            Event::Reduced { .. } => Some(ExecType::Replaced),
        }
    }
}

/// OrdStatus (39).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OrdStatus {
    /// `0`
    New,
    /// `1`
    PartiallyFilled,
    /// `2`
    Filled,
    /// `4`
    Canceled,
    /// `5`
    Replaced,
    /// `8`
    Rejected,
    /// `C`
    Expired,
}

impl OrdStatus {
    /// Wire code.
    pub const fn code(self) -> u8 {
        match self {
            OrdStatus::New => b'0',
            OrdStatus::PartiallyFilled => b'1',
            OrdStatus::Filled => b'2',
            OrdStatus::Canceled => b'4',
            OrdStatus::Replaced => b'5',
            OrdStatus::Rejected => b'8',
            OrdStatus::Expired => b'C',
        }
    }
    /// From a wire code.
    pub const fn from_code(c: u8) -> Option<OrdStatus> {
        match c {
            b'0' => Some(OrdStatus::New),
            b'1' => Some(OrdStatus::PartiallyFilled),
            b'2' => Some(OrdStatus::Filled),
            b'4' => Some(OrdStatus::Canceled),
            b'5' => Some(OrdStatus::Replaced),
            b'8' => Some(OrdStatus::Rejected),
            b'C' => Some(OrdStatus::Expired),
            _ => None,
        }
    }
}

/// OrdRejReason (103) for an engine reject reason.
pub const fn ord_rej_reason(reason: RejectReason) -> u32 {
    match reason {
        RejectReason::DuplicateOrderId => 6,
        RejectReason::UnknownOrder => 5,
        RejectReason::ZeroQuantity | RejectReason::InvalidLot => 13,
        RejectReason::Halted => 2,
        RejectReason::Risk(_) => 3,
        RejectReason::WouldTake | RejectReason::FokUnfillable | RejectReason::NoLiquidity => 0,
        RejectReason::InvalidTick | RejectReason::PriceBand | RejectReason::InvalidReduce => 99,
    }
}

// @student-begin w14
/// Fails unless `msg` has MsgType `t`.
#[inline]
fn expect_type(msg: &RawMessage<'_>, t: &[u8]) -> Result<(), FixError> {
    if msg.msg_type == t {
        Ok(())
    } else {
        Err(FixError::UnexpectedMsgType)
    }
}

/// The single character of a one-byte field.
#[inline]
fn one_char(f: &Field<'_>) -> Result<u8, FixError> {
    match f.value {
        [c] => Ok(*c),
        _ => Err(FixError::InvalidValue(f.tag)),
    }
}

#[inline]
fn uint(f: &Field<'_>) -> Result<u64, FixError> {
    parse_uint(f.value).ok_or(FixError::InvalidValue(f.tag))
}

#[inline]
fn decimal(f: &Field<'_>) -> Result<Decimal, FixError> {
    Decimal::parse(f.value).ok_or(FixError::InvalidValue(f.tag))
}

/// A quantity: a decimal with no fractional part (`100`, `100.00`).
#[inline]
fn qty(f: &Field<'_>) -> Result<u64, FixError> {
    match decimal(f)?.to_ticks(PriceScale::UNITS) {
        Some(q) if q >= 0 => Ok(q as u64),
        _ => Err(FixError::InvalidValue(f.tag)),
    }
}

#[inline]
fn side(f: &Field<'_>) -> Result<Side, FixError> {
    side_from_code(one_char(f)?).ok_or(FixError::InvalidValue(f.tag))
}

#[inline]
fn yes_no(f: &Field<'_>) -> Result<bool, FixError> {
    match f.value {
        b"Y" => Ok(true),
        b"N" => Ok(false),
        _ => Err(FixError::InvalidValue(f.tag)),
    }
}

#[inline]
fn required<T>(v: Option<T>, tag: u32) -> Result<T, FixError> {
    v.ok_or(FixError::MissingField(tag))
}
// @student-end

// ---------------------------------------------------------------------------
// NewOrderSingle (D)

/// NewOrderSingle (35=D): a client order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewOrderSingle<'a> {
    /// ClOrdID (11): the client's id for the order.
    pub cl_ord_id: &'a [u8],
    /// Account (1).
    pub account: Option<&'a [u8]>,
    /// Symbol (55).
    pub symbol: &'a [u8],
    /// Side (54).
    pub side: Side,
    /// TransactTime (60), unparsed.
    pub transact_time: &'a [u8],
    /// OrderQty (38).
    pub order_qty: Decimal,
    /// OrdType (40).
    pub ord_type: OrdType,
    /// Price (44); required for limit orders.
    pub price: Option<Decimal>,
    /// TimeInForce (59).
    pub time_in_force: Tif,
    /// ExecInst (18) contains `6` (participate, don't initiate).
    pub post_only: bool,
}

// @student w14
impl<'a> NewOrderSingle<'a> {
    /// Decodes the body of a validated message. One pass over the body
    /// fields, no allocation; strings borrow the message.
    pub fn decode(msg: &RawMessage<'a>) -> Result<NewOrderSingle<'a>, FixError> {
        expect_type(msg, msg_type::NEW_ORDER_SINGLE)?;
        let (mut cl_ord_id, mut account, mut symbol, mut side_v, mut transact_time) =
            (None, None, None, None, None);
        let (mut order_qty, mut ord_type, mut price) = (None, None, None);
        let mut time_in_force = Tif::Day;
        let mut post_only = false;
        for f in msg.fields() {
            let f = f?;
            match f.tag {
                11 => cl_ord_id = Some(f.value),
                1 => account = Some(f.value),
                55 => symbol = Some(f.value),
                54 => side_v = Some(side(&f)?),
                60 => transact_time = Some(f.value),
                38 => order_qty = Some(decimal(&f)?),
                40 => {
                    ord_type =
                        Some(OrdType::from_code(one_char(&f)?).ok_or(FixError::InvalidValue(40))?)
                }
                44 => price = Some(decimal(&f)?),
                59 => {
                    time_in_force =
                        Tif::from_code(one_char(&f)?).ok_or(FixError::InvalidValue(59))?
                }
                18 => post_only = f.value.split(|&b| b == b' ').any(|v| v == b"6"),
                _ => {}
            }
        }
        let ord_type = required(ord_type, 40)?;
        if ord_type == OrdType::Limit && price.is_none() {
            return Err(FixError::MissingField(44));
        }
        Ok(NewOrderSingle {
            cl_ord_id: required(cl_ord_id, 11)?,
            account,
            symbol: required(symbol, 55)?,
            side: required(side_v, 54)?,
            transact_time: required(transact_time, 60)?,
            order_qty: required(order_qty, 38)?,
            ord_type,
            price,
            time_in_force,
            post_only,
        })
    }

    /// Maps to the engine's order. The gateway supplies what FIX does not
    /// carry in our form: the exchange order id, the session's account and
    /// the instrument resolved from Symbol. The price is converted to ticks
    /// at `scale` without floats; OrderQty must be a whole number of lots.
    /// Fails with `InvalidValue(44)` for a price not on a tick and
    /// `InvalidValue(18)` for a post-only market order. O(1).
    pub fn to_new_order(
        &self,
        id: OrderId,
        account: AccountId,
        instrument: InstrumentId,
        scale: PriceScale,
    ) -> Result<NewOrder, FixError> {
        let qty = match self.order_qty.to_ticks(PriceScale::UNITS) {
            Some(q) if q >= 0 => Qty(q as u64),
            _ => return Err(FixError::InvalidValue(38)),
        };
        let kind = match self.ord_type {
            OrdType::Market if self.post_only => return Err(FixError::InvalidValue(18)),
            OrdType::Market => OrderKind::Market,
            OrdType::Limit => {
                let price = self.price.ok_or(FixError::MissingField(44))?;
                let ticks = price.to_ticks(scale).ok_or(FixError::InvalidValue(44))?;
                OrderKind::Limit {
                    price: types::Price(ticks),
                    post_only: self.post_only,
                }
            }
        };
        Ok(NewOrder {
            id,
            account,
            instrument,
            side: self.side,
            qty,
            kind,
            tif: self.time_in_force.to_types(),
        })
    }
}

// @student w14
impl FixBody for NewOrderSingle<'_> {
    const MSG_TYPE: &'static [u8] = msg_type::NEW_ORDER_SINGLE;

    fn write_body(&self, w: &mut FieldWriter<'_>) {
        w.bytes(11, self.cl_ord_id);
        if let Some(a) = self.account {
            w.bytes(1, a);
        }
        w.bytes(55, self.symbol)
            .char(54, side_code(self.side))
            .bytes(60, self.transact_time)
            .decimal(38, self.order_qty)
            .char(40, self.ord_type.code());
        if let Some(p) = self.price {
            w.decimal(44, p);
        }
        w.char(59, self.time_in_force.code());
        if self.post_only {
            w.char(18, b'6');
        }
    }
}

// ---------------------------------------------------------------------------
// OrderCancelRequest (F)

/// OrderCancelRequest (35=F).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrderCancelRequest<'a> {
    /// OrigClOrdID (41): the order to cancel, by client id.
    pub orig_cl_ord_id: &'a [u8],
    /// ClOrdID (11) of this request.
    pub cl_ord_id: &'a [u8],
    /// OrderID (37): the exchange id, if the client knows it.
    pub order_id: Option<u64>,
    /// Symbol (55).
    pub symbol: &'a [u8],
    /// Side (54).
    pub side: Side,
    /// TransactTime (60), unparsed.
    pub transact_time: &'a [u8],
}

// @student w14
impl<'a> OrderCancelRequest<'a> {
    /// Decodes the body of a validated message. O(fields), no allocation.
    pub fn decode(msg: &RawMessage<'a>) -> Result<OrderCancelRequest<'a>, FixError> {
        expect_type(msg, msg_type::ORDER_CANCEL_REQUEST)?;
        let (mut orig, mut cl, mut order_id, mut symbol, mut side_v, mut time) =
            (None, None, None, None, None, None);
        for f in msg.fields() {
            let f = f?;
            match f.tag {
                41 => orig = Some(f.value),
                11 => cl = Some(f.value),
                37 => order_id = Some(uint(&f)?),
                55 => symbol = Some(f.value),
                54 => side_v = Some(side(&f)?),
                60 => time = Some(f.value),
                _ => {}
            }
        }
        Ok(OrderCancelRequest {
            orig_cl_ord_id: required(orig, 41)?,
            cl_ord_id: required(cl, 11)?,
            order_id,
            symbol: required(symbol, 55)?,
            side: required(side_v, 54)?,
            transact_time: required(time, 60)?,
        })
    }

    /// `Command::Cancel` when OrderID (37) is present; otherwise the gateway
    /// must resolve OrigClOrdID to an order id itself.
    pub fn to_command(&self) -> Option<Command> {
        self.order_id.map(|id| Command::Cancel { id: OrderId(id) })
    }
}

// @student w14
impl FixBody for OrderCancelRequest<'_> {
    const MSG_TYPE: &'static [u8] = msg_type::ORDER_CANCEL_REQUEST;

    fn write_body(&self, w: &mut FieldWriter<'_>) {
        w.bytes(41, self.orig_cl_ord_id).bytes(11, self.cl_ord_id);
        if let Some(id) = self.order_id {
            w.uint(37, id);
        }
        w.bytes(55, self.symbol)
            .char(54, side_code(self.side))
            .bytes(60, self.transact_time);
    }
}

// ---------------------------------------------------------------------------
// ExecutionReport (8)

/// ExecutionReport (35=8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionReport<'a> {
    /// OrderID (37): the exchange order id.
    pub order_id: u64,
    /// ClOrdID (11).
    pub cl_ord_id: &'a [u8],
    /// OrigClOrdID (41), on cancel / replace reports.
    pub orig_cl_ord_id: Option<&'a [u8]>,
    /// ExecID (17): unique per report.
    pub exec_id: u64,
    /// ExecType (150).
    pub exec_type: ExecType,
    /// OrdStatus (39).
    pub ord_status: OrdStatus,
    /// OrdRejReason (103), on rejects.
    pub ord_rej_reason: Option<u32>,
    /// Symbol (55).
    pub symbol: &'a [u8],
    /// Side (54).
    pub side: Side,
    /// OrderQty (38).
    pub order_qty: Option<u64>,
    /// Price (44).
    pub price: Option<Decimal>,
    /// LastQty (32), on fills.
    pub last_qty: Option<u64>,
    /// LastPx (31), on fills.
    pub last_px: Option<Decimal>,
    /// LeavesQty (151).
    pub leaves_qty: u64,
    /// CumQty (14).
    pub cum_qty: u64,
    /// AvgPx (6).
    pub avg_px: Decimal,
    /// Text (58).
    pub text: Option<&'a [u8]>,
}

// @student w14
impl<'a> ExecutionReport<'a> {
    /// Decodes the body of a validated message. O(fields), no allocation.
    pub fn decode(msg: &RawMessage<'a>) -> Result<ExecutionReport<'a>, FixError> {
        expect_type(msg, msg_type::EXECUTION_REPORT)?;
        let (mut order_id, mut cl, mut orig, mut exec_id, mut exec_type, mut ord_status) =
            (None, None, None, None, None, None);
        let (mut rej, mut symbol, mut side_v, mut order_qty, mut price) =
            (None, None, None, None, None);
        let (mut last_qty, mut last_px, mut leaves, mut cum, mut avg, mut text) =
            (None, None, None, None, None, None);
        for f in msg.fields() {
            let f = f?;
            match f.tag {
                37 => order_id = Some(uint(&f)?),
                11 => cl = Some(f.value),
                41 => orig = Some(f.value),
                17 => exec_id = Some(uint(&f)?),
                150 => {
                    exec_type = Some(
                        ExecType::from_code(one_char(&f)?).ok_or(FixError::InvalidValue(150))?,
                    )
                }
                39 => {
                    ord_status = Some(
                        OrdStatus::from_code(one_char(&f)?).ok_or(FixError::InvalidValue(39))?,
                    )
                }
                103 => rej = Some(uint(&f)? as u32),
                55 => symbol = Some(f.value),
                54 => side_v = Some(side(&f)?),
                38 => order_qty = Some(qty(&f)?),
                44 => price = Some(decimal(&f)?),
                32 => last_qty = Some(qty(&f)?),
                31 => last_px = Some(decimal(&f)?),
                151 => leaves = Some(qty(&f)?),
                14 => cum = Some(qty(&f)?),
                6 => avg = Some(decimal(&f)?),
                58 => text = Some(f.value),
                _ => {}
            }
        }
        Ok(ExecutionReport {
            order_id: required(order_id, 37)?,
            cl_ord_id: required(cl, 11)?,
            orig_cl_ord_id: orig,
            exec_id: required(exec_id, 17)?,
            exec_type: required(exec_type, 150)?,
            ord_status: required(ord_status, 39)?,
            ord_rej_reason: rej,
            symbol: required(symbol, 55)?,
            side: required(side_v, 54)?,
            order_qty,
            price,
            last_qty,
            last_px,
            leaves_qty: required(leaves, 151)?,
            cum_qty: required(cum, 14)?,
            avg_px: required(avg, 6)?,
            text,
        })
    }
}

// @student w14
impl FixBody for ExecutionReport<'_> {
    const MSG_TYPE: &'static [u8] = msg_type::EXECUTION_REPORT;

    fn write_body(&self, w: &mut FieldWriter<'_>) {
        w.uint(37, self.order_id).bytes(11, self.cl_ord_id);
        if let Some(o) = self.orig_cl_ord_id {
            w.bytes(41, o);
        }
        w.uint(17, self.exec_id)
            .char(150, self.exec_type.code())
            .char(39, self.ord_status.code());
        if let Some(r) = self.ord_rej_reason {
            w.uint(103, r as u64);
        }
        w.bytes(55, self.symbol).char(54, side_code(self.side));
        if let Some(q) = self.order_qty {
            w.uint(38, q);
        }
        if let Some(p) = self.price {
            w.decimal(44, p);
        }
        if let Some(q) = self.last_qty {
            w.uint(32, q);
        }
        if let Some(p) = self.last_px {
            w.decimal(31, p);
        }
        w.uint(151, self.leaves_qty)
            .uint(14, self.cum_qty)
            .decimal(6, self.avg_px);
        if let Some(t) = self.text {
            w.bytes(58, t);
        }
    }
}

// ---------------------------------------------------------------------------
// Session-level messages

/// Logon (35=A).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Logon<'a> {
    /// HeartBtInt (108), seconds.
    pub heart_bt_int: u32,
    /// ResetSeqNumFlag (141) = Y: both sides restart at MsgSeqNum 1.
    pub reset_seq_num: bool,
    /// Username (553).
    pub username: Option<&'a [u8]>,
    /// Password (554).
    pub password: Option<&'a [u8]>,
}

// @student w14
impl<'a> Logon<'a> {
    /// Decodes; EncryptMethod (98) must be 0 (none).
    pub fn decode(msg: &RawMessage<'a>) -> Result<Logon<'a>, FixError> {
        expect_type(msg, msg_type::LOGON)?;
        let (mut encrypt, mut hb, mut reset, mut user, mut pass) = (None, None, false, None, None);
        for f in msg.fields() {
            let f = f?;
            match f.tag {
                98 => encrypt = Some(uint(&f)?),
                108 => {
                    hb = Some(u32::try_from(uint(&f)?).map_err(|_| FixError::InvalidValue(108))?)
                }
                141 => reset = yes_no(&f)?,
                553 => user = Some(f.value),
                554 => pass = Some(f.value),
                _ => {}
            }
        }
        if required(encrypt, 98)? != 0 {
            return Err(FixError::InvalidValue(98));
        }
        Ok(Logon {
            heart_bt_int: required(hb, 108)?,
            reset_seq_num: reset,
            username: user,
            password: pass,
        })
    }
}

// @student w14
impl FixBody for Logon<'_> {
    const MSG_TYPE: &'static [u8] = msg_type::LOGON;

    fn write_body(&self, w: &mut FieldWriter<'_>) {
        w.uint(98, 0).uint(108, self.heart_bt_int as u64);
        if self.reset_seq_num {
            w.flag(141, true);
        }
        if let Some(u) = self.username {
            w.bytes(553, u);
        }
        if let Some(p) = self.password {
            w.bytes(554, p);
        }
    }
}

/// Heartbeat (35=0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Heartbeat<'a> {
    /// TestReqID (112) when answering a TestRequest.
    pub test_req_id: Option<&'a [u8]>,
}

// @student w14
impl<'a> Heartbeat<'a> {
    /// Decodes.
    pub fn decode(msg: &RawMessage<'a>) -> Result<Heartbeat<'a>, FixError> {
        expect_type(msg, msg_type::HEARTBEAT)?;
        let test_req_id = msg.fields().find_map(|f| match f {
            Ok(f) if f.tag == 112 => Some(Ok(f.value)),
            Err(e) => Some(Err(e)),
            _ => None,
        });
        Ok(Heartbeat {
            test_req_id: test_req_id.transpose()?,
        })
    }
}

// @student w14
impl FixBody for Heartbeat<'_> {
    const MSG_TYPE: &'static [u8] = msg_type::HEARTBEAT;

    fn write_body(&self, w: &mut FieldWriter<'_>) {
        if let Some(id) = self.test_req_id {
            w.bytes(112, id);
        }
    }
}

/// TestRequest (35=1): "send me a Heartbeat with this id".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TestRequest<'a> {
    /// TestReqID (112).
    pub test_req_id: &'a [u8],
}

// @student w14
impl<'a> TestRequest<'a> {
    /// Decodes.
    pub fn decode(msg: &RawMessage<'a>) -> Result<TestRequest<'a>, FixError> {
        expect_type(msg, msg_type::TEST_REQUEST)?;
        for f in msg.fields() {
            let f = f?;
            if f.tag == 112 {
                return Ok(TestRequest {
                    test_req_id: f.value,
                });
            }
        }
        Err(FixError::MissingField(112))
    }
}

// @student w14
impl FixBody for TestRequest<'_> {
    const MSG_TYPE: &'static [u8] = msg_type::TEST_REQUEST;

    fn write_body(&self, w: &mut FieldWriter<'_>) {
        w.bytes(112, self.test_req_id);
    }
}

/// ResendRequest (35=2): "resend MsgSeqNum `begin..=end`" (`end` 0 = all).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResendRequest {
    /// BeginSeqNo (7).
    pub begin_seq_no: u64,
    /// EndSeqNo (16); 0 means "up to the latest".
    pub end_seq_no: u64,
}

// @student w14
impl ResendRequest {
    /// Decodes.
    pub fn decode(msg: &RawMessage<'_>) -> Result<ResendRequest, FixError> {
        expect_type(msg, msg_type::RESEND_REQUEST)?;
        let (mut begin, mut end) = (None, None);
        for f in msg.fields() {
            let f = f?;
            match f.tag {
                7 => begin = Some(uint(&f)?),
                16 => end = Some(uint(&f)?),
                _ => {}
            }
        }
        Ok(ResendRequest {
            begin_seq_no: required(begin, 7)?,
            end_seq_no: required(end, 16)?,
        })
    }
}

// @student w14
impl FixBody for ResendRequest {
    const MSG_TYPE: &'static [u8] = msg_type::RESEND_REQUEST;

    fn write_body(&self, w: &mut FieldWriter<'_>) {
        w.uint(7, self.begin_seq_no).uint(16, self.end_seq_no);
    }
}

/// SequenceReset (35=4). Gap-fill mode skips over messages that will not be
/// resent; reset mode forces the receiver's expected MsgSeqNum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SequenceReset {
    /// NewSeqNo (36): the next MsgSeqNum to expect.
    pub new_seq_no: u64,
    /// GapFillFlag (123) = Y.
    pub gap_fill: bool,
}

// @student w14
impl SequenceReset {
    /// Decodes.
    pub fn decode(msg: &RawMessage<'_>) -> Result<SequenceReset, FixError> {
        expect_type(msg, msg_type::SEQUENCE_RESET)?;
        let (mut new_seq, mut gap_fill) = (None, false);
        for f in msg.fields() {
            let f = f?;
            match f.tag {
                36 => new_seq = Some(uint(&f)?),
                123 => gap_fill = yes_no(&f)?,
                _ => {}
            }
        }
        Ok(SequenceReset {
            new_seq_no: required(new_seq, 36)?,
            gap_fill,
        })
    }
}

// @student w14
impl FixBody for SequenceReset {
    const MSG_TYPE: &'static [u8] = msg_type::SEQUENCE_RESET;

    fn write_body(&self, w: &mut FieldWriter<'_>) {
        if self.gap_fill {
            w.flag(123, true);
        }
        w.uint(36, self.new_seq_no);
    }
}

/// Logout (35=5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Logout<'a> {
    /// Text (58): why.
    pub text: Option<&'a [u8]>,
}

// @student w14
impl<'a> Logout<'a> {
    /// Decodes.
    pub fn decode(msg: &RawMessage<'a>) -> Result<Logout<'a>, FixError> {
        expect_type(msg, msg_type::LOGOUT)?;
        let mut text = None;
        for f in msg.fields() {
            let f = f?;
            if f.tag == 58 {
                text = Some(f.value);
            }
        }
        Ok(Logout { text })
    }
}

// @student w14
impl FixBody for Logout<'_> {
    const MSG_TYPE: &'static [u8] = msg_type::LOGOUT;

    fn write_body(&self, w: &mut FieldWriter<'_>) {
        if let Some(t) = self.text {
            w.bytes(58, t);
        }
    }
}
