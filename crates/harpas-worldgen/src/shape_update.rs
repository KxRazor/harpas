//! Shape updates (`BlockState.updateShape`) for the blocks that features change next to the blocks they place.
//!
//! A tree runs `StructureTemplate.updateShapeAtEdge` once its blocks are in place: every face between a block of
//! the tree and a block outside it updates both blocks against each other. Most blocks keep their state. The
//! ones ported here change: grass blocks, podzol and mycelium follow whether snow lies on them
//! (`SnowyBlock`), vegetation and carpets break into air once they cannot survive (`VegetationBlock`,
//! `CarpetBlock`), a spore blossom breaks once the block above no longer holds it, and either half of a double
//! plant breaks without the other (`DoublePlantBlock`). Blocks that only schedule a tick, such as leaves,
//! liquids, sugar cane or cactus, keep their state.

use std::collections::HashMap;

use harpas_core::block::block_states;

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
        let Some(rule) = compile_block_survival("shape updates", block, data)? else {
            return Ok(None);
        };
        Ok(Some(match (short, rule) {
            ("spore_blossom", rule) => Update::SurviveFromAbove(rule),
            (_, rule @ SurvivalRule::DoublePlant(_)) => Update::DoublePlant(rule),
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
        }
    }
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
fn same_block(a: u32, b: u32) -> bool {
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
