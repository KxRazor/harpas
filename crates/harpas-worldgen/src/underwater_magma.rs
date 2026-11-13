//! `minecraft:underwater_magma`: searches down a water column and replaces enclosed blocks around its
//! floor with magma. The destination must be neither air nor water, and the blocks below and beside it
//! must close their faces towards it with their occlusion shapes.

use harpas_core::block::block_states;
use harpas_core::block_behavior::{Face, block_behavior};
use harpas_core::random::{RandomSource, WorldgenRandom};
use serde_json::Value;

use crate::configured::FeatureCompiler;
use crate::data::WorldgenDataError;
use crate::feature::FeatureWorld;

/// A compiled `minecraft:underwater_magma` feature.
#[derive(Debug)]
pub struct CompiledUnderwaterMagma {
    search_range: i32,
    radius: i32,
    probability: f32,
    magma: u32,
}

fn is_water(state: u32) -> bool {
    block_states()
        .properties(state)
        .is_some_and(|(name, _)| name == "minecraft:water")
}

/// Places underwater magma (`UnderwaterMagmaFeature.place`). The origin must be water and the first
/// non-water block below it must be strictly less than `floor_search_range` blocks away. Every position
/// of the floor-centered cube draws a float before testing its blocks, in X, then Y, then Z order.
pub fn place_underwater_magma<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    magma: &CompiledUnderwaterMagma,
    random: &mut WorldgenRandom<R>,
    [x, y, z]: [i32; 3],
) -> bool {
    if !is_water(world.block_state(x, y, z)) {
        return false;
    }
    let Some(floor) = (1..magma.search_range)
        .map(|distance| y - distance)
        .find(|&py| !is_water(world.block_state(x, py, z)))
    else {
        return false;
    };
    let mut placed = false;
    for pz in z - magma.radius..=z + magma.radius {
        for py in floor - magma.radius..=floor + magma.radius {
            for px in x - magma.radius..=x + magma.radius {
                if random.next_float() < magma.probability
                    && !block_states().is_air(world.block_state(px, py, pz))
                    && !is_water(world.block_state(px, py, pz))
                    && block_behavior()
                        .is_occlusion_face_full(world.block_state(px, py - 1, pz), Face::Up)
                    && [
                        (0, -1, Face::South),
                        (1, 0, Face::West),
                        (0, 1, Face::North),
                        (-1, 0, Face::East),
                    ]
                    .into_iter()
                    .all(|(dx, dz, face)| {
                        block_behavior()
                            .is_occlusion_face_full(world.block_state(px + dx, py, pz + dz), face)
                    })
                {
                    world.set_block_state(px, py, pz, magma.magma);
                    placed = true;
                }
            }
        }
    }
    placed
}

impl FeatureCompiler<'_> {
    pub(crate) fn underwater_magma(
        &self,
        name: &str,
        config: &Value,
    ) -> Result<CompiledUnderwaterMagma, WorldgenDataError> {
        let invalid = |message: String| WorldgenDataError::InvalidDefinition {
            context: name.to_string(),
            message,
        };
        let integer = |key: &str, max: i32| {
            config
                .get(key)
                .and_then(Value::as_i64)
                .filter(|&value| (0..=i64::from(max)).contains(&value))
                .map(|value| value as i32)
                .ok_or_else(|| invalid(format!("`{key}` must be an integer between 0 and {max}")))
        };
        let probability = config
            .get("placement_probability_per_valid_position")
            .and_then(Value::as_f64)
            .filter(|value| (0.0..=1.0).contains(value))
            .ok_or_else(|| {
                invalid("`placement_probability_per_valid_position` must be between 0 and 1".into())
            })? as f32;
        Ok(CompiledUnderwaterMagma {
            search_range: integer("floor_search_range", 512)?,
            radius: integer("placement_radius_around_floor", 64)?,
            probability,
            magma: block_states()
                .default_state("minecraft:magma_block")
                .unwrap(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChunkFeatureWorld, GeneratedChunk, WorldgenData, place_feature};
    use harpas_core::random::XoroshiroRandom;
    use serde_json::json;

    fn config() -> Value {
        json!({"floor_search_range": 5, "placement_radius_around_floor": 1,
            "placement_probability_per_valid_position": 1.0})
    }

    fn data(config: Value) -> WorldgenData {
        WorldgenData::from_json_entries([(
            "data/test/worldgen/configured_feature/magma.json",
            json!({"type": "minecraft:underwater_magma", "config": config}).to_string(),
        )])
        .unwrap()
    }

    fn state(name: &str) -> u32 {
        block_states()
            .default_state(&format!("minecraft:{name}"))
            .unwrap()
    }

    fn world() -> GeneratedChunk {
        let mut chunk = GeneratedChunk::new(0, 0, 0, 32, 1);
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    chunk.set_block_state(x, y, z, state(if y <= 8 { "stone" } else { "water" }));
                }
            }
        }
        chunk
    }

    #[test]
    fn a_failed_floor_search_draws_no_random_values() {
        let data = data(config());
        let feature = FeatureCompiler::new(&data)
            .configured("test:magma")
            .unwrap()
            .unwrap();
        for origin in [[8, 8, 8], [8, 13, 8], [32, 12, 8]] {
            let mut chunk = world();
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
    fn zero_probability_still_draws_for_every_position_of_the_cube() {
        let mut config = config();
        config["placement_probability_per_valid_position"] = json!(0.0);
        let data = data(config);
        let feature = FeatureCompiler::new(&data)
            .configured("test:magma")
            .unwrap()
            .unwrap();
        let mut chunk = world();
        let mut world = ChunkFeatureWorld::new(&mut chunk, 0);
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
        assert!(!place_feature(
            &mut world,
            &feature,
            &mut random,
            [8, 12, 8]
        ));
        let mut expected = WorldgenRandom::new(XoroshiroRandom::new(42));
        for _ in 0..27 {
            expected.next_float();
        }
        assert_eq!(random.next_int(), expected.next_int());
        assert_eq!(world.block_state(8, 8, 8), state("stone"));
    }

    #[test]
    fn submerged_floor_places_below_the_surface_and_keeps_exposed_blocks() {
        let data = data(config());
        let feature = FeatureCompiler::new(&data)
            .configured("test:magma")
            .unwrap()
            .unwrap();
        let mut chunk = world();
        let mut world = ChunkFeatureWorld::new(&mut chunk, 0);
        world.set_block_state(10, 8, 8, state("water"));
        assert!(place_feature(
            &mut world,
            &feature,
            &mut WorldgenRandom::new(XoroshiroRandom::new(42)),
            [8, 12, 8]
        ));
        assert_eq!(world.block_state(8, 8, 8), state("magma_block"));
        assert_eq!(world.block_state(8, 7, 8), state("magma_block"));
        assert_eq!(world.block_state(8, 9, 8), state("water"));
        assert_eq!(world.block_state(9, 8, 8), state("stone"));
    }

    #[test]
    fn invalid_configuration_is_reported_instead_of_skipped() {
        for (key, invalids) in [
            (
                "floor_search_range",
                vec![json!(-1), json!(513), json!(1.5), Value::Null],
            ),
            (
                "placement_radius_around_floor",
                vec![json!(-1), json!(65), json!("1"), Value::Null],
            ),
            (
                "placement_probability_per_valid_position",
                vec![json!(-0.1), json!(1.1), json!(true), Value::Null],
            ),
        ] {
            for value in invalids {
                let mut config = config();
                config[key] = value;
                let data = data(config);
                assert!(
                    FeatureCompiler::new(&data)
                        .configured("test:magma")
                        .is_err(),
                    "{key}"
                );
            }
        }
        for (range, radius) in [(0, 0), (512, 64)] {
            let mut config = config();
            config["floor_search_range"] = json!(range);
            config["placement_radius_around_floor"] = json!(radius);
            let data = data(config);
            assert!(
                FeatureCompiler::new(&data)
                    .configured("test:magma")
                    .unwrap()
                    .is_some()
            );
        }
    }
}
