//! JSON-to-NBT conversion and recursive merging for the `append_static` rule modifier.

use harpas_core::nbt::{Compound, List, MAX_DEPTH, Nbt};
use serde_json::Value;

use super::invalid;
use crate::WorldgenDataError;

pub(super) fn compile(context: &str, value: &Value) -> Result<Compound, WorldgenDataError> {
    match convert(context, value, 0)? {
        Nbt::Compound(tag) => Ok(tag),
        _ => Err(invalid(context, "static data must be a compound")),
    }
}

fn convert(context: &str, value: &Value, depth: usize) -> Result<Nbt, WorldgenDataError> {
    if depth > MAX_DEPTH {
        return Err(invalid(context, "static data is nested too deeply"));
    }
    Ok(match value {
        Value::Null => return Err(invalid(context, "null is not an NBT value")),
        Value::Bool(value) => Nbt::Byte(i8::from(*value)),
        Value::String(value) => Nbt::String(value.clone()),
        Value::Number(value) => {
            // JsonOps chooses the narrowest exact numeric representation, rather than using the
            // JSON token's spelling to select an NBT type.
            let double = value.as_f64().unwrap();
            let integer = value.as_i64().or_else(|| {
                (double >= i64::MIN as f64 && double < -(i64::MIN as f64) && double.fract() == 0.0)
                    .then_some(double as i64)
            });
            if let Some(integer) = integer {
                if let Ok(byte) = i8::try_from(integer) {
                    Nbt::Byte(byte)
                } else if let Ok(short) = i16::try_from(integer) {
                    Nbt::Short(short)
                } else if let Ok(int) = i32::try_from(integer) {
                    Nbt::Int(int)
                } else {
                    Nbt::Long(integer)
                }
            } else if double as f32 as f64 == double {
                Nbt::Float(double as f32)
            } else {
                Nbt::Double(double)
            }
        }
        Value::Object(values) => Nbt::Compound(
            values
                .iter()
                .map(|(name, value)| Ok((name.clone(), convert(context, value, depth + 1)?)))
                .collect::<Result<_, WorldgenDataError>>()?,
        ),
        Value::Array(values) => {
            let values = values
                .iter()
                .map(|value| convert(context, value, depth + 1))
                .collect::<Result<Vec<_>, _>>()?;
            Nbt::List(list(values))
        }
    })
}

fn list(values: Vec<Nbt>) -> List {
    let Some(first) = values.first() else {
        return List::Empty;
    };
    let id = first.tag_id();
    if id == 10 || values.iter().any(|value| value.tag_id() != id) {
        // Vanilla's heterogeneous lists are written as compounds on the wire. Non-compounds
        // are wrapped under the empty key. A compound with only that key is itself wrapped
        // so that vanilla's list reader can distinguish it from an encoded scalar.
        return List::Compound(
            values
                .into_iter()
                .map(|value| match value {
                    Nbt::Compound(tag) if tag.len() != 1 || tag.get("").is_none() => tag,
                    value => [("", value)].into_iter().collect(),
                })
                .collect(),
        );
    }
    macro_rules! homogeneous {
        ($($variant:ident),*) => {
            match first {
                $(Nbt::$variant(_) => List::$variant(values.into_iter().map(|value| {
                    let Nbt::$variant(value) = value else { unreachable!() };
                    value
                }).collect()),)*
            }
        };
    }
    homogeneous!(
        Byte, Short, Int, Long, Float, Double, ByteArray, String, List, Compound, IntArray,
        LongArray
    )
}

pub(super) fn merge(target: &mut Compound, source: &Compound) {
    for (name, value) in source.iter() {
        let value = match (target.get(name), value) {
            (Some(Nbt::Compound(existing)), Nbt::Compound(incoming)) => {
                let mut nested = existing.clone();
                merge(&mut nested, incoming);
                Nbt::Compound(nested)
            }
            _ => value.clone(),
        };
        target.insert(name, value);
    }
}
