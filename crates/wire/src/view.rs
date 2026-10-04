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

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
#[allow(unused_imports)] // used by the implementation the learner writes
use crate::codec::{
    read_i64, read_u32, read_u64, side_from_byte, tif_from_byte, DecodeError, Logon, MsgType,
    FLAG_POST_ONLY, ORD_LIMIT, ORD_MARKET, PROTOCOL_VERSION,
};
#[allow(unused_imports)]
use types::{
    AccountId, Command, InstrumentId, NewOrder, OrderId, OrderKind, Price, Qty, Side, TimeInForce,
};

/// Zero-copy view of a `NewOrder` (`N`, 40 bytes).
#[derive(Clone, Copy, Debug)]
pub struct NewOrderView<'a> {
    // TODO(w11): design this type's fields.
    _todo: std::marker::PhantomData<(&'a (),)>,
}

#[allow(unused_variables)]
impl<'a> NewOrderView<'a> {
    /// Checks tag, length (≥ 40; extra bytes are ignored), side, order type,
    /// time in force and flags. A market order must carry price 0 and no
    /// flags. O(1), no allocation.
    #[inline]
    pub fn new(buf: &'a [u8]) -> Result<NewOrderView<'a>, DecodeError> {
        todo!("w11: new")
    }

    /// Client-chosen order id.
    #[inline]
    pub fn id(&self) -> OrderId {
        todo!("w11: id")
    }

    /// Account the order trades for.
    #[inline]
    pub fn account(&self) -> AccountId {
        todo!("w11: account")
    }

    /// Instrument to trade.
    #[inline]
    pub fn instrument(&self) -> InstrumentId {
        todo!("w11: instrument")
    }

    /// Buy or sell.
    #[inline]
    pub fn side(&self) -> Side {
        todo!("w11: side")
    }

    /// Time in force.
    #[inline]
    pub fn tif(&self) -> TimeInForce {
        todo!("w11: tif")
    }

    /// Quantity in lots.
    #[inline]
    pub fn qty(&self) -> Qty {
        todo!("w11: qty")
    }

    /// Raw price field in ticks (0 for market orders).
    #[inline]
    pub fn price(&self) -> Price {
        todo!("w11: price")
    }

    /// Limit (with price and post-only flag) or market.
    #[inline]
    pub fn kind(&self) -> OrderKind {
        todo!("w11: kind")
    }

    /// The 40 bytes this view reads.
    #[inline]
    pub fn as_bytes(&self) -> &'a [u8] {
        todo!("w11: as_bytes")
    }

    /// Materializes the order. Always inlined so the fields are built in
    /// registers and stored once into the caller's result: an out-of-line
    /// copy here is written with narrow stores and immediately re-read with
    /// wider loads, which stalls store-to-load forwarding (~20 ns).
    #[inline(always)]
    pub fn to_new_order(&self) -> NewOrder {
        todo!("w11: to_new_order")
    }
}

/// Zero-copy view of a `Cancel` (`X`, 16 bytes).
#[derive(Clone, Copy, Debug)]
pub struct CancelView<'a> {
    // TODO(w11): design this type's fields.
    _todo: std::marker::PhantomData<(&'a (),)>,
}

#[allow(unused_variables)]
impl<'a> CancelView<'a> {
    /// Checks tag and length. O(1).
    #[inline]
    pub fn new(buf: &'a [u8]) -> Result<CancelView<'a>, DecodeError> {
        todo!("w11: new")
    }

    /// The order to cancel.
    #[inline]
    pub fn id(&self) -> OrderId {
        todo!("w11: id")
    }

    /// `Command::Cancel`.
    #[inline]
    pub fn to_command(&self) -> Command {
        todo!("w11: to_command")
    }
}

/// Zero-copy view of a `Reduce` (`R`, 24 bytes).
#[derive(Clone, Copy, Debug)]
pub struct ReduceView<'a> {
    // TODO(w11): design this type's fields.
    _todo: std::marker::PhantomData<(&'a (),)>,
}

#[allow(unused_variables)]
impl<'a> ReduceView<'a> {
    /// Checks tag and length. O(1).
    #[inline]
    pub fn new(buf: &'a [u8]) -> Result<ReduceView<'a>, DecodeError> {
        todo!("w11: new")
    }

    /// The order to reduce.
    #[inline]
    pub fn id(&self) -> OrderId {
        todo!("w11: id")
    }

    /// The new (lower) remaining quantity.
    #[inline]
    pub fn qty(&self) -> Qty {
        todo!("w11: qty")
    }

    /// `Command::Reduce`.
    #[inline]
    pub fn to_command(&self) -> Command {
        todo!("w11: to_command")
    }
}

/// Zero-copy view of a `Logon` (`L`, 16 bytes).
#[derive(Clone, Copy, Debug)]
pub struct LogonView<'a> {
    // TODO(w11): design this type's fields.
    _todo: std::marker::PhantomData<(&'a (),)>,
}

#[allow(unused_variables)]
impl<'a> LogonView<'a> {
    /// Checks tag, length and protocol version. O(1).
    #[inline]
    pub fn new(buf: &'a [u8]) -> Result<LogonView<'a>, DecodeError> {
        todo!("w11: new")
    }

    /// Account the session trades for.
    #[inline]
    pub fn account(&self) -> AccountId {
        todo!("w11: account")
    }

    /// Credential.
    #[inline]
    pub fn token(&self) -> u64 {
        todo!("w11: token")
    }

    /// Materializes the message.
    #[inline]
    pub fn to_logon(&self) -> Logon {
        todo!("w11: to_logon")
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

#[allow(unused_variables)]
impl<'a> InboundView<'a> {
    /// Dispatches on the tag byte and builds the matching view. Tags of
    /// exchange→client or market-data messages are `UnknownType` here: a
    /// client may not send them. O(1), no allocation.
    #[inline]
    pub fn parse(frame: &'a [u8]) -> Result<InboundView<'a>, DecodeError> {
        todo!("w11: parse")
    }

    /// The engine command this message carries (`None` for `Logon`).
    #[inline]
    pub fn to_command(&self) -> Option<Command> {
        todo!("w11: to_command")
    }
}
