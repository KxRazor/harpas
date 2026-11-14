//! Shared providers for configured features, including their declared ranges.

use harpas_core::random::RandomSource;
use serde_json::Value;

use crate::data::{IntProviderDef, WorldgenDataError};

#[derive(Debug, Clone, Copy)]
pub(crate) enum FloatProvider {
    Constant(f32),
    Uniform(f32, f32),
    Trapezoid(f32, f32, f32),
    Normal(f32, f32, f32, f32),
}

impl FloatProvider {
    pub(crate) fn sample(self, random: &mut impl RandomSource) -> f32 {
        match self {
            Self::Constant(value) => value,
            Self::Uniform(min, max) => min + random.next_float() * (max - min),
            Self::Trapezoid(min, max, plateau) => {
                let slope = (max - min - plateau) / 2.0;
                min + random.next_float() * (max - min - slope) + random.next_float() * slope
            }
            Self::Normal(mean, deviation, min, max) => {
                (mean + random.next_gaussian() as f32 * deviation).clamp(min, max)
            }
        }
    }
}

pub(crate) fn int_bounds(provider: &IntProviderDef) -> Option<(i32, i32)> {
    Some(match provider {
        IntProviderDef::Constant(value) => (*value, *value),
        IntProviderDef::Uniform {
            min_inclusive,
            max_inclusive,
        }
        | IntProviderDef::BiasedToBottom {
            min_inclusive,
            max_inclusive,
        }
        | IntProviderDef::ClampedNormal {
            min_inclusive,
            max_inclusive,
            ..
        } => (*min_inclusive, *max_inclusive),
        IntProviderDef::Clamped {
            source,
            min_inclusive,
            max_inclusive,
        } => {
            let (min, max) = int_bounds(source)?;
            (min.max(*min_inclusive), max.min(*max_inclusive))
        }
        IntProviderDef::Trapezoid { min, max, .. } => (*min, *max),
        IntProviderDef::WeightedList(entries) => {
            let bounds: Option<Vec<_>> = entries.iter().map(|(p, _)| int_bounds(p)).collect();
            let bounds = bounds?;
            (
                bounds.iter().map(|b| b.0).min()?,
                bounds.iter().map(|b| b.1).max()?,
            )
        }
        IntProviderDef::Other(_) => return None,
    })
}

pub(crate) fn compile_float_provider(
    name: &str,
    value: &Value,
    low: f32,
    high: f32,
) -> Result<Option<FloatProvider>, WorldgenDataError> {
    let invalid = |message: String| WorldgenDataError::InvalidDefinition {
        context: name.into(),
        message,
    };
    let n = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_f64)
            .map(|v| v as f32)
            .filter(|v| v.is_finite())
            .ok_or_else(|| invalid(format!("invalid float provider field `{key}`")))
    };
    let (provider, min, max) = if let Some(v) = value.as_f64() {
        (FloatProvider::Constant(v as f32), v as f32, v as f32)
    } else {
        match value.get("type").and_then(Value::as_str) {
            Some("minecraft:constant") => {
                let v = n("value")?;
                (FloatProvider::Constant(v), v, v)
            }
            Some("minecraft:uniform") => {
                let (min, max) = (n("min_inclusive")?, n("max_exclusive")?);
                (FloatProvider::Uniform(min, max), min, max)
            }
            Some("minecraft:trapezoid") => {
                let (min, max, plateau) = (n("min")?, n("max")?, n("plateau")?);
                if plateau > max - min {
                    return Err(invalid("invalid float provider plateau".into()));
                }
                (FloatProvider::Trapezoid(min, max, plateau), min, max)
            }
            Some("minecraft:clamped_normal") => {
                let (min, max) = (n("min")?, n("max")?);
                (
                    FloatProvider::Normal(n("mean")?, n("deviation")?, min, max),
                    min,
                    max,
                )
            }
            Some(_) => return Ok(None),
            None => return Err(invalid("invalid float provider".into())),
        }
    };
    if !min.is_finite() || !max.is_finite() || min < low || max > high || min > max {
        return Err(invalid(format!("provider must be within {low} and {high}")));
    }
    Ok(Some(provider))
}
