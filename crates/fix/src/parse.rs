//! Tag=value parsing: the field iterator, BodyLength / CheckSum validation,
//! stream framing, and the standard header.
//!
//! A FIX message on the wire (`|` stands for SOH, `0x01`):
//!
//! ```text
//! 8=FIX.4.4|9=63|35=0|49=EXCH|56=CLIENT1|34=12|52=20261004-12:00:00.000|10=210|
//! └─ begin ─┘└len┘└────────────── body: 63 bytes ───────────────────┘└trailer┘
//! ```
//!
//! * `9=` BodyLength counts the bytes after its own SOH up to and including
//!   the SOH before `10=`.
//! * `10=` CheckSum is the sum of every byte before `10=`, modulo 256, as
//!   exactly three digits.

use std::fmt;

/// The field delimiter.
pub const SOH: u8 = 0x01;

/// BeginString of every message this crate reads and writes.
pub const BEGIN_STRING: &[u8] = b"FIX.4.4";

/// Largest BodyLength accepted (anything bigger is a corrupt stream).
pub const MAX_BODY_LEN: usize = 65_536;

/// `8=FIX.4.4|9=`: the bytes every message starts with.
pub const PREFIX: &[u8] = b"8=FIX.4.4\x019=";

/// Length of the trailer `10=nnn|`.
pub const TRAILER_LEN: usize = 7;

/// One `tag=value` pair, borrowing the message bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field<'a> {
    /// Numeric tag.
    pub tag: u32,
    /// Raw value bytes (never empty, never containing SOH).
    pub value: &'a [u8],
}

/// Why a message could not be parsed or mapped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FixError {
    /// `tag=value|` syntax error at this byte offset (non-digit tag, missing
    /// `=`, empty value, missing SOH).
    Malformed {
        /// Byte offset in the message.
        offset: usize,
    },
    /// The message does not start with `8=FIX.4.4|9=`.
    BadBeginString,
    /// BodyLength is missing, not a number, or does not match where the
    /// trailer actually is.
    BadBodyLength,
    /// CheckSum mismatch.
    BadChecksum {
        /// Value in the `10=` field.
        declared: u8,
        /// Sum of the bytes.
        computed: u8,
    },
    /// BodyLength above [`MAX_BODY_LEN`].
    TooLarge,
    /// A required field is absent.
    MissingField(u32),
    /// A field's value is out of its domain (bad number, unknown code,
    /// price not on a tick, ...).
    InvalidValue(u32),
    /// A typed decoder was handed a message of another MsgType.
    UnexpectedMsgType,
}

impl fmt::Display for FixError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FixError::Malformed { offset } => write!(f, "malformed tag=value at byte {offset}"),
            FixError::BadBeginString => write!(f, "message does not start with 8=FIX.4.4"),
            FixError::BadBodyLength => write!(f, "BodyLength(9) is missing or wrong"),
            FixError::BadChecksum { declared, computed } => {
                write!(
                    f,
                    "CheckSum(10) is {declared:03}, bytes sum to {computed:03}"
                )
            }
            FixError::TooLarge => write!(f, "BodyLength(9) above {MAX_BODY_LEN}"),
            FixError::MissingField(t) => write!(f, "required tag {t} missing"),
            FixError::InvalidValue(t) => write!(f, "invalid value for tag {t}"),
            FixError::UnexpectedMsgType => write!(f, "unexpected MsgType(35)"),
        }
    }
}

impl std::error::Error for FixError {}

// @student w14
/// FIX CheckSum: the byte sum modulo 256. O(n), vectorizes.
#[inline]
pub fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b))
}

// @student w14
/// Parses an unsigned decimal integer (digits only, no sign, at most 19
/// digits). `None` on anything else, including the empty string. O(n).
#[inline]
pub fn parse_uint(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || bytes.len() > 19 {
        return None;
    }
    let mut v: u64 = 0;
    for &b in bytes {
        let d = b.wrapping_sub(b'0');
        if d > 9 {
            return None;
        }
        v = v * 10 + d as u64;
    }
    Some(v)
}

// @student w14
/// Allocation-free iterator over the `tag=value|` fields of a byte slice.
/// Yields `Err` once on the first syntax error, then stops.
#[derive(Clone, Debug)]
pub struct FieldIter<'a> {
    buf: &'a [u8],
    pos: usize,
    failed: bool,
}

// @student w14
impl<'a> FieldIter<'a> {
    /// Iterates the fields of `buf` from its first byte.
    #[inline]
    pub fn new(buf: &'a [u8]) -> FieldIter<'a> {
        FieldIter {
            buf,
            pos: 0,
            failed: false,
        }
    }

    /// Byte offset of the next field (the length of `buf` when done).
    #[inline]
    pub fn offset(&self) -> usize {
        self.pos
    }
}

// @student w14
impl<'a> Iterator for FieldIter<'a> {
    type Item = Result<Field<'a>, FixError>;

    /// Parses the next field in O(field length), without allocating.
    #[inline]
    fn next(&mut self) -> Option<Result<Field<'a>, FixError>> {
        if self.failed || self.pos >= self.buf.len() {
            return None;
        }
        let start = self.pos;
        let rest = &self.buf[start..];
        // Tag: 1..=9 digits, no leading zero, then '='.
        let mut tag: u32 = 0;
        let mut i = 0;
        loop {
            match rest.get(i) {
                Some(&b'=') if i > 0 => break,
                Some(&b) if b.is_ascii_digit() && i < 9 && !(i == 0 && b == b'0') => {
                    tag = tag * 10 + (b - b'0') as u32;
                    i += 1;
                }
                _ => {
                    self.failed = true;
                    return Some(Err(FixError::Malformed { offset: start + i }));
                }
            }
        }
        let value_start = i + 1;
        match rest[value_start..].iter().position(|&b| b == SOH) {
            Some(n) if n > 0 => {
                self.pos = start + value_start + n + 1;
                Some(Ok(Field {
                    tag,
                    value: &rest[value_start..value_start + n],
                }))
            }
            _ => {
                self.failed = true;
                Some(Err(FixError::Malformed {
                    offset: start + value_start,
                }))
            }
        }
    }
}

// @student w14
/// Stream framing: how many bytes the first message in `buf` occupies.
///
/// `Ok(None)` if `buf` is a valid but incomplete prefix (read more bytes);
/// `Err` if the bytes cannot be the start of a FIX 4.4 message. Reads only
/// the `8=FIX.4.4|9=<len>|` prefix: O(1), no allocation. The checksum is
/// verified later by [`RawMessage::parse`].
pub fn message_len(buf: &[u8]) -> Result<Option<usize>, FixError> {
    let n = PREFIX.len().min(buf.len());
    if buf[..n] != PREFIX[..n] {
        return Err(FixError::BadBeginString);
    }
    let mut body_len: usize = 0;
    let mut i = PREFIX.len();
    loop {
        match buf.get(i) {
            None => return Ok(None),
            Some(&SOH) if i > PREFIX.len() => break,
            Some(&b) if b.is_ascii_digit() && i - PREFIX.len() < 6 => {
                body_len = body_len * 10 + (b - b'0') as usize;
                i += 1;
            }
            Some(_) => return Err(FixError::BadBodyLength),
        }
    }
    if body_len > MAX_BODY_LEN {
        return Err(FixError::TooLarge);
    }
    let total = i + 1 + body_len + TRAILER_LEN;
    Ok((buf.len() >= total).then_some(total))
}

/// A validated message: BeginString, BodyLength and CheckSum checked, the
/// standard header parsed, the body located. Borrows the message bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawMessage<'a> {
    /// MsgType (35), e.g. `b"D"`.
    pub msg_type: &'a [u8],
    /// MsgSeqNum (34).
    pub seq_num: u64,
    /// SenderCompID (49).
    pub sender: &'a [u8],
    /// TargetCompID (56).
    pub target: &'a [u8],
    /// SendingTime (52), unparsed.
    pub sending_time: &'a [u8],
    /// PossDupFlag (43) = Y.
    pub poss_dup: bool,
    /// The whole message, `8=` through the trailer.
    pub bytes: &'a [u8],
    /// The fields after the standard header, up to (not including) `10=`.
    pub body: &'a [u8],
}

/// Tags of the FIX 4.4 standard header (besides 8, 9 and 35).
pub fn is_header_tag(tag: u32) -> bool {
    matches!(
        tag,
        34 | 43
            | 49
            | 50
            | 52
            | 56
            | 57
            | 90
            | 91
            | 97
            | 115
            | 116
            | 122
            | 128
            | 129
            | 142
            | 143
            | 144
            | 145
            | 212
            | 213
            | 347
            | 369
            | 627
            | 628
            | 629
            | 630
    )
}

// @student w14
impl<'a> RawMessage<'a> {
    /// Validates and parses one complete message (exactly the bytes
    /// [`message_len`] reported). Checks BeginString, BodyLength (the
    /// trailer must be exactly where it says), CheckSum, that MsgType is the
    /// first body field, and that 49, 56, 34 and 52 are present.
    ///
    /// One pass over the bytes for the checksum plus one over the header
    /// fields: O(n), no allocation.
    pub fn parse(msg: &'a [u8]) -> Result<RawMessage<'a>, FixError> {
        let total = message_len(msg)?.ok_or(FixError::BadBodyLength)?;
        if total != msg.len() {
            return Err(FixError::BadBodyLength);
        }
        let body_end = total - TRAILER_LEN;
        let trailer = &msg[body_end..];
        if &trailer[..3] != b"10=" || trailer[6] != SOH {
            return Err(FixError::BadBodyLength);
        }
        let declared = match parse_uint(&trailer[3..6]) {
            Some(v) if v <= 255 => v as u8,
            _ => return Err(FixError::InvalidValue(10)),
        };
        let computed = checksum(&msg[..body_end]);
        if declared != computed {
            return Err(FixError::BadChecksum { declared, computed });
        }

        // Body starts after `9=<len>|`.
        let len_end = PREFIX.len()
            + msg[PREFIX.len()..]
                .iter()
                .position(|&b| b == SOH)
                .unwrap_or(0);
        let body_start = len_end + 1;
        let fields = &msg[body_start..body_end];
        let shift = |e: FixError| match e {
            FixError::Malformed { offset } => FixError::Malformed {
                offset: offset + body_start,
            },
            e => e,
        };

        let mut it = FieldIter::new(fields);
        let msg_type = match it.next() {
            Some(Ok(Field { tag: 35, value })) => value,
            Some(Err(e)) => return Err(shift(e)),
            _ => return Err(FixError::MissingField(35)),
        };
        let (mut seq_num, mut sender, mut target, mut sending_time) = (None, None, None, None);
        let mut poss_dup = false;
        let mut header_end = fields.len();
        loop {
            let at = it.offset();
            let Some(field) = it.next() else { break };
            let f = field.map_err(shift)?;
            match f.tag {
                34 => seq_num = Some(parse_uint(f.value).ok_or(FixError::InvalidValue(34))?),
                49 => sender = Some(f.value),
                56 => target = Some(f.value),
                52 => sending_time = Some(f.value),
                43 => {
                    poss_dup = match f.value {
                        b"Y" => true,
                        b"N" => false,
                        _ => return Err(FixError::InvalidValue(43)),
                    }
                }
                t if is_header_tag(t) => {}
                _ => {
                    header_end = at;
                    break;
                }
            }
        }
        Ok(RawMessage {
            msg_type,
            seq_num: seq_num.ok_or(FixError::MissingField(34))?,
            sender: sender.ok_or(FixError::MissingField(49))?,
            target: target.ok_or(FixError::MissingField(56))?,
            sending_time: sending_time.ok_or(FixError::MissingField(52))?,
            poss_dup,
            bytes: msg,
            body: &fields[header_end..],
        })
    }

    /// Iterates the body fields (after the standard header).
    #[inline]
    pub fn fields(&self) -> FieldIter<'a> {
        FieldIter::new(self.body)
    }
}
