//! The protocol schema: one directory per protocol version, one TOML file per connection state

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::Path;

use serde::Deserialize;

/// Connection states, in the order a connection goes through them.
pub const STATES: [&str; 5] = ["handshaking", "status", "login", "configuration", "play"];

/// Longest string vanilla reads when a field states no limit, in UTF-16 code units.
pub const DEFAULT_STRING_MAX: usize = 32767;

/// The schema of one protocol version.
#[derive(Debug)]
pub struct Version {
    /// Version name, such as `26.1`.
    pub minecraft: String,
    /// Protocol version number sent in the handshake.
    pub protocol: i32,
    /// States present in the schema, in connection order.
    pub states: Vec<State>,
}

/// The packets and types of one connection state.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    /// State name, such as `login`.
    #[serde(skip)]
    pub name: String,
    /// Packets sent by the server.
    #[serde(default)]
    pub clientbound: Vec<Packet>,
    /// Packets sent by the client.
    #[serde(default)]
    pub serverbound: Vec<Packet>,
    /// Structures used by the packets of this state.
    #[serde(default)]
    pub structs: BTreeMap<String, Struct>,
    /// Enumerations used by the packets of this state.
    #[serde(default)]
    pub enums: BTreeMap<String, Enum>,
    /// minecraft-data packets that are written by hand instead of generated.
    #[serde(default)]
    pub hand_written: Vec<HandWritten>,
}

/// Direction a packet travels in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// From the server to the client.
    Clientbound,
    /// From the client to the server.
    Serverbound,
}

impl Direction {
    /// Name used in the schema and in generated module names.
    pub fn name(self) -> &'static str {
        match self {
            Direction::Clientbound => "clientbound",
            Direction::Serverbound => "serverbound",
        }
    }

    /// Name minecraft-data uses for this direction.
    pub fn mcdata_name(self) -> &'static str {
        match self {
            Direction::Clientbound => "toClient",
            Direction::Serverbound => "toServer",
        }
    }
}

/// A packet.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Packet {
    /// Packet ID within its state and direction.
    pub id: i32,
    /// Rust type name.
    pub name: String,
    /// Name of the same packet in minecraft-data.
    pub mcdata: String,
    /// Documentation of the generated type.
    pub doc: String,
    /// Fields, in wire order.
    #[serde(default)]
    pub fields: Vec<Field>,
}

/// A structure used inside packets.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Struct {
    /// Documentation of the generated type.
    pub doc: String,
    /// Fields, in wire order.
    pub fields: Vec<Field>,
}

/// An enumeration sent as an integer.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Enum {
    /// Documentation of the generated type.
    pub doc: String,
    /// Integer type on the wire.
    #[serde(deserialize_with = "deserialize_prim")]
    pub repr: Prim,
    /// Variant names and their values.
    pub values: BTreeMap<String, i64>,
}

impl Enum {
    /// Variants ordered by value.
    pub fn variants(&self) -> Vec<(&str, i64)> {
        let mut variants: Vec<_> = self
            .values
            .iter()
            .map(|(name, value)| (name.as_str(), *value))
            .collect();
        variants.sort_by_key(|(_, value)| *value);
        variants
    }
}

/// A packet left out of generation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandWritten {
    /// Name of the packet in minecraft-data.
    pub mcdata: String,
    /// Direction of the packet.
    pub direction: Direction,
    /// Why it is written by hand.
    pub reason: String,
}

/// A field of a packet or structure.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    /// Rust field name.
    pub name: String,
    /// Wire type.
    #[serde(rename = "type", deserialize_with = "deserialize_type")]
    pub ty: Type,
    /// Documentation of the generated field.
    pub doc: Option<String>,
}

/// Fixed-size types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prim {
    /// `bool`, one byte that must be 0 or 1.
    Bool,
    /// `u8`.
    U8,
    /// `i8`.
    I8,
    /// `u16`, big endian.
    U16,
    /// `i16`, big endian.
    I16,
    /// `u32`, big endian.
    U32,
    /// `i32`, big endian.
    I32,
    /// `u64`, big endian.
    U64,
    /// `i64`, big endian.
    I64,
    /// `f32`, big endian.
    F32,
    /// `f64`, big endian.
    F64,
    /// `varint`: an `i32` in 1 to 5 LEB128 bytes.
    VarInt,
    /// `varlong`: an `i64` in 1 to 10 LEB128 bytes.
    VarLong,
    /// `uuid`: 16 bytes, big endian.
    Uuid,
}

impl Prim {
    const ALL: [Prim; 14] = [
        Prim::Bool,
        Prim::U8,
        Prim::I8,
        Prim::U16,
        Prim::I16,
        Prim::U32,
        Prim::I32,
        Prim::U64,
        Prim::I64,
        Prim::F32,
        Prim::F64,
        Prim::VarInt,
        Prim::VarLong,
        Prim::Uuid,
    ];

    /// Name in the schema, and in minecraft-data except for `uuid`.
    pub fn name(self) -> &'static str {
        match self {
            Prim::Bool => "bool",
            Prim::U8 => "u8",
            Prim::I8 => "i8",
            Prim::U16 => "u16",
            Prim::I16 => "i16",
            Prim::U32 => "u32",
            Prim::I32 => "i32",
            Prim::U64 => "u64",
            Prim::I64 => "i64",
            Prim::F32 => "f32",
            Prim::F64 => "f64",
            Prim::VarInt => "varint",
            Prim::VarLong => "varlong",
            Prim::Uuid => "uuid",
        }
    }

    /// Looks a type up by its schema name.
    pub fn from_name(name: &str) -> Option<Prim> {
        Prim::ALL.into_iter().find(|prim| prim.name() == name)
    }

    /// Rust type the value decodes to.
    pub fn rust_type(self) -> &'static str {
        match self {
            Prim::VarInt => "i32",
            Prim::VarLong => "i64",
            Prim::Uuid => "u128",
            other => other.name(),
        }
    }

    /// Whether the type can carry enum values, which are converted to `i64`.
    pub fn is_enum_repr(self) -> bool {
        !matches!(
            self,
            Prim::Bool | Prim::U64 | Prim::F32 | Prim::F64 | Prim::Uuid
        )
    }
}

/// Wire type of a field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    /// A fixed-size type.
    Prim(Prim),
    /// `string(max)`: UTF-8 with a VarInt byte length, at most `max` UTF-16 code units.
    String {
        /// Longest accepted string.
        max: usize,
    },
    /// `bytes(max)`: a byte array with a VarInt length.
    Bytes {
        /// Longest accepted array; without one, the rest of the packet bounds it.
        max: Option<usize>,
    },
    /// `rest(max)`: every byte left in the packet.
    Rest {
        /// Longest accepted array; without one, the rest of the packet bounds it.
        max: Option<usize>,
    },
    /// `option(T)`: a `bool`, then the value when it is true.
    Option(Box<Type>),
    /// `array(max, T)`: a VarInt count, then the elements.
    Array {
        /// Largest accepted count; without one, the rest of the packet bounds it.
        max: Option<usize>,
        /// Element type.
        item: Box<Type>,
    },
    /// A structure or enumeration of the same state.
    Named(String),
}

impl Type {
    /// Parses a type expression such as `array(16, option(string(64)))`.
    pub fn parse(text: &str) -> Result<Type, String> {
        let mut parser = TypeParser { text, pos: 0 };
        let ty = parser.ty()?;
        parser.skip_spaces();
        if parser.pos != text.len() {
            return Err(parser.error("unexpected text after the type"));
        }
        Ok(ty)
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let limit = |max: &Option<usize>| max.map(|max| format!("({max})")).unwrap_or_default();
        match self {
            Type::Prim(prim) => f.write_str(prim.name()),
            Type::String { max } => write!(f, "string({max})"),
            Type::Bytes { max } => write!(f, "bytes{}", limit(max)),
            Type::Rest { max } => write!(f, "rest{}", limit(max)),
            Type::Option(inner) => write!(f, "option({inner})"),
            Type::Array {
                max: Some(max),
                item,
            } => write!(f, "array({max}, {item})"),
            Type::Array { max: None, item } => write!(f, "array({item})"),
            Type::Named(name) => f.write_str(name),
        }
    }
}

struct TypeParser<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> TypeParser<'a> {
    fn ty(&mut self) -> Result<Type, String> {
        let name = self.word();
        if name.is_empty() {
            return Err(self.error("expected a type name"));
        }
        let mut args = Vec::new();
        if self.eat('(') {
            loop {
                self.skip_spaces();
                let rest = &self.text[self.pos..];
                if rest.starts_with(|c: char| c.is_ascii_digit()) {
                    let digits = self.word();
                    let number = digits.parse().map_err(|_| self.error("invalid number"))?;
                    args.push(Arg::Number(number));
                } else {
                    args.push(Arg::Type(self.ty()?));
                }
                if self.eat(')') {
                    break;
                }
                if !self.eat(',') {
                    return Err(self.error("expected `,` or `)`"));
                }
            }
        }
        let ty = match (name, args.as_slice()) {
            ("string", []) => Type::String {
                max: DEFAULT_STRING_MAX,
            },
            ("string", [Arg::Number(max)]) => Type::String { max: *max },
            ("bytes", []) => Type::Bytes { max: None },
            ("bytes", [Arg::Number(max)]) => Type::Bytes { max: Some(*max) },
            ("rest", []) => Type::Rest { max: None },
            ("rest", [Arg::Number(max)]) => Type::Rest { max: Some(*max) },
            ("option", [Arg::Type(inner)]) => Type::Option(Box::new(inner.clone())),
            ("array", [Arg::Type(item)]) => Type::Array {
                max: None,
                item: Box::new(item.clone()),
            },
            ("array", [Arg::Number(max), Arg::Type(item)]) => Type::Array {
                max: Some(*max),
                item: Box::new(item.clone()),
            },
            (name, []) => match Prim::from_name(name) {
                Some(prim) => Type::Prim(prim),
                None if is_pascal_case(name) => Type::Named(name.to_owned()),
                None => return Err(format!("unknown type `{name}`")),
            },
            (name, _) => return Err(format!("wrong arguments for `{name}`")),
        };
        Ok(ty)
    }

    fn word(&mut self) -> &'a str {
        self.skip_spaces();
        let start = self.pos;
        let len = self.text[start..]
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(self.text.len() - start);
        self.pos += len;
        &self.text[start..self.pos]
    }

    fn eat(&mut self, c: char) -> bool {
        self.skip_spaces();
        let found = self.text[self.pos..].starts_with(c);
        if found {
            self.pos += c.len_utf8();
        }
        found
    }

    fn skip_spaces(&mut self) {
        self.pos = self.text.len() - self.text[self.pos..].trim_start().len();
    }

    fn error(&self, message: &str) -> String {
        format!("{message} at column {} of `{}`", self.pos + 1, self.text)
    }
}

enum Arg {
    Number(usize),
    Type(Type),
}

fn deserialize_type<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Type, D::Error> {
    let text = String::deserialize(deserializer)?;
    Type::parse(&text).map_err(serde::de::Error::custom)
}

fn deserialize_prim<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Prim, D::Error> {
    let text = String::deserialize(deserializer)?;
    Prim::from_name(&text).ok_or_else(|| serde::de::Error::custom(format!("unknown type `{text}`")))
}

impl Version {
    /// Loads and validates the schema in `dir`: `version.toml` and one file per state.
    pub fn load(dir: &Path) -> Result<Version, String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Header {
            minecraft: String,
            protocol: i32,
        }

        let read = |name: &str| {
            let path = dir.join(name);
            fs::read_to_string(&path).map(|text| (path, text))
        };
        let (path, text) = read("version.toml")
            .map_err(|e| format!("{}: {e}", dir.join("version.toml").display()))?;
        let header: Header =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut states = Vec::new();
        for name in STATES {
            let Ok((path, text)) = read(&format!("{name}.toml")) else {
                continue;
            };
            let mut state: State =
                toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            state.name = name.to_owned();
            state
                .validate()
                .map_err(|e| format!("{}: {e}", path.display()))?;
            states.push(state);
        }
        Ok(Version {
            minecraft: header.minecraft,
            protocol: header.protocol,
            states,
        })
    }

    /// Rust module name, such as `v26_1`.
    pub fn module_name(&self) -> String {
        format!("v{}", self.minecraft.replace('.', "_"))
    }
}

impl State {
    /// Packets of one direction.
    pub fn packets(&self, direction: Direction) -> &[Packet] {
        match direction {
            Direction::Clientbound => &self.clientbound,
            Direction::Serverbound => &self.serverbound,
        }
    }

    fn validate(&self) -> Result<(), String> {
        let mut type_names = BTreeSet::new();
        for (name, item) in &self.structs {
            check_type_name(name)?;
            check_fields(self, name, &item.fields)?;
            type_names.insert(name.as_str());
        }
        for (name, item) in &self.enums {
            check_type_name(name)?;
            if !type_names.insert(name.as_str()) {
                return Err(format!("`{name}` is both a struct and an enum"));
            }
            if !item.repr.is_enum_repr() {
                return Err(format!(
                    "enum `{name}` cannot be sent as `{}`",
                    item.repr.name()
                ));
            }
            for variant in item.values.keys() {
                check_type_name(variant)?;
            }
        }
        for direction in [Direction::Clientbound, Direction::Serverbound] {
            let mut ids = BTreeSet::new();
            let mut names = BTreeSet::new();
            for packet in self.packets(direction) {
                check_type_name(&packet.name)?;
                if packet.name == "Packet" || type_names.contains(packet.name.as_str()) {
                    return Err(format!("packet name `{}` is already used", packet.name));
                }
                if packet.id < 0 || !ids.insert(packet.id) {
                    return Err(format!(
                        "{} packet ID {:#04x} is negative or repeated",
                        direction.name(),
                        packet.id
                    ));
                }
                if !names.insert(packet.name.as_str()) {
                    return Err(format!(
                        "{} packet `{}` is repeated",
                        direction.name(),
                        packet.name
                    ));
                }
                if packet.doc.trim().is_empty() {
                    return Err(format!("packet `{}` has no doc", packet.name));
                }
                check_fields(self, &packet.name, &packet.fields)?;
            }
        }
        Ok(())
    }

    /// Whether `name` is an enumeration of this state.
    pub fn is_enum(&self, name: &str) -> bool {
        self.enums.contains_key(name)
    }
}

fn check_fields(state: &State, owner: &str, fields: &[Field]) -> Result<(), String> {
    let mut names = BTreeSet::new();
    for field in fields {
        let valid = field.name.starts_with(|c: char| c.is_ascii_lowercase())
            && field
                .name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !valid {
            return Err(format!("field `{owner}.{}` is not snake_case", field.name));
        }
        if !names.insert(field.name.as_str()) {
            return Err(format!("field `{owner}.{}` is repeated", field.name));
        }
        check_type_refs(state, owner, &field.ty)?;
    }
    Ok(())
}

fn check_type_refs(state: &State, owner: &str, ty: &Type) -> Result<(), String> {
    match ty {
        Type::Named(name) if name == owner => Err(format!("`{owner}` contains itself")),
        Type::Named(name)
            if !state.structs.contains_key(name) && !state.enums.contains_key(name) =>
        {
            Err(format!(
                "`{owner}` uses `{name}`, which is not defined in this state"
            ))
        }
        Type::Option(inner) | Type::Array { item: inner, .. } => {
            check_type_refs(state, owner, inner)
        }
        _ => Ok(()),
    }
}

fn check_type_name(name: &str) -> Result<(), String> {
    if is_pascal_case(name) {
        Ok(())
    } else {
        Err(format!("`{name}` is not PascalCase"))
    }
}

fn is_pascal_case(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_uppercase())
        && name.chars().all(|c| c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_type_expressions() {
        let parsed = Type::parse("array(16, option( string(64) ))").unwrap();
        let expected = Type::Array {
            max: Some(16),
            item: Box::new(Type::Option(Box::new(Type::String { max: 64 }))),
        };
        assert_eq!(parsed, expected);
        assert_eq!(parsed.to_string(), "array(16, option(string(64)))");
        assert_eq!(
            Type::parse("string").unwrap(),
            Type::String {
                max: DEFAULT_STRING_MAX
            }
        );
        assert_eq!(Type::parse("bytes").unwrap(), Type::Bytes { max: None });
        assert_eq!(Type::parse("varint").unwrap(), Type::Prim(Prim::VarInt));
        assert_eq!(
            Type::parse("Property").unwrap(),
            Type::Named("Property".into())
        );
    }

    #[test]
    fn rejects_bad_type_expressions() {
        for text in [
            "",
            "strin",
            "string(",
            "string(a)",
            "option()",
            "option(16)",
            "array(16)",
            "u16 u8",
            "varint(1)",
        ] {
            assert!(Type::parse(text).is_err(), "`{text}` was accepted");
        }
    }

    #[test]
    fn rejects_undefined_types_and_repeated_ids() {
        let undefined = "[[serverbound]]\nid = 0\nname = \"A\"\nmcdata = \"a\"\ndoc = \"A.\"\nfields = [{ name = \"x\", type = \"Missing\" }]\n";
        let state: State = toml::from_str(undefined).unwrap();
        assert!(state.validate().unwrap_err().contains("Missing"));

        let repeated = "[[serverbound]]\nid = 0\nname = \"A\"\nmcdata = \"a\"\ndoc = \"A.\"\n\n[[serverbound]]\nid = 0\nname = \"B\"\nmcdata = \"b\"\ndoc = \"B.\"\n";
        let state: State = toml::from_str(repeated).unwrap();
        assert!(state.validate().unwrap_err().contains("repeated"));
    }
}
