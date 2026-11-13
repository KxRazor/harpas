//! `minecraft:bamboo`: five to sixteen stalk blocks, cut short by anything but air. After at least three
//! stalk blocks, the leafy tip occupies the first position above them, even replacing an obstruction there.
//! A probability also selects a circular podzol patch of radius one to four on the surrounding surface,
//! using the data pack's replacement tag.

use harpas_core::block::block_states;
use harpas_core::random::{RandomSource, WorldgenRandom};
use serde_json::Value;

use crate::configured::FeatureCompiler;
use crate::data::WorldgenDataError;
use crate::feature::FeatureWorld;
use crate::generator::HeightmapType;
use crate::placement::StateSet;
use crate::survival::{SurvivalRule, compile_block_survival};

/// A compiled bamboo feature, including its ground tags and the four stalk states it places.
#[derive(Debug)]
pub struct CompiledBamboo {
    probability: f32,
    survival: SurvivalRule,
    podzol_replaceable: StateSet,
    podzol: u32,
    stalk: u32,
    small: u32,
    large: u32,
    tip: u32,
}

/// Places a bamboo stalk and its optional podzol patch (`BambooFeature.place`). An empty origin counts as
/// success even when the ground cannot support bamboo; that case consumes no random values.
pub fn place_bamboo<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    bamboo: &CompiledBamboo,
    random: &mut WorldgenRandom<R>,
    [x, y, z]: [i32; 3],
) -> bool {
    if !block_states().is_air(world.block_state(x, y, z)) {
        return false;
    }
    if !bamboo.survival.test(world, [x, y, z], bamboo.stalk) {
        return true;
    }
    let height = random.next_int_bounded(12) + 5;
    if random.next_float() < bamboo.probability {
        let radius = random.next_int_bounded(4) + 1;
        for px in x - radius..=x + radius {
            for pz in z - radius..=z + radius {
                let (dx, dz) = (px - x, pz - z);
                if dx * dx + dz * dz > radius * radius {
                    continue;
                }
                let py = world.heightmap(HeightmapType::WorldSurface, px, pz) - 1;
                if bamboo
                    .podzol_replaceable
                    .contains(world.block_state(px, py, pz))
                {
                    world.set_block_state(px, py, pz, bamboo.podzol);
                }
            }
        }
    }
    let mut top = y;
    while top < y + height && block_states().is_air(world.block_state(x, top, z)) {
        world.set_block_state(x, top, z, bamboo.stalk);
        top += 1;
    }
    if top - y >= 3 {
        for (offset, state) in [(0, bamboo.tip), (1, bamboo.large), (2, bamboo.small)] {
            world.set_block_state(x, top - offset, z, state);
        }
    }
    true
}

impl FeatureCompiler<'_> {
    pub(crate) fn bamboo(
        &self,
        name: &str,
        config: &Value,
    ) -> Result<CompiledBamboo, WorldgenDataError> {
        let invalid = |message: &str| WorldgenDataError::InvalidDefinition {
            context: name.to_string(),
            message: message.to_string(),
        };
        let probability = config
            .get("probability")
            .and_then(Value::as_f64)
            .filter(|value| (0.0..=1.0).contains(value))
            .ok_or_else(|| invalid("`probability` must be between 0 and 1"))?
            as f32;
        let state = |leaves: &str, stage: &str| {
            block_states()
                .state(
                    "minecraft:bamboo",
                    [("age", "1"), ("leaves", leaves), ("stage", stage)],
                )
                .map(|state| state.0)
                .ok_or_else(|| invalid("unknown bamboo state"))
        };
        Ok(CompiledBamboo {
            probability,
            survival: compile_block_survival(name, "minecraft:bamboo", self.data())?
                .ok_or_else(|| invalid("unsupported bamboo survival rule"))?,
            podzol_replaceable: StateSet::from_membership(
                &self
                    .data()
                    .block_tag_states("minecraft:beneath_bamboo_podzol_replaceable")?,
            ),
            podzol: block_states()
                .default_state("minecraft:podzol")
                .ok_or_else(|| invalid("unknown podzol block"))?,
            stalk: state("none", "0")?,
            small: state("small", "0")?,
            large: state("large", "0")?,
            tip: state("large", "1")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GeneratedChunk, RegionFeatureWorld, WorldgenData, place_feature};
    use harpas_core::random::XoroshiroRandom;

    fn data(probability: Value) -> WorldgenData {
        WorldgenData::from_json_entries([
            (
                "data/minecraft/worldgen/configured_feature/test.json",
                serde_json::json!({
                    "type": "minecraft:bamboo", "config": {"probability": probability}
                })
                .to_string(),
            ),
            (
                "data/minecraft/tags/block/supports_bamboo.json",
                serde_json::json!({
                    "values": ["minecraft:dirt", "minecraft:bamboo", "minecraft:bamboo_sapling"]
                })
                .to_string(),
            ),
            (
                "data/minecraft/tags/block/beneath_bamboo_podzol_replaceable.json",
                serde_json::json!({
                    "values": ["minecraft:dirt"]
                })
                .to_string(),
            ),
        ])
        .unwrap()
    }

    fn state(name: &str) -> u32 {
        block_states()
            .default_state(&format!("minecraft:{name}"))
            .unwrap()
    }

    fn ground() -> RegionFeatureWorld {
        let mut chunk = GeneratedChunk::new(0, 0, 0, 32, 1);
        for x in 3..14 {
            for z in 3..14 {
                chunk.set_block_state(x, 0, z, state("dirt"));
            }
        }
        RegionFeatureWorld::new([chunk], 0).unwrap()
    }

    #[test]
    fn blocked_and_unsupported_origins_draw_no_random_values() {
        let data = data(Value::from(1.0));
        let feature = FeatureCompiler::new(&data)
            .configured("minecraft:test")
            .unwrap()
            .unwrap();
        for (at, below, result) in [
            ("air", "stone", true),
            ("water", "dirt", false),
            ("short_grass", "dirt", false),
        ] {
            let mut world = ground();
            world.set_block_state(8, 0, 8, state(below));
            world.set_block_state(8, 1, 8, state(at));
            let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
            assert_eq!(
                place_feature(&mut world, &feature, &mut random, [8, 1, 8]),
                result
            );
            assert_eq!(
                random.next_int(),
                WorldgenRandom::new(XoroshiroRandom::new(42)).next_int()
            );
            assert_eq!(world.block_state(8, 1, 8), state(at));
        }
    }

    #[test]
    fn a_stalk_needs_three_blocks_for_its_leafy_tip() {
        let data = data(Value::from(0.0));
        let feature = FeatureCompiler::new(&data)
            .configured("minecraft:test")
            .unwrap()
            .unwrap();
        let bamboo = |leaves, stage| {
            block_states()
                .state(
                    "minecraft:bamboo",
                    [("age", "1"), ("leaves", leaves), ("stage", stage)],
                )
                .unwrap()
                .0
        };
        for height in [2, 3] {
            let mut world = ground();
            world.set_block_state(8, height + 1, 8, state("stone"));
            let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
            assert!(place_feature(&mut world, &feature, &mut random, [8, 1, 8]));
            let expected = if height == 2 {
                vec![bamboo("none", "0"); 2]
            } else {
                vec![
                    bamboo("none", "0"),
                    bamboo("small", "0"),
                    bamboo("large", "0"),
                    bamboo("large", "1"),
                ]
            };
            for (i, state) in expected.into_iter().enumerate() {
                assert_eq!(world.block_state(8, i as i32 + 1, 8), state);
            }
            if height == 2 {
                assert_eq!(world.block_state(8, height + 1, 8), state("stone"));
            } else {
                assert_eq!(world.block_state(8, height + 1, 8), bamboo("large", "1"));
            }
        }
    }

    #[test]
    fn podzol_replaces_only_tagged_surface_blocks() {
        let data = data(Value::from(1.0));
        let feature = FeatureCompiler::new(&data)
            .configured("minecraft:test")
            .unwrap()
            .unwrap();
        let mut world = ground();
        world.set_block_state(7, 0, 8, state("sand"));
        world.set_block_state(8, 1, 9, state("short_grass"));
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
        assert!(place_feature(&mut world, &feature, &mut random, [8, 1, 8]));
        assert_eq!(world.block_state(8, 0, 8), state("podzol"));
        assert_eq!(world.block_state(9, 0, 8), state("podzol"));
        assert_eq!(world.block_state(7, 0, 8), state("sand"));
        assert_eq!(world.block_state(8, 0, 9), state("dirt"));
    }

    #[test]
    fn invalid_probabilities_do_not_compile() {
        for probability in [
            Value::from(-0.1),
            Value::from(1.1),
            Value::Null,
            Value::from("0.2"),
        ] {
            let data = data(probability);
            assert!(
                FeatureCompiler::new(&data)
                    .configured("minecraft:test")
                    .is_err()
            );
        }
    }
}
