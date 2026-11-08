//! `minecraft:vegetation_patch` (`VegetationPatchFeature`): a patch of ground blocks on the floor or ceiling around
//! the origin, then vegetation placed on some of them.
//!
//! The patch is a rectangle of two radii drawn from `xz_radius` plus one, without its corners and with each edge
//! column kept by `extra_edge_column_chance`. In each column the feature moves through air toward the surface and
//! back out of the blocks there, each within `vertical_range`, and replaces up to `depth` blocks from the surface beyond it that are in `replaceable`,
//! when that surface has a sturdy face towards the air. Each patched column then grows `vegetation_feature` by
//! `vegetation_chance`, in Java's iteration order of the `HashSet` that holds them.

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

/// A compiled `minecraft:vegetation_patch`.
#[derive(Debug)]
pub struct CompiledVegetationPatch {
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
    let surface = ground_patch(world, patch, random, origin, radius_x, radius_z);
    for position in surface.iter() {
        if patch.vegetation_chance > 0.0 && random.next_float() < patch.vegetation_chance {
            place_placed(
                world,
                &patch.vegetation_feature,
                random,
                patch.surface.opposite().step(position),
            );
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
    /// Compiles a `minecraft:vegetation_patch` configuration; None for parts not ported yet.
    pub(crate) fn vegetation_patch(
        &mut self,
        name: &str,
        config: &Value,
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
