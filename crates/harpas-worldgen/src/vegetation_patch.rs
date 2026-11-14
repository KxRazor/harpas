//! `minecraft:vegetation_patch` and `minecraft:waterlogged_vegetation_patch`: ground on the floor or ceiling around
//! the origin, then vegetation placed on some of them.
//!
//! The patch is a rectangle of two radii drawn from `xz_radius` plus one, without its corners and with each edge
//! column kept by `extra_edge_column_chance`. In each column the feature moves through air toward the surface and
//! back out of the blocks there, each within `vertical_range`, and replaces up to `depth` blocks from the surface beyond it that are in `replaceable`,
//! when that surface has a sturdy face towards the air. Each patched column then grows `vegetation_feature` by
//! `vegetation_chance`, in Java's iteration order of the `HashSet` that holds them.
//! Waterlogged patches first keep only positions with sturdy faces below and beside them, fill them with water,
//! and grow their vegetation in the water, waterlogging the block at each successful origin.

use std::sync::Arc;

use harpas_core::block::block_states;
use harpas_core::block_behavior::{Face, block_behavior};
use harpas_core::random::{RandomSource, WorldgenRandom};
use serde_json::Value;

use crate::configured::{CompiledPlaced, FeatureCompiler, StateProvider, place_placed};
use crate::data::{IntProviderDef, WorldgenDataError, parse_int_provider};
use crate::feature::FeatureWorld;
use crate::java_hash::BlockPosSet;
use crate::placement::{StateSet, sample_int_provider};
use crate::shape_update::{Direction, same_block};

/// A compiled `minecraft:vegetation_patch` or `minecraft:waterlogged_vegetation_patch`.
#[derive(Debug)]
pub struct CompiledVegetationPatch {
    /// Flood enclosed ground positions and grow vegetation in their water instead of above the ground.
    waterlogged: bool,
    replaceable: StateSet,
    ground_state: StateProvider,
    vegetation_feature: Arc<CompiledPlaced>,
    /// `surface`: down onto the floor, or up onto the ceiling.
    surface: Direction,
    depth: IntProviderDef,
    extra_bottom_block_chance: f32,
    vertical_range: i32,
    vegetation_chance: f32,
    xz_radius: IntProviderDef,
    extra_edge_column_chance: f32,
}

/// `VegetationPatchFeature.place`.
pub fn place_vegetation_patch<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    patch: &CompiledVegetationPatch,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    let radius_x = sample_int_provider(&patch.xz_radius, random).unwrap_or(0) + 1;
    let radius_z = sample_int_provider(&patch.xz_radius, random).unwrap_or(0) + 1;
    let mut surface = ground_patch(world, patch, random, origin, radius_x, radius_z);
    if patch.waterlogged {
        let mut enclosed = BlockPosSet::new();
        for position in surface.iter() {
            let exposed = [
                (Face::Down, Face::Up),
                (Face::North, Face::South),
                (Face::South, Face::North),
                (Face::West, Face::East),
                (Face::East, Face::West),
            ]
            .into_iter()
            .any(|(face, opposite)| {
                let [dx, dy, dz] = face.offset();
                let neighbor = [position[0] + dx, position[1] + dy, position[2] + dz];
                !block_behavior().is_face_sturdy(
                    world.block_state(neighbor[0], neighbor[1], neighbor[2]),
                    opposite,
                )
            });
            if !exposed {
                enclosed.insert(position);
            }
        }
        let water = block_states().default_state("minecraft:water").unwrap_or(0);
        for [x, y, z] in enclosed.iter() {
            world.set_block_state(x, y, z, water);
        }
        surface = enclosed;
    }
    for position in surface.iter() {
        if patch.vegetation_chance > 0.0 && random.next_float() < patch.vegetation_chance {
            let at = if patch.waterlogged {
                position
            } else {
                patch.surface.opposite().step(position)
            };
            let placed = place_placed(world, &patch.vegetation_feature, random, at);
            if patch.waterlogged && placed {
                let state = world.block_state(at[0], at[1], at[2]);
                let state = crate::shape_update::with_property(state, "waterlogged", "true");
                world.set_block_state(at[0], at[1], at[2], state);
            }
        }
    }
    !surface.is_empty()
}

/// `VegetationPatchFeature.placeGroundPatch`.
fn ground_patch<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    patch: &CompiledVegetationPatch,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
    radius_x: i32,
    radius_z: i32,
) -> BlockPosSet {
    let toward = patch.surface;
    let away = toward.opposite();
    let face = match away {
        Direction::Down => Face::Down,
        _ => Face::Up,
    };
    let mut surface = BlockPosSet::new();
    for dx in -radius_x..=radius_x {
        let edge_x = dx == -radius_x || dx == radius_x;
        for dz in -radius_z..=radius_z {
            let edge_z = dz == -radius_z || dz == radius_z;
            if edge_x && edge_z {
                continue;
            }
            if (edge_x || edge_z)
                && (patch.extra_edge_column_chance == 0.0
                    || random.next_float() > patch.extra_edge_column_chance)
            {
                continue;
            }
            let mut position = [origin[0] + dx, origin[1], origin[2] + dz];
            let is_air =
                |world: &W, [x, y, z]: [i32; 3]| block_states().is_air(world.block_state(x, y, z));
            // Through air toward the surface, then back out of the blocks there.
            let mut i = 0;
            while is_air(world, position) && i < patch.vertical_range {
                position = toward.step(position);
                i += 1;
            }
            let mut i = 0;
            while !is_air(world, position) && i < patch.vertical_range {
                position = away.step(position);
                i += 1;
            }
            let mut ground = toward.step(position);
            let ground_state = world.block_state(ground[0], ground[1], ground[2]);
            if !is_air(world, position) || !block_behavior().is_face_sturdy(ground_state, face) {
                continue;
            }
            let depth = sample_int_provider(&patch.depth, random).unwrap_or(0);
            let extra = patch.extra_bottom_block_chance > 0.0
                && random.next_float() < patch.extra_bottom_block_chance;
            let depth = depth + i32::from(extra);
            let start = ground;
            if place_ground(world, patch, random, &mut ground, depth) {
                surface.insert(start);
            }
        }
    }
    surface
}

/// `VegetationPatchFeature.placeGround`: up to `depth` ground blocks into the surface, stopping at a block that
/// is not replaceable; a block that is already the ground block is kept and the same position tried again.
fn place_ground<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    patch: &CompiledVegetationPatch,
    random: &mut WorldgenRandom<R>,
    position: &mut [i32; 3],
    depth: i32,
) -> bool {
    for i in 0..depth {
        let state = patch.ground_state.state(world, random, *position);
        let current = world.block_state(position[0], position[1], position[2]);
        if same_block(state, current) {
            continue;
        }
        if !patch.replaceable.contains(current) {
            return i != 0;
        }
        world.set_block_state(position[0], position[1], position[2], state);
        *position = patch.surface.step(*position);
    }
    true
}

impl FeatureCompiler<'_> {
    /// Compiles either vegetation patch configuration; None for parts not ported yet.
    pub(crate) fn vegetation_patch(
        &mut self,
        name: &str,
        config: &Value,
        waterlogged: bool,
    ) -> Result<Option<CompiledVegetationPatch>, WorldgenDataError> {
        let invalid = |message: String| WorldgenDataError::InvalidDefinition {
            context: name.to_string(),
            message,
        };
        let field = |key: &str| {
            config
                .get(key)
                .ok_or_else(|| invalid(format!("missing field `{key}`")))
        };
        let number = |key: &str| {
            field(key)?
                .as_f64()
                .ok_or_else(|| invalid(format!("`{key}` must be a number")))
        };
        let replaceable = field("replaceable")?
            .as_str()
            .and_then(|tag| tag.strip_prefix('#'))
            .ok_or_else(|| invalid("`replaceable` must be a block tag".to_string()))?;
        let surface = match field("surface")?.as_str() {
            Some("floor") => Direction::Down,
            Some("ceiling") => Direction::Up,
            _ => {
                return Err(invalid(
                    "`surface` must be `floor` or `ceiling`".to_string(),
                ));
            }
        };
        let depth = parse_int_provider(name, field("depth")?)?;
        let xz_radius = parse_int_provider(name, field("xz_radius")?)?;
        if matches!(depth, IntProviderDef::Other(_))
            || matches!(xz_radius, IntProviderDef::Other(_))
        {
            return Ok(None);
        }
        let (Some(ground_state), Some(vegetation_feature)) = (
            self.provider(name, field("ground_state")?)?,
            self.placed_ref(name, field("vegetation_feature")?)?,
        ) else {
            return Ok(None);
        };
        let vertical_range = field("vertical_range")?
            .as_i64()
            .and_then(|value| i32::try_from(value).ok())
            .ok_or_else(|| invalid("`vertical_range` must be an integer".to_string()))?;
        Ok(Some(CompiledVegetationPatch {
            waterlogged,
            replaceable: StateSet::from_membership(&self.data().block_tag_states(replaceable)?),
            ground_state,
            vegetation_feature,
            surface,
            depth,
            extra_bottom_block_chance: number("extra_bottom_block_chance")? as f32,
            vertical_range,
            vegetation_chance: number("vegetation_chance")? as f32,
            xz_radius,
            extra_edge_column_chance: number("extra_edge_column_chance")? as f32,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GeneratedChunk, RegionFeatureWorld, WorldgenData, place_feature};
    use harpas_core::random::XoroshiroRandom;
    use serde_json::json;

    fn state(name: &str) -> u32 {
        block_states()
            .default_state(&format!("minecraft:{name}"))
            .unwrap()
    }

    fn data(wet: bool, vegetation: f32) -> WorldgenData {
        WorldgenData::from_json_entries([
            ("data/test/tags/block/replaceable.json", json!({"values": ["minecraft:stone"]}).to_string()),
            ("data/minecraft/tags/block/supports_small_dripleaf.json", json!({"values": ["minecraft:clay"]}).to_string()),
            ("data/minecraft/tags/block/supports_vegetation.json", json!({"values": ["minecraft:dirt"]}).to_string()),
            ("data/test/worldgen/configured_feature/patch.json", json!({
                "type": if wet { "minecraft:waterlogged_vegetation_patch" } else { "minecraft:vegetation_patch" },
                "config": {
                    "replaceable": "#test:replaceable", "ground_state": {"type": "minecraft:simple_state_provider", "state": {"Name": "minecraft:clay"}},
                    "vegetation_feature": {"feature": {"type": "minecraft:simple_block", "config": {"to_place": {"type": "minecraft:simple_state_provider", "state": {"Name": "minecraft:small_dripleaf"}}}}, "placement": []},
                    "surface": "floor", "depth": 2, "extra_bottom_block_chance": 0.0, "vertical_range": 3,
                    "vegetation_chance": vegetation, "xz_radius": 0, "extra_edge_column_chance": 1.0
                }
            }).to_string()),
        ]).unwrap()
    }

    fn floor() -> RegionFeatureWorld {
        let mut chunk = GeneratedChunk::new(0, 0, -64, 384, 1);
        for y in 0..=4 {
            for z in 5..=11 {
                for x in 5..=11 {
                    chunk.set_block_state(x, y, z, state("stone"));
                }
            }
        }
        RegionFeatureWorld::new([chunk], 0).unwrap()
    }

    #[test]
    fn dry_and_flooded_patches_grow_both_halves_at_different_heights() {
        for wet in [false, true] {
            let data = data(wet, 1.0);
            let patch = FeatureCompiler::new(&data)
                .configured("test:patch")
                .unwrap()
                .unwrap();
            let mut world = floor();
            let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
            assert!(place_feature(&mut world, &patch, &mut random, [8, 5, 8]));
            let y = if wet { 4 } else { 5 };
            for (dy, half) in [(0, "lower"), (1, "upper")] {
                let (name, properties) = block_states()
                    .properties(world.block_state(8, y + dy, 8))
                    .unwrap();
                assert_eq!(name, "minecraft:small_dripleaf");
                assert!(properties.contains(&("half", half)));
                assert!(
                    properties
                        .contains(&("waterlogged", if wet && dy == 0 { "true" } else { "false" }))
                );
            }
            assert_eq!(world.block_state(8, y - 1, 8), state("clay"));
        }
    }

    #[test]
    fn pool_does_not_flood_positions_with_a_side_or_bottom_opening() {
        let data = data(true, 0.0);
        let patch = FeatureCompiler::new(&data)
            .configured("test:patch")
            .unwrap()
            .unwrap();
        let mut world = floor();
        world.set_block_state(8, 3, 8, state("air"));
        world.set_block_state(7, 4, 7, state("air"));
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
        assert!(place_feature(&mut world, &patch, &mut random, [8, 5, 8]));
        assert_eq!(
            world.block_state(8, 4, 8),
            state("clay"),
            "an opening below"
        );
        assert_eq!(
            world.block_state(7, 4, 8),
            state("clay"),
            "an opening beside"
        );
        assert_eq!(
            world.block_state(9, 4, 8),
            state("water"),
            "enclosed by solid faces"
        );
    }
}
