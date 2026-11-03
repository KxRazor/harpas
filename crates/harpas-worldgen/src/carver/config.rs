//! Compilation of data-pack carver definitions into validated distributions and block selectors.

use harpas_core::block::block_states;
use harpas_core::random::RandomSource;
use serde_json::{Map, Value};

use super::{CarverFloatProvider, CarverHeightProvider};
use crate::data::{WorldgenData, WorldgenDataError};
use crate::surface::VerticalAnchor;

/// The generation parameters and replaceable block states of a configured carver.
#[derive(Debug, Clone)]
pub struct CarverConfig {
    /// Probability that a source chunk starts this carver.
    pub probability: f32,
    /// Distribution of the source's starting height.
    pub y: CarverHeightProvider,
    /// Vertical scale of cave rooms or canyon tunnels.
    pub y_scale: CarverFloatProvider,
    /// At or below this anchor, the carver fills with lava.
    pub lava_level: VerticalAnchor,
    /// Membership indexed by global block state ID.
    pub replaceable: Vec<bool>,
    /// Parameters specific to the configured shape.
    pub shape: CarverShape,
}

/// Shape-specific fields of a cave, Nether cave or canyon.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CarverShape {
    /// A cave made of a possible room and winding tunnels.
    Cave {
        /// Whether this is the Nether variant.
        nether: bool,
        /// Horizontal tunnel radius multiplier.
        horizontal_radius: CarverFloatProvider,
        /// Vertical tunnel radius multiplier.
        vertical_radius: CarverFloatProvider,
        /// Normalized floor cutoff.
        floor_level: CarverFloatProvider,
    },
    /// A canyon with its own path and radius distributions.
    Canyon(CanyonShape),
}

/// The data-driven parameters of a canyon carver.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CanyonShape {
    /// Initial vertical angle.
    pub vertical_rotation: CarverFloatProvider,
    /// Multiplier for the length of the path.
    pub distance_factor: CarverFloatProvider,
    /// Horizontal radius multiplier.
    pub horizontal_radius_factor: CarverFloatProvider,
    /// Initial tunnel thickness distribution.
    pub thickness: CarverFloatProvider,
    /// Default vertical radius factor.
    pub vertical_radius_default_factor: f32,
    /// Change in vertical radius toward the middle of the canyon.
    pub vertical_radius_center_factor: f32,
    /// Number of layers for width changes to smooth across.
    pub width_smoothness: i32,
}

impl CanyonShape {
    /// Samples the canyon's vertical width profile from bottom to top.
    ///
    /// # Panics
    /// Panics if `width_smoothness` is not positive. Compiled configurations always satisfy this constraint.
    pub fn width_factors(&self, random: &mut impl RandomSource, height: usize) -> Vec<f32> {
        assert!(
            self.width_smoothness > 0,
            "width smoothness must be positive"
        );
        let mut values = Vec::with_capacity(height);
        let mut width = 1.0_f32;
        for index in 0..height {
            if index == 0 || random.next_int_bounded(self.width_smoothness) == 0 {
                width = 1.0 + random.next_float() * random.next_float();
            }
            values.push(width * width);
        }
        values
    }
}

impl CarverConfig {
    /// Draws the source-chunk start decision, advancing the supplied random source.
    pub fn starts(&self, random: &mut impl RandomSource) -> bool {
        random.next_float() <= self.probability
    }

    /// Compiles a configured carver and its replaceable block tags from the supplied data pack.
    /// Unsupported provider types and debug carving are errors instead of silently producing different terrain.
    pub fn compile(name: &str, data: &WorldgenData) -> Result<Self, WorldgenDataError> {
        let compiler = Compiler { name, data };
        let definition = data
            .configured_carvers
            .get(name)
            .ok_or_else(|| compiler.error("unknown configured carver"))?;
        let config = compiler.object(&definition.config, "config")?;
        if let Some(debug) = config.get("debug_settings") {
            let debug = compiler.object(debug, "debug_settings")?;
            match debug.get("debug_mode") {
                None | Some(Value::Bool(false)) => {}
                Some(Value::Bool(true)) => {
                    return Err(compiler.error("debug carving is not supported"));
                }
                Some(_) => return Err(compiler.error("debug_mode must be a boolean")),
            }
        }
        let probability = compiler.number(compiler.field(config, "probability")?, "probability")?;
        if !(0.0..=1.0).contains(&probability) {
            return Err(compiler.error("probability must be between 0 and 1"));
        }
        let shape = match definition.carver_type.as_str() {
            "minecraft:cave" | "cave" | "minecraft:nether_cave" | "nether_cave" => {
                CarverShape::Cave {
                    nether: definition.carver_type.ends_with("nether_cave"),
                    horizontal_radius: compiler.float(config, "horizontal_radius_multiplier")?,
                    vertical_radius: compiler.float(config, "vertical_radius_multiplier")?,
                    floor_level: compiler.float(config, "floor_level")?,
                }
            }
            "minecraft:canyon" | "canyon" => {
                let shape = compiler.object(compiler.field(config, "shape")?, "shape")?;
                let width = compiler.integer(
                    compiler.field(shape, "width_smoothness")?,
                    "width_smoothness",
                )?;
                if width <= 0 {
                    return Err(compiler.error("width_smoothness must be positive"));
                }
                CarverShape::Canyon(CanyonShape {
                    vertical_rotation: compiler.float(config, "vertical_rotation")?,
                    distance_factor: compiler.float(shape, "distance_factor")?,
                    horizontal_radius_factor: compiler.float(shape, "horizontal_radius_factor")?,
                    thickness: compiler.float(shape, "thickness")?,
                    vertical_radius_default_factor: compiler.number(
                        compiler.field(shape, "vertical_radius_default_factor")?,
                        "vertical_radius_default_factor",
                    )?,
                    vertical_radius_center_factor: compiler.number(
                        compiler.field(shape, "vertical_radius_center_factor")?,
                        "vertical_radius_center_factor",
                    )?,
                    width_smoothness: width,
                })
            }
            other => return Err(compiler.error(format!("unsupported carver type `{other}`"))),
        };
        Ok(Self {
            probability,
            y: compiler.height(compiler.field(config, "y")?)?,
            y_scale: compiler.float(config, "yScale")?,
            lava_level: compiler.anchor(compiler.field(config, "lava_level")?)?,
            replaceable: compiler.blocks(compiler.field(config, "replaceable")?)?,
            shape,
        })
    }
}

struct Compiler<'a> {
    name: &'a str,
    data: &'a WorldgenData,
}

impl Compiler<'_> {
    fn error(&self, message: impl Into<String>) -> WorldgenDataError {
        WorldgenDataError::InvalidDefinition {
            context: self.name.to_owned(),
            message: message.into(),
        }
    }

    fn object<'a>(
        &self,
        value: &'a Value,
        field: &str,
    ) -> Result<&'a Map<String, Value>, WorldgenDataError> {
        value
            .as_object()
            .ok_or_else(|| self.error(format!("{field} must be an object")))
    }

    fn field<'a>(
        &self,
        object: &'a Map<String, Value>,
        field: &str,
    ) -> Result<&'a Value, WorldgenDataError> {
        object
            .get(field)
            .ok_or_else(|| self.error(format!("missing `{field}`")))
    }

    fn number(&self, value: &Value, field: &str) -> Result<f32, WorldgenDataError> {
        value
            .as_f64()
            .map(|v| v as f32)
            .filter(|v| v.is_finite())
            .ok_or_else(|| self.error(format!("{field} must be a finite float")))
    }

    fn integer(&self, value: &Value, field: &str) -> Result<i32, WorldgenDataError> {
        value
            .as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| self.error(format!("{field} must be a 32-bit integer")))
    }

    fn anchor(&self, value: &Value) -> Result<VerticalAnchor, WorldgenDataError> {
        let obj = self.object(value, "vertical anchor")?;
        if obj.len() != 1 {
            return Err(self.error("vertical anchor must have exactly one field"));
        }
        let (key, value) = obj.iter().next().expect("one field");
        let value = self.integer(value, key)?;
        match key.as_str() {
            "absolute" => Ok(VerticalAnchor::Absolute(value)),
            "above_bottom" => Ok(VerticalAnchor::AboveBottom(value)),
            "below_top" => Ok(VerticalAnchor::BelowTop(value)),
            _ => Err(self.error(format!("unsupported vertical anchor `{key}`"))),
        }
    }

    fn height(&self, value: &Value) -> Result<CarverHeightProvider, WorldgenDataError> {
        let obj = self.object(value, "height provider")?;
        let Some(kind) = obj.get("type") else {
            return self.anchor(value).map(CarverHeightProvider::Constant);
        };
        match kind.as_str() {
            Some("minecraft:uniform" | "uniform") => Ok(CarverHeightProvider::Uniform {
                min: self.anchor(self.field(obj, "min_inclusive")?)?,
                max: self.anchor(self.field(obj, "max_inclusive")?)?,
            }),
            _ => Err(self.error("unsupported height provider")),
        }
    }

    fn float(
        &self,
        object: &Map<String, Value>,
        field: &str,
    ) -> Result<CarverFloatProvider, WorldgenDataError> {
        let value = self.field(object, field)?;
        if value.is_number() {
            return Ok(CarverFloatProvider::Constant(self.number(value, field)?));
        }
        let obj = self.object(value, field)?;
        match self.field(obj, "type")?.as_str() {
            Some("minecraft:uniform" | "uniform") => {
                let min = self.number(self.field(obj, "min_inclusive")?, field)?;
                let max = self.number(self.field(obj, "max_exclusive")?, field)?;
                if min >= max {
                    return Err(self.error(format!("{field} maximum must exceed minimum")));
                }
                Ok(CarverFloatProvider::Uniform { min, max })
            }
            Some("minecraft:trapezoid" | "trapezoid") => {
                let min = self.number(self.field(obj, "min")?, field)?;
                let max = self.number(self.field(obj, "max")?, field)?;
                let plateau = self.number(self.field(obj, "plateau")?, field)?;
                if min >= max {
                    return Err(self.error(format!("invalid trapezoid for {field}")));
                }
                Ok(CarverFloatProvider::Trapezoid { min, max, plateau })
            }
            _ => Err(self.error(format!("unsupported float provider for {field}"))),
        }
    }

    fn blocks(&self, value: &Value) -> Result<Vec<bool>, WorldgenDataError> {
        let entries = match value {
            Value::String(_) => vec![value],
            Value::Array(entries) => entries.iter().collect(),
            _ => return Err(self.error("replaceable must be a block tag or a list of blocks")),
        };
        let registry = block_states();
        let mut result = vec![false; registry.len() as usize];
        for entry in entries {
            let name = entry
                .as_str()
                .ok_or_else(|| self.error("replaceable entry must be a string"))?;
            if name.starts_with('#') {
                let tag = self.data.block_tag_states(name)?;
                for (slot, matches) in result.iter_mut().zip(tag) {
                    *slot |= matches;
                }
            } else {
                let name = if name.contains(':') {
                    name.to_owned()
                } else {
                    format!("minecraft:{name}")
                };
                if registry.default_state(&name).is_none() {
                    return Err(self.error(format!("unknown replaceable block `{name}`")));
                }
                for (id, slot) in result.iter_mut().enumerate() {
                    if registry
                        .properties(id as u32)
                        .is_some_and(|(block, _)| block == name)
                    {
                        *slot = true;
                    }
                }
            }
        }
        Ok(result)
    }
}
