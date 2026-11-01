//! Wire types shared by every codec: the runtime the generated code calls
//!
//! Decoders read from `&mut &[u8]`, advancing the slice past what they read. They check every length against
//! the bytes left before allocating, so a packet can never make a decoder allocate more than its own size.
//! Encoders append to a `Vec<u8>` and never fail; the caller is responsible for staying within vanilla's
//! limits.

use std::error::Error;
use std::fmt;

/// A type that can be appended to a packet.
pub trait Encode {
    /// Appends `self` to `buf`.
    fn encode(&self, buf: &mut Vec<u8>);
}

/// A type that can be read from a packet.
pub trait Decode: Sized {
    /// Reads a value from the start of `input` and advances it.
    fn decode(input: &mut &[u8]) -> Result<Self, DecodeError>;
}

/// Why a packet could not be decoded. Any of these closes the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The packet ended in the middle of a value.
    UnexpectedEnd,
    /// A VarInt or VarLong had more bytes than its type allows.
    VarIntTooLong,
    /// A `bool` was neither 0 nor 1.
    InvalidBool(u8),
    /// A length prefix was negative.
    NegativeLength(i32),
    /// A string, array or byte array was longer than its limit.
    TooLong {
        /// Length found.
        len: usize,
        /// Limit for the field.
        max: usize,
    },
    /// A string was not valid UTF-8.
    InvalidUtf8,
    /// An enumeration had a value with no variant.
    UnknownEnumValue {
        /// Name of the enumeration.
        name: &'static str,
        /// Value found.
        value: i64,
    },
    /// No packet has this ID in the current state.
    UnknownPacket(i32),
    /// Bytes were left after the last field of the packet.
    TrailingBytes(usize),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::UnexpectedEnd => f.write_str("packet ended unexpectedly"),
            DecodeError::VarIntTooLong => f.write_str("VarInt is too long"),
            DecodeError::InvalidBool(value) => write!(f, "invalid boolean {value}"),
            DecodeError::NegativeLength(len) => write!(f, "negative length {len}"),
            DecodeError::TooLong { len, max } => {
                write!(f, "length {len} is over the limit of {max}")
            }
            DecodeError::InvalidUtf8 => f.write_str("string is not valid UTF-8"),
            DecodeError::UnknownEnumValue { name, value } => {
                write!(f, "{value} is not a valid {name}")
            }
            DecodeError::UnknownPacket(id) => write!(f, "unknown packet ID {id:#04x}"),
            DecodeError::TrailingBytes(len) => write!(f, "{len} bytes left after the packet"),
        }
    }
}

impl Error for DecodeError {}

/// Fails unless every byte of `input` was read.
pub fn expect_end(input: &[u8]) -> Result<(), DecodeError> {
    if input.is_empty() {
        Ok(())
    } else {
        Err(DecodeError::TrailingBytes(input.len()))
    }
}

fn take<'a>(input: &mut &'a [u8], len: usize) -> Result<&'a [u8], DecodeError> {
    if input.len() < len {
        return Err(DecodeError::UnexpectedEnd);
    }
    let (taken, rest) = input.split_at(len);
    *input = rest;
    Ok(taken)
}

fn take_array<const N: usize>(input: &mut &[u8]) -> Result<[u8; N], DecodeError> {
    let bytes = take(input, N)?;
    Ok(bytes.try_into().expect("take returned N bytes"))
}

macro_rules! fixed {
    ($($read:ident, $write:ident, $ty:ty;)*) => {$(
        #[doc = concat!("Reads a big-endian `", stringify!($ty), "`.")]
        pub fn $read(input: &mut &[u8]) -> Result<$ty, DecodeError> {
            take_array(input).map(<$ty>::from_be_bytes)
        }

        #[doc = concat!("Writes a big-endian `", stringify!($ty), "`.")]
        pub fn $write(buf: &mut Vec<u8>, value: $ty) {
            buf.extend_from_slice(&value.to_be_bytes());
        }
    )*};
}

fixed! {
    read_u8, write_u8, u8;
    read_i8, write_i8, i8;
    read_u16, write_u16, u16;
    read_i16, write_i16, i16;
    read_u32, write_u32, u32;
    read_i32, write_i32, i32;
    read_u64, write_u64, u64;
    read_i64, write_i64, i64;
    read_f32, write_f32, f32;
    read_f64, write_f64, f64;
    read_uuid, write_uuid, u128;
}

/// Reads a `bool`, which must be 0 or 1.
pub fn read_bool(input: &mut &[u8]) -> Result<bool, DecodeError> {
    match read_u8(input)? {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(DecodeError::InvalidBool(other)),
    }
}

/// Writes a `bool`.
pub fn write_bool(buf: &mut Vec<u8>, value: bool) {
    buf.push(u8::from(value));
}

fn read_leb128(input: &mut &[u8], max_bytes: u32) -> Result<u64, DecodeError> {
    let mut value = 0u64;
    for i in 0..max_bytes {
        let byte = read_u8(input)?;
        value |= u64::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(DecodeError::VarIntTooLong)
}

fn write_leb128(buf: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        buf.push(value as u8 | 0x80);
        value >>= 7;
    }
    buf.push(value as u8);
}

/// Reads a VarInt: an `i32` in at most 5 bytes.
pub fn read_varint(input: &mut &[u8]) -> Result<i32, DecodeError> {
    // Bits past the 32nd of a 5-byte VarInt are dropped, as vanilla does.
    read_leb128(input, 5).map(|value| value as u32 as i32)
}

/// Writes a VarInt.
pub fn write_varint(buf: &mut Vec<u8>, value: i32) {
    write_leb128(buf, u64::from(value as u32));
}

/// Reads a VarLong: an `i64` in at most 10 bytes.
pub fn read_varlong(input: &mut &[u8]) -> Result<i64, DecodeError> {
    read_leb128(input, 10).map(|value| value as i64)
}

/// Writes a VarLong.
pub fn write_varlong(buf: &mut Vec<u8>, value: i64) {
    write_leb128(buf, value as u64);
}

/// Reads a VarInt length and checks it against `max` and against the bytes left.
fn read_len(input: &mut &[u8], max: usize) -> Result<usize, DecodeError> {
    let len = read_varint(input)?;
    let len = usize::try_from(len).map_err(|_| DecodeError::NegativeLength(len))?;
    if len > max {
        return Err(DecodeError::TooLong { len, max });
    }
    if len > input.len() {
        return Err(DecodeError::UnexpectedEnd);
    }
    Ok(len)
}

fn write_len(buf: &mut Vec<u8>, len: usize) {
    write_varint(buf, i32::try_from(len).expect("length fits in a VarInt"));
}

/// Reads a string of at most `max` UTF-16 code units, as vanilla counts them.
pub fn read_string(input: &mut &[u8], max: usize) -> Result<String, DecodeError> {
    // A UTF-16 code unit takes at most 3 bytes in UTF-8.
    let len = read_len(input, max.saturating_mul(3))?;
    let text = std::str::from_utf8(take(input, len)?).map_err(|_| DecodeError::InvalidUtf8)?;
    let units = text.encode_utf16().count();
    if units > max {
        return Err(DecodeError::TooLong { len: units, max });
    }
    Ok(text.to_owned())
}

/// Writes a string.
pub fn write_string(buf: &mut Vec<u8>, value: &str) {
    write_bytes(buf, value.as_bytes());
}

/// Reads a byte array with a VarInt length of at most `max`.
pub fn read_bytes(input: &mut &[u8], max: usize) -> Result<Vec<u8>, DecodeError> {
    let len = read_len(input, max)?;
    Ok(take(input, len)?.to_vec())
}

/// Writes a byte array with its VarInt length.
pub fn write_bytes(buf: &mut Vec<u8>, value: &[u8]) {
    write_len(buf, value.len());
    buf.extend_from_slice(value);
}

/// Reads every byte left in the packet, at most `max`.
pub fn read_rest(input: &mut &[u8], max: usize) -> Result<Vec<u8>, DecodeError> {
    if input.len() > max {
        return Err(DecodeError::TooLong {
            len: input.len(),
            max,
        });
    }
    Ok(take(input, input.len())?.to_vec())
}

/// Writes bytes with no length; they run to the end of the packet.
pub fn write_rest(buf: &mut Vec<u8>, value: &[u8]) {
    buf.extend_from_slice(value);
}

/// Reads a `bool`, then a value with `read` when it is true.
pub fn read_option<T>(
    input: &mut &[u8],
    read: impl FnOnce(&mut &[u8]) -> Result<T, DecodeError>,
) -> Result<Option<T>, DecodeError> {
    if read_bool(input)? {
        read(input).map(Some)
    } else {
        Ok(None)
    }
}

/// Writes whether `value` is present, then the value with `write`.
pub fn write_option<T>(buf: &mut Vec<u8>, value: &Option<T>, write: impl FnOnce(&mut Vec<u8>, &T)) {
    write_bool(buf, value.is_some());
    if let Some(value) = value {
        write(buf, value);
    }
}

/// Reads a VarInt count of at most `max`, then that many elements with `read`.
pub fn read_array<T>(
    input: &mut &[u8],
    max: usize,
    mut read: impl FnMut(&mut &[u8]) -> Result<T, DecodeError>,
) -> Result<Vec<T>, DecodeError> {
    // Every element is counted as at least one byte, so a count can never ask for more elements, or more
    // iterations, than the packet has bytes.
    let len = read_len(input, max)?;
    let mut items = Vec::with_capacity(len);
    for _ in 0..len {
        items.push(read(input)?);
    }
    Ok(items)
}

/// Writes the VarInt count of `items`, then each element with `write`.
pub fn write_array<T>(buf: &mut Vec<u8>, items: &[T], mut write: impl FnMut(&mut Vec<u8>, &T)) {
    write_len(buf, items.len());
    for item in items {
        write(buf, item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_match_the_documented_examples() {
        let cases: [(i32, &[u8]); 7] = [
            (0, &[0x00]),
            (127, &[0x7f]),
            (128, &[0x80, 0x01]),
            (25565, &[0xdd, 0xc7, 0x01]),
            (i32::MAX, &[0xff, 0xff, 0xff, 0xff, 0x07]),
            (-1, &[0xff, 0xff, 0xff, 0xff, 0x0f]),
            (i32::MIN, &[0x80, 0x80, 0x80, 0x80, 0x08]),
        ];
        for (value, bytes) in cases {
            let mut buf = Vec::new();
            write_varint(&mut buf, value);
            assert_eq!(buf, bytes, "{value}");
            assert_eq!(read_varint(&mut &bytes[..]), Ok(value));
        }
        assert_eq!(
            read_varint(&mut &[0xff; 6][..]),
            Err(DecodeError::VarIntTooLong)
        );
        assert_eq!(
            read_varint(&mut &[0x80][..]),
            Err(DecodeError::UnexpectedEnd)
        );
    }

    #[test]
    fn varlongs_round_trip() {
        for value in [0, 1, -1, i64::MAX, i64::MIN] {
            let mut buf = Vec::new();
            write_varlong(&mut buf, value);
            assert_eq!(read_varlong(&mut &buf[..]), Ok(value));
        }
        assert_eq!(
            read_varlong(&mut &[0xff; 11][..]),
            Err(DecodeError::VarIntTooLong)
        );
    }

    #[test]
    fn strings_are_limited_in_utf16_code_units() {
        let mut buf = Vec::new();
        write_string(&mut buf, "ñandú");
        assert_eq!(read_string(&mut &buf[..], 5), Ok("ñandú".to_owned()));
        assert_eq!(
            read_string(&mut &buf[..], 4),
            Err(DecodeError::TooLong { len: 5, max: 4 })
        );
        // Each emoji is two UTF-16 code units in four UTF-8 bytes.
        let mut buf = Vec::new();
        write_string(&mut buf, "🦀🦀");
        assert_eq!(
            read_string(&mut &buf[..], 3),
            Err(DecodeError::TooLong { len: 4, max: 3 })
        );
    }

    #[test]
    fn lengths_are_checked_before_allocating() {
        // A count of 2^31 - 1 with nothing after it.
        let huge = [0xff, 0xff, 0xff, 0xff, 0x07];
        assert_eq!(
            read_bytes(&mut &huge[..], usize::MAX),
            Err(DecodeError::UnexpectedEnd)
        );
        assert_eq!(
            read_string(&mut &huge[..], usize::MAX),
            Err(DecodeError::UnexpectedEnd)
        );
        assert_eq!(
            read_array(&mut &huge[..], usize::MAX, |_| Ok(())),
            Err(DecodeError::UnexpectedEnd)
        );
        assert_eq!(
            read_bytes(&mut &[0x80, 0x80, 0x80, 0x80, 0x08][..], 10),
            Err(DecodeError::NegativeLength(i32::MIN))
        );
        assert_eq!(
            read_rest(&mut &[0u8; 3][..], 2),
            Err(DecodeError::TooLong { len: 3, max: 2 })
        );
    }
}
