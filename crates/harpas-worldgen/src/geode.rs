//! `minecraft:geode`: noisy hollow shells, optional cracks and directional inner crystals.
//! The shape noise uses the world seed, independently of the feature's decoration random source.
//! Fluid ticks requested beside a crack follow with scheduled ticks in M4.

use harpas_core::block::block_states;
use harpas_core::noise::NormalNoise;
use harpas_core::random::{LegacyRandom, RandomSource, WorldgenRandom};
use serde_json::Value;

use crate::configured::{FeatureCompiler, StateProvider};
use crate::data::{IntProviderDef, WorldgenDataError, parse_block_state_def, parse_int_provider};
use crate::feature::FeatureWorld;
use crate::feature_provider::int_bounds;
use crate::placement::{Fluid, StateSet, fluid, sample_int_provider};
use crate::shape_update::{Direction, with_property};
use crate::surface::resolve_block_state;

/// A compiled `GeodeConfiguration`, including its providers and block tags.
#[derive(Debug)]
pub struct CompiledGeode {
    providers: [StateProvider; 5],
    placements: Vec<InnerPlacement>,
    cannot_replace: StateSet,
    invalid_blocks: StateSet,
    layers: [f64; 4],
    crack_chance: f64,
    crack_size: f64,
    crack_offset: i32,
    placement_chance: f64,
    alternate_chance: f64,
    require_alternate: bool,
    wall_distance: IntProviderDef,
    points: IntProviderDef,
    point_offset: IntProviderDef,
    offsets: [i32; 2],
    noise_multiplier: f64,
    invalid_threshold: i32,
}

#[derive(Debug)]
struct InnerPlacement {
    state: u32,
    facing: bool,
}

fn distance_squared(a: [i32; 3], b: [i32; 3]) -> f64 {
    let [x, y, z] = std::array::from_fn(|i| f64::from(a[i]) - f64::from(b[i]));
    x * x + y * y + z * z
}

fn safe_set(world: &mut impl FeatureWorld, config: &CompiledGeode, p: [i32; 3], state: u32) {
    if !config
        .cannot_replace
        .contains(world.block_state(p[0], p[1], p[2]))
    {
        world.set_block_state(p[0], p[1], p[2], state);
    }
}

/// Places a geode (`GeodeFeature.place`), keeping vanilla's point, block and direction iteration order.
pub fn place_geode<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    config: &CompiledGeode,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    let count = sample_int_provider(&config.points, random).unwrap();
    let mut noise_random = WorldgenRandom::new(LegacyRandom::new(world.seed()));
    let noise = NormalNoise::new(&mut noise_random, -4, &[1.0]);
    let scale = f64::from(count) / f64::from(int_bounds(&config.wall_distance).unwrap().1);
    let [filling, inner, middle, outer] =
        std::array::from_fn(|i| 1.0 / (config.layers[i] + if i == 0 { 0.0 } else { scale }).sqrt());
    let crack_threshold = 1.0
        / (config.crack_size + random.next_double() / 2.0 + if count > 3 { scale } else { 0.0 })
            .sqrt();
    let crack = f64::from(random.next_float()) < config.crack_chance;
    let mut invalid = 0;
    let mut points = Vec::new();
    for _ in 0..count {
        let p = origin.map(|v| v + sample_int_provider(&config.wall_distance, random).unwrap());
        let state = world.block_state(p[0], p[1], p[2]);
        if block_states().is_air(state) || config.invalid_blocks.contains(state) {
            invalid += 1;
            if invalid > config.invalid_threshold {
                return false;
            }
        }
        points.push((
            p,
            sample_int_provider(&config.point_offset, random).unwrap(),
        ));
    }
    let mut cracks = Vec::new();
    if crack {
        let width = count * 2 + 1;
        let (x, z) = match random.next_int_bounded(4) {
            0 => (width, 0),
            1 => (0, width),
            2 => (width, width),
            _ => (0, 0),
        };
        for y in [7, 5, 1] {
            cracks.push([origin[0] + x, origin[1] + y, origin[2] + z]);
        }
    }
    let air = block_states().default_state("minecraft:air").unwrap();
    let mut potential = Vec::new();
    let [min, max] = [
        config.offsets[0].min(config.offsets[1]),
        config.offsets[0].max(config.offsets[1]),
    ];
    // BlockPos.betweenClosed: X changes first, then Y, then Z.
    for z in origin[2] + min..=origin[2] + max {
        for y in origin[1] + min..=origin[1] + max {
            for x in origin[0] + min..=origin[0] + max {
                let p = [x, y, z];
                let perturbation = noise.get_value(f64::from(x), f64::from(y), f64::from(z))
                    * config.noise_multiplier;
                let mut density = 0.0;
                let mut crack_density = 0.0;
                for &(point, offset) in &points {
                    density += 1.0 / (distance_squared(p, point) + f64::from(offset)).sqrt()
                        + perturbation;
                }
                for &point in &cracks {
                    crack_density += 1.0
                        / (distance_squared(p, point) + f64::from(config.crack_offset)).sqrt()
                        + perturbation;
                }
                if density < outer {
                    continue;
                }
                if crack && crack_density >= crack_threshold && density < filling {
                    safe_set(world, config, p, air);
                } else if density >= filling {
                    let state = config.providers[0].state(world, random, p);
                    safe_set(world, config, p, state);
                } else if density >= inner {
                    let alternate = f64::from(random.next_float()) < config.alternate_chance;
                    let state =
                        config.providers[if alternate { 2 } else { 1 }].state(world, random, p);
                    safe_set(world, config, p, state);
                    // A protected block can still be a potential crystal position.
                    if (!config.require_alternate || alternate)
                        && f64::from(random.next_float()) < config.placement_chance
                    {
                        potential.push(p);
                    }
                } else {
                    let state = config.providers[if density >= middle { 3 } else { 4 }]
                        .state(world, random, p);
                    safe_set(world, config, p, state);
                }
            }
        }
    }
    for p in potential {
        let placement =
            &config.placements[random.next_int_bounded(config.placements.len() as i32) as usize];
        let mut state = placement.state;
        for direction in Direction::ALL {
            if placement.facing {
                state = with_property(state, "facing", direction.name());
            }
            let at = direction.step(p);
            let neighbor = world.block_state(at[0], at[1], at[2]);
            state = with_property(
                state,
                "waterlogged",
                if matches!(fluid(neighbor), Fluid::Water | Fluid::Lava) {
                    "true"
                } else {
                    "false"
                },
            );
            let water = block_states()
                .properties(neighbor)
                .is_some_and(|(name, properties)| {
                    name == "minecraft:water"
                        && properties.iter().any(|&(key, value)| {
                            key == "level"
                                && (value == "0" || value.parse::<u8>().is_ok_and(|v| v >= 8))
                        })
                });
            if block_states().is_air(neighbor) || water {
                safe_set(world, config, at, state);
                break;
            }
        }
    }
    true
}

impl FeatureCompiler<'_> {
    pub(crate) fn geode(
        &mut self,
        name: &str,
        config: &Value,
    ) -> Result<Option<CompiledGeode>, WorldgenDataError> {
        let invalid = |message: String| WorldgenDataError::InvalidDefinition {
            context: name.into(),
            message,
        };
        let field = |value: &Value, key: &str| {
            value
                .get(key)
                .cloned()
                .ok_or_else(|| invalid(format!("missing `{key}`")))
        };
        let blocks = field(config, "blocks")?;
        let layers = field(config, "layers")?;
        let crack = field(config, "crack")?;
        let mut providers = Vec::new();
        for key in [
            "filling_provider",
            "inner_layer_provider",
            "alternate_inner_layer_provider",
            "middle_layer_provider",
            "outer_layer_provider",
        ] {
            let Some(provider) = self.provider(name, &field(&blocks, key)?)? else {
                return Ok(None);
            };
            providers.push(provider);
        }
        let placements: Vec<_> = field(&blocks, "inner_placements")?
            .as_array()
            .ok_or_else(|| invalid("`inner_placements` must be a nonempty list".into()))?
            .iter()
            .map(|v| {
                let state = resolve_block_state(&parse_block_state_def(name, v)?, name)?;
                // FACING has all six values. HORIZONTAL_FACING and FACING_HOPPER share its name,
                // but are different properties and GeodeFeature leaves them unchanged.
                let facing = block_states()
                    .properties(state)
                    .and_then(|(block, _)| block_states().state(block, [("facing", "up")]))
                    .is_some_and(|(_, ignored)| ignored.is_empty());
                Ok(InnerPlacement { state, facing })
            })
            .collect::<Result<_, WorldgenDataError>>()?;
        if placements.is_empty() {
            return Err(invalid("`inner_placements` must be a nonempty list".into()));
        }
        let tag = |key: &str| -> Result<StateSet, WorldgenDataError> {
            let v = field(&blocks, key)?;
            let tag = v
                .as_str()
                .and_then(|s| s.strip_prefix('#'))
                .ok_or_else(|| invalid(format!("`{key}` must be a block tag")))?;
            Ok(StateSet::from_membership(
                &self.data().block_tag_states(tag)?,
            ))
        };
        let number = |value: &Value, key: &str, min: f64, max: f64, default: f64| {
            value
                .get(key)
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite() && (min..=max).contains(v))
                .unwrap_or(default)
        };
        let integer = |value: &Value, key: &str| {
            value
                .get(key)
                .and_then(Value::as_i64)
                .and_then(|v| i32::try_from(v).ok())
        };
        let provider = |key: &str, low, high, default: IntProviderDef| {
            let parsed = config
                .get(key)
                .and_then(|v| parse_int_provider(name, v).ok())
                .unwrap_or(default.clone());
            let (min, max) = int_bounds(&parsed)?;
            Some(if min < low || max > high || min > max {
                default
            } else {
                parsed
            })
        };
        let (Some(wall_distance), Some(points), Some(point_offset)) = (
            provider(
                "outer_wall_distance",
                1,
                20,
                IntProviderDef::Uniform {
                    min_inclusive: 4,
                    max_inclusive: 5,
                },
            ),
            provider(
                "distribution_points",
                1,
                20,
                IntProviderDef::Uniform {
                    min_inclusive: 3,
                    max_inclusive: 4,
                },
            ),
            provider(
                "point_offset",
                0,
                10,
                IntProviderDef::Uniform {
                    min_inclusive: 1,
                    max_inclusive: 2,
                },
            ),
        ) else {
            return Ok(None);
        };
        Ok(Some(CompiledGeode {
            providers: providers.try_into().unwrap(),
            placements,
            cannot_replace: tag("cannot_replace")?,
            invalid_blocks: tag("invalid_blocks")?,
            layers: [
                ("filling", 1.7),
                ("inner_layer", 2.2),
                ("middle_layer", 3.2),
                ("outer_layer", 4.2),
            ]
            .map(|(key, default)| number(&layers, key, 0.01, 50.0, default)),
            crack_chance: number(&crack, "generate_crack_chance", 0.0, 1.0, 1.0),
            crack_size: number(&crack, "base_crack_size", 0.0, 5.0, 2.0),
            crack_offset: integer(&crack, "crack_point_offset")
                .filter(|v| (0..=10).contains(v))
                .unwrap_or(2),
            placement_chance: number(config, "use_potential_placements_chance", 0.0, 1.0, 0.35),
            alternate_chance: number(config, "use_alternate_layer0_chance", 0.0, 1.0, 0.0),
            require_alternate: config
                .get("placements_require_layer0_alternate")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            wall_distance,
            points,
            point_offset,
            offsets: [
                integer(config, "min_gen_offset").unwrap_or(-16),
                integer(config, "max_gen_offset").unwrap_or(16),
            ],
            noise_multiplier: number(config, "noise_multiplier", 0.0, 1.0, 0.05),
            invalid_threshold: integer(config, "invalid_blocks_threshold")
                .ok_or_else(|| invalid("missing or invalid `invalid_blocks_threshold`".into()))?,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChunkFeatureWorld, CompiledFeature, GeneratedChunk, WorldgenData, place_feature};
    use harpas_core::random::XoroshiroRandom;
    use serde_json::json;

    fn config() -> Value {
        let provider =
            |name| json!({"type": "minecraft:simple_state_provider", "state": {"Name": name}});
        json!({"blocks": {
            "filling_provider": provider("minecraft:air"),
            "inner_layer_provider": provider("minecraft:amethyst_block"),
            "alternate_inner_layer_provider": provider("minecraft:budding_amethyst"),
            "middle_layer_provider": provider("minecraft:calcite"),
            "outer_layer_provider": provider("minecraft:smooth_basalt"),
            "inner_placements": [{"Name": "minecraft:amethyst_cluster"}],
            "cannot_replace": "#test:protected", "invalid_blocks": "#test:invalid"
        }, "layers": {}, "crack": {}, "invalid_blocks_threshold": 1,
            "outer_wall_distance": 4, "distribution_points": 3, "point_offset": 1})
    }

    fn data(config: Value) -> WorldgenData {
        WorldgenData::from_json_entries([
            (
                "data/test/worldgen/configured_feature/geode.json",
                json!({"type": "minecraft:geode", "config": config}).to_string(),
            ),
            (
                "data/test/tags/block/protected.json",
                json!({"values": ["minecraft:bedrock"]}).to_string(),
            ),
            (
                "data/test/tags/block/invalid.json",
                json!({"values": ["minecraft:lava"]}).to_string(),
            ),
        ])
        .unwrap()
    }

    fn compiled(data: &WorldgenData) -> std::sync::Arc<CompiledFeature> {
        FeatureCompiler::new(data)
            .configured("test:geode")
            .unwrap()
            .unwrap()
    }

    #[test]
    fn rejects_invalid_distribution_points_before_writing() {
        let data = data(config());
        let feature = compiled(&data);
        for material in ["minecraft:air", "minecraft:lava"] {
            let mut chunk = GeneratedChunk::new(0, 0, 0, 32, 1);
            let state = block_states().default_state(material).unwrap();
            chunk.set_block_state(8, 12, 8, state);
            let before = chunk.clone();
            let mut world = ChunkFeatureWorld::new(&mut chunk, 0);
            let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
            assert!(!place_feature(&mut world, &feature, &mut random, [4, 8, 4]));
            let mut expected = WorldgenRandom::new(XoroshiroRandom::new(42));
            expected.next_double();
            expected.next_float();
            // Constant providers draw nothing, even though the feature visits the same invalid point twice.
            assert_eq!(random.next_int(), expected.next_int());
            for y in 0..32 {
                for z in 0..16 {
                    for x in 0..16 {
                        assert_eq!(chunk.block_state(x, y, z), before.block_state(x, y, z));
                    }
                }
            }
        }
    }

    #[test]
    fn protected_blocks_keep_their_state() {
        let data = data(config());
        let feature = compiled(&data);
        let bedrock = block_states().default_state("minecraft:bedrock").unwrap();
        let mut chunk = GeneratedChunk::new(0, 0, 0, 32, 1);
        for y in 0..32 {
            for z in 0..16 {
                for x in 0..16 {
                    chunk.set_block_state(x, y, z, bedrock);
                }
            }
        }
        let mut world = ChunkFeatureWorld::new(&mut chunk, 0);
        world.start_region(123, 0, 0);
        assert!(place_feature(
            &mut world,
            &feature,
            &mut WorldgenRandom::new(XoroshiroRandom::new(42)),
            [4, 8, 4]
        ));
        for y in 0..32 {
            for z in 0..16 {
                for x in 0..16 {
                    assert_eq!(chunk.block_state(x, y, z), bedrock);
                }
            }
        }
    }

    #[test]
    fn optional_fields_keep_vanillas_lenient_defaults() {
        let mut config = config();
        config["outer_wall_distance"] = json!(0);
        config["point_offset"] = json!(11);
        config["layers"]["filling"] = json!(-1);
        config["crack"]["generate_crack_chance"] = json!(2);
        let data = data(config);
        let feature = compiled(&data);
        let CompiledFeature::Geode(geode) = feature.as_ref() else {
            panic!("geode")
        };
        assert_eq!(int_bounds(&geode.wall_distance), Some((4, 5)));
        assert_eq!(int_bounds(&geode.point_offset), Some((1, 2)));
        assert_eq!(geode.layers, [1.7, 2.2, 3.2, 4.2]);
        assert_eq!(geode.crack_chance, 1.0);
    }

    #[test]
    fn unsupported_providers_skip_the_whole_feature_and_invalid_required_fields_fail() {
        for (key, value) in [
            ("inner_placements", json!([])),
            ("invalid_blocks", json!("minecraft:lava")),
        ] {
            let mut config = config();
            config["blocks"][key] = value;
            let data = data(config);
            assert!(
                FeatureCompiler::new(&data)
                    .configured("test:geode")
                    .is_err()
            );
        }
        let mut config = config();
        config["blocks"]["filling_provider"]["type"] = json!("test:unported");
        let data = data(config);
        assert!(
            FeatureCompiler::new(&data)
                .configured("test:geode")
                .unwrap()
                .is_none()
        );
    }
}
