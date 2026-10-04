//! Week 35: schema-driven codecs with versioning (SBE-style).
//!
//! Week 11 wrote every offset, encoder and accessor by hand. Here a message
//! is *described* once — an ordered list of typed fields — and
//! [`wire_schema!`](crate::schema#the-schema-macro) generates the owned
//! struct, the encoder, the zero-copy view and the layout constants from
//! that description.
//!
//! ## Why `macro_rules!`
//!
//! Three ways to get from a schema to code were on the table:
//!
//! * **`build.rs` + a text schema** is what SBE and Cap'n Proto do, but the
//!   generated code lives in `OUT_DIR` where you cannot read it next to the
//!   schema, errors point into generated files, and a broken generator
//!   breaks the whole crate's build.
//! * **`const` field tables interpreted at run time** keep everything in
//!   plain Rust, but every access goes through a table lookup and the
//!   struct still has to be written by hand.
//! * **`macro_rules!`** keeps the schema, the generator and the generated
//!   items in one file, in plain Rust syntax; `cargo expand` shows exactly
//!   what was generated, errors point at the schema line, and every offset
//!   becomes a compile-time constant expression, so the generated accessors
//!   are exactly as fast as the hand-written week-11 ones.
//!
//! The macro is the clearest of the three, so it is what we use.
//!
//! ## Wire format
//!
//! Every message starts with a 4-byte header, followed by the *root block*
//! (the fields, packed in schema order, little-endian, no padding):
//!
//! | offset | size | field                                                   |
//! |-------:|-----:|---------------------------------------------------------|
//! | 0      | 1    | `template`: message type id                             |
//! | 1      | 1    | `version`: schema version the sender encoded (`1..`)    |
//! | 2      | 2    | `block_len`: bytes of root block after the header, u16 LE |
//! | 4      | …    | root block: v1 fields, then v2 fields                   |
//!
//! ## Versioning rules
//!
//! * A version only ever **appends** fields to the root block; nothing is
//!   removed, reordered or resized.
//! * Fields added in v2 are **optional**: they are `Option<T>` in Rust.
//!   `None` is `None` either because the message is v1 (the bytes are not
//!   there) or because the sender wrote the type's null value (`u64::MAX`,
//!   `i64::MIN`, ...; see [`OptionalField`]).
//! * Decoders accept **v1 and v2**, and also any later version: the header's
//!   `block_len` says where the message ends, so fields a decoder does not
//!   know are skipped. A message with `version >= 2` must have a block at
//!   least [`V2_BLOCK`](NewOrder::V2_BLOCK) long; `version == 0` is invalid.
//!
//! ## Messages
//!
//! `NewOrder` (template 1):
//!
//! | offset | size | field        | since |
//! |-------:|-----:|--------------|------:|
//! | 4      | 4    | `account` u32 | v1 |
//! | 8      | 8    | `id` u64      | v1 |
//! | 16     | 8    | `qty` u64     | v1 |
//! | 24     | 8    | `price` i64   | v1 |
//! | 32     | 4    | `instrument` u32 | v1 |
//! | 36     | 1    | `side` (`B`/`S`) | v1 |
//! | 37     | 1    | `ord_type` (`L`/`M`/`P`) | v1 |
//! | 38     | 1    | `tif` (`G`/`I`/`F`) | v1 |
//! | 39     | 8    | `client_ts` u64 (null `u64::MAX`) | v2 |
//! | 47     | 8    | `client_tag` u64 (null `u64::MAX`) | v2 |
//!
//! v1 block 35 bytes (message 39), v2 block 51 bytes (message 55).
//!
//! `Cancel` (template 2): `id` u64 @4 (v1); `client_ts` u64 @12 (v2). Blocks 8 / 16.
//!
//! `Reduce` (template 3): `id` u64 @4, `qty` u64 @12 (v1); `client_ts` u64 @20 (v2).
//! Blocks 16 / 24.
//!
//! `Trade` (template 4): `taker` u64 @4, `maker` u64 @12, `price` i64 @20,
//! `qty` u64 @28, `taker_side` @36 (v1); `match_id` u64 @37, `exec_ts` u64
//! @45 (v2). Blocks 33 / 49.
//!
//! ## The schema macro
//!
//! ```text
//! wire_schema! {
//!     /// docs for the struct
//!     message Name = <template id>, view NameView {
//!         v1 { field: Type, ... }      // Type: Field
//!         v2 { field: Type, ... }      // Type: OptionalField
//!     }
//! }
//! ```
//!
//! generates, for each message:
//!
//! * `pub struct Name { v1 fields: T, v2 fields: Option<T> }`
//! * `Name::TEMPLATE`, `V1_BLOCK`, `V2_BLOCK`, `FIELDS` (offset table),
//!   `encoded_len(version)`, `encode_into(&self, version, buf)`,
//!   `decode(buf)`
//! * `pub struct NameView<'a>` with `new(buf)`, `version()`,
//!   `message_len()`, one accessor per field, and `to_msg()`.
//!
//! Field names must not be `new`, `version`, `message_len` or `to_msg`.
//!
//! ## Learner template
//!
//! The template keeps the schema invocations and the public contract but
//! not the generator: a stub `wire_schema!` (defined first in this file)
//! expands to the same items with `todo!()` bodies. The real macro, written
//! after it, shadows it — `macro_rules!` names are scoped textually, so the
//! last definition before an invocation wins.

#[allow(unused_imports)] // used by the generator the learner writes
use crate::codec::{
    read_i64, read_u16, read_u32, read_u64, side_byte, side_from_byte, tif_byte, tif_from_byte,
    write_i64, write_u16, write_u32, write_u64, DecodeError,
};
use types::{Side, TimeInForce};

/// Bytes in the message header (template, version, block length).
pub const HEADER_LEN: usize = 4;

/// Schema versions this crate can encode. Decoders accept any version ≥ 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Version {
    /// The original field set.
    V1 = 1,
    /// v1 plus optional trailing fields.
    V2 = 2,
}

impl Version {
    /// The version encoders use by default.
    pub const CURRENT: Version = Version::V2;
}

/// The 4-byte header in front of every schema message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Header {
    /// Message type id.
    pub template: u8,
    /// Schema version the sender encoded.
    pub version: u8,
    /// Root block length in bytes, not counting the header.
    pub block_len: u16,
}

// @student w35
impl Header {
    /// Reads the header at the start of `buf` and checks that the version
    /// is not 0 and that `buf` holds the whole block. Does not check the
    /// template (the caller dispatches on it). O(1).
    #[inline]
    pub fn read(buf: &[u8]) -> Result<Header, DecodeError> {
        if buf.len() < HEADER_LEN {
            return Err(DecodeError::Truncated {
                need: HEADER_LEN,
                have: buf.len(),
            });
        }
        let header = Header {
            template: buf[0],
            version: buf[1],
            block_len: read_u16(buf, 2),
        };
        if header.version == 0 {
            return Err(DecodeError::UnsupportedVersion(0));
        }
        if buf.len() < header.message_len() {
            return Err(DecodeError::Truncated {
                need: header.message_len(),
                have: buf.len(),
            });
        }
        Ok(header)
    }

    /// Writes the header into the first 4 bytes of `buf`. Panics if `buf`
    /// is shorter. O(1).
    #[inline]
    pub fn write(&self, buf: &mut [u8]) {
        buf[0] = self.template;
        buf[1] = self.version;
        write_u16(buf, 2, self.block_len);
    }

    /// Header plus block: the bytes this message occupies.
    #[inline]
    pub fn message_len(&self) -> usize {
        HEADER_LEN + self.block_len as usize
    }
}

/// A type that can be a schema field: fixed size, little-endian.
pub trait Field: Copy {
    /// Encoded size in bytes.
    const SIZE: usize;
    /// Writes `self` at `buf[at..at + SIZE]`.
    fn write(self, buf: &mut [u8], at: usize);
    /// Reads the field at `at`. Only called after [`Field::check`]
    /// succeeded, so it may assume the bytes are valid.
    fn read(buf: &[u8], at: usize) -> Self;
    /// Validates the encoded bytes; `Err(raw value)` if out of domain.
    /// Integers are always valid.
    #[inline]
    fn check(buf: &[u8], at: usize) -> Result<(), u64> {
        let _ = (buf, at);
        Ok(())
    }
}

/// A field type usable as an optional (v2) field: one value is reserved as
/// "null" (absent). `Some(NULL)` cannot be represented and encodes as `None`.
pub trait OptionalField: Field + PartialEq {
    /// The reserved null value.
    const NULL: Self;
}

/// One row of a message's layout table (`Name::FIELDS`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldInfo {
    /// Field name as written in the schema.
    pub name: &'static str,
    /// Byte offset from the start of the message (header included).
    pub offset: usize,
    /// Encoded size in bytes.
    pub size: usize,
    /// Schema version that introduced the field.
    pub since: u8,
}

impl FieldInfo {
    /// A row with its offset not yet computed (see [`layout`]).
    pub const fn new(name: &'static str, size: usize, since: u8) -> FieldInfo {
        FieldInfo {
            name,
            offset: 0,
            size,
            since,
        }
    }
}

/// Fills in offsets: fields are packed in order right after the header.
/// Evaluated at compile time for every `Name::FIELDS`. O(N).
pub const fn layout<const N: usize>(mut fields: [FieldInfo; N]) -> [FieldInfo; N] {
    let mut at = HEADER_LEN;
    let mut i = 0;
    while i < N {
        fields[i].offset = at;
        at += fields[i].size;
        i += 1;
    }
    fields
}

macro_rules! int_field {
    ($($t:ty => $read:ident, $write:ident, $null:expr;)*) => {$(
        impl Field for $t {
            const SIZE: usize = std::mem::size_of::<$t>();
            #[inline]
            fn write(self, buf: &mut [u8], at: usize) {
                $write(buf, at, self)
            }
            #[inline]
            fn read(buf: &[u8], at: usize) -> $t {
                $read(buf, at)
            }
        }
        impl OptionalField for $t {
            const NULL: $t = $null;
        }
    )*};
}

#[inline]
fn read_u8(buf: &[u8], at: usize) -> u8 {
    buf[at]
}

#[inline]
fn write_u8(buf: &mut [u8], at: usize, v: u8) {
    buf[at] = v;
}

int_field! {
    u8 => read_u8, write_u8, u8::MAX;
    u16 => read_u16, write_u16, u16::MAX;
    u32 => read_u32, write_u32, u32::MAX;
    u64 => read_u64, write_u64, u64::MAX;
    i64 => read_i64, write_i64, i64::MIN;
}

impl Field for Side {
    const SIZE: usize = 1;
    #[inline]
    fn write(self, buf: &mut [u8], at: usize) {
        buf[at] = side_byte(self);
    }
    #[inline]
    fn read(buf: &[u8], at: usize) -> Side {
        if buf[at] == crate::codec::SIDE_BUY {
            Side::Buy
        } else {
            Side::Sell
        }
    }
    #[inline]
    fn check(buf: &[u8], at: usize) -> Result<(), u64> {
        side_from_byte(buf[at]).map(|_| ()).ok_or(buf[at] as u64)
    }
}

impl Field for TimeInForce {
    const SIZE: usize = 1;
    #[inline]
    fn write(self, buf: &mut [u8], at: usize) {
        buf[at] = tif_byte(self);
    }
    #[inline]
    fn read(buf: &[u8], at: usize) -> TimeInForce {
        tif_from_byte(buf[at]).unwrap_or_default()
    }
    #[inline]
    fn check(buf: &[u8], at: usize) -> Result<(), u64> {
        tif_from_byte(buf[at]).map(|_| ()).ok_or(buf[at] as u64)
    }
}

/// Order type of a schema `NewOrder` (one byte: `L`, `M`, `P`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OrdType {
    /// `L`: limit order.
    Limit,
    /// `M`: market order (price ignored).
    Market,
    /// `P`: post-only limit order.
    PostOnly,
}

impl OrdType {
    /// Wire byte.
    pub const fn byte(self) -> u8 {
        match self {
            OrdType::Limit => b'L',
            OrdType::Market => b'M',
            OrdType::PostOnly => b'P',
        }
    }

    /// Order type for a wire byte.
    pub const fn from_byte(b: u8) -> Option<OrdType> {
        match b {
            b'L' => Some(OrdType::Limit),
            b'M' => Some(OrdType::Market),
            b'P' => Some(OrdType::PostOnly),
            _ => None,
        }
    }
}

impl Field for OrdType {
    const SIZE: usize = 1;
    #[inline]
    fn write(self, buf: &mut [u8], at: usize) {
        buf[at] = self.byte();
    }
    #[inline]
    fn read(buf: &[u8], at: usize) -> OrdType {
        OrdType::from_byte(buf[at]).unwrap_or(OrdType::Limit)
    }
    #[inline]
    fn check(buf: &[u8], at: usize) -> Result<(), u64> {
        OrdType::from_byte(buf[at])
            .map(|_| ())
            .ok_or(buf[at] as u64)
    }
}

// ---------------------------------------------------------------------------
// The generator.

/// Template stub: expands to the same public items as the generator below,
/// with `todo!()` bodies, so the learner template compiles before the
/// generator exists. The generator is defined after this stub and shadows
/// it (`macro_rules!` names are scoped textually); in the template, write
/// it at the `TODO(w35)` marker that follows. In the reference this stub is
/// never used.
#[allow(unused_macros)]
macro_rules! wire_schema {
    (
        $(#[$meta:meta])*
        message $name:ident = $template:literal, view $view:ident {
            v1 { $( $(#[$m1:meta])* $f1:ident : $t1:ty ),* $(,)? }
            v2 { $( $(#[$m2:meta])* $f2:ident : $t2:ty ),* $(,)? }
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct $name {
            $( $(#[$m1])* pub $f1: $t1, )*
            $( $(#[$m2])* pub $f2: ::core::option::Option<$t2>, )*
        }

        impl $name {
            /// Template id: byte 0 of every encoded message.
            pub const TEMPLATE: u8 = $template;
            /// Root block length of a version-1 message.
            pub const V1_BLOCK: usize = 0 $( + <$t1 as $crate::schema::Field>::SIZE )*;
            /// Root block length of a version-2 message.
            pub const V2_BLOCK: usize = Self::V1_BLOCK $( + <$t2 as $crate::schema::Field>::SIZE )*;
            /// Every field with its offset, size and version, in wire order.
            pub const FIELDS: &'static [$crate::schema::FieldInfo] = &$crate::schema::layout([
                $( $crate::schema::FieldInfo::new(stringify!($f1), <$t1 as $crate::schema::Field>::SIZE, 1), )*
                $( $crate::schema::FieldInfo::new(stringify!($f2), <$t2 as $crate::schema::Field>::SIZE, 2), )*
            ]);

            /// Encoded length (header + block) at `version`.
            pub const fn encoded_len(version: $crate::schema::Version) -> usize {
                $crate::schema::HEADER_LEN
                    + match version {
                        $crate::schema::Version::V1 => Self::V1_BLOCK,
                        $crate::schema::Version::V2 => Self::V2_BLOCK,
                    }
            }

            /// Encodes the message as `version` at the start of `buf`.
            pub fn encode_into(&self, _version: $crate::schema::Version, _buf: &mut [u8]) -> usize {
                todo!("w35: encode_into (wire_schema!)")
            }

            /// Decodes an owned copy of the message.
            pub fn decode(_buf: &[u8]) -> ::core::result::Result<$name, $crate::DecodeError> {
                todo!("w35: decode (wire_schema!)")
            }
        }

        #[doc = concat!("Zero-copy view of a [`", stringify!($name), "`] message.")]
        #[derive(Clone, Copy, Debug)]
        pub struct $view<'a> {
            _bytes: ::core::marker::PhantomData<&'a [u8]>,
        }

        impl<'a> $view<'a> {
            /// Validates header, template, block length and fields.
            pub fn new(_buf: &'a [u8]) -> ::core::result::Result<$view<'a>, $crate::DecodeError> {
                todo!("w35: view new (wire_schema!)")
            }
            /// Schema version the sender encoded.
            pub fn version(&self) -> u8 {
                todo!("w35: view version (wire_schema!)")
            }
            /// Bytes the message occupies (header + block length).
            pub fn message_len(&self) -> usize {
                todo!("w35: view message_len (wire_schema!)")
            }
            $(
                $(#[$m1])*
                pub fn $f1(&self) -> $t1 {
                    todo!("w35: field accessor (wire_schema!)")
                }
            )*
            $(
                $(#[$m2])*
                pub fn $f2(&self) -> ::core::option::Option<$t2> {
                    todo!("w35: optional field accessor (wire_schema!)")
                }
            )*
            /// Copies every field out.
            pub fn to_msg(&self) -> $name {
                todo!("w35: to_msg (wire_schema!)")
            }
        }
    };
}

// @student-begin w35
/// The schema generator (see the module docs for the grammar and what it
/// generates).
macro_rules! wire_schema {
    // Accessors for v1 fields. `$at` is the field's offset as a constant
    // expression: the previous field's offset plus its size. Each step
    // emits one accessor and recurses on the rest of the list.
    (@v1 [$at:expr]) => {};
    (@v1 [$at:expr] $(#[$m:meta])* $f:ident : $t:ty, $($rest:tt)*) => {
        $(#[$m])*
        #[inline]
        pub fn $f(&self) -> $t {
            <$t as $crate::schema::Field>::read(self.buf, $at)
        }
        wire_schema!(@v1 [$at + <$t as $crate::schema::Field>::SIZE] $($rest)*);
    };
    // Accessors for v2 fields: absent in a v1 message, null means absent.
    (@v2 [$at:expr]) => {};
    (@v2 [$at:expr] $(#[$m:meta])* $f:ident : $t:ty, $($rest:tt)*) => {
        $(#[$m])*
        ///
        /// `None` in a version-1 message or when the sender wrote null.
        #[inline]
        pub fn $f(&self) -> ::core::option::Option<$t> {
            if self.version < 2 {
                return None;
            }
            let v = <$t as $crate::schema::Field>::read(self.buf, $at);
            if v == <$t as $crate::schema::OptionalField>::NULL {
                None
            } else {
                Some(v)
            }
        }
        wire_schema!(@v2 [$at + <$t as $crate::schema::Field>::SIZE] $($rest)*);
    };
    (
        $(#[$meta:meta])*
        message $name:ident = $template:literal, view $view:ident {
            v1 { $( $(#[$m1:meta])* $f1:ident : $t1:ty ),* $(,)? }
            v2 { $( $(#[$m2:meta])* $f2:ident : $t2:ty ),* $(,)? }
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct $name {
            $( $(#[$m1])* pub $f1: $t1, )*
            $( $(#[$m2])* pub $f2: ::core::option::Option<$t2>, )*
        }

        impl $name {
            /// Template id: byte 0 of every encoded message.
            pub const TEMPLATE: u8 = $template;
            /// Root block length of a version-1 message.
            pub const V1_BLOCK: usize = 0 $( + <$t1 as $crate::schema::Field>::SIZE )*;
            /// Root block length of a version-2 message.
            pub const V2_BLOCK: usize = Self::V1_BLOCK $( + <$t2 as $crate::schema::Field>::SIZE )*;
            /// Every field with its offset, size and version, in wire order.
            pub const FIELDS: &'static [$crate::schema::FieldInfo] = &$crate::schema::layout([
                $( $crate::schema::FieldInfo::new(stringify!($f1), <$t1 as $crate::schema::Field>::SIZE, 1), )*
                $( $crate::schema::FieldInfo::new(stringify!($f2), <$t2 as $crate::schema::Field>::SIZE, 2), )*
            ]);

            /// Encoded length (header + block) at `version`.
            pub const fn encoded_len(version: $crate::schema::Version) -> usize {
                $crate::schema::HEADER_LEN
                    + match version {
                        $crate::schema::Version::V1 => Self::V1_BLOCK,
                        $crate::schema::Version::V2 => Self::V2_BLOCK,
                    }
            }

            /// Encodes the message as `version` at the start of `buf` and
            /// returns the bytes written (`encoded_len(version)`). As v1 the
            /// v2 fields are dropped; as v2 a `None` is written as the
            /// type's null value. O(fields), no allocation; panics if `buf`
            /// is too short.
            pub fn encode_into(&self, version: $crate::schema::Version, buf: &mut [u8]) -> usize {
                use $crate::schema::{Field, Header, OptionalField, Version, HEADER_LEN};
                let len = Self::encoded_len(version);
                assert!(buf.len() >= len, "encode_into: need {len} bytes, buffer has {}", buf.len());
                let buf = &mut buf[..len];
                Header { template: $template, version: version as u8, block_len: (len - HEADER_LEN) as u16 }
                    .write(buf);
                let at = HEADER_LEN;
                $(
                    Field::write(self.$f1, buf, at);
                    let at = at + <$t1 as Field>::SIZE;
                )*
                let at = if version >= Version::V2 {
                    $(
                        Field::write(self.$f2.unwrap_or(<$t2 as OptionalField>::NULL), buf, at);
                        let at = at + <$t2 as Field>::SIZE;
                    )*
                    at
                } else {
                    at
                };
                debug_assert_eq!(at, len);
                len
            }

            /// Decodes an owned copy of the message at the start of `buf`
            /// (any version ≥ 1). O(fields), no allocation.
            #[inline]
            pub fn decode(buf: &[u8]) -> ::core::result::Result<$name, $crate::DecodeError> {
                Ok($view::new(buf)?.to_msg())
            }
        }

        #[doc = concat!("Zero-copy view of a [`", stringify!($name), "`] message.")]
        #[derive(Clone, Copy, Debug)]
        pub struct $view<'a> {
            /// Exactly the message: header + `block_len` bytes.
            buf: &'a [u8],
            version: u8,
        }

        impl<'a> $view<'a> {
            /// Validates the header (version ≥ 1, whole block present), the
            /// template, the block length its version requires, and every
            /// enumerated field. O(fields), no allocation.
            #[inline]
            pub fn new(buf: &'a [u8]) -> ::core::result::Result<$view<'a>, $crate::DecodeError> {
                use $crate::schema::{Field, Header, HEADER_LEN};
                use $crate::DecodeError;
                let header = Header::read(buf)?;
                if header.template != $template {
                    return Err(DecodeError::UnexpectedType { expected: $template, found: header.template });
                }
                let need = if header.version >= 2 { $name::V2_BLOCK } else { $name::V1_BLOCK };
                if (header.block_len as usize) < need {
                    return Err(DecodeError::BlockTooShort { need, have: header.block_len as usize });
                }
                let at = HEADER_LEN;
                $(
                    if let Err(value) = <$t1 as Field>::check(buf, at) {
                        return Err(DecodeError::InvalidField { field: stringify!($f1), value });
                    }
                    let at = at + <$t1 as Field>::SIZE;
                )*
                if header.version >= 2 {
                    $(
                        if let Err(value) = <$t2 as Field>::check(buf, at) {
                            return Err(DecodeError::InvalidField { field: stringify!($f2), value });
                        }
                        let at = at + <$t2 as Field>::SIZE;
                    )*
                    let _ = at;
                }
                Ok($view { buf: &buf[..header.message_len()], version: header.version })
            }

            /// Schema version the sender encoded (may be newer than 2).
            #[inline]
            pub fn version(&self) -> u8 {
                self.version
            }

            /// Bytes the message occupies: header + `block_len`, including
            /// trailing fields this decoder does not know.
            #[inline]
            pub fn message_len(&self) -> usize {
                self.buf.len()
            }

            wire_schema!(@v1 [$crate::schema::HEADER_LEN] $( $(#[$m1])* $f1 : $t1, )*);
            wire_schema!(@v2 [$crate::schema::HEADER_LEN + $name::V1_BLOCK] $( $(#[$m2])* $f2 : $t2, )*);

            /// Copies every field out.
            #[inline]
            pub fn to_msg(&self) -> $name {
                $name {
                    $( $f1: self.$f1(), )*
                    $( $f2: self.$f2(), )*
                }
            }
        }
    };
}
// @student-end

// ---------------------------------------------------------------------------
// The schema.

wire_schema! {
    /// Enter an order — the schema-driven twin of week 11's `NewOrder`.
    message NewOrder = 1, view NewOrderView {
        v1 {
            /// Account the order trades for.
            account: u32,
            /// Client-chosen order id.
            id: u64,
            /// Quantity in lots.
            qty: u64,
            /// Limit price in ticks (ignored for market orders).
            price: i64,
            /// Instrument to trade.
            instrument: u32,
            /// Buy or sell.
            side: Side,
            /// Limit, market or post-only.
            ord_type: OrdType,
            /// Time in force.
            tif: TimeInForce,
        }
        v2 {
            /// Client send time, ns since the epoch (latency tracing).
            client_ts: u64,
            /// Opaque client reference, echoed on executions.
            client_tag: u64,
        }
    }
}

wire_schema! {
    /// Cancel an order.
    message Cancel = 2, view CancelView {
        v1 {
            /// The order to cancel.
            id: u64,
        }
        v2 {
            /// Client send time, ns since the epoch.
            client_ts: u64,
        }
    }
}

wire_schema! {
    /// Reduce an order's remaining quantity.
    message Reduce = 3, view ReduceView {
        v1 {
            /// The order to reduce.
            id: u64,
            /// New (lower) remaining quantity.
            qty: u64,
        }
        v2 {
            /// Client send time, ns since the epoch.
            client_ts: u64,
        }
    }
}

wire_schema! {
    /// A fill between a taker and a maker.
    message Trade = 4, view TradeView {
        v1 {
            /// Incoming order.
            taker: u64,
            /// Resting order.
            maker: u64,
            /// Execution price in ticks (the maker's).
            price: i64,
            /// Executed quantity.
            qty: u64,
            /// Side of the taker.
            taker_side: Side,
        }
        v2 {
            /// Exchange-wide match number.
            match_id: u64,
            /// Exchange execution time, ns since the epoch.
            exec_ts: u64,
        }
    }
}

/// Any message of the schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaMessage {
    /// Template 1.
    NewOrder(NewOrder),
    /// Template 2.
    Cancel(Cancel),
    /// Template 3.
    Reduce(Reduce),
    /// Template 4.
    Trade(Trade),
}

impl SchemaMessage {
    /// Encodes as `version`; returns the bytes written. Panics if `buf` is
    /// too short. O(fields), no allocation.
    pub fn encode_into(&self, version: Version, buf: &mut [u8]) -> usize {
        match self {
            SchemaMessage::NewOrder(m) => m.encode_into(version, buf),
            SchemaMessage::Cancel(m) => m.encode_into(version, buf),
            SchemaMessage::Reduce(m) => m.encode_into(version, buf),
            SchemaMessage::Trade(m) => m.encode_into(version, buf),
        }
    }
}

// @student w35
/// Decodes the message at the start of `buf`, whatever its template, and
/// returns it with the number of bytes it occupied (header + block length,
/// so a stream of concatenated messages can be walked even when they carry
/// fields this decoder does not know). O(fields), no allocation.
pub fn decode_message(buf: &[u8]) -> Result<(SchemaMessage, usize), DecodeError> {
    let header = Header::read(buf)?;
    let msg = match header.template {
        NewOrder::TEMPLATE => SchemaMessage::NewOrder(NewOrder::decode(buf)?),
        Cancel::TEMPLATE => SchemaMessage::Cancel(Cancel::decode(buf)?),
        Reduce::TEMPLATE => SchemaMessage::Reduce(Reduce::decode(buf)?),
        Trade::TEMPLATE => SchemaMessage::Trade(Trade::decode(buf)?),
        other => return Err(DecodeError::UnknownType(other)),
    };
    Ok((msg, header.message_len()))
}

// ---------------------------------------------------------------------------
// Mapping to the shared types.

impl NewOrder {
    /// The schema message for an engine order (v2 fields absent).
    pub fn from_order(o: &types::NewOrder) -> NewOrder {
        let (ord_type, price) = match o.kind {
            types::OrderKind::Limit {
                price,
                post_only: false,
            } => (OrdType::Limit, price.0),
            types::OrderKind::Limit {
                price,
                post_only: true,
            } => (OrdType::PostOnly, price.0),
            types::OrderKind::Market => (OrdType::Market, 0),
        };
        NewOrder {
            account: o.account.0,
            id: o.id.0,
            qty: o.qty.0,
            price,
            instrument: o.instrument.0,
            side: o.side,
            ord_type,
            tif: o.tif,
            client_ts: None,
            client_tag: None,
        }
    }

    /// The engine order this message asks for.
    pub fn to_order(&self) -> types::NewOrder {
        let kind = match self.ord_type {
            OrdType::Limit => types::OrderKind::Limit {
                price: types::Price(self.price),
                post_only: false,
            },
            OrdType::PostOnly => types::OrderKind::Limit {
                price: types::Price(self.price),
                post_only: true,
            },
            OrdType::Market => types::OrderKind::Market,
        };
        types::NewOrder {
            id: types::OrderId(self.id),
            account: types::AccountId(self.account),
            instrument: types::InstrumentId(self.instrument),
            side: self.side,
            qty: types::Qty(self.qty),
            kind,
            tif: self.tif,
        }
    }
}

impl SchemaMessage {
    /// The engine command this message carries (`None` for `Trade`).
    pub fn to_command(&self) -> Option<types::Command> {
        match *self {
            SchemaMessage::NewOrder(m) => Some(types::Command::New(m.to_order())),
            SchemaMessage::Cancel(m) => Some(types::Command::Cancel {
                id: types::OrderId(m.id),
            }),
            SchemaMessage::Reduce(m) => Some(types::Command::Reduce {
                id: types::OrderId(m.id),
                qty: types::Qty(m.qty),
            }),
            SchemaMessage::Trade(_) => None,
        }
    }
}

impl Trade {
    /// The engine event for this fill.
    pub fn to_event(&self) -> types::Event {
        types::Event::Trade {
            taker: types::OrderId(self.taker),
            maker: types::OrderId(self.maker),
            taker_side: self.taker_side,
            price: types::Price(self.price),
            qty: types::Qty(self.qty),
        }
    }
}
