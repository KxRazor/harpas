//! Comparison of the schema with minecraft-data's `protocol.json`
//!
//! Field names and limits are Harpas' own, so the comparison covers what decides the bytes on the wire:
//! which packets exist, their IDs, and the sequence of wire types in each one. Enums are compared by their
//! values where minecraft-data has a mapper, and tagged unions against minecraft-data's switches on an
//! earlier field of the same container. Packets with hand-written types are compared by ID only.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_json::Value;

use crate::schema::{Direction, Prim, STATES, State, Type, Version};

/// Result of a comparison.
#[derive(Debug, Default)]
pub struct Report {
    /// Differences that make the schema disagree with minecraft-data.
    pub errors: Vec<String>,
    /// States minecraft-data has and the schema does not cover yet.
    pub missing_states: Vec<String>,
    /// minecraft-data packets not in a state the schema marks as partial.
    pub pending: Vec<String>,
    /// Packets compared by ID only, because they use hand-written types.
    pub unchecked: Vec<String>,
}

/// Compares `version` with a parsed `protocol.json`.
pub fn check(version: &Version, protocol: &Value) -> Report {
    let mut report = Report::default();
    let global = &protocol["types"];
    for name in STATES {
        let Some(state) = version.states.iter().find(|state| state.name == name) else {
            if protocol.get(name).is_some() {
                report.missing_states.push(name.to_owned());
            }
            continue;
        };
        let Some(mcdata_state) = protocol.get(name) else {
            report.errors.push(format!("{name}: not in minecraft-data"));
            continue;
        };
        for direction in [Direction::Clientbound, Direction::Serverbound] {
            let types = &mcdata_state[direction.mcdata_name()]["types"];
            let resolver = Resolver {
                local: types,
                global,
            };
            check_direction(state, direction, &resolver, &mut report);
        }
    }
    report
}

fn check_direction(
    state: &State,
    direction: Direction,
    resolver: &Resolver<'_>,
    report: &mut Report,
) {
    let errors = &mut report.errors;
    let context = format!("{} {}", state.name, direction.name());
    let packets = match mcdata_packets(resolver.local) {
        Ok(packets) => packets,
        Err(e) => return errors.push(format!("{context}: {e}")),
    };
    for packet in state.packets(direction) {
        let Some(found) = packets.iter().find(|found| found.name == packet.mcdata) else {
            errors.push(format!(
                "{context} {}: no `{}` packet in minecraft-data",
                packet.name, packet.mcdata
            ));
            continue;
        };
        if found.id != packet.id {
            errors.push(format!(
                "{context} {}: ID {:#04x}, minecraft-data has {:#04x}",
                packet.name, packet.id, found.id
            ));
        }
        let ours = Wire::seq(
            packet
                .fields
                .iter()
                .map(|field| Wire::from_schema(state, &field.ty))
                .collect(),
        );
        if ours.has_extern() {
            report.unchecked.push(format!("{context} {}", packet.name));
            continue;
        }
        match resolver.resolve(&Value::String(found.type_name.clone())) {
            Ok(theirs) if ours.matches(&theirs) => {}
            Ok(theirs) => errors.push(format!(
                "{context} {}: wire types are {ours}, minecraft-data has {theirs}",
                packet.name
            )),
            Err(e) => errors.push(format!("{context} {}: {e}", packet.name)),
        }
    }
    for found in &packets {
        let generated = state
            .packets(direction)
            .iter()
            .any(|packet| packet.mcdata == found.name);
        let hand_written = state
            .hand_written
            .iter()
            .any(|h| h.direction == direction && h.mcdata == found.name);
        if !generated && !hand_written && state.partial {
            report
                .pending
                .push(format!("{context} {} ({:#04x})", found.name, found.id));
        } else if !generated && !hand_written {
            errors.push(format!(
                "{context}: minecraft-data packet `{}` ({:#04x}) is not in the schema",
                found.name, found.id
            ));
        }
    }
}

struct McdataPacket {
    id: i32,
    name: String,
    type_name: String,
}

/// Reads the packet list from the `packet` container: an ID mapper, then a switch from names to types.
fn mcdata_packets(types: &Value) -> Result<Vec<McdataPacket>, String> {
    let fields = types["packet"][1]
        .as_array()
        .ok_or("no `packet` container")?;
    let mappings = fields
        .first()
        .and_then(|f| f["type"][1]["mappings"].as_object())
        .ok_or("no packet ID mapper")?;
    let switch = fields
        .get(1)
        .and_then(|f| f["type"][1]["fields"].as_object())
        .ok_or("no packet switch")?;
    let mut packets = Vec::new();
    for (id, name) in mappings {
        let name = name.as_str().ok_or("packet name is not a string")?;
        let parsed = id
            .strip_prefix("0x")
            .and_then(|hex| i32::from_str_radix(hex, 16).ok());
        let id = parsed.ok_or_else(|| format!("invalid packet ID `{id}`"))?;
        let type_name = switch
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("no type for `{name}`"))?;
        packets.push(McdataPacket {
            id,
            name: name.to_owned(),
            type_name: type_name.to_owned(),
        });
    }
    Ok(packets)
}

/// The shape of a type on the wire, which both formats reduce to.
#[derive(Debug, PartialEq, Eq)]
enum Wire {
    Prim(Prim),
    String,
    Bytes,
    Rest,
    Nbt,
    OptionalNbt,
    Fixed(usize),
    LpVec3,
    Holder(Box<Wire>),
    /// An integer from a known set of values: an enum in the schema, a mapper in minecraft-data.
    Enum(Prim, BTreeSet<i64>),
    /// Bit flags: the integer and the bits that have a flag.
    Flags(Prim, BTreeSet<u32>),
    /// Bit fields, from the most significant: their size and whether they are signed.
    Bitfield(Vec<(u32, bool)>),
    Option(Box<Wire>),
    Array(Box<Wire>),
    Seq(Vec<Wire>),
    /// A tag, the wire types its values select, and the wire type of any other value.
    Union(Prim, BTreeMap<i64, Wire>, Option<Box<Wire>>),
    /// A hand-written type of the schema, which matches anything.
    Extern(String),
}

impl Wire {
    fn from_schema(state: &State, ty: &Type) -> Wire {
        let fields = |fields: &[crate::schema::Field]| {
            Wire::seq(
                fields
                    .iter()
                    .map(|f| Wire::from_schema(state, &f.ty))
                    .collect(),
            )
        };
        match ty {
            Type::Prim(prim) => Wire::Prim(*prim),
            Type::String { .. } => Wire::String,
            Type::Bytes { .. } => Wire::Bytes,
            Type::Rest { .. } => Wire::Rest,
            Type::Nbt => Wire::Nbt,
            Type::OptionalNbt => Wire::OptionalNbt,
            Type::Fixed(len) => Wire::Fixed(*len),
            Type::LpVec3 => Wire::LpVec3,
            Type::Holder(inner) => Wire::Holder(Box::new(Wire::from_schema(state, inner))),
            Type::Option(inner) => Wire::Option(Box::new(Wire::from_schema(state, inner))),
            Type::Array { item, .. } => Wire::Array(Box::new(Wire::from_schema(state, item))),
            Type::Named(name) => {
                if let Some(item) = state.enums.get(name) {
                    Wire::Enum(item.repr, item.values.values().copied().collect())
                } else if let Some(item) = state.structs.get(name) {
                    fields(&item.fields)
                } else if let Some(item) = state.unions.get(name) {
                    let cases = item.variants.iter().filter_map(|variant| {
                        let items = variant.ty.iter().map(|ty| Wire::from_schema(state, ty));
                        Some((variant.tag?, Wire::seq(items.collect())))
                    });
                    let default = item
                        .variants
                        .iter()
                        .any(|variant| variant.default)
                        .then(|| Box::new(Wire::Seq(Vec::new())));
                    Wire::Union(item.tag, cases.collect(), default)
                } else if let Some(item) = state.flags.get(name) {
                    Wire::Flags(item.repr, item.bits.values().copied().collect())
                } else if let Some(item) = state.bitfields.get(name) {
                    Wire::Bitfield(item.fields.iter().map(|f| (f.bits, f.signed)).collect())
                } else if state.externs.contains_key(name) {
                    Wire::Extern(name.clone())
                } else {
                    unreachable!("the schema was validated")
                }
            }
        }
    }

    /// A sequence, with nested sequences spliced in: both write the same bytes.
    fn seq(items: Vec<Wire>) -> Wire {
        let mut flat = Vec::with_capacity(items.len());
        for item in items {
            match item {
                Wire::Seq(inner) => flat.extend(inner),
                other => flat.push(other),
            }
        }
        Wire::Seq(flat)
    }

    /// Whether the type contains a hand-written type.
    fn has_extern(&self) -> bool {
        match self {
            Wire::Extern(_) => true,
            Wire::Holder(inner) | Wire::Option(inner) | Wire::Array(inner) => inner.has_extern(),
            Wire::Seq(items) => items.iter().any(Wire::has_extern),
            Wire::Union(_, cases, default) => {
                cases.values().any(Wire::has_extern)
                    || default.as_ref().is_some_and(|d| d.has_extern())
            }
            _ => false,
        }
    }

    /// Whether `self`, from the schema, sends the same bytes as `theirs`, from minecraft-data.
    ///
    /// - An enum or a set of flags matches a plain integer, since minecraft-data leaves many as integers.
    /// - Integers of the same size match whatever their sign, and `bytes` matches an array of bytes.
    /// - A union without a default variant matches a switch whose default sends nothing, as long as its
    ///   extra tags send nothing either: the schema rejects values that minecraft-data does not check.
    fn matches(&self, theirs: &Wire) -> bool {
        let same_int =
            |a: &Prim, b: &Prim| a == b || (a.int_size().is_some() && a.int_size() == b.int_size());
        match (self, theirs) {
            // A sequence of one type is that type, as an array of one-field containers is.
            (Wire::Seq(ours), theirs) if ours.len() == 1 && !matches!(theirs, Wire::Seq(_)) => {
                ours[0].matches(theirs)
            }
            (ours, Wire::Seq(theirs)) if theirs.len() == 1 && !matches!(ours, Wire::Seq(_)) => {
                ours.matches(&theirs[0])
            }
            (Wire::Prim(a), Wire::Prim(b)) => same_int(a, b),
            (Wire::Enum(ours, _) | Wire::Flags(ours, _), Wire::Prim(prim)) => same_int(ours, prim),
            (Wire::Enum(a, ours), Wire::Enum(b, theirs)) => same_int(a, b) && ours == theirs,
            (Wire::Flags(a, ours), Wire::Flags(b, theirs)) => same_int(a, b) && ours == theirs,
            (Wire::Bytes, Wire::Array(item)) | (Wire::Array(item), Wire::Bytes) => {
                matches!(**item, Wire::Prim(Prim::U8 | Prim::I8))
            }
            (Wire::Option(ours), Wire::Option(theirs))
            | (Wire::Array(ours), Wire::Array(theirs))
            | (Wire::Holder(ours), Wire::Holder(theirs)) => ours.matches(theirs),
            (Wire::Seq(ours), Wire::Seq(theirs)) => {
                ours.len() == theirs.len() && ours.iter().zip(theirs).all(|(a, b)| a.matches(b))
            }
            (Wire::Union(a, ours, our_default), Wire::Union(b, theirs, their_default)) => {
                let case = |tag: &i64| ours.get(tag).or(our_default.as_deref());
                let their_case = |tag: &i64| theirs.get(tag).or(their_default.as_deref());
                same_int(a, b)
                    && theirs
                        .keys()
                        .all(|tag| case(tag).is_some_and(|c| c.matches(&theirs[tag])))
                    && ours
                        .iter()
                        .all(|(tag, c)| their_case(tag).is_some_and(|t| c.matches(t)))
                    && match (our_default, their_default) {
                        (Some(ours), Some(theirs)) => ours.matches(theirs),
                        (Some(_), None) => false,
                        (None, _) => true,
                    }
            }
            (Wire::Extern(_), _) => true,
            (ours, theirs) => ours == theirs,
        }
    }
}

impl fmt::Display for Wire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Wire::Prim(prim) => f.write_str(prim.name()),
            Wire::String => f.write_str("string"),
            Wire::Bytes => f.write_str("bytes"),
            Wire::Rest => f.write_str("rest"),
            Wire::Nbt => f.write_str("nbt"),
            Wire::OptionalNbt => f.write_str("optional_nbt"),
            Wire::Fixed(len) => write!(f, "fixed({len})"),
            Wire::LpVec3 => f.write_str("lp_vec3"),
            Wire::Holder(inner) => write!(f, "holder({inner})"),
            Wire::Extern(name) => f.write_str(name),
            Wire::Enum(prim, values) => {
                let values: Vec<_> = values.iter().map(i64::to_string).collect();
                write!(f, "{prim}[{}]", values.join(", "), prim = prim.name())
            }
            Wire::Flags(prim, bits) => {
                let bits: Vec<_> = bits.iter().map(u32::to_string).collect();
                write!(f, "{prim} flags[{}]", bits.join(", "), prim = prim.name())
            }
            Wire::Bitfield(fields) => {
                let fields: Vec<_> = fields
                    .iter()
                    .map(|(bits, signed)| format!("{}{bits}", if *signed { "i" } else { "u" }))
                    .collect();
                write!(f, "bitfield({})", fields.join(", "))
            }
            Wire::Union(prim, cases, default) => {
                write!(f, "switch {} {{", prim.name())?;
                for (i, (tag, case)) in cases.iter().enumerate() {
                    let sep = if i > 0 { "," } else { "" };
                    write!(f, "{sep} {tag}: {case}")?;
                }
                if let Some(default) = default {
                    write!(f, ", _: {default}")?;
                }
                f.write_str(" }")
            }
            Wire::Option(inner) => write!(f, "option({inner})"),
            Wire::Array(item) => write!(f, "array({item})"),
            Wire::Seq(items) => {
                f.write_str("(")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_str(")")
            }
        }
    }
}

/// Resolves ProtoDef types, looking in the state's types first and then in the global ones.
struct Resolver<'a> {
    local: &'a Value,
    global: &'a Value,
}

impl Resolver<'_> {
    fn resolve(&self, ty: &Value) -> Result<Wire, String> {
        match ty {
            Value::String(name) => self.resolve_name(name, 0),
            Value::Array(parts) => self.resolve_compound(parts),
            other => Err(format!("unexpected type `{other}`")),
        }
    }

    fn resolve_name(&self, name: &str, depth: usize) -> Result<Wire, String> {
        if depth > 32 {
            return Err(format!("type `{name}` refers to itself"));
        }
        let definition = self.local.get(name).or_else(|| self.global.get(name));
        match definition {
            Some(Value::String(native)) if native == "native" => native_type(name),
            Some(Value::String(alias)) => self.resolve_name(alias, depth + 1),
            Some(Value::Array(parts)) => self.resolve_compound(parts),
            Some(other) => Err(format!("unexpected definition of `{name}`: {other}")),
            None => Err(format!("unknown type `{name}`")),
        }
    }

    fn resolve_compound(&self, parts: &[Value]) -> Result<Wire, String> {
        let kind = parts
            .first()
            .and_then(Value::as_str)
            .ok_or("compound type without a name")?;
        let args = parts.get(1).unwrap_or(&Value::Null);
        let varint_count = || match args["countType"].as_str() {
            Some("varint") => Ok(()),
            other => Err(format!("`{kind}` counted by {other:?}, not a varint")),
        };
        match kind {
            "pstring" => varint_count().map(|()| Wire::String),
            "buffer" => match args["count"].as_u64() {
                Some(len) => Ok(Wire::Fixed(len as usize)),
                None => varint_count().map(|()| Wire::Bytes),
            },
            "bitflags" => match self.resolve(&args["type"])? {
                Wire::Prim(prim) => {
                    let flags = args["flags"]
                        .as_array()
                        .ok_or("bitflags without a flag list")?;
                    Ok(Wire::Flags(prim, (0..flags.len() as u32).collect()))
                }
                other => Err(format!("bitflags over `{other}`, not an integer")),
            },
            "bitfield" => {
                let fields = args.as_array().ok_or("bitfield without fields")?;
                let fields = fields.iter().map(|field| {
                    let bits = field["size"]
                        .as_u64()
                        .ok_or("bitfield field without a size")?;
                    Ok((bits as u32, field["signed"].as_bool().unwrap_or(false)))
                });
                Ok(Wire::Bitfield(fields.collect::<Result<_, String>>()?))
            }
            "registryEntryHolder" => Ok(Wire::Holder(Box::new(
                self.resolve(&args["otherwise"]["type"])?,
            ))),
            "option" => Ok(Wire::Option(Box::new(self.resolve(args)?))),
            "array" => {
                varint_count()?;
                Ok(Wire::Array(Box::new(self.resolve(&args["type"])?)))
            }
            "mapper" => match self.resolve(&args["type"])? {
                Wire::Prim(prim) => {
                    let (_, values) = mapper_values(args)?;
                    Ok(Wire::Enum(prim, values.into_values().collect()))
                }
                other => Err(format!("mapper over `{other}`, not an integer")),
            },
            "container" => {
                self.resolve_container(args.as_array().ok_or("container without fields")?)
            }
            "switch" => Err(format!(
                "switch on `{}`, which is not an earlier field of the same container",
                args["compareTo"].as_str().unwrap_or_default()
            )),
            other => Err(format!("`{other}` has no equivalent in the schema yet")),
        }
    }

    /// Resolves a container. A field that later switches compare to becomes a union, together with every
    /// field up to the last of those switches.
    fn resolve_container(&self, fields: &[Value]) -> Result<Wire, String> {
        let mut items = Vec::new();
        let mut i = 0;
        while i < fields.len() {
            let name = fields[i]["name"].as_str();
            let last = name.and_then(|name| {
                fields
                    .iter()
                    .rposition(|field| switch_target(field) == Some(name))
            });
            match (name, last) {
                (Some(name), Some(last)) if last > i => {
                    items.push(self.resolve_union(
                        name,
                        &fields[i]["type"],
                        &fields[i + 1..=last],
                    )?);
                    i = last + 1;
                }
                _ => {
                    items.push(self.resolve(&fields[i]["type"])?);
                    i += 1;
                }
            }
        }
        Ok(Wire::seq(items))
    }

    /// Resolves the union tagged by the field `name` of type `tag`, made of the fields that follow it.
    fn resolve_union(&self, name: &str, tag: &Value, fields: &[Value]) -> Result<Wire, String> {
        let (prim, names) = match self.expand(tag)? {
            Value::Array(parts) if parts[0] == "mapper" => match self.resolve(&parts[1]["type"])? {
                Wire::Prim(prim) => mapper_values(&parts[1]).map(|(names, _)| (prim, names))?,
                other => return Err(format!("mapper over `{other}`, not an integer")),
            },
            _ => match self.resolve(tag)? {
                Wire::Prim(Prim::Bool) => (
                    Prim::Bool,
                    BTreeMap::from([("false".to_owned(), 0), ("true".to_owned(), 1)]),
                ),
                Wire::Prim(prim) if prim.is_enum_repr() => (prim, BTreeMap::new()),
                other => return Err(format!("switch on `{name}` of type `{other}`")),
            },
        };
        let tag_value = |key: &str| {
            names
                .get(key)
                .copied()
                .or_else(|| parse_int(key))
                .ok_or_else(|| format!("switch on `{name}` has an unknown case `{key}`"))
        };
        // Each field: its type per tag value when it switches on the tag, or its only type otherwise.
        let mut columns = Vec::new();
        let mut tags = BTreeSet::new();
        for field in fields {
            if switch_target(field) != Some(name) {
                columns.push(Err(&field["type"]));
                continue;
            }
            let args = &field["type"][1];
            let mut cases = BTreeMap::new();
            for (key, ty) in args["fields"].as_object().ok_or("switch without fields")? {
                let value = tag_value(key)?;
                tags.insert(value);
                cases.insert(value, ty);
            }
            // A switch with no default sends nothing for the values it does not list.
            columns.push(Ok((cases, args.get("default"))));
        }
        let case = |tag: Option<i64>| -> Result<Wire, String> {
            let mut items = Vec::new();
            for column in &columns {
                let ty = match column {
                    Ok((by_tag, default)) => {
                        tag.and_then(|tag| by_tag.get(&tag).copied()).or(*default)
                    }
                    Err(ty) => Some(*ty),
                };
                if let Some(ty) = ty {
                    items.push(self.resolve(ty)?);
                }
            }
            Ok(Wire::seq(items))
        };
        let mut cases = BTreeMap::new();
        for tag in tags {
            cases.insert(tag, case(Some(tag))?);
        }
        // A `bool` with both values listed has no other value to default to.
        let default = if prim == Prim::Bool && cases.len() == 2 {
            None
        } else {
            Some(Box::new(case(None)?))
        };
        Ok(Wire::Union(prim, cases, default))
    }

    /// Follows type aliases to the definition they name; a compound type is returned as it is.
    fn expand<'v>(&'v self, ty: &'v Value) -> Result<&'v Value, String> {
        let mut ty = ty;
        for _ in 0..32 {
            let Value::String(name) = ty else {
                return Ok(ty);
            };
            match self.local.get(name).or_else(|| self.global.get(name)) {
                Some(Value::String(native)) if native == "native" => return Ok(ty),
                Some(definition) => ty = definition,
                None => return Err(format!("unknown type `{name}`")),
            }
        }
        Err(format!("type `{ty}` refers to itself"))
    }
}

/// The field a container field switches on, when its type is a switch.
fn switch_target(field: &Value) -> Option<&str> {
    match &field["type"] {
        Value::Array(parts) if parts.first().is_some_and(|kind| kind == "switch") => {
            parts.get(1)?["compareTo"].as_str()
        }
        _ => None,
    }
}

/// The names of a mapper's values, and its values by name.
type MapperValues = (BTreeMap<String, i64>, BTreeMap<String, i64>);

/// Reads a mapper's `mappings`, from values to names, into both directions.
fn mapper_values(args: &Value) -> Result<MapperValues, String> {
    let mappings = args["mappings"]
        .as_object()
        .ok_or("mapper without mappings")?;
    let mut by_name = BTreeMap::new();
    let mut by_value = BTreeMap::new();
    for (key, name) in mappings {
        let value = parse_int(key).ok_or_else(|| format!("invalid mapper value `{key}`"))?;
        let name = name.as_str().ok_or("mapper name is not a string")?;
        by_name.insert(name.to_owned(), value);
        by_value.insert(key.clone(), value);
    }
    Ok((by_name, by_value))
}

fn parse_int(text: &str) -> Option<i64> {
    match text.strip_prefix("0x") {
        Some(hex) => i64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

fn native_type(name: &str) -> Result<Wire, String> {
    match name {
        "UUID" => Ok(Wire::Prim(Prim::Uuid)),
        "restBuffer" => Ok(Wire::Rest),
        "anonymousNbt" => Ok(Wire::Nbt),
        "anonOptionalNbt" => Ok(Wire::OptionalNbt),
        "lpVec3" => Ok(Wire::LpVec3),
        "void" => Ok(Wire::Seq(Vec::new())),
        other => Prim::from_name(other)
            .filter(|prim| *prim != Prim::Uuid)
            .map(Wire::Prim)
            .ok_or_else(|| format!("native type `{other}` has no equivalent in the schema yet")),
    }
}
