//! `minecraft:tree` (`TreeFeature`) with straight and fancy trunks, blob, fancy, spruce and pine foliage, and the
//! decorators of [`crate::tree_decorator`].
//!
//! A tree draws its height, its foliage height and radius, checks that the column is inside the world and,
//! shrinking the height when blocks are in the way (`getMaxFreeTreeHeight`), free of blocks a tree may not
//! replace; then it places the block below its trunk, the trunk and the foliage at each attachment its trunk
//! placer returns. Every block it places joins one of four position sets, which vanilla keeps in `HashSet`s:
//! roots, trunk, foliage and decorations. The decorators then run in order on lists of the roots, trunk and
//! foliage taken in Java's iteration order and sorted by height, adding blocks to the decoration set.
//! `updateLeaves` then gives every leaf in the tree's bounds its distance to the nearest log, walking those sets
//! in Java's iteration order, and `updateShapeAtEdge` updates the blocks on both sides of the tree's surface.
//!
//! Trees with a root placer, the other trunk and foliage placers and the other decorators are not ported yet.

use std::sync::Arc;

use harpas_core::block::block_states;
use harpas_core::math::{cos_f64, floor_f32, floor_f64, sin_f64};
use harpas_core::random::{RandomSource, WorldgenRandom};
use serde_json::Value;

use crate::configured::{FeatureCompiler, StateProvider};
use crate::data::{IntProviderDef, WorldgenData, WorldgenDataError, parse_int_provider};
use crate::feature::FeatureWorld;
use crate::java_hash::BlockPosSet;
use crate::placement::{Fluid, StateSet, fluid, sample_int_provider, states_where};
use crate::shape_update::{
    Direction, ShapeUpdates, VoxelShape, property, update_shape_at_edge, with_property,
};
use crate::tree_decorator::{Decorating, Decorator, DecoratorContext, HORIZONTAL};

/// A compiled `minecraft:tree`.
#[derive(Debug)]
pub struct CompiledTree {
    trunk_provider: StateProvider,
    trunk_placer: TrunkPlacer,
    foliage_provider: StateProvider,
    foliage_placer: FoliagePlacer,
    minimum_size: FeatureSize,
    ignore_vines: bool,
    below_trunk_provider: StateProvider,
    decorators: Vec<Decorator>,
    blocks: Arc<TreeBlocks>,
}

/// The block sets every tree reads, compiled once per data pack.
#[derive(Debug)]
pub struct TreeBlocks {
    /// `#replaceable_by_trees`.
    replaceable: StateSet,
    /// `#logs`.
    logs: StateSet,
    /// `#prevents_nearby_leaf_decay`, at distance 0 from a log.
    prevents_decay: StateSet,
    /// States with leaves' `distance` property.
    leaves: StateSet,
    /// Any vine state.
    pub(crate) vine: StateSet,
    /// `#leaves`.
    leaves_tag: StateSet,
    /// Shape updates for the blocks around a tree.
    updates: ShapeUpdates,
    /// The default vine, cocoa and bee nest states.
    pub(crate) vine_default: u32,
    pub(crate) cocoa: u32,
    pub(crate) bee_nest: u32,
}

impl TreeBlocks {
    /// Compiles the block sets from a data pack's tags.
    pub fn new(data: &WorldgenData) -> Result<Self, WorldgenDataError> {
        let tag = |tag: &str| -> Result<StateSet, WorldgenDataError> {
            Ok(StateSet::from_membership(&data.block_tag_states(tag)?))
        };
        let states = block_states();
        Ok(Self {
            replaceable: tag("minecraft:replaceable_by_trees")?,
            logs: tag("minecraft:logs")?,
            prevents_decay: tag("minecraft:prevents_nearby_leaf_decay")?,
            leaves: states_where(|id| {
                states.properties(id).is_some_and(|(name, properties)| {
                    name != "minecraft:scaffolding"
                        && properties.iter().any(|(key, _)| *key == "distance")
                })
            }),
            vine: states_where(|id| {
                states
                    .properties(id)
                    .is_some_and(|(name, _)| name == "minecraft:vine")
            }),
            updates: ShapeUpdates::new(data)?,
            leaves_tag: tag("minecraft:leaves")?,
            vine_default: default_state("minecraft:vine")?,
            cocoa: default_state("minecraft:cocoa")?,
            bee_nest: default_state("minecraft:bee_nest")?,
        })
    }

    /// `TreeFeature.validTreePos`: air or `#replaceable_by_trees`.
    pub(crate) fn valid(&self, state: u32) -> bool {
        block_states().is_air(state) || self.replaceable.contains(state)
    }

    /// `TrunkPlacer.isFree`: a valid tree position or a log.
    fn free(&self, state: u32) -> bool {
        self.valid(state) || self.logs.contains(state)
    }

    /// `LeavesBlock.getOptionalDistanceAt`.
    fn distance(&self, state: u32) -> Option<i32> {
        if self.prevents_decay.contains(state) {
            return Some(0);
        }
        if !self.leaves.contains(state) {
            return None;
        }
        property(state, "distance")?.parse().ok()
    }
}

/// `TrunkPlacer`.
#[derive(Debug)]
enum TrunkPlacer {
    /// `straight_trunk_placer`.
    Straight(TrunkHeight),
    /// `fancy_trunk_placer`.
    Fancy(TrunkHeight),
    /// `forking_trunk_placer`: a trunk that leans one way near its top, and may fork another way.
    Forking(TrunkHeight),
    /// `giant_trunk_placer`: a two-by-two trunk.
    Giant(TrunkHeight),
    /// `mega_jungle_trunk_placer`: a giant trunk with branches near its top.
    MegaJungle(TrunkHeight),
    /// `dark_oak_trunk_placer`: a two-by-two trunk that leans near its top, with short stubs around it.
    DarkOak(TrunkHeight),
    /// `cherry_trunk_placer`: a trunk with one or two branches that bend up or down to their foliage.
    Cherry(TrunkHeight, Box<CherryBranches>),
}

/// The branch providers of `cherry_trunk_placer`.
#[derive(Debug)]
struct CherryBranches {
    count: IntProviderDef,
    horizontal_length: IntProviderDef,
    /// `branch_start_offset_from_top`, a uniform range.
    start: (i32, i32),
    end: IntProviderDef,
}

/// The chances of `cherry_foliage_placer`.
#[derive(Debug)]
struct CherryFoliage {
    height: IntProviderDef,
    wide_bottom_layer_hole: f32,
    corner_hole: f32,
    hanging_leaves: f32,
    hanging_leaves_extension: f32,
}

/// A trunk placer's base height and the bounds of its two random additions.
#[derive(Debug, Clone, Copy)]
struct TrunkHeight {
    base: i32,
    rand_a: i32,
    rand_b: i32,
}

/// `FoliagePlacer`, with its radius and offset providers.
#[derive(Debug)]
struct FoliagePlacer {
    radius: IntProviderDef,
    offset: IntProviderDef,
    kind: FoliageKind,
}

#[derive(Debug)]
enum FoliageKind {
    /// `blob_foliage_placer`, with its fixed height.
    Blob(i32),
    /// `fancy_foliage_placer`, with its fixed height.
    Fancy(i32),
    /// `spruce_foliage_placer`, with the trunk height its foliage leaves bare.
    Spruce(IntProviderDef),
    /// `pine_foliage_placer`, with its height.
    Pine(IntProviderDef),
    /// `acacia_foliage_placer`: a flat canopy.
    Acacia,
    /// `bush_foliage_placer`, with its fixed height.
    Bush(i32),
    /// `jungle_foliage_placer` (`MegaJungleFoliagePlacer`), with its fixed height.
    Jungle(i32),
    /// `mega_pine_foliage_placer`, with the height of its crown.
    MegaPine(IntProviderDef),
    /// `dark_oak_foliage_placer`.
    DarkOak,
    /// `cherry_foliage_placer`.
    Cherry(Box<CherryFoliage>),
}

/// `FeatureSize`: the radius a tree needs free at each height.
#[derive(Debug)]
struct FeatureSize {
    kind: SizeKind,
    min_clipped_height: Option<i32>,
}

#[derive(Debug)]
enum SizeKind {
    /// `two_layers_feature_size`.
    TwoLayers { limit: i32, lower: i32, upper: i32 },
    /// `three_layers_feature_size`.
    ThreeLayers {
        limit: i32,
        upper_limit: i32,
        lower: i32,
        middle: i32,
        upper: i32,
    },
}

impl FeatureSize {
    /// `getSizeAtHeight`.
    fn size_at_height(&self, height: i32, y: i32) -> i32 {
        match self.kind {
            SizeKind::TwoLayers {
                limit,
                lower,
                upper,
            } => {
                if y < limit {
                    lower
                } else {
                    upper
                }
            }
            SizeKind::ThreeLayers {
                limit,
                upper_limit,
                lower,
                middle,
                upper,
            } => {
                if y < limit {
                    lower
                } else if y >= height - upper_limit {
                    upper
                } else {
                    middle
                }
            }
        }
    }
}

/// `FoliagePlacer.FoliageAttachment`.
#[derive(Debug, Clone, Copy)]
struct Attachment {
    position: [i32; 3],
    radius_offset: i32,
    double_trunk: bool,
}

impl Attachment {
    fn at(position: [i32; 3]) -> Self {
        Self {
            position,
            radius_offset: 0,
            double_trunk: false,
        }
    }
}

/// The four position sets of `TreeFeature.place`.
#[derive(Default)]
struct TreeSets {
    roots: BlockPosSet,
    trunk: BlockPosSet,
    foliage: BlockPosSet,
    decorations: BlockPosSet,
}

/// What a tree places, and the random source and sets it places with.
struct Placer<'a, W, R: RandomSource> {
    world: &'a mut W,
    tree: &'a CompiledTree,
    random: &'a mut WorldgenRandom<R>,
    sets: TreeSets,
}

impl<W: FeatureWorld, R: RandomSource> Placer<'_, W, R> {
    fn state(&self, [x, y, z]: [i32; 3]) -> u32 {
        self.world.block_state(x, y, z)
    }

    /// The trunk setter: also used for the block below the trunk.
    fn set_trunk(&mut self, position: [i32; 3], state: u32) {
        self.sets.trunk.insert(position);
        self.world
            .set_block_state(position[0], position[1], position[2], state);
    }

    fn set_foliage(&mut self, position: [i32; 3], state: u32) {
        self.sets.foliage.insert(position);
        self.world
            .set_block_state(position[0], position[1], position[2], state);
    }

    fn sample(&mut self, provider: &IntProviderDef) -> i32 {
        sample_int_provider(provider, self.random).unwrap_or(0)
    }

    /// `TreeFeature.doPlace`.
    fn place(&mut self, origin: [i32; 3]) -> bool {
        let tree = self.tree;
        let tree_height = tree.trunk_placer.tree_height(self.random);
        let foliage_height = match &tree.foliage_placer.kind {
            FoliageKind::Blob(height)
            | FoliageKind::Fancy(height)
            | FoliageKind::Bush(height)
            | FoliageKind::Jungle(height) => *height,
            FoliageKind::Acacia => 0,
            FoliageKind::DarkOak => 4,
            FoliageKind::Cherry(cherry) => self.sample(&cherry.height),
            FoliageKind::Spruce(trunk) => 4.max(tree_height - self.sample(trunk)),
            FoliageKind::Pine(height) | FoliageKind::MegaPine(height) => self.sample(height),
        };
        let trunk_length = tree_height - foliage_height;
        let mut foliage_radius = self.sample(&tree.foliage_placer.radius);
        if matches!(tree.foliage_placer.kind, FoliageKind::Pine(_)) {
            foliage_radius += self.random.next_int_bounded((trunk_length + 1).max(1));
        }
        let top = origin[1] + tree_height + 1;
        let world_max_y = self.world.min_y() + self.world.height() - 1;
        if origin[1] < self.world.min_y() + 1 || top > world_max_y + 1 {
            return false;
        }
        let free_height = self.max_free_height(tree_height, origin);
        if free_height < tree_height
            && tree
                .minimum_size
                .min_clipped_height
                .is_none_or(|min| free_height < min)
        {
            return false;
        }
        let attachments = match tree.trunk_placer {
            TrunkPlacer::Straight(_) => self.straight_trunk(free_height, origin),
            TrunkPlacer::Fancy(_) => self.fancy_trunk(free_height, origin),
            TrunkPlacer::Forking(_) => self.forking_trunk(free_height, origin),
            TrunkPlacer::Giant(_) => self.giant_trunk(free_height, origin),
            TrunkPlacer::MegaJungle(_) => self.mega_jungle_trunk(free_height, origin),
            TrunkPlacer::DarkOak(_) => self.dark_oak_trunk(free_height, origin),
            TrunkPlacer::Cherry(_, ref branches) => {
                self.cherry_trunk(free_height, origin, branches)
            }
        };
        for attachment in attachments {
            self.create_foliage(attachment, foliage_height, foliage_radius);
        }
        true
    }

    /// `TreeFeature.getMaxFreeTreeHeight`.
    fn max_free_height(&self, tree_height: i32, [x, y, z]: [i32; 3]) -> i32 {
        let tree = self.tree;
        for dy in 0..=tree_height + 1 {
            let radius = tree.minimum_size.size_at_height(tree_height, dy);
            for dx in -radius..=radius {
                for dz in -radius..=radius {
                    let state = self.state([x + dx, y + dy, z + dz]);
                    if !tree.blocks.free(state)
                        || (!tree.ignore_vines && tree.blocks.vine.contains(state))
                    {
                        return dy - 2;
                    }
                }
            }
        }
        tree_height
    }

    /// `TrunkPlacer.placeBelowTrunkBlock`.
    fn below_trunk(&mut self, position: [i32; 3]) {
        if let Some(state) =
            self.tree
                .below_trunk_provider
                .optional_state(self.world, self.random, position)
        {
            self.set_trunk(position, state);
        }
    }

    /// `TrunkPlacer.placeLog`, setting the log's axis when one is given.
    fn place_log(&mut self, position: [i32; 3], axis: Option<&str>) -> bool {
        if !self.tree.blocks.valid(self.state(position)) {
            return false;
        }
        let mut state = self
            .tree
            .trunk_provider
            .state(self.world, self.random, position);
        if let Some(axis) = axis {
            state = with_property(state, "axis", axis);
        }
        self.set_trunk(position, state);
        true
    }

    /// `StraightTrunkPlacer.placeTrunk`.
    fn straight_trunk(&mut self, height: i32, [x, y, z]: [i32; 3]) -> Vec<Attachment> {
        self.below_trunk([x, y - 1, z]);
        for dy in 0..height {
            self.place_log([x, y + dy, z], None);
        }
        vec![Attachment::at([x, y + height, z])]
    }

    /// `FancyTrunkPlacer.placeTrunk`.
    fn fancy_trunk(&mut self, free_height: i32, origin: [i32; 3]) -> Vec<Attachment> {
        let height = free_height + 2;
        let trunk_height = floor_f64(f64::from(height) * 0.618);
        self.below_trunk([origin[0], origin[1] - 1, origin[2]]);
        let ratio = f64::from(height) / 13.0;
        let clusters = 1.min(floor_f64(1.382 + ratio * ratio));
        let tree_top = origin[1] + trunk_height;
        let mut relative_y = height - 5;
        // Each foliage cluster with the height its branch starts at.
        let mut foliage = vec![([origin[0], origin[1] + relative_y, origin[2]], tree_top)];
        while relative_y >= 0 {
            let shape = tree_shape(height, relative_y);
            if shape >= 0.0 {
                for _ in 0..clusters {
                    let radius = f64::from(shape) * (f64::from(self.random.next_float()) + 0.328);
                    let angle = f64::from(self.random.next_float() * 2.0) * std::f64::consts::PI;
                    let x = radius * angle.sin() + 0.5;
                    let z = radius * angle.cos() + 0.5;
                    let start = [
                        origin[0] + floor_f64(x),
                        origin[1] + relative_y - 1,
                        origin[2] + floor_f64(z),
                    ];
                    let end = [start[0], start[1] + 5, start[2]];
                    if !self.make_limb(start, end, false) {
                        continue;
                    }
                    let dx = origin[0] - start[0];
                    let dz = origin[2] - start[2];
                    let branch_height =
                        f64::from(start[1]) - f64::from(dx * dx + dz * dz).sqrt() * 0.381;
                    let branch_top = if branch_height > f64::from(tree_top) {
                        tree_top
                    } else {
                        branch_height as i32
                    };
                    let base = [origin[0], branch_top, origin[2]];
                    if self.make_limb(base, start, false) {
                        foliage.push((start, branch_top));
                    }
                }
            }
            relative_y -= 1;
        }
        self.make_limb(
            origin,
            [origin[0], origin[1] + trunk_height, origin[2]],
            true,
        );
        for &(position, branch_base) in &foliage {
            let base = [origin[0], branch_base, origin[2]];
            if base != position && trim_branches(height, branch_base - origin[1]) {
                self.make_limb(base, position, true);
            }
        }
        foliage
            .into_iter()
            .filter(|&(_, branch_base)| trim_branches(height, branch_base - origin[1]))
            .map(|(position, _)| Attachment::at(position))
            .collect()
    }

    /// `ForkingTrunkPlacer.placeTrunk`: from a random height near the top, the trunk leans up to three blocks
    /// one way; a second branch in another random direction starts lower and leans one to three blocks.
    fn forking_trunk(&mut self, height: i32, origin: [i32; 3]) -> Vec<Attachment> {
        self.below_trunk([origin[0], origin[1] - 1, origin[2]]);
        let mut attachments = Vec::new();
        let lean = HORIZONTAL[self.random.next_int_bounded(4) as usize];
        let lean_height = height - self.random.next_int_bounded(4) - 1;
        let mut lean_steps = 3 - self.random.next_int_bounded(3);
        let [mut x, _, mut z] = origin;
        let mut top = None;
        for dy in 0..height {
            let y = origin[1] + dy;
            if dy >= lean_height && lean_steps > 0 {
                let [step_x, _, step_z] = lean.offset();
                x += step_x;
                z += step_z;
                lean_steps -= 1;
            }
            if self.place_log([x, y, z], None) {
                top = Some(y + 1);
            }
        }
        if let Some(top) = top {
            attachments.push(Attachment {
                position: [x, top, z],
                radius_offset: 1,
                double_trunk: false,
            });
        }
        [x, _, z] = origin;
        let fork = HORIZONTAL[self.random.next_int_bounded(4) as usize];
        if fork != lean {
            let fork_height = lean_height - self.random.next_int_bounded(2) - 1;
            let mut fork_steps = 1 + self.random.next_int_bounded(3);
            let mut top = None;
            let mut dy = fork_height;
            while dy < height && fork_steps > 0 {
                if dy >= 1 {
                    let y = origin[1] + dy;
                    let [step_x, _, step_z] = fork.offset();
                    x += step_x;
                    z += step_z;
                    if self.place_log([x, y, z], None) {
                        top = Some(y + 1);
                    }
                }
                dy += 1;
                fork_steps -= 1;
            }
            if let Some(top) = top {
                attachments.push(Attachment::at([x, top, z]));
            }
        }
        attachments
    }

    /// `GiantTrunkPlacer.placeTrunk`: a two-by-two trunk toward positive X and Z, whose top layer is only the
    /// origin's column, with dirt below all four columns.
    fn giant_trunk(&mut self, height: i32, [x, y, z]: [i32; 3]) -> Vec<Attachment> {
        for [dx, dz] in [[0, 0], [1, 0], [0, 1], [1, 1]] {
            self.below_trunk([x + dx, y - 1, z + dz]);
        }
        for dy in 0..height {
            self.place_log_if_free([x, y + dy, z]);
            if dy < height - 1 {
                self.place_log_if_free([x + 1, y + dy, z]);
                self.place_log_if_free([x + 1, y + dy, z + 1]);
                self.place_log_if_free([x, y + dy, z + 1]);
            }
        }
        vec![Attachment {
            position: [x, y + height, z],
            radius_offset: 0,
            double_trunk: true,
        }]
    }

    /// `MegaJungleTrunkPlacer.placeTrunk`: a giant trunk, then branches of five logs leaning out from its upper
    /// half, each with a small foliage cluster.
    fn mega_jungle_trunk(&mut self, height: i32, origin: [i32; 3]) -> Vec<Attachment> {
        let mut attachments = self.giant_trunk(height, origin);
        let mut branch_y = height - 2 - self.random.next_int_bounded(4);
        while branch_y > height / 2 {
            let angle = self.random.next_float() * std::f32::consts::TAU;
            let (mut dx, mut dz) = (0, 0);
            for step in 0..5 {
                dx = (1.5 + cos_f64(f64::from(angle)) * step as f32) as i32;
                dz = (1.5 + sin_f64(f64::from(angle)) * step as f32) as i32;
                let position = [
                    origin[0] + dx,
                    origin[1] + branch_y - 3 + step / 2,
                    origin[2] + dz,
                ];
                self.place_log(position, None);
            }
            attachments.push(Attachment {
                position: [origin[0] + dx, origin[1] + branch_y, origin[2] + dz],
                radius_offset: -2,
                double_trunk: false,
            });
            branch_y -= 2 + self.random.next_int_bounded(4);
        }
        attachments
    }

    /// `DarkOakTrunkPlacer.placeTrunk`: a two-by-two trunk with dirt below it that leans up to two blocks one
    /// way near its top, placed only over air or leaves, then stubs of two to four logs hanging from the top
    /// layer in a third of the positions around the unleaned trunk, each with its own foliage.
    fn dark_oak_trunk(&mut self, height: i32, origin: [i32; 3]) -> Vec<Attachment> {
        let [ox, oy, oz] = origin;
        for [dx, dz] in [[0, 0], [1, 0], [0, 1], [1, 1]] {
            self.below_trunk([ox + dx, oy - 1, oz + dz]);
        }
        let lean = HORIZONTAL[self.random.next_int_bounded(4) as usize];
        let lean_height = height - self.random.next_int_bounded(4);
        let mut lean_steps = 2 - self.random.next_int_bounded(3);
        let (mut x, mut z) = (ox, oz);
        let top = oy + height - 1;
        for dy in 0..height {
            if dy >= lean_height && lean_steps > 0 {
                let [step_x, _, step_z] = lean.offset();
                x += step_x;
                z += step_z;
                lean_steps -= 1;
            }
            let position = [x, oy + dy, z];
            if self.is_air_or_leaves(position) {
                for [dx, dz] in [[0, 0], [1, 0], [0, 1], [1, 1]] {
                    self.place_log([x + dx, oy + dy, z + dz], None);
                }
            }
        }
        let mut attachments = vec![Attachment {
            position: [x, top, z],
            radius_offset: 0,
            double_trunk: true,
        }];
        for dx in -1..=2 {
            for dz in -1..=2 {
                if (0..=1).contains(&dx) && (0..=1).contains(&dz) {
                    continue;
                }
                if self.random.next_int_bounded(3) > 0 {
                    continue;
                }
                let length = self.random.next_int_bounded(3) + 2;
                for i in 0..length {
                    self.place_log([ox + dx, top - i - 1, oz + dz], None);
                }
                attachments.push(Attachment::at([ox + dx, top, oz + dz]));
            }
        }
        attachments
    }

    /// `TreeFeature.isAirOrLeaves`.
    fn is_air_or_leaves(&self, position: [i32; 3]) -> bool {
        let state = self.state(position);
        block_states().is_air(state) || self.tree.blocks.leaves_tag.contains(state)
    }

    /// `CherryTrunkPlacer.placeTrunk`: one to three branches from below the top. With three, the trunk reaches
    /// the full height and holds foliage of its own; with two, it stops above the higher branch.
    fn cherry_trunk(
        &mut self,
        height: i32,
        origin: [i32; 3],
        branches: &CherryBranches,
    ) -> Vec<Attachment> {
        self.below_trunk([origin[0], origin[1] - 1, origin[2]]);
        let (min, max) = branches.start;
        let first = 0.max(height - 1 + self.random.next_int_between_inclusive(min, max));
        let mut second = 0.max(height - 1 + self.random.next_int_between_inclusive(min, max - 1));
        if second >= first {
            second += 1;
        }
        let count = self.sample(&branches.count);
        let (three, two) = (count == 3, count >= 2);
        let trunk_height = if three {
            height
        } else if two {
            first.max(second) + 1
        } else {
            first + 1
        };
        for dy in 0..trunk_height {
            self.place_log([origin[0], origin[1] + dy, origin[2]], None);
        }
        let mut attachments = Vec::new();
        if three {
            attachments.push(Attachment::at([
                origin[0],
                origin[1] + trunk_height,
                origin[2],
            ]));
        }
        let direction = HORIZONTAL[self.random.next_int_bounded(4) as usize];
        attachments.push(self.cherry_branch(
            height,
            origin,
            branches,
            direction,
            first,
            first < trunk_height - 1,
        ));
        if two {
            attachments.push(self.cherry_branch(
                height,
                origin,
                branches,
                direction.opposite(),
                second,
                second < trunk_height - 1,
            ));
        }
        attachments
    }

    /// `CherryTrunkPlacer.generateBranch`: from the trunk at `start`, one or two logs sideways, then a path to
    /// the branch's end that steps up or down with a chance that grows with the height left to cover.
    fn cherry_branch(
        &mut self,
        height: i32,
        origin: [i32; 3],
        branches: &CherryBranches,
        direction: Direction,
        start: i32,
        below_top: bool,
    ) -> Attachment {
        let mut position = [origin[0], origin[1] + start, origin[2]];
        let end_height = height - 1 + self.sample(&branches.end);
        let extend = below_top || end_height < start;
        let length = self.sample(&branches.horizontal_length) + i32::from(extend);
        let [dx, _, dz] = direction.offset();
        let end = [
            origin[0] + dx * length,
            origin[1] + end_height,
            origin[2] + dz * length,
        ];
        let axis = if dx != 0 { "x" } else { "z" };
        for _ in 0..if extend { 2 } else { 1 } {
            position = direction.step(position);
            self.place_log(position, Some(axis));
        }
        let vertical = if end[1] > position[1] {
            Direction::Up
        } else {
            Direction::Down
        };
        loop {
            let distance = (0..3).map(|i| (position[i] - end[i]).abs()).sum::<i32>();
            if distance == 0 {
                break;
            }
            let chance = (end[1] - position[1]).abs() as f32 / distance as f32;
            let up_or_down = self.random.next_float() < chance;
            position = if up_or_down {
                vertical.step(position)
            } else {
                direction.step(position)
            };
            self.place_log(position, if up_or_down { None } else { Some(axis) });
        }
        Attachment::at([end[0], end[1] + 1, end[2]])
    }

    /// `TrunkPlacer.placeLogIfFree`: a log where the position is free; `placeLog` then places it only on a
    /// valid tree position, so an existing log stays.
    fn place_log_if_free(&mut self, position: [i32; 3]) {
        if self.tree.blocks.free(self.state(position)) {
            self.place_log(position, None);
        }
    }

    /// `FancyTrunkPlacer.makeLimb`: places logs along a line, or checks that it is free.
    fn make_limb(&mut self, start: [i32; 3], end: [i32; 3], make: bool) -> bool {
        if !make && start == end {
            return true;
        }
        let delta = [end[0] - start[0], end[1] - start[1], end[2] - start[2]];
        let steps = delta[0].abs().max(delta[1].abs()).max(delta[2].abs());
        let step = delta.map(|d| d as f32 / steps as f32);
        for i in 0..=steps {
            let at = |axis: usize| start[axis] + floor_f32(0.5 + i as f32 * step[axis]);
            let position = [at(0), at(1), at(2)];
            if make {
                let axis = log_axis(start, position);
                self.place_log(position, Some(axis));
            } else if !self.tree.blocks.free(self.state(position)) {
                return false;
            }
        }
        true
    }

    /// `FoliagePlacer.createFoliage`, after the offset is drawn.
    fn create_foliage(&mut self, attachment: Attachment, foliage_height: i32, foliage_radius: i32) {
        let tree = self.tree;
        let offset = self.sample(&tree.foliage_placer.offset);
        let position = attachment.position;
        let double_trunk = attachment.double_trunk;
        match tree.foliage_placer.kind {
            FoliageKind::Blob(_) => {
                for y in (offset - foliage_height..=offset).rev() {
                    let radius = (foliage_radius + attachment.radius_offset - 1 - y / 2).max(0);
                    self.leaves_row(position, radius, y, double_trunk);
                }
            }
            FoliageKind::Fancy(_) => {
                for y in (offset - foliage_height..=offset).rev() {
                    let edge = y == offset || y == offset - foliage_height;
                    let radius = foliage_radius + if edge { 0 } else { 1 };
                    self.leaves_row(position, radius, y, double_trunk);
                }
            }
            FoliageKind::Spruce(_) => {
                let mut current = self.random.next_int_bounded(2);
                let mut max = 1;
                let mut min = 0;
                for y in (-foliage_height..=offset).rev() {
                    self.leaves_row(position, current, y, double_trunk);
                    if current >= max {
                        current = min;
                        min = 1;
                        max = (max + 1).min(foliage_radius + attachment.radius_offset);
                    } else {
                        current += 1;
                    }
                }
            }
            FoliageKind::Acacia => {
                let position = [position[0], position[1] + offset, position[2]];
                let radius = foliage_radius + attachment.radius_offset;
                self.leaves_row(position, radius, -1 - foliage_height, double_trunk);
                self.leaves_row(position, foliage_radius - 1, -foliage_height, double_trunk);
                self.leaves_row(position, radius - 1, 0, double_trunk);
            }
            FoliageKind::Bush(_) => {
                for y in (offset - foliage_height..=offset).rev() {
                    let radius = foliage_radius + attachment.radius_offset - 1 - y;
                    self.leaves_row(position, radius, y, double_trunk);
                }
            }
            FoliageKind::Jungle(_) => {
                let height = if double_trunk {
                    foliage_height
                } else {
                    1 + self.random.next_int_bounded(2)
                };
                for y in (offset - height..=offset).rev() {
                    let radius = foliage_radius + attachment.radius_offset + 1 - y;
                    self.leaves_row(position, radius, y, double_trunk);
                }
            }
            FoliageKind::DarkOak => {
                let position = [position[0], position[1] + offset, position[2]];
                if double_trunk {
                    self.leaves_row(position, foliage_radius + 2, -1, true);
                    self.leaves_row(position, foliage_radius + 3, 0, true);
                    self.leaves_row(position, foliage_radius + 2, 1, true);
                    if self.random.next_bool() {
                        self.leaves_row(position, foliage_radius, 2, true);
                    }
                } else {
                    self.leaves_row(position, foliage_radius + 2, -1, false);
                    self.leaves_row(position, foliage_radius + 1, 0, false);
                }
            }
            FoliageKind::Cherry(ref cherry) => {
                let position = [position[0], position[1] + offset, position[2]];
                let radius = foliage_radius + attachment.radius_offset - 1;
                self.leaves_row(position, radius - 2, foliage_height - 3, double_trunk);
                self.leaves_row(position, radius - 1, foliage_height - 4, double_trunk);
                for y in (0..=foliage_height - 5).rev() {
                    self.leaves_row(position, radius, y, double_trunk);
                }
                let (hanging, extension) = (cherry.hanging_leaves, cherry.hanging_leaves_extension);
                self.hanging_leaves_row(position, radius, -1, double_trunk, hanging, extension);
                self.hanging_leaves_row(position, radius - 1, -2, double_trunk, hanging, extension);
            }
            FoliageKind::MegaPine(_) => {
                // Rows from the bottom of the crown up, widening downward, with every other row one wider
                // where the width would repeat.
                let mut previous = 0;
                for y in position[1] - foliage_height + offset..=position[1] + offset {
                    let depth = position[1] - y;
                    let radius = foliage_radius
                        + attachment.radius_offset
                        + floor_f32(depth as f32 / foliage_height as f32 * 3.5);
                    let row = if depth > 0 && radius == previous && y & 1 == 0 {
                        radius + 1
                    } else {
                        radius
                    };
                    self.leaves_row([position[0], y, position[2]], row, 0, double_trunk);
                    previous = radius;
                }
            }
            FoliageKind::Pine(_) => {
                let mut current = 0;
                for y in (offset - foliage_height..=offset).rev() {
                    self.leaves_row(position, current, y, double_trunk);
                    if current >= 1 && y == offset - foliage_height + 1 {
                        current -= 1;
                    } else if current < foliage_radius + attachment.radius_offset {
                        current += 1;
                    }
                }
            }
        }
    }

    /// `FoliagePlacer.placeLeavesRow`.
    fn leaves_row(&mut self, [x, y, z]: [i32; 3], radius: i32, dy: i32, double_trunk: bool) {
        let extra = i32::from(double_trunk);
        for dx in -radius..=radius + extra {
            for dz in -radius..=radius + extra {
                if !self.skip_signed(dx, dy, dz, radius, double_trunk) {
                    self.try_place_leaf([x + dx, y + dy, z + dz]);
                }
            }
        }
    }

    /// `FoliagePlacer.placeLeavesRowWithHangingLeavesBelow`: a row, then below each leaf along its edge, by
    /// chance, a hanging leaf, and below that, by another chance, a second one, all within six blocks of the
    /// position below the attachment.
    fn hanging_leaves_row(
        &mut self,
        position: [i32; 3],
        radius: i32,
        y: i32,
        double_trunk: bool,
        hanging: f32,
        extension: f32,
    ) {
        self.leaves_row(position, radius, y, double_trunk);
        let extra = i32::from(double_trunk);
        let below = [position[0], position[1] - 1, position[2]];
        for direction in HORIZONTAL {
            let clockwise = match direction {
                Direction::North => Direction::East,
                Direction::East => Direction::South,
                Direction::South => Direction::West,
                _ => Direction::North,
            };
            let along = if matches!(clockwise, Direction::East | Direction::South) {
                radius + extra
            } else {
                radius
            };
            let [cx, _, cz] = clockwise.offset();
            let [dx, _, dz] = direction.offset();
            let mut leaf = [
                position[0] + cx * along - dx * radius,
                position[1] + y - 1,
                position[2] + cz * along - dz * radius,
            ];
            for _ in -radius..radius + extra {
                let above = [leaf[0], leaf[1] + 1, leaf[2]];
                if self.sets.foliage.contains(above) && self.hanging_extension(hanging, below, leaf)
                {
                    let lower = [leaf[0], leaf[1] - 1, leaf[2]];
                    self.hanging_extension(extension, below, lower);
                }
                leaf = direction.step(leaf);
            }
        }
    }

    /// `FoliagePlacer.tryPlaceExtension`.
    fn hanging_extension(&mut self, chance: f32, origin: [i32; 3], position: [i32; 3]) -> bool {
        let distance = (0..3).map(|i| (position[i] - origin[i]).abs()).sum::<i32>();
        if distance >= 7 || self.random.next_float() > chance {
            return false;
        }
        self.try_place_leaf(position)
    }

    /// `FoliagePlacer.shouldSkipLocationSigned`, then each placer's `shouldSkipLocation`.
    fn skip_signed(&mut self, dx: i32, y: i32, dz: i32, radius: i32, double_trunk: bool) -> bool {
        // `DarkOakFoliagePlacer.shouldSkipLocationSigned`: the corners of the middle layer of a double trunk.
        if matches!(self.tree.foliage_placer.kind, FoliageKind::DarkOak)
            && y == 0
            && double_trunk
            && (dx == -radius || dx >= radius)
            && (dz == -radius || dz >= radius)
        {
            return true;
        }
        let (dx, dz) = if double_trunk {
            (dx.abs().min((dx - 1).abs()), dz.abs().min((dz - 1).abs()))
        } else {
            (dx.abs(), dz.abs())
        };
        match self.tree.foliage_placer.kind {
            FoliageKind::Blob(_) => {
                dx == radius && dz == radius && (self.random.next_int_bounded(2) == 0 || y == 0)
            }
            FoliageKind::Fancy(_) => {
                let square = |v: i32| (v as f32 + 0.5) * (v as f32 + 0.5);
                square(dx) + square(dz) > (radius * radius) as f32
            }
            FoliageKind::Spruce(_) | FoliageKind::Pine(_) => {
                dx == radius && dz == radius && radius > 0
            }
            FoliageKind::Acacia => {
                if y == 0 {
                    (dx > 1 || dz > 1) && dx != 0 && dz != 0
                } else {
                    dx == radius && dz == radius && radius > 0
                }
            }
            FoliageKind::Bush(_) => {
                dx == radius && dz == radius && self.random.next_int_bounded(2) == 0
            }
            FoliageKind::Jungle(_) | FoliageKind::MegaPine(_) => {
                dx + dz >= 7 || dx * dx + dz * dz > radius * radius
            }
            FoliageKind::DarkOak => {
                if y == -1 && !double_trunk {
                    dx == radius && dz == radius
                } else if y == 1 {
                    dx + dz > radius * 2 - 2
                } else {
                    false
                }
            }
            FoliageKind::Cherry(ref cherry) => {
                if y == -1
                    && (dx == radius || dz == radius)
                    && self.random.next_float() < cherry.wide_bottom_layer_hole
                {
                    return true;
                }
                let corner = dx == radius && dz == radius;
                if radius > 2 {
                    corner
                        || (dx + dz > radius * 2 - 2
                            && self.random.next_float() < cherry.corner_hole)
                } else {
                    corner && self.random.next_float() < cherry.corner_hole
                }
            }
        }
    }

    /// `FoliagePlacer.tryPlaceLeaf`: leaves replace valid tree positions that are not persistent leaves, and
    /// are waterlogged in a water source.
    fn try_place_leaf(&mut self, position: [i32; 3]) -> bool {
        let current = self.state(position);
        if property(current, "persistent") == Some("true") || !self.tree.blocks.valid(current) {
            return false;
        }
        let mut state = self
            .tree
            .foliage_provider
            .state(self.world, self.random, position);
        if property(state, "waterlogged").is_some() {
            let water = if fluid(current) == Fluid::Water {
                "true"
            } else {
                "false"
            };
            state = with_property(state, "waterlogged", water);
        }
        self.set_foliage(position, state);
        true
    }
}

fn default_state(name: &str) -> Result<u32, WorldgenDataError> {
    block_states()
        .default_state(name)
        .ok_or_else(|| invalid(name, "unknown block"))
}

impl TrunkPlacer {
    /// `TrunkPlacer.getTreeHeight`.
    fn tree_height<R: RandomSource>(&self, random: &mut WorldgenRandom<R>) -> i32 {
        let (Self::Straight(height)
        | Self::Fancy(height)
        | Self::Forking(height)
        | Self::Giant(height)
        | Self::MegaJungle(height)
        | Self::DarkOak(height)
        | Self::Cherry(height, _)) = self;
        height.base
            + random.next_int_bounded(height.rand_a + 1)
            + random.next_int_bounded(height.rand_b + 1)
    }
}

/// `FancyTrunkPlacer.treeShape`.
fn tree_shape(height: i32, y: i32) -> f32 {
    if (y as f32) < height as f32 * 0.3 {
        return -1.0;
    }
    let radius = height as f32 / 2.0;
    let adjacent = radius - y as f32;
    let mut distance = (radius * radius - adjacent * adjacent).sqrt();
    if adjacent == 0.0 {
        distance = radius;
    } else if adjacent.abs() >= radius {
        return 0.0;
    }
    distance * 0.5
}

/// `FancyTrunkPlacer.trimBranches`.
fn trim_branches(height: i32, local_y: i32) -> bool {
    f64::from(local_y) >= f64::from(height) * 0.2
}

/// `FancyTrunkPlacer.getLogAxis`.
fn log_axis(start: [i32; 3], end: [i32; 3]) -> &'static str {
    let dx = (end[0] - start[0]).abs();
    let dz = (end[2] - start[2]).abs();
    let max = dx.max(dz);
    if max > 0 {
        if dx == max { "x" } else { "z" }
    } else {
        "y"
    }
}

/// `TreeFeature.place`: places the tree, then updates the distance of the leaves in its bounds and the shapes
/// around it.
pub fn place_tree<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    tree: &CompiledTree,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    let mut placer = Placer {
        world,
        tree,
        random,
        sets: TreeSets::default(),
    };
    if !placer.place(origin) || (placer.sets.trunk.is_empty() && placer.sets.foliage.is_empty()) {
        return false;
    }
    let Placer {
        world,
        random,
        mut sets,
        ..
    } = placer;
    if !tree.decorators.is_empty() {
        let context = DecoratorContext::new(&sets.trunk, &sets.foliage, &sets.roots);
        Decorating {
            world: &mut *world,
            random: &mut *random,
            blocks: &tree.blocks,
            decorations: Some(&mut sets.decorations),
        }
        .run(&tree.decorators, &context);
    }
    let positions = sets
        .roots
        .iter()
        .chain(sets.trunk.iter())
        .chain(sets.foliage.iter())
        .chain(sets.decorations.iter());
    let Some(bounds) = Bounds::encapsulating(positions) else {
        return false;
    };
    let shape = update_leaves(world, &tree.blocks, &bounds, &sets);
    update_shape_at_edge(world, &tree.blocks.updates, &shape, bounds.min);
    true
}

/// An inclusive box of block positions (`BoundingBox`).
struct Bounds {
    min: [i32; 3],
    max: [i32; 3],
}

impl Bounds {
    /// `BoundingBox.encapsulatingPositions`.
    fn encapsulating(positions: impl Iterator<Item = [i32; 3]>) -> Option<Self> {
        let mut bounds: Option<Self> = None;
        for position in positions {
            let bounds = bounds.get_or_insert(Self {
                min: position,
                max: position,
            });
            for (axis, &value) in position.iter().enumerate() {
                bounds.min[axis] = bounds.min[axis].min(value);
                bounds.max[axis] = bounds.max[axis].max(value);
            }
        }
        bounds
    }

    fn contains(&self, position: [i32; 3]) -> bool {
        (0..3).all(|axis| (self.min[axis]..=self.max[axis]).contains(&position[axis]))
    }

    fn cell(&self, position: [i32; 3]) -> [i32; 3] {
        [
            position[0] - self.min[0],
            position[1] - self.min[1],
            position[2] - self.min[2],
        ]
    }

    fn size(&self) -> [i32; 3] {
        [
            self.max[0] - self.min[0] + 1,
            self.max[1] - self.min[1] + 1,
            self.max[2] - self.min[2] + 1,
        ]
    }
}

/// `TreeFeature.updateLeaves`: starting from the trunk, sets each leaf in the bounds to its distance from the
/// nearest log, and returns the cells it visited with the decorations and roots.
///
/// Positions wait in one `HashSet` per distance and are taken from the lowest distance that has any, in Java's
/// iteration order. Vanilla lowers the current distance as soon as a neighbor joins a lower set, before it
/// looks at the remaining neighbors, and a position waiting at two distances is set at both; both quirks
/// decide the distances of some leaves and are kept.
fn update_leaves(
    world: &mut impl FeatureWorld,
    blocks: &TreeBlocks,
    bounds: &Bounds,
    sets: &TreeSets,
) -> VoxelShape {
    const MAX_DISTANCE: i32 = 7;
    let mut shape = VoxelShape::new(bounds.size());
    let mut waiting: Vec<BlockPosSet> = (0..MAX_DISTANCE).map(|_| BlockPosSet::new()).collect();
    for position in sets.decorations.iter().chain(sets.roots.iter()) {
        if bounds.contains(position) {
            shape.fill(bounds.cell(position));
        }
    }
    for position in sets.trunk.iter() {
        waiting[0].insert(position);
    }
    let mut distance = 0;
    loop {
        while distance < MAX_DISTANCE && waiting[distance as usize].is_empty() {
            distance += 1;
        }
        if distance >= MAX_DISTANCE {
            break;
        }
        let Some(position) = waiting[distance as usize].pop_first() else {
            break;
        };
        if !bounds.contains(position) {
            continue;
        }
        let [x, y, z] = position;
        if distance != 0 {
            let state = world.block_state(x, y, z);
            let updated = with_property(state, "distance", &distance.to_string());
            world.set_block_state(x, y, z, updated);
        }
        shape.fill(bounds.cell(position));
        for direction in Direction::ALL {
            let neighbor = direction.step(position);
            if !bounds.contains(neighbor) || shape.is_full(bounds.cell(neighbor)) {
                continue;
            }
            let state = world.block_state(neighbor[0], neighbor[1], neighbor[2]);
            let Some(current) = blocks.distance(state) else {
                continue;
            };
            let next = current.min(distance + 1);
            if next < MAX_DISTANCE {
                waiting[next as usize].insert(neighbor);
                distance = distance.min(next);
            }
        }
    }
    shape
}

impl FeatureCompiler<'_> {
    /// Compiles a `minecraft:tree` configuration; None for parts not ported yet.
    pub(crate) fn tree(
        &mut self,
        name: &str,
        config: &Value,
    ) -> Result<Option<CompiledTree>, WorldgenDataError> {
        let field = |key: &str| {
            config
                .get(key)
                .ok_or_else(|| invalid(name, format!("missing field `{key}`")))
        };
        if config.get("root_placer").is_some() {
            return Ok(None);
        }
        let Some(decorators) = self.decorators(name, config.get("decorators"))? else {
            return Ok(None);
        };
        let (Some(trunk_provider), Some(foliage_provider), Some(below_trunk_provider)) = (
            self.provider(name, field("trunk_provider")?)?,
            self.provider(name, field("foliage_provider")?)?,
            self.provider(name, field("below_trunk_provider")?)?,
        ) else {
            return Ok(None);
        };
        let (Some(trunk_placer), Some(foliage_placer), Some(minimum_size)) = (
            trunk_placer(name, field("trunk_placer")?)?,
            foliage_placer(name, field("foliage_placer")?)?,
            feature_size(name, field("minimum_size")?)?,
        ) else {
            return Ok(None);
        };
        Ok(Some(CompiledTree {
            trunk_provider,
            trunk_placer,
            foliage_provider,
            foliage_placer,
            minimum_size,
            ignore_vines: config
                .get("ignore_vines")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            below_trunk_provider,
            decorators,
            blocks: self.tree_blocks()?,
        }))
    }
}

fn integer(
    name: &str,
    json: &Value,
    key: &str,
    default: Option<i32>,
) -> Result<i32, WorldgenDataError> {
    match json.get(key) {
        Some(value) => value
            .as_i64()
            .and_then(|value| i32::try_from(value).ok())
            .ok_or_else(|| invalid(name, format!("`{key}` must be an integer"))),
        None => default.ok_or_else(|| invalid(name, format!("missing field `{key}`"))),
    }
}

/// An int provider field; None when its type is not ported yet.
fn int_provider(
    name: &str,
    json: &Value,
    key: &str,
) -> Result<Option<IntProviderDef>, WorldgenDataError> {
    let provider = parse_int_provider(
        name,
        json.get(key)
            .ok_or_else(|| invalid(name, format!("missing field `{key}`")))?,
    )?;
    Ok((!matches!(provider, IntProviderDef::Other(_))).then_some(provider))
}

fn kind<'a>(name: &str, json: &'a Value) -> Result<&'a str, WorldgenDataError> {
    json.get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(name, "a tree part needs a `type`"))
}

fn trunk_placer(name: &str, json: &Value) -> Result<Option<TrunkPlacer>, WorldgenDataError> {
    let height = TrunkHeight {
        base: integer(name, json, "base_height", None)?,
        rand_a: integer(name, json, "height_rand_a", None)?,
        rand_b: integer(name, json, "height_rand_b", None)?,
    };
    Ok(match kind(name, json)? {
        "minecraft:straight_trunk_placer" => Some(TrunkPlacer::Straight(height)),
        "minecraft:fancy_trunk_placer" => Some(TrunkPlacer::Fancy(height)),
        "minecraft:forking_trunk_placer" => Some(TrunkPlacer::Forking(height)),
        "minecraft:giant_trunk_placer" => Some(TrunkPlacer::Giant(height)),
        "minecraft:mega_jungle_trunk_placer" => Some(TrunkPlacer::MegaJungle(height)),
        "minecraft:dark_oak_trunk_placer" => Some(TrunkPlacer::DarkOak(height)),
        "minecraft:cherry_trunk_placer" => {
            let start = json
                .get("branch_start_offset_from_top")
                .and_then(|start| {
                    let bound = |key| start.get(key)?.as_i64()?.try_into().ok();
                    Some((bound("min_inclusive")?, bound("max_inclusive")?))
                })
                .ok_or_else(|| invalid(name, "`branch_start_offset_from_top` needs its bounds"))?;
            let (Some(count), Some(horizontal_length), Some(end)) = (
                int_provider(name, json, "branch_count")?,
                int_provider(name, json, "branch_horizontal_length")?,
                int_provider(name, json, "branch_end_offset_from_top")?,
            ) else {
                return Ok(None);
            };
            Some(TrunkPlacer::Cherry(
                height,
                Box::new(CherryBranches {
                    count,
                    horizontal_length,
                    start,
                    end,
                }),
            ))
        }
        _ => None,
    })
}

fn foliage_placer(name: &str, json: &Value) -> Result<Option<FoliagePlacer>, WorldgenDataError> {
    let foliage_kind = match kind(name, json)? {
        "minecraft:blob_foliage_placer" => FoliageKind::Blob(integer(name, json, "height", None)?),
        "minecraft:acacia_foliage_placer" => FoliageKind::Acacia,
        "minecraft:dark_oak_foliage_placer" => FoliageKind::DarkOak,
        "minecraft:cherry_foliage_placer" => {
            let chance = |key: &str| {
                json.get(key)
                    .and_then(Value::as_f64)
                    .map(|chance| chance as f32)
                    .ok_or_else(|| invalid(name, format!("missing field `{key}`")))
            };
            let Some(height) = int_provider(name, json, "height")? else {
                return Ok(None);
            };
            FoliageKind::Cherry(Box::new(CherryFoliage {
                height,
                wide_bottom_layer_hole: chance("wide_bottom_layer_hole_chance")?,
                corner_hole: chance("corner_hole_chance")?,
                hanging_leaves: chance("hanging_leaves_chance")?,
                hanging_leaves_extension: chance("hanging_leaves_extension_chance")?,
            }))
        }
        "minecraft:bush_foliage_placer" => FoliageKind::Bush(integer(name, json, "height", None)?),
        "minecraft:jungle_foliage_placer" => {
            FoliageKind::Jungle(integer(name, json, "height", None)?)
        }
        "minecraft:fancy_foliage_placer" => {
            FoliageKind::Fancy(integer(name, json, "height", None)?)
        }
        "minecraft:spruce_foliage_placer" => match int_provider(name, json, "trunk_height")? {
            Some(trunk) => FoliageKind::Spruce(trunk),
            None => return Ok(None),
        },
        "minecraft:mega_pine_foliage_placer" => match int_provider(name, json, "crown_height")? {
            Some(height) => FoliageKind::MegaPine(height),
            None => return Ok(None),
        },
        "minecraft:pine_foliage_placer" => match int_provider(name, json, "height")? {
            Some(height) => FoliageKind::Pine(height),
            None => return Ok(None),
        },
        _ => return Ok(None),
    };
    let (Some(radius), Some(offset)) = (
        int_provider(name, json, "radius")?,
        int_provider(name, json, "offset")?,
    ) else {
        return Ok(None);
    };
    Ok(Some(FoliagePlacer {
        radius,
        offset,
        kind: foliage_kind,
    }))
}

fn feature_size(name: &str, json: &Value) -> Result<Option<FeatureSize>, WorldgenDataError> {
    let size = match kind(name, json)? {
        "minecraft:two_layers_feature_size" => SizeKind::TwoLayers {
            limit: integer(name, json, "limit", Some(1))?,
            lower: integer(name, json, "lower_size", Some(0))?,
            upper: integer(name, json, "upper_size", Some(1))?,
        },
        "minecraft:three_layers_feature_size" => SizeKind::ThreeLayers {
            limit: integer(name, json, "limit", Some(1))?,
            upper_limit: integer(name, json, "upper_limit", Some(1))?,
            lower: integer(name, json, "lower_size", Some(0))?,
            middle: integer(name, json, "middle_size", Some(1))?,
            upper: integer(name, json, "upper_size", Some(1))?,
        },
        _ => return Ok(None),
    };
    let min_clipped_height = match json.get("min_clipped_height") {
        Some(_) => Some(integer(name, json, "min_clipped_height", None)?),
        None => None,
    };
    Ok(Some(FeatureSize {
        kind: size,
        min_clipped_height,
    }))
}

fn invalid(context: &str, message: impl Into<String>) -> WorldgenDataError {
    WorldgenDataError::InvalidDefinition {
        context: context.to_string(),
        message: message.into(),
    }
}
