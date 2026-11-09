//! `minecraft:root_system` (`RootSystemFeature`): a tree grown on the first spot above the origin that has room
//! for it, with a column of rooted blocks under it and hanging roots around the origin.
//!
//! From an air block, usually a cave ceiling's underside, the feature climbs up to `root_column_max_height` blocks
//! looking for a position that `allowed_tree_position` accepts, with `required_vertical_space_for_tree` blocks of
//! air above it, or of water in the first `allowed_vertical_water_for_tree` of them. The block below that position
//! must be solid and not lava, or the feature stops there. If the tree grows, each block from the origin up to two
//! below the tree gets `root_placement_attempts` tries at a random horizontal offset to replace a block of
//! `root_replaceable` with the root state; then `hanging_root_placement_attempts` tries around the origin place
//! the hanging root state in air below a block with a sturdy bottom face.

use std::sync::Arc;

use harpas_core::block::block_states;
use harpas_core::block_behavior::{Face, block_behavior};
use harpas_core::random::{RandomSource, WorldgenRandom};
use serde_json::Value;

use crate::configured::{CompiledPlaced, FeatureCompiler, StateProvider, place_placed};
use crate::data::WorldgenDataError;
use crate::feature::FeatureWorld;
use crate::placement::{BlockPredicate, Fluid, StateSet, fluid};

/// A compiled `minecraft:root_system`.
#[derive(Debug)]
pub struct CompiledRootSystem {
    feature: Arc<CompiledPlaced>,
    required_vertical_space_for_tree: i32,
    root_radius: i32,
    root_replaceable: StateSet,
    root_state_provider: StateProvider,
    root_placement_attempts: i32,
    root_column_max_height: i32,
    hanging_root_radius: i32,
    hanging_roots_vertical_span: i32,
    hanging_root_state_provider: StateProvider,
    hanging_root_placement_attempts: i32,
    allowed_vertical_water_for_tree: i32,
    allowed_tree_position: BlockPredicate,
}

/// `RootSystemFeature.place`: false only when the origin is not air; a tree that finds no room still counts as
/// placed.
pub fn place_root_system<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    roots: &CompiledRootSystem,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    if !is_air(world, origin) {
        return false;
    }
    if place_dirt_and_tree(world, roots, random, origin) {
        place_hanging_roots(world, roots, random, origin);
    }
    true
}

fn is_air<W: FeatureWorld>(world: &W, [x, y, z]: [i32; 3]) -> bool {
    block_states().is_air(world.block_state(x, y, z))
}

/// `RootSystemFeature.placeDirtAndTree`: climbs from the origin to the first position that holds the tree.
fn place_dirt_and_tree<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    roots: &CompiledRootSystem,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    for i in 0..roots.root_column_max_height {
        let position = [origin[0], origin[1] + i + 1, origin[2]];
        if !roots.allowed_tree_position.test(world, position)
            || !space_for_tree(world, roots, position)
        {
            continue;
        }
        let below = world.block_state(position[0], position[1] - 1, position[2]);
        if matches!(fluid(below), Fluid::Lava | Fluid::FlowingLava)
            || !block_behavior().is_solid(below)
        {
            return false;
        }
        if place_placed(world, &roots.feature, random, position) {
            for y in origin[1]..origin[1] + i {
                place_rooted_dirt(world, roots, random, [origin[0], y, origin[2]]);
            }
            return true;
        }
    }
    false
}

/// `RootSystemFeature.spaceForTree`: air above the position, or water in its lowest blocks.
fn space_for_tree<W: FeatureWorld>(
    world: &W,
    roots: &CompiledRootSystem,
    [x, y, z]: [i32; 3],
) -> bool {
    (1..=roots.required_vertical_space_for_tree).all(|i| {
        let state = world.block_state(x, y + i, z);
        block_states().is_air(state)
            || (i < roots.allowed_vertical_water_for_tree
                && matches!(fluid(state), Fluid::Water | Fluid::FlowingWater))
    })
}

/// `RootSystemFeature.placeRootedDirt`: tries at random horizontal offsets in the layer of `position`.
fn place_rooted_dirt<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    roots: &CompiledRootSystem,
    random: &mut WorldgenRandom<R>,
    [x, y, z]: [i32; 3],
) {
    let radius = roots.root_radius;
    for _ in 0..roots.root_placement_attempts {
        let dx = random.next_int_bounded(radius) - random.next_int_bounded(radius);
        let dz = random.next_int_bounded(radius) - random.next_int_bounded(radius);
        let at = [x + dx, y, z + dz];
        if roots
            .root_replaceable
            .contains(world.block_state(at[0], at[1], at[2]))
        {
            let state = roots.root_state_provider.state(world, random, at);
            world.set_block_state(at[0], at[1], at[2], state);
        }
    }
}

/// `RootSystemFeature.placeRoots`: hanging roots in air around the origin, each under a sturdy bottom face, which
/// `HangingRootsBlock.canSurvive` also requires.
fn place_hanging_roots<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    roots: &CompiledRootSystem,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) {
    let radius = roots.hanging_root_radius;
    let span = roots.hanging_roots_vertical_span;
    for _ in 0..roots.hanging_root_placement_attempts {
        let dx = random.next_int_bounded(radius) - random.next_int_bounded(radius);
        let dy = random.next_int_bounded(span) - random.next_int_bounded(span);
        let dz = random.next_int_bounded(radius) - random.next_int_bounded(radius);
        let [x, y, z] = [origin[0] + dx, origin[1] + dy, origin[2] + dz];
        if !is_air(world, [x, y, z]) {
            continue;
        }
        let state = roots
            .hanging_root_state_provider
            .state(world, random, [x, y, z]);
        if block_behavior().is_face_sturdy(world.block_state(x, y + 1, z), Face::Down) {
            world.set_block_state(x, y, z, state);
        }
    }
}

impl FeatureCompiler<'_> {
    /// Compiles a `minecraft:root_system` configuration; None for parts not ported yet.
    pub(crate) fn root_system(
        &mut self,
        name: &str,
        config: &Value,
    ) -> Result<Option<CompiledRootSystem>, WorldgenDataError> {
        let invalid = |message: String| WorldgenDataError::InvalidDefinition {
            context: name.to_string(),
            message,
        };
        let field = |key: &str| {
            config
                .get(key)
                .ok_or_else(|| invalid(format!("missing field `{key}`")))
        };
        let integer = |key: &str| {
            field(key)?
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())
                .ok_or_else(|| invalid(format!("`{key}` must be an integer")))
        };
        let root_replaceable = field("root_replaceable")?
            .as_str()
            .and_then(|tag| tag.strip_prefix('#'))
            .ok_or_else(|| invalid("`root_replaceable` must be a block tag".to_string()))?;
        let (
            Some(feature),
            Some(root_state_provider),
            Some(hanging_root_state_provider),
            Some(allowed_tree_position),
        ) = (
            self.placed_ref(name, field("feature")?)?,
            self.provider(name, field("root_state_provider")?)?,
            self.provider(name, field("hanging_root_state_provider")?)?,
            self.predicate(name, field("allowed_tree_position")?)?,
        )
        else {
            return Ok(None);
        };
        Ok(Some(CompiledRootSystem {
            feature,
            required_vertical_space_for_tree: integer("required_vertical_space_for_tree")?,
            root_radius: integer("root_radius")?,
            root_replaceable: StateSet::from_membership(
                &self.data().block_tag_states(root_replaceable)?,
            ),
            root_state_provider,
            root_placement_attempts: integer("root_placement_attempts")?,
            root_column_max_height: integer("root_column_max_height")?,
            hanging_root_radius: integer("hanging_root_radius")?,
            hanging_roots_vertical_span: integer("hanging_roots_vertical_span")?,
            hanging_root_state_provider,
            hanging_root_placement_attempts: integer("hanging_root_placement_attempts")?,
            allowed_vertical_water_for_tree: integer("allowed_vertical_water_for_tree")?,
            allowed_tree_position,
        }))
    }
}
