//! Binary wire protocol: fixed-layout codecs, zero-copy views, framing,
//! schema-driven codecs.
//!
//! Curriculum: Weeks 11, 35 (see <https://tepy.dev/hft>).
//!
//! * **Week 11** — [`codec`], [`view`], [`frame`]: a hand-written,
//!   ITCH/OUCH-inspired protocol. Every message has a fixed length decided
//!   by its one-byte type tag; integers are little-endian at fixed offsets;
//!   a 2-byte length prefix frames messages on a byte stream. Decoding
//!   allocates nothing and copies nothing it does not have to.
//! * **Week 35** — [`schema`]: the same kind of messages generated from a
//!   schema by `macro_rules!`, with a header carrying a template id, a
//!   version and a block length, so v2 can append fields without breaking v1
//!   decoders.
//!
//! The gateway (week 13) uses [`FrameDecoder`] on each TCP connection,
//! [`InboundView::parse`] on each frame, and [`encode_frame`] for replies.
//!
//! # Week 11 protocol
//!
//! ## Framing
//!
//! | offset | size | field                                     |
//! |-------:|-----:|-------------------------------------------|
//! | 0      | 2    | `len`: payload length, u16 LE, `1..=max`  |
//! | 2      | len  | payload: one message                      |
//!
//! ## Conventions
//!
//! * All integers little-endian. Prices are `i64` ticks, quantities `u64`
//!   lots, order ids `u64`, accounts and instruments `u32`.
//! * Byte 0 is the type tag ([`MsgType`]); the tag fixes the length.
//! * `res` bytes are reserved: encoders write zero, decoders ignore them.
//! * Decoders ignore bytes past the fixed length (room to grow), and reject
//!   out-of-domain enumerated values.
//! * Fields are laid out so every `u64` sits at a multiple of 8 and every
//!   `u32` at a multiple of 4 from the start of the message.
//!
//! | code               | values                                           |
//! |--------------------|--------------------------------------------------|
//! | side               | `B` (0x42) buy, `S` (0x53) sell                   |
//! | ord type           | `L` limit, `M` market                             |
//! | time in force      | `G` GTC, `I` IOC, `F` FOK                         |
//! | flags              | bit 0 post-only (limit only); other bits must be 0 |
//! | reject reason      | 1 DuplicateOrderId, 2 UnknownOrder, 3 ZeroQuantity, 4 InvalidTick, 5 InvalidLot, 6 PriceBand, 7 WouldTake, 8 FokUnfillable, 9 NoLiquidity, 10 InvalidReduce, 11 Halted, 32 Risk/MaxOrderQty, 33 Risk/MaxNotional, 34 Risk/PositionLimit, 35 Risk/RateLimit, 36 Risk/KillSwitch |
//!
//! ## Client → exchange
//!
//! **Logon** `L`, 16 bytes
//!
//! | off | size | field |
//! |----:|-----:|-------|
//! | 0 | 1 | tag `L` |
//! | 1 | 1 | protocol version (1) |
//! | 2 | 2 | res |
//! | 4 | 4 | account u32 |
//! | 8 | 8 | token u64 |
//!
//! **NewOrder** `N`, 40 bytes → [`types::Command::New`]
//!
//! | off | size | field |
//! |----:|-----:|-------|
//! | 0 | 1 | tag `N` |
//! | 1 | 1 | side |
//! | 2 | 1 | ord type |
//! | 3 | 1 | time in force |
//! | 4 | 1 | flags |
//! | 5 | 3 | res |
//! | 8 | 8 | order id u64 |
//! | 16 | 4 | account u32 |
//! | 20 | 4 | instrument u32 |
//! | 24 | 8 | qty u64 |
//! | 32 | 8 | price i64 (market: 0) |
//!
//! **Cancel** `X`, 16 bytes → [`types::Command::Cancel`]
//!
//! | off | size | field |
//! |----:|-----:|-------|
//! | 0 | 1 | tag `X` |
//! | 1 | 7 | res |
//! | 8 | 8 | order id u64 |
//!
//! **Reduce** `R`, 24 bytes → [`types::Command::Reduce`]
//!
//! | off | size | field |
//! |----:|-----:|-------|
//! | 0 | 1 | tag `R` |
//! | 1 | 7 | res |
//! | 8 | 8 | order id u64 |
//! | 16 | 8 | new remaining qty u64 |
//!
//! ## Exchange → client
//!
//! **LogonAck** `l`, 16 bytes: same layout as Logon, with the
//! gateway-assigned session id in place of the token.
//!
//! **Accepted** `a`, 16 bytes → [`types::Event::Accepted`]: tag, 7 res,
//! order id u64 @8.
//!
//! **Rejected** `j`, 16 bytes → [`types::Event::Rejected`]
//!
//! | off | size | field |
//! |----:|-----:|-------|
//! | 0 | 1 | tag `j` |
//! | 1 | 1 | reject reason code |
//! | 2 | 6 | res |
//! | 8 | 8 | order id u64 |
//!
//! **Trade** `t`, 40 bytes → [`types::Event::Trade`]
//!
//! | off | size | field |
//! |----:|-----:|-------|
//! | 0 | 1 | tag `t` |
//! | 1 | 1 | taker side |
//! | 2 | 6 | res |
//! | 8 | 8 | taker order id u64 |
//! | 16 | 8 | maker order id u64 |
//! | 24 | 8 | price i64 |
//! | 32 | 8 | qty u64 |
//!
//! **Rested** `r`, 32 bytes → [`types::Event::Rested`]
//!
//! | off | size | field |
//! |----:|-----:|-------|
//! | 0 | 1 | tag `r` |
//! | 1 | 1 | side |
//! | 2 | 6 | res |
//! | 8 | 8 | order id u64 |
//! | 16 | 8 | price i64 |
//! | 24 | 8 | qty u64 |
//!
//! **Cancelled** `c` / **Expired** `e` / **Reduced** `d`, 24 bytes →
//! [`types::Event::Cancelled`] / [`Expired`](types::Event::Expired) /
//! [`Reduced`](types::Event::Reduced)
//!
//! | off | size | field |
//! |----:|-----:|-------|
//! | 0 | 1 | tag |
//! | 1 | 7 | res |
//! | 8 | 8 | order id u64 |
//! | 16 | 8 | remaining qty u64 |
//!
//! ## Market data
//!
//! **AddOrder** `A` / **OrderExecuted** `E`, 32 bytes
//!
//! | off | size | field |
//! |----:|-----:|-------|
//! | 0 | 1 | tag |
//! | 1 | 1 | side (`A`); res (`E`) |
//! | 2 | 2 | res |
//! | 4 | 4 | instrument u32 |
//! | 8 | 8 | order id u64 (the resting order) |
//! | 16 | 8 | price i64 (limit / execution price) |
//! | 24 | 8 | qty u64 (displayed / executed) |
//!
//! **OrderDelete** `D`, 16 bytes: tag, 3 res, instrument u32 @4, order id
//! u64 @8.
//!
//! **L2Update** `P`, 24 bytes
//!
//! | off | size | field |
//! |----:|-----:|-------|
//! | 0 | 1 | tag `P` |
//! | 1 | 1 | side |
//! | 2 | 2 | res |
//! | 4 | 4 | instrument u32 |
//! | 8 | 8 | price i64 |
//! | 16 | 8 | total qty at level u64 (0 = level removed) |
//!
//! # Week 35 protocol
//!
//! See [`schema`] for the header, the versioning rules and the generated
//! layouts.

#![warn(missing_docs)]

pub mod codec;
pub mod frame;
pub mod schema;
pub mod view;

pub use codec::{
    decode, encode_into, DecodeError, Logon, LogonAck, MarketData, Message, MsgType, MAX_MSG_LEN,
    PROTOCOL_VERSION,
};
pub use frame::{
    encode_frame, write_frame, FrameDecoder, FrameError, FRAME_HEADER_LEN, MAX_FRAME_LEN,
};
pub use view::{CancelView, InboundView, LogonView, NewOrderView, ReduceView};
