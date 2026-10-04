//! Compact binary encoding of [`Command`] — the journal's payload format.
//!
//! All integers are little-endian and unaligned; there is no padding. The
//! first byte is a tag that fixes the payload length, so a decoder knows
//! exactly how many bytes to expect and can reject anything else.
//!
//! ```text
//! Command::New — NEW_ORDER_LEN = 34 bytes
//!   off  size  field
//!     0     1  tag = TAG_NEW (0x01)
//!     1     1  flags   bit 0     side       0 = Buy, 1 = Sell      (FLAG_SELL)
//!                      bit 1     kind       0 = Limit, 1 = Market  (FLAG_MARKET)
//!                      bit 2     post_only  Limit only             (FLAG_POST_ONLY)
//!                      bits 3-4  tif        0 = Gtc, 1 = Ioc, 2 = Fok
//!                      bits 5-7  reserved, must be zero
//!     2     8  id          u64
//!    10     4  account     u32
//!    14     4  instrument  u32
//!    18     8  qty         u64
//!    26     8  price       i64   (Limit price; must be 0 for Market)
//!
//! Command::Cancel — CANCEL_LEN = 9 bytes
//!     0     1  tag = TAG_CANCEL (0x02)
//!     1     8  id   u64
//!
//! Command::Reduce — REDUCE_LEN = 17 bytes
//!     0     1  tag = TAG_REDUCE (0x03)
//!     1     8  id   u64
//!     9     8  qty  u64   (new remaining quantity)
//! ```
//!
//! Decoding is strict — wrong length, unknown tag, reserved bits set, a
//! market order with a price or a post-only flag — all are errors, never
//! silently "fixed". `decode(encode(c)) == c` for every command, and every
//! byte string that decodes re-encodes to itself.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use std::fmt;
use types::Command;

/// Tag byte of [`Command::New`].
pub const TAG_NEW: u8 = 0x01;
/// Tag byte of [`Command::Cancel`].
pub const TAG_CANCEL: u8 = 0x02;
/// Tag byte of [`Command::Reduce`].
pub const TAG_REDUCE: u8 = 0x03;

/// Encoded size of [`Command::New`].
pub const NEW_ORDER_LEN: usize = 34;
/// Encoded size of [`Command::Cancel`].
pub const CANCEL_LEN: usize = 9;
/// Encoded size of [`Command::Reduce`].
pub const REDUCE_LEN: usize = 17;
/// Largest encoded command; [`encode_command`] writes into a buffer of
/// this size.
pub const MAX_COMMAND_LEN: usize = NEW_ORDER_LEN;

/// New-order flag: the order sells (clear = buys).
pub const FLAG_SELL: u8 = 1 << 0;
/// New-order flag: market order (clear = limit).
pub const FLAG_MARKET: u8 = 1 << 1;
/// New-order flag: post-only limit order.
pub const FLAG_POST_ONLY: u8 = 1 << 2;
/// Bit offset of the two-bit time-in-force field in the flags byte.
pub const TIF_SHIFT: u8 = 3;
/// Flags bits that must be zero.
pub const RESERVED_FLAGS: u8 = 0b1110_0000;

/// Why a payload is not a valid command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DecodeError {
    /// Zero-length payload.
    Empty,
    /// First byte is not a known tag.
    UnknownTag(u8),
    /// Payload length does not match the tag's fixed length.
    BadLength {
        tag: u8,
        expected: usize,
        actual: usize,
    },
    /// Reserved flag bits set, time-in-force `3`, or post-only on a market
    /// order.
    BadFlags(u8),
    /// Market order with a non-zero price field.
    MarketWithPrice(i64),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            DecodeError::Empty => write!(f, "empty command payload"),
            DecodeError::UnknownTag(t) => write!(f, "unknown command tag {t:#04x}"),
            DecodeError::BadLength {
                tag,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "command tag {tag:#04x} needs {expected} bytes, got {actual}"
                )
            }
            DecodeError::BadFlags(b) => write!(f, "invalid new-order flags {b:#010b}"),
            DecodeError::MarketWithPrice(p) => write!(f, "market order carries price {p}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Encoded size of `cmd` in bytes (one of [`NEW_ORDER_LEN`],
/// [`CANCEL_LEN`], [`REDUCE_LEN`]). O(1).
pub fn encoded_len(cmd: &Command) -> usize {
    match cmd {
        Command::New(_) => NEW_ORDER_LEN,
        Command::Cancel { .. } => CANCEL_LEN,
        Command::Reduce { .. } => REDUCE_LEN,
    }
}

// TODO(w09): implement — see the week guide on tepy.dev/hft.

/// Writes the encoding of `cmd` (layout in the [module docs](self)) to the
/// front of `out` and returns its length. O(1), no allocation; bytes of
/// `out` past the returned length are left untouched.
#[allow(unused_variables)]
pub fn encode_command(cmd: &Command, out: &mut [u8; MAX_COMMAND_LEN]) -> usize {
    todo!("w09: encode_command")
}

/// Decodes one command; `bytes` must be exactly one encoded command (the
/// journal frame supplies the length). O(1), no allocation.
#[allow(unused_variables)]
pub fn decode_command(bytes: &[u8]) -> Result<Command, DecodeError> {
    todo!("w09: decode_command")
}
