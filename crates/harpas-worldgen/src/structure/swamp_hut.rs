//! Swamp hut blocks, observed through vanilla piece placement; entity spawning belongs to gameplay.

use harpas_core::block::block_states;
use harpas_core::nbt::Nbt;

use super::{Direction, StructurePiece};
use crate::{GeneratedChunk, HeightmapType};

pub(super) fn place(piece: &mut StructurePiece, chunk: &mut GeneratedChunk) {
    let bounds = piece.bounding_box;
    let (cx, cz) = (chunk.chunk_x() * 16, chunk.chunk_z() * 16);
    if !bounds.intersects_columns(cx, cz, cx + 15, cz + 15) {
        return;
    }
    let Some(direction) = piece.orientation else {
        return;
    };
    if !matches!(piece.save_data.get("HPos"), Some(Nbt::Int(y)) if *y >= 0) {
        let mut sum = 0;
        let mut count = 0;
        for z in bounds.min[2].max(cz)..=bounds.max[2].min(cz + 15) {
            for x in bounds.min[0].max(cx)..=bounds.max[0].min(cx + 15) {
                sum += chunk.heightmaps().get(
                    HeightmapType::MotionBlockingNoLeaves,
                    (x - cx) as usize,
                    (z - cz) as usize,
                );
                count += 1;
            }
        }
        if count == 0 {
            return;
        }
        let height = sum / count;
        let offset = height - bounds.min[1];
        piece.bounding_box.min[1] += offset;
        piece.bounding_box.max[1] += offset;
        piece.save_data.insert("HPos", height);
    }
    let mut hut = Hut {
        piece,
        chunk,
        direction,
    };
    let states = block_states();
    let planks = states.default_state("minecraft:spruce_planks").unwrap();
    let log = states.default_state("minecraft:oak_log").unwrap();
    let fence = states.default_state("minecraft:oak_fence").unwrap();
    // Floor and porch, ceiling, front/back walls and side walls. Unwritten interior blocks stay intact.
    for (min, max) in [
        ([1, 1, 1], [5, 1, 7]),
        ([2, 1, 0], [4, 1, 0]),
        ([1, 4, 2], [5, 4, 7]),
        ([2, 2, 2], [3, 3, 2]),
        ([2, 2, 7], [4, 3, 7]),
        ([1, 2, 3], [1, 3, 6]),
        ([5, 2, 3], [5, 3, 6]),
    ] {
        hut.fill(min, max, planks);
    }
    for x in [1, 5] {
        for z in [2, 7] {
            hut.fill([x, 0, z], [x, 3, z], log);
        }
    }
    for pos in [[2, 3, 2], [3, 3, 7], [1, 2, 1], [5, 2, 1]] {
        hut.block(pos, fence);
    }
    for pos in [[1, 3, 4], [5, 3, 4], [5, 3, 5]] {
        hut.block(pos, 0);
    }
    for (pos, name) in [
        ([1, 3, 5], "minecraft:potted_red_mushroom"),
        ([3, 2, 6], "minecraft:crafting_table"),
        ([4, 2, 6], "minecraft:cauldron"),
    ] {
        hut.block(pos, states.default_state(name).unwrap());
    }
    for x in 0..=6 {
        hut.stair(
            [x, 4, 1],
            Direction::North,
            corner(x, "outer_right", "outer_left"),
        );
        hut.stair(
            [x, 4, 8],
            Direction::South,
            corner(x, "outer_left", "outer_right"),
        );
    }
    for z in 2..=7 {
        hut.stair([0, 4, z], Direction::East, "straight");
        hut.stair([6, 4, z], Direction::West, "straight");
    }
    for x in [1, 5] {
        for z in [2, 7] {
            let [wx, mut y, wz] = hut.position([x, -1, z]);
            if (wx >> 4, wz >> 4) != (hut.chunk.chunk_x(), hut.chunk.chunk_z()) {
                continue;
            }
            while y > hut.chunk.min_y() + 1 {
                let state = hut
                    .chunk
                    .block_state((wx & 15) as usize, y, (wz & 15) as usize);
                let name = states.properties(state).unwrap().0;
                if !states.is_air(state)
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
                hut.chunk
                    .set_block_state((wx & 15) as usize, y, (wz & 15) as usize, log);
                y -= 1;
            }
        }
    }
    // Keep Witch/Cat unchanged: vanilla must still be able to spawn them when it resumes this proto-chunk.
}

fn corner(x: i32, left: &'static str, right: &'static str) -> &'static str {
    match x {
        0 => left,
        6 => right,
        _ => "straight",
    }
}

struct Hut<'a> {
    piece: &'a StructurePiece,
    chunk: &'a mut GeneratedChunk,
    direction: Direction,
}

impl Hut<'_> {
    fn position(&self, [x, y, z]: [i32; 3]) -> [i32; 3] {
        let b = self.piece.bounding_box;
        let (x, z) = match self.direction {
            Direction::North => (b.min[0] + x, b.max[2] - z),
            Direction::South => (b.min[0] + x, b.min[2] + z),
            Direction::West => (b.max[0] - z, b.min[2] + x),
            Direction::East => (b.min[0] + z, b.min[2] + x),
        };
        [x, b.min[1] + y, z]
    }

    fn block(&mut self, pos: [i32; 3], state: u32) {
        let [x, y, z] = self.position(pos);
        if (x >> 4, z >> 4) == (self.chunk.chunk_x(), self.chunk.chunk_z()) {
            self.chunk
                .set_block_state((x & 15) as usize, y, (z & 15) as usize, state);
            if y >= self.chunk.min_y()
                && y < self.chunk.min_y() + self.chunk.height()
                && block_states().properties(state).unwrap().0 == "minecraft:oak_fence"
            {
                self.chunk
                    .mark_fluid_postprocessing((x & 15) as u8, y, (z & 15) as u8);
            }
        }
    }

    fn fill(&mut self, min: [i32; 3], max: [i32; 3], state: u32) {
        for y in min[1]..=max[1] {
            for x in min[0]..=max[0] {
                for z in min[2]..=max[2] {
                    self.block([x, y, z], state);
                }
            }
        }
    }

    fn stair(&mut self, pos: [i32; 3], mut facing: Direction, mut shape: &str) {
        if matches!(self.direction, Direction::South | Direction::West) {
            facing = match facing {
                Direction::North => Direction::South,
                Direction::South => Direction::North,
                other => other,
            };
            shape = match shape {
                "outer_left" => "outer_right",
                "outer_right" => "outer_left",
                other => other,
            };
        }
        if matches!(self.direction, Direction::East | Direction::West) {
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
        let state = block_states()
            .state(
                "minecraft:spruce_stairs",
                [("facing", facing), ("shape", shape)],
            )
            .unwrap()
            .0;
        self.block(pos, state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BoundingBox;

    fn piece(x: i32, z: i32) -> StructurePiece {
        StructurePiece {
            kind: "minecraft:tesh".into(),
            bounding_box: BoundingBox::oriented([x, 64, z], Direction::South, 7, 7, 9),
            orientation: Some(Direction::South),
            gen_depth: 0,
            junctions: Vec::new(),
            jigsaw: None,
            template: None,
            save_data: [
                ("HPos", Nbt::Int(-1)),
                ("Witch", Nbt::Byte(0)),
                ("Cat", Nbt::Byte(1)),
            ]
            .into_iter()
            .collect(),
        }
    }

    fn state(name: &str) -> u32 {
        block_states()
            .default_state(&format!("minecraft:{name}"))
            .unwrap()
    }

    #[test]
    fn height_ignores_leaves_and_preserves_interior_and_entity_flags() {
        let mut chunk = GeneratedChunk::new(-2, 3, -64, 384, 1);
        for z in 0..16 {
            for x in 0..16 {
                chunk.set_block_state(x, 60, z, state("stone"));
                chunk.set_block_state(x, 80, z, state("oak_leaves"));
            }
        }
        // Interior blocks are not cleared, while windows are.
        chunk.set_block_state(3, 63, 4, state("dirt"));
        chunk.set_block_state(5, 64, 4, state("dirt"));
        let mut piece = piece(-32, 48);
        place(&mut piece, &mut chunk);
        assert_eq!(piece.save_data.get("HPos"), Some(&Nbt::Int(61)));
        assert_eq!(piece.bounding_box.min, [-32, 61, 48]);
        assert_eq!(piece.save_data.get("Witch"), Some(&Nbt::Byte(0)));
        assert_eq!(piece.save_data.get("Cat"), Some(&Nbt::Byte(1)));
        assert_eq!(chunk.block_state(3, 63, 4), state("dirt"));
        assert_eq!(chunk.block_state(5, 64, 4), 0);
        assert_eq!(chunk.block_state(1, 64, 5), state("potted_red_mushroom"));
        assert_eq!(
            chunk.fluid_postprocessing(),
            &[(2, 64, 2), (3, 64, 7), (1, 63, 1), (5, 63, 1)]
        );
    }

    #[test]
    fn saved_height_clips_writes_and_outside_chunks_leave_piece_untouched() {
        let mut piece = piece(14, 14);
        piece.save_data.insert("HPos", 64);
        let mut chunk = GeneratedChunk::new(1, 1, -64, 384, 1);
        // Far higher terrain must not reset the saved ground height.
        chunk.set_block_state(0, 90, 0, state("stone"));
        place(&mut piece, &mut chunk);
        assert_eq!(piece.bounding_box.min, [14, 64, 14]);
        assert_eq!(piece.save_data.get("HPos"), Some(&Nbt::Int(64)));
        assert_eq!(chunk.block_state(1, 66, 4), state("crafting_table"));
        assert_eq!(chunk.block_state(3, -62, 0), state("oak_log"));
        assert_eq!(chunk.block_state(3, -63, 0), 0);
        assert_eq!(chunk.block_state(0, 90, 0), state("stone"));
        let before = piece.clone();
        let mut outside = GeneratedChunk::new(-1, -1, -64, 384, 1);
        place(&mut piece, &mut outside);
        assert_eq!(piece, before);
        assert!(outside.fluid_postprocessing().is_empty());
        assert_eq!(
            outside.heightmaps().get(HeightmapType::WorldSurface, 0, 0),
            -64
        );
    }
}
