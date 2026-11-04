//! Desert pyramid placement, reconstructed from observations of vanilla piece and structure placement.

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, Nbt};
use harpas_core::random::{LegacyRandom, RandomSource};

use super::{Direction, StructurePiece};
use crate::GeneratedChunk;

pub(super) fn place(
    piece: &mut StructurePiece,
    chunk: &mut GeneratedChunk,
    random: &mut impl RandomSource,
    region: &mut impl RandomSource,
    seed: i64,
    height_at: &mut impl FnMut(i32, i32) -> i32,
    block_at: &mut impl FnMut(i32, i32, i32) -> u32,
) -> Vec<Compound> {
    let b = piece.bounding_box;
    let (cx, cz) = (chunk.chunk_x() * 16, chunk.chunk_z() * 16);
    if !b.intersects_columns(cx, cz, cx + 15, cz + 15) || piece.orientation.is_none() {
        return Vec::new();
    }
    let burial = random.next_int_bounded(3);
    align_height(piece, burial, chunk.min_y() + chunk.height() - 1, height_at);
    let mut pyramid = Pyramid {
        piece,
        chunk,
        entities: Vec::new(),
    };
    pyramid.shell();
    pyramid.treasure(random, block_at);
    let (candidates, roof) = pyramid.cellar(region, seed);
    pyramid.archaeology(seed, candidates, roof);
    pyramid.entities
}

pub(super) fn align_height(
    piece: &mut StructurePiece,
    burial: i32,
    max_y: i32,
    height_at: &mut impl FnMut(i32, i32) -> i32,
) {
    if !matches!(piece.save_data.get("HPos"), Some(Nbt::Int(y)) if *y >= 0) {
        let b = piece.bounding_box;
        let mut lowest = max_y;
        for z in b.min[2]..=b.max[2] {
            for x in b.min[0]..=b.max[0] {
                lowest = lowest.min(height_at(x, z));
            }
        }
        piece.save_data.insert("HPos", lowest);
        let offset = lowest - burial - b.min[1];
        piece.bounding_box.min[1] += offset;
        piece.bounding_box.max[1] += offset;
    }
}

struct Pyramid<'a> {
    piece: &'a mut StructurePiece,
    chunk: &'a mut GeneratedChunk,
    entities: Vec<Compound>,
}

fn state(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}

impl Pyramid<'_> {
    fn position(&self, [x, y, z]: [i32; 3]) -> [i32; 3] {
        let b = self.piece.bounding_box;
        let (x, z) = match self.piece.orientation.unwrap() {
            Direction::North => (b.min[0] + x, b.max[2] - z),
            Direction::South => (b.min[0] + x, b.min[2] + z),
            Direction::West => (b.max[0] - z, b.min[2] + x),
            Direction::East => (b.min[0] + z, b.min[2] + x),
        };
        [x, b.min[1] + y, z]
    }

    fn contains(&self, [x, y, z]: [i32; 3]) -> bool {
        (x >> 4, z >> 4) == (self.chunk.chunk_x(), self.chunk.chunk_z())
            && y >= self.chunk.min_y()
            && y < self.chunk.min_y() + self.chunk.height()
    }

    fn world_block(&mut self, pos: [i32; 3], block: u32) {
        if self.contains(pos) {
            self.chunk.set_block_state(
                (pos[0] & 15) as usize,
                pos[1],
                (pos[2] & 15) as usize,
                block,
            );
        }
    }

    fn block(&mut self, pos: [i32; 3], block: u32) {
        self.world_block(self.position(pos), block);
    }

    fn fill(&mut self, min: [i32; 3], max: [i32; 3], block: u32) {
        for y in min[1]..=max[1] {
            for x in min[0]..=max[0] {
                for z in min[2]..=max[2] {
                    self.block([x, y, z], block);
                }
            }
        }
    }

    fn hollow(&mut self, min: [i32; 3], max: [i32; 3], block: u32) {
        self.fill(min, max, block);
        self.fill(min.map(|v| v + 1), max.map(|v| v - 1), 0);
    }

    fn stair(&mut self, pos: [i32; 3], mut facing: Direction) {
        if matches!(
            self.piece.orientation,
            Some(Direction::South | Direction::West)
        ) {
            facing = match facing {
                Direction::North => Direction::South,
                Direction::South => Direction::North,
                other => other,
            };
        }
        if matches!(
            self.piece.orientation,
            Some(Direction::East | Direction::West)
        ) {
            facing = match facing {
                Direction::North => Direction::East,
                Direction::East => Direction::South,
                Direction::South => Direction::West,
                Direction::West => Direction::North,
            };
        }
        let facing = match facing {
            Direction::North => "north",
            Direction::East => "east",
            Direction::South => "south",
            Direction::West => "west",
        };
        self.block(
            pos,
            block_states()
                .state("minecraft:sandstone_stairs", [("facing", facing)])
                .unwrap()
                .0,
        );
    }

    fn shell(&mut self) {
        use Direction::{East, North, South, West};
        let (sandstone, cut, chiseled, orange) = (
            state("sandstone"),
            state("cut_sandstone"),
            state("chiseled_sandstone"),
            state("orange_terracotta"),
        );
        self.fill([0, -4, 0], [20, 0, 20], sandstone);
        for y in 1..=9 {
            self.fill([y, y, y], [20 - y, y, 20 - y], sandstone);
            self.fill([y + 1, y, y + 1], [19 - y, y, 19 - y], 0);
        }
        // The foundation descends only through blocks replaceable by structures.
        for x in 0..=20 {
            for z in 0..=20 {
                let [wx, mut y, wz] = self.position([x, -5, z]);
                if !self.contains([wx, y, wz]) {
                    continue;
                }
                while y > self.chunk.min_y() + 1 {
                    let block = self
                        .chunk
                        .block_state((wx & 15) as usize, y, (wz & 15) as usize);
                    let name = block_states().properties(block).unwrap().0;
                    if !block_states().is_air(block)
                        && !matches!(
                            name,
                            "minecraft:water"
                                | "minecraft:lava"
                                | "minecraft:bubble_column"
                                | "minecraft:glow_lichen"
                                | "minecraft:seagrass"
                                | "minecraft:tall_seagrass"
                        )
                    {
                        break;
                    }
                    self.world_block([wx, y, wz], sandstone);
                    y -= 1;
                }
            }
        }
        for x in [0, 16] {
            self.hollow([x, 0, 0], [x + 4, 9, 4], sandstone);
            self.fill([x + 1, 10, 1], [x + 3, 10, 3], sandstone);
            for (pos, facing) in [
                ([x + 2, 10, 0], North),
                ([x + 2, 10, 4], South),
                ([x, 10, 2], East),
                ([x + 4, 10, 2], West),
            ] {
                self.stair(pos, facing);
            }
        }
        self.hollow([8, 0, 0], [12, 4, 4], sandstone);
        self.fill([9, 1, 0], [11, 3, 4], 0);
        for x in [9, 11] {
            self.fill([x, 1, 1], [x, 3, 1], cut);
        }
        self.block([10, 3, 1], cut);
        for x in [4, 12] {
            self.hollow([x, 1, 1], [x + 4, 3, 3], sandstone);
            self.fill([x, 1, 2], [x + 4, 2, 2], 0);
        }
        self.fill([5, 4, 5], [15, 4, 15], sandstone);
        self.fill([9, 4, 9], [11, 4, 11], 0);
        for x in [8, 12] {
            for z in [8, 12] {
                self.fill([x, 1, z], [x, 3, z], cut);
            }
        }
        for (min, max) in [([1, 1, 5], [4, 4, 11]), ([16, 1, 5], [19, 4, 11])] {
            self.fill(min, max, sandstone);
        }
        for (x, inside) in [(5, 6), (15, 14)] {
            self.fill([inside, 7, 9], [inside, 7, 11], sandstone);
            self.fill([x, 5, 9], [x, 7, 11], cut);
            self.fill([x, 5, 10], [x, 6, 10], 0);
            self.block([inside, 6, 10], 0);
        }
        for x in [2, 18] {
            self.fill([x, 4, 4], [x, 6, 4], 0);
            self.stair([x, 4, 5], North);
            self.stair([x, 3, 4], North);
        }
        for (a, b) in [(1, 2), (18, 19)] {
            self.fill([a, 1, 3], [b, 2, 3], sandstone);
        }
        for x in [1, 19] {
            self.block([x, 1, 2], sandstone);
            self.block([x, 2, 2], state("sandstone_slab"));
        }
        self.stair([2, 1, 2], West);
        self.stair([18, 1, 2], East);
        for x in [4, 16] {
            self.fill([x, 3, 5], [x, 3, 17], sandstone);
            let left = if x == 4 { 3 } else { 15 };
            self.fill([left, 1, 5], [left + 1, 2, 16], 0);
            for z in (5..=17).step_by(2) {
                self.block([x, 1, z], cut);
                self.block([x, 2, z], chiseled);
            }
        }
        // Floor emblem: a central blue tile surrounded by a hollow orange diamond.
        for (dx, dz) in [
            (0, -3),
            (0, -2),
            (-1, -1),
            (1, -1),
            (-3, 0),
            (-2, 0),
            (2, 0),
            (3, 0),
            (-1, 1),
            (1, 1),
            (0, 2),
            (0, 3),
        ] {
            self.block([10 + dx, 0, 10 + dz], orange);
        }
        self.block([10, 0, 10], state("blue_terracotta"));
        // Seven rows of the tower emblem, repeated on their outside and entrance faces.
        for y in 2..=8 {
            for d in 1..=3 {
                let block = match (y, d) {
                    (8, _) => cut,
                    (4 | 6, 2) => chiseled,
                    (4 | 6, _) | (7, _) | (_, 2) => orange,
                    _ => cut,
                };
                for x in [0, 20] {
                    self.block([x, y, d], block);
                }
                for x in [0, 16] {
                    self.block([x + d, y, 0], block);
                }
            }
        }
        self.fill([8, 4, 0], [12, 6, 0], cut);
        for x in [8, 12] {
            self.block([x, 6, 0], 0);
        }
        for (x, block) in [(9, orange), (10, chiseled), (11, orange)] {
            self.block([x, 5, 0], block);
        }
    }

    fn treasure(
        &mut self,
        random: &mut impl RandomSource,
        block_at: &mut impl FnMut(i32, i32, i32) -> u32,
    ) {
        let (cut, chiseled) = (state("cut_sandstone"), state("chiseled_sandstone"));
        for (a, b, block) in [
            (-14, -11, cut),
            (-10, -10, chiseled),
            (-9, -9, cut),
            (-8, -1, state("sandstone")),
        ] {
            self.fill([8, a, 8], [12, b, 12], block);
        }
        self.fill([9, -11, 9], [11, -1, 11], 0);
        self.block([10, -11, 10], state("stone_pressure_plate"));
        self.fill([9, -13, 9], [11, -13, 11], state("tnt"));
        for [dx, dz] in [[-1, 0], [1, 0], [0, -1], [0, 1]] {
            self.fill(
                [10 + 2 * dx, -11, 10 + 2 * dz],
                [10 + 2 * dx, -10, 10 + 2 * dz],
                0,
            );
            self.block([10 + 3 * dx, -10, 10 + 3 * dz], chiseled);
            self.block([10 + 3 * dx, -11, 10 + 3 * dz], cut);
        }
        // Placement draws follow north/east/south/west; saved flags use Direction.get2DDataValue.
        for (i, [dx, dz]) in [[0, -1], [1, 0], [0, 1], [-1, 0]].into_iter().enumerate() {
            let flag = format!("hasPlacedChest{}", [2, 3, 0, 1][i]);
            if self.piece.save_data.get(&flag) == Some(&Nbt::Byte(1)) {
                continue;
            }
            let pos = self.position([10 + 2 * dx, -11, 10 + 2 * dz]);
            if !self.contains(pos) {
                continue;
            }
            let existing =
                self.chunk
                    .block_state((pos[0] & 15) as usize, pos[1], (pos[2] & 15) as usize);
            if block_states().properties(existing).unwrap().0 == "minecraft:chest" {
                continue;
            }
            let [x, y, z] = pos;
            let neighbors = [(x, z - 1), (x + 1, z), (x, z + 1), (x - 1, z)].map(|(x, z)| {
                if (x >> 4, z >> 4) == (self.chunk.chunk_x(), self.chunk.chunk_z()) {
                    self.chunk
                        .block_state((x & 15) as usize, y, (z & 15) as usize)
                } else {
                    block_at(x, y, z)
                }
            });
            let facing = super::placement::reorient_chest(neighbors);
            self.world_block(
                pos,
                block_states()
                    .state("minecraft:chest", [("facing", facing)])
                    .unwrap()
                    .0,
            );
            self.entity(pos, "chest", "chests/desert_pyramid", random.next_long());
            self.piece.save_data.insert(flag, 1_i8);
        }
    }

    fn entity(&mut self, [x, y, z]: [i32; 3], kind: &str, loot: &str, seed: i64) {
        let mut entity = Compound::new();
        entity.insert("id", format!("minecraft:{kind}"));
        entity.insert("x", x);
        entity.insert("y", y);
        entity.insert("z", z);
        entity.insert("components", Compound::new());
        entity.insert("LootTable", format!("minecraft:{loot}"));
        if seed != 0 {
            entity.insert("LootTableSeed", seed);
        }
        self.entities.push(entity);
    }

    fn cellar(&mut self, region: &mut impl RandomSource, seed: i64) -> (Vec<[i32; 3]>, [i32; 3]) {
        let (sand, sandstone, cut, chiseled, orange) = (
            state("sand"),
            state("sandstone"),
            state("cut_sandstone"),
            state("chiseled_sandstone"),
            state("orange_terracotta"),
        );
        for i in 1..=3 {
            self.stair([12 + i, -i, 17], Direction::West);
        }
        let variant = region.next_bool();
        for x in 12..=16 {
            self.block([x, 0, 17], sand);
        }
        for (x, y, block) in [
            (14, -1, sand),
            (15, -1, if variant { sand } else { sandstone }),
            (16, -1, if variant { sandstone } else { sand }),
            (15, -2, sand),
            (16, -2, sandstone),
            (16, -3, sand),
        ] {
            self.block([x, y, 17], block);
        }
        for (y, block) in [(-3, cut), (-2, chiseled), (-1, cut)] {
            for (min, max) in [
                ([13, y, 10], [13, y, 15]),
                ([19, y, 10], [19, y, 15]),
                ([13, y, 10], [19, y, 11]),
                ([13, y, 16], [19, y, 16]),
            ] {
                self.fill(min, max, block);
            }
        }
        let [x, y, z] = self.position([14, 0, 11]);
        let mut roof_random = LegacyRandom::new(seed).fork_positional().at(x, y, z);
        let roof = self.position([
            14 + roof_random.next_int_bounded(5),
            0,
            11 + roof_random.next_int_bounded(5),
        ]);
        for x in 14..=18 {
            for z in 11..=15 {
                let block = if region.next_float() < 0.33 {
                    sandstone
                } else {
                    sand
                };
                self.block([x, 0, z], block);
            }
        }
        self.block([16, -4, 13], state("blue_terracotta"));
        for (dx, dz) in [
            (-1, -1),
            (-1, 1),
            (1, -1),
            (1, 1),
            (-2, 0),
            (2, 0),
            (0, -2),
            (0, 2),
            (-3, 0),
            (3, 0),
            (0, -3),
            (0, 3),
        ] {
            self.block([16 + dx, -4, 13 + dz], orange);
        }
        for (x, z) in [(20, 13), (12, 13), (16, 9)] {
            self.block([x, -3, z], cut);
            self.block([x, -2, z], chiseled);
        }
        let mut candidates = Vec::new();
        for y in -3..=-1 {
            for x in 14..=18 {
                for z in 11..=15 {
                    candidates.push(self.position([x, y, z]));
                }
            }
        }
        for (x, z) in [(19, 13), (13, 13), (16, 16), (16, 10)] {
            for y in -3..=-2 {
                candidates.push(self.position([x, y, z]));
            }
        }
        (candidates, roof)
    }

    fn archaeology(&mut self, seed: i64, mut candidates: Vec<[i32; 3]>, roof: [i32; 3]) {
        // Selected positions are shuffled independently of the feature stream.
        candidates.sort_by_key(|&[x, y, z]| (y, z, x));
        let [x, y, z] = self.piece.bounding_box.center();
        let mut random = LegacyRandom::new(seed).fork_positional().at(x, y, z);
        for i in (2..=candidates.len()).rev() {
            let j = random.next_int_bounded(i as i32) as usize;
            candidates.swap(i - 1, j);
        }
        let count = random.next_int_bounded(3) + 5;
        for (i, pos) in candidates.into_iter().enumerate() {
            if self.contains(pos) {
                if i < (count as usize) {
                    self.suspicious(pos);
                } else {
                    self.world_block(pos, state("sand"));
                }
            }
        }
        if self.contains(roof) {
            self.suspicious(roof);
        }
    }

    fn suspicious(&mut self, pos: [i32; 3]) {
        self.world_block(pos, state("suspicious_sand"));
        let packed = ((i64::from(pos[0]) & 0x3ff_ffff) << 38)
            | ((i64::from(pos[2]) & 0x3ff_ffff) << 12)
            | (i64::from(pos[1]) & 0xfff);
        self.entity(pos, "brushable_block", "archaeology/desert_pyramid", packed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BoundingBox, StructureStart};

    fn start() -> StructureStart {
        let bounds = BoundingBox::oriented([0, 64, 0], Direction::South, 21, 15, 21);
        let mut save_data = Compound::new();
        save_data.insert("HPos", -1);
        for i in 0..4 {
            save_data.insert(format!("hasPlacedChest{i}"), 0_i8);
        }
        StructureStart {
            structure: "minecraft:desert_pyramid".into(),
            chunk: (0, 0),
            references: 0,
            bounding_box: bounds,
            pieces: vec![StructurePiece {
                kind: "minecraft:tedp".into(),
                bounding_box: bounds,
                orientation: Some(Direction::South),
                gen_depth: 0,
                junctions: Vec::new(),
                jigsaw: None,
                template: None,
                save_data,
            }],
        }
    }

    #[test]
    fn shared_height_uses_the_whole_footprint_and_survives_neighbor_placement() {
        let mut start = start();
        // The lowest column is in the diagonal neighbor, outside the source chunk.
        start.prepare_pyramid_height(42, 319, &mut |x, z| {
            if (x, z) == (20, 20) { 70 } else { 80 }
        });
        assert_eq!(start.pieces[0].bounding_box.min, [0, 69, 0]);
        assert_eq!(start.pieces[0].save_data.get("HPos"), Some(&Nbt::Int(70)));
        let mut chunk = GeneratedChunk::new(1, 1, -64, 384, 1);
        start.place_in_chunk(
            &mut chunk,
            42,
            &mut |_, _| panic!("saved height must not be recalculated"),
            &mut |_, _, _| 0,
        );
        assert_eq!(start.pieces[0].bounding_box.min, [0, 69, 0]);
        assert_eq!(chunk.block_state(4, 69, 4), state("sandstone"));
    }

    #[test]
    fn unrelated_chunks_do_not_move_pieces_or_consume_terrain_reads() {
        let mut start = start();
        let before = start.clone();
        let mut chunk = GeneratedChunk::new(-1, -1, -64, 384, 1);
        assert!(
            start
                .place_in_chunk(
                    &mut chunk,
                    42,
                    &mut |_, _| panic!("outside height read"),
                    &mut |_, _, _| panic!("outside block read"),
                )
                .is_empty()
        );
        assert_eq!(start, before);
        assert!(chunk.fluid_postprocessing().is_empty());
    }
}
