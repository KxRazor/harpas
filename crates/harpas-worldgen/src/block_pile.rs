//! Village piles of hay, melons, pumpkins, snow and ice (`minecraft:block_pile`).

use harpas_core::block::block_states;
use harpas_core::block_behavior::{Face, block_behavior};
use harpas_core::random::{RandomSource, WorldgenRandom};

use crate::configured::StateProvider;
use crate::feature::FeatureWorld;

/// Places a two-layer pile in air above sturdy ground, with a coin flip for dirt paths. Origins below
/// `min_y + 5` fail without drawing random values; other origins succeed even when no block can be placed.
pub fn place_block_pile<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    provider: &StateProvider,
    random: &mut WorldgenRandom<R>,
    [x, y, z]: [i32; 3],
) -> bool {
    if y < world.min_y() + 5 {
        return false;
    }
    let rx = 2 + random.next_int_bounded(2);
    let rz = 2 + random.next_int_bounded(2);
    let states = block_states();
    let path = states.default_state("minecraft:dirt_path").unwrap();
    // betweenClosed visits X first, then Y, then Z. Earlier blocks can support the upper layer.
    for pz in z - rz..=z + rz {
        for py in y..=y + 1 {
            for px in x - rx..=x + rx {
                let (dx, dz) = (x - px, z - pz);
                if (dx * dx + dz * dz) as f32
                    > random.next_float() * 10.0 - random.next_float() * 6.0
                    && random.next_float() >= 0.031
                {
                    continue;
                }
                if !states.is_air(world.block_state(px, py, pz)) {
                    continue;
                }
                let below = world.block_state(px, py - 1, pz);
                let supported = if below == path {
                    random.next_bool()
                } else {
                    block_behavior().is_face_sturdy(below, Face::Up)
                };
                if supported {
                    let state = provider.state(world, random, [px, py, pz]);
                    world.set_block_state(px, py, pz, state);
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FeatureCompiler, GeneratedChunk, RegionFeatureWorld, WorldgenData, place_feature};
    use harpas_core::random::XoroshiroRandom;
    use serde_json::{Value, json};

    fn data(provider: Value) -> WorldgenData {
        WorldgenData::from_json_entries([(
            "data/minecraft/worldgen/configured_feature/test.json",
            json!({"type": "minecraft:block_pile", "config": {"state_provider": provider}})
                .to_string(),
        )])
        .unwrap()
    }

    fn world() -> RegionFeatureWorld {
        RegionFeatureWorld::new([GeneratedChunk::new(0, 0, 0, 32, 1)], 0).unwrap()
    }

    fn state(name: &str) -> u32 {
        block_states().default_state(name).unwrap()
    }

    #[test]
    fn low_and_unwritable_origins_fail_without_drawing_random_values() {
        let data = data(
            json!({"type": "minecraft:simple_state_provider", "state": {"Name": "minecraft:melon"}}),
        );
        let feature = FeatureCompiler::new(&data)
            .configured("minecraft:test")
            .unwrap()
            .unwrap();
        for origin in [[8, 4, 8], [16, 5, 8]] {
            let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
            assert!(!place_feature(&mut world(), &feature, &mut random, origin));
            assert_eq!(
                random.next_int(),
                WorldgenRandom::new(XoroshiroRandom::new(42)).next_int()
            );
        }
    }

    #[test]
    fn unsupported_ground_and_occupied_positions_succeed_without_placing_blocks() {
        let provider = StateProvider::Simple(state("minecraft:melon"));
        for target in ["minecraft:air", "minecraft:water", "minecraft:short_grass"] {
            let mut world = world();
            for x in 0..16 {
                for z in 0..16 {
                    if target != "minecraft:air" {
                        world.set_block_state(x, 7, z, state("minecraft:stone"));
                    }
                    world.set_block_state(x, 8, z, state(target));
                }
            }
            let before = world.chunk(0, 0).unwrap().clone();
            let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
            assert!(place_block_pile(
                &mut world,
                &provider,
                &mut random,
                [8, 8, 8]
            ));
            assert_eq!(world.chunk(0, 0).unwrap().sections(), before.sections());
        }
    }

    #[test]
    fn rotated_provider_uses_default_properties_and_all_three_axes() {
        let data = data(
            json!({"type": "minecraft:rotated_block_provider", "state": {
                "Name": "minecraft:iron_chain", "Properties": {"axis": "z", "waterlogged": "true"}
            }}),
        );
        let mut compiler = FeatureCompiler::new(&data);
        let feature = compiler.configured("minecraft:test").unwrap().unwrap();
        let crate::CompiledFeature::BlockPile(provider) = &*feature else {
            panic!("pile")
        };
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
        let mut axes = std::collections::BTreeSet::new();
        for _ in 0..100 {
            let s = provider.state(&world(), &mut random, [0; 3]);
            let (block, properties) = block_states().properties(s).unwrap();
            assert_eq!(block, "minecraft:iron_chain");
            assert!(properties.contains(&("waterlogged", "false")));
            axes.insert(properties.iter().find(|&&(k, _)| k == "axis").unwrap().1);
        }
        assert_eq!(axes.into_iter().collect::<Vec<_>>(), ["x", "y", "z"]);
    }

    #[test]
    fn unknown_providers_skip_the_pile_and_invalid_rotated_blocks_are_errors() {
        let unsupported = data(json!({"type": "test:unported"}));
        assert!(
            FeatureCompiler::new(&unsupported)
                .configured("minecraft:test")
                .unwrap()
                .is_none()
        );
        for name in ["minecraft:stone", "minecraft:nether_portal", "test:missing"] {
            let invalid =
                data(json!({"type": "minecraft:rotated_block_provider", "state": {"Name": name}}));
            assert!(
                FeatureCompiler::new(&invalid)
                    .configured("minecraft:test")
                    .is_err()
            );
        }
    }
}
