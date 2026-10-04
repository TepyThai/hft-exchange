//! Message generation: `tag=value` writing, BodyLength and CheckSum.
//!
//! The body is written first into a reusable scratch buffer, because
//! BodyLength (`9=`) comes before the body and its digit count is not known
//! until the body is done. Then `8=FIX.4.4|9=<len>|`, the body and
//! `10=<sum>|` are appended to the caller's output buffer. Both buffers are
//! reused across messages, so steady-state encoding does not allocate.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::decimal::Decimal;
use crate::messages::FixBody;
#[allow(unused_imports)] // used by the implementation the learner writes
use crate::parse::{checksum, PREFIX, SOH};
use std::ops::Range;

/// The standard header fields every message carries (besides 8, 9, 35).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StdHeader<'a> {
    /// SenderCompID (49).
    pub sender: &'a [u8],
    /// TargetCompID (56).
    pub target: &'a [u8],
    /// MsgSeqNum (34).
    pub seq_num: u64,
    /// SendingTime (52), e.g. from [`format_utc_timestamp`].
    pub sending_time: &'a [u8],
    /// PossDupFlag (43) = Y; OrigSendingTime (122) is then written with the
    /// same value as SendingTime.
    pub poss_dup: bool,
}

/// Formats `v` in decimal into `buf` and returns the digits. O(digits).
#[inline]
pub fn format_uint(mut v: u64, buf: &mut [u8; 20]) -> &[u8] {
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    &buf[i..]
}

/// Appends the decimal digits of `v`. O(digits).
#[inline]
pub fn write_uint(out: &mut Vec<u8>, v: u64) {
    let mut buf = [0u8; 20];
    out.extend_from_slice(format_uint(v, &mut buf));
}

/// Formats nanoseconds since the Unix epoch as a FIX UTCTimestamp with
/// milliseconds, `YYYYMMDD-HH:MM:SS.sss` (21 bytes). O(1), no allocation.
pub fn format_utc_timestamp(nanos: u64, out: &mut [u8; 21]) {
    let secs = nanos / 1_000_000_000;
    let millis = (nanos / 1_000_000) % 1000;
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let sod = secs % 86_400;
    let (h, m, s) = (sod / 3600, (sod / 60) % 60, sod % 60);
    let put = |out: &mut [u8; 21], at: usize, width: usize, mut v: u64| {
        for i in (0..width).rev() {
            out[at + i] = b'0' + (v % 10) as u8;
            v /= 10;
        }
    };
    put(out, 0, 4, year as u64);
    put(out, 4, 2, month as u64);
    put(out, 6, 2, day as u64);
    out[8] = b'-';
    put(out, 9, 2, h);
    out[11] = b':';
    put(out, 12, 2, m);
    out[14] = b':';
    put(out, 15, 2, s);
    out[17] = b'.';
    put(out, 18, 3, millis);
}

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian
/// (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Appends `tag=value|` fields to a message body.
pub struct FieldWriter<'b> {
    // TODO(w14): design this type's fields.
    _todo: std::marker::PhantomData<(&'b (),)>,
}

#[allow(unused_variables)]
impl<'b> FieldWriter<'b> {
    /// A writer appending to `buf`.
    #[inline]
    pub fn new(buf: &'b mut Vec<u8>) -> FieldWriter<'b> {
        todo!("w14: new")
    }

    /// Writes `tag=value|`. `value` must be non-empty and contain no SOH.
    #[inline]
    pub fn bytes(&mut self, tag: u32, value: &[u8]) -> &mut Self {
        todo!("w14: bytes")
    }

    /// Writes an unsigned integer field.
    #[inline]
    pub fn uint(&mut self, tag: u32, v: u64) -> &mut Self {
        todo!("w14: uint")
    }

    /// Writes a single-character field (`54=1`).
    #[inline]
    pub fn char(&mut self, tag: u32, c: u8) -> &mut Self {
        todo!("w14: char")
    }

    /// Writes a Boolean field as `Y` / `N`.
    #[inline]
    pub fn flag(&mut self, tag: u32, v: bool) -> &mut Self {
        todo!("w14: flag")
    }

    /// Writes a price / quantity field from an exact decimal.
    #[inline]
    pub fn decimal(&mut self, tag: u32, d: Decimal) -> &mut Self {
        todo!("w14: decimal")
    }
}

// TODO(w14): implement — see the week guide on tepy.dev/hft.

/// Builds complete messages: header, body, BodyLength, CheckSum. Holds a
/// reusable scratch buffer for the body.
#[derive(Debug, Default)]
pub struct Encoder {
    // TODO(w14): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl Encoder {
    /// An encoder with a 512-byte body buffer (grows if ever needed).
    pub fn new() -> Encoder {
        todo!("w14: new")
    }

    /// Appends one complete message — `8`, `9`, `35`, the standard header,
    /// `msg`'s body fields, `10` — to `out` and returns its byte range in
    /// `out`. O(message length); no allocation once the buffers are warm.
    pub fn encode<M: FixBody>(
        &mut self,
        header: &StdHeader<'_>,
        msg: &M,
        out: &mut Vec<u8>,
    ) -> Range<usize> {
        todo!("w14: encode")
    }

    /// Like [`Encoder::encode`] with the body written by a closure, for
    /// messages without a typed struct.
    pub fn encode_with(
        &mut self,
        msg_type: &[u8],
        header: &StdHeader<'_>,
        out: &mut Vec<u8>,
        body: impl FnOnce(&mut FieldWriter<'_>),
    ) -> Range<usize> {
        todo!("w14: encode_with")
    }
}
