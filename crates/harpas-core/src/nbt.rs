//! NBT values and their Java Edition binary form
//!
//! Java Edition writes NBT big endian, with strings in Java's modified UTF-8. Files, such as chunks in region
//! files and `level.dat`, store a tag type, the root's name and its payload. Network NBT, used in packets since
//! Java 1.20.2, is a tag type and its payload with no root name. Lists are stored by element type, so a
//! value can never hold a list that mixes types, which the format cannot express.
//!
//! Readers take untrusted input: every length is checked against the bytes left before allocating, and
//! nesting is limited to [`MAX_DEPTH`], as in vanilla. Network NBT is also limited in the memory its value
//! takes, to [`NETWORK_QUOTA`]: a list of empty compounds is one byte each on the wire and far more in memory.
//! Files have no quota in vanilla; [`Nbt::read_named_within`] lets their reader set one.

use std::error::Error;
use std::fmt;

use indexmap::IndexMap;

/// Deepest nesting of lists and compounds accepted, as vanilla's `NbtAccounter`.
pub const MAX_DEPTH: usize = 512;

/// Most memory a network NBT value may take once read, as the quota of vanilla's `NbtAccounter` for packets:
/// 2 MiB.
pub const NETWORK_QUOTA: usize = 2 * 1024 * 1024;

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

/// Why NBT could not be read or written.
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
    /// The encoded name or string exceeds its unsigned 16-bit length field.
    StringTooLong,
    /// The value would take more memory than its reader allows: [`NETWORK_QUOTA`] for a network value.
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
            NbtError::StringTooLong => {
                f.write_str("NBT string exceeds 65,535 bytes in modified UTF-8")
            }
            NbtError::TooLarge => f.write_str("NBT takes more memory than allowed"),
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

    /// Reads NBT as files store it: a tag type, the root's name, then its payload. Returns the name and the value.
    /// The value has no memory quota, as in vanilla.
    pub fn read_named(input: &mut &[u8]) -> Result<(String, Nbt), NbtError> {
        Self::read_named_within(input, usize::MAX)
    }

    /// [`Nbt::read_named`] for a value that may take at most `quota` bytes of memory, for files that may not
    /// come from the server's owner, such as a downloaded world.
    pub fn read_named_within(input: &mut &[u8], quota: usize) -> Result<(String, Nbt), NbtError> {
        match read_u8(input)? {
            END => Err(NbtError::EndRoot),
            id => {
                let budget = &mut Budget(quota);
                let name = read_string(input, budget)?;
                Ok((name, read_payload(input, budget, id, 0)?))
            }
        }
    }

    /// Writes NBT as files store it, with `name` as the root's name; vanilla names the root of its files `""`.
    ///
    /// Returns an error without appending anything if the value cannot be represented.
    pub fn write_named(&self, name: &str, buf: &mut Vec<u8>) -> Result<(), NbtError> {
        self.write_named_within(name, buf, usize::MAX, usize::MAX)
    }

    /// Writes named NBT within both an encoded byte limit and a decoded memory quota.
    pub fn write_named_within(
        &self,
        name: &str,
        buf: &mut Vec<u8>,
        max_bytes: usize,
        quota: usize,
    ) -> Result<(), NbtError> {
        write_root(buf, self.tag_id(), Some(name), max_bytes, quota, |size| {
            self.measure(size, 0)
        })?;
        self.write_payload(buf);
        Ok(())
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
    pub fn write_network_optional(value: Option<&Nbt>, buf: &mut Vec<u8>) -> Result<(), NbtError> {
        match value {
            Some(value) => value.write_network(buf),
            None => {
                buf.try_reserve(1).map_err(|_| NbtError::TooLarge)?;
                buf.push(END);
                Ok(())
            }
        }
    }

    /// Writes network NBT.
    ///
    /// Returns an error without appending anything if the value cannot be represented.
    pub fn write_network(&self, buf: &mut Vec<u8>) -> Result<(), NbtError> {
        write_root(buf, self.tag_id(), None, usize::MAX, usize::MAX, |size| {
            self.measure(size, 0)
        })?;
        self.write_payload(buf);
        Ok(())
    }

    fn measure(&self, size: &mut WriteSize, depth: usize) -> Result<(), NbtError> {
        match self {
            Nbt::Byte(_) => size.bytes.charge(1),
            Nbt::Short(_) => size.bytes.charge(2),
            Nbt::Int(_) | Nbt::Float(_) => size.bytes.charge(4),
            Nbt::Long(_) | Nbt::Double(_) => size.bytes.charge(8),
            Nbt::ByteArray(v) => size.array(v.len(), 1),
            Nbt::IntArray(v) => size.array(v.len(), 4),
            Nbt::LongArray(v) => size.array(v.len(), 8),
            Nbt::String(v) => size.string(v),
            Nbt::List(v) => v.measure(size, depth + 1),
            Nbt::Compound(v) => v.measure(size, depth + 1),
        }
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

    fn measure(&self, size: &mut WriteSize, depth: usize) -> Result<(), NbtError> {
        check_depth(depth)?;
        size.bytes.charge(1)?;
        match self {
            List::Empty => size.bytes.charge(4),
            List::Byte(v) => size.array(v.len(), 1),
            List::Short(v) => size.array(v.len(), 2),
            List::Int(v) => size.array(v.len(), 4),
            List::Long(v) => size.array(v.len(), 8),
            List::Float(v) => size.array(v.len(), 4),
            List::Double(v) => size.array(v.len(), 8),
            List::ByteArray(v) => size.elements(v, |size, v| size.array(v.len(), 1)),
            List::IntArray(v) => size.elements(v, |size, v| size.array(v.len(), 4)),
            List::LongArray(v) => size.elements(v, |size, v| size.array(v.len(), 8)),
            List::String(v) => size.elements(v, |size, v| size.string(v)),
            List::List(v) => size.elements(v, |size, v| v.measure(size, depth + 1)),
            List::Compound(v) => size.elements(v, |size, v| v.measure(size, depth + 1)),
        }
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
    /// Writes this compound as a named root within the byte and decoded memory limits.
    pub fn write_named_within(
        &self,
        name: &str,
        buf: &mut Vec<u8>,
        max_bytes: usize,
        quota: usize,
    ) -> Result<(), NbtError> {
        write_root(buf, COMPOUND, Some(name), max_bytes, quota, |size| {
            self.measure(size, 1)
        })?;
        self.write_payload(buf);
        Ok(())
    }

    fn measure(&self, size: &mut WriteSize, depth: usize) -> Result<(), NbtError> {
        check_depth(depth)?;
        size.bytes.charge(1)?;
        size.memory
            .charge(compound_bytes(compound_capacity(self.len())))?;
        for (name, value) in self.iter() {
            size.bytes.charge(1)?;
            size.string(name)?;
            value.measure(size, depth)?;
        }
        Ok(())
    }
    fn write_payload(&self, buf: &mut Vec<u8>) {
        for (name, value) in &self.0 {
            buf.push(value.tag_id());
            write_string(buf, name);
            value.write_payload(buf);
        }
        buf.push(END);
    }
}

struct WriteSize {
    bytes: Budget,
    memory: Budget,
}

fn write_root(
    buf: &mut Vec<u8>,
    id: u8,
    name: Option<&str>,
    max_bytes: usize,
    quota: usize,
    measure: impl FnOnce(&mut WriteSize) -> Result<(), NbtError>,
) -> Result<(), NbtError> {
    let mut size = WriteSize::new(max_bytes, quota);
    size.bytes.charge(1)?;
    if let Some(name) = name {
        size.string(name)?;
    }
    measure(&mut size)?;
    buf.try_reserve(max_bytes - size.bytes.0)
        .map_err(|_| NbtError::TooLarge)?;
    buf.push(id);
    if let Some(name) = name {
        write_string(buf, name);
    }
    Ok(())
}

impl WriteSize {
    fn new(bytes: usize, memory: usize) -> Self {
        Self {
            bytes: Budget(bytes),
            memory: Budget(memory),
        }
    }

    fn string(&mut self, value: &str) -> Result<(), NbtError> {
        u16::try_from(value.len()).map_err(|_| NbtError::StringTooLong)?;
        let len = value
            .encode_utf16()
            .map(|unit| match unit {
                1..=0x7f => 1,
                0 | 0x80..=0x7ff => 2,
                _ => 3,
            })
            .sum::<usize>();
        u16::try_from(len).map_err(|_| NbtError::StringTooLong)?;
        self.memory.charge(value.len())?;
        self.bytes.charge(2 + len)
    }

    fn array(&mut self, len: usize, width: usize) -> Result<(), NbtError> {
        let bytes = self.sequence(len, width)?;
        self.bytes.charge(bytes)
    }

    fn elements<T>(
        &mut self,
        values: &[T],
        mut measure: impl FnMut(&mut Self, &T) -> Result<(), NbtError>,
    ) -> Result<(), NbtError> {
        self.sequence(values.len(), size_of::<T>())?;
        for value in values {
            measure(self, value)?;
        }
        Ok(())
    }

    fn sequence(&mut self, len: usize, width: usize) -> Result<usize, NbtError> {
        i32::try_from(len).map_err(|_| NbtError::TooLarge)?;
        let bytes = len.checked_mul(width).ok_or(NbtError::TooLarge)?;
        self.memory.charge(bytes)?;
        self.bytes.charge(4)?;
        Ok(bytes)
    }
}

fn check_depth(depth: usize) -> Result<(), NbtError> {
    if depth > MAX_DEPTH {
        Err(NbtError::TooDeep)
    } else {
        Ok(())
    }
}

fn compound_capacity(len: usize) -> usize {
    if len == 0 { 0 } else { len.next_power_of_two() }
}

fn compound_bytes(capacity: usize) -> usize {
    if capacity == 0 {
        return 0;
    }
    // IndexMap holds a dense entry array and a separate hash index with spare buckets.
    let entries =
        capacity.saturating_mul(size_of::<String>() + size_of::<Nbt>() + size_of::<usize>());
    let buckets = capacity.saturating_mul(2).max(4);
    entries
        .saturating_add(buckets.saturating_mul(size_of::<usize>() + 1))
        .saturating_add(32)
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
    check_depth(depth)?;
    let id = read_u8(input)?;
    let len = read_len(input)?;
    let read_i32 = |input: &mut &[u8], _: &mut Budget| take_array(input).map(i32::from_be_bytes);
    Ok(match id {
        END if len != 0 => return Err(NbtError::EndList),
        BYTE => List::Byte(read_elements(input, budget, len, 1, |input, _| {
            Ok(read_u8(input)? as i8)
        })?),
        SHORT => List::Short(read_elements(input, budget, len, 2, |input, _| {
            take_array(input).map(i16::from_be_bytes)
        })?),
        INT => List::Int(read_elements(input, budget, len, 4, read_i32)?),
        LONG => List::Long(read_elements(input, budget, len, 8, |input, _| {
            take_array(input).map(i64::from_be_bytes)
        })?),
        FLOAT => List::Float(read_elements(input, budget, len, 4, |input, _| {
            take_array(input).map(f32::from_be_bytes)
        })?),
        DOUBLE => List::Double(read_elements(input, budget, len, 8, |input, _| {
            take_array(input).map(f64::from_be_bytes)
        })?),
        BYTE_ARRAY => List::ByteArray(read_elements(input, budget, len, 4, read_byte_array)?),
        STRING => List::String(read_elements(input, budget, len, 2, read_string)?),
        LIST => List::List(read_elements(input, budget, len, 5, |input, budget| {
            read_list(input, budget, depth + 1)
        })?),
        COMPOUND => List::Compound(read_elements(input, budget, len, 1, |input, budget| {
            read_compound(input, budget, depth + 1)
        })?),
        INT_ARRAY => List::IntArray(read_elements(input, budget, len, 4, read_int_array)?),
        LONG_ARRAY => List::LongArray(read_elements(input, budget, len, 4, read_long_array)?),
        _ if len == 0 => List::Empty,
        other => return Err(NbtError::UnknownTag(other)),
    })
}

fn read_compound(
    input: &mut &[u8],
    budget: &mut Budget,
    depth: usize,
) -> Result<Compound, NbtError> {
    check_depth(depth)?;
    let mut compound = Compound::new();
    loop {
        let id = read_u8(input)?;
        if id == END {
            return Ok(compound);
        }
        let name = read_string(input, budget)?;
        let value = read_payload(input, budget, id, depth)?;
        let entries = &mut compound.0;
        let capacity = entries.capacity();
        if entries.len() == capacity && !entries.contains_key(&name) {
            let next = compound_capacity(entries.len() + 1);
            budget.charge(compound_bytes(next) - compound_bytes(capacity))?;
            entries
                .try_reserve_exact(next - entries.len())
                .map_err(|_| NbtError::TooLarge)?;
        }
        // A repeated name replaces the earlier value, as in vanilla's map.
        entries.insert(name, value);
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
    read: impl FnMut(&mut &[u8], &mut Budget) -> Result<T, NbtError>,
) -> Result<Vec<T>, NbtError> {
    let len = read_len(input)?;
    read_elements(input, budget, len, min_size, read)
}

fn read_len(input: &mut &[u8]) -> Result<usize, NbtError> {
    let len = i32::from_be_bytes(take_array(input)?);
    usize::try_from(len).map_err(|_| NbtError::NegativeLength(len))
}

fn read_elements<T>(
    input: &mut &[u8],
    budget: &mut Budget,
    len: usize,
    min_size: usize,
    mut read: impl FnMut(&mut &[u8], &mut Budget) -> Result<T, NbtError>,
) -> Result<Vec<T>, NbtError> {
    if len.saturating_mul(min_size) > input.len() {
        return Err(NbtError::UnexpectedEnd);
    }
    budget.charge(len.saturating_mul(size_of::<T>()))?;
    let mut items = Vec::new();
    if min_size == size_of::<T>() {
        items
            .try_reserve_exact(len)
            .map_err(|_| NbtError::TooLarge)?;
    }
    for _ in 0..len {
        let item = read(input, budget)?;
        if items.len() == items.capacity() {
            // Variable-width entries may end early even if their minimum widths fit.
            let additional = items.len().max(16).min(len - items.len());
            items
                .try_reserve_exact(additional)
                .map_err(|_| NbtError::TooLarge)?;
        }
        items.push(item);
    }
    budget.charge((items.capacity() - len).saturating_mul(size_of::<T>()))?;
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
    let (bytes, rest) = input.split_at(len);
    *input = rest;
    decode_mutf8(bytes, budget)
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

fn decode_mutf8(bytes: &[u8], budget: &mut Budget) -> Result<String, NbtError> {
    // Most strings are ASCII without U+0000, where both encodings agree.
    if bytes.iter().all(|&b| (1..0x80).contains(&b)) {
        budget.charge(bytes.len())?;
        let mut value = String::new();
        value
            .try_reserve_exact(bytes.len())
            .map_err(|_| NbtError::TooLarge)?;
        value.push_str(std::str::from_utf8(bytes).expect("ASCII is UTF-8"));
        return Ok(value);
    }
    let continuation = |b: Option<&u8>| match b {
        Some(&b) if b & 0xc0 == 0x80 => Ok(u16::from(b & 0x3f)),
        _ => Err(NbtError::InvalidString),
    };
    let mut units = Vec::new();
    units
        .try_reserve_exact(bytes.len())
        .map_err(|_| NbtError::TooLarge)?;
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
    let chars = || char::decode_utf16(units.iter().copied());
    let len = chars().try_fold(0, |len, c| {
        c.map(|c| len + c.len_utf8())
            .map_err(|_| NbtError::InvalidString)
    })?;
    budget.charge(len)?;
    let mut value = String::new();
    value
        .try_reserve_exact(len)
        .map_err(|_| NbtError::TooLarge)?;
    value.extend(chars().map(|c| c.expect("already validated")));
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(value: &Nbt) -> Vec<u8> {
        let mut buf = Vec::new();
        value.write_network(&mut buf).unwrap();
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
    fn file_nbt_has_a_root_name() {
        // The same example with the root name "hello world" that the page gives it.
        let value = Nbt::Compound([("name", "Bananrama")].into_iter().collect());
        let mut buf = Vec::new();
        value.write_named("hello world", &mut buf).unwrap();
        let mut expected = vec![0x0a, 0x00, 0x0b];
        expected.extend_from_slice(b"hello world");
        expected.extend_from_slice(&[0x08, 0x00, 0x04]);
        expected.extend_from_slice(b"name");
        expected.extend_from_slice(&[0x00, 0x09]);
        expected.extend_from_slice(b"Bananrama");
        expected.push(0x00);
        assert_eq!(buf, expected);
        assert_eq!(
            Nbt::read_named(&mut &buf[..]),
            Ok(("hello world".to_owned(), value))
        );
        assert_eq!(Nbt::read_named(&mut &[0u8][..]), Err(NbtError::EndRoot));
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
        Nbt::from("\0ñ🦀").write_network(&mut buf).unwrap();
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
    fn failed_writes_leave_the_buffer_unchanged() {
        let mut buf = vec![42];
        let oversized = Nbt::from("\0".repeat(32_768));
        assert_eq!(
            oversized.write_named("", &mut buf),
            Err(NbtError::StringTooLong)
        );
        let mut list = List::Empty;
        for _ in 0..MAX_DEPTH {
            list = List::List(vec![list]);
        }
        assert_eq!(
            Nbt::List(list).write_network(&mut buf),
            Err(NbtError::TooDeep)
        );
        let value = Nbt::Compound(Compound::from_iter([("name", "value")]));
        assert_eq!(
            value.write_named_within("", &mut buf, 5, usize::MAX),
            Err(NbtError::TooLarge)
        );
        assert_eq!(
            value.write_named_within("", &mut buf, 100, 1),
            Err(NbtError::TooLarge)
        );
        assert_eq!(buf, [42]);

        let quota = compound_bytes(1) + "namevalue".len();
        value.write_named_within("", &mut buf, 100, quota).unwrap();
        assert_eq!(
            Nbt::read_named_within(&mut &buf[1..], quota),
            Ok((String::new(), value))
        );
    }

    #[test]
    fn an_empty_list_needs_no_element_decoder() {
        assert_eq!(
            Nbt::read_network(&mut &[LIST, 255, 0, 0, 0, 0][..]),
            Ok(Nbt::List(List::Empty))
        );
        assert_eq!(
            Nbt::read_network(&mut &[LIST, 255, 0, 0, 0, 1][..]),
            Err(NbtError::UnknownTag(255))
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
        // Files have no quota, as in vanilla, unless their reader sets one.
        let mut file = vec![0x09, 0x00, 0x00];
        file.extend_from_slice(&bomb[1..]);
        assert!(Nbt::read_named(&mut &file[..]).is_ok());
        assert_eq!(
            Nbt::read_named_within(&mut &file[..], NETWORK_QUOTA),
            Err(NbtError::TooLarge)
        );
    }

    #[test]
    fn optional_network_nbt_uses_tag_end_for_none() {
        let mut buf = Vec::new();
        Nbt::write_network_optional(None, &mut buf).unwrap();
        Nbt::write_network_optional(Some(&Nbt::Byte(5)), &mut buf).unwrap();
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
