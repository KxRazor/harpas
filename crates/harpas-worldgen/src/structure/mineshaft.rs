//! Mineshafts: vanilla's `MineshaftStructure` and the layout of `MineshaftPieces`.

use harpas_core::random::RandomSource;

use super::start::{BoundingBox, Direction, StructurePiece, below_sea_level_offset};
use crate::generator::{HeightmapType, NoiseBasedChunkGenerator};

/// The Y a mineshaft is laid out at, before it moves to its final height.
const START_Y: i32 = 50;
/// How far a corridor may start from the first room along X or Z.
const MAX_DISTANCE: i32 = 80;
/// How deep a piece may be for another to connect to it.
const MAX_DEPTH: i32 = 8;
/// How far below the sea level a normal mineshaft stays at least.
const BELOW_SEA_LEVEL: i32 = 10;

/// `MineshaftPieces`' piece types, with what their children need to know.
#[derive(Clone, Copy)]
enum Shape {
    Room,
    Corridor,
    Crossing(Direction),
    Stairs,
}

/// `MineshaftStructure.findGenerationPoint`: the generation point and the pieces.
pub(super) fn start(
    generator: &NoiseBasedChunkGenerator,
    random: &mut impl RandomSource,
    mesa: bool,
    min_x: i32,
    min_z: i32,
) -> ([i32; 3], Vec<StructurePiece>) {
    random.next_double();
    let mut pieces = layout(random, min_x + 2, min_z + 2);
    let mut bounds = pieces[0].bounding_box;
    for piece in &pieces[1..] {
        bounds.encapsulate(&piece.bounding_box);
    }
    let sea_level = generator.sea_level();
    let dy = if mesa {
        let [x, y, z] = bounds.center();
        let surface = generator.base_height(x, z, HeightmapType::WorldSurfaceWg);
        let target = if surface <= sea_level {
            sea_level
        } else {
            random.next_int_between_inclusive(sea_level, surface)
        };
        target - y
    } else {
        below_sea_level_offset(
            &bounds,
            random,
            sea_level,
            generator.min_y(),
            BELOW_SEA_LEVEL,
        )
    };
    for piece in &mut pieces {
        piece.bounding_box.min[1] += dy;
        piece.bounding_box.max[1] += dy;
    }
    ([min_x + 8, START_Y + dy, min_z], pieces)
}

/// Lays out a mineshaft from its first room, at the Y where it starts.
fn layout(random: &mut impl RandomSource, x: i32, z: i32) -> Vec<StructurePiece> {
    let max_x = x + 7 + random.next_int_bounded(6);
    let max_y = 54 + random.next_int_bounded(6);
    let max_z = z + 7 + random.next_int_bounded(6);
    let room = piece(
        "minecraft:msroom",
        BoundingBox {
            min: [x, START_Y, z],
            max: [max_x, max_y, max_z],
        },
        None,
        0,
    );
    let mut layout = Layout {
        random,
        pieces: vec![room],
        start: (x, z),
    };
    layout.add_children(0, Shape::Room);
    layout.pieces
}

const fn piece(
    kind: &'static str,
    bounding_box: BoundingBox,
    orientation: Option<Direction>,
    gen_depth: i32,
) -> StructurePiece {
    StructurePiece {
        kind,
        bounding_box,
        orientation,
        gen_depth,
        junctions: Vec::new(),
        jigsaw: None,
        template: None,
    }
}

/// A box 3 blocks wide that runs `length` blocks from a foot, from `low_y` up to 2 above it.
const fn run_box(
    direction: Direction,
    [x, y, z]: [i32; 3],
    low_y: i32,
    length: i32,
) -> BoundingBox {
    let far = length - 1;
    let (min, max) = match direction {
        Direction::North => ([0, low_y, -far], [2, 2, 0]),
        Direction::South => ([0, low_y, 0], [2, 2, far]),
        Direction::West => ([-far, low_y, 0], [0, 2, 2]),
        Direction::East => ([0, low_y, 0], [far, 2, 2]),
    };
    BoundingBox {
        min: [min[0] + x, min[1] + y, min[2] + z],
        max: [max[0] + x, max[1] + y, max[2] + z],
    }
}

/// The box of a crossing whose top is `top` above a foot (`MineShaftCrossing.findCrossing`).
const fn crossing_box(direction: Direction, [x, y, z]: [i32; 3], top: i32) -> BoundingBox {
    let (min, max) = match direction {
        Direction::North => ([-1, 0, -4], [3, top, 0]),
        Direction::South => ([-1, 0, 0], [3, top, 4]),
        Direction::West => ([-4, 0, -1], [0, top, 3]),
        Direction::East => ([0, 0, -1], [4, top, 3]),
    };
    BoundingBox {
        min: [min[0] + x, min[1] + y, min[2] + z],
        max: [max[0] + x, max[1] + y, max[2] + z],
    }
}

struct Layout<'r, R> {
    random: &'r mut R,
    pieces: Vec<StructurePiece>,
    /// The lowest X and Z of the first room.
    start: (i32, i32),
}

impl<R: RandomSource> Layout<'_, R> {
    fn is_free(&self, bounding_box: &BoundingBox) -> bool {
        !self
            .pieces
            .iter()
            .any(|piece| piece.bounding_box.intersects(bounding_box))
    }

    /// `MineShaftCorridor.findCorridorSize`: the longest free length, from 2 to 4 sections at most.
    fn corridor_box(&mut self, foot: [i32; 3], direction: Direction) -> Option<BoundingBox> {
        let longest = self.random.next_int_bounded(3) + 2;
        (1..=longest)
            .rev()
            .map(|sections| run_box(direction, foot, 0, sections * 5))
            .find(|bounding_box| self.is_free(bounding_box))
    }

    /// `MineshaftPieces.createRandomShaftPiece`.
    fn create(
        &mut self,
        foot: [i32; 3],
        direction: Direction,
        gen_depth: i32,
    ) -> Option<(StructurePiece, Shape)> {
        let selection = self.random.next_int_bounded(100);
        if selection >= 80 {
            let top = if self.random.next_int_bounded(4) == 0 {
                6
            } else {
                2
            };
            let bounding_box = crossing_box(direction, foot, top);
            self.is_free(&bounding_box).then(|| {
                let crossing = piece("minecraft:mscrossing", bounding_box, None, gen_depth);
                (crossing, Shape::Crossing(direction))
            })
        } else if selection >= 70 {
            let bounding_box = run_box(direction, foot, -5, 9);
            self.is_free(&bounding_box).then(|| {
                let stairs = piece(
                    "minecraft:msstairs",
                    bounding_box,
                    Some(direction),
                    gen_depth,
                );
                (stairs, Shape::Stairs)
            })
        } else {
            let bounding_box = self.corridor_box(foot, direction)?;
            // Whether the corridor has rails, and if not whether it has spiders. Placing it will need these.
            if self.random.next_int_bounded(3) != 0 {
                self.random.next_int_bounded(23);
            }
            let corridor = piece(
                "minecraft:mscorridor",
                bounding_box,
                Some(direction),
                gen_depth,
            );
            Some((corridor, Shape::Corridor))
        }
    }

    /// `MineshaftPieces.generateAndAddPiece`.
    fn add(&mut self, foot: [i32; 3], direction: Direction, depth: i32) {
        if depth > MAX_DEPTH
            || (foot[0] - self.start.0).abs() > MAX_DISTANCE
            || (foot[2] - self.start.1).abs() > MAX_DISTANCE
        {
            return;
        }
        if let Some((piece, shape)) = self.create(foot, direction, depth + 1) {
            self.pieces.push(piece);
            self.add_children(self.pieces.len() - 1, shape);
        }
    }

    /// `addChildren` of each piece type.
    fn add_children(&mut self, index: usize, shape: Shape) {
        let piece = &self.pieces[index];
        let (bounds, depth) = (piece.bounding_box, piece.gen_depth);
        let orientation = piece.orientation;
        match shape {
            Shape::Room => self.room_children(&bounds, depth),
            Shape::Corridor => {
                self.corridor_children(&bounds, orientation.expect("a corridor faces"), depth);
            }
            Shape::Crossing(direction) => self.crossing_children(&bounds, direction, depth),
            Shape::Stairs => {
                let [min_x, min_y, min_z] = bounds.min;
                let [max_x, _, max_z] = bounds.max;
                let direction = orientation.expect("stairs face");
                let foot = match direction {
                    Direction::North => [min_x, min_y, min_z - 1],
                    Direction::South => [min_x, min_y, max_z + 1],
                    Direction::West => [min_x - 1, min_y, min_z],
                    Direction::East => [max_x + 1, min_y, min_z],
                };
                self.add(foot, direction, depth);
            }
        }
    }

    /// `MineShaftRoom.addChildren`: exits along each wall, in the order north, south, west and east.
    fn room_children(&mut self, bounds: &BoundingBox, depth: i32) {
        let [min_x, min_y, min_z] = bounds.min;
        let [max_x, max_y, max_z] = bounds.max;
        let height_space = (max_y - min_y + 1 - 3 - 1).max(1);
        let (x_span, z_span) = (max_x - min_x + 1, max_z - min_z + 1);
        for (span, direction) in [
            (x_span, Direction::North),
            (x_span, Direction::South),
            (z_span, Direction::West),
            (z_span, Direction::East),
        ] {
            let mut position = 0;
            while position < span {
                position += self.random.next_int_bounded(span);
                if position + 3 > span {
                    break;
                }
                let y = min_y + self.random.next_int_bounded(height_space) + 1;
                let foot = match direction {
                    Direction::North => [min_x + position, y, min_z - 1],
                    Direction::South => [min_x + position, y, max_z + 1],
                    Direction::West => [min_x - 1, y, min_z + position],
                    Direction::East => [max_x + 1, y, min_z + position],
                };
                self.add(foot, direction, depth);
                position += 4;
            }
        }
    }

    /// `MineShaftCorridor.addChildren`: one piece at the end, ahead or to a side, and some along its sides.
    fn corridor_children(&mut self, bounds: &BoundingBox, orientation: Direction, depth: i32) {
        let [min_x, min_y, min_z] = bounds.min;
        let [max_x, _, max_z] = bounds.max;
        let end = self.random.next_int_bounded(4);
        let y = min_y - 1 + self.random.next_int_bounded(3);
        let (foot, direction) = match (orientation, end) {
            (Direction::North, 0 | 1) => ([min_x, y, min_z - 1], orientation),
            (Direction::North, 2) => ([min_x - 1, y, min_z], Direction::West),
            (Direction::North, _) => ([max_x + 1, y, min_z], Direction::East),
            (Direction::South, 0 | 1) => ([min_x, y, max_z + 1], orientation),
            (Direction::South, 2) => ([min_x - 1, y, max_z - 3], Direction::West),
            (Direction::South, _) => ([max_x + 1, y, max_z - 3], Direction::East),
            (Direction::West, 0 | 1) => ([min_x - 1, y, min_z], orientation),
            (Direction::West, 2) => ([min_x, y, min_z - 1], Direction::North),
            (Direction::West, _) => ([min_x, y, max_z + 1], Direction::South),
            (Direction::East, 0 | 1) => ([max_x + 1, y, min_z], orientation),
            (Direction::East, 2) => ([max_x - 3, y, min_z - 1], Direction::North),
            (Direction::East, _) => ([max_x - 3, y, max_z + 1], Direction::South),
        };
        self.add(foot, direction, depth);
        if depth >= MAX_DEPTH {
            return;
        }
        if matches!(orientation, Direction::North | Direction::South) {
            let mut z = min_z + 3;
            while z + 3 <= max_z {
                match self.random.next_int_bounded(5) {
                    0 => self.add([min_x - 1, min_y, z], Direction::West, depth + 1),
                    1 => self.add([max_x + 1, min_y, z], Direction::East, depth + 1),
                    _ => {}
                }
                z += 5;
            }
        } else {
            let mut x = min_x + 3;
            while x + 3 <= max_x {
                match self.random.next_int_bounded(5) {
                    0 => self.add([x, min_y, min_z - 1], Direction::North, depth + 1),
                    1 => self.add([x, min_y, max_z + 1], Direction::South, depth + 1),
                    _ => {}
                }
                x += 5;
            }
        }
    }

    /// `MineShaftCrossing.addChildren`: exits on every side but the one it was entered from, and sometimes on an
    /// upper floor.
    fn crossing_children(&mut self, bounds: &BoundingBox, direction: Direction, depth: i32) {
        let [min_x, min_y, min_z] = bounds.min;
        let [max_x, max_y, max_z] = bounds.max;
        let north = [min_x + 1, min_y, min_z - 1];
        let south = [min_x + 1, min_y, max_z + 1];
        let west = [min_x - 1, min_y, min_z + 1];
        let east = [max_x + 1, min_y, min_z + 1];
        let exits = match direction {
            Direction::North => [
                (north, Direction::North),
                (west, Direction::West),
                (east, Direction::East),
            ],
            Direction::South => [
                (south, Direction::South),
                (west, Direction::West),
                (east, Direction::East),
            ],
            Direction::West => [
                (north, Direction::North),
                (south, Direction::South),
                (west, Direction::West),
            ],
            Direction::East => [
                (north, Direction::North),
                (south, Direction::South),
                (east, Direction::East),
            ],
        };
        for (foot, direction) in exits {
            self.add(foot, direction, depth);
        }
        if max_y - min_y + 1 > 3 {
            for (foot, direction) in [
                (north, Direction::North),
                (west, Direction::West),
                (east, Direction::East),
                (south, Direction::South),
            ] {
                if self.random.next_bool() {
                    self.add([foot[0], foot[1] + 4, foot[2]], direction, depth);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use harpas_core::random::LegacyRandom;

    use super::*;

    #[test]
    fn a_mineshaft_starts_with_a_room_and_its_pieces_never_overlap() {
        let mut largest = 0;
        for seed in 0..64 {
            let pieces = layout(&mut LegacyRandom::new(seed), 2, 2);
            assert_eq!(pieces[0].kind, "minecraft:msroom");
            assert_eq!(pieces[0].bounding_box.min, [2, START_Y, 2]);
            assert_eq!(pieces[0].gen_depth, 0);
            assert!(pieces[0].orientation.is_none());
            largest = largest.max(pieces.len());
            for (index, piece) in pieces.iter().enumerate() {
                assert!((0..=MAX_DEPTH + 1).contains(&piece.gen_depth));
                for other in &pieces[index + 1..] {
                    assert!(!piece.bounding_box.intersects(&other.bounding_box));
                }
            }
        }
        assert!(largest > 1, "no mineshaft has more than a room");
    }

    #[test]
    fn the_same_seed_lays_out_the_same_mineshaft() {
        let lay = |seed| layout(&mut LegacyRandom::new(seed), -30, 44);
        assert_eq!(lay(7), lay(7));
    }

    #[test]
    fn runs_grow_away_from_their_foot() {
        let north = run_box(Direction::North, [10, 20, 30], 0, 10);
        assert_eq!((north.min, north.max), ([10, 20, 21], [12, 22, 30]));
        let east = run_box(Direction::East, [10, 20, 30], -5, 9);
        assert_eq!((east.min, east.max), ([10, 15, 30], [18, 22, 32]));
    }

    #[test]
    fn crossings_reach_one_block_to_each_side_of_their_foot() {
        let south = crossing_box(Direction::South, [10, 20, 30], 6);
        assert_eq!((south.min, south.max), ([9, 20, 30], [13, 26, 34]));
        let west = crossing_box(Direction::West, [10, 20, 30], 2);
        assert_eq!((west.min, west.max), ([6, 20, 29], [10, 22, 33]));
    }
}
