//! Comparison of the schema with minecraft-data's `protocol.json`
//!
//! Field names and limits are Harpas' own, so the comparison covers what decides the bytes on the wire:
//! which packets exist, their IDs, and the sequence of wire types in each one.

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
            check_direction(state, direction, &resolver, &mut report.errors);
        }
    }
    report
}

fn check_direction(
    state: &State,
    direction: Direction,
    resolver: &Resolver<'_>,
    errors: &mut Vec<String>,
) {
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
        let ours = Wire::Seq(
            packet
                .fields
                .iter()
                .map(|field| Wire::from_schema(state, &field.ty))
                .collect(),
        );
        match resolver.resolve(&Value::String(found.type_name.clone())) {
            Ok(theirs) if theirs == ours => {}
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
        if !generated && !hand_written {
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
    Option(Box<Wire>),
    Array(Box<Wire>),
    Seq(Vec<Wire>),
}

impl Wire {
    fn from_schema(state: &State, ty: &Type) -> Wire {
        match ty {
            Type::Prim(prim) => Wire::Prim(*prim),
            Type::String { .. } => Wire::String,
            Type::Bytes { .. } => Wire::Bytes,
            Type::Rest { .. } => Wire::Rest,
            Type::Option(inner) => Wire::Option(Box::new(Wire::from_schema(state, inner))),
            Type::Array { item, .. } => Wire::Array(Box::new(Wire::from_schema(state, item))),
            Type::Named(name) => match (state.enums.get(name), state.structs.get(name)) {
                (Some(item), _) => Wire::Prim(item.repr),
                (None, Some(item)) => Wire::Seq(
                    item.fields
                        .iter()
                        .map(|f| Wire::from_schema(state, &f.ty))
                        .collect(),
                ),
                (None, None) => unreachable!("the schema was validated"),
            },
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
            "buffer" => varint_count().map(|()| Wire::Bytes),
            "option" => Ok(Wire::Option(Box::new(self.resolve(args)?))),
            "array" => {
                varint_count()?;
                Ok(Wire::Array(Box::new(self.resolve(&args["type"])?)))
            }
            "container" => {
                let fields = args.as_array().ok_or("container without fields")?;
                let items = fields.iter().map(|field| self.resolve(&field["type"]));
                Ok(Wire::Seq(items.collect::<Result<_, _>>()?))
            }
            other => Err(format!("`{other}` has no equivalent in the schema yet")),
        }
    }
}

fn native_type(name: &str) -> Result<Wire, String> {
    match name {
        "UUID" => Ok(Wire::Prim(Prim::Uuid)),
        "restBuffer" => Ok(Wire::Rest),
        "void" => Ok(Wire::Seq(Vec::new())),
        other => Prim::from_name(other)
            .filter(|prim| *prim != Prim::Uuid)
            .map(Wire::Prim)
            .ok_or_else(|| format!("native type `{other}` has no equivalent in the schema yet")),
    }
}
