//! Shape updates (`BlockState.updateShape`) for the blocks that features change next to the blocks they place.
//!
//! A tree runs `StructureTemplate.updateShapeAtEdge` once its blocks are in place: every face between a block of
//! the tree and a block outside it updates both blocks against each other. Most blocks keep their state. The
//! ones ported here change: grass blocks, podzol and mycelium follow whether snow lies on them
//! (`SnowyBlock`), vegetation and carpets break into air once they cannot survive (`VegetationBlock`,
//! `CarpetBlock`), a spore blossom breaks once the block above no longer holds it, and either half of a double
//! plant breaks without the other (`DoublePlantBlock`), a vine loses the faces nothing holds and breaks
//! without faces (`VineBlock`), cocoa breaks once the block it faces no longer holds it (`CocoaBlock`), and
//! bamboo follows an older stalk above it, with a bamboo shoot becoming a stalk below another stalk.
//! Small dripleaves follow double plants; a big dripleaf breaks without support below or becomes a stem
//! when another big dripleaf grows above it.
//! Blocks that only schedule a tick, such as leaves, liquids, sugar cane or cactus, keep their state.

use std::collections::HashMap;

use harpas_core::block::block_states;
use harpas_core::block_behavior::{Face, block_behavior};

use crate::data::{WorldgenData, WorldgenDataError};
use crate::feature::FeatureWorld;
use crate::placement::StateSet;
use crate::survival::{SurvivalRule, compile_block_survival};

/// One of the six directions (`Direction`), in vanilla's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// -Y.
    Down,
    /// +Y.
    Up,
    /// -Z.
    North,
    /// +Z.
    South,
    /// -X.
    West,
    /// +X.
    East,
}

impl Direction {
    /// Every direction in `Direction.values()` order.
    pub const ALL: [Self; 6] = [
        Self::Down,
        Self::Up,
        Self::North,
        Self::South,
        Self::West,
        Self::East,
    ];

    /// The offset of one step in this direction.
    pub const fn offset(self) -> [i32; 3] {
        match self {
            Self::Down => [0, -1, 0],
            Self::Up => [0, 1, 0],
            Self::North => [0, 0, -1],
            Self::South => [0, 0, 1],
            Self::West => [-1, 0, 0],
            Self::East => [1, 0, 0],
        }
    }

    /// The opposite direction.
    pub const fn opposite(self) -> Self {
        match self {
            Self::Down => Self::Up,
            Self::Up => Self::Down,
            Self::North => Self::South,
            Self::South => Self::North,
            Self::West => Self::East,
            Self::East => Self::West,
        }
    }

    /// The direction with a name, as `Direction.CODEC` reads it.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|direction| direction.name() == name)
    }

    /// The direction's name, as a `facing` property spells it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Down => "down",
            Self::Up => "up",
            Self::North => "north",
            Self::South => "south",
            Self::West => "west",
            Self::East => "east",
        }
    }

    /// The position one step from `position`.
    pub const fn step(self, [x, y, z]: [i32; 3]) -> [i32; 3] {
        let [dx, dy, dz] = self.offset();
        [x + dx, y + dy, z + dz]
    }
}

/// How a block reacts to a neighbor.
#[derive(Debug)]
enum Update {
    /// `SnowyBlock`: `snowy` follows whether the block above is in `#snow`.
    Snowy,
    /// `VegetationBlock` and `CarpetBlock`: air once the block cannot survive.
    Survive(SurvivalRule),
    /// `SporeBlossomBlock`: air once it cannot survive, checked when the block above changes.
    SurviveFromAbove(SurvivalRule),
    /// `DoublePlantBlock`.
    DoublePlant(SurvivalRule),
    /// `BigDripleafBlock`: breaks without support below, or becomes a stem below another leaf.
    BigDripleaf(SurvivalRule),
    /// `VineBlock`: each face keeps only while something supports it, and the vine is air without faces.
    Vine,
    /// `CocoaBlock`: air once the block it faces is not in `#supports_cocoa`, checked when that block changes.
    Cocoa(StateSet),
    /// `MossyCarpetBlock`: its sides follow what holds them, and it is air once it cannot survive or has none.
    MossyCarpet,
    /// `HangingMossBlock`: a tip unless more hanging moss is below; it only schedules a tick when unsupported.
    HangingMoss,
    /// `BambooStalkBlock`: age follows an older stalk above; loss of support only schedules a tick.
    Bamboo,
    /// `BambooSaplingBlock`: breaks without support below, or becomes a stalk below another stalk.
    BambooSapling(SurvivalRule),
}

/// The shape updates of the blocks they are ported for.
#[derive(Debug)]
pub struct ShapeUpdates {
    /// For every block state, an index into `updates`, or `u16::MAX` for blocks that keep their state.
    kinds: Vec<u16>,
    updates: Vec<Update>,
    snow: StateSet,
    air: u32,
}

/// Blocks whose `snowy` property follows the block above.
const SNOWY_BLOCKS: [&str; 3] = [
    "minecraft:grass_block",
    "minecraft:podzol",
    "minecraft:mycelium",
];

impl ShapeUpdates {
    /// Compiles the shape updates from a data pack's tags.
    pub fn new(data: &WorldgenData) -> Result<Self, WorldgenDataError> {
        let states = block_states();
        let mut kinds = vec![u16::MAX; states.len() as usize];
        let mut updates = Vec::new();
        let mut by_block: HashMap<String, Option<u16>> = HashMap::new();
        for id in 0..states.len() {
            let Some((name, _)) = states.properties(id) else {
                continue;
            };
            let kind = match by_block.get(name) {
                Some(kind) => *kind,
                None => {
                    let update = Self::update_of(name, data)?;
                    let kind = update.map(|update| {
                        updates.push(update);
                        (updates.len() - 1) as u16
                    });
                    by_block.insert(name.to_string(), kind);
                    kind
                }
            };
            if let Some(kind) = kind {
                kinds[id as usize] = kind;
            }
        }
        Ok(Self {
            kinds,
            updates,
            snow: StateSet::from_membership(&data.block_tag_states("minecraft:snow")?),
            air: states.default_state("minecraft:air").unwrap_or(0),
        })
    }

    fn update_of(block: &str, data: &WorldgenData) -> Result<Option<Update>, WorldgenDataError> {
        if SNOWY_BLOCKS.contains(&block) {
            return Ok(Some(Update::Snowy));
        }
        let short = block.strip_prefix("minecraft:").unwrap_or(block);
        if matches!(short, "sugar_cane" | "cactus" | "melon" | "pumpkin") {
            return Ok(None);
        }
        if short == "vine" {
            return Ok(Some(Update::Vine));
        }
        if short == "pale_moss_carpet" {
            return Ok(Some(Update::MossyCarpet));
        }
        if short == "pale_hanging_moss" {
            return Ok(Some(Update::HangingMoss));
        }
        if short == "bamboo" {
            return Ok(Some(Update::Bamboo));
        }
        if short == "big_dripleaf_stem" {
            return Ok(None);
        }
        if short == "cocoa" {
            let support = data.block_tag_states("minecraft:supports_cocoa")?;
            return Ok(Some(Update::Cocoa(StateSet::from_membership(&support))));
        }
        let Some(rule) = compile_block_survival("shape updates", block, data)? else {
            return Ok(None);
        };
        Ok(Some(match (short, rule) {
            ("spore_blossom", rule) => Update::SurviveFromAbove(rule),
            ("bamboo_sapling", rule) => Update::BambooSapling(rule),
            ("big_dripleaf", rule) => Update::BigDripleaf(rule),
            (_, rule @ SurvivalRule::DoublePlant(_)) => Update::DoublePlant(rule),
            (_, rule @ SurvivalRule::SmallDripleaf { .. }) => Update::DoublePlant(rule),
            (_, rule) => Update::Survive(rule),
        }))
    }

    /// `updateShape`: the state of the block at `position` once its neighbor in `direction` is
    /// `neighbor`.
    pub fn update(
        &self,
        world: &impl FeatureWorld,
        state: u32,
        position: [i32; 3],
        direction: Direction,
        neighbor: u32,
    ) -> u32 {
        let Some(update) = self
            .kinds
            .get(state as usize)
            .and_then(|&kind| self.updates.get(kind as usize))
        else {
            return state;
        };
        match update {
            Update::Snowy => {
                if direction != Direction::Up {
                    return state;
                }
                let snowy = if self.snow.contains(neighbor) {
                    "true"
                } else {
                    "false"
                };
                with_property(state, "snowy", snowy)
            }
            Update::Survive(rule) => {
                if rule.test(world, position, state) {
                    state
                } else {
                    self.air
                }
            }
            Update::SurviveFromAbove(rule) => {
                if direction == Direction::Up && !rule.test(world, position, state) {
                    self.air
                } else {
                    state
                }
            }
            Update::DoublePlant(rule) => {
                let half = property(state, "half");
                let lower = half == Some("lower");
                if matches!(direction, Direction::Up | Direction::Down)
                    && lower == (direction == Direction::Up)
                    && (!same_block(neighbor, state) || property(neighbor, "half") == half)
                {
                    return self.air;
                }
                // The lower half's own check below and `VegetationBlock`'s both ask whether it survives.
                if rule.test(world, position, state) {
                    state
                } else {
                    self.air
                }
            }
            Update::BigDripleaf(rule) => {
                if direction == Direction::Down && !rule.test(world, position, state) {
                    return self.air;
                }
                if direction == Direction::Up && same_block(state, neighbor) {
                    let stem = block_states()
                        .default_state("minecraft:big_dripleaf_stem")
                        .unwrap_or(state);
                    let stem =
                        with_property(stem, "facing", property(state, "facing").unwrap_or("north"));
                    with_property(
                        stem,
                        "waterlogged",
                        property(state, "waterlogged").unwrap_or("false"),
                    )
                } else {
                    state
                }
            }
            Update::Vine => {
                if direction == Direction::Down {
                    return state;
                }
                let updated = vine_state(world, state, position);
                let faces = ["up", "north", "east", "south", "west"];
                if faces
                    .iter()
                    .any(|face| property(updated, face) == Some("true"))
                {
                    updated
                } else {
                    self.air
                }
            }
            Update::MossyCarpet => {
                crate::mossy_carpet::update_shape(world, state, position, self.air)
            }
            Update::HangingMoss => {
                let [x, y, z] = position;
                let tip = !same_block(world.block_state(x, y - 1, z), state);
                with_property(state, "tip", if tip { "true" } else { "false" })
            }
            Update::Bamboo => {
                if direction == Direction::Up
                    && same_block(state, neighbor)
                    && property(state, "age") == Some("0")
                    && property(neighbor, "age") == Some("1")
                {
                    with_property(state, "age", "1")
                } else {
                    state
                }
            }
            Update::BambooSapling(rule) => {
                if !rule.test(world, position, state) {
                    self.air
                } else if direction == Direction::Up
                    && block_states()
                        .properties(neighbor)
                        .is_some_and(|(name, _)| name == "minecraft:bamboo")
                {
                    block_states()
                        .default_state("minecraft:bamboo")
                        .unwrap_or(state)
                } else {
                    state
                }
            }
            Update::Cocoa(support) => {
                let facing = property(state, "facing");
                if Some(direction.name()) != facing {
                    return state;
                }
                let [x, y, z] = direction.step(position);
                if support.contains(world.block_state(x, y, z)) {
                    state
                } else {
                    self.air
                }
            }
        }
    }
}

/// `VineBlock.getUpdatedState`: the top face keeps while the block above can be attached to, as
/// `isAcceptableNeighbour` asks it with `DOWN`; each side face while the block on that side can be attached
/// to, or the vine above has the same face.
fn vine_state(world: &impl FeatureWorld, mut state: u32, position: [i32; 3]) -> u32 {
    let above = Direction::Up.step(position);
    let flag = |value: bool| if value { "true" } else { "false" };
    if property(state, "up") == Some("true") {
        state = with_property(
            state,
            "up",
            flag(can_attach_to(world, above, Direction::Down)),
        );
    }
    let above_state = world.block_state(above[0], above[1], above[2]);
    for direction in [
        Direction::North,
        Direction::East,
        Direction::South,
        Direction::West,
    ] {
        let face = direction.name();
        if property(state, face) != Some("true") {
            continue;
        }
        let supported = can_attach_to(world, direction.step(position), direction)
            || (same_block(above_state, state) && property(above_state, face) == Some("true"));
        state = with_property(state, face, flag(supported));
    }
    state
}

/// `MultifaceBlock.canAttachTo`: whether the block at `neighbor`, reached in `direction`, has a full support or
/// collision face towards the opposite direction.
pub(crate) fn can_attach_to(
    world: &impl FeatureWorld,
    neighbor: [i32; 3],
    direction: Direction,
) -> bool {
    let state = world.block_state(neighbor[0], neighbor[1], neighbor[2]);
    let face = match direction.opposite() {
        Direction::Down => Face::Down,
        Direction::Up => Face::Up,
        Direction::North => Face::North,
        Direction::South => Face::South,
        Direction::West => Face::West,
        Direction::East => Face::East,
    };
    let behavior = block_behavior();
    behavior.is_face_sturdy(state, face) || behavior.is_collision_face_full(state, face)
}

/// A property value of a block state.
pub(crate) fn property(state: u32, key: &str) -> Option<&'static str> {
    let (_, properties) = block_states().properties(state)?;
    properties
        .into_iter()
        .find(|(property, _)| *property == key)
        .map(|(_, value)| value)
}

/// The state with one property set; unchanged when the block does not have the property or the value.
pub(crate) fn with_property(state: u32, key: &str, value: &str) -> u32 {
    let Some((name, properties)) = block_states().properties(state) else {
        return state;
    };
    let properties = properties
        .into_iter()
        .map(|(property, current)| (property, if property == key { value } else { current }));
    block_states()
        .state(name, properties)
        .map_or(state, |(id, _)| id)
}

/// Whether two block states belong to the same block.
pub(crate) fn same_block(a: u32, b: u32) -> bool {
    let name = |state| block_states().properties(state).map(|(name, _)| name);
    name(a).is_some() && name(a) == name(b)
}

/// `StructureTemplate.updateShapeAtEdge`: updates both blocks on each side of every face between a filled and
/// an empty cell of `shape`, the cells taken from `min`. Faces normal to Z come first, then Y, then X, as
/// `DiscreteVoxelShape.forAllFaces` visits them.
pub fn update_shape_at_edge(
    world: &mut impl FeatureWorld,
    updates: &ShapeUpdates,
    shape: &VoxelShape,
    min: [i32; 3],
) {
    let [sx, sy, sz] = shape.size;
    let face = |world: &mut _, direction: Direction, [x, y, z]: [i32; 3]| {
        update_face(
            world,
            updates,
            direction,
            [min[0] + x, min[1] + y, min[2] + z],
        );
    };
    for x in 0..sx {
        for y in 0..sy {
            shape.axis_faces(
                sz,
                |z| [x, y, z],
                Direction::North,
                Direction::South,
                |d, p| face(world, d, p),
            );
        }
    }
    for z in 0..sz {
        for x in 0..sx {
            shape.axis_faces(
                sy,
                |y| [x, y, z],
                Direction::Down,
                Direction::Up,
                |d, p| face(world, d, p),
            );
        }
    }
    for y in 0..sy {
        for z in 0..sz {
            shape.axis_faces(
                sx,
                |x| [x, y, z],
                Direction::West,
                Direction::East,
                |d, p| face(world, d, p),
            );
        }
    }
}

/// The lambda of `updateShapeAtEdge`: the block updates against its neighbor, then the neighbor against the
/// block's new state. Vanilla writes changed blocks with flag 2, which can also mark them for postprocessing;
/// those marks are not kept yet.
fn update_face(
    world: &mut impl FeatureWorld,
    updates: &ShapeUpdates,
    direction: Direction,
    position: [i32; 3],
) {
    let neighbor = direction.step(position);
    let state = world.block_state(position[0], position[1], position[2]);
    let neighbor_state = world.block_state(neighbor[0], neighbor[1], neighbor[2]);
    let updated = updates.update(world, state, position, direction, neighbor_state);
    if updated != state {
        world.set_block_state(position[0], position[1], position[2], updated);
    }
    let neighbor_updated = updates.update(
        world,
        neighbor_state,
        neighbor,
        direction.opposite(),
        updated,
    );
    if neighbor_updated != neighbor_state {
        world.set_block_state(neighbor[0], neighbor[1], neighbor[2], neighbor_updated);
    }
}

/// A box of cells, each full or empty (`BitSetDiscreteVoxelShape`).
#[derive(Debug, Clone)]
pub struct VoxelShape {
    size: [i32; 3],
    cells: Vec<bool>,
}

impl VoxelShape {
    /// An empty shape of `size` cells.
    pub fn new(size: [i32; 3]) -> Self {
        Self {
            size,
            cells: vec![false; (size[0] * size[1] * size[2]).max(0) as usize],
        }
    }

    fn index(&self, [x, y, z]: [i32; 3]) -> usize {
        ((x * self.size[1] + y) * self.size[2] + z) as usize
    }

    /// Fills a cell.
    pub fn fill(&mut self, cell: [i32; 3]) {
        let index = self.index(cell);
        self.cells[index] = true;
    }

    /// Whether a cell is full.
    pub fn is_full(&self, cell: [i32; 3]) -> bool {
        self.cells[self.index(cell)]
    }

    /// One line of `forAllAxisFaces`: a face in the negative direction where an empty cell, or the start, is
    /// followed by a full one, and in the positive direction where a full cell is followed by an empty one or
    /// the end.
    fn axis_faces(
        &self,
        length: i32,
        cell: impl Fn(i32) -> [i32; 3],
        negative: Direction,
        positive: Direction,
        mut consume: impl FnMut(Direction, [i32; 3]),
    ) {
        let mut previous = false;
        for k in 0..=length {
            let full = k != length && self.is_full(cell(k));
            if !previous && full {
                consume(negative, cell(k));
            }
            if previous && !full {
                consume(positive, cell(k - 1));
            }
            previous = full;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::RegionFeatureWorld;
    use crate::generator::GeneratedChunk;

    fn state(name: &str, properties: &[(&str, &str)]) -> u32 {
        block_states()
            .state(name, properties.iter().copied())
            .unwrap()
            .0
    }

    fn world(blocks: &[([i32; 3], u32)]) -> RegionFeatureWorld {
        let mut chunk = GeneratedChunk::new(0, 0, -64, 384, 1);
        for &([x, y, z], state) in blocks {
            chunk.set_block_state(x as usize, y, z as usize, state);
        }
        RegionFeatureWorld::new([chunk], 0).unwrap()
    }

    #[test]
    fn vines_keep_the_faces_something_holds() {
        let east = state("minecraft:vine", &[("east", "true")]);
        let leaves = state("minecraft:oak_leaves", &[]);
        // Leaves have no support shape but a full collision shape, which holds a vine.
        let held = world(&[([5, 70, 5], east), ([6, 70, 5], leaves)]);
        assert_eq!(vine_state(&held, east, [5, 70, 5]), east);
        // A vine below another with the same face hangs from it.
        let hanging = world(&[([5, 70, 5], east), ([5, 71, 5], east)]);
        assert_eq!(vine_state(&hanging, east, [5, 70, 5]), east);
        let alone = world(&[([5, 70, 5], east)]);
        let bare = vine_state(&alone, east, [5, 70, 5]);
        assert_eq!(property(bare, "east"), Some("false"));
    }
}
