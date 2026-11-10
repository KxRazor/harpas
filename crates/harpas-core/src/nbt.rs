//! NBT values and their Java Edition binary form
//!
//! Java Edition writes NBT big endian, with strings in Java's modified UTF-8. Network NBT, used in packets
//! since Java 1.20.2, is a tag type and its payload with no root name. Lists are stored by element type, so a
//! value can never hold a list that mixes types, which the format cannot express.
//!
//! Readers take untrusted input: every length is checked against the bytes left before allocating, and
//! nesting is limited to [`MAX_DEPTH`], as in vanilla. Network NBT is also limited in the memory its value
//! takes, to [`NETWORK_QUOTA`]: a list of empty compounds is one byte each on the wire and far more in memory.

use std::error::Error;
use std::fmt;

use indexmap::IndexMap;

/// Deepest nesting of lists and compounds accepted, as vanilla's `NbtAccounter`.
pub const MAX_DEPTH: usize = 512;

/// Most memory a network NBT value may take once read, as the quota of vanilla's `NbtAccounter` for packets:
/// 2 MiB.
pub const NETWORK_QUOTA: usize = 2 * 1024 * 1024;

/// What a compound's entry is counted as besides its name and value: the map's own bookkeeping.
const ENTRY_OVERHEAD: usize = 16;

/// The memory a value being read may still take.
struct Budget(usize);

impl Budget {
    fn charge(&mut self, bytes: usize) -> Result<(), NbtError> {
        self.0 = self.0.checked_sub(bytes).ok_or(NbtError::TooLarge)?;
        Ok(())
    }
}

/// An NBT value.
#[derive(Debug, Clone, PartialEq)]
pub enum Nbt {
    /// `TAG_Byte`.
    Byte(i8),
    /// `TAG_Short`.
    Short(i16),
    /// `TAG_Int`.
    Int(i32),
    /// `TAG_Long`.
    Long(i64),
    /// `TAG_Float`.
    Float(f32),
    /// `TAG_Double`.
    Double(f64),
    /// `TAG_Byte_Array`.
    ByteArray(Vec<i8>),
    /// `TAG_String`.
    String(String),
    /// `TAG_List`.
    List(List),
    /// `TAG_Compound`.
    Compound(Compound),
    /// `TAG_Int_Array`.
    IntArray(Vec<i32>),
    /// `TAG_Long_Array`.
    LongArray(Vec<i64>),
}

/// A `TAG_List`, stored by element type.
#[derive(Debug, Clone, PartialEq)]
pub enum List {
    /// An empty list, sent with element type `TAG_End`.
    Empty,
    /// A list of `TAG_Byte`.
    Byte(Vec<i8>),
    /// A list of `TAG_Short`.
    Short(Vec<i16>),
    /// A list of `TAG_Int`.
    Int(Vec<i32>),
    /// A list of `TAG_Long`.
    Long(Vec<i64>),
    /// A list of `TAG_Float`.
    Float(Vec<f32>),
    /// A list of `TAG_Double`.
    Double(Vec<f64>),
    /// A list of `TAG_Byte_Array`.
    ByteArray(Vec<Vec<i8>>),
    /// A list of `TAG_String`.
    String(Vec<String>),
    /// A list of `TAG_List`.
    List(Vec<List>),
    /// A list of `TAG_Compound`.
    Compound(Vec<Compound>),
    /// A list of `TAG_Int_Array`.
    IntArray(Vec<Vec<i32>>),
    /// A list of `TAG_Long_Array`.
    LongArray(Vec<Vec<i64>>),
}

/// A `TAG_Compound`: named values, kept in insertion order so that values round-trip byte for byte.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Compound(IndexMap<String, Nbt>);

impl Compound {
    /// An empty compound.
    pub fn new() -> Self {
        Self::default()
    }

    /// Value named `name`.
    pub fn get(&self, name: &str) -> Option<&Nbt> {
        self.0.get(name)
    }

    /// Sets `name` to `value`, returning the previous value. A replaced value keeps its position.
    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<Nbt>) -> Option<Nbt> {
        self.0.insert(name.into(), value.into())
    }

    /// Removes `name`, keeping the order of the other values.
    pub fn remove(&mut self, name: &str) -> Option<Nbt> {
        self.0.shift_remove(name)
    }

    /// Number of values.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the compound has no values.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Names and values, in order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Nbt)> {
        self.0.iter().map(|(name, value)| (name.as_str(), value))
    }
}

impl<K: Into<String>, V: Into<Nbt>> FromIterator<(K, V)> for Compound {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Self(
            iter.into_iter()
                .map(|(name, value)| (name.into(), value.into()))
                .collect(),
        )
    }
}

macro_rules! from {
    ($($ty:ty => $variant:ident;)*) => {$(
        impl From<$ty> for Nbt {
            fn from(value: $ty) -> Self {
                Nbt::$variant(value.into())
            }
        }
    )*};
}

from! {
    i8 => Byte;
    i16 => Short;
    i32 => Int;
    i64 => Long;
    f32 => Float;
    f64 => Double;
    String => String;
    &str => String;
    List => List;
    Compound => Compound;
}

/// Why NBT could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NbtError {
    /// The input ended in the middle of a value.
    UnexpectedEnd,
    /// A tag type was not one of the 13 defined.
    UnknownTag(u8),
    /// The root was `TAG_End`, where a value was required.
    EndRoot,
    /// A list or array length was negative.
    NegativeLength(i32),
    /// A list of `TAG_End` had elements.
    EndList,
    /// Lists and compounds were nested deeper than [`MAX_DEPTH`].
    TooDeep,
    /// A string was not valid modified UTF-8, or held an unpaired surrogate.
    InvalidString,
    /// A network value would take more memory than [`NETWORK_QUOTA`].
    TooLarge,
}

impl fmt::Display for NbtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NbtError::UnexpectedEnd => f.write_str("NBT ended unexpectedly"),
            NbtError::UnknownTag(id) => write!(f, "unknown NBT tag type {id}"),
            NbtError::EndRoot => f.write_str("NBT root is TAG_End"),
            NbtError::NegativeLength(len) => write!(f, "negative NBT length {len}"),
            NbtError::EndList => f.write_str("NBT list of TAG_End has elements"),
            NbtError::TooDeep => write!(f, "NBT nested deeper than {MAX_DEPTH}"),
            NbtError::InvalidString => f.write_str("NBT string is not valid modified UTF-8"),
            NbtError::TooLarge => write!(f, "NBT takes more than {NETWORK_QUOTA} bytes of memory"),
        }
    }
}

impl Error for NbtError {}

const END: u8 = 0;
const BYTE: u8 = 1;
const SHORT: u8 = 2;
const INT: u8 = 3;
const LONG: u8 = 4;
const FLOAT: u8 = 5;
const DOUBLE: u8 = 6;
const BYTE_ARRAY: u8 = 7;
const STRING: u8 = 8;
const LIST: u8 = 9;
const COMPOUND: u8 = 10;
const INT_ARRAY: u8 = 11;
const LONG_ARRAY: u8 = 12;

impl Nbt {
    /// Tag type ID.
    pub fn tag_id(&self) -> u8 {
        match self {
            Nbt::Byte(_) => BYTE,
            Nbt::Short(_) => SHORT,
            Nbt::Int(_) => INT,
            Nbt::Long(_) => LONG,
            Nbt::Float(_) => FLOAT,
            Nbt::Double(_) => DOUBLE,
            Nbt::ByteArray(_) => BYTE_ARRAY,
            Nbt::String(_) => STRING,
            Nbt::List(_) => LIST,
            Nbt::Compound(_) => COMPOUND,
            Nbt::IntArray(_) => INT_ARRAY,
            Nbt::LongArray(_) => LONG_ARRAY,
        }
    }

    /// Reads network NBT: a tag type, then its payload, with no root name. The value may take at most
    /// [`NETWORK_QUOTA`] bytes of memory.
    pub fn read_network(input: &mut &[u8]) -> Result<Nbt, NbtError> {
        match read_u8(input)? {
            END => Err(NbtError::EndRoot),
            id => read_payload(input, &mut Budget(NETWORK_QUOTA), id, 0),
        }
    }

    /// Reads optional network NBT, where `TAG_End` alone means no value. The limit of [`Nbt::read_network`]
    /// applies.
    pub fn read_network_optional(input: &mut &[u8]) -> Result<Option<Nbt>, NbtError> {
        match read_u8(input)? {
            END => Ok(None),
            id => read_payload(input, &mut Budget(NETWORK_QUOTA), id, 0).map(Some),
        }
    }

    /// Writes optional network NBT: the value, or `TAG_End` alone for none.
    pub fn write_network_optional(value: Option<&Nbt>, buf: &mut Vec<u8>) {
        match value {
            Some(value) => value.write_network(buf),
            None => buf.push(END),
        }
    }

    /// Writes network NBT.
    ///
    /// Panics if a string is longer than 65,535 bytes in modified UTF-8, or a list or array has more than
    /// `i32::MAX` elements; vanilla cannot write those either.
    pub fn write_network(&self, buf: &mut Vec<u8>) {
        buf.push(self.tag_id());
        self.write_payload(buf);
    }

    fn write_payload(&self, buf: &mut Vec<u8>) {
        match self {
            Nbt::Byte(value) => buf.push(*value as u8),
            Nbt::Short(value) => buf.extend_from_slice(&value.to_be_bytes()),
            Nbt::Int(value) => buf.extend_from_slice(&value.to_be_bytes()),
            Nbt::Long(value) => buf.extend_from_slice(&value.to_be_bytes()),
            Nbt::Float(value) => buf.extend_from_slice(&value.to_be_bytes()),
            Nbt::Double(value) => buf.extend_from_slice(&value.to_be_bytes()),
            Nbt::ByteArray(values) => write_array(buf, values, |buf, v| buf.push(*v as u8)),
            Nbt::String(value) => write_string(buf, value),
            Nbt::List(list) => list.write_payload(buf),
            Nbt::Compound(compound) => compound.write_payload(buf),
            Nbt::IntArray(values) => write_array(buf, values, |buf, v| {
                buf.extend_from_slice(&v.to_be_bytes());
            }),
            Nbt::LongArray(values) => write_array(buf, values, |buf, v| {
                buf.extend_from_slice(&v.to_be_bytes());
            }),
        }
    }
}

impl List {
    /// Tag type ID of the elements; `TAG_End` for an empty list.
    pub fn element_id(&self) -> u8 {
        match self {
            List::Empty => END,
            List::Byte(_) => BYTE,
            List::Short(_) => SHORT,
            List::Int(_) => INT,
            List::Long(_) => LONG,
            List::Float(_) => FLOAT,
            List::Double(_) => DOUBLE,
            List::ByteArray(_) => BYTE_ARRAY,
            List::String(_) => STRING,
            List::List(_) => LIST,
            List::Compound(_) => COMPOUND,
            List::IntArray(_) => INT_ARRAY,
            List::LongArray(_) => LONG_ARRAY,
        }
    }

    /// Number of elements.
    pub fn len(&self) -> usize {
        match self {
            List::Empty => 0,
            List::Byte(v) => v.len(),
            List::Short(v) => v.len(),
            List::Int(v) => v.len(),
            List::Long(v) => v.len(),
            List::Float(v) => v.len(),
            List::Double(v) => v.len(),
            List::ByteArray(v) => v.len(),
            List::String(v) => v.len(),
            List::List(v) => v.len(),
            List::Compound(v) => v.len(),
            List::IntArray(v) => v.len(),
            List::LongArray(v) => v.len(),
        }
    }

    /// Whether the list has no elements.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn write_payload(&self, buf: &mut Vec<u8>) {
        // A list whose vector is empty is still sent with its element type, as vanilla does for a typed
        // empty list.
        buf.push(self.element_id());
        let be = |buf: &mut Vec<u8>, bytes: &[u8]| buf.extend_from_slice(bytes);
        match self {
            List::Empty => write_len(buf, 0),
            List::Byte(v) => write_array(buf, v, |buf, v| buf.push(*v as u8)),
            List::Short(v) => write_array(buf, v, |buf, v| be(buf, &v.to_be_bytes())),
            List::Int(v) => write_array(buf, v, |buf, v| be(buf, &v.to_be_bytes())),
            List::Long(v) => write_array(buf, v, |buf, v| be(buf, &v.to_be_bytes())),
            List::Float(v) => write_array(buf, v, |buf, v| be(buf, &v.to_be_bytes())),
            List::Double(v) => write_array(buf, v, |buf, v| be(buf, &v.to_be_bytes())),
            List::ByteArray(v) => write_array(buf, v, |buf, v| {
                write_array(buf, v, |buf, v| buf.push(*v as u8));
            }),
            List::String(v) => write_array(buf, v, |buf, v| write_string(buf, v)),
            List::List(v) => write_array(buf, v, |buf, v| v.write_payload(buf)),
            List::Compound(v) => write_array(buf, v, |buf, v| v.write_payload(buf)),
            List::IntArray(v) => write_array(buf, v, |buf, v| {
                write_array(buf, v, |buf, v| be(buf, &v.to_be_bytes()));
            }),
            List::LongArray(v) => write_array(buf, v, |buf, v| {
                write_array(buf, v, |buf, v| be(buf, &v.to_be_bytes()));
            }),
        }
    }
}

impl Compound {
    fn write_payload(&self, buf: &mut Vec<u8>) {
        for (name, value) in &self.0 {
            buf.push(value.tag_id());
            write_string(buf, name);
            value.write_payload(buf);
        }
        buf.push(END);
    }
}

fn read_payload(
    input: &mut &[u8],
    budget: &mut Budget,
    id: u8,
    depth: usize,
) -> Result<Nbt, NbtError> {
    Ok(match id {
        BYTE => Nbt::Byte(read_u8(input)? as i8),
        SHORT => Nbt::Short(i16::from_be_bytes(take_array(input)?)),
        INT => Nbt::Int(i32::from_be_bytes(take_array(input)?)),
        LONG => Nbt::Long(i64::from_be_bytes(take_array(input)?)),
        FLOAT => Nbt::Float(f32::from_be_bytes(take_array(input)?)),
        DOUBLE => Nbt::Double(f64::from_be_bytes(take_array(input)?)),
        BYTE_ARRAY => Nbt::ByteArray(read_byte_array(input, budget)?),
        STRING => Nbt::String(read_string(input, budget)?),
        LIST => Nbt::List(read_list(input, budget, depth + 1)?),
        COMPOUND => Nbt::Compound(read_compound(input, budget, depth + 1)?),
        INT_ARRAY => Nbt::IntArray(read_int_array(input, budget)?),
        LONG_ARRAY => Nbt::LongArray(read_long_array(input, budget)?),
        other => return Err(NbtError::UnknownTag(other)),
    })
}

fn read_list(input: &mut &[u8], budget: &mut Budget, depth: usize) -> Result<List, NbtError> {
    if depth > MAX_DEPTH {
        return Err(NbtError::TooDeep);
    }
    let id = read_u8(input)?;
    let read_i32 = |input: &mut &[u8], _: &mut Budget| take_array(input).map(i32::from_be_bytes);
    Ok(match id {
        END => match i32::from_be_bytes(take_array(input)?) {
            len if len < 0 => return Err(NbtError::NegativeLength(len)),
            0 => List::Empty,
            _ => return Err(NbtError::EndList),
        },
        BYTE => List::Byte(read_array(input, budget, 1, |input, _| {
            Ok(read_u8(input)? as i8)
        })?),
        SHORT => List::Short(read_array(input, budget, 2, |input, _| {
            take_array(input).map(i16::from_be_bytes)
        })?),
        INT => List::Int(read_array(input, budget, 4, read_i32)?),
        LONG => List::Long(read_array(input, budget, 8, |input, _| {
            take_array(input).map(i64::from_be_bytes)
        })?),
        FLOAT => List::Float(read_array(input, budget, 4, |input, _| {
            take_array(input).map(f32::from_be_bytes)
        })?),
        DOUBLE => List::Double(read_array(input, budget, 8, |input, _| {
            take_array(input).map(f64::from_be_bytes)
        })?),
        BYTE_ARRAY => List::ByteArray(read_array(input, budget, 4, read_byte_array)?),
        STRING => List::String(read_array(input, budget, 2, read_string)?),
        LIST => List::List(read_array(input, budget, 5, |input, budget| {
            read_list(input, budget, depth + 1)
        })?),
        COMPOUND => List::Compound(read_array(input, budget, 1, |input, budget| {
            read_compound(input, budget, depth + 1)
        })?),
        INT_ARRAY => List::IntArray(read_array(input, budget, 4, read_int_array)?),
        LONG_ARRAY => List::LongArray(read_array(input, budget, 4, read_long_array)?),
        other => return Err(NbtError::UnknownTag(other)),
    })
}

fn read_compound(
    input: &mut &[u8],
    budget: &mut Budget,
    depth: usize,
) -> Result<Compound, NbtError> {
    if depth > MAX_DEPTH {
        return Err(NbtError::TooDeep);
    }
    let mut compound = Compound::new();
    loop {
        let id = read_u8(input)?;
        if id == END {
            return Ok(compound);
        }
        budget.charge(size_of::<String>() + size_of::<Nbt>() + ENTRY_OVERHEAD)?;
        let name = read_string(input, budget)?;
        let value = read_payload(input, budget, id, depth)?;
        // A repeated name replaces the earlier value, as in vanilla's map.
        compound.0.insert(name, value);
    }
}

fn read_byte_array(input: &mut &[u8], budget: &mut Budget) -> Result<Vec<i8>, NbtError> {
    read_array(input, budget, 1, |input, _| Ok(read_u8(input)? as i8))
}

fn read_int_array(input: &mut &[u8], budget: &mut Budget) -> Result<Vec<i32>, NbtError> {
    read_array(input, budget, 4, |input, _| {
        take_array(input).map(i32::from_be_bytes)
    })
}

fn read_long_array(input: &mut &[u8], budget: &mut Budget) -> Result<Vec<i64>, NbtError> {
    read_array(input, budget, 8, |input, _| {
        take_array(input).map(i64::from_be_bytes)
    })
}

/// Reads an `i32` count, then that many elements of at least `min_size` bytes each. The elements' own size
/// in memory is charged to `budget` before they are allocated.
fn read_array<T>(
    input: &mut &[u8],
    budget: &mut Budget,
    min_size: usize,
    mut read: impl FnMut(&mut &[u8], &mut Budget) -> Result<T, NbtError>,
) -> Result<Vec<T>, NbtError> {
    let len = i32::from_be_bytes(take_array(input)?);
    let len = usize::try_from(len).map_err(|_| NbtError::NegativeLength(len))?;
    if len.saturating_mul(min_size) > input.len() {
        return Err(NbtError::UnexpectedEnd);
    }
    budget.charge(len.saturating_mul(size_of::<T>()))?;
    let mut items = Vec::with_capacity(len);
    for _ in 0..len {
        items.push(read(input, budget)?);
    }
    Ok(items)
}

fn write_len(buf: &mut Vec<u8>, len: usize) {
    let len = i32::try_from(len).expect("NBT list or array has at most i32::MAX elements");
    buf.extend_from_slice(&len.to_be_bytes());
}

fn write_array<T>(buf: &mut Vec<u8>, items: &[T], mut write: impl FnMut(&mut Vec<u8>, &T)) {
    write_len(buf, items.len());
    for item in items {
        write(buf, item);
    }
}

fn read_u8(input: &mut &[u8]) -> Result<u8, NbtError> {
    take_array::<1>(input).map(|[byte]| byte)
}

fn take_array<const N: usize>(input: &mut &[u8]) -> Result<[u8; N], NbtError> {
    let Some((bytes, rest)) = input.split_first_chunk::<N>() else {
        return Err(NbtError::UnexpectedEnd);
    };
    *input = rest;
    Ok(*bytes)
}

/// Reads a `u16` byte length, then a string in modified UTF-8.
fn read_string(input: &mut &[u8], budget: &mut Budget) -> Result<String, NbtError> {
    let len = usize::from(u16::from_be_bytes(take_array(input)?));
    if len > input.len() {
        return Err(NbtError::UnexpectedEnd);
    }
    budget.charge(len)?;
    let (bytes, rest) = input.split_at(len);
    *input = rest;
    decode_mutf8(bytes)
}

/// Writes a `u16` byte length, then `value` in modified UTF-8.
fn write_string(buf: &mut Vec<u8>, value: &str) {
    let len_at = buf.len();
    buf.extend_from_slice(&[0, 0]);
    encode_mutf8(buf, value);
    let len = u16::try_from(buf.len() - len_at - 2)
        .expect("NBT string is at most 65,535 bytes in modified UTF-8");
    buf[len_at..len_at + 2].copy_from_slice(&len.to_be_bytes());
}

/// Modified UTF-8 is UTF-8 over UTF-16 code units: U+0000 takes two bytes, and characters outside the Basic
/// Multilingual Plane are a surrogate pair of three bytes each.
fn encode_mutf8(buf: &mut Vec<u8>, value: &str) {
    for unit in value.encode_utf16() {
        match unit {
            0x0001..=0x007f => buf.push(unit as u8),
            0x0000 | 0x0080..=0x07ff => {
                buf.extend_from_slice(&[0xc0 | (unit >> 6) as u8, 0x80 | (unit & 0x3f) as u8]);
            }
            _ => buf.extend_from_slice(&[
                0xe0 | (unit >> 12) as u8,
                0x80 | ((unit >> 6) & 0x3f) as u8,
                0x80 | (unit & 0x3f) as u8,
            ]),
        }
    }
}

fn decode_mutf8(bytes: &[u8]) -> Result<String, NbtError> {
    // Most strings are ASCII without U+0000, where both encodings agree.
    if bytes.iter().all(|&b| (1..0x80).contains(&b)) {
        return Ok(String::from_utf8(bytes.to_vec()).expect("ASCII is UTF-8"));
    }
    let continuation = |b: Option<&u8>| match b {
        Some(&b) if b & 0xc0 == 0x80 => Ok(u16::from(b & 0x3f)),
        _ => Err(NbtError::InvalidString),
    };
    let mut units = Vec::with_capacity(bytes.len());
    let mut iter = bytes.iter();
    while let Some(&b) = iter.next() {
        let unit = match b {
            0x01..=0x7f => u16::from(b),
            0xc0..=0xdf => (u16::from(b & 0x1f) << 6) | continuation(iter.next())?,
            0xe0..=0xef => {
                (u16::from(b & 0x0f) << 12)
                    | (continuation(iter.next())? << 6)
                    | continuation(iter.next())?
            }
            _ => return Err(NbtError::InvalidString),
        };
        units.push(unit);
    }
    String::from_utf16(&units).map_err(|_| NbtError::InvalidString)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(value: &Nbt) -> Vec<u8> {
        let mut buf = Vec::new();
        value.write_network(&mut buf);
        assert_eq!(Nbt::read_network(&mut &buf[..]).as_ref(), Ok(value));
        buf
    }

    #[test]
    fn network_compound_matches_the_documented_bytes() {
        // {name: "Bananrama"} from minecraft.wiki's NBT format page, without the root name.
        let value = Nbt::Compound([("name", "Bananrama")].into_iter().collect());
        let mut expected = vec![0x0a, 0x08, 0x00, 0x04];
        expected.extend_from_slice(b"name");
        expected.extend_from_slice(&[0x00, 0x09]);
        expected.extend_from_slice(b"Bananrama");
        expected.push(0x00);
        assert_eq!(round_trip(&value), expected);
    }

    #[test]
    fn every_tag_round_trips() {
        let mut inner = Compound::new();
        inner.insert("byte", 1i8);
        inner.insert("short", -2i16);
        inner.insert("int", 3i32);
        inner.insert("long", i64::MIN);
        inner.insert("float", 0.5f32);
        inner.insert("double", -0.25f64);
        inner.insert("bytes", Nbt::ByteArray(vec![-1, 0, 1]));
        inner.insert("ints", Nbt::IntArray(vec![i32::MAX]));
        inner.insert("longs", Nbt::LongArray(vec![]));
        let mut root = Compound::new();
        root.insert("inner", inner.clone());
        root.insert("empty", List::Empty);
        root.insert("typed empty", List::Int(vec![]));
        root.insert("compounds", List::Compound(vec![inner, Compound::new()]));
        root.insert("lists", List::List(vec![List::String(vec!["a".into()])]));
        round_trip(&Nbt::Compound(root));
        // Any tag can be the root of network NBT; text components are often a string.
        round_trip(&Nbt::String("hi".into()));
    }

    #[test]
    fn strings_use_modified_utf8() {
        let mut buf = Vec::new();
        Nbt::from("\0ñ🦀").write_network(&mut buf);
        assert_eq!(
            buf,
            [
                0x08, 0x00, 0x0a, 0xc0, 0x80, 0xc3, 0xb1, 0xed, 0xa0, 0xbe, 0xed, 0xb6, 0x80
            ]
        );
        assert_eq!(Nbt::read_network(&mut &buf[..]), Ok(Nbt::from("\0ñ🦀")));
        // An unpaired surrogate is valid in Java but has no Rust string.
        let lone = [0x08, 0x00, 0x03, 0xed, 0xa0, 0xbe];
        assert_eq!(
            Nbt::read_network(&mut &lone[..]),
            Err(NbtError::InvalidString)
        );
    }

    #[test]
    fn hostile_input_is_rejected_before_allocating() {
        let read = |bytes: &[u8]| Nbt::read_network(&mut &bytes[..]);
        assert_eq!(read(&[0x00]), Err(NbtError::EndRoot));
        assert_eq!(read(&[0x0d]), Err(NbtError::UnknownTag(13)));
        // A list of 2^31 - 1 longs, and a byte array of the same length, with nothing after them.
        assert_eq!(
            read(&[0x09, 0x04, 0x7f, 0xff, 0xff, 0xff]),
            Err(NbtError::UnexpectedEnd)
        );
        assert_eq!(
            read(&[0x07, 0x7f, 0xff, 0xff, 0xff]),
            Err(NbtError::UnexpectedEnd)
        );
        assert_eq!(
            read(&[0x07, 0xff, 0xff, 0xff, 0xff]),
            Err(NbtError::NegativeLength(-1))
        );
        assert_eq!(
            read(&[0x09, 0x00, 0x00, 0x00, 0x00, 0x01]),
            Err(NbtError::EndList)
        );
        // Lists nested one deeper than vanilla allows.
        let mut deep = vec![0x09];
        for _ in 0..MAX_DEPTH {
            deep.extend_from_slice(&[0x09, 0x00, 0x00, 0x00, 0x01]);
        }
        deep.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(read(&deep), Err(NbtError::TooDeep));
    }

    #[test]
    fn network_values_are_limited_in_memory() {
        // A megabyte of empty compounds, one byte each, would take tens of megabytes as values.
        let count = 1024 * 1024_u32;
        let mut bomb = vec![0x09, 0x0a];
        bomb.extend_from_slice(&count.to_be_bytes());
        bomb.resize(bomb.len() + count as usize, 0x00);
        assert_eq!(Nbt::read_network(&mut &bomb[..]), Err(NbtError::TooLarge));
        assert_eq!(
            Nbt::read_network_optional(&mut &bomb[..]),
            Err(NbtError::TooLarge)
        );
        // The same list fits while its values stay under the quota, and the quota is per value.
        let count = (NETWORK_QUOTA / size_of::<Compound>()) as u32;
        let mut list = vec![0x09, 0x0a];
        list.extend_from_slice(&count.to_be_bytes());
        list.resize(list.len() + count as usize, 0x00);
        for _ in 0..2 {
            assert!(matches!(
                Nbt::read_network(&mut &list[..]),
                Ok(Nbt::List(List::Compound(compounds))) if compounds.len() == count as usize
            ));
        }
        // Entries and strings count too: 3 MiB of 64 KiB strings in one compound.
        let mut strings = vec![0x0a];
        for name in 0..48_u8 {
            strings.extend_from_slice(&[0x08, 0x00, 0x01, b'0' + name, 0xff, 0xff]);
            strings.resize(strings.len() + 0xffff, b'a');
        }
        strings.push(0x00);
        assert_eq!(
            Nbt::read_network(&mut &strings[..]),
            Err(NbtError::TooLarge)
        );
    }

    #[test]
    fn optional_network_nbt_uses_tag_end_for_none() {
        let mut buf = Vec::new();
        Nbt::write_network_optional(None, &mut buf);
        Nbt::write_network_optional(Some(&Nbt::Byte(5)), &mut buf);
        assert_eq!(buf, [0x00, 0x01, 0x05]);
        let mut input = &buf[..];
        assert_eq!(Nbt::read_network_optional(&mut input), Ok(None));
        assert_eq!(
            Nbt::read_network_optional(&mut input),
            Ok(Some(Nbt::Byte(5)))
        );
        assert!(input.is_empty());
    }

    #[test]
    fn repeated_names_keep_the_last_value() {
        let bytes = [
            0x0a, 0x01, 0x00, 0x01, b'a', 0x01, 0x01, 0x00, 0x01, b'a', 0x02, 0x00,
        ];
        let Ok(Nbt::Compound(compound)) = Nbt::read_network(&mut &bytes[..]) else {
            panic!("not a compound");
        };
        assert_eq!(compound.len(), 1);
        assert_eq!(compound.get("a"), Some(&Nbt::Byte(2)));
    }
}
