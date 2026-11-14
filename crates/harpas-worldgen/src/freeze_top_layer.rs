//! Surface ice and snow during FEATURES, before a chunk initializes its light. Block light is zero at
//! this stage, including beside emitting blocks. Temperature uses the Overworld's sea level (63).

use harpas_core::block::block_states;

use crate::configured::FeatureCompiler;
use crate::data::{BiomeDef, WorldgenDataError};
use crate::feature::FeatureWorld;
use crate::generator::HeightmapType;
use crate::shape_update::{property, with_property};
use crate::surface::BiomeTemperatureSampler;
use crate::survival::{SurvivalRule, compile_block_survival};

/// Compiled `minecraft:freeze_top_layer`, with climates in the world's numeric biome order.
#[derive(Debug)]
pub struct CompiledFreezeTopLayer {
    biomes: Vec<BiomeDef>,
    temperature: BiomeTemperatureSampler,
    snow_survival: SurvivalRule,
    ice: u32,
    snow: u32,
}

/// Freezes surface water and places snow in the 16×16 columns east and south of the origin. The biome
/// at the first available motion-blocking Y decides both the ice below and the snow above. No random
/// values are consumed, and the feature succeeds even when every column stays unchanged.
pub fn place_freeze_top_layer(
    world: &mut impl FeatureWorld,
    surface: &CompiledFreezeTopLayer,
    [x, _, z]: [i32; 3],
) -> bool {
    let states = block_states();
    let (min_y, max_y) = (world.min_y(), world.min_y() + world.height());
    for dx in 0..16 {
        for dz in 0..16 {
            let (x, z) = (x + dx, z + dz);
            let y = world.heightmap(HeightmapType::MotionBlocking, x, z);
            let Some(biome) = surface.biomes.get(world.biome(x, y, z) as usize) else {
                continue;
            };
            let cold = |y| surface.temperature.cold_enough_to_snow(biome, x, y, z, 63);
            let below = world.block_state(x, y - 1, z);
            if (min_y..max_y).contains(&(y - 1))
                && cold(y - 1)
                && states
                    .properties(below)
                    .is_some_and(|(name, _)| name == "minecraft:water")
                && property(below, "level") == Some("0")
            {
                world.set_block_state(x, y - 1, z, surface.ice);
            }
            let above = world.block_state(x, y, z);
            if (min_y..max_y).contains(&y)
                && cold(y)
                && (states.is_air(above)
                    || states
                        .properties(above)
                        .is_some_and(|(name, _)| name == "minecraft:snow"))
                && surface.snow_survival.test(world, [x, y, z], surface.snow)
            {
                world.set_block_state(x, y, z, surface.snow);
                let below = world.block_state(x, y - 1, z);
                if property(below, "snowy").is_some() {
                    world.set_block_state(x, y - 1, z, with_property(below, "snowy", "true"));
                }
            }
        }
    }
    true
}

impl FeatureCompiler<'_> {
    pub(crate) fn freeze_top_layer(
        &self,
        name: &str,
    ) -> Result<CompiledFreezeTopLayer, WorldgenDataError> {
        Ok(CompiledFreezeTopLayer {
            biomes: self.data().biomes.values().cloned().collect(),
            temperature: BiomeTemperatureSampler::new(),
            snow_survival: compile_block_survival(name, "minecraft:snow", self.data())?.unwrap(),
            ice: block_states().default_state("minecraft:ice").unwrap(),
            snow: block_states().default_state("minecraft:snow").unwrap(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChunkFeatureWorld, GeneratedChunk, WorldgenData, place_feature};
    use harpas_core::random::{RandomSource, WorldgenRandom, XoroshiroRandom};
    use serde_json::json;

    fn data(temperature: f32) -> WorldgenData {
        WorldgenData::from_json_entries([
            (
                "data/test/worldgen/biome/test.json",
                json!({"has_precipitation": false,
                "temperature": temperature, "downfall": 0.0, "features": []})
                .to_string(),
            ),
            (
                "data/minecraft/tags/block/cannot_support_snow_layer.json",
                json!({"values": ["minecraft:ice", "minecraft:packed_ice"]}).to_string(),
            ),
            (
                "data/minecraft/tags/block/support_override_snow_layer.json",
                json!({"values": ["minecraft:soul_sand"]}).to_string(),
            ),
            (
                "data/test/worldgen/configured_feature/freeze.json",
                json!({"type": "minecraft:freeze_top_layer", "config": {}}).to_string(),
            ),
        ])
        .unwrap()
    }

    fn state(name: &str) -> u32 {
        block_states()
            .default_state(&format!("minecraft:{name}"))
            .unwrap()
    }

    fn decorate(chunk: &mut GeneratedChunk, temperature: f32) {
        let data = data(temperature);
        let feature = FeatureCompiler::new(&data)
            .configured("test:freeze")
            .unwrap()
            .unwrap();
        let mut world = ChunkFeatureWorld::new(chunk, 0);
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
        assert!(place_feature(
            &mut world,
            &feature,
            &mut random,
            [0, -1000, 0]
        ));
        assert_eq!(
            random.next_int(),
            WorldgenRandom::new(XoroshiroRandom::new(42)).next_int()
        );
    }

    #[test]
    fn cold_columns_get_snow_and_snowy_ground_even_without_precipitation() {
        let mut chunk = GeneratedChunk::new(0, 0, -64, 384, 1);
        chunk.set_block_state(0, 63, 0, state("grass_block"));
        chunk.set_block_state(1, 63, 0, state("soul_sand"));
        chunk.set_block_state(2, 63, 0, state("packed_ice"));
        decorate(&mut chunk, 0.0);
        assert_eq!(chunk.block_state(0, 64, 0), state("snow"));
        assert_eq!(property(chunk.block_state(0, 63, 0), "snowy"), Some("true"));
        assert_eq!(chunk.block_state(1, 64, 0), state("snow"));
        assert_eq!(chunk.block_state(2, 64, 0), state("air"));
    }

    #[test]
    fn only_source_water_freezes_and_ice_does_not_support_snow() {
        let mut chunk = GeneratedChunk::new(0, 0, -64, 384, 1);
        chunk.set_block_state(0, 63, 0, state("water"));
        let flowing = block_states()
            .state("minecraft:water", [("level", "1")])
            .unwrap()
            .0;
        chunk.set_block_state(1, 63, 0, flowing);
        chunk.set_block_state(2, 63, 0, state("bubble_column"));
        decorate(&mut chunk, 0.0);
        assert_eq!(chunk.block_state(0, 63, 0), state("ice"));
        assert_eq!(chunk.block_state(0, 64, 0), state("air"));
        assert_eq!(chunk.block_state(1, 63, 0), flowing);
        assert_eq!(chunk.block_state(2, 63, 0), state("bubble_column"));
    }

    #[test]
    fn warm_columns_remain_unchanged() {
        let mut chunk = GeneratedChunk::new(0, 0, -64, 384, 1);
        chunk.set_block_state(0, 63, 0, state("grass_block"));
        chunk.set_block_state(1, 63, 0, state("water"));
        decorate(&mut chunk, 0.15);
        assert_eq!(chunk.block_state(0, 64, 0), state("air"));
        assert_eq!(
            property(chunk.block_state(0, 63, 0), "snowy"),
            Some("false")
        );
        assert_eq!(chunk.block_state(1, 63, 0), state("water"));
    }

    #[test]
    fn world_limits_allow_bottom_snow_and_top_ice() {
        let mut chunk = GeneratedChunk::new(0, 0, -64, 384, 1);
        chunk.set_block_state(0, -64, 0, state("stone"));
        chunk.set_block_state(1, 319, 0, state("water"));
        chunk.set_block_state(2, 319, 0, state("grass_block"));
        decorate(&mut chunk, 0.0);
        assert_eq!(chunk.block_state(0, -63, 0), state("snow"));
        assert_eq!(chunk.block_state(1, 319, 0), state("ice"));
        assert_eq!(
            property(chunk.block_state(2, 319, 0), "snowy"),
            Some("false")
        );
        assert_eq!(chunk.block_state(3, -64, 0), state("air"));
    }
}
