//! Ocean monument starts: the surrounding biomes and the single saved building piece.
//! Internal rooms are reconstructed from the structure seed; only the building is saved in NBT.
//! Block placement follows separately.

use std::collections::BTreeMap;

use harpas_core::random::{LegacyRandom, RandomSource, WorldgenRandom};

use super::start::{BoundingBox, Direction, StructurePiece, StructureStart};
use crate::data::{WorldgenData, WorldgenDataError};
use crate::generator::NoiseBasedChunkGenerator;

/// Radius of `OceanMonumentStructure`'s `BiomeSource.getBiomesWithin` check, in blocks.
const BIOME_RADIUS: i32 = 29;

// Connection slots follow Direction's 3D data values. North advances the room grid's Z.
const UP: usize = 1;
const NORTH: usize = 2;
const SOUTH: usize = 3;
const WEST: usize = 4;
const EAST: usize = 5;
const SOURCE: usize = 2;

/// One cell in the monument's room graph (`RoomDefinition`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonumentRoom {
    /// Grid index `x + 5*z + 25*y`; 1001, 1002 and 1003 connect the wings and penthouse.
    pub index: usize,
    /// Adjacent cells in down, up, north, south, west, east order, before closing passages.
    pub connections: [Option<usize>; 6],
    /// Open passages in the same order. Special connectors retain vanilla's asymmetric openings.
    pub openings: [bool; 6],
}

/// The shape of an internal piece. Double pieces occupy two or four grid cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonumentRoomKind {
    /// Entrance cell.
    Entry,
    /// The eight-cell treasure room.
    Core,
    /// Two cells across and two high.
    DoubleXY,
    /// Two cells deep and two high.
    DoubleYZ,
    /// Two cells deep.
    DoubleZ,
    /// Two cells across.
    DoubleX,
    /// Two cells high.
    DoubleY,
    /// A cell whose only possible passage is through its floor.
    SimpleTop,
    /// A single cell with one of three designs.
    Simple,
    /// One of the two wings, with alternating designs.
    Wing,
    /// The room above the grid.
    Penthouse,
}

/// An internal piece, kept in memory rather than added to the start's saved children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonumentRoomPiece {
    /// Piece shape.
    pub kind: MonumentRoomKind,
    /// Anchor cell, absent for wings and the penthouse.
    pub room: Option<usize>,
    /// Inclusive world bounds.
    pub bounding_box: BoundingBox,
    /// Design of a simple room (0..=2) or wing (0..=1).
    pub design: Option<i32>,
}

/// The monument's connected room graph and internal pieces (`MonumentBuilding`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonumentLayout {
    /// Cells by their grid index, including the three special connectors.
    pub rooms: BTreeMap<usize, MonumentRoom>,
    /// The treasure room's anchor cell in the unrotated grid.
    pub core_room: usize,
    /// All internal pieces in vanilla's construction order.
    pub pieces: Vec<MonumentRoomPiece>,
    /// Building orientation shared by every internal piece.
    pub orientation: Direction,
}

impl MonumentLayout {
    /// Builds a layout with the random stream positioned just after choosing the building orientation.
    /// `min_x` and `min_z` are the building's minimum block coordinates, not the start chunk's.
    pub fn new(
        random: &mut impl RandomSource,
        min_x: i32,
        min_z: i32,
        orientation: Direction,
    ) -> Self {
        let (rooms, core_room, order, mut claimed) = room_graph(random);
        claimed.insert(SOURCE);
        let mut layout = Self {
            rooms,
            core_room,
            pieces: Vec::new(),
            orientation,
        };
        layout.add_grid_piece(MonumentRoomKind::Entry, SOURCE, None, min_x, min_z);
        layout.add_grid_piece(MonumentRoomKind::Core, core_room, None, min_x, min_z);
        for room in order {
            if claimed.contains(&room) || room >= 1000 {
                continue;
            }
            let (kind, anchor, cells) = layout.fit(room, &claimed);
            claimed.extend(cells);
            let design = (kind == MonumentRoomKind::Simple).then(|| random.next_int_bounded(3));
            layout.add_grid_piece(kind, anchor, design, min_x, min_z);
        }
        let wing_design = random.next_int();
        for (kind, a, b, design) in [
            (
                MonumentRoomKind::Wing,
                [1, 1, 1],
                [23, 8, 21],
                Some(wing_design & 1),
            ),
            (
                MonumentRoomKind::Wing,
                [34, 1, 1],
                [56, 8, 21],
                Some(wing_design.wrapping_add(1) & 1),
            ),
            (
                MonumentRoomKind::Penthouse,
                [22, 13, 22],
                [35, 17, 35],
                None,
            ),
        ] {
            layout.pieces.push(MonumentRoomPiece {
                kind,
                room: None,
                design,
                bounding_box: BoundingBox::from_corners(
                    world_pos(orientation, min_x, min_z, a),
                    world_pos(orientation, min_x, min_z, b),
                ),
            });
        }
        layout
    }

    /// Reconstructs the internal rooms of a generated or loaded monument start, using the world seed.
    /// Non-monument and empty starts return `None`. Saved orientation and building origin take precedence.
    pub fn from_start(level_seed: i64, start: &StructureStart) -> Option<Self> {
        let piece = start
            .pieces
            .first()
            .filter(|piece| piece.kind == "minecraft:omb")?;
        let mut random = WorldgenRandom::new(LegacyRandom::new(0));
        random.set_large_feature_seed(level_seed, start.chunk.0, start.chunk.1);
        let drawn = Direction::random(&mut random);
        Some(Self::new(
            &mut random,
            piece.bounding_box.min[0],
            piece.bounding_box.min[2],
            piece.orientation.unwrap_or(drawn),
        ))
    }

    fn fit(
        &self,
        room: usize,
        claimed: &std::collections::BTreeSet<usize>,
    ) -> (MonumentRoomKind, usize, Vec<usize>) {
        use MonumentRoomKind::{DoubleX, DoubleXY, DoubleY, DoubleYZ, DoubleZ, Simple, SimpleTop};
        // A multi-cell room requires each of its internal passages, including both vertical sides.
        for (kind, paths) in [
            (DoubleXY, &[&[EAST][..], &[UP][..], &[EAST, UP][..]][..]),
            (DoubleYZ, &[&[NORTH][..], &[UP][..], &[NORTH, UP][..]][..]),
            (DoubleZ, &[&[NORTH][..]][..]),
            (DoubleX, &[&[EAST][..]][..]),
            (DoubleY, &[&[UP][..]][..]),
        ] {
            let mut cells = vec![room];
            for path in paths {
                let mut at = Some(room);
                for &direction in *path {
                    at = at.and_then(|id| {
                        self.rooms[&id].openings[direction]
                            .then_some(self.rooms[&id].connections[direction])
                            .flatten()
                    });
                }
                if let Some(id) = at.filter(|id| !claimed.contains(id)) {
                    cells.push(id);
                }
            }
            if cells.len() == paths.len() + 1 {
                return (kind, room, cells);
            }
        }
        let r = &self.rooms[&room];
        let top = [UP, NORTH, SOUTH, WEST, EAST]
            .into_iter()
            .all(|d| !r.openings[d]);
        (if top { SimpleTop } else { Simple }, room, vec![room])
    }

    fn add_grid_piece(
        &mut self,
        kind: MonumentRoomKind,
        room: usize,
        design: Option<i32>,
        min_x: i32,
        min_z: i32,
    ) {
        use MonumentRoomKind::{Core, DoubleX, DoubleXY, DoubleY, DoubleYZ, DoubleZ};
        let [w, h, d] = match kind {
            Core => [2, 2, 2],
            DoubleXY => [2, 2, 1],
            DoubleYZ => [1, 2, 2],
            DoubleX => [2, 1, 1],
            DoubleY => [1, 2, 1],
            DoubleZ => [1, 1, 2],
            _ => [1, 1, 1],
        };
        let (x, y, z) = (
            room as i32 % 5 * 8,
            room as i32 / 25 * 4,
            room as i32 / 5 % 5 * 8,
        );
        let (a, b) = match self.orientation {
            Direction::South => ([x, y, z], [x + w * 8 - 1, y + h * 4 - 1, z + d * 8 - 1]),
            Direction::North => ([x, y, -z - d * 8 + 1], [x + w * 8 - 1, y + h * 4 - 1, -z]),
            Direction::East => ([z, y, x], [z + d * 8 - 1, y + h * 4 - 1, x + w * 8 - 1]),
            Direction::West => ([-z - d * 8 + 1, y, x], [-z, y + h * 4 - 1, x + w * 8 - 1]),
        };
        let offset = world_pos(self.orientation, min_x, min_z, [9, 0, 22]);
        self.pieces.push(MonumentRoomPiece {
            kind,
            room: Some(room),
            design,
            bounding_box: BoundingBox {
                min: std::array::from_fn(|i| a[i] + offset[i]),
                max: std::array::from_fn(|i| b[i] + offset[i]),
            },
        });
    }
}

fn world_pos(direction: Direction, min_x: i32, min_z: i32, [x, y, z]: [i32; 3]) -> [i32; 3] {
    match direction {
        Direction::South => [min_x + x, 39 + y, min_z + z],
        Direction::North => [min_x + x, 39 + y, min_z + 57 - z],
        Direction::East => [min_x + z, 39 + y, min_z + x],
        Direction::West => [min_x + 57 - z, 39 + y, min_z + x],
    }
}

type RoomGraph = BTreeMap<usize, MonumentRoom>;
type Claimed = std::collections::BTreeSet<usize>;

fn room_graph(random: &mut impl RandomSource) -> (RoomGraph, usize, Vec<usize>, Claimed) {
    let mut rooms = RoomGraph::new();
    for y in 0..3 {
        for z in 0..if y == 2 { 2 } else { 4 } {
            for x in if y == 2 { 1..4 } else { 0..5 } {
                let index = x + 5 * z + 25 * y;
                rooms.insert(
                    index,
                    MonumentRoom {
                        index,
                        connections: [None; 6],
                        openings: [false; 6],
                    },
                );
            }
        }
    }
    let mut order: Vec<_> = rooms.keys().copied().collect();
    for &id in &order {
        let (x, y, z) = (id % 5, id / 25, id / 5 % 5);
        for (direction, neighbor) in [
            (UP, (y < 2).then_some(id + 25)),
            (NORTH, (z < 4).then_some(id + 5)),
            (EAST, (x < 4).then_some(id + 1)),
        ] {
            if let Some(other) = neighbor.filter(|id| rooms.contains_key(id)) {
                connect(&mut rooms, id, direction, other);
            }
        }
    }
    let mut claimed = Claimed::from([1001, 1002, 1003]);
    for (id, direction, special) in [(52, UP, 1003), (25, SOUTH, 1001), (29, SOUTH, 1002)] {
        rooms.insert(
            special,
            MonumentRoom {
                index: special,
                connections: [None; 6],
                openings: [false; 6],
            },
        );
        connect(&mut rooms, id, direction, special);
    }
    let core = 10 + random.next_int_bounded(4) as usize;
    for offset in [0, 1, 5, 6, 25, 26, 30, 31] {
        claimed.insert(core + offset);
    }
    for (&id, room) in &mut rooms {
        // Wings start without reverse openings. A rejected closure can later reopen their reverse edge.
        if id < 1000 || id == 1003 {
            room.openings = room.connections.map(|neighbor| neighbor.is_some());
        }
    }
    for end in (2..=order.len()).rev() {
        let at = random.next_int_bounded(end as i32) as usize;
        order.swap(end - 1, at);
    }
    for &id in &order {
        let mut removed = 0;
        for _ in 0..5 {
            if removed == 2 {
                break;
            }
            let direction = random.next_int_bounded(6) as usize;
            if !rooms[&id].openings[direction] {
                continue;
            }
            let other = rooms[&id].connections[direction].expect("open passage has a neighbor");
            rooms.get_mut(&id).unwrap().openings[direction] = false;
            rooms.get_mut(&other).unwrap().openings[direction ^ 1] = false;
            if reaches_source(&rooms, id) && reaches_source(&rooms, other) {
                removed += 1;
            } else {
                rooms.get_mut(&id).unwrap().openings[direction] = true;
                rooms.get_mut(&other).unwrap().openings[direction ^ 1] = true;
            }
        }
    }
    (rooms, core, order, claimed)
}

fn connect(rooms: &mut RoomGraph, a: usize, direction: usize, b: usize) {
    rooms.get_mut(&a).unwrap().connections[direction] = Some(b);
    rooms.get_mut(&b).unwrap().connections[direction ^ 1] = Some(a);
}

fn reaches_source(rooms: &RoomGraph, start: usize) -> bool {
    let mut visited = Claimed::new();
    let mut pending = vec![start];
    while let Some(id) = pending.pop() {
        if id == SOURCE {
            return true;
        }
        if !visited.insert(id) {
            continue;
        }
        let room = &rooms[&id];
        for d in 0..6 {
            if room.openings[d] {
                pending.push(room.connections[d].expect("open passage has a neighbor"));
            }
        }
    }
    false
}

/// The biome at the generation point is checked separately against the structure's allowed biomes.
pub(super) fn has_valid_surroundings(
    generator: &NoiseBasedChunkGenerator,
    data: &WorldgenData,
    min_x: i32,
    min_z: i32,
) -> Result<bool, WorldgenDataError> {
    let allowed =
        data.resolve_biomes(&["#minecraft:required_ocean_monument_surrounding".to_string()])?;
    let mut noise = generator.create_noise_chunk(min_x >> 4, min_z >> 4);
    Ok(all_surrounding_biomes(
        [min_x + 9, generator.sea_level(), min_z + 9],
        |[x, y, z]| {
            let climate = noise.sample_climate(x, y, z);
            let biome = generator.biome_source().get_noise_biome(climate, &mut None);
            allowed.contains(&biome)
        },
    ))
}

/// Checks the inclusive quart grid of the whole volume, not just its horizontal plane.
fn all_surrounding_biomes(center: [i32; 3], mut allowed: impl FnMut([i32; 3]) -> bool) -> bool {
    let min = center.map(|v| (v - BIOME_RADIUS) >> 2);
    let max = center.map(|v| (v + BIOME_RADIUS) >> 2);
    for x in min[0]..=max[0] {
        for y in min[1]..=max[1] {
            for z in min[2]..=max[2] {
                if !allowed([x, y, z]) {
                    return false;
                }
            }
        }
    }
    true
}

/// The one piece stored in the start. Its rooms are internal to this piece in vanilla.
pub(super) fn piece(random: &mut impl RandomSource, min_x: i32, min_z: i32) -> StructurePiece {
    let orientation = Direction::random(random);
    StructurePiece {
        kind: "minecraft:omb".to_string(),
        bounding_box: BoundingBox::oriented([min_x - 29, 39, min_z - 29], orientation, 58, 23, 58),
        orientation: Some(orientation),
        gen_depth: 0,
        junctions: Vec::new(),
        jigsaw: None,
        template: None,
        save_data: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rooms_tile_the_grid_once_and_keep_a_path_to_the_entrance() {
        let mut kinds = Vec::new();
        for seed in 0..128 {
            let layout =
                MonumentLayout::new(&mut LegacyRandom::new(seed), 10, 20, Direction::South);
            assert_eq!(layout.rooms.len(), 49);
            assert!((10..14).contains(&layout.core_room));
            let mut covered = Claimed::new();
            for piece in &layout.pieces {
                if !kinds.contains(&piece.kind) {
                    kinds.push(piece.kind);
                }
                if piece.room.is_none() {
                    continue;
                }
                let b = piece.bounding_box;
                for x in ((b.min[0] - 19) / 8)..=((b.max[0] - 19) / 8) {
                    for y in ((b.min[1] - 39) / 4)..=((b.max[1] - 39) / 4) {
                        for z in ((b.min[2] - 42) / 8)..=((b.max[2] - 42) / 8) {
                            let id = (x + z * 5 + y * 25) as usize;
                            assert!(covered.insert(id), "overlap at cell {id}, seed {seed}");
                        }
                    }
                }
            }
            assert_eq!(
                covered,
                layout
                    .rooms
                    .keys()
                    .copied()
                    .filter(|&id| id < 1000)
                    .collect()
            );
            for id in covered {
                assert!(
                    reaches_source(&layout.rooms, id),
                    "unreachable cell {id}, seed {seed}"
                );
            }
        }
        assert_eq!(kinds.len(), 11, "exercise every kind of internal piece");
    }

    #[test]
    fn rebuilding_rooms_preserves_the_single_saved_building_and_unknown_data() {
        let mut random = WorldgenRandom::new(LegacyRandom::new(0));
        random.set_large_feature_seed(42, -3, 4);
        let mut building = piece(&mut random, -48, 64);
        building.save_data.insert("unknown", 123);
        let generated = MonumentLayout::new(&mut random, -77, 35, building.orientation.unwrap());
        let start = StructureStart {
            structure: "minecraft:monument".to_string(),
            chunk: (-3, 4),
            references: 7,
            bounding_box: building.bounding_box,
            pieces: vec![building],
        };
        let data = WorldgenData::from_json_entries([(
            "data/minecraft/worldgen/structure/monument.json",
            r#"{"type":"minecraft:ocean_monument","biomes":[],"terrain_adaptation":"none"}"#,
        )])
        .unwrap();
        let saved = start.to_nbt();
        let loaded = StructureStart::from_nbt(&saved, &data, &Default::default())
            .unwrap()
            .unwrap();
        assert_eq!(MonumentLayout::from_start(42, &loaded).unwrap(), generated);
        assert_eq!(loaded.to_nbt(), saved);
        assert_eq!(loaded.pieces.len(), 1);
        let mut empty = start.clone();
        empty.pieces.clear();
        assert!(MonumentLayout::from_start(42, &empty).is_none());
        let mut other = start;
        other.pieces[0].kind = "minecraft:btp".to_string();
        assert!(MonumentLayout::from_start(42, &other).is_none());
    }

    #[test]
    fn the_surroundings_include_every_quart_at_both_corners() {
        // Chunk (-3, 4): negative coordinates must round down before scanning the 3D volume.
        let center = [-39, 63, 73];
        let mut sampled = std::collections::HashSet::new();
        assert!(all_surrounding_biomes(center, |pos| {
            assert!(sampled.insert(pos));
            true
        }));
        assert_eq!(sampled.len(), 15 * 16 * 15);
        assert!(sampled.contains(&[-17, 8, 11]));
        assert!(sampled.contains(&[-3, 23, 25]));
        assert!(!sampled.contains(&[-18, 8, 11]));
        assert!(!sampled.contains(&[-3, 24, 25]));
    }

    #[test]
    fn a_bad_biome_above_below_or_at_the_horizontal_boundary_refuses_the_start() {
        let center = [9, 63, 9];
        for bad in [[2, 8, 2], [2, 23, 2], [-5, 15, -5], [9, 15, 9]] {
            assert!(!all_surrounding_biomes(center, |pos| pos != bad));
        }
        for outside in [[2, 7, 2], [2, 24, 2], [-6, 15, -5], [10, 15, 9]] {
            assert!(all_surrounding_biomes(center, |pos| pos != outside));
        }
    }
}
