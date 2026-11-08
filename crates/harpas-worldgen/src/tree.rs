//! `minecraft:tree` (`TreeFeature`) with straight and fancy trunks and blob, fancy, spruce and pine foliage.
//!
//! A tree draws its height, its foliage height and radius, checks that the column is inside the world and,
//! shrinking the height when blocks are in the way (`getMaxFreeTreeHeight`), free of blocks a tree may not
//! replace; then it places the block below its trunk, the trunk and the foliage at each attachment its trunk
//! placer returns. Every block it places joins one of four position sets, which vanilla keeps in `HashSet`s:
//! roots, trunk, foliage and decorations. `updateLeaves` then gives every leaf in the tree's bounds its distance
//! to the nearest log, walking those sets in Java's iteration order, and `updateShapeAtEdge` updates the
//! blocks on both sides of the tree's surface.
//!
//! Trees with decorators or a root placer, and the other trunk and foliage placers, are not ported yet.

use std::sync::Arc;

use harpas_core::block::block_states;
use harpas_core::math::{floor_f32, floor_f64};
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
    vine: StateSet,
    /// Shape updates for the blocks around a tree.
    updates: ShapeUpdates,
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
        })
    }

    /// `TreeFeature.validTreePos`: air or `#replaceable_by_trees`.
    fn valid(&self, state: u32) -> bool {
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
            FoliageKind::Blob(height) | FoliageKind::Fancy(height) => *height,
            FoliageKind::Spruce(trunk) => 4.max(tree_height - self.sample(trunk)),
            FoliageKind::Pine(height) => self.sample(height),
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

    /// `FoliagePlacer.shouldSkipLocationSigned`, then each placer's `shouldSkipLocation`.
    fn skip_signed(&mut self, dx: i32, y: i32, dz: i32, radius: i32, double_trunk: bool) -> bool {
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

impl TrunkPlacer {
    /// `TrunkPlacer.getTreeHeight`.
    fn tree_height<R: RandomSource>(&self, random: &mut WorldgenRandom<R>) -> i32 {
        let (Self::Straight(height) | Self::Fancy(height)) = self;
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
    let Placer { world, sets, .. } = placer;
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
        let has_decorators = config
            .get("decorators")
            .and_then(Value::as_array)
            .is_some_and(|decorators| !decorators.is_empty());
        if has_decorators || config.get("root_placer").is_some() {
            return Ok(None);
        }
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
        _ => None,
    })
}

fn foliage_placer(name: &str, json: &Value) -> Result<Option<FoliagePlacer>, WorldgenDataError> {
    let foliage_kind = match kind(name, json)? {
        "minecraft:blob_foliage_placer" => FoliageKind::Blob(integer(name, json, "height", None)?),
        "minecraft:fancy_foliage_placer" => {
            FoliageKind::Fancy(integer(name, json, "height", None)?)
        }
        "minecraft:spruce_foliage_placer" => match int_provider(name, json, "trunk_height")? {
            Some(trunk) => FoliageKind::Spruce(trunk),
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
