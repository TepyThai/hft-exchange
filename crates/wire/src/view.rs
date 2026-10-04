//! Week 11: zero-copy views of the client→exchange messages.
//!
//! A view borrows the bytes it was built from (`&'a [u8]`, usually a frame
//! straight out of the socket buffer) and reads each field at its fixed
//! offset when asked. Building a view checks the type tag, the length and
//! every enumerated field once, so the accessors are infallible and cost one
//! load each — nothing is copied or allocated.
//!
//! ```
//! use types::{Command, NewOrder, Side};
//! use wire::{encode_into, InboundView, Message, MAX_MSG_LEN};
//!
//! let order = NewOrder::limit(7, Side::Buy, 10_050, 25);
//! let mut buf = [0u8; MAX_MSG_LEN];
//! let n = encode_into(&Message::Command(Command::New(order)), &mut buf);
//!
//! match InboundView::parse(&buf[..n]).unwrap() {
//!     InboundView::NewOrder(v) => {
//!         assert_eq!(v.qty().0, 25); // read in place, nothing decoded yet
//!         assert_eq!(v.to_new_order(), order);
//!     }
//!     other => panic!("unexpected {other:?}"),
//! }
//! ```

#[allow(unused_imports)] // used by the implementation the learner writes
use crate::codec::{
    read_i64, read_u32, read_u64, side_from_byte, tif_from_byte, DecodeError, Logon, MsgType,
    FLAG_POST_ONLY, ORD_LIMIT, ORD_MARKET, PROTOCOL_VERSION,
};
#[allow(unused_imports)]
use types::{
    AccountId, Command, InstrumentId, NewOrder, OrderId, OrderKind, Price, Qty, Side, TimeInForce,
};

// @student w11
/// Zero-copy view of a `NewOrder` (`N`, 40 bytes).
#[derive(Clone, Copy, Debug)]
pub struct NewOrderView<'a> {
    bytes: &'a [u8; MsgType::NewOrder.wire_len()],
}

// @student w11
impl<'a> NewOrderView<'a> {
    /// Checks tag, length (≥ 40; extra bytes are ignored), side, order type,
    /// time in force and flags. A market order must carry price 0 and no
    /// flags. O(1), no allocation.
    #[inline]
    pub fn new(buf: &'a [u8]) -> Result<NewOrderView<'a>, DecodeError> {
        use crate::codec::at::new_order::*;
        let bytes = crate::codec::fixed::<40>(buf, MsgType::NewOrder)?;
        let invalid = |field, value: u8| DecodeError::InvalidField {
            field,
            value: value as u64,
        };
        if side_from_byte(bytes[SIDE]).is_none() {
            return Err(invalid("side", bytes[SIDE]));
        }
        if tif_from_byte(bytes[TIF]).is_none() {
            return Err(invalid("tif", bytes[TIF]));
        }
        let flags = bytes[FLAGS];
        match bytes[ORD_TYPE] {
            ORD_LIMIT if flags & !FLAG_POST_ONLY == 0 => {}
            ORD_LIMIT => return Err(invalid("flags", flags)),
            ORD_MARKET if flags != 0 => return Err(invalid("flags", flags)),
            ORD_MARKET if read_i64(bytes, PRICE) != 0 => {
                return Err(DecodeError::InvalidField {
                    field: "price",
                    value: read_i64(bytes, PRICE) as u64,
                })
            }
            ORD_MARKET => {}
            other => return Err(invalid("ord_type", other)),
        }
        Ok(NewOrderView { bytes })
    }

    /// Client-chosen order id.
    #[inline]
    pub fn id(&self) -> OrderId {
        OrderId(read_u64(self.bytes, crate::codec::at::new_order::ID))
    }

    /// Account the order trades for.
    #[inline]
    pub fn account(&self) -> AccountId {
        AccountId(read_u32(self.bytes, crate::codec::at::new_order::ACCOUNT))
    }

    /// Instrument to trade.
    #[inline]
    pub fn instrument(&self) -> InstrumentId {
        InstrumentId(read_u32(
            self.bytes,
            crate::codec::at::new_order::INSTRUMENT,
        ))
    }

    /// Buy or sell.
    #[inline]
    pub fn side(&self) -> Side {
        // Validated in `new`: anything but `B` is `S`.
        if self.bytes[crate::codec::at::new_order::SIDE] == crate::codec::SIDE_BUY {
            Side::Buy
        } else {
            Side::Sell
        }
    }

    /// Time in force.
    #[inline]
    pub fn tif(&self) -> TimeInForce {
        match self.bytes[crate::codec::at::new_order::TIF] {
            crate::codec::TIF_IOC => TimeInForce::Ioc,
            crate::codec::TIF_FOK => TimeInForce::Fok,
            _ => TimeInForce::Gtc,
        }
    }

    /// Quantity in lots.
    #[inline]
    pub fn qty(&self) -> Qty {
        Qty(read_u64(self.bytes, crate::codec::at::new_order::QTY))
    }

    /// Raw price field in ticks (0 for market orders).
    #[inline]
    pub fn price(&self) -> Price {
        Price(read_i64(self.bytes, crate::codec::at::new_order::PRICE))
    }

    /// Limit (with price and post-only flag) or market.
    #[inline]
    pub fn kind(&self) -> OrderKind {
        use crate::codec::at::new_order::*;
        if self.bytes[ORD_TYPE] == ORD_MARKET {
            OrderKind::Market
        } else {
            OrderKind::Limit {
                price: self.price(),
                post_only: self.bytes[FLAGS] & FLAG_POST_ONLY != 0,
            }
        }
    }

    /// The 40 bytes this view reads.
    #[inline]
    pub fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Materializes the order. Always inlined so the fields are built in
    /// registers and stored once into the caller's result: an out-of-line
    /// copy here is written with narrow stores and immediately re-read with
    /// wider loads, which stalls store-to-load forwarding (~20 ns).
    #[inline(always)]
    pub fn to_new_order(&self) -> NewOrder {
        NewOrder {
            id: self.id(),
            account: self.account(),
            instrument: self.instrument(),
            side: self.side(),
            qty: self.qty(),
            kind: self.kind(),
            tif: self.tif(),
        }
    }
}

// @student w11
/// Zero-copy view of a `Cancel` (`X`, 16 bytes).
#[derive(Clone, Copy, Debug)]
pub struct CancelView<'a> {
    bytes: &'a [u8; MsgType::Cancel.wire_len()],
}

// @student w11
impl<'a> CancelView<'a> {
    /// Checks tag and length. O(1).
    #[inline]
    pub fn new(buf: &'a [u8]) -> Result<CancelView<'a>, DecodeError> {
        Ok(CancelView {
            bytes: crate::codec::fixed::<16>(buf, MsgType::Cancel)?,
        })
    }

    /// The order to cancel.
    #[inline]
    pub fn id(&self) -> OrderId {
        OrderId(read_u64(self.bytes, crate::codec::at::order::ID))
    }

    /// `Command::Cancel`.
    #[inline]
    pub fn to_command(&self) -> Command {
        Command::Cancel { id: self.id() }
    }
}

// @student w11
/// Zero-copy view of a `Reduce` (`R`, 24 bytes).
#[derive(Clone, Copy, Debug)]
pub struct ReduceView<'a> {
    bytes: &'a [u8; MsgType::Reduce.wire_len()],
}

// @student w11
impl<'a> ReduceView<'a> {
    /// Checks tag and length. O(1).
    #[inline]
    pub fn new(buf: &'a [u8]) -> Result<ReduceView<'a>, DecodeError> {
        Ok(ReduceView {
            bytes: crate::codec::fixed::<24>(buf, MsgType::Reduce)?,
        })
    }

    /// The order to reduce.
    #[inline]
    pub fn id(&self) -> OrderId {
        OrderId(read_u64(self.bytes, crate::codec::at::order::ID))
    }

    /// The new (lower) remaining quantity.
    #[inline]
    pub fn qty(&self) -> Qty {
        Qty(read_u64(self.bytes, crate::codec::at::order::QTY))
    }

    /// `Command::Reduce`.
    #[inline]
    pub fn to_command(&self) -> Command {
        Command::Reduce {
            id: self.id(),
            qty: self.qty(),
        }
    }
}

// @student w11
/// Zero-copy view of a `Logon` (`L`, 16 bytes).
#[derive(Clone, Copy, Debug)]
pub struct LogonView<'a> {
    bytes: &'a [u8; MsgType::Logon.wire_len()],
}

// @student w11
impl<'a> LogonView<'a> {
    /// Checks tag, length and protocol version. O(1).
    #[inline]
    pub fn new(buf: &'a [u8]) -> Result<LogonView<'a>, DecodeError> {
        let bytes = crate::codec::fixed::<16>(buf, MsgType::Logon)?;
        let version = bytes[crate::codec::at::logon::VERSION];
        if version != PROTOCOL_VERSION {
            return Err(DecodeError::UnsupportedVersion(version));
        }
        Ok(LogonView { bytes })
    }

    /// Account the session trades for.
    #[inline]
    pub fn account(&self) -> AccountId {
        AccountId(read_u32(self.bytes, crate::codec::at::logon::ACCOUNT))
    }

    /// Credential.
    #[inline]
    pub fn token(&self) -> u64 {
        read_u64(self.bytes, crate::codec::at::logon::TOKEN)
    }

    /// Materializes the message.
    #[inline]
    pub fn to_logon(&self) -> Logon {
        Logon {
            account: self.account(),
            token: self.token(),
        }
    }
}

/// A view of any client→exchange message: what a gateway session builds
/// from each inbound frame.
#[derive(Clone, Copy, Debug)]
pub enum InboundView<'a> {
    /// `L`
    Logon(LogonView<'a>),
    /// `N`
    NewOrder(NewOrderView<'a>),
    /// `X`
    Cancel(CancelView<'a>),
    /// `R`
    Reduce(ReduceView<'a>),
}

// @student w11
impl<'a> InboundView<'a> {
    /// Dispatches on the tag byte and builds the matching view. Tags of
    /// exchange→client or market-data messages are `UnknownType` here: a
    /// client may not send them. O(1), no allocation.
    #[inline]
    pub fn parse(frame: &'a [u8]) -> Result<InboundView<'a>, DecodeError> {
        let Some(&tag) = frame.first() else {
            return Err(DecodeError::Truncated { need: 1, have: 0 });
        };
        match MsgType::from_tag(tag) {
            Some(MsgType::NewOrder) => NewOrderView::new(frame).map(InboundView::NewOrder),
            Some(MsgType::Cancel) => CancelView::new(frame).map(InboundView::Cancel),
            Some(MsgType::Reduce) => ReduceView::new(frame).map(InboundView::Reduce),
            Some(MsgType::Logon) => LogonView::new(frame).map(InboundView::Logon),
            _ => Err(DecodeError::UnknownType(tag)),
        }
    }

    /// The engine command this message carries (`None` for `Logon`).
    #[inline]
    pub fn to_command(&self) -> Option<Command> {
        match self {
            InboundView::NewOrder(v) => Some(Command::New(v.to_new_order())),
            InboundView::Cancel(v) => Some(v.to_command()),
            InboundView::Reduce(v) => Some(v.to_command()),
            InboundView::Logon(_) => None,
        }
    }
}
