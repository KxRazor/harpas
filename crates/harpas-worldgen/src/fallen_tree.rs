//! `minecraft:fallen_tree` (`FallenTreeFeature`): a one-block stump and, a little away from it, a log lying on
//! the ground.
//!
//! The stump replaces whatever is at the origin and runs the stump decorators. The log then points in a random
//! horizontal direction, two or three blocks from the stump, two blocks shorter than its drawn length. Its first
//! block drops to the highest position, up to five blocks below the stump's top, that is free for a tree and
//! stands on a sturdy top face. The log is placed only if each of its blocks is free for a tree and no more than
//! two in a row overhang; its blocks lie along the direction's axis and run the log decorators, in Java's
//! iteration order of their `HashSet`. Every log block marks up to two non-air blocks above it for
//! postprocessing. A fallen tree updates no shapes and keeps no decoration set.

use std::sync::Arc;

use harpas_core::block_behavior::{Face, block_behavior};
use harpas_core::random::{RandomSource, WorldgenRandom};
use serde_json::Value;

use crate::configured::{FeatureCompiler, StateProvider, mark_above_for_postprocessing};
use crate::data::{IntProviderDef, WorldgenDataError, parse_int_provider};
use crate::feature::FeatureWorld;
use crate::java_hash::BlockPosSet;
use crate::placement::sample_int_provider;
use crate::shape_update::{Direction, with_property};
use crate::tree::TreeBlocks;
use crate::tree_decorator::{Decorating, Decorator, DecoratorContext, HORIZONTAL};

/// A compiled `minecraft:fallen_tree`.
#[derive(Debug)]
pub struct CompiledFallenTree {
    trunk_provider: StateProvider,
    log_length: IntProviderDef,
    stump_decorators: Vec<Decorator>,
    log_decorators: Vec<Decorator>,
    blocks: Arc<TreeBlocks>,
}

/// `FallenTreeFeature.placeFallenTree`; a fallen tree always counts as placed.
pub fn place_fallen_tree<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    tree: &CompiledFallenTree,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    place_log_block(world, tree, random, origin, None);
    let mut stump = BlockPosSet::new();
    stump.insert(origin);
    decorate(world, tree, random, &stump, &tree.stump_decorators);

    let direction = HORIZONTAL[random.next_int_bounded(4) as usize];
    let length = sample_int_provider(&tree.log_length, random).unwrap_or(0) - 2;
    let distance = 2 + random.next_int_bounded(2);
    let [dx, _, dz] = direction.offset();
    let mut start = [
        origin[0] + dx * distance,
        origin[1] + 1,
        origin[2] + dz * distance,
    ];
    // `setGroundHeightForFallenLogStartPos`.
    for _ in 0..6 {
        if may_place_on(world, tree, start) {
            break;
        }
        start[1] -= 1;
    }
    if !can_place_entire_log(world, tree, length, start, direction) {
        return true;
    }
    let axis = if dx != 0 { "x" } else { "z" };
    let mut logs = BlockPosSet::new();
    let mut position = start;
    for _ in 0..length {
        place_log_block(world, tree, random, position, Some(axis));
        logs.insert(position);
        position = direction.step(position);
    }
    decorate(world, tree, random, &logs, &tree.log_decorators);
    true
}

/// `FallenTreeFeature.placeLogBlock`: the trunk state, turned to `axis` when given, then the blocks above it
/// marked for postprocessing.
fn place_log_block<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    tree: &CompiledFallenTree,
    random: &mut WorldgenRandom<R>,
    position: [i32; 3],
    axis: Option<&str>,
) {
    let mut state = tree.trunk_provider.state(world, random, position);
    if let Some(axis) = axis {
        state = with_property(state, "axis", axis);
    }
    world.set_block_state(position[0], position[1], position[2], state);
    mark_above_for_postprocessing(world, position);
}

/// `FallenTreeFeature.decorateLogs`: the decorators on the given logs, without leaves or roots.
fn decorate<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    tree: &CompiledFallenTree,
    random: &mut WorldgenRandom<R>,
    logs: &BlockPosSet,
    decorators: &[Decorator],
) {
    if decorators.is_empty() {
        return;
    }
    let context = DecoratorContext::new(logs, &BlockPosSet::new(), &BlockPosSet::new());
    Decorating {
        world,
        random,
        blocks: &tree.blocks,
        decorations: None,
    }
    .run(decorators, &context);
}

/// `FallenTreeFeature.mayPlaceOn`: free for a tree, over solid ground.
fn may_place_on(world: &impl FeatureWorld, tree: &CompiledFallenTree, position: [i32; 3]) -> bool {
    valid_tree_pos(world, tree, position) && over_solid_ground(world, position)
}

/// `TreeFeature.validTreePos`.
fn valid_tree_pos(
    world: &impl FeatureWorld,
    tree: &CompiledFallenTree,
    [x, y, z]: [i32; 3],
) -> bool {
    tree.blocks.valid(world.block_state(x, y, z))
}

/// `FallenTreeFeature.isOverSolidGround`: the block below has a sturdy top face.
fn over_solid_ground(world: &impl FeatureWorld, [x, y, z]: [i32; 3]) -> bool {
    block_behavior().is_face_sturdy(world.block_state(x, y - 1, z), Face::Up)
}

/// `FallenTreeFeature.canPlaceEntireFallenLog`: every block free for a tree, with at most two in a row over no
/// solid ground.
fn can_place_entire_log(
    world: &impl FeatureWorld,
    tree: &CompiledFallenTree,
    length: i32,
    start: [i32; 3],
    direction: Direction,
) -> bool {
    let mut gap = 0;
    let mut position = start;
    for _ in 0..length {
        if !valid_tree_pos(world, tree, position) {
            return false;
        }
        if over_solid_ground(world, position) {
            gap = 0;
        } else {
            gap += 1;
            if gap > 2 {
                return false;
            }
        }
        position = direction.step(position);
    }
    true
}

impl FeatureCompiler<'_> {
    /// Compiles a `minecraft:fallen_tree` configuration; None for parts not ported yet.
    pub(crate) fn fallen_tree(
        &mut self,
        name: &str,
        config: &Value,
    ) -> Result<Option<CompiledFallenTree>, WorldgenDataError> {
        let field = |key: &str| {
            config
                .get(key)
                .ok_or_else(|| WorldgenDataError::InvalidDefinition {
                    context: name.to_string(),
                    message: format!("missing field `{key}`"),
                })
        };
        let Some(trunk_provider) = self.provider(name, field("trunk_provider")?)? else {
            return Ok(None);
        };
        let log_length = parse_int_provider(name, field("log_length")?)?;
        if matches!(log_length, IntProviderDef::Other(_)) {
            return Ok(None);
        }
        let (Some(stump_decorators), Some(log_decorators)) = (
            self.decorators(name, config.get("stump_decorators"))?,
            self.decorators(name, config.get("log_decorators"))?,
        ) else {
            return Ok(None);
        };
        Ok(Some(CompiledFallenTree {
            trunk_provider,
            log_length,
            stump_decorators,
            log_decorators,
            blocks: self.tree_blocks()?,
        }))
    }
}
