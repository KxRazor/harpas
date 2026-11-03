//! The pieces of a stronghold start: vanilla's `StrongholdStructure.generatePieces` and `StrongholdPieces`.

use harpas_core::random::{LegacyRandom, RandomSource, WorldgenRandom};

use super::start::{BoundingBox, Direction, StructurePiece};

/// Pieces deeper than this get no children (`StrongholdPieces.MAX_DEPTH`).
const MAX_DEPTH: i32 = 50;
/// How far from the start a door may lead to a new piece, in blocks.
const MAX_DISTANCE: i32 = 112;
/// Pieces must stay above this Y (`StrongholdPieces.LOWEST_Y_POSITION`).
const LOWEST_Y: i32 = 10;
/// How far below the sea level the top of the stronghold must be.
const BELOW_SEA_LEVEL: i32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Start,
    Straight,
    PrisonHall,
    LeftTurn,
    RightTurn,
    RoomCrossing,
    StraightStairsDown,
    StairsDown,
    FiveCrossing,
    ChestCorridor,
    Library,
    PortalRoom,
    FillerCorridor,
}

impl Kind {
    const fn id(self) -> &'static str {
        match self {
            Self::Start => "minecraft:shstart",
            Self::Straight => "minecraft:shs",
            Self::PrisonHall => "minecraft:shph",
            Self::LeftTurn => "minecraft:shlt",
            Self::RightTurn => "minecraft:shrt",
            Self::RoomCrossing => "minecraft:shrc",
            Self::StraightStairsDown => "minecraft:shssd",
            Self::StairsDown => "minecraft:shsd",
            Self::FiveCrossing => "minecraft:sh5c",
            Self::ChestCorridor => "minecraft:shcc",
            Self::Library => "minecraft:shli",
            Self::PortalRoom => "minecraft:shpr",
            Self::FillerCorridor => "minecraft:shfc",
        }
    }
}

/// `StrongholdPieces.PieceWeight`, in `STRONGHOLD_PIECE_WEIGHTS` order.
#[derive(Debug, Clone, Copy)]
struct PieceWeight {
    kind: Kind,
    weight: i32,
    max_place_count: i32,
    place_count: i32,
}

impl PieceWeight {
    const ALL: [Self; 11] = [
        Self::new(Kind::Straight, 40, 0),
        Self::new(Kind::PrisonHall, 5, 5),
        Self::new(Kind::LeftTurn, 20, 0),
        Self::new(Kind::RightTurn, 20, 0),
        Self::new(Kind::RoomCrossing, 10, 6),
        Self::new(Kind::StraightStairsDown, 5, 5),
        Self::new(Kind::StairsDown, 5, 5),
        Self::new(Kind::FiveCrossing, 5, 4),
        Self::new(Kind::ChestCorridor, 5, 4),
        Self::new(Kind::Library, 10, 2),
        Self::new(Kind::PortalRoom, 20, 1),
    ];

    const fn new(kind: Kind, weight: i32, max_place_count: i32) -> Self {
        Self {
            kind,
            weight,
            max_place_count,
            place_count: 0,
        }
    }

    const fn is_valid(&self) -> bool {
        self.max_place_count == 0 || self.place_count < self.max_place_count
    }

    const fn do_place(&self, depth: i32) -> bool {
        self.is_valid()
            && match self.kind {
                Kind::Library => depth > 4,
                Kind::PortalRoom => depth > 5,
                _ => true,
            }
    }
}

/// A stronghold piece while the stronghold is laid out, with what decides its children.
#[derive(Debug, Clone)]
struct Piece {
    kind: Kind,
    bounding_box: BoundingBox,
    orientation: Direction,
    gen_depth: i32,
    /// Whether the start's stairs lead to a five-way crossing (`StairsDown.isSource`).
    is_source: bool,
    /// `FiveCrossing`'s `leftLow`, `leftHigh`, `rightLow` and `rightHigh`, or `Straight`'s `leftChild` and
    /// `rightChild` in the first two.
    children: [bool; 4],
}

impl Piece {
    fn new(kind: Kind, bounding_box: BoundingBox, orientation: Direction, gen_depth: i32) -> Self {
        Self {
            kind,
            bounding_box,
            orientation,
            gen_depth,
            is_source: false,
            children: [false; 4],
        }
    }
}

/// `BoundingBox.orientBox`: a box of `width` across, `height` up and `depth` forward, offset from a door's foot.
fn orient_box(
    [x, y, z]: [i32; 3],
    [off_x, off_y, off_z]: [i32; 3],
    [width, height, depth]: [i32; 3],
    direction: Direction,
) -> BoundingBox {
    let (min, max) = match direction {
        Direction::South => (
            [x + off_x, y + off_y, z + off_z],
            [
                x + width - 1 + off_x,
                y + height - 1 + off_y,
                z + depth - 1 + off_z,
            ],
        ),
        Direction::North => (
            [x + off_x, y + off_y, z - depth + 1 + off_z],
            [x + width - 1 + off_x, y + height - 1 + off_y, z + off_z],
        ),
        Direction::West => (
            [x - depth + 1 + off_z, y + off_y, z + off_x],
            [x + off_z, y + height - 1 + off_y, z + width - 1 + off_x],
        ),
        Direction::East => (
            [x + off_z, y + off_y, z + off_x],
            [
                x + depth - 1 + off_z,
                y + height - 1 + off_y,
                z + width - 1 + off_x,
            ],
        ),
    };
    BoundingBox { min, max }
}

fn intersects(a: &BoundingBox, b: &BoundingBox) -> bool {
    (0..3).all(|axis| a.max[axis] >= b.min[axis] && a.min[axis] <= b.max[axis])
}

struct Layout<'r> {
    random: &'r mut WorldgenRandom<LegacyRandom>,
    pieces: Vec<Piece>,
    pending: Vec<usize>,
    weights: Vec<PieceWeight>,
    total_weight: i32,
    previous: Option<Kind>,
    imposed: Option<Kind>,
    has_portal_room: bool,
}

impl Layout<'_> {
    fn collides(&self, bounding_box: &BoundingBox) -> Option<&Piece> {
        self.pieces
            .iter()
            .find(|piece| intersects(&piece.bounding_box, bounding_box))
    }

    /// `randomSmallDoor`: the entry door type, which only the placed blocks use.
    fn small_door(&mut self) {
        self.random.next_int_bounded(5);
    }

    /// `findAndCreatePieceFactory` and each piece's `createPiece`.
    fn create(
        &mut self,
        kind: Kind,
        foot: [i32; 3],
        direction: Direction,
        depth: i32,
    ) -> Option<Piece> {
        let fits = |layout: &Self, bounding_box: &BoundingBox| {
            bounding_box.min[1] > LOWEST_Y && layout.collides(bounding_box).is_none()
        };
        let (offset, size) = match kind {
            Kind::Straight | Kind::ChestCorridor => ([-1, -1, 0], [5, 5, 7]),
            Kind::PrisonHall => ([-1, -1, 0], [9, 5, 11]),
            Kind::LeftTurn | Kind::RightTurn => ([-1, -1, 0], [5, 5, 5]),
            Kind::RoomCrossing => ([-4, -1, 0], [11, 7, 11]),
            Kind::StraightStairsDown => ([-1, -7, 0], [5, 11, 8]),
            Kind::StairsDown => ([-1, -7, 0], [5, 11, 5]),
            Kind::FiveCrossing => ([-4, -3, 0], [10, 9, 11]),
            Kind::Library => ([-4, -1, 0], [14, 11, 15]),
            Kind::PortalRoom => ([-4, -1, 0], [11, 8, 16]),
            Kind::Start | Kind::FillerCorridor => unreachable!("not chosen by weight"),
        };
        let mut bounding_box = orient_box(foot, offset, size, direction);
        if !fits(self, &bounding_box) {
            if kind != Kind::Library {
                return None;
            }
            bounding_box = orient_box(foot, offset, [14, 6, 15], direction);
            if !fits(self, &bounding_box) {
                return None;
            }
        }
        let mut piece = Piece::new(kind, bounding_box, direction, depth);
        if kind != Kind::PortalRoom {
            self.small_door();
        }
        match kind {
            Kind::FiveCrossing => {
                for child in &mut piece.children[..3] {
                    *child = self.random.next_bool();
                }
                piece.children[3] = self.random.next_int_bounded(3) > 0;
            }
            Kind::RoomCrossing => {
                self.random.next_int_bounded(5);
            }
            Kind::Straight => {
                piece.children[0] = self.random.next_int_bounded(2) == 0;
                piece.children[1] = self.random.next_int_bounded(2) == 0;
            }
            _ => {}
        }
        Some(piece)
    }

    /// `StrongholdPieces.generatePieceFromSmallDoor`.
    fn piece_from_small_door(
        &mut self,
        foot: [i32; 3],
        direction: Direction,
        depth: i32,
    ) -> Option<Piece> {
        let has_any_pieces = self.weights.iter().any(|weight| {
            weight.max_place_count > 0 && weight.place_count < weight.max_place_count
        });
        self.total_weight = self.weights.iter().map(|weight| weight.weight).sum();
        if !has_any_pieces {
            return None;
        }
        if let Some(kind) = self.imposed.take()
            && let Some(piece) = self.create(kind, foot, direction, depth)
        {
            return Some(piece);
        }
        for _ in 0..5 {
            let mut selection = self.random.next_int_bounded(self.total_weight);
            let mut index = 0;
            while index < self.weights.len() {
                let weight = self.weights[index];
                selection -= weight.weight;
                if selection < 0 {
                    if !weight.do_place(depth) || Some(weight.kind) == self.previous {
                        break;
                    }
                    if let Some(piece) = self.create(weight.kind, foot, direction, depth) {
                        self.weights[index].place_count += 1;
                        self.previous = Some(weight.kind);
                        if !self.weights[index].is_valid() {
                            self.weights.remove(index);
                        }
                        return Some(piece);
                    }
                }
                index += 1;
            }
        }
        self.filler_corridor(foot, direction, depth)
    }

    /// `FillerCorridor.findPieceBox`: a short corridor up to the piece in the way.
    fn filler_corridor(&self, foot: [i32; 3], direction: Direction, depth: i32) -> Option<Piece> {
        let probe = orient_box(foot, [-1, -1, 0], [5, 5, 4], direction);
        let collision = self.collides(&probe)?;
        if collision.bounding_box.min[1] != probe.min[1] {
            return None;
        }
        let length = (1..=2).rev().find(|&length| {
            let bounding_box = orient_box(foot, [-1, -1, 0], [5, 5, length], direction);
            !intersects(&collision.bounding_box, &bounding_box)
        })?;
        let bounding_box = orient_box(foot, [-1, -1, 0], [5, 5, length + 1], direction);
        (bounding_box.min[1] > 1)
            .then(|| Piece::new(Kind::FillerCorridor, bounding_box, direction, depth))
    }

    /// `StrongholdPieces.generateAndAddPiece`.
    fn add_piece(&mut self, foot: [i32; 3], direction: Direction, depth: i32) {
        let start = self.pieces[0].bounding_box.min;
        if depth > MAX_DEPTH
            || (foot[0] - start[0]).abs() > MAX_DISTANCE
            || (foot[2] - start[2]).abs() > MAX_DISTANCE
        {
            return;
        }
        if let Some(piece) = self.piece_from_small_door(foot, direction, depth + 1) {
            self.pieces.push(piece);
            self.pending.push(self.pieces.len() - 1);
        }
    }

    /// `generateSmallDoorChildForward`.
    fn forward(&mut self, index: usize, x_off: i32, y_off: i32) {
        let Piece {
            bounding_box: BoundingBox { min, max },
            orientation,
            gen_depth,
            ..
        } = self.pieces[index];
        let foot = match orientation {
            Direction::North => [min[0] + x_off, min[1] + y_off, min[2] - 1],
            Direction::South => [min[0] + x_off, min[1] + y_off, max[2] + 1],
            Direction::West => [min[0] - 1, min[1] + y_off, min[2] + x_off],
            Direction::East => [max[0] + 1, min[1] + y_off, min[2] + x_off],
        };
        self.add_piece(foot, orientation, gen_depth);
    }

    /// `generateSmallDoorChildLeft`.
    fn left(&mut self, index: usize, y_off: i32, z_off: i32) {
        let Piece {
            bounding_box: BoundingBox { min, .. },
            orientation,
            gen_depth,
            ..
        } = self.pieces[index];
        match orientation {
            Direction::North | Direction::South => self.add_piece(
                [min[0] - 1, min[1] + y_off, min[2] + z_off],
                Direction::West,
                gen_depth,
            ),
            Direction::West | Direction::East => self.add_piece(
                [min[0] + z_off, min[1] + y_off, min[2] - 1],
                Direction::North,
                gen_depth,
            ),
        }
    }

    /// `generateSmallDoorChildRight`.
    fn right(&mut self, index: usize, y_off: i32, z_off: i32) {
        let Piece {
            bounding_box: BoundingBox { min, max },
            orientation,
            gen_depth,
            ..
        } = self.pieces[index];
        match orientation {
            Direction::North | Direction::South => self.add_piece(
                [max[0] + 1, min[1] + y_off, min[2] + z_off],
                Direction::East,
                gen_depth,
            ),
            Direction::West | Direction::East => self.add_piece(
                [min[0] + z_off, min[1] + y_off, max[2] + 1],
                Direction::South,
                gen_depth,
            ),
        }
    }

    /// Each piece's `addChildren`.
    fn add_children(&mut self, index: usize) {
        let Piece {
            kind,
            orientation,
            is_source,
            children,
            ..
        } = self.pieces[index];
        let north_or_east = matches!(orientation, Direction::North | Direction::East);
        match kind {
            Kind::Start | Kind::StairsDown => {
                if is_source {
                    self.imposed = Some(Kind::FiveCrossing);
                }
                self.forward(index, 1, 1);
            }
            Kind::ChestCorridor | Kind::PrisonHall | Kind::StraightStairsDown => {
                self.forward(index, 1, 1)
            }
            Kind::FiveCrossing => {
                let (a, b) = if matches!(orientation, Direction::West | Direction::North) {
                    (5, 3)
                } else {
                    (3, 5)
                };
                self.forward(index, 5, 1);
                // Vanilla passes these offsets as Y and the door heights as Z.
                if children[0] {
                    self.left(index, a, 1);
                }
                if children[1] {
                    self.left(index, b, 7);
                }
                if children[2] {
                    self.right(index, a, 1);
                }
                if children[3] {
                    self.right(index, b, 7);
                }
            }
            Kind::LeftTurn if north_or_east => self.left(index, 1, 1),
            Kind::LeftTurn => self.right(index, 1, 1),
            Kind::RightTurn if north_or_east => self.right(index, 1, 1),
            Kind::RightTurn => self.left(index, 1, 1),
            Kind::RoomCrossing => {
                self.forward(index, 4, 1);
                self.left(index, 1, 4);
                self.right(index, 1, 4);
            }
            Kind::Straight => {
                self.forward(index, 1, 1);
                if children[0] {
                    self.left(index, 1, 2);
                }
                if children[1] {
                    self.right(index, 1, 2);
                }
            }
            Kind::PortalRoom => self.has_portal_room = true,
            Kind::Library | Kind::FillerCorridor => {}
        }
    }
}

/// Lays out a stronghold starting in a chunk, trying again with the next seed until it has a portal room
/// (`StrongholdStructure.generatePieces`).
pub(super) fn pieces(
    level_seed: i64,
    chunk_x: i32,
    chunk_z: i32,
    sea_level: i32,
    min_y: i32,
) -> Vec<StructurePiece> {
    let mut random = WorldgenRandom::new(LegacyRandom::new(0));
    let mut tries: i64 = 0;
    loop {
        random.set_large_feature_seed(level_seed.wrapping_add(tries), chunk_x, chunk_z);
        tries += 1;
        let orientation = Direction::random(&mut random);
        let start_box = BoundingBox::oriented(
            [chunk_x * 16 + 2, 64, chunk_z * 16 + 2],
            orientation,
            5,
            11,
            5,
        );
        let mut start = Piece::new(Kind::Start, start_box, orientation, 0);
        start.is_source = true;
        let mut layout = Layout {
            random: &mut random,
            pieces: vec![start],
            pending: Vec::new(),
            weights: PieceWeight::ALL.to_vec(),
            total_weight: 0,
            previous: None,
            imposed: None,
            has_portal_room: false,
        };
        layout.add_children(0);
        while !layout.pending.is_empty() {
            let position = layout.random.next_int_bounded(layout.pending.len() as i32) as usize;
            let index = layout.pending.remove(position);
            layout.add_children(index);
        }
        let Layout {
            mut pieces,
            has_portal_room,
            ..
        } = layout;
        move_below_sea_level(&mut pieces, &mut random, sea_level, min_y);
        if has_portal_room {
            return pieces
                .into_iter()
                .map(|piece| StructurePiece {
                    kind: piece.kind.id(),
                    bounding_box: piece.bounding_box,
                    orientation: Some(piece.orientation),
                    gen_depth: piece.gen_depth,
                    junctions: Vec::new(),
                    jigsaw: None,
                })
                .collect();
        }
    }
}

/// `StructurePiecesBuilder.moveBelowSeaLevel`.
fn move_below_sea_level(
    pieces: &mut [Piece],
    random: &mut impl RandomSource,
    sea_level: i32,
    min_y: i32,
) {
    let max_y = sea_level - BELOW_SEA_LEVEL;
    let lowest = pieces
        .iter()
        .map(|piece| piece.bounding_box.min[1])
        .min()
        .expect("a start");
    let highest = pieces
        .iter()
        .map(|piece| piece.bounding_box.max[1])
        .max()
        .expect("a start");
    let mut top = highest - lowest + 1 + min_y + 1;
    if top < max_y {
        top += random.next_int_bounded(max_y - top);
    }
    let dy = top - highest;
    for piece in pieces {
        piece.bounding_box.min[1] += dy;
        piece.bounding_box.max[1] += dy;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oriented_boxes_grow_forward_from_the_door() {
        let size = [5, 5, 7];
        let south = orient_box([0, 20, 0], [-1, -1, 0], size, Direction::South);
        assert_eq!((south.min, south.max), ([-1, 19, 0], [3, 23, 6]));
        let north = orient_box([0, 20, 0], [-1, -1, 0], size, Direction::North);
        assert_eq!((north.min, north.max), ([-1, 19, -6], [3, 23, 0]));
        let west = orient_box([0, 20, 0], [-1, -1, 0], size, Direction::West);
        assert_eq!((west.min, west.max), ([-6, 19, -1], [0, 23, 3]));
        let east = orient_box([0, 20, 0], [-1, -1, 0], size, Direction::East);
        assert_eq!((east.min, east.max), ([0, 19, -1], [6, 23, 3]));
    }

    #[test]
    fn a_stronghold_has_one_portal_room_below_the_sea() {
        for (seed, x, z) in [(42, 10, -20), (0, -100, 3), (-1, 77, 77)] {
            let pieces = pieces(seed, x, z, 63, -64);
            assert_eq!(pieces[0].kind, "minecraft:shstart");
            let portal_rooms = pieces.iter().filter(|p| p.kind == "minecraft:shpr").count();
            assert_eq!(portal_rooms, 1);
            assert!(
                pieces
                    .iter()
                    .all(|p| p.bounding_box.max[1] <= 63 - BELOW_SEA_LEVEL)
            );
            assert_eq!(pieces, super::pieces(seed, x, z, 63, -64));
        }
    }
}
