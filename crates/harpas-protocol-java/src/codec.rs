//! Wire types shared by every codec: the runtime the generated code calls
//!
//! Decoders read from `&mut &[u8]`, advancing the slice past what they read. They check every length against
//! the bytes left before allocating, so a packet can never make a decoder allocate more than its own size.
//! Encoders append to a `Vec<u8>` and never fail; the caller is responsible for staying within vanilla's
//! limits.

use std::error::Error;
use std::fmt;

pub use harpas_core::nbt::Nbt;
use harpas_core::nbt::NbtError;

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
    /// Network NBT was invalid.
    Nbt(NbtError),
    /// A registry holder had a negative ID.
    InvalidRegistryId(i32),
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
            DecodeError::Nbt(e) => e.fmt(f),
            DecodeError::InvalidRegistryId(id) => write!(f, "invalid registry ID {id}"),
        }
    }
}

impl Error for DecodeError {}

impl From<NbtError> for DecodeError {
    fn from(e: NbtError) -> Self {
        DecodeError::Nbt(e)
    }
}

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

/// Reads network NBT: a tag type and its payload, with no root name. The root cannot be `TAG_End`.
pub fn read_nbt(input: &mut &[u8]) -> Result<Nbt, DecodeError> {
    Ok(Nbt::read_network(input)?)
}

/// Writes network NBT.
pub fn write_nbt(buf: &mut Vec<u8>, value: &Nbt) {
    value.write_network(buf);
}

/// Reads optional network NBT, where `TAG_End` alone means no value.
pub fn read_optional_nbt(input: &mut &[u8]) -> Result<Option<Nbt>, DecodeError> {
    Ok(Nbt::read_network_optional(input)?)
}

/// Writes optional network NBT.
pub fn write_optional_nbt(buf: &mut Vec<u8>, value: &Option<Nbt>) {
    Nbt::write_network_optional(value.as_ref(), buf);
}

/// Reads exactly `N` bytes, with no length prefix.
pub fn read_fixed<const N: usize>(input: &mut &[u8]) -> Result<[u8; N], DecodeError> {
    take_array(input)
}

/// Writes `N` bytes, with no length prefix.
pub fn write_fixed<const N: usize>(buf: &mut Vec<u8>, value: &[u8; N]) {
    buf.extend_from_slice(value);
}

/// A registry entry: a reference to an entry the client has, or a value sent inline.
#[derive(Debug, Clone, PartialEq)]
pub enum Holder<T> {
    /// Numeric ID of the entry in its registry, in the order of Registry Data.
    Reference(i32),
    /// A value sent inline.
    Direct(T),
}

/// Reads a registry holder: a VarInt, 0 for a value read with `read` or the registry ID plus one.
pub fn read_holder<T>(
    input: &mut &[u8],
    read: impl FnOnce(&mut &[u8]) -> Result<T, DecodeError>,
) -> Result<Holder<T>, DecodeError> {
    match read_varint(input)? {
        0 => read(input).map(Holder::Direct),
        id if id > 0 => Ok(Holder::Reference(id - 1)),
        id => Err(DecodeError::InvalidRegistryId(id)),
    }
}

/// Writes a registry holder, with `write` for a value sent inline.
pub fn write_holder<T>(buf: &mut Vec<u8>, value: &Holder<T>, write: impl FnOnce(&mut Vec<u8>, &T)) {
    match value {
        Holder::Reference(id) => {
            write_varint(
                buf,
                id.checked_add(1).expect("registry ID fits in a VarInt"),
            );
        }
        Holder::Direct(value) => {
            write_varint(buf, 0);
            write(buf, value);
        }
    }
}

/// A vector of three doubles in low precision, used for entity velocities: each axis is 15 bits of a shared
/// scale, which is the largest absolute value rounded up to an integer.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LpVec3 {
    /// X component.
    pub x: f64,
    /// Y component.
    pub y: f64,
    /// Z component.
    pub z: f64,
}

impl LpVec3 {
    /// Largest absolute value sent; larger ones are clamped.
    pub const ABS_MAX: f64 = 1.7179869183E10;
    /// Smallest absolute value sent; a vector whose components are all smaller is sent as zero.
    pub const ABS_MIN: f64 = 3.051944088384301E-5;
    const MAX_QUANTIZED: f64 = 32766.0;
    const CONTINUATION: u64 = 4;

    fn pack(value: f64) -> u64 {
        // Java's `Math.round`, on values from 0 to 32766.
        ((value * 0.5 + 0.5) * Self::MAX_QUANTIZED + 0.5).floor() as u64
    }

    fn unpack(value: u64) -> f64 {
        ((value & 0x7fff) as f64).min(Self::MAX_QUANTIZED) * 2.0 / Self::MAX_QUANTIZED - 1.0
    }

    fn sanitize(value: f64) -> f64 {
        if value.is_nan() {
            0.0
        } else {
            value.clamp(-Self::ABS_MAX, Self::ABS_MAX)
        }
    }
}

impl Encode for LpVec3 {
    fn encode(&self, buf: &mut Vec<u8>) {
        let (x, y, z) = (
            Self::sanitize(self.x),
            Self::sanitize(self.y),
            Self::sanitize(self.z),
        );
        let largest = x.abs().max(y.abs()).max(z.abs());
        if largest < Self::ABS_MIN {
            buf.push(0);
            return;
        }
        let scale = largest.ceil() as u64;
        let partial = scale & 3 != scale;
        let markers = if partial {
            scale & 3 | Self::CONTINUATION
        } else {
            scale
        };
        let scale_f = scale as f64;
        let packed = markers
            | Self::pack(x / scale_f) << 3
            | Self::pack(y / scale_f) << 18
            | Self::pack(z / scale_f) << 33;
        buf.push(packed as u8);
        buf.push((packed >> 8) as u8);
        write_u32(buf, (packed >> 16) as u32);
        if partial {
            write_varint(buf, (scale >> 2) as u32 as i32);
        }
    }
}

impl Decode for LpVec3 {
    fn decode(input: &mut &[u8]) -> Result<Self, DecodeError> {
        let lowest = read_u8(input)?;
        if lowest == 0 {
            return Ok(Self::default());
        }
        let middle = read_u8(input)?;
        let highest = read_u32(input)?;
        let packed = u64::from(highest) << 16 | u64::from(middle) << 8 | u64::from(lowest);
        let mut scale = u64::from(lowest) & 3;
        if u64::from(lowest) & Self::CONTINUATION != 0 {
            scale |= u64::from(read_varint(input)? as u32) << 2;
        }
        let scale = scale as f64;
        Ok(Self {
            x: Self::unpack(packed >> 3) * scale,
            y: Self::unpack(packed >> 18) * scale,
            z: Self::unpack(packed >> 33) * scale,
        })
    }
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
    fn holders_are_offset_by_one() {
        let mut buf = Vec::new();
        write_holder(&mut buf, &Holder::Reference(4), |buf, v: &u8| buf.push(*v));
        write_holder(&mut buf, &Holder::Direct(9u8), |buf, v| buf.push(*v));
        assert_eq!(buf, [0x05, 0x00, 0x09]);
        let mut input = &buf[..];
        assert_eq!(read_holder(&mut input, read_u8), Ok(Holder::Reference(4)));
        assert_eq!(read_holder(&mut input, read_u8), Ok(Holder::Direct(9)));
        assert_eq!(
            read_holder(&mut &[0xff, 0xff, 0xff, 0xff, 0x0f][..], read_u8),
            Err(DecodeError::InvalidRegistryId(-1))
        );
    }

    #[test]
    fn lp_vec3_matches_vanilla_packing() {
        let encode = |x, y, z| {
            let mut buf = Vec::new();
            LpVec3 { x, y, z }.encode(&mut buf);
            buf
        };
        // Below the smallest value, the vector is a single zero byte.
        assert_eq!(encode(0.0, 1e-6, 0.0), [0x00]);
        // Scale 1; x = -1 packs to 0, y = 0 to 16383 and z = 1 to 32766.
        let packed: u64 = 1 | 16383 << 18 | 32766 << 33;
        let mut expected = vec![packed as u8, (packed >> 8) as u8];
        expected.extend_from_slice(&((packed >> 16) as u32).to_be_bytes());
        assert_eq!(encode(-1.0, 0.0, 1.0), expected);
        // Scale 5 does not fit in two bits: 1 and the continuation flag, then the rest (1) as a VarInt.
        let buf = encode(4.5, 0.0, 0.0);
        assert_eq!(buf[0] & 7, 0b101);
        assert_eq!(buf[6..], [0x01]);
        let decoded = LpVec3::decode(&mut &buf[..]).unwrap();
        assert!((decoded.x - 4.5).abs() < 5.0 / 16383.0, "{decoded:?}");
        assert!(decoded.y.abs() < 5.0 / 16383.0, "{decoded:?}");
        assert_eq!(encode(decoded.x, decoded.y, decoded.z), buf);
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
