//! Dripstone clusters: wet cave floors, layered bases and paired, sometimes merged pointed columns.

use harpas_core::block::block_states;
use harpas_core::random::{RandomSource, WorldgenRandom};
use serde_json::Value;

use crate::aquifer::clamped_map;
use crate::configured::FeatureCompiler;
use crate::data::{IntProviderDef, WorldgenDataError, parse_int_provider};
use crate::feature::FeatureWorld;
use crate::feature_provider::{FloatProvider, compile_float_provider, int_bounds};
use crate::placement::{Fluid, StateSet, fluid, sample_int_provider};

/// A compiled `minecraft:dripstone_cluster` feature with its base and pool-support tags.
#[derive(Debug)]
pub struct CompiledDripstoneCluster {
    search: i32,
    height: IntProviderDef,
    radius: IntProviderDef,
    height_diff: i32,
    deviation: i32,
    layer: IntProviderDef,
    density: FloatProvider,
    wetness: FloatProvider,
    edge_chance: f32,
    edge_distance: i32,
    center_distance: i32,
    replaceable: StateSet,
    pool_support: StateSet,
    base: u32,
    water: u32,
    // Direction (down, up), thickness (base, middle, frustum, tip, tip_merge), waterlogging.
    states: [[[u32; 2]; 5]; 2],
}

fn is_block(state: u32, name: &str) -> bool {
    block_states()
        .properties(state)
        .is_some_and(|(block, _)| block == name)
}

fn empty(state: u32) -> bool {
    block_states().is_air(state) || is_block(state, "minecraft:water")
}

fn water(state: u32) -> bool {
    matches!(fluid(state), Fluid::Water | Fluid::FlowingWater)
}

impl CompiledDripstoneCluster {
    fn is_base(&self, state: u32) -> bool {
        state == self.base || self.replaceable.contains(state)
    }

    fn boundary(&self, world: &impl FeatureWorld, [x, y, z]: [i32; 3], step: i32) -> Option<i32> {
        let mut py = y;
        for _ in 1..self.search {
            if !empty(world.block_state(x, py, z)) {
                break;
            }
            py += step;
        }
        (!empty(world.block_state(x, py, z))).then_some(py)
    }

    fn pool(&self, world: &impl FeatureWorld, [x, y, z]: [i32; 3]) -> bool {
        let state = world.block_state(x, y, z);
        if state == self.base
            || is_block(state, "minecraft:pointed_dripstone")
            || is_block(state, "minecraft:water")
            || water(world.block_state(x, y + 1, z))
        {
            return false;
        }
        [[0, 0, -1], [1, 0, 0], [0, 0, 1], [-1, 0, 0], [0, -1, 0]]
            .into_iter()
            .all(|[dx, dy, dz]| {
                let state = world.block_state(x + dx, y + dy, z + dz);
                self.pool_support.contains(state) || water(state)
            })
    }

    fn replace_layer(
        &self,
        world: &mut impl FeatureWorld,
        [x, y, z]: [i32; 3],
        step: i32,
        height: i32,
    ) {
        for i in 0..height {
            let py = y + i * step;
            if !self.replaceable.contains(world.block_state(x, py, z)) {
                break;
            }
            world.set_block_state(x, py, z, self.base);
        }
    }

    fn pointed(
        &self,
        world: &mut impl FeatureWorld,
        [x, y, z]: [i32; 3],
        step: i32,
        height: i32,
        merge: bool,
    ) {
        if !self.is_base(world.block_state(x, y - step, z)) {
            return;
        }
        for i in 0..height {
            let thickness = if i == height - 1 {
                if merge { 4 } else { 3 }
            } else if i == height - 2 {
                2
            } else if i == 0 {
                0
            } else {
                1
            };
            let py = y + i * step;
            let wet = usize::from(water(world.block_state(x, py, z)));
            world.set_block_state(x, py, z, self.states[usize::from(step > 0)][thickness][wet]);
        }
    }

    fn pointed_height(
        &self,
        random: &mut impl RandomSource,
        [dx, dz]: [i32; 2],
        density: f32,
        height: i32,
    ) -> i32 {
        if random.next_float() > density {
            return 0;
        }
        let mean = clamped_map(
            f64::from(dx.abs() + dz.abs()),
            0.0,
            f64::from(self.center_distance),
            f64::from(height) / 2.0,
            0.0,
        ) as f32;
        (mean + random.next_gaussian() as f32 * self.deviation as f32).clamp(0.0, height as f32)
            as i32
    }

    #[allow(clippy::too_many_arguments)]
    fn column<R: RandomSource>(
        &self,
        world: &mut impl FeatureWorld,
        random: &mut WorldgenRandom<R>,
        at: [i32; 3],
        offset: [i32; 2],
        wetness: f32,
        chance: f64,
        height: i32,
        density: f32,
    ) {
        let [x, y, z] = at;
        if !empty(world.block_state(x, y, z)) {
            return;
        }
        let ceiling = self.boundary(world, at, 1);
        let mut floor = self.boundary(world, at, -1);
        if ceiling.is_none() && floor.is_none() {
            return;
        }
        // The wetness draw precedes both presence tests; a pool lowers the effective floor by one.
        if random.next_float() < wetness
            && let Some(f) = floor.filter(|&f| self.pool(world, [x, f, z]))
        {
            world.set_block_state(x, f, z, self.water);
            floor = Some(f - 1);
        }
        let down = random.next_double() < chance;
        let mut down_height = 0;
        if let Some(c) =
            ceiling.filter(|&c| down && !is_block(world.block_state(x, c, z), "minecraft:lava"))
        {
            let layer = sample_int_provider(&self.layer, random).unwrap();
            self.replace_layer(world, [x, c, z], 1, layer);
            let height = floor.map_or(height, |f| height.min(c - f));
            down_height = self.pointed_height(random, offset, density, height);
        }
        let up = random.next_double() < chance;
        let mut up_height = 0;
        if let Some(f) =
            floor.filter(|&f| up && !is_block(world.block_state(x, f, z), "minecraft:lava"))
        {
            let layer = sample_int_provider(&self.layer, random).unwrap();
            self.replace_layer(world, [x, f, z], -1, layer);
            up_height = if ceiling.is_some() {
                (down_height
                    + random.next_int_between_inclusive(-self.height_diff, self.height_diff))
                .max(0)
            } else {
                self.pointed_height(random, offset, density, height)
            };
        }
        if let (Some(c), Some(f)) = (ceiling, floor)
            && c - down_height <= f + up_height
        {
            let split = random.next_int_between_inclusive(
                (c - down_height).max(f + 1),
                (f + up_height).min(c - 1) + 1,
            );
            down_height = c - split;
            up_height = split - 1 - f;
        }
        // Vanilla always consumes this boolean, even for an absent or zero-height column.
        let merge = random.next_bool()
            && down_height > 0
            && up_height > 0
            && ceiling
                .zip(floor)
                .is_some_and(|(c, f)| down_height + up_height == c - f - 1);
        if let Some(c) = ceiling {
            self.pointed(world, [x, c - 1, z], -1, down_height, merge);
        }
        if let Some(f) = floor {
            self.pointed(world, [x, f + 1, z], 1, up_height, merge);
        }
    }
}

/// Places a cluster in X/Z order (`DripstoneClusterFeature.place`). Only the origin must be empty or water;
/// each column scans independently, and a single floor or ceiling is sufficient. Success need not write blocks.
pub fn place_dripstone_cluster<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    cluster: &CompiledDripstoneCluster,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    let [x, y, z] = origin;
    if !empty(world.block_state(x, y, z)) {
        return false;
    }
    let height = sample_int_provider(&cluster.height, random).unwrap();
    let wetness = cluster.wetness.sample(random);
    let density = cluster.density.sample(random);
    let rx = sample_int_provider(&cluster.radius, random).unwrap();
    let rz = sample_int_provider(&cluster.radius, random).unwrap();
    for dx in -rx..=rx {
        for dz in -rz..=rz {
            let distance = (rx - dx.abs()).min(rz - dz.abs()) as f32;
            // This map uses float precision before widening the chance to double.
            let t = (distance / cluster.edge_distance as f32).clamp(0.0, 1.0);
            let chance = cluster.edge_chance + t * (1.0 - cluster.edge_chance);
            cluster.column(
                world,
                random,
                [x + dx, y, z + dz],
                [dx, dz],
                wetness,
                f64::from(chance),
                height,
                density,
            );
        }
    }
    true
}

impl FeatureCompiler<'_> {
    pub(crate) fn dripstone_cluster(
        &self,
        name: &str,
        config: &Value,
    ) -> Result<Option<CompiledDripstoneCluster>, WorldgenDataError> {
        let invalid = |message: String| WorldgenDataError::InvalidDefinition {
            context: name.into(),
            message,
        };
        let field = |key: &str| {
            config
                .get(key)
                .ok_or_else(|| invalid(format!("missing `{key}`")))
        };
        let integer = |key: &str, min, max| {
            field(key)?
                .as_i64()
                .and_then(|v| i32::try_from(v).ok())
                .filter(|v| (min..=max).contains(v))
                .ok_or_else(|| invalid(format!("`{key}` must be between {min} and {max}")))
        };
        let provider = |key: &str, min, max| -> Result<_, WorldgenDataError> {
            let provider = parse_int_provider(name, field(key)?)?;
            let Some((low, high)) = int_bounds(&provider) else {
                return Ok(None);
            };
            if low < min || high > max || low > high {
                return Err(invalid(format!(
                    "`{key}` provider must be within {min} and {max}"
                )));
            }
            Ok(Some(provider))
        };
        let (Some(height), Some(radius), Some(layer), Some(density), Some(wetness)) = (
            provider("height", 1, 128)?,
            provider("radius", 1, 128)?,
            provider("dripstone_block_layer_thickness", 0, 128)?,
            compile_float_provider(name, field("density")?, 0.0, 2.0)?,
            compile_float_provider(name, field("wetness")?, 0.0, 2.0)?,
        ) else {
            return Ok(None);
        };
        let mut states = [[[0; 2]; 5]; 2];
        for (d, direction) in ["down", "up"].into_iter().enumerate() {
            for (t, thickness) in ["base", "middle", "frustum", "tip", "tip_merge"]
                .into_iter()
                .enumerate()
            {
                for (w, wet) in ["false", "true"].into_iter().enumerate() {
                    states[d][t][w] = block_states()
                        .state(
                            "minecraft:pointed_dripstone",
                            [
                                ("vertical_direction", direction),
                                ("thickness", thickness),
                                ("waterlogged", wet),
                            ],
                        )
                        .unwrap()
                        .0;
                }
            }
        }
        Ok(Some(CompiledDripstoneCluster {
            search: integer("floor_to_ceiling_search_range", 1, 512)?,
            height,
            radius,
            height_diff: integer("max_stalagmite_stalactite_height_diff", 0, 64)?,
            deviation: integer("height_deviation", 1, 64)?,
            layer,
            density,
            wetness,
            edge_chance: field("chance_of_dripstone_column_at_max_distance_from_center")?
                .as_f64()
                .map(|v| v as f32)
                .filter(|v| (0.0..=1.0).contains(v))
                .ok_or_else(|| invalid("column chance must be between 0 and 1".into()))?,
            edge_distance: integer(
                "max_distance_from_edge_affecting_chance_of_dripstone_column",
                1,
                64,
            )?,
            center_distance: integer("max_distance_from_center_affecting_height_bias", 1, 64)?,
            replaceable: StateSet::from_membership(
                &self
                    .data()
                    .block_tag_states("minecraft:dripstone_replaceable_blocks")?,
            ),
            pool_support: StateSet::from_membership(
                &self
                    .data()
                    .block_tag_states("minecraft:base_stone_overworld")?,
            ),
            base: block_states()
                .default_state("minecraft:dripstone_block")
                .unwrap(),
            water: block_states().default_state("minecraft:water").unwrap(),
            states,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GeneratedChunk, RegionFeatureWorld, WorldgenData, place_feature};
    use harpas_core::random::XoroshiroRandom;
    use serde_json::json;

    fn config() -> Value {
        json!({"floor_to_ceiling_search_range":30, "height":24, "radius":3,
            "max_stalagmite_stalactite_height_diff":3, "height_deviation":3,
            "dripstone_block_layer_thickness":2, "density":1, "wetness":1,
            "chance_of_dripstone_column_at_max_distance_from_center":1,
            "max_distance_from_edge_affecting_chance_of_dripstone_column":3,
            "max_distance_from_center_affecting_height_bias":8})
    }

    fn data(config: Value) -> WorldgenData {
        WorldgenData::from_json_entries([
            (
                "data/test/worldgen/configured_feature/cluster.json",
                json!({"type":"minecraft:dripstone_cluster", "config":config}).to_string(),
            ),
            (
                "data/minecraft/tags/block/dripstone_replaceable_blocks.json",
                json!({"values":["minecraft:stone"]}).to_string(),
            ),
            (
                "data/minecraft/tags/block/base_stone_overworld.json",
                json!({"values":["minecraft:stone"]}).to_string(),
            ),
        ])
        .unwrap()
    }

    fn state(name: &str) -> u32 {
        block_states()
            .default_state(&format!("minecraft:{name}"))
            .unwrap()
    }

    fn cave() -> RegionFeatureWorld {
        let mut chunk = GeneratedChunk::new(0, 0, 0, 64, 1);
        for y in 0..64 {
            if (17..48).contains(&y) {
                continue;
            }
            for z in 0..16 {
                for x in 0..16 {
                    chunk.set_block_state(x, y, z, state("stone"));
                }
            }
        }
        chunk.freeze_worldgen_heightmaps();
        RegionFeatureWorld::new([chunk], 0).unwrap()
    }

    #[test]
    fn blocked_and_unwritable_origins_consume_no_random_values() {
        let data = data(config());
        let feature = FeatureCompiler::new(&data)
            .configured("test:cluster")
            .unwrap()
            .unwrap();
        let mut world = cave();
        for at in [[8, 16, 8], [32, 32, 8]] {
            let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
            assert!(!place_feature(&mut world, &feature, &mut random, at));
            assert_eq!(
                random.next_int(),
                WorldgenRandom::new(XoroshiroRandom::new(42)).next_int()
            );
        }
    }

    #[test]
    fn missing_boundaries_succeed_without_writes_or_column_random_draws() {
        let data = data(config());
        let feature = FeatureCompiler::new(&data)
            .configured("test:cluster")
            .unwrap()
            .unwrap();
        let mut world = RegionFeatureWorld::new([GeneratedChunk::new(0, 0, 0, 64, 1)], 0).unwrap();
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
        assert!(place_feature(&mut world, &feature, &mut random, [8, 32, 8]));
        assert_eq!(
            random.next_int(),
            WorldgenRandom::new(XoroshiroRandom::new(42)).next_int()
        );
        assert!((0..64).all(|y| world.block_state(8, y, 8) == 0));
    }

    #[test]
    fn wet_pools_lower_the_floor_and_waterlog_the_new_column() {
        let data = data(config());
        let feature = FeatureCompiler::new(&data)
            .configured("test:cluster")
            .unwrap()
            .unwrap();
        let mut world = cave();
        assert!(place_feature(
            &mut world,
            &feature,
            &mut WorldgenRandom::new(XoroshiroRandom::new(42)),
            [8, 32, 8]
        ));
        let mut wet_columns = 0;
        let mut pools = 0;
        for x in 5..=11 {
            for z in 5..=11 {
                let floor = world.block_state(x, 16, z);
                if floor == state("water") {
                    pools += 1;
                }
                if is_block(floor, "minecraft:pointed_dripstone") {
                    assert!(water(floor));
                    assert_eq!(world.block_state(x, 15, z), state("dripstone_block"));
                    wet_columns += 1;
                }
            }
        }
        assert!(wet_columns > 0);
        assert!(pools + wet_columns > 0);
    }

    #[test]
    fn pool_support_uses_water_fluids_and_the_base_stone_tag() {
        let data = data(config());
        let cluster = FeatureCompiler::new(&data)
            .dripstone_cluster("test", &config())
            .unwrap()
            .unwrap();
        for material in ["stone", "water", "dirt", "dripstone_block"] {
            let mut world = cave();
            world.set_block_state(9, 16, 8, state(material));
            assert_eq!(
                cluster.pool(&world, [8, 16, 8]),
                matches!(material, "stone" | "water")
            );
        }
        let mut world = cave();
        world.set_block_state(
            9,
            16,
            8,
            block_states()
                .state("minecraft:oak_slab", [("waterlogged", "true")])
                .unwrap()
                .0,
        );
        assert!(cluster.pool(&world, [8, 16, 8]));
        world.set_block_state(8, 17, 8, state("water"));
        assert!(!cluster.pool(&world, [8, 16, 8]));
    }

    #[test]
    fn malformed_configs_and_provider_ranges_are_rejected() {
        for (key, value) in [
            ("floor_to_ceiling_search_range", json!(0)),
            ("floor_to_ceiling_search_range", json!(513)),
            ("height", json!(0)),
            ("height", json!(129)),
            ("radius", json!(0)),
            ("radius", json!(129)),
            ("max_stalagmite_stalactite_height_diff", json!(-1)),
            ("max_stalagmite_stalactite_height_diff", json!(65)),
            ("height_deviation", json!(0)),
            ("height_deviation", json!(65)),
            ("dripstone_block_layer_thickness", json!(-1)),
            ("dripstone_block_layer_thickness", json!(129)),
            ("density", json!(-0.1)),
            ("density", json!(2.1)),
            ("wetness", json!(-0.1)),
            ("wetness", json!(2.1)),
            (
                "chance_of_dripstone_column_at_max_distance_from_center",
                json!(-0.1),
            ),
            (
                "chance_of_dripstone_column_at_max_distance_from_center",
                json!(1.1),
            ),
            (
                "max_distance_from_edge_affecting_chance_of_dripstone_column",
                json!(0),
            ),
            ("max_distance_from_center_affecting_height_bias", json!(65)),
            (
                "radius",
                json!({"type":"minecraft:uniform", "min_inclusive":1, "max_inclusive":129}),
            ),
            (
                "density",
                json!({"type":"minecraft:uniform", "min_inclusive":0, "max_exclusive":2.1}),
            ),
        ] {
            let mut config = config();
            config[key] = value;
            assert!(
                FeatureCompiler::new(&data(config))
                    .configured("test:cluster")
                    .is_err(),
                "{key}"
            );
        }
        for key in config().as_object().unwrap().keys() {
            let mut config = config();
            config.as_object_mut().unwrap().remove(key);
            assert!(
                FeatureCompiler::new(&data(config))
                    .configured("test:cluster")
                    .is_err(),
                "missing {key}"
            );
        }
        for key in [
            "radius",
            "height",
            "dripstone_block_layer_thickness",
            "density",
            "wetness",
        ] {
            let mut config = config();
            config[key] = json!({"type":"test:unsupported"});
            assert!(
                FeatureCompiler::new(&data(config))
                    .configured("test:cluster")
                    .unwrap()
                    .is_none()
            );
        }
    }
}
