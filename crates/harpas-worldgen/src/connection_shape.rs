//! Shape updates of the blocks that connect to their neighbors (`BlockState.updateShape`), which structure
//! templates place: fences, panes and bars, walls, fence gates, stairs, doors and chests.
//!
//! A fence connects to a full face that is not an exception (leaves, pumpkins, melons, barriers and shulker
//! boxes), to fences of its kind (wooden or not) and to a fence gate across its path. Panes and bars connect to
//! the same faces, to each other and to walls. A wall connects to walls, those faces, panes and bars and gates;
//! its sides are tall where the block above covers them and low otherwise, and its post rises unless it runs
//! straight between two tall sides, or two sides that meet, with nothing above that needs it. A fence gate is
//! in a wall when a wall stands on either side of its hinge axis. Stairs take their corner shapes from the
//! stairs in front and behind. A door half follows the other half and breaks without it, and its lower half
//! breaks without a full face below. A chest joins a single chest facing the same way beside it, or becomes
//! single once the chest it was joined to is gone.

use harpas_core::block::block_states;
use harpas_core::block_behavior::{Face, block_behavior};

use crate::data::{WorldgenData, WorldgenDataError};
use crate::placement::StateSet;
use crate::shape_update::{Direction, property, with_property};

/// The kinds of connecting blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Connecting {
    /// `FenceBlock`, wooden or not.
    Fence { wooden: bool },
    /// `IronBarsBlock`: panes and bars.
    Bars,
    /// `WallBlock`.
    Wall,
    /// `FenceGateBlock`.
    FenceGate,
    /// `StairBlock`.
    Stairs,
    /// `DoorBlock`.
    Door,
    /// `ChestBlock`.
    Chest,
    /// Blocks that hang on the side of the block they face away from and break without it: ladders and wall
    /// torches, which need a full face, and wall banners, which need a solid block.
    WallAttached { solid: bool },
    /// `TorchBlock`: breaks without the center of a face below.
    Torch,
    /// `FaceAttachedHorizontalDirectionalBlock`, such as levers and buttons: breaks without a full face where
    /// it is attached.
    FaceAttached,
}

/// The block tags connecting blocks read.
#[derive(Debug)]
pub(crate) struct Connections {
    fences: StateSet,
    wooden_fences: StateSet,
    walls: StateSet,
    fence_gates: StateSet,
    /// `Block.isExceptionForConnection`.
    exceptions: StateSet,
    wall_post_override: StateSet,
    air: u32,
}

/// What a wall's side test or post test asks the block above to cover, in 1/32 of a block: (x0, z0, x1, z1).
const POST_TEST: [i32; 4] = [14, 14, 18, 18];

fn side_test(direction: Direction) -> [i32; 4] {
    match direction {
        Direction::North => [14, 0, 18, 18],
        Direction::South => [14, 14, 18, 32],
        Direction::West => [0, 14, 18, 18],
        _ => [14, 14, 32, 18],
    }
}

const fn face(direction: Direction) -> Face {
    match direction {
        Direction::Down => Face::Down,
        Direction::Up => Face::Up,
        Direction::North => Face::North,
        Direction::South => Face::South,
        Direction::West => Face::West,
        Direction::East => Face::East,
    }
}

const fn horizontal(direction: Direction) -> bool {
    matches!(
        direction,
        Direction::North | Direction::South | Direction::West | Direction::East
    )
}

/// Whether two horizontal directions lie on the same axis.
const fn same_axis(a: Direction, b: Direction) -> bool {
    matches!(
        (a, b),
        (
            Direction::North | Direction::South,
            Direction::North | Direction::South
        ) | (
            Direction::West | Direction::East,
            Direction::West | Direction::East
        )
    )
}

const fn counterclockwise(direction: Direction) -> Direction {
    match direction {
        Direction::North => Direction::West,
        Direction::West => Direction::South,
        Direction::South => Direction::East,
        Direction::East => Direction::North,
        other => other,
    }
}

fn facing(state: u32) -> Option<Direction> {
    property(state, "facing").and_then(Direction::from_name)
}

fn name(state: u32) -> &'static str {
    block_states()
        .properties(state)
        .map_or("", |(name, _)| name)
}

/// Whether a block is a pane or bars (`IronBarsBlock`).
fn is_bars(state: u32) -> bool {
    let name = name(state);
    name.ends_with("glass_pane") || name.ends_with("_bars")
}

/// Whether a block is stairs (`StairBlock.isStairs`).
fn is_stairs(state: u32) -> bool {
    name(state).ends_with("_stairs")
}

impl Connecting {
    /// The kind of a block, or None for blocks that do not connect.
    pub(crate) fn of(block: &str, data: &WorldgenData) -> Result<Option<Self>, WorldgenDataError> {
        let short = block.strip_prefix("minecraft:").unwrap_or(block);
        let default = block_states().default_state(block);
        let in_tag = |tag: &str| -> Result<bool, WorldgenDataError> {
            Ok(default.is_some_and(|state| {
                data.block_tag_states(tag)
                    .is_ok_and(|states| states.get(state as usize).copied().unwrap_or(false))
            }))
        };
        Ok(Some(if in_tag("minecraft:fences")? {
            Self::Fence {
                wooden: in_tag("minecraft:wooden_fences")?,
            }
        } else if short.ends_with("glass_pane") || short.ends_with("_bars") {
            Self::Bars
        } else if in_tag("minecraft:walls")? {
            Self::Wall
        } else if in_tag("minecraft:fence_gates")? {
            Self::FenceGate
        } else if short.ends_with("_stairs") {
            Self::Stairs
        } else if short.ends_with("_door") {
            Self::Door
        } else if matches!(short, "chest" | "trapped_chest") || short.ends_with("copper_chest") {
            Self::Chest
        } else if short == "ladder" || short.ends_with("wall_torch") {
            Self::WallAttached { solid: false }
        } else if short.ends_with("_wall_banner") {
            Self::WallAttached { solid: true }
        } else if matches!(
            short,
            "torch" | "soul_torch" | "copper_torch" | "redstone_torch"
        ) {
            Self::Torch
        } else if short == "lever" || short.ends_with("_button") {
            Self::FaceAttached
        } else {
            return Ok(None);
        }))
    }
}

impl Connections {
    /// Reads the tags from a data pack.
    pub(crate) fn new(data: &WorldgenData) -> Result<Self, WorldgenDataError> {
        let tag = |tag: &str| -> Result<StateSet, WorldgenDataError> {
            Ok(StateSet::from_membership(&data.block_tag_states(tag)?))
        };
        let states = block_states();
        let mut exceptions = data.block_tag_states("minecraft:leaves")?;
        let shulker_boxes = data.block_tag_states("minecraft:shulker_boxes")?;
        for (id, member) in shulker_boxes.iter().enumerate() {
            exceptions[id] |= *member;
        }
        for id in 0..states.len() {
            if states.properties(id).is_some_and(|(name, _)| {
                matches!(
                    name,
                    "minecraft:barrier"
                        | "minecraft:carved_pumpkin"
                        | "minecraft:jack_o_lantern"
                        | "minecraft:melon"
                        | "minecraft:pumpkin"
                )
            }) {
                exceptions[id as usize] = true;
            }
        }
        Ok(Self {
            fences: tag("minecraft:fences")?,
            wooden_fences: tag("minecraft:wooden_fences")?,
            walls: tag("minecraft:walls")?,
            fence_gates: tag("minecraft:fence_gates")?,
            exceptions: StateSet::from_membership(&exceptions),
            wall_post_override: tag("minecraft:wall_post_override")?,
            air: states.default_state("minecraft:air").unwrap_or(0),
        })
    }

    /// Whether a block in `direction` from another has a full face towards it, and is not an exception.
    fn sturdy_face(&self, neighbor: u32, direction: Direction) -> bool {
        !self.exceptions.contains(neighbor)
            && block_behavior().is_face_sturdy(neighbor, face(direction.opposite()))
    }

    /// `FenceGateBlock.connectsToDirection`: a gate connects across its path, perpendicular to its facing.
    fn gate_across(&self, neighbor: u32, direction: Direction) -> bool {
        self.fence_gates.contains(neighbor)
            && facing(neighbor).is_some_and(|f| same_axis(f, direction.clockwise()))
    }

    /// `updateShape` of a connecting block, whose neighbor in `direction` is now `neighbor`; `read` reads the
    /// world, where `neighbor` already stands.
    pub(crate) fn update(
        &self,
        kind: Connecting,
        state: u32,
        direction: Direction,
        neighbor: u32,
        read: &impl Fn(Direction) -> u32,
    ) -> u32 {
        let flag = |value: bool| if value { "true" } else { "false" };
        match kind {
            Connecting::Fence { wooden } => {
                if !horizontal(direction) {
                    return state;
                }
                let same_kind = self.fences.contains(neighbor)
                    && self.wooden_fences.contains(neighbor) == wooden;
                let connects = self.sturdy_face(neighbor, direction)
                    || same_kind
                    || self.gate_across(neighbor, direction);
                with_property(state, direction.name(), flag(connects))
            }
            Connecting::Bars => {
                if !horizontal(direction) {
                    return state;
                }
                let connects = self.sturdy_face(neighbor, direction)
                    || is_bars(neighbor)
                    || self.walls.contains(neighbor);
                with_property(state, direction.name(), flag(connects))
            }
            Connecting::Wall => self.wall(state, direction, neighbor, read),
            Connecting::FenceGate => {
                let Some(gate_facing) = facing(state) else {
                    return state;
                };
                if !horizontal(direction) || !same_axis(gate_facing.clockwise(), direction) {
                    return state;
                }
                let in_wall = self.walls.contains(neighbor)
                    || self.walls.contains(read(direction.opposite()));
                with_property(state, "in_wall", flag(in_wall))
            }
            Connecting::Stairs => {
                if !horizontal(direction) {
                    return state;
                }
                with_property(state, "shape", stairs_shape(state, read))
            }
            Connecting::Door => {
                let half = property(state, "half");
                let lower = half == Some("lower");
                if matches!(direction, Direction::Up | Direction::Down)
                    && lower == (direction == Direction::Up)
                {
                    let other_half =
                        name(neighbor).ends_with("_door") && property(neighbor, "half") != half;
                    return if other_half {
                        with_property(neighbor, "half", half.unwrap_or("lower"))
                    } else {
                        self.air
                    };
                }
                if lower
                    && direction == Direction::Down
                    && !block_behavior().is_face_sturdy(neighbor, Face::Up)
                {
                    return self.air;
                }
                state
            }
            Connecting::Chest => chest_shape(state, direction, neighbor),
            Connecting::WallAttached { solid } => {
                let Some(facing) = facing(state) else {
                    return state;
                };
                if direction != facing.opposite() {
                    return state;
                }
                let behavior = block_behavior();
                let holds = if solid {
                    behavior.is_solid(neighbor)
                } else {
                    behavior.is_face_sturdy(neighbor, face(facing))
                };
                if holds { state } else { self.air }
            }
            Connecting::Torch => {
                if direction == Direction::Down
                    && !block_behavior().is_face_center_sturdy(neighbor, Face::Up)
                {
                    self.air
                } else {
                    state
                }
            }
            Connecting::FaceAttached => {
                // The direction from the block to what holds it.
                let support = match property(state, "face") {
                    Some("ceiling") => Some(Direction::Up),
                    Some("floor") => Some(Direction::Down),
                    _ => facing(state).map(Direction::opposite),
                };
                if support != Some(direction) {
                    return state;
                }
                if block_behavior().is_face_sturdy(neighbor, face(direction.opposite())) {
                    state
                } else {
                    self.air
                }
            }
        }
    }

    /// `WallBlock.updateShape`: the side towards a changed neighbor reconnects, and every side and the post
    /// follow the block above.
    fn wall(
        &self,
        state: u32,
        direction: Direction,
        neighbor: u32,
        read: &impl Fn(Direction) -> u32,
    ) -> u32 {
        if direction == Direction::Down {
            return state;
        }
        let sides = [
            Direction::North,
            Direction::East,
            Direction::South,
            Direction::West,
        ];
        let connected = sides.map(|side| {
            if side == direction {
                self.walls.contains(neighbor)
                    || self.sturdy_face(neighbor, direction)
                    || is_bars(neighbor)
                    || self.gate_across(neighbor, direction.opposite())
            } else {
                property(state, side.name()).is_some_and(|value| value != "none")
            }
        });
        let above = if direction == Direction::Up {
            neighbor
        } else {
            read(Direction::Up)
        };
        let covered = bottom_face(above);
        let mut updated = state;
        let mut values = ["none"; 4];
        for (i, side) in sides.into_iter().enumerate() {
            values[i] = match (connected[i], covers(&covered, side_test(side))) {
                (false, _) => "none",
                (true, true) => "tall",
                (true, false) => "low",
            };
            updated = with_property(updated, side.name(), values[i]);
        }
        let none = values.map(|value| value == "none");
        let [north, east, south, west] = none;
        // The post rises under a block that needs it or a wall post, and where the sides do not run straight.
        let raise = if self.wall_post_override.contains(above)
            || (self.walls.contains(above) && property(above, "up") == Some("true"))
            || (north && east && south && west)
            || north != south
            || west != east
        {
            true
        } else if (values[0] == "tall" && values[2] == "tall")
            || (values[1] == "tall" && values[3] == "tall")
        {
            false
        } else {
            covers(&covered, POST_TEST)
        };
        with_property(updated, "up", if raise { "true" } else { "false" })
    }
}

/// A chest's shape after its neighbor in `direction` changes (`ChestBlock.updateShape`).
pub fn chest_shape(state: u32, direction: Direction, neighbor: u32) -> u32 {
    if !horizontal(direction) {
        state
    } else if name(neighbor) == name(state) {
        let neighbor_type = property(neighbor, "type");
        if property(state, "type") == Some("single")
            && neighbor_type != Some("single")
            && facing(state) == facing(neighbor)
            && connected_chest_direction(neighbor) == Some(direction.opposite())
        {
            let opposite = if neighbor_type == Some("left") {
                "right"
            } else {
                "left"
            };
            return with_property(state, "type", opposite);
        }
        state
    } else if connected_chest_direction(state) == Some(direction) {
        with_property(state, "type", "single")
    } else {
        state
    }
}

fn connected_chest_direction(state: u32) -> Option<Direction> {
    let facing = facing(state)?;
    Some(if property(state, "type") == Some("left") {
        facing.clockwise()
    } else {
        counterclockwise(facing)
    })
}

/// `StairBlock.getStairsShape`.
fn stairs_shape(state: u32, read: &impl Fn(Direction) -> u32) -> &'static str {
    let Some(stair_facing) = facing(state) else {
        return "straight";
    };
    let half = property(state, "half");
    let can_take_shape = |direction: Direction| {
        let other = read(direction);
        !is_stairs(other) || facing(other) != Some(stair_facing) || property(other, "half") != half
    };
    let front = read(stair_facing);
    if is_stairs(front) && property(front, "half") == half {
        let front_facing = facing(front).unwrap_or(stair_facing);
        if !same_axis(front_facing, stair_facing) && can_take_shape(front_facing.opposite()) {
            return if front_facing == counterclockwise(stair_facing) {
                "outer_left"
            } else {
                "outer_right"
            };
        }
    }
    let back = read(stair_facing.opposite());
    if is_stairs(back) && property(back, "half") == half {
        let back_facing = facing(back).unwrap_or(stair_facing);
        if !same_axis(back_facing, stair_facing) && can_take_shape(back_facing) {
            return if back_facing == counterclockwise(stair_facing) {
                "inner_left"
            } else {
                "inner_right"
            };
        }
    }
    "straight"
}

/// The cells of a block's collision shape on its bottom face (`getFaceShape(DOWN)`), in a 32 by 32 grid of
/// x and z.
fn bottom_face(state: u32) -> Vec<bool> {
    let mut cells = vec![false; 32 * 32];
    for shape in block_behavior().collision_shape(state) {
        if shape.min[1] > 0 || shape.max[1] <= 0 {
            continue;
        }
        let range = |a: i8, b: i8| i32::from(a).clamp(0, 32)..i32::from(b).clamp(0, 32);
        for x in range(shape.min[0], shape.max[0]) {
            for z in range(shape.min[2], shape.max[2]) {
                cells[(x * 32 + z) as usize] = true;
            }
        }
    }
    cells
}

/// Whether the bottom face covers a test rectangle (`WallBlock.isCovered`).
fn covers(cells: &[bool], [x0, z0, x1, z1]: [i32; 4]) -> bool {
    (x0..x1).all(|x| (z0..z1).all(|z| cells[(x * 32 + z) as usize]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{BlockTagDef, BlockTagEntry};

    fn state(name: &str, properties: &[(&str, &str)]) -> u32 {
        block_states()
            .state(name, properties.iter().copied())
            .unwrap()
            .0
    }

    /// Data with the tags connecting blocks read, holding only the blocks these tests use.
    fn connections() -> Connections {
        let mut data = WorldgenData::default();
        let tags: [(&str, &[&str]); 7] = [
            (
                "fences",
                &["minecraft:oak_fence", "minecraft:nether_brick_fence"],
            ),
            ("wooden_fences", &["minecraft:oak_fence"]),
            ("walls", &["minecraft:cobblestone_wall"]),
            ("fence_gates", &["minecraft:oak_fence_gate"]),
            ("leaves", &["minecraft:oak_leaves"]),
            ("shulker_boxes", &["minecraft:shulker_box"]),
            ("wall_post_override", &["minecraft:torch"]),
        ];
        for (tag, blocks) in tags {
            data.block_tags.insert(
                format!("minecraft:{tag}"),
                BlockTagDef {
                    replace: false,
                    values: blocks
                        .iter()
                        .map(|id| BlockTagEntry {
                            id: (*id).to_string(),
                            required: true,
                        })
                        .collect(),
                },
            );
        }
        Connections::new(&data).unwrap()
    }

    fn air(_: Direction) -> u32 {
        0
    }

    #[test]
    fn fences_connect_to_full_faces_their_kind_and_gates_across_their_path() {
        let c = connections();
        let fence = state("minecraft:oak_fence", &[]);
        let wooden = Connecting::Fence { wooden: true };
        let update = |neighbor| c.update(wooden, fence, Direction::East, neighbor, &air);
        let east = |state| property(state, "east");
        assert_eq!(east(update(state("minecraft:stone", &[]))), Some("true"));
        assert_eq!(east(update(state("minecraft:melon", &[]))), Some("false"));
        assert_eq!(east(update(fence)), Some("true"));
        assert_eq!(
            east(update(state("minecraft:nether_brick_fence", &[]))),
            Some("false")
        );
        let gate = |facing| state("minecraft:oak_fence_gate", &[("facing", facing)]);
        assert_eq!(east(update(gate("north"))), Some("true"));
        assert_eq!(east(update(gate("east"))), Some("false"));
    }

    #[test]
    fn walls_are_tall_under_full_blocks_and_drop_the_post_between_two_tall_sides() {
        let c = connections();
        let wall = state(
            "minecraft:cobblestone_wall",
            &[("north", "low"), ("south", "none")],
        );
        let stone = state("minecraft:stone", &[]);
        let above = |_: Direction| stone;
        // The south side connects to stone; with stone above both sides are tall and the post drops.
        let updated = c.update(Connecting::Wall, wall, Direction::South, stone, &above);
        assert_eq!(property(updated, "north"), Some("tall"));
        assert_eq!(property(updated, "south"), Some("tall"));
        assert_eq!(property(updated, "up"), Some("false"));
        // With air above, the sides are low and the post stays unless they run straight.
        let low = c.update(Connecting::Wall, wall, Direction::South, stone, &air);
        assert_eq!(property(low, "south"), Some("low"));
        assert_eq!(property(low, "up"), Some("false"));
        let alone = c.update(Connecting::Wall, wall, Direction::South, 0, &air);
        assert_eq!(property(alone, "south"), Some("none"));
        assert_eq!(property(alone, "up"), Some("true"));
    }

    #[test]
    fn chests_join_a_matching_single_chest_and_part_when_it_is_gone() {
        let c = connections();
        let chest = |kind| state("minecraft:chest", &[("facing", "north"), ("type", kind)]);
        // A chest facing north whose partner is to the west is the right half; the single chest west of it
        // joins it as the left half, whose partner is to the east.
        let joined = c.update(
            Connecting::Chest,
            chest("single"),
            Direction::East,
            chest("right"),
            &air,
        );
        assert_eq!(property(joined, "type"), Some("left"));
        let parted = c.update(Connecting::Chest, chest("left"), Direction::East, 0, &air);
        assert_eq!(property(parted, "type"), Some("single"));
    }

    #[test]
    fn stairs_take_corner_shapes_and_doors_follow_their_other_half() {
        let c = connections();
        let stairs = |facing| {
            state(
                "minecraft:oak_stairs",
                &[
                    ("facing", facing),
                    ("half", "bottom"),
                    ("shape", "straight"),
                ],
            )
        };
        let east = stairs("east");
        // Stairs facing north in front of stairs facing east make an outer corner.
        let read = |d: Direction| {
            if d == Direction::East {
                stairs("north")
            } else {
                0
            }
        };
        let corner = c.update(
            Connecting::Stairs,
            east,
            Direction::East,
            stairs("north"),
            &read,
        );
        assert_eq!(property(corner, "shape"), Some("outer_left"));
        let door = |half| state("minecraft:oak_door", &[("half", half), ("open", "true")]);
        let lower = state("minecraft:oak_door", &[("half", "lower")]);
        let follows = c.update(Connecting::Door, lower, Direction::Up, door("upper"), &air);
        assert_eq!(property(follows, "open"), Some("true"));
        assert_eq!(c.update(Connecting::Door, lower, Direction::Up, 0, &air), 0);
    }

    #[test]
    fn hanging_blocks_break_without_their_support() {
        let c = connections();
        let ladder = state("minecraft:ladder", &[("facing", "east")]);
        let kind = Connecting::WallAttached { solid: false };
        let stone = state("minecraft:stone", &[]);
        assert_eq!(c.update(kind, ladder, Direction::West, stone, &air), ladder);
        assert_eq!(c.update(kind, ladder, Direction::West, 0, &air), 0);
        // Only the supporting side matters.
        assert_eq!(c.update(kind, ladder, Direction::East, 0, &air), ladder);
        let torch = state("minecraft:torch", &[]);
        assert_eq!(
            c.update(Connecting::Torch, torch, Direction::Down, 0, &air),
            0
        );
    }
}
