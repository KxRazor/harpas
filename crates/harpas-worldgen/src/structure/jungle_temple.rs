//! Jungle temple blocks reconstructed from observed vanilla placement, including its decoration stream.

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, Nbt};
use harpas_core::random::RandomSource;

use super::{Direction, StructurePiece};
use crate::{GeneratedChunk, HeightmapType};

pub(super) fn place(
    piece: &mut StructurePiece,
    chunk: &mut GeneratedChunk,
    random: &mut impl RandomSource,
    block_at: &mut impl FnMut(i32, i32, i32) -> u32,
) -> Vec<Compound> {
    let b = piece.bounding_box;
    let (cx, cz) = (chunk.chunk_x() * 16, chunk.chunk_z() * 16);
    if piece.orientation.is_none() || !b.intersects_columns(cx, cz, cx + 15, cz + 15) {
        return Vec::new();
    }
    if !matches!(piece.save_data.get("HPos"), Some(Nbt::Int(y)) if *y >= 0) {
        let mut sum = 0;
        let mut count = 0;
        for z in b.min[2].max(cz)..=b.max[2].min(cz + 15) {
            for x in b.min[0].max(cx)..=b.max[0].min(cx + 15) {
                sum += chunk.heightmaps().get(
                    HeightmapType::MotionBlockingNoLeaves,
                    (x & 15) as usize,
                    (z & 15) as usize,
                );
                count += 1;
            }
        }
        let height = sum / count;
        let offset = height - b.min[1];
        piece.bounding_box.min[1] += offset;
        piece.bounding_box.max[1] += offset;
        piece.save_data.insert("HPos", height);
    }
    let mut temple = Temple {
        piece,
        chunk,
        entities: Vec::new(),
    };
    temple.shell(random);
    temple.traps(random, block_at);
    temple.entities
}

fn state(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}

struct Temple<'a> {
    piece: &'a mut StructurePiece,
    chunk: &'a mut GeneratedChunk,
    entities: Vec<Compound>,
}

impl Temple<'_> {
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

    fn block(&mut self, pos: [i32; 3], block: u32) {
        let [x, y, z] = self.position(pos);
        if self.contains([x, y, z]) {
            self.chunk
                .set_block_state((x & 15) as usize, y, (z & 15) as usize, block);
        }
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

    fn stone(&mut self, min: [i32; 3], max: [i32; 3], random: &mut impl RandomSource) {
        let (plain, mossy) = (state("cobblestone"), state("mossy_cobblestone"));
        // Draws include positions clipped out of the target chunk.
        for y in min[1]..=max[1] {
            for x in min[0]..=max[0] {
                for z in min[2]..=max[2] {
                    let block = if random.next_float() < 0.4 {
                        plain
                    } else {
                        mossy
                    };
                    self.block([x, y, z], block);
                }
            }
        }
    }

    // Properties below describe the observed south-facing piece. Mirror/rotate them with its local axes.
    fn direction(&self, value: &str) -> &'static str {
        let i = match value {
            "north" => 0,
            "east" => 1,
            "south" => 2,
            "west" => 3,
            "up" => return "up",
            _ => unreachable!(),
        };
        (match self.piece.orientation.unwrap() {
            Direction::South => ["north", "east", "south", "west"],
            Direction::North => ["south", "east", "north", "west"],
            Direction::West => ["east", "south", "west", "north"],
            Direction::East => ["west", "south", "east", "north"],
        })[i]
    }

    fn oriented(&mut self, pos: [i32; 3], name: &str, properties: &[(&str, &str)]) {
        let properties: Vec<_> = properties
            .iter()
            .map(|&(key, value)| {
                if key == "facing" {
                    (key, self.direction(value))
                } else if matches!(key, "north" | "east" | "south" | "west") {
                    (self.direction(key), value)
                } else {
                    (key, value)
                }
            })
            .collect();
        let block = block_states()
            .state(&format!("minecraft:{name}"), properties)
            .unwrap()
            .0;
        self.block(pos, block);
    }

    fn stair(&mut self, pos: [i32; 3], facing: &str) {
        self.oriented(pos, "cobblestone_stairs", &[("facing", facing)]);
    }

    fn shell(&mut self, random: &mut impl RandomSource) {
        // Foundation, lower walls, upper walls and the stepped roof, in decoration draw order.
        for (min, max) in [
            ([0, -4, 0], [11, 0, 14]),
            ([2, 1, 2], [9, 2, 2]),
            ([2, 1, 12], [9, 2, 12]),
            ([2, 1, 3], [2, 2, 11]),
            ([9, 1, 3], [9, 2, 11]),
            ([1, 3, 1], [10, 6, 1]),
            ([1, 3, 13], [10, 6, 13]),
            ([1, 3, 2], [1, 6, 12]),
            ([10, 3, 2], [10, 6, 12]),
            ([2, 3, 2], [9, 3, 12]),
            ([2, 6, 2], [9, 6, 12]),
            ([3, 7, 3], [8, 7, 11]),
            ([4, 8, 4], [7, 8, 10]),
        ] {
            self.stone(min, max, random);
        }
        for (min, max) in [
            ([3, 1, 3], [8, 2, 11]),
            ([4, 3, 6], [7, 3, 9]),
            ([2, 4, 2], [9, 5, 12]),
            ([4, 6, 5], [7, 6, 9]),
            ([5, 7, 6], [6, 7, 8]),
            ([5, 1, 2], [6, 2, 2]),
            ([5, 2, 12], [6, 2, 12]),
            ([5, 5, 1], [6, 5, 1]),
            ([5, 5, 13], [6, 5, 13]),
            ([1, 5, 5], [1, 5, 5]),
            ([10, 5, 5], [10, 5, 5]),
            ([1, 5, 9], [1, 5, 9]),
            ([10, 5, 9], [10, 5, 9]),
        ] {
            self.fill(min, max, 0);
        }
        for z in [0, 14] {
            for x in [2, 4, 7, 9] {
                self.stone([x, 4, z], [x, 5, z], random);
            }
        }
        self.stone([5, 6, 0], [6, 6, 0], random);
        for x in [0, 11] {
            for z in (2..=12).step_by(2) {
                self.stone([x, 4, z], [x, 5, z], random);
            }
            for z in [5, 9] {
                self.stone([x, 6, z], [x, 6, z], random);
            }
        }
        for (x, z) in [(2, 2), (9, 2), (2, 12), (9, 12)] {
            self.stone([x, 7, z], [x, 9, z], random);
        }
        for (x, z) in [(4, 4), (7, 4), (4, 10), (7, 10)] {
            self.stone([x, 9, z], [x, 9, z], random);
        }
        self.stone([5, 9, 7], [6, 9, 7], random);
        for (z, facing) in [(6, "south"), (8, "north")] {
            for x in [5, 6] {
                self.stair([x, 9, z], facing);
            }
        }
        for x in 4..=7 {
            self.stair([x, 0, 0], "south");
        }
        for x in [4, 7] {
            for y in 1..=3 {
                self.stair([x, y, 7 + y], "south");
            }
        }
        self.stone([4, 1, 9], [4, 1, 9], random);
        self.stone([7, 1, 9], [7, 1, 9], random);
        self.stone([4, 1, 10], [7, 2, 10], random);
        self.stone([5, 4, 5], [6, 4, 5], random);
        self.stair([4, 4, 5], "east");
        self.stair([7, 4, 5], "west");
        for depth in 0..4 {
            for x in [5, 6] {
                self.stair([x, -depth, 6 + depth], "north");
            }
            self.fill([5, -depth, 7 + depth], [6, -depth, 9 + depth], 0);
        }
        for (min, max) in [
            ([1, -3, 12], [10, -1, 13]),
            ([1, -3, 1], [3, -1, 13]),
            ([1, -3, 1], [9, -1, 5]),
        ] {
            self.fill(min, max, 0);
        }
        for z in (1..=13).step_by(2) {
            self.stone([1, -3, z], [1, -2, z], random);
        }
        for z in (2..=12).step_by(2) {
            self.stone([1, -1, z], [3, -1, z], random);
        }
        for (min, max) in [
            ([2, -2, 1], [5, -2, 1]),
            ([7, -2, 1], [9, -2, 1]),
            ([6, -3, 1], [6, -3, 1]),
            ([6, -1, 1], [6, -1, 1]),
        ] {
            self.stone(min, max, random);
        }
    }

    fn wire(&mut self, pos: [i32; 3], north: &str, east: &str, south: &str, west: &str) {
        self.oriented(
            pos,
            "redstone_wire",
            &[
                ("north", north),
                ("east", east),
                ("south", south),
                ("west", west),
            ],
        );
    }

    fn traps(
        &mut self,
        random: &mut impl RandomSource,
        block_at: &mut impl FnMut(i32, i32, i32) -> u32,
    ) {
        let mossy = state("mossy_cobblestone");
        for (pos, facing) in [([1, -3, 8], "east"), ([4, -3, 8], "west")] {
            self.oriented(
                pos,
                "tripwire_hook",
                &[("facing", facing), ("attached", "true")],
            );
        }
        for x in [2, 3] {
            self.oriented(
                [x, -3, 8],
                "tripwire",
                &[("attached", "true"), ("east", "true"), ("west", "true")],
            );
        }
        for z in (2..=7).rev() {
            self.wire([5, -3, z], "side", "none", "side", "none");
        }
        self.wire([5, -3, 1], "none", "none", "side", "side");
        self.wire([4, -3, 1], "none", "side", "none", "side");
        self.block([3, -3, 1], mossy);
        self.container(
            [3, -2, 1],
            "dispenser",
            "south",
            "placedTrap1",
            random,
            block_at,
        );
        self.oriented([3, -2, 2], "vine", &[("north", "true")]);
        for (pos, facing) in [([7, -3, 1], "south"), ([7, -3, 5], "north")] {
            self.oriented(
                pos,
                "tripwire_hook",
                &[("facing", facing), ("attached", "true")],
            );
        }
        for z in 2..=4 {
            self.oriented(
                [7, -3, z],
                "tripwire",
                &[("attached", "true"), ("north", "true"), ("south", "true")],
            );
        }
        self.wire([8, -3, 6], "none", "side", "none", "side");
        self.wire([9, -3, 6], "side", "none", "none", "side");
        self.wire([9, -3, 5], "up", "none", "side", "none");
        self.block([9, -3, 4], mossy);
        self.wire([9, -2, 4], "side", "none", "side", "none");
        self.container(
            [9, -2, 3],
            "dispenser",
            "west",
            "placedTrap2",
            random,
            block_at,
        );
        for y in [-1, -2] {
            self.oriented([8, y, 3], "vine", &[("east", "true")]);
        }
        self.container(
            [8, -3, 3],
            "chest",
            "north",
            "placedMainChest",
            random,
            block_at,
        );
        for pos in [
            [9, -3, 2],
            [8, -3, 1],
            [4, -3, 5],
            [5, -2, 5],
            [5, -1, 5],
            [6, -3, 5],
            [7, -2, 5],
            [7, -1, 5],
            [8, -3, 5],
        ] {
            self.block(pos, mossy);
        }
        self.stone([9, -1, 1], [9, -1, 5], random);
        self.fill([8, -3, 8], [10, -1, 10], 0);
        for x in 8..=10 {
            self.block([x, -2, 11], state("chiseled_stone_bricks"));
        }
        for x in 8..=10 {
            self.oriented(
                [x, -2, 12],
                "lever",
                &[("face", "wall"), ("facing", "south")],
            );
        }
        self.stone([8, -3, 8], [8, -3, 10], random);
        self.stone([10, -3, 8], [10, -3, 10], random);
        self.block([10, -2, 9], mossy);
        for z in [9, 10] {
            self.wire([8, -2, z], "side", "none", "side", "none");
        }
        self.wire([10, -1, 9], "side", "side", "side", "side");
        for (pos, facing) in [
            ([9, -2, 8], "up"),
            ([10, -2, 8], "west"),
            ([10, -1, 8], "west"),
        ] {
            self.oriented(pos, "sticky_piston", &[("facing", facing)]);
        }
        self.oriented([10, -2, 10], "repeater", &[("facing", "south")]);
        self.container(
            [9, -3, 10],
            "chest",
            "north",
            "placedHiddenChest",
            random,
            block_at,
        );
    }

    fn container(
        &mut self,
        local: [i32; 3],
        kind: &str,
        facing: &str,
        flag: &str,
        random: &mut impl RandomSource,
        block_at: &mut impl FnMut(i32, i32, i32) -> u32,
    ) {
        if self.piece.save_data.get(flag) == Some(&Nbt::Byte(1)) {
            return;
        }
        let pos @ [x, y, z] = self.position(local);
        if !self.contains(pos) {
            return;
        }
        let existing = self
            .chunk
            .block_state((x & 15) as usize, y, (z & 15) as usize);
        if block_states().properties(existing).unwrap().0 == format!("minecraft:{kind}") {
            return;
        }
        let facing = if kind == "chest" {
            let neighbors = [(x, z - 1), (x + 1, z), (x, z + 1), (x - 1, z)].map(|(x, z)| {
                if (x >> 4, z >> 4) == (self.chunk.chunk_x(), self.chunk.chunk_z()) {
                    self.chunk
                        .block_state((x & 15) as usize, y, (z & 15) as usize)
                } else {
                    block_at(x, y, z)
                }
            });
            // These non-occluding mechanism blocks can surround the main chest. The terrain-only
            // heightmap approximation used by the shared reorientation helper would count them as solid.
            super::placement::reorient_chest(neighbors.map(|block| {
                if matches!(
                    block_states().properties(block).unwrap().0,
                    "minecraft:tripwire"
                        | "minecraft:tripwire_hook"
                        | "minecraft:redstone_wire"
                        | "minecraft:vine"
                ) {
                    0
                } else {
                    block
                }
            }))
        } else {
            self.direction(facing)
        };
        let block = block_states()
            .state(&format!("minecraft:{kind}"), [("facing", facing)])
            .unwrap()
            .0;
        self.block(local, block);
        let mut entity = Compound::new();
        entity.insert("id", format!("minecraft:{kind}"));
        entity.insert("x", x);
        entity.insert("y", y);
        entity.insert("z", z);
        entity.insert("components", Compound::new());
        entity.insert(
            "LootTable",
            if kind == "chest" {
                "minecraft:chests/jungle_temple"
            } else {
                "minecraft:chests/jungle_temple_dispenser"
            },
        );
        let seed = random.next_long();
        if seed != 0 {
            entity.insert("LootTableSeed", seed);
        }
        self.entities.push(entity);
        self.piece.save_data.insert(flag, 1_i8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BoundingBox, StructureStart};

    fn temple(x: i32, z: i32) -> StructureStart {
        let bounds = BoundingBox::oriented([x, 64, z], Direction::South, 12, 10, 15);
        let mut save_data = Compound::new();
        save_data.insert("HPos", -1);
        for key in [
            "placedMainChest",
            "placedHiddenChest",
            "placedTrap1",
            "placedTrap2",
        ] {
            save_data.insert(key, 0_i8);
        }
        StructureStart {
            structure: "minecraft:jungle_pyramid".into(),
            chunk: (x >> 4, z >> 4),
            references: 0,
            bounding_box: bounds,
            pieces: vec![StructurePiece {
                kind: "minecraft:tejp".into(),
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
    fn average_ground_ignores_leaves_and_loot_flags_follow_placed_containers() {
        let mut chunk = GeneratedChunk::new(0, 0, -64, 384, 1);
        for z in 0..16 {
            for x in 0..16 {
                chunk.set_block_state(x, 60 + (x % 3) as i32, z, state("stone"));
                chunk.set_block_state(x, 90, z, state("jungle_leaves"));
            }
        }
        let mut start = temple(0, 0);
        let entities = start.place_in_chunk(
            &mut chunk,
            42,
            &mut |_, _| panic!("no lowest-ground query"),
            &mut |_, _, _| panic!("containers fit inside chunk"),
        );
        assert_eq!(start.pieces[0].save_data.get("HPos"), Some(&Nbt::Int(62)));
        assert_eq!(start.bounding_box.min, [0, 62, 0]);
        assert_eq!(entities.len(), 4);
        assert_eq!(
            chunk.block_state(9, 60, 8),
            block_states()
                .state("minecraft:sticky_piston", [("facing", "up")])
                .unwrap()
                .0
        );
        for key in [
            "placedMainChest",
            "placedHiddenChest",
            "placedTrap1",
            "placedTrap2",
        ] {
            assert_eq!(start.pieces[0].save_data.get(key), Some(&Nbt::Byte(1)));
        }
        assert!(
            entities
                .iter()
                .all(|e| matches!(e.get("LootTableSeed"), Some(Nbt::Long(_))))
        );
    }

    #[test]
    fn saved_height_and_outside_chunks_preserve_piece_state() {
        let mut start = temple(14, 14);
        start.pieces[0].save_data.insert("HPos", 64);
        start.pieces[0].save_data.insert("placedTrap1", 1_i8);
        let before = start.clone();
        let mut outside = GeneratedChunk::new(-1, -1, -64, 384, 1);
        assert!(
            start
                .place_in_chunk(
                    &mut outside,
                    42,
                    &mut |_, _| unreachable!(),
                    &mut |_, _, _| unreachable!()
                )
                .is_empty()
        );
        assert_eq!(start, before);
        assert!(outside.fluid_postprocessing().is_empty());
        let mut neighbor = GeneratedChunk::new(1, 1, -64, 384, 1);
        neighbor.set_block_state(0, 100, 0, state("stone"));
        let entities = start.place_in_chunk(
            &mut neighbor,
            42,
            &mut |_, _| unreachable!(),
            &mut |_, _, _| 0,
        );
        assert_eq!(start.bounding_box.min, [14, 64, 14]);
        assert_eq!(start.pieces[0].save_data.get("HPos"), Some(&Nbt::Int(64)));
        assert_eq!(neighbor.block_state(0, 100, 0), state("stone"));
        assert_eq!(entities.len(), 3);
        assert!(
            entities
                .iter()
                .all(|e| e.get("x") != Some(&Nbt::Int(17)) || e.get("z") != Some(&Nbt::Int(15)))
        );
    }
}
