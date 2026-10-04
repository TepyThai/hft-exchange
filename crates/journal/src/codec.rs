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

// @student-begin w09
use types::{AccountId, InstrumentId, NewOrder, OrderId, OrderKind, Price, Qty, Side, TimeInForce};

#[inline]
fn put<const N: usize>(out: &mut [u8], at: usize, bytes: [u8; N]) {
    out[at..at + N].copy_from_slice(&bytes);
}

#[inline]
fn get<const N: usize>(b: &[u8], at: usize) -> [u8; N] {
    let mut a = [0u8; N];
    a.copy_from_slice(&b[at..at + N]);
    a
}

fn expect_len(b: &[u8], expected: usize) -> Result<(), DecodeError> {
    if b.len() == expected {
        Ok(())
    } else {
        Err(DecodeError::BadLength {
            tag: b[0],
            expected,
            actual: b.len(),
        })
    }
}
// @student-end

/// Writes the encoding of `cmd` (layout in the [module docs](self)) to the
/// front of `out` and returns its length. O(1), no allocation; bytes of
/// `out` past the returned length are left untouched.
// @student w09
pub fn encode_command(cmd: &Command, out: &mut [u8; MAX_COMMAND_LEN]) -> usize {
    match *cmd {
        Command::New(o) => {
            let mut flags = 0u8;
            if o.side == Side::Sell {
                flags |= FLAG_SELL;
            }
            let price = match o.kind {
                OrderKind::Limit { price, post_only } => {
                    if post_only {
                        flags |= FLAG_POST_ONLY;
                    }
                    price.0
                }
                OrderKind::Market => {
                    flags |= FLAG_MARKET;
                    0
                }
            };
            let tif = match o.tif {
                TimeInForce::Gtc => 0u8,
                TimeInForce::Ioc => 1,
                TimeInForce::Fok => 2,
            };
            flags |= tif << TIF_SHIFT;
            out[0] = TAG_NEW;
            out[1] = flags;
            put(out, 2, o.id.0.to_le_bytes());
            put(out, 10, o.account.0.to_le_bytes());
            put(out, 14, o.instrument.0.to_le_bytes());
            put(out, 18, o.qty.0.to_le_bytes());
            put(out, 26, price.to_le_bytes());
            NEW_ORDER_LEN
        }
        Command::Cancel { id } => {
            out[0] = TAG_CANCEL;
            put(out, 1, id.0.to_le_bytes());
            CANCEL_LEN
        }
        Command::Reduce { id, qty } => {
            out[0] = TAG_REDUCE;
            put(out, 1, id.0.to_le_bytes());
            put(out, 9, qty.0.to_le_bytes());
            REDUCE_LEN
        }
    }
}

/// Decodes one command; `bytes` must be exactly one encoded command (the
/// journal frame supplies the length). O(1), no allocation.
// @student w09
pub fn decode_command(bytes: &[u8]) -> Result<Command, DecodeError> {
    let Some(&tag) = bytes.first() else {
        return Err(DecodeError::Empty);
    };
    match tag {
        TAG_NEW => {
            expect_len(bytes, NEW_ORDER_LEN)?;
            let flags = bytes[1];
            let tif = match (flags >> TIF_SHIFT) & 0b11 {
                0 => TimeInForce::Gtc,
                1 => TimeInForce::Ioc,
                2 => TimeInForce::Fok,
                _ => return Err(DecodeError::BadFlags(flags)),
            };
            if flags & RESERVED_FLAGS != 0 {
                return Err(DecodeError::BadFlags(flags));
            }
            let price = i64::from_le_bytes(get(bytes, 26));
            let post_only = flags & FLAG_POST_ONLY != 0;
            let kind = if flags & FLAG_MARKET != 0 {
                if post_only {
                    return Err(DecodeError::BadFlags(flags));
                }
                if price != 0 {
                    return Err(DecodeError::MarketWithPrice(price));
                }
                OrderKind::Market
            } else {
                OrderKind::Limit {
                    price: Price(price),
                    post_only,
                }
            };
            Ok(Command::New(NewOrder {
                id: OrderId(u64::from_le_bytes(get(bytes, 2))),
                account: AccountId(u32::from_le_bytes(get(bytes, 10))),
                instrument: InstrumentId(u32::from_le_bytes(get(bytes, 14))),
                side: if flags & FLAG_SELL != 0 {
                    Side::Sell
                } else {
                    Side::Buy
                },
                qty: Qty(u64::from_le_bytes(get(bytes, 18))),
                kind,
                tif,
            }))
        }
        TAG_CANCEL => {
            expect_len(bytes, CANCEL_LEN)?;
            Ok(Command::Cancel {
                id: OrderId(u64::from_le_bytes(get(bytes, 1))),
            })
        }
        TAG_REDUCE => {
            expect_len(bytes, REDUCE_LEN)?;
            Ok(Command::Reduce {
                id: OrderId(u64::from_le_bytes(get(bytes, 1))),
                qty: Qty(u64::from_le_bytes(get(bytes, 9))),
            })
        }
        other => Err(DecodeError::UnknownTag(other)),
    }
}
