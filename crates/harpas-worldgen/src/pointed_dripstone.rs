//! Small stalactites and stalagmites (`minecraft:pointed_dripstone`), with their spreading base patch.

use harpas_core::block::block_states;
use harpas_core::random::{RandomSource, WorldgenRandom};
use serde_json::Value;

use crate::configured::FeatureCompiler;
use crate::data::WorldgenDataError;
use crate::feature::FeatureWorld;
use crate::placement::{Fluid, StateSet, fluid};

/// A compiled pointed dripstone feature, with the data pack's replaceable stone tag.
#[derive(Debug)]
pub struct CompiledPointedDripstone {
    taller: f32,
    directional: f32,
    radius2: f32,
    radius3: f32,
    replaceable: StateSet,
    base: u32,
    // Direction (down, up), thickness (tip, frustum), waterlogging (dry, wet).
    states: [[[u32; 2]; 2]; 2],
}

// Direction.getRandom uses enum order, whereas the patch visits the horizontal plane clockwise.
const DIRECTIONS: [[i32; 3]; 6] = [
    [0, -1, 0],
    [0, 1, 0],
    [0, 0, -1],
    [0, 0, 1],
    [-1, 0, 0],
    [1, 0, 0],
];

fn offset(a: [i32; 3], b: [i32; 3]) -> [i32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

/// Places pointed dripstone (`PointedDripstoneFeature.place`). A base above or below is required; when both
/// qualify, a boolean chooses the ceiling. Patch spread draws continue even through unreplaceable blocks.
/// The origin itself is overwritten, and a second block grows only into air or a water block.
pub fn place_pointed_dripstone<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    dripstone: &CompiledPointedDripstone,
    random: &mut WorldgenRandom<R>,
    [x, y, z]: [i32; 3],
) -> bool {
    let is_base = |state| state == dripstone.base || dripstone.replaceable.contains(state);
    let above = is_base(world.block_state(x, y + 1, z));
    let below = is_base(world.block_state(x, y - 1, z));
    let down = match (above, below) {
        (false, false) => return false,
        (true, true) => random.next_bool(),
        (true, false) => true,
        (false, true) => false,
    };
    let step = if down { -1 } else { 1 };
    let base = [x, y - step, z];
    let replace = |world: &mut W, [px, py, pz]: [i32; 3]| {
        if dripstone
            .replaceable
            .contains(world.block_state(px, py, pz))
        {
            world.set_block_state(px, py, pz, dripstone.base);
        }
    };
    replace(world, base);
    for direction in [[0, 0, -1], [1, 0, 0], [0, 0, 1], [-1, 0, 0]] {
        if random.next_float() < dripstone.directional {
            let mut at = offset(base, direction);
            replace(world, at);
            if random.next_float() < dripstone.radius2 {
                at = offset(at, DIRECTIONS[random.next_int_bounded(6) as usize]);
                replace(world, at);
                if random.next_float() < dripstone.radius3 {
                    at = offset(at, DIRECTIONS[random.next_int_bounded(6) as usize]);
                    replace(world, at);
                }
            }
        }
    }
    let height = if random.next_float() < dripstone.taller
        && (block_states().is_air(world.block_state(x, y + step, z))
            || block_states()
                .properties(world.block_state(x, y + step, z))
                .is_some_and(|(name, _)| name == "minecraft:water"))
    {
        2
    } else {
        1
    };
    for i in 0..height {
        let py = y + i * step;
        let wet = matches!(
            fluid(world.block_state(x, py, z)),
            Fluid::Water | Fluid::FlowingWater
        );
        let state =
            dripstone.states[usize::from(!down)][usize::from(i + 1 < height)][usize::from(wet)];
        world.set_block_state(x, py, z, state);
    }
    true
}

impl FeatureCompiler<'_> {
    pub(crate) fn pointed_dripstone(
        &self,
        name: &str,
        config: &Value,
    ) -> Result<CompiledPointedDripstone, WorldgenDataError> {
        let invalid = |message: String| WorldgenDataError::InvalidDefinition {
            context: name.to_string(),
            message,
        };
        let object = config
            .as_object()
            .ok_or_else(|| invalid("pointed dripstone config must be an object".into()))?;
        let probability = |key: &str, default: f32| {
            object.get(key).map_or(Ok(default), |value| {
                value
                    .as_f64()
                    .filter(|value| (0.0..=1.0).contains(value))
                    .map(|value| value as f32)
                    .ok_or_else(|| invalid(format!("`{key}` must be between 0 and 1")))
            })
        };
        let mut states = [[[0; 2]; 2]; 2];
        for (direction, vertical) in ["down", "up"].into_iter().enumerate() {
            for (thickness, shape) in ["tip", "frustum"].into_iter().enumerate() {
                for (wet, waterlogged) in ["false", "true"].into_iter().enumerate() {
                    states[direction][thickness][wet] = block_states()
                        .state(
                            "minecraft:pointed_dripstone",
                            [
                                ("vertical_direction", vertical),
                                ("thickness", shape),
                                ("waterlogged", waterlogged),
                            ],
                        )
                        .unwrap()
                        .0;
                }
            }
        }
        Ok(CompiledPointedDripstone {
            taller: probability("chance_of_taller_dripstone", 0.2)?,
            directional: probability("chance_of_directional_spread", 0.7)?,
            radius2: probability("chance_of_spread_radius2", 0.5)?,
            radius3: probability("chance_of_spread_radius3", 0.5)?,
            replaceable: StateSet::from_membership(
                &self
                    .data()
                    .block_tag_states("minecraft:dripstone_replaceable_blocks")?,
            ),
            base: block_states()
                .default_state("minecraft:dripstone_block")
                .unwrap(),
            states,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GeneratedChunk, RegionFeatureWorld, WorldgenData, place_feature};
    use harpas_core::random::XoroshiroRandom;
    use serde_json::json;

    fn data(config: Value) -> WorldgenData {
        WorldgenData::from_json_entries([
            (
                "data/test/worldgen/configured_feature/dripstone.json",
                json!({"type": "minecraft:pointed_dripstone", "config": config}).to_string(),
            ),
            (
                "data/minecraft/tags/block/dripstone_replaceable_blocks.json",
                json!({"values": ["minecraft:stone"]}).to_string(),
            ),
        ])
        .unwrap()
    }

    fn state(name: &str) -> u32 {
        block_states()
            .default_state(&format!("minecraft:{name}"))
            .unwrap()
    }

    fn world() -> RegionFeatureWorld {
        RegionFeatureWorld::new([GeneratedChunk::new(0, 0, 0, 32, 1)], 0).unwrap()
    }

    #[test]
    fn no_base_or_unwritable_origin_draws_no_random_values() {
        let data = data(json!({}));
        let feature = FeatureCompiler::new(&data)
            .configured("test:dripstone")
            .unwrap()
            .unwrap();
        let mut world = world();
        for at in [[8, 8, 8], [32, 8, 8]] {
            let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
            assert!(!place_feature(&mut world, &feature, &mut random, at));
            assert_eq!(
                random.next_int(),
                WorldgenRandom::new(XoroshiroRandom::new(42)).next_int()
            );
        }
    }

    #[test]
    fn tall_waterlogged_column_keeps_its_base_and_stops_at_obstacles() {
        let data =
            data(json!({"chance_of_taller_dripstone": 1, "chance_of_directional_spread": 0}));
        let feature = FeatureCompiler::new(&data)
            .configured("test:dripstone")
            .unwrap()
            .unwrap();
        for down in [false, true] {
            for blocked in [false, true] {
                let mut world = world();
                let step = if down { -1 } else { 1 };
                world.set_block_state(8, 8 - step, 8, state("stone"));
                world.set_block_state(8, 8, 8, state("water"));
                let front = state(if blocked { "dirt" } else { "water" });
                world.set_block_state(8, 8 + step, 8, front);
                assert!(place_feature(
                    &mut world,
                    &feature,
                    &mut WorldgenRandom::new(XoroshiroRandom::new(42)),
                    [8, 8, 8]
                ));
                assert_eq!(world.block_state(8, 8 - step, 8), state("dripstone_block"));
                let expected = |shape| {
                    block_states()
                        .state(
                            "minecraft:pointed_dripstone",
                            [
                                ("vertical_direction", if down { "down" } else { "up" }),
                                ("thickness", shape),
                                ("waterlogged", "true"),
                            ],
                        )
                        .unwrap()
                        .0
                };
                assert_eq!(
                    world.block_state(8, 8, 8),
                    expected(if blocked { "tip" } else { "frustum" })
                );
                assert_eq!(
                    world.block_state(8, 8 + step, 8),
                    if blocked { front } else { expected("tip") }
                );
            }
        }
    }

    #[test]
    fn probabilities_default_independently_and_reject_invalid_values() {
        for key in [
            "chance_of_taller_dripstone",
            "chance_of_directional_spread",
            "chance_of_spread_radius2",
            "chance_of_spread_radius3",
        ] {
            for value in [
                json!(-0.1),
                json!(1.1),
                json!(true),
                json!("0.5"),
                Value::Null,
            ] {
                let data = data(json!({key: value}));
                assert!(
                    FeatureCompiler::new(&data)
                        .configured("test:dripstone")
                        .is_err(),
                    "{key}"
                );
            }
            for value in [0, 1] {
                let data = data(json!({key: value}));
                assert!(
                    FeatureCompiler::new(&data)
                        .configured("test:dripstone")
                        .unwrap()
                        .is_some()
                );
            }
        }
        let data = data(json!({}));
        let feature = FeatureCompiler::new(&data)
            .pointed_dripstone("test", &json!({}))
            .unwrap();
        assert_eq!(
            [
                feature.taller,
                feature.directional,
                feature.radius2,
                feature.radius3
            ],
            [0.2, 0.7, 0.5, 0.5]
        );
    }
}
