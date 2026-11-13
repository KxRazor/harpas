//! `minecraft:block_blob`: the mossy cobblestone boulders of old growth taigas. A blob searches downward
//! for its configured support, then places three overlapping rounded lumps, drifting down and toward
//! negative X and Z between them. The lumps replace every block in their volume.

use harpas_core::random::{RandomSource, WorldgenRandom};

use crate::feature::FeatureWorld;
use crate::placement::BlockPredicate;

/// Places a block blob (`BlockBlobFeature.place`). The support predicate is evaluated below the origin,
/// including air if the predicate permits it; reaching `min_y + 3` aborts without drawing
/// random values.
pub fn place_block_blob<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    state: u32,
    can_place_on: &BlockPredicate,
    random: &mut WorldgenRandom<R>,
    [mut x, mut y, mut z]: [i32; 3],
) -> bool {
    while y > world.min_y() + 3 {
        if can_place_on.test(world, [x, y - 1, z]) {
            break;
        }
        y -= 1;
    }
    if y <= world.min_y() + 3 {
        return false;
    }
    for _ in 0..3 {
        let rx = random.next_int_bounded(2);
        let ry = random.next_int_bounded(2);
        let rz = random.next_int_bounded(2);
        let radius = (rx + ry + rz) as f32 * 0.333 + 0.5;
        for pz in z - rz..=z + rz {
            for py in y - ry..=y + ry {
                for px in x - rx..=x + rx {
                    let (dx, dy, dz) = (px - x, py - y, pz - z);
                    if f64::from(dx * dx + dy * dy + dz * dz) <= f64::from(radius * radius) {
                        world.set_block_state(px, py, pz, state);
                    }
                }
            }
        }
        x += random.next_int_bounded(2) - 1;
        y -= random.next_int_bounded(2);
        z += random.next_int_bounded(2) - 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FeatureCompiler, GeneratedChunk, RegionFeatureWorld, WorldgenData, place_feature};
    use harpas_core::block::block_states;
    use harpas_core::random::XoroshiroRandom;
    use serde_json::{Value, json};

    fn data(predicate: Value) -> WorldgenData {
        WorldgenData::from_json_entries([(
            "data/minecraft/worldgen/configured_feature/test.json",
            json!({
                "type": "minecraft:block_blob",
                "config": {"state": {"Name": "minecraft:mossy_cobblestone"}, "can_place_on": predicate}
            })
            .to_string(),
        )])
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
    fn missing_support_and_the_bottom_margin_draw_no_random_values() {
        let data = data(json!({"type": "minecraft:matching_blocks", "blocks": "minecraft:stone"}));
        let feature = FeatureCompiler::new(&data)
            .configured("minecraft:test")
            .unwrap()
            .unwrap();
        for support in [None, Some(2)] {
            let mut world = world();
            if let Some(y) = support {
                world.set_block_state(8, y, 8, state("stone"));
            }
            let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
            assert!(!place_feature(
                &mut world,
                &feature,
                &mut random,
                [8, 20, 8]
            ));
            assert_eq!(
                random.next_int(),
                WorldgenRandom::new(XoroshiroRandom::new(42)).next_int()
            );
        }
    }

    #[test]
    fn the_support_predicate_can_accept_air() {
        let data = data(json!({"type": "minecraft:true"}));
        let feature = FeatureCompiler::new(&data)
            .configured("minecraft:test")
            .unwrap()
            .unwrap();
        let mut world = world();
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
        assert!(place_feature(&mut world, &feature, &mut random, [8, 20, 8]));
        assert_eq!(world.block_state(8, 20, 8), state("mossy_cobblestone"));
    }

    #[test]
    fn searches_through_unsupported_blocks_and_replaces_the_lump_volume() {
        let data = data(json!({"type": "minecraft:matching_blocks", "blocks": "minecraft:dirt"}));
        let feature = FeatureCompiler::new(&data)
            .configured("minecraft:test")
            .unwrap()
            .unwrap();
        let mut world = world();
        world.set_block_state(8, 10, 8, state("dirt"));
        world.set_block_state(8, 11, 8, state("water"));
        world.set_block_state(8, 15, 8, state("stone"));
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
        assert!(place_feature(&mut world, &feature, &mut random, [8, 20, 8]));
        assert_eq!(world.block_state(8, 11, 8), state("mossy_cobblestone"));
        assert_eq!(world.block_state(8, 15, 8), state("stone"));
    }

    #[test]
    fn unsupported_predicates_skip_the_feature_and_invalid_states_are_errors() {
        let unsupported = data(json!({"type": "test:unported"}));
        assert!(
            FeatureCompiler::new(&unsupported)
                .configured("minecraft:test")
                .unwrap()
                .is_none()
        );
        let mut invalid = data(json!({"type": "minecraft:true"}));
        invalid
            .configured_features
            .get_mut("minecraft:test")
            .unwrap()
            .config = crate::FeatureConfigDef::Other(json!({
            "state": {"Name": "test:missing"}, "can_place_on": {"type": "minecraft:true"}
        }));
        assert!(
            FeatureCompiler::new(&invalid)
                .configured("minecraft:test")
                .is_err()
        );
    }
}
