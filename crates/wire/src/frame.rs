//! Week 11: length-prefixed framing over a byte stream.
//!
//! TCP delivers bytes, not messages: one `read` may return half a message,
//! or three and a half. A frame is a 2-byte little-endian payload length
//! followed by the payload (one message):
//!
//! | offset | size | field                                   |
//! |-------:|-----:|-----------------------------------------|
//! | 0      | 2    | `len`: payload length, u16 LE, `1..=max` |
//! | 2      | len  | payload (one message, tag first)         |
//!
//! [`FrameDecoder`] turns arbitrary chunks back into frames. Frames that lie
//! entirely inside the chunk just read are returned as slices *of that
//! chunk* — no copy. Only a frame split across two reads is copied, into a
//! buffer allocated once when the decoder is created.
//!
//! ```
//! use wire::{encode_frame, decode, FrameDecoder, Message};
//! use types::{Command, OrderId};
//!
//! let mut stream = [0u8; 64];
//! let mut n = 0;
//! for id in 1..=3 {
//!     n += encode_frame(&Message::Command(Command::Cancel { id: OrderId(id) }), &mut stream[n..]);
//! }
//!
//! let mut dec = FrameDecoder::default();
//! let mut seen = Vec::new();
//! for chunk in stream[..n].chunks(5) {            // bytes trickle in 5 at a time
//!     let mut input = chunk;
//!     while let Some(frame) = dec.next_frame(&mut input).unwrap() {
//!         seen.push(decode(frame).unwrap());
//!     }
//! }
//! assert_eq!(seen.len(), 3);
//! ```

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
#[allow(unused_imports)] // encode_into: used by the implementation the learner writes
use crate::codec::{encode_into, Message};
use std::fmt;

/// Bytes in the frame header (the `u16` length).
pub const FRAME_HEADER_LEN: usize = 2;

/// Default maximum payload length accepted by [`FrameDecoder::default`].
/// Anything longer is treated as a corrupt stream.
pub const MAX_FRAME_LEN: usize = 1024;

/// A frame header that cannot be valid. The byte stream is out of sync
/// after this; the connection should be closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// A zero-length frame (every message has at least its tag byte).
    ZeroLength,
    /// The header announces more than the decoder's maximum.
    TooLong {
        /// Announced payload length.
        len: usize,
        /// The decoder's limit.
        max: usize,
    },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::ZeroLength => write!(f, "zero-length frame"),
            FrameError::TooLong { len, max } => {
                write!(f, "frame of {len} bytes exceeds the {max}-byte limit")
            }
        }
    }
}

impl std::error::Error for FrameError {}

/// Writes `msg` as one frame (`len` + payload) at the start of `buf` and
/// returns the bytes written (`2 + msg.wire_len()`). O(1), no allocation.
/// Panics if `buf` is too small (`2 + MAX_MSG_LEN` always suffices).
#[allow(unused_variables)]
pub fn encode_frame(msg: &Message, buf: &mut [u8]) -> usize {
    todo!("w11: encode_frame")
}

/// Writes an already-encoded payload (e.g. a week-35 schema message) as one
/// frame and returns the bytes written. O(len). Panics if `payload` is
/// empty, longer than `u16::MAX`, or does not fit in `buf`.
#[allow(unused_variables)]
pub fn write_frame(payload: &[u8], buf: &mut [u8]) -> usize {
    todo!("w11: write_frame")
}

/// Incremental frame decoder: feed it whatever each `read` returned and
/// pull complete frames out.
#[derive(Debug)]
pub struct FrameDecoder {
    // TODO(w11): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl FrameDecoder {
    /// A decoder accepting payloads of `1..=max_len` bytes. Allocates its
    /// `2 + max_len`-byte carry buffer here, once. Panics if `max_len` is 0
    /// or above `u16::MAX`.
    pub fn new(max_len: usize) -> FrameDecoder {
        todo!("w11: new")
    }

    /// Returns the next complete frame's payload, consuming bytes from the
    /// front of `input`; `Ok(None)` once `input` is used up without
    /// completing a frame (the partial frame is kept for the next call).
    ///
    /// Call it in a loop until it returns `None`, then read more bytes:
    ///
    /// ```text
    /// let mut input = &read_buf[..n];
    /// while let Some(frame) = dec.next_frame(&mut input)? { handle(frame) }
    /// ```
    ///
    /// A frame entirely inside `input` is returned as a slice of `input`
    /// (zero copy); a frame completed from bytes of several reads is
    /// returned from the decoder's own buffer. Either way the slice is valid
    /// until the next call. O(frame length) only when copying, O(1)
    /// otherwise; never allocates.
    pub fn next_frame<'s, 'i: 's>(
        &'s mut self,
        input: &mut &'i [u8],
    ) -> Result<Option<&'s [u8]>, FrameError> {
        todo!("w11: next_frame")
    }

    /// Bytes of an incomplete frame held from earlier reads (0 when the
    /// stream is at a frame boundary).
    pub fn buffered(&self) -> usize {
        todo!("w11: buffered")
    }

    /// The maximum payload length this decoder accepts.
    pub fn max_len(&self) -> usize {
        todo!("w11: max_len")
    }
}

// TODO(w11): implement — see the week guide on tepy.dev/hft.

impl Default for FrameDecoder {
    /// A decoder accepting payloads up to [`MAX_FRAME_LEN`].
    fn default() -> FrameDecoder {
        FrameDecoder::new(MAX_FRAME_LEN)
    }
}
