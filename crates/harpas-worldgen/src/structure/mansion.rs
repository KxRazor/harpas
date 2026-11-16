//! The pieces of a woodland mansion start: vanilla's `WoodlandMansionStructure` and `WoodlandMansionPieces`.
//!
//! A mansion is laid out on an 11 by 11 grid of 8-block cells. Corridors grow from the entrance with the start's
//! random, the cells beside them become rooms of one, two or four cells, and a third floor may grow above a
//! two-cell room with stairs. The layout then becomes template pieces: outer walls traced around each floor,
//! roofs, corridor floors and carpets, inner walls and doors, and a room template for each room. Compared piece
//! by piece with vanilla's starts (`structure_start_parity`).

use harpas_core::random::RandomSource;

use super::start::{StructurePiece, TemplatePiece};
use super::templated::placed_piece;
use crate::data::WorldgenDataError;
use crate::template::{Facing, Mirror, Rotation, StructureTemplates};

/// The piece type of every mansion piece.
const PIECE: &str = "minecraft:wmp";
/// Mansions whose lowest corner is below this Y do not start.
pub(super) const MIN_Y: i32 = 60;
/// The grid's width and depth, in cells.
const SIZE: i32 = 11;

/// What a cell of a floor's layout holds.
const CLEAR: i32 = 0;
const CORRIDOR: i32 = 1;
const ROOM: i32 = 2;
const START_ROOM: i32 = 3;
const TEST_ROOM: i32 = 4;
const BLOCKED: i32 = 5;

/// A cell of a floor's rooms: the room's size, its number and flags.
const ROOM_1X1: i32 = 0x1_0000;
const ROOM_1X2: i32 = 0x2_0000;
const ROOM_2X2: i32 = 0x4_0000;
const ROOM_TYPE_MASK: i32 = 0xf_0000;
const ROOM_ID_MASK: i32 = 0xffff;
/// The cell of the room that its door, or its stairs, is placed from.
const ROOM_ORIGIN: i32 = 0x10_0000;
/// The room has a door to a corridor.
const ROOM_DOOR: i32 = 0x20_0000;
/// A two-cell room on the second floor with stairs to the third.
const ROOM_STAIRS: i32 = 0x40_0000;
/// The cell is the landing of stairs, which corridors open onto.
const ROOM_CORRIDOR: i32 = 0x80_0000;

/// The four horizontal directions, in vanilla's `Direction.Plane.HORIZONTAL` order.
const HORIZONTAL: [Facing; 4] = [Facing::North, Facing::East, Facing::South, Facing::West];

/// One cell's step along a horizontal direction, as (x, z).
const fn step(facing: Facing) -> (i32, i32) {
    match facing {
        Facing::North => (0, -1),
        Facing::South => (0, 1),
        Facing::West => (-1, 0),
        Facing::East => (1, 0),
        Facing::Up | Facing::Down => (0, 0),
    }
}

const fn clockwise(facing: Facing) -> Facing {
    Rotation::Clockwise90.rotate(facing)
}

const fn counterclockwise(facing: Facing) -> Facing {
    Rotation::Counterclockwise90.rotate(facing)
}

const fn opposite(facing: Facing) -> Facing {
    Rotation::Clockwise180.rotate(facing)
}

/// `Rotation.getRotated`: both turns, one after the other.
fn turned(a: Rotation, b: Rotation) -> Rotation {
    let index = |r| {
        Rotation::ALL
            .iter()
            .position(|&x| x == r)
            .expect("a rotation")
    };
    Rotation::ALL[(index(a) + index(b)) % 4]
}

/// A position moved `distance` blocks towards `facing`.
const fn moved([x, y, z]: [i32; 3], facing: Facing, distance: i32) -> [i32; 3] {
    let (dx, dz) = step(facing);
    [x + dx * distance, y, z + dz * distance]
}

/// A position moved `distance` blocks towards `facing` turned by the mansion's rotation.
fn toward(position: [i32; 3], rotation: Rotation, facing: Facing, distance: i32) -> [i32; 3] {
    moved(position, rotation.rotate(facing), distance)
}

const fn above([x, y, z]: [i32; 3], distance: i32) -> [i32; 3] {
    [x, y + distance, z]
}

/// A grid of cells, with a value for every cell outside it (`WoodlandMansionPieces.SimpleGrid`).
#[derive(Clone)]
struct Grid {
    cells: Vec<i32>,
    outside: i32,
}

impl Grid {
    fn new(outside: i32) -> Self {
        Self {
            cells: vec![CLEAR; (SIZE * SIZE) as usize],
            outside,
        }
    }

    const fn contains(x: i32, z: i32) -> bool {
        x >= 0 && x < SIZE && z >= 0 && z < SIZE
    }

    fn get(&self, x: i32, z: i32) -> i32 {
        if Self::contains(x, z) {
            self.cells[(x * SIZE + z) as usize]
        } else {
            self.outside
        }
    }

    fn set(&mut self, x: i32, z: i32, value: i32) {
        if Self::contains(x, z) {
            self.cells[(x * SIZE + z) as usize] = value;
        }
    }

    /// Fills the cells from one corner to the other, both included.
    fn fill(&mut self, x0: i32, z0: i32, x1: i32, z1: i32, value: i32) {
        for z in z0..=z1 {
            for x in x0..=x1 {
                self.set(x, z, value);
            }
        }
    }

    /// Sets a cell only if it holds `expected`.
    fn replace(&mut self, x: i32, z: i32, expected: i32, value: i32) {
        if self.get(x, z) == expected {
            self.set(x, z, value);
        }
    }

    /// Whether a cell beside this one holds `value`.
    fn borders(&self, x: i32, z: i32, value: i32) -> bool {
        self.get(x - 1, z) == value
            || self.get(x + 1, z) == value
            || self.get(x, z + 1) == value
            || self.get(x, z - 1) == value
    }

    /// Whether a cell is part of the house: a corridor or a room.
    fn is_house(&self, x: i32, z: i32) -> bool {
        matches!(self.get(x, z), CORRIDOR | ROOM | START_ROOM | TEST_ROOM)
    }
}

/// The layout of a mansion's three floors (`WoodlandMansionPieces.MansionGrid`).
struct Layout {
    base: Grid,
    third_floor: Grid,
    rooms: [Grid; 3],
    entrance: (i32, i32),
}

impl Layout {
    fn new(random: &mut impl RandomSource) -> Self {
        let (ex, ez) = (7, 4);
        let mut base = Grid::new(BLOCKED);
        base.fill(ex, ez, ex + 1, ez + 1, START_ROOM);
        base.fill(ex - 1, ez, ex - 1, ez + 1, ROOM);
        base.fill(ex + 2, ez - 2, ex + 3, ez + 3, BLOCKED);
        base.fill(ex + 1, ez - 2, ex + 1, ez - 1, CORRIDOR);
        base.fill(ex + 1, ez + 2, ex + 1, ez + 3, CORRIDOR);
        base.set(ex - 1, ez - 1, CORRIDOR);
        base.set(ex - 1, ez + 2, CORRIDOR);
        base.fill(0, 0, SIZE, 1, BLOCKED);
        base.fill(0, 9, SIZE, SIZE, BLOCKED);
        corridor(&mut base, random, ex, ez - 2, Facing::West, 6);
        corridor(&mut base, random, ex, ez + 3, Facing::West, 6);
        corridor(&mut base, random, ex - 2, ez - 1, Facing::West, 3);
        corridor(&mut base, random, ex - 2, ez + 2, Facing::West, 3);
        while clean_edges(&mut base) {}
        let mut rooms = [Grid::new(BLOCKED), Grid::new(BLOCKED), Grid::new(BLOCKED)];
        identify_rooms(&base, &mut rooms[0], random);
        identify_rooms(&base, &mut rooms[1], random);
        rooms[0].fill(ex + 1, ez, ex + 1, ez + 1, ROOM_CORRIDOR);
        rooms[1].fill(ex + 1, ez, ex + 1, ez + 1, ROOM_CORRIDOR);
        let mut layout = Self {
            base,
            third_floor: Grid::new(BLOCKED),
            rooms,
            entrance: (ex, ez),
        };
        layout.third_floor_corridors(random);
        let [_, _, third_rooms] = &mut layout.rooms;
        identify_rooms(&layout.third_floor, third_rooms, random);
        layout
    }

    /// Whether a cell of a floor belongs to the room with this number.
    fn is_room(&self, x: i32, z: i32, floor: usize, id: i32) -> bool {
        self.rooms[floor].get(x, z) & ROOM_ID_MASK == id
    }

    /// The direction of a two-cell room's other cell.
    fn other_cell(&self, x: i32, z: i32, floor: usize, id: i32) -> Option<Facing> {
        HORIZONTAL.into_iter().find(|&facing| {
            let (dx, dz) = step(facing);
            self.is_room(x + dx, z + dz, floor, id)
        })
    }

    /// Picks a second-floor room with a door and two cells for stairs, and grows the third floor's corridors
    /// from its far cell (`MansionGrid.setupThirdFloor`).
    fn third_floor_corridors(&mut self, random: &mut impl RandomSource) {
        let mut candidates = Vec::new();
        for z in 0..SIZE {
            for x in 0..SIZE {
                let value = self.rooms[1].get(x, z);
                if value & ROOM_TYPE_MASK == ROOM_1X2 && value & ROOM_DOOR == ROOM_DOOR {
                    candidates.push((x, z));
                }
            }
        }
        if candidates.is_empty() {
            self.third_floor.fill(0, 0, SIZE, SIZE, BLOCKED);
            return;
        }
        let (sx, sz) = candidates[random.next_int_bounded(candidates.len() as i32) as usize];
        let value = self.rooms[1].get(sx, sz);
        self.rooms[1].set(sx, sz, value | ROOM_STAIRS);
        let direction = self
            .other_cell(sx, sz, 1, value & ROOM_ID_MASK)
            .expect("a two-cell room");
        let (dx, dz) = step(direction);
        let (end_x, end_z) = (sx + dx, sz + dz);
        for z in 0..SIZE {
            for x in 0..SIZE {
                if !self.base.is_house(x, z) {
                    self.third_floor.set(x, z, BLOCKED);
                } else if (x, z) == (sx, sz) {
                    self.third_floor.set(x, z, START_ROOM);
                } else if (x, z) == (end_x, end_z) {
                    self.third_floor.set(x, z, START_ROOM);
                    self.rooms[2].set(x, z, ROOM_CORRIDOR);
                }
            }
        }
        let open: Vec<_> = HORIZONTAL
            .into_iter()
            .filter(|&facing| {
                let (dx, dz) = step(facing);
                self.third_floor.get(end_x + dx, end_z + dz) == CLEAR
            })
            .collect();
        if open.is_empty() {
            self.third_floor.fill(0, 0, SIZE, SIZE, BLOCKED);
            self.rooms[1].set(sx, sz, value);
            return;
        }
        let heading = open[random.next_int_bounded(open.len() as i32) as usize];
        let (dx, dz) = step(heading);
        corridor(
            &mut self.third_floor,
            random,
            end_x + dx,
            end_z + dz,
            heading,
            4,
        );
        while clean_edges(&mut self.third_floor) {}
    }
}

/// Grows a corridor from a cell, turning at random, and marks the clear cells around it as rooms
/// (`MansionGrid.recursiveCorridor`).
fn corridor(
    grid: &mut Grid,
    random: &mut impl RandomSource,
    x: i32,
    z: i32,
    heading: Facing,
    depth: i32,
) {
    if depth <= 0 {
        return;
    }
    let (hx, hz) = step(heading);
    grid.set(x, z, CORRIDOR);
    grid.replace(x + hx, z + hz, CLEAR, CORRIDOR);
    for _ in 0..8 {
        // `Direction.from2DDataValue`.
        let next = [Facing::South, Facing::West, Facing::North, Facing::East]
            [random.next_int_bounded(4) as usize];
        if next == opposite(heading) || (next == Facing::East && random.next_bool()) {
            continue;
        }
        let (nx, nz) = step(next);
        let (ahead_x, ahead_z) = (x + hx, z + hz);
        if grid.get(ahead_x + nx, ahead_z + nz) == CLEAR
            && grid.get(ahead_x + nx * 2, ahead_z + nz * 2) == CLEAR
        {
            corridor(grid, random, ahead_x + nx, ahead_z + nz, next, depth - 1);
            break;
        }
    }
    let (cx, cz) = step(clockwise(heading));
    let (ccx, ccz) = step(counterclockwise(heading));
    for (dx, dz) in [
        (cx, cz),
        (ccx, ccz),
        (hx + cx, hz + cz),
        (hx + ccx, hz + ccz),
        (hx * 2, hz * 2),
        (cx * 2, cz * 2),
        (ccx * 2, ccz * 2),
    ] {
        grid.replace(x + dx, z + dz, CLEAR, ROOM);
    }
}

/// Turns clear cells that the house nearly surrounds into rooms; returns whether any changed
/// (`MansionGrid.cleanEdges`).
fn clean_edges(grid: &mut Grid) -> bool {
    let mut changed = false;
    for z in 0..SIZE {
        for x in 0..SIZE {
            if grid.get(x, z) != CLEAR {
                continue;
            }
            let count = |cells: [(i32, i32); 4]| {
                cells
                    .into_iter()
                    .filter(|&(dx, dz)| grid.is_house(x + dx, z + dz))
                    .count()
            };
            let sides = count([(1, 0), (-1, 0), (0, 1), (0, -1)]);
            if sides >= 3 || (sides == 2 && count([(1, 1), (-1, 1), (1, -1), (-1, -1)]) <= 1) {
                grid.set(x, z, ROOM);
                changed = true;
            }
        }
    }
    changed
}

/// Groups a floor's room cells into rooms of one, two or four cells, in a shuffled order, and picks the cell each
/// room's door is placed from (`MansionGrid.identifyRooms`).
fn identify_rooms(layout: &Grid, rooms: &mut Grid, random: &mut impl RandomSource) {
    let mut cells = Vec::new();
    for z in 0..SIZE {
        for x in 0..SIZE {
            if layout.get(x, z) == ROOM {
                cells.push((x, z));
            }
        }
    }
    // `Util.shuffle`.
    for i in (1..cells.len()).rev() {
        let j = random.next_int_bounded(i as i32 + 1) as usize;
        cells.swap(i, j);
    }
    let mut id = 10;
    for (x, z) in cells {
        if rooms.get(x, z) != CLEAR {
            continue;
        }
        let free = |dx: i32, dz: i32| {
            rooms.get(x + dx, z + dz) == CLEAR && layout.get(x + dx, z + dz) == ROOM
        };
        let square = |dx: i32, dz: i32| free(dx, 0) && free(0, dz) && free(dx, dz);
        let (mut x0, mut z0, mut x1, mut z1) = (x, z, x, z);
        let kind = if square(1, 1) {
            (x1, z1) = (x + 1, z + 1);
            ROOM_2X2
        } else if square(-1, 1) {
            (x0, z1) = (x - 1, z + 1);
            ROOM_2X2
        } else if square(-1, -1) {
            (x0, z0) = (x - 1, z - 1);
            ROOM_2X2
        } else if free(1, 0) {
            x1 = x + 1;
            ROOM_1X2
        } else if free(0, 1) {
            z1 = z + 1;
            ROOM_1X2
        } else if free(-1, 0) {
            x0 = x - 1;
            ROOM_1X2
        } else if free(0, -1) {
            z0 = z - 1;
            ROOM_1X2
        } else {
            ROOM_1X1
        };
        // The door starts at a random corner, then tries the opposite corner and the two left. A room that no
        // corner connects to a corridor has no door, and its first cell is its origin.
        let mut door_x = if random.next_bool() { x0 } else { x1 };
        let mut door_z = if random.next_bool() { z0 } else { z1 };
        let other = |value, a, b| if value == a { b } else { a };
        let mut door = ROOM_DOOR;
        if !layout.borders(door_x, door_z, CORRIDOR) {
            door_x = other(door_x, x0, x1);
            door_z = other(door_z, z0, z1);
            if !layout.borders(door_x, door_z, CORRIDOR) {
                door_z = other(door_z, z0, z1);
                if !layout.borders(door_x, door_z, CORRIDOR) {
                    door_x = other(door_x, x0, x1);
                    door_z = other(door_z, z0, z1);
                    if !layout.borders(door_x, door_z, CORRIDOR) {
                        door = 0;
                        (door_x, door_z) = (x0, z0);
                    }
                }
            }
        }
        for cz in z0..=z1 {
            for cx in x0..=x1 {
                let flags = if (cx, cz) == (door_x, door_z) {
                    ROOM_ORIGIN | door
                } else {
                    0
                };
                rooms.set(cx, cz, flags | kind | id);
            }
        }
        id += 1;
    }
}

/// The room templates of a floor (`FirstFloorRoomCollection` and `SecondFloorRoomCollection`, which the third
/// floor shares).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Floor {
    First,
    Upper,
}

impl Floor {
    fn one_cell(self, random: &mut impl RandomSource) -> String {
        match self {
            Self::First => format!("1x1_a{}", random.next_int_bounded(5) + 1),
            Self::Upper => format!("1x1_b{}", random.next_int_bounded(5) + 1),
        }
    }

    /// Every floor's one-cell rooms without a door come from the first floor's.
    fn secret_one_cell(random: &mut impl RandomSource) -> String {
        format!("1x1_as{}", random.next_int_bounded(4) + 1)
    }

    fn side_entrance(self, random: &mut impl RandomSource, stairs: bool) -> String {
        match self {
            Self::First => format!("1x2_a{}", random.next_int_bounded(9) + 1),
            Self::Upper if stairs => "1x2_c_stairs".to_string(),
            Self::Upper => format!("1x2_c{}", random.next_int_bounded(4) + 1),
        }
    }

    fn front_entrance(self, random: &mut impl RandomSource, stairs: bool) -> String {
        match self {
            Self::First => format!("1x2_b{}", random.next_int_bounded(5) + 1),
            Self::Upper if stairs => "1x2_d_stairs".to_string(),
            Self::Upper => format!("1x2_d{}", random.next_int_bounded(5) + 1),
        }
    }

    fn secret_two_cells(self, random: &mut impl RandomSource) -> String {
        match self {
            Self::First => format!("1x2_s{}", random.next_int_bounded(2) + 1),
            Self::Upper => format!("1x2_se{}", random.next_int_bounded(1) + 1),
        }
    }

    fn four_cells(self, random: &mut impl RandomSource) -> String {
        match self {
            Self::First => format!("2x2_a{}", random.next_int_bounded(4) + 1),
            Self::Upper => format!("2x2_b{}", random.next_int_bounded(5) + 1),
        }
    }
}

/// Where the outer walls being traced have reached (`WoodlandMansionPieces.PlacementData`).
struct Cursor {
    position: [i32; 3],
    rotation: Rotation,
    wall: &'static str,
}

/// Turns a mansion's layout into template pieces (`WoodlandMansionPieces.MansionPiecePlacer`).
struct Placer<'a, R> {
    templates: &'a StructureTemplates,
    random: &'a mut R,
    pieces: Vec<StructurePiece>,
    start: (i32, i32),
}

impl<R: RandomSource> Placer<'_, R> {
    fn add(
        &mut self,
        name: &str,
        position: [i32; 3],
        rotation: Rotation,
        mirror: Mirror,
    ) -> Result<(), WorldgenDataError> {
        self.pieces.push(placed_piece(
            PIECE,
            self.templates,
            TemplatePiece {
                template: format!("minecraft:woodland_mansion/{name}"),
                position,
                rotation,
                mirror,
                pivot: [0, 0, 0],
            },
        )?);
        Ok(())
    }

    /// The north-west corner of a cell's 8-block square on a floor whose origin is `origin`.
    fn cell(&self, origin: [i32; 3], rotation: Rotation, x: i32, z: i32) -> [i32; 3] {
        let position = toward(origin, rotation, Facing::South, 8 + (z - self.start.1) * 8);
        toward(position, rotation, Facing::East, (x - self.start.0) * 8)
    }

    fn create(
        &mut self,
        origin: [i32; 3],
        rotation: Rotation,
        layout: &Layout,
    ) -> Result<(), WorldgenDataError> {
        let mut ground = Cursor {
            position: origin,
            rotation,
            wall: "wall_flat",
        };
        self.add(
            "entrance",
            toward(origin, rotation, Facing::West, 9),
            rotation,
            Mirror::None,
        )?;
        ground.position = toward(ground.position, rotation, Facing::South, 16);
        let mut second = Cursor {
            position: above(ground.position, 8),
            rotation,
            wall: "wall_window",
        };
        let (ex, ez) = layout.entrance;
        self.start = (ex + 1, ez + 1);
        let end = (ex + 1, ez);
        let (sx, sz) = self.start;
        self.outer_walls(&mut ground, &layout.base, (sx, sz), end)?;
        self.outer_walls(&mut second, &layout.base, (sx, sz), end)?;
        let mut third = Cursor {
            position: above(ground.position, 19),
            rotation,
            wall: "wall_window",
        };
        // The third floor's walls start from its first cell, scanning rows from the east.
        'scan: for z in 0..SIZE {
            for x in (0..SIZE).rev() {
                if layout.third_floor.is_house(x, z) {
                    third.position =
                        toward(third.position, rotation, Facing::South, 8 + (z - sz) * 8);
                    third.position = toward(third.position, rotation, Facing::East, (x - sx) * 8);
                    self.wall_piece(&mut third)?;
                    self.outer_walls(&mut third, &layout.third_floor, (x, z), (x, z))?;
                    break 'scan;
                }
            }
        }
        self.roof(
            above(origin, 16),
            rotation,
            &layout.base,
            Some(&layout.third_floor),
        )?;
        self.roof(above(origin, 27), rotation, &layout.third_floor, None)?;
        for floor in 0..3 {
            self.floor(origin, rotation, layout, floor)?;
        }
        Ok(())
    }

    /// Traces a floor's outer walls clockwise, from one cell back to another (`traverseOuterWalls`).
    fn outer_walls(
        &mut self,
        cursor: &mut Cursor,
        grid: &Grid,
        (mut x, mut z): (i32, i32),
        end: (i32, i32),
    ) -> Result<(), WorldgenDataError> {
        let start_direction = Facing::South;
        let mut direction = start_direction;
        loop {
            let (dx, dz) = step(direction);
            let (lx, lz) = step(counterclockwise(direction));
            if !grid.is_house(x + dx, z + dz) {
                // Turn right at a corner.
                cursor.position = toward(cursor.position, cursor.rotation, Facing::South, -1);
                self.add(
                    "wall_corner",
                    cursor.position,
                    cursor.rotation,
                    Mirror::None,
                )?;
                cursor.position = toward(cursor.position, cursor.rotation, Facing::South, -7);
                cursor.position = toward(cursor.position, cursor.rotation, Facing::West, -6);
                cursor.rotation = turned(cursor.rotation, Rotation::Clockwise90);
                direction = clockwise(direction);
                if (x, z) != end || direction != start_direction {
                    self.wall_piece(cursor)?;
                }
            } else if grid.is_house(x + dx + lx, z + dz + lz) {
                // Turn left around an inner corner.
                cursor.position = toward(cursor.position, cursor.rotation, Facing::South, 6);
                cursor.position = toward(cursor.position, cursor.rotation, Facing::East, 8);
                cursor.rotation = turned(cursor.rotation, Rotation::Counterclockwise90);
                (x, z) = (x + dx, z + dz);
                direction = counterclockwise(direction);
            } else {
                (x, z) = (x + dx, z + dz);
                if (x, z) != end || direction != start_direction {
                    self.wall_piece(cursor)?;
                }
            }
            if (x, z) == end && direction == start_direction {
                return Ok(());
            }
        }
    }

    /// One wall along a cell, after which the cursor moves to the next cell (`traverseWallPiece`).
    fn wall_piece(&mut self, cursor: &mut Cursor) -> Result<(), WorldgenDataError> {
        self.add(
            cursor.wall,
            toward(cursor.position, cursor.rotation, Facing::East, 7),
            cursor.rotation,
            Mirror::None,
        )?;
        cursor.position = toward(cursor.position, cursor.rotation, Facing::South, 8);
        Ok(())
    }

    /// A floor's roof, with small walls where the floor above covers it (`createRoof`).
    fn roof(
        &mut self,
        origin: [i32; 3],
        rotation: Rotation,
        grid: &Grid,
        above_grid: Option<&Grid>,
    ) -> Result<(), WorldgenDataError> {
        let r = |turn| turned(rotation, turn);
        let covered = |x, z| above_grid.is_some_and(|g| g.is_house(x, z));
        let house = |x, z| grid.is_house(x, z);
        let at = |p, facing, distance| toward(p, rotation, facing, distance);
        for z in 0..SIZE {
            for x in 0..SIZE {
                let p = self.cell(origin, rotation, x, z);
                if !house(x, z) || covered(x, z) {
                    continue;
                }
                self.add("roof", above(p, 3), rotation, Mirror::None)?;
                if !house(x + 1, z) {
                    self.add("roof_front", at(p, Facing::East, 6), rotation, Mirror::None)?;
                }
                if !house(x - 1, z) {
                    let q = at(at(p, Facing::East, 0), Facing::South, 7);
                    self.add("roof_front", q, r(Rotation::Clockwise180), Mirror::None)?;
                }
                if !house(x, z - 1) {
                    let q = at(p, Facing::West, 1);
                    self.add(
                        "roof_front",
                        q,
                        r(Rotation::Counterclockwise90),
                        Mirror::None,
                    )?;
                }
                if !house(x, z + 1) {
                    let q = at(at(p, Facing::East, 6), Facing::South, 6);
                    self.add("roof_front", q, r(Rotation::Clockwise90), Mirror::None)?;
                }
            }
        }
        if above_grid.is_some() {
            for z in 0..SIZE {
                for x in 0..SIZE {
                    let p = self.cell(origin, rotation, x, z);
                    if !house(x, z) || !covered(x, z) {
                        continue;
                    }
                    if !house(x + 1, z) {
                        self.add("small_wall", at(p, Facing::East, 7), rotation, Mirror::None)?;
                    }
                    if !house(x - 1, z) {
                        let q = at(at(p, Facing::West, 1), Facing::South, 6);
                        self.add("small_wall", q, r(Rotation::Clockwise180), Mirror::None)?;
                    }
                    if !house(x, z - 1) {
                        let q = at(at(p, Facing::West, 0), Facing::North, 1);
                        self.add(
                            "small_wall",
                            q,
                            r(Rotation::Counterclockwise90),
                            Mirror::None,
                        )?;
                    }
                    if !house(x, z + 1) {
                        let q = at(at(p, Facing::East, 6), Facing::South, 7);
                        self.add("small_wall", q, r(Rotation::Clockwise90), Mirror::None)?;
                    }
                    if !house(x + 1, z) {
                        if !house(x, z - 1) {
                            let q = at(at(p, Facing::East, 7), Facing::North, 2);
                            self.add("small_wall_corner", q, rotation, Mirror::None)?;
                        }
                        if !house(x, z + 1) {
                            let q = at(at(p, Facing::East, 8), Facing::South, 7);
                            self.add(
                                "small_wall_corner",
                                q,
                                r(Rotation::Clockwise90),
                                Mirror::None,
                            )?;
                        }
                    }
                    if !house(x - 1, z) {
                        if !house(x, z - 1) {
                            let q = at(at(p, Facing::West, 2), Facing::North, 1);
                            let turn = r(Rotation::Counterclockwise90);
                            self.add("small_wall_corner", q, turn, Mirror::None)?;
                        }
                        if !house(x, z + 1) {
                            let q = at(at(p, Facing::West, 1), Facing::South, 8);
                            let turn = r(Rotation::Clockwise180);
                            self.add("small_wall_corner", q, turn, Mirror::None)?;
                        }
                    }
                }
            }
        }
        for z in 0..SIZE {
            for x in 0..SIZE {
                let p = self.cell(origin, rotation, x, z);
                if !house(x, z) || covered(x, z) {
                    continue;
                }
                if !house(x + 1, z) {
                    let q = at(p, Facing::East, 6);
                    if !house(x, z + 1) {
                        self.add(
                            "roof_corner",
                            at(q, Facing::South, 6),
                            rotation,
                            Mirror::None,
                        )?;
                    } else if house(x + 1, z + 1) {
                        let inner = at(q, Facing::South, 5);
                        self.add("roof_inner_corner", inner, rotation, Mirror::None)?;
                    }
                    if !house(x, z - 1) {
                        let turn = r(Rotation::Counterclockwise90);
                        self.add("roof_corner", q, turn, Mirror::None)?;
                    } else if house(x + 1, z - 1) {
                        let inner = at(at(p, Facing::East, 9), Facing::North, 2);
                        let turn = r(Rotation::Clockwise90);
                        self.add("roof_inner_corner", inner, turn, Mirror::None)?;
                    }
                }
                if !house(x - 1, z) {
                    let q = at(at(p, Facing::East, 0), Facing::South, 0);
                    if !house(x, z + 1) {
                        let corner = at(q, Facing::South, 6);
                        let turn = r(Rotation::Clockwise90);
                        self.add("roof_corner", corner, turn, Mirror::None)?;
                    } else if house(x - 1, z + 1) {
                        let inner = at(at(q, Facing::South, 8), Facing::West, 3);
                        let turn = r(Rotation::Counterclockwise90);
                        self.add("roof_inner_corner", inner, turn, Mirror::None)?;
                    }
                    if !house(x, z - 1) {
                        let turn = r(Rotation::Clockwise180);
                        self.add("roof_corner", q, turn, Mirror::None)?;
                    } else if house(x - 1, z - 1) {
                        let inner = at(q, Facing::South, 1);
                        let turn = r(Rotation::Clockwise180);
                        self.add("roof_inner_corner", inner, turn, Mirror::None)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// A floor's corridors with their carpets, the walls and doors of its rooms, and the rooms themselves.
    fn floor(
        &mut self,
        origin: [i32; 3],
        rotation: Rotation,
        layout: &Layout,
        floor: usize,
    ) -> Result<(), WorldgenDataError> {
        let origin = above(origin, 8 * floor as i32 + if floor == 2 { 3 } else { 0 });
        let rooms = &layout.rooms[floor];
        let grid = if floor == 2 {
            &layout.third_floor
        } else {
            &layout.base
        };
        let collection = [Floor::First, Floor::Upper, Floor::Upper][floor];
        let r = |turn| turned(rotation, turn);
        let at = |p, facing, distance| toward(p, rotation, facing, distance);
        let (south_carpet, west_carpet) = if floor == 0 {
            ("carpet_south_1", "carpet_west_1")
        } else {
            ("carpet_south_2", "carpet_west_2")
        };
        let opens = |x, z| grid.get(x, z) == CORRIDOR || rooms.get(x, z) & ROOM_CORRIDOR != 0;
        for z in 0..SIZE {
            for x in 0..SIZE {
                if grid.get(x, z) != CORRIDOR {
                    continue;
                }
                let p = self.cell(origin, rotation, x, z);
                self.add("corridor_floor", p, rotation, Mirror::None)?;
                if opens(x, z - 1) {
                    self.add(
                        "carpet_north",
                        above(at(p, Facing::East, 1), 1),
                        rotation,
                        Mirror::None,
                    )?;
                }
                if opens(x + 1, z) {
                    let q = above(at(at(p, Facing::South, 1), Facing::East, 5), 1);
                    self.add("carpet_east", q, rotation, Mirror::None)?;
                }
                if opens(x, z + 1) {
                    let q = at(at(p, Facing::South, 5), Facing::West, 1);
                    self.add(south_carpet, q, rotation, Mirror::None)?;
                }
                if opens(x - 1, z) {
                    let q = at(at(p, Facing::West, 1), Facing::North, 1);
                    self.add(west_carpet, q, rotation, Mirror::None)?;
                }
            }
        }
        let (wall, door) = if floor == 0 {
            ("indoors_wall_1", "indoors_door_1")
        } else {
            ("indoors_wall_2", "indoors_door_2")
        };
        for z in 0..SIZE {
            for x in 0..SIZE {
                let landing = floor == 2 && grid.get(x, z) == START_ROOM;
                if grid.get(x, z) != ROOM && !landing {
                    continue;
                }
                let value = rooms.get(x, z);
                let kind = value & ROOM_TYPE_MASK;
                let id = value & ROOM_ID_MASK;
                let landing = landing && value & ROOM_CORRIDOR != 0;
                let doors: Vec<_> = if value & ROOM_DOOR != 0 {
                    HORIZONTAL
                        .into_iter()
                        .filter(|&facing| {
                            let (dx, dz) = step(facing);
                            grid.get(x + dx, z + dz) == CORRIDOR
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                let door_side = if !doors.is_empty() {
                    Some(doors[self.random.next_int_bounded(doors.len() as i32) as usize])
                } else if value & ROOM_ORIGIN != 0 {
                    Some(Facing::Up)
                } else {
                    None
                };
                let p = self.cell(origin, rotation, x, z);
                let p = at(p, Facing::East, -1);
                let piece = |side| if door_side == Some(side) { door } else { wall };
                if grid.is_house(x - 1, z) && !layout.is_room(x - 1, z, floor, id) {
                    self.add(piece(Facing::West), p, rotation, Mirror::None)?;
                }
                if grid.get(x + 1, z) == CORRIDOR && !landing {
                    self.add(
                        piece(Facing::East),
                        at(p, Facing::East, 8),
                        rotation,
                        Mirror::None,
                    )?;
                }
                if grid.is_house(x, z + 1) && !layout.is_room(x, z + 1, floor, id) {
                    let q = at(at(p, Facing::South, 7), Facing::East, 7);
                    self.add(
                        piece(Facing::South),
                        q,
                        r(Rotation::Clockwise90),
                        Mirror::None,
                    )?;
                }
                if grid.get(x, z - 1) == CORRIDOR && !landing {
                    let q = at(at(p, Facing::North, 1), Facing::East, 7);
                    self.add(
                        piece(Facing::North),
                        q,
                        r(Rotation::Clockwise90),
                        Mirror::None,
                    )?;
                }
                match (kind, door_side) {
                    (ROOM_1X1, _) => self.one_cell_room(p, rotation, door_side, collection)?,
                    (ROOM_1X2, Some(door_side)) => {
                        let other = layout.other_cell(x, z, floor, id);
                        let stairs = value & ROOM_STAIRS != 0;
                        self.two_cell_room(p, rotation, other, door_side, collection, stairs)?;
                    }
                    // Every floor has the same secret four-cell room.
                    (ROOM_2X2, Some(Facing::Up)) => {
                        self.add("2x2_s1", at(p, Facing::East, 1), rotation, Mirror::None)?;
                    }
                    (ROOM_2X2, Some(door_side)) => {
                        let mut side = clockwise(door_side);
                        let (dx, dz) = step(side);
                        if !layout.is_room(x + dx, z + dz, floor, id) {
                            side = opposite(side);
                        }
                        self.four_cell_room(p, rotation, side, door_side, collection)?;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// `addRoom1x1`: a room turned so that its door faces the door side.
    fn one_cell_room(
        &mut self,
        position: [i32; 3],
        rotation: Rotation,
        door: Option<Facing>,
        floor: Floor,
    ) -> Result<(), WorldgenDataError> {
        // A room without a door is a secret room, placed as if its door were to the east. Its floor's room is
        // drawn first all the same.
        let mut name = floor.one_cell(self.random);
        if door == Some(Facing::Up) {
            name = Floor::secret_one_cell(self.random);
        }
        let room_rotation = match door {
            Some(Facing::East | Facing::Up) => Rotation::None,
            Some(Facing::North) => Rotation::Counterclockwise90,
            Some(Facing::South) => Rotation::Clockwise90,
            _ => Rotation::Clockwise180,
        };
        // `StructureTemplate.getZeroPositionWithTransform` of (1, 0, 0) in a 7 by 7 room, then turned with the
        // mansion.
        let corner = match room_rotation {
            Rotation::None => [1, 0, 0],
            Rotation::Clockwise90 => [7, 0, 0],
            Rotation::Clockwise180 => [7, 0, 6],
            Rotation::Counterclockwise90 => [1, 0, 6],
        };
        let [dx, _, dz] = rotation.transform(corner);
        let [x, y, z] = position;
        self.add(
            &name,
            [x + dx, y, z + dz],
            turned(room_rotation, rotation),
            Mirror::None,
        )
    }

    /// `addRoom1x2`: a side or front entrance room, or a secret room without a door, placed for where its other
    /// cell and its door are.
    fn two_cell_room(
        &mut self,
        p: [i32; 3],
        rotation: Rotation,
        other: Option<Facing>,
        door: Facing,
        floor: Floor,
        stairs: bool,
    ) -> Result<(), WorldgenDataError> {
        use Facing::{East, North, South, Up, West};
        let r = |turn| turned(rotation, turn);
        let at = |p, facing, distance| toward(p, rotation, facing, distance);
        let Some(other) = other else {
            return Ok(());
        };
        let (kind, position, room_rotation, mirror) = match (door, other) {
            (East, South) => (0, at(p, East, 1), rotation, Mirror::None),
            (East, North) => (0, at(at(p, East, 1), South, 6), rotation, Mirror::LeftRight),
            (West, North) => (
                0,
                at(at(p, East, 7), South, 6),
                r(Rotation::Clockwise180),
                Mirror::None,
            ),
            (West, South) => (0, at(p, East, 7), rotation, Mirror::FrontBack),
            (South, East) => (
                0,
                at(p, East, 1),
                r(Rotation::Clockwise90),
                Mirror::LeftRight,
            ),
            (South, West) => (0, at(p, East, 7), r(Rotation::Clockwise90), Mirror::None),
            (North, West) => (
                0,
                at(at(p, East, 7), South, 6),
                r(Rotation::Clockwise90),
                Mirror::FrontBack,
            ),
            (North, East) => (
                0,
                at(at(p, East, 1), South, 6),
                r(Rotation::Counterclockwise90),
                Mirror::None,
            ),
            (South, North) => (1, at(at(p, East, 1), North, 8), rotation, Mirror::None),
            (North, South) => (
                1,
                at(at(p, East, 7), South, 14),
                r(Rotation::Clockwise180),
                Mirror::None,
            ),
            (West, East) => (1, at(p, East, 15), r(Rotation::Clockwise90), Mirror::None),
            (East, West) => (
                1,
                at(at(p, West, 7), South, 6),
                r(Rotation::Counterclockwise90),
                Mirror::None,
            ),
            // A secret room has no door, and its origin is its first cell.
            (Up, East) => (2, at(p, East, 15), r(Rotation::Clockwise90), Mirror::None),
            (Up, South) => (2, at(p, East, 1), rotation, Mirror::None),
            _ => return Ok(()),
        };
        let name = match kind {
            0 => floor.side_entrance(self.random, stairs),
            1 => floor.front_entrance(self.random, stairs),
            _ => floor.secret_two_cells(self.random),
        };
        self.add(&name, position, room_rotation, mirror)
    }

    /// `addRoom2x2`: a four-cell room placed for its door and the side it extends to.
    fn four_cell_room(
        &mut self,
        p: [i32; 3],
        rotation: Rotation,
        side: Facing,
        door: Facing,
        floor: Floor,
    ) -> Result<(), WorldgenDataError> {
        use Facing::{East, North, South, West};
        let r = |turn| turned(rotation, turn);
        let (east, south, room_rotation, mirror) = match (door, side) {
            (East, South) => (-7, 0, rotation, Mirror::None),
            (East, North) => (-7, 6, rotation, Mirror::LeftRight),
            (North, East) => (1, 14, r(Rotation::Counterclockwise90), Mirror::None),
            (North, West) => (7, 14, r(Rotation::Counterclockwise90), Mirror::LeftRight),
            (South, West) => (7, -8, r(Rotation::Clockwise90), Mirror::None),
            (South, East) => (1, -8, r(Rotation::Clockwise90), Mirror::LeftRight),
            (West, North) => (15, 6, r(Rotation::Clockwise180), Mirror::None),
            (West, South) => (15, 0, rotation, Mirror::FrontBack),
            _ => (0, 0, rotation, Mirror::None),
        };
        let position = toward(toward(p, rotation, East, east), rotation, South, south);
        let name = floor.four_cells(self.random);
        self.add(&name, position, room_rotation, mirror)
    }
}

/// `WoodlandMansionPieces.generateMansion`: lays the mansion out with the start's random and makes its pieces,
/// from its origin at the start's lowest corner.
pub(super) fn pieces(
    templates: &StructureTemplates,
    random: &mut impl RandomSource,
    origin: [i32; 3],
    rotation: Rotation,
) -> Result<Vec<StructurePiece>, WorldgenDataError> {
    let layout = Layout::new(random);
    let mut placer = Placer {
        templates,
        random,
        pieces: Vec::new(),
        start: (0, 0),
    };
    placer.create(origin, rotation, &layout)?;
    Ok(placer.pieces)
}

/// The cells of a mansion's layout, laid out with `random`: the base grid, the third floor's grid and the rooms
/// of each floor, column by column. Vanilla does not save the layout, so this is what `mansion_layout_parity`
/// compares.
pub fn mansion_layout_grids(random: &mut impl RandomSource) -> [Vec<i32>; 5] {
    let Layout {
        base,
        third_floor,
        rooms: [first, second, third],
        ..
    } = Layout::new(random);
    [base, third_floor, first, second, third].map(|grid| grid.cells)
}

/// The offsets of the corners whose lowest surface a mansion starts on, for its rotation
/// (`Structure.getLowestYIn5by5BoxOffset7Blocks`).
pub(super) const fn corner_offsets(rotation: Rotation) -> (i32, i32) {
    match rotation {
        Rotation::None => (5, 5),
        Rotation::Clockwise90 => (-5, 5),
        Rotation::Clockwise180 => (-5, -5),
        Rotation::Counterclockwise90 => (5, -5),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harpas_core::random::{LegacyRandom, WorldgenRandom};

    #[test]
    fn the_layout_keeps_the_entrance_and_every_room_has_a_number() {
        for seed in 0..64 {
            let mut random = WorldgenRandom::new(LegacyRandom::new(seed));
            let layout = Layout::new(&mut random);
            assert_eq!(layout.base.get(7, 4), START_ROOM);
            assert_eq!(layout.base.get(8, 5), START_ROOM);
            for floor in 0..2 {
                for z in 0..SIZE {
                    for x in 0..SIZE {
                        if layout.base.get(x, z) == ROOM {
                            let value = layout.rooms[floor].get(x, z);
                            assert!(
                                value & ROOM_ID_MASK >= 10,
                                "seed {seed}: room cell ({x}, {z})"
                            );
                            assert_ne!(value & ROOM_TYPE_MASK, 0);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn rotations_compose_in_vanillas_order() {
        assert_eq!(
            turned(Rotation::Clockwise90, Rotation::Counterclockwise90),
            Rotation::None
        );
        assert_eq!(
            turned(Rotation::Clockwise180, Rotation::Clockwise90),
            Rotation::Counterclockwise90
        );
        assert_eq!(
            toward([0, 0, 0], Rotation::Clockwise90, Facing::South, 2),
            [-2, 0, 0]
        );
    }
}
