//! Large dripstone columns: cave scanning, embedded bases, radial profiles and shared wind offsets.

use harpas_core::block::block_states;
use harpas_core::math::{cos_f32, sin_f32};
use harpas_core::random::{RandomSource, WorldgenRandom};
use serde_json::Value;

use crate::configured::FeatureCompiler;
use crate::data::{IntProviderDef, WorldgenDataError, parse_int_provider};
use crate::feature::FeatureWorld;
use crate::generator::HeightmapType;
use crate::placement::StateSet;

/// A compiled `minecraft:large_dripstone` feature.
#[derive(Debug)]
pub struct CompiledLargeDripstone {
    search: i32,
    radius: (i32, i32),
    scale: FloatProvider,
    ratio: f32,
    bluntness: [FloatProvider; 2],
    wind: FloatProvider,
    wind_radius: i32,
    wind_bluntness: f32,
    replaceable: StateSet,
    block: u32,
}

#[derive(Debug, Clone, Copy)]
enum FloatProvider {
    Constant(f32),
    Uniform(f32, f32),
    Trapezoid(f32, f32, f32),
    Normal(f32, f32, f32, f32),
}

impl FloatProvider {
    fn sample(self, random: &mut impl RandomSource) -> f32 {
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

fn is_block(state: u32, block: &str) -> bool {
    block_states()
        .properties(state)
        .is_some_and(|(name, _)| name == block)
}

fn empty(state: u32) -> bool {
    block_states().is_air(state) || is_block(state, "minecraft:water")
}

fn empty_or_lava(state: u32) -> bool {
    empty(state) || is_block(state, "minecraft:lava")
}

#[derive(Clone, Copy)]
struct Wind {
    y: i32,
    speed: [f64; 2],
}

impl Wind {
    fn offset(self, [x, y, z]: [i32; 3]) -> [i32; 3] {
        let dy = f64::from(self.y - y);
        [
            x + (dy * self.speed[0]).floor() as i32,
            y,
            z + (dy * self.speed[1]).floor() as i32,
        ]
    }
}

struct Column {
    root: [i32; 3],
    step: i32,
    radius: i32,
    bluntness: f64,
    scale: f64,
}

impl Column {
    // The radial profile is sampled in double precision, after a float distance calculation. Its flat
    // central part is set by bluntness. The parity probe also checks the profile independently of placement.
    fn height(&self, distance: f32) -> i32 {
        let d = f64::from(distance).max(self.bluntness) / f64::from(self.radius) * 0.384;
        let profile = 0.75 * d.powf(4.0 / 3.0) - d.powf(2.0 / 3.0) - d.ln() / 3.0;
        ((self.scale * profile).max(0.0) / 0.384 * f64::from(self.radius)) as i32
    }

    fn embedded(&mut self, world: &impl FeatureWorld, wind: Wind) -> bool {
        while self.radius > 1 {
            let mut at = self.root;
            for _ in 0..self.height(0.0).min(10) {
                if is_block(world.block_state(at[0], at[1], at[2]), "minecraft:lava") {
                    return false;
                }
                let center = wind.offset(at);
                if circle_embedded(world, center, self.radius) {
                    self.root = at;
                    return true;
                }
                at[1] -= self.step;
            }
            self.radius /= 2;
        }
        false
    }

    fn place<R: RandomSource>(
        &self,
        world: &mut impl FeatureWorld,
        random: &mut WorldgenRandom<R>,
        wind: Wind,
        config: &CompiledLargeDripstone,
    ) {
        for dx in -self.radius..=self.radius {
            for dz in -self.radius..=self.radius {
                let distance = ((dx * dx + dz * dz) as f32).sqrt();
                if distance > self.radius as f32 {
                    continue;
                }
                let mut height = self.height(distance);
                if height == 0 {
                    continue;
                }
                if random.next_float() < 0.2 {
                    height = (height as f32 * (0.8 + random.next_float() * 0.2)) as i32;
                }
                let [x, y, z] = [self.root[0] + dx, self.root[1], self.root[2] + dz];
                let surface = if self.step > 0 {
                    world.heightmap(HeightmapType::WorldSurfaceWg, x, z)
                } else {
                    world.min_y() + world.height()
                };
                let mut placed = false;
                for i in 0..height {
                    let py = y + self.step * i;
                    if py < world.min_y() || py >= surface {
                        break;
                    }
                    let [px, py, pz] = wind.offset([x, py, z]);
                    if empty_or_lava(world.block_state(px, py, pz)) {
                        world.set_block_state(px, py, pz, config.block);
                        placed = true;
                    } else if placed && config.replaceable.contains(world.block_state(px, py, pz)) {
                        break;
                    }
                }
            }
        }
    }
}

fn circle_embedded(world: &impl FeatureWorld, [x, y, z]: [i32; 3], radius: i32) -> bool {
    if empty_or_lava(world.block_state(x, y, z)) {
        return false;
    }
    let mut angle = 0.0_f32;
    while angle < std::f32::consts::TAU {
        let px = x + (cos_f32(angle) * radius as f32) as i32;
        let pz = z + (sin_f32(angle) * radius as f32) as i32;
        if empty_or_lava(world.block_state(px, y, pz)) {
            return false;
        }
        angle += 6.0 / radius as f32;
    }
    true
}

/// Places large dripstone (`LargeDripstoneFeature.place`). Both a floor and a ceiling are required.
/// The radius is sampled uniformly from the provider's bounds, capped by cave height; the provider's
/// distribution is never sampled. Both columns sample their shape even when neither base can be embedded.
pub fn place_large_dripstone<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    config: &CompiledLargeDripstone,
    random: &mut WorldgenRandom<R>,
    [x, y, z]: [i32; 3],
) -> bool {
    if !empty(world.block_state(x, y, z)) {
        return false;
    }
    let boundary = |step| {
        (0..config.search)
            .map(|d| y + d * step)
            .take_while(|&py| py >= world.min_y() && py < world.min_y() + world.height())
            .find(|&py| !empty(world.block_state(x, py, z)))
            .filter(|&py| {
                let state = world.block_state(x, py, z);
                state == config.block
                    || config.replaceable.contains(state)
                    || is_block(state, "minecraft:lava")
            })
    };
    let (Some(ceiling), Some(floor)) = (boundary(1), boundary(-1)) else {
        return false;
    };
    let cave_height = ceiling - floor - 1;
    if cave_height < 4 {
        return false;
    }
    let max_radius = (cave_height as f32 * config.ratio) as i32;
    let radius = random.next_int_between_inclusive(
        config.radius.0,
        max_radius.clamp(config.radius.0, config.radius.1),
    );
    let mut columns = [
        (-1, ceiling - 1, config.bluntness[0]),
        (1, floor + 1, config.bluntness[1]),
    ]
    .map(|(step, y, bluntness)| Column {
        root: [x, y, z],
        step,
        radius,
        bluntness: f64::from(bluntness.sample(random)),
        scale: f64::from(config.scale.sample(random)),
    });
    let mut wind = Wind { y, speed: [0.0; 2] };
    if columns
        .iter()
        .all(|c| c.radius >= config.wind_radius && c.bluntness >= f64::from(config.wind_bluntness))
    {
        let speed = config.wind.sample(random);
        let angle = random.next_float() * std::f32::consts::PI;
        wind.speed = [
            f64::from(cos_f32(angle) * speed),
            f64::from(sin_f32(angle) * speed),
        ];
    }
    let embedded = columns.each_mut().map(|c| c.embedded(world, wind));
    for (column, embedded) in columns.iter().zip(embedded) {
        if embedded {
            column.place(world, random, wind, config);
        }
    }
    true
}

fn int_bounds(provider: &IntProviderDef) -> Option<(i32, i32)> {
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

impl FeatureCompiler<'_> {
    pub(crate) fn large_dripstone(
        &self,
        name: &str,
        config: &Value,
    ) -> Result<Option<CompiledLargeDripstone>, WorldgenDataError> {
        let invalid = |message: String| WorldgenDataError::InvalidDefinition {
            context: name.into(),
            message,
        };
        let field = |key: &str| {
            config
                .get(key)
                .ok_or_else(|| invalid(format!("missing `{key}`")))
        };
        let number = |key: &str, min: f32, max: f32| {
            field(key)?
                .as_f64()
                .map(|v| v as f32)
                .filter(|v| v.is_finite() && (min..=max).contains(v))
                .ok_or_else(|| invalid(format!("`{key}` must be between {min} and {max}")))
        };
        let integer = |key: &str| {
            field(key)?
                .as_i64()
                .and_then(|v| i32::try_from(v).ok())
                .ok_or_else(|| invalid(format!("`{key}` must be an integer")))
        };
        let provider =
            |key: &str, low: f32, high: f32| -> Result<Option<FloatProvider>, WorldgenDataError> {
                let value = field(key)?;
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
                                return Err(invalid(format!("invalid plateau for `{key}`")));
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
                        None => return Err(invalid(format!("invalid float provider `{key}`"))),
                    }
                };
                if !min.is_finite() || !max.is_finite() || min < low || max > high || min > max {
                    return Err(invalid(format!(
                        "`{key}` provider must be within {low} and {high}"
                    )));
                }
                Ok(Some(provider))
            };
        let Some(radius) = int_bounds(&parse_int_provider(name, field("column_radius")?)?) else {
            return Ok(None);
        };
        if radius.0 < 1 || radius.1 > 60 || radius.0 > radius.1 {
            return Err(invalid("`column_radius` must be within 1 and 60".into()));
        }
        let (Some(scale), Some(ceiling), Some(floor), Some(wind)) = (
            provider("height_scale", 0.0, 20.0)?,
            provider("stalactite_bluntness", 0.1, 10.0)?,
            provider("stalagmite_bluntness", 0.1, 10.0)?,
            provider("wind_speed", 0.0, 2.0)?,
        ) else {
            return Ok(None);
        };
        let wind_radius = integer("min_radius_for_wind")?;
        if !(0..=100).contains(&wind_radius) {
            return Err(invalid(
                "`min_radius_for_wind` must be between 0 and 100".into(),
            ));
        }
        let block = block_states()
            .default_state("minecraft:dripstone_block")
            .unwrap();
        let replaceable = self
            .data()
            .block_tag_states("minecraft:dripstone_replaceable_blocks")?;
        Ok(Some(CompiledLargeDripstone {
            // Vanilla uses a lenient optional positive integer for this field.
            search: config
                .get("floor_to_ceiling_search_range")
                .and_then(Value::as_i64)
                .and_then(|v| i32::try_from(v).ok())
                .filter(|&v| v > 0)
                .unwrap_or(30),
            radius,
            scale,
            ratio: number("max_column_radius_to_cave_height_ratio", 0.1, 1.0)?,
            bluntness: [ceiling, floor],
            wind,
            wind_radius,
            wind_bluntness: number("min_bluntness_for_wind", 0.0, 5.0)?,
            replaceable: StateSet::from_membership(&replaceable),
            block,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChunkFeatureWorld, GeneratedChunk, WorldgenData, place_feature};
    use harpas_core::random::XoroshiroRandom;
    use serde_json::json;

    fn config() -> Value {
        json!({"floor_to_ceiling_search_range": 30, "column_radius": 4, "height_scale": 1,
            "max_column_radius_to_cave_height_ratio": 0.33, "stalactite_bluntness": 0.5,
            "stalagmite_bluntness": 0.7, "wind_speed": 0.2,
            "min_radius_for_wind": 100, "min_bluntness_for_wind": 0.6})
    }

    fn data(config: Value) -> WorldgenData {
        WorldgenData::from_json_entries([
            (
                "data/test/worldgen/configured_feature/dripstone.json",
                json!({"type": "minecraft:large_dripstone", "config": config}).to_string(),
            ),
            (
                "data/minecraft/tags/block/dripstone_replaceable_blocks.json",
                json!({"values": ["minecraft:stone"]}).to_string(),
            ),
        ])
        .unwrap()
    }

    fn cave(floor: i32, ceiling: i32) -> GeneratedChunk {
        let mut chunk = GeneratedChunk::new(0, 0, 0, 64, 1);
        let stone = block_states().default_state("minecraft:stone").unwrap();
        for y in 0..64 {
            for z in 0..16 {
                for x in 0..16 {
                    if y <= floor || y >= ceiling {
                        chunk.set_block_state(x, y, z, stone);
                    }
                }
            }
        }
        chunk.freeze_worldgen_heightmaps();
        chunk
    }

    #[test]
    fn a_missing_boundary_or_short_cave_draws_no_random_values() {
        let data = data(config());
        let feature = FeatureCompiler::new(&data)
            .configured("test:dripstone")
            .unwrap()
            .unwrap();
        for (floor, ceiling, origin) in [
            (16, 64, [8, 32, 8]),
            (-1, 48, [8, 32, 8]),
            (30, 34, [8, 32, 8]),
            (16, 48, [8, 16, 8]),
            (16, 48, [32, 32, 8]),
        ] {
            let mut chunk = cave(floor, ceiling);
            let mut world = ChunkFeatureWorld::new(&mut chunk, 0);
            let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
            assert!(!place_feature(&mut world, &feature, &mut random, origin));
            assert_eq!(
                random.next_int(),
                WorldgenRandom::new(XoroshiroRandom::new(42)).next_int()
            );
        }
    }

    #[test]
    fn both_columns_grow_into_the_cave_without_replacing_their_bases() {
        let data = data(config());
        let feature = FeatureCompiler::new(&data)
            .configured("test:dripstone")
            .unwrap()
            .unwrap();
        let mut chunk = cave(16, 48);
        let mut world = ChunkFeatureWorld::new(&mut chunk, 0);
        assert!(place_feature(
            &mut world,
            &feature,
            &mut WorldgenRandom::new(XoroshiroRandom::new(42)),
            [8, 32, 8]
        ));
        let stone = block_states().default_state("minecraft:stone").unwrap();
        let dripstone = block_states()
            .default_state("minecraft:dripstone_block")
            .unwrap();
        assert_eq!(world.block_state(8, 16, 8), stone);
        assert_eq!(world.block_state(8, 17, 8), dripstone);
        assert_eq!(world.block_state(8, 47, 8), dripstone);
        assert_eq!(world.block_state(8, 48, 8), stone);
        assert!(block_states().is_air(world.block_state(8, 32, 8)));
    }

    #[test]
    fn a_radius_of_one_succeeds_without_placing_blocks() {
        let mut config = config();
        config["column_radius"] = json!(1);
        let data = data(config);
        let feature = FeatureCompiler::new(&data)
            .configured("test:dripstone")
            .unwrap()
            .unwrap();
        let mut chunk = cave(16, 48);
        let before = chunk.sections().to_vec();
        let mut world = ChunkFeatureWorld::new(&mut chunk, 0);
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
        assert!(place_feature(&mut world, &feature, &mut random, [8, 32, 8]));
        let mut expected = WorldgenRandom::new(XoroshiroRandom::new(42));
        expected.next_int_bounded(1);
        assert_eq!(random.next_int(), expected.next_int());
        assert!(
            before
                .iter()
                .zip(chunk.sections())
                .all(|(before, after)| before.block_states().pack() == after.block_states().pack())
        );
    }

    #[test]
    fn non_replaceable_obstacles_are_skipped_but_replaceable_stone_stops_growth() {
        let data = data(config());
        let config = FeatureCompiler::new(&data)
            .large_dripstone("test", &config())
            .unwrap()
            .unwrap();
        for material in [
            "minecraft:obsidian",
            "minecraft:dripstone_block",
            "minecraft:stone",
        ] {
            let mut chunk = cave(16, 48);
            let mut world = ChunkFeatureWorld::new(&mut chunk, 0);
            let obstacle = block_states().default_state(material).unwrap();
            world.set_block_state(8, 18, 8, obstacle);
            let column = Column {
                root: [8, 16, 8],
                step: 1,
                radius: 4,
                bluntness: 0.7,
                scale: 1.0,
            };
            column.place(
                &mut world,
                &mut WorldgenRandom::new(XoroshiroRandom::new(42)),
                Wind {
                    y: 32,
                    speed: [0.0; 2],
                },
                &config,
            );
            assert_eq!(world.block_state(8, 18, 8), obstacle);
            assert_eq!(
                world.block_state(8, 19, 8) == config.block,
                material != "minecraft:stone"
            );
        }
    }

    #[test]
    fn invalid_or_missing_search_ranges_keep_vanillas_lenient_default() {
        for value in [Value::Null, json!(true), json!("30"), json!(0), json!(-1)] {
            let mut config = config();
            config["floor_to_ceiling_search_range"] = value;
            let data = data(config.clone());
            assert_eq!(
                FeatureCompiler::new(&data)
                    .large_dripstone("test", &config)
                    .unwrap()
                    .unwrap()
                    .search,
                30
            );
        }
        let mut config = config();
        config
            .as_object_mut()
            .unwrap()
            .remove("floor_to_ceiling_search_range");
        let data = data(config.clone());
        assert_eq!(
            FeatureCompiler::new(&data)
                .large_dripstone("test", &config)
                .unwrap()
                .unwrap()
                .search,
            30
        );
    }

    #[test]
    fn malformed_configs_and_out_of_range_providers_are_rejected() {
        for (key, value) in [
            ("column_radius", json!(0)),
            ("column_radius", json!(61)),
            (
                "column_radius",
                json!({"type": "minecraft:uniform", "min_inclusive":8, "max_inclusive":3}),
            ),
            ("height_scale", json!(-0.1)),
            ("height_scale", json!(20.1)),
            ("stalactite_bluntness", json!(0)),
            ("stalagmite_bluntness", json!(10.1)),
            ("wind_speed", json!(2.1)),
            ("min_radius_for_wind", json!(-1)),
            ("min_radius_for_wind", json!(101)),
            ("min_bluntness_for_wind", json!(5.1)),
            ("max_column_radius_to_cave_height_ratio", json!(0.09)),
            ("max_column_radius_to_cave_height_ratio", json!(1.01)),
            (
                "height_scale",
                json!({"type":"minecraft:uniform", "min_inclusive": 1, "max_exclusive": 0}),
            ),
        ] {
            let mut config = config();
            config[key] = value;
            assert!(
                FeatureCompiler::new(&data(config))
                    .configured("test:dripstone")
                    .is_err(),
                "{key}"
            );
        }
        for key in config()
            .as_object()
            .unwrap()
            .keys()
            .filter(|k| k.as_str() != "floor_to_ceiling_search_range")
        {
            let mut config = config();
            config.as_object_mut().unwrap().remove(key);
            assert!(
                FeatureCompiler::new(&data(config))
                    .configured("test:dripstone")
                    .is_err(),
                "missing {key}"
            );
        }
    }
}
