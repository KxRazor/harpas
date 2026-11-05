//! Stronghold blocks reconstructed by observing vanilla's piece placement on supplied terrain.

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_core::random::RandomSource;

use super::{Direction, StructurePiece, StructureStart};
use crate::GeneratedChunk;

pub(super) fn place(
    piece: &mut StructurePiece,
    chunk: &mut GeneratedChunk,
    random: &mut impl RandomSource,
    block_at: &mut impl FnMut(i32, i32, i32) -> u32,
) -> Vec<Compound> {
    let b = piece.bounding_box;
    let (x, z) = (chunk.chunk_x() * 16, chunk.chunk_z() * 16);
    if piece.orientation.is_none()
        || !b.intersects_columns(x, z, x + 15, z + 15)
        || b.max[1] < chunk.min_y()
        || b.min[1] >= chunk.min_y() + chunk.height()
    {
        return Vec::new();
    }
    let mut room = Room {
        piece,
        chunk,
        entities: Vec::new(),
    };
    room.build(random, block_at);
    room.entities
}

fn state(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}

struct Room<'a> {
    piece: &'a mut StructurePiece,
    chunk: &'a mut GeneratedChunk,
    entities: Vec<Compound>,
}

impl StructureStart {
    /// Neighboring world positions read to orient a stronghold chest placed in this chunk. Workers
    /// resolve only these positions, rather than generating terrain for the entire stronghold.
    pub fn stronghold_chest_neighbors(&self, chunk: &GeneratedChunk) -> Vec<[i32; 3]> {
        let mut samples = Vec::new();
        if self.structure != "minecraft:stronghold" {
            return samples;
        }
        for piece in &self.pieces {
            if piece.orientation.is_none() {
                continue;
            }
            let positions: &[[i32; 3]] = match piece.kind.as_str() {
                "minecraft:shcc" => &[[3, 2, 3]],
                "minecraft:shrc" if piece.save_data.get("Type") == Some(&Nbt::Int(2)) => {
                    &[[3, 4, 8]]
                }
                "minecraft:shli" if piece.save_data.get("Tall") == Some(&Nbt::Byte(1)) => {
                    &[[3, 3, 5], [12, 8, 1]]
                }
                "minecraft:shli" => &[[3, 3, 5]],
                _ => &[],
            };
            for &pos in positions {
                let [x, y, z] = position(piece, pos);
                if (x >> 4, z >> 4) != (chunk.chunk_x(), chunk.chunk_z())
                    || y < chunk.min_y()
                    || y >= chunk.min_y() + chunk.height()
                {
                    continue;
                }
                for [dx, dz] in [[0, -1], [1, 0], [0, 1], [-1, 0]] {
                    if ((x + dx) >> 4, (z + dz) >> 4) != (chunk.chunk_x(), chunk.chunk_z()) {
                        samples.push([x + dx, y, z + dz]);
                    }
                }
            }
        }
        samples
    }
}

fn position(piece: &StructurePiece, [x, y, z]: [i32; 3]) -> [i32; 3] {
    let b = piece.bounding_box;
    let (x, z) = match piece.orientation.unwrap() {
        Direction::North => (b.min[0] + x, b.max[2] - z),
        Direction::South => (b.min[0] + x, b.min[2] + z),
        Direction::West => (b.max[0] - z, b.min[2] + x),
        Direction::East => (b.min[0] + z, b.min[2] + x),
    };
    [x, b.min[1] + y, z]
}

impl Room<'_> {
    fn flag(&self, key: &str) -> bool {
        self.piece.save_data.get(key) == Some(&Nbt::Byte(1))
    }

    fn position(&self, [x, y, z]: [i32; 3]) -> [i32; 3] {
        position(self.piece, [x, y, z])
    }

    fn contains(&self, [x, y, z]: [i32; 3]) -> bool {
        (x >> 4, z >> 4) == (self.chunk.chunk_x(), self.chunk.chunk_z())
            && y >= self.chunk.min_y()
            && y < self.chunk.min_y() + self.chunk.height()
    }

    fn read(&self, pos: [i32; 3]) -> u32 {
        let [x, y, z] = self.position(pos);
        if self.contains([x, y, z]) {
            self.chunk
                .block_state((x & 15) as usize, y, (z & 15) as usize)
        } else {
            0
        }
    }

    fn block(&mut self, pos: [i32; 3], block: u32) {
        let [x, y, z] = self.position(pos);
        if self.contains([x, y, z]) {
            self.chunk
                .set_block_state((x & 15) as usize, y, (z & 15) as usize, block);
            let name = block_states().properties(block).unwrap().0;
            if matches!(
                name,
                "minecraft:iron_bars"
                    | "minecraft:oak_fence"
                    | "minecraft:ladder"
                    | "minecraft:wall_torch"
                    | "minecraft:torch"
            ) {
                self.chunk
                    .mark_fluid_postprocessing((x & 15) as u8, y, (z & 15) as u8);
            }
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

    // Selector draws occur only for the boundary; skip-air boxes also skip positions outside the chunk.
    fn masonry(
        &mut self,
        min: [i32; 3],
        max: [i32; 3],
        skip_air: bool,
        rng: &mut impl RandomSource,
    ) {
        for y in min[1]..=max[1] {
            for x in min[0]..=max[0] {
                for z in min[2]..=max[2] {
                    let pos = [x, y, z];
                    if skip_air && block_states().is_air(self.read(pos)) {
                        continue;
                    }
                    let boundary = (0..3).any(|i| pos[i] == min[i] || pos[i] == max[i]);
                    let block = if boundary {
                        let f = rng.next_float();
                        state(if f < 0.2 {
                            "cracked_stone_bricks"
                        } else if f < 0.5 {
                            "mossy_stone_bricks"
                        } else if f < 0.55 {
                            "infested_stone_bricks"
                        } else {
                            "stone_bricks"
                        })
                    } else {
                        state("cave_air")
                    };
                    self.block(pos, block);
                }
            }
        }
    }

    fn direction(&self, direction: &str) -> &'static str {
        let index = match direction {
            "north" => 0,
            "east" => 1,
            "south" => 2,
            "west" => 3,
            _ => unreachable!(),
        };
        (match self.piece.orientation.unwrap() {
            Direction::South => ["north", "east", "south", "west"],
            Direction::North => ["south", "east", "north", "west"],
            Direction::West => ["east", "south", "west", "north"],
            Direction::East => ["west", "south", "east", "north"],
        })[index]
    }

    fn oriented(&self, name: &str, properties: &[(&str, &str)]) -> u32 {
        let mirrored = matches!(
            self.piece.orientation,
            Some(Direction::North | Direction::East)
        );
        let properties: Vec<_> = properties
            .iter()
            .map(|&(k, v)| {
                if k == "facing" {
                    (k, self.direction(v))
                } else if matches!(k, "north" | "east" | "south" | "west") {
                    (self.direction(k), v)
                } else if k == "hinge" && mirrored {
                    (k, if v == "left" { "right" } else { "left" })
                } else {
                    (k, v)
                }
            })
            .collect();
        block_states()
            .state(&format!("minecraft:{name}"), properties)
            .unwrap()
            .0
    }

    fn facing(&mut self, pos: [i32; 3], name: &str, direction: &str) {
        self.block(pos, self.oriented(name, &[("facing", direction)]));
    }

    fn entry(&mut self, [x, y, z]: [i32; 3]) {
        let door = match self.piece.save_data.get("EntryDoor") {
            Some(Nbt::String(s)) => s.as_str(),
            _ => "OPENING",
        }
        .to_owned();
        self.door([x, y, z], &door);
    }

    fn door(&mut self, [x, y, z]: [i32; 3], door: &str) {
        if door == "OPENING" {
            self.fill([x, y, z], [x + 2, y + 2, z], state("cave_air"));
            return;
        }
        if door == "GRATES" {
            self.fill([x + 1, y, z], [x + 1, y + 1, z], state("cave_air"));
            for dx in 0..=2 {
                for step in 0..=2 {
                    let dy = if dx == 2 { 2 - step } else { step };
                    if dx == 1 && dy < 2 {
                        continue;
                    }
                    let props = if dy == 2 {
                        vec![("east", "true"), ("west", "true")]
                    } else if dx == 0 {
                        vec![("west", "true")]
                    } else {
                        vec![("east", "true")]
                    };
                    self.block([x + dx, y + dy, z], self.oriented("iron_bars", &props));
                }
            }
            return;
        }
        for dx in 0..=2 {
            for dy in 0..=2 {
                if dx != 1 || dy == 2 {
                    self.block([x + dx, y + dy, z], state("stone_bricks"));
                }
            }
        }
        let name = if door == "WOOD_DOOR" {
            "oak_door"
        } else {
            "iron_door"
        };
        for (dy, half) in [(0, "lower"), (1, "upper")] {
            self.block(
                [x + 1, y + dy, z],
                self.oriented(
                    name,
                    &[("facing", "south"), ("half", half), ("hinge", "right")],
                ),
            );
        }
        if door == "IRON_DOOR" {
            self.block(
                [x + 2, y + 1, z + 1],
                self.oriented("stone_button", &[("face", "wall"), ("facing", "south")]),
            );
            self.block(
                [x + 2, y + 1, z - 1],
                self.oriented("stone_button", &[("face", "wall"), ("facing", "north")]),
            );
        }
    }

    fn chest(
        &mut self,
        pos: [i32; 3],
        loot: &str,
        rng: &mut impl RandomSource,
        outside: &mut impl FnMut(i32, i32, i32) -> u32,
    ) {
        let [x, y, z] = self.position(pos);
        if !self.contains([x, y, z])
            || block_states().properties(self.read(pos)).unwrap().0 == "minecraft:chest"
        {
            return;
        }
        let neighbors = [(0, -1), (1, 0), (0, 1), (-1, 0)].map(|(dx, dz)| {
            let at = [x + dx, y, z + dz];
            if self.contains(at) {
                self.chunk
                    .block_state((at[0] & 15) as usize, y, (at[2] & 15) as usize)
            } else {
                outside(at[0], y, at[2])
            }
        });
        let facing = super::placement::reorient_chest(neighbors.map(|block| {
            let (name, properties) = block_states().properties(block).unwrap();
            if (name.ends_with("_slab") && !properties.contains(&("type", "double")))
                || name.ends_with("_stairs")
                || name.ends_with("_fence")
                || name.ends_with("_wall")
                || name.ends_with("_door")
                || name.ends_with("_trapdoor")
                || name.ends_with("glass_pane")
                || name == "minecraft:iron_bars"
                || name.ends_with("glass")
                || matches!(
                    name,
                    "minecraft:ice"
                        | "minecraft:frosted_ice"
                        | "minecraft:slime_block"
                        | "minecraft:honey_block"
                )
            {
                0
            } else {
                block
            }
        }));
        self.block(
            pos,
            block_states()
                .state("minecraft:chest", [("facing", facing)])
                .unwrap()
                .0,
        );
        let mut tag = Compound::new();
        tag.insert("id", "minecraft:chest");
        tag.insert("x", x);
        tag.insert("y", y);
        tag.insert("z", z);
        tag.insert("components", Compound::new());
        tag.insert("LootTable", format!("minecraft:chests/stronghold_{loot}"));
        let seed = rng.next_long();
        if seed != 0 {
            tag.insert("LootTableSeed", seed);
        }
        self.entities.push(tag);
    }

    fn build(
        &mut self,
        rng: &mut impl RandomSource,
        outside: &mut impl FnMut(i32, i32, i32) -> u32,
    ) {
        let air = state("cave_air");
        let stone = state("stone_bricks");
        let slab = state("smooth_stone_slab");
        let (size, entry) = match self.piece.kind.as_str() {
            "minecraft:shs" | "minecraft:shcc" => ([5, 5, 7], [1, 1, 0]),
            "minecraft:shlt" | "minecraft:shrt" => ([5, 5, 5], [1, 1, 0]),
            "minecraft:shph" => ([9, 5, 11], [1, 1, 0]),
            "minecraft:shrc" => ([11, 7, 11], [4, 1, 0]),
            "minecraft:shssd" => ([5, 11, 8], [1, 7, 0]),
            "minecraft:shstart" | "minecraft:shsd" => ([5, 11, 5], [1, 7, 0]),
            "minecraft:sh5c" => ([10, 9, 11], [4, 3, 0]),
            "minecraft:shli" => ([14, if self.flag("Tall") { 11 } else { 6 }, 15], [4, 1, 0]),
            "minecraft:shpr" => {
                self.portal(rng);
                return;
            }
            "minecraft:shfc" => {
                self.filler();
                return;
            }
            _ => return,
        };
        self.masonry([0, 0, 0], size.map(|v| v - 1), true, rng);
        self.entry(entry);
        match self.piece.kind.as_str() {
            "minecraft:shs" => {
                self.door([1, 1, 6], "OPENING");
                for (pos, facing) in [
                    ([1, 2, 1], "east"),
                    ([3, 2, 1], "west"),
                    ([1, 2, 5], "east"),
                    ([3, 2, 5], "west"),
                ] {
                    if rng.next_float() < 0.1 {
                        self.facing(pos, "wall_torch", facing);
                    }
                }
                if self.flag("Left") {
                    self.fill([0, 1, 2], [0, 3, 4], air);
                }
                if self.flag("Right") {
                    self.fill([4, 1, 2], [4, 3, 4], air);
                }
            }
            "minecraft:shcc" => {
                self.door([1, 1, 6], "OPENING");
                self.fill([3, 1, 2], [3, 1, 4], stone);
                for pos in [
                    [3, 1, 1],
                    [3, 1, 5],
                    [3, 2, 2],
                    [3, 2, 4],
                    [2, 1, 2],
                    [2, 1, 3],
                    [2, 1, 4],
                ] {
                    self.block(pos, state("stone_brick_slab"));
                }
                if !self.flag("Chest") && self.contains(self.position([3, 2, 3])) {
                    self.piece.save_data.insert("Chest", 1_i8);
                    self.chest([3, 2, 3], "corridor", rng, outside);
                }
            }
            "minecraft:shlt" | "minecraft:shrt" => {
                let north_east = matches!(
                    self.piece.orientation,
                    Some(Direction::North | Direction::East)
                );
                let left = self.piece.kind == "minecraft:shlt";
                let x = if north_east == left { 0 } else { 4 };
                self.fill([x, 1, 1], [x, 3, 3], air);
            }
            "minecraft:shph" => self.prison(rng),
            "minecraft:shssd" => {
                self.door([1, 1, 7], "OPENING");
                for z in 1..=6 {
                    for x in 1..=3 {
                        self.facing([x, 7 - z, z], "cobblestone_stairs", "north");
                    }
                    if z < 6 {
                        self.fill([1, 6 - z, z], [3, 6 - z, z], stone);
                    }
                }
            }
            "minecraft:shstart" | "minecraft:shsd" => {
                self.door([1, 1, 4], "OPENING");
                for pos in [
                    [2, 6, 1],
                    [1, 5, 1],
                    [1, 5, 2],
                    [1, 4, 3],
                    [2, 4, 3],
                    [3, 3, 3],
                    [3, 3, 2],
                    [3, 2, 1],
                    [2, 2, 1],
                    [1, 1, 1],
                    [1, 1, 2],
                ] {
                    self.block(pos, stone);
                }
                for pos in [
                    [1, 6, 1],
                    [1, 5, 3],
                    [3, 4, 3],
                    [3, 3, 1],
                    [1, 2, 1],
                    [1, 1, 3],
                ] {
                    self.block(pos, slab);
                }
            }
            "minecraft:shrc" => self.crossing(rng, outside),
            "minecraft:sh5c" => self.five_crossing(rng),
            "minecraft:shli" => self.library(rng, outside),
            _ => unreachable!(),
        }
    }

    fn prison(&mut self, rng: &mut impl RandomSource) {
        self.fill([1, 1, 10], [3, 3, 10], state("cave_air"));
        for z in [1, 3, 7, 9] {
            self.masonry([4, 1, z], [4, 3, z], false, rng);
        }
        let ns = self.oriented("iron_bars", &[("north", "true"), ("south", "true")]);
        let ew = self.oriented("iron_bars", &[("east", "true"), ("west", "true")]);
        let tee = self.oriented(
            "iron_bars",
            &[("east", "true"), ("north", "true"), ("south", "true")],
        );
        for y in 1..=3 {
            self.block([4, y, 4], ns);
            self.block([4, y, 5], tee);
            self.block([4, y, 6], ns);
            self.fill([5, y, 5], [7, y, 5], ew);
        }
        for z in [2, 8] {
            self.block([4, 3, z], ns);
            for (y, half) in [(1, "lower"), (2, "upper")] {
                self.block(
                    [4, y, z],
                    self.oriented(
                        "iron_door",
                        &[("facing", "west"), ("hinge", "right"), ("half", half)],
                    ),
                );
            }
        }
    }

    fn filler(&mut self) {
        let length = match self.piece.save_data.get("Steps") {
            Some(Nbt::Int(n)) => *n,
            _ => 0,
        };
        for z in 0..length {
            self.fill([0, 0, z], [4, 0, z], state("stone_bricks"));
            self.fill([0, 1, z], [0, 3, z], state("stone_bricks"));
            self.fill([4, 1, z], [4, 3, z], state("stone_bricks"));
            self.fill([1, 1, z], [3, 3, z], state("cave_air"));
            self.fill([0, 4, z], [4, 4, z], state("stone_bricks"));
        }
    }

    fn five_crossing(&mut self, rng: &mut impl RandomSource) {
        let air = state("cave_air");
        let slab = state("smooth_stone_slab");
        for (flag, x, y, z) in [
            ("leftLow", 0, 3, 1),
            ("rightLow", 9, 3, 1),
            ("leftHigh", 0, 5, 7),
            ("rightHigh", 9, 5, 7),
        ] {
            if self.flag(flag) {
                self.fill([x, y, z], [x, y + 2, z + 2], air);
            }
        }
        self.fill([5, 1, 10], [7, 3, 10], air);
        for (min, max) in [
            ([1, 2, 1], [8, 2, 6]),
            ([4, 1, 5], [4, 4, 9]),
            ([8, 1, 5], [8, 4, 9]),
            ([1, 4, 7], [3, 4, 9]),
            ([1, 3, 5], [3, 3, 6]),
        ] {
            self.masonry(min, max, false, rng);
        }
        self.fill([1, 3, 4], [3, 3, 4], slab);
        self.fill([1, 4, 6], [3, 4, 6], slab);
        self.masonry([5, 1, 7], [7, 1, 8], false, rng);
        for (min, max) in [
            ([5, 1, 9], [7, 1, 9]),
            ([5, 2, 7], [7, 2, 7]),
            ([4, 5, 7], [4, 5, 9]),
            ([8, 5, 7], [8, 5, 9]),
        ] {
            self.fill(min, max, slab);
        }
        self.fill(
            [5, 5, 7],
            [7, 5, 9],
            block_states()
                .state("minecraft:smooth_stone_slab", [("type", "double")])
                .unwrap()
                .0,
        );
        self.facing([6, 5, 6], "wall_torch", "north");
    }

    fn crossing(
        &mut self,
        rng: &mut impl RandomSource,
        outside: &mut impl FnMut(i32, i32, i32) -> u32,
    ) {
        let air = state("cave_air");
        let stone = state("stone_bricks");
        for (min, max) in [
            ([4, 1, 10], [6, 3, 10]),
            ([0, 1, 4], [0, 3, 6]),
            ([10, 1, 4], [10, 3, 6]),
        ] {
            self.fill(min, max, air);
        }
        let variant = match self.piece.save_data.get("Type") {
            Some(Nbt::Int(n)) => *n,
            _ => 0,
        };
        match variant {
            0 => {
                self.fill([5, 1, 5], [5, 3, 5], stone);
                for (pos, dir) in [
                    ([4, 3, 5], "west"),
                    ([6, 3, 5], "east"),
                    ([5, 3, 4], "north"),
                    ([5, 3, 6], "south"),
                ] {
                    self.facing(pos, "wall_torch", dir);
                }
                for x in 4..=6 {
                    for z in 4..=6 {
                        if x != 5 || z != 5 {
                            self.block([x, 1, z], state("smooth_stone_slab"));
                        }
                    }
                }
            }
            1 => {
                for i in 3..=7 {
                    for pos in [[3, 1, i], [7, 1, i], [i, 1, 3], [i, 1, 7]] {
                        self.block(pos, stone);
                    }
                }
                self.fill([5, 1, 5], [5, 3, 5], stone);
                self.block([5, 4, 5], state("water"));
            }
            2 => self.storage_crossing(rng, outside),
            _ => {}
        }
    }

    fn storage_crossing(
        &mut self,
        rng: &mut impl RandomSource,
        outside: &mut impl FnMut(i32, i32, i32) -> u32,
    ) {
        // The balcony and its supports are filled below after the common crossing shell.
        for z in [1, 9] {
            self.fill([1, 3, z], [9, 3, z], state("cobblestone"));
        }
        for x in [1, 9] {
            self.fill([x, 3, 1], [x, 3, 9], state("cobblestone"));
        }
        for pos in [
            [5, 1, 4],
            [5, 1, 6],
            [5, 3, 4],
            [5, 3, 6],
            [4, 1, 5],
            [6, 1, 5],
            [4, 3, 5],
            [6, 3, 5],
        ] {
            self.block(pos, state("cobblestone"));
        }
        for x in [4, 6] {
            for z in [4, 6] {
                self.fill([x, 1, z], [x, 3, z], state("cobblestone"));
            }
        }
        self.facing([5, 3, 5], "wall_torch", "south");
        for z in 2..=8 {
            for x in 2..=8 {
                if x <= 3 || x >= 7 || z <= 3 || z >= 7 {
                    self.block([x, 3, z], state("oak_planks"));
                }
            }
        }
        for y in 1..=3 {
            self.facing([9, y, 3], "ladder", "west");
        }
        self.chest([3, 4, 8], "crossing", rng, outside);
    }

    fn library(
        &mut self,
        rng: &mut impl RandomSource,
        outside: &mut impl FnMut(i32, i32, i32) -> u32,
    ) {
        for y in 1..=4 {
            for x in 2..=11 {
                for z in 1..=13 {
                    if rng.next_float() < 0.07 {
                        self.block([x, y, z], state("cobweb"));
                    }
                }
            }
        }
        let tall = self.flag("Tall");
        for z in 1..=13 {
            let support = (z - 1) % 4 == 0;
            let material = state(if support { "oak_planks" } else { "bookshelf" });
            for x in [1, 12] {
                self.fill([x, 1, z], [x, 4, z], material);
            }
            if support {
                self.facing([2, 3, z], "wall_torch", "east");
                self.facing([11, 3, z], "wall_torch", "west");
            }
            if tall {
                for x in [1, 12] {
                    self.fill([x, 6, z], [x, 9, z], material);
                }
            }
        }
        for z in (3..=11).step_by(2) {
            for x in [3, 6, 9] {
                self.fill([x, 1, z], [x + 1, 3, z], state("bookshelf"));
            }
        }
        if tall {
            self.library_balcony();
        }
        self.chest([3, 3, 5], "library", rng, outside);
        if tall {
            self.block([12, 9, 1], state("cave_air"));
            self.chest([12, 8, 1], "library", rng, outside);
        }
    }

    fn fence(&mut self, pos: [i32; 3], connections: &[&str]) {
        let properties: Vec<_> = connections.iter().map(|&s| (s, "true")).collect();
        self.block(pos, self.oriented("oak_fence", &properties));
    }

    fn library_balcony(&mut self) {
        for (min, max) in [
            ([1, 5, 1], [3, 5, 13]),
            ([10, 5, 1], [12, 5, 13]),
            ([4, 5, 1], [9, 5, 2]),
            ([4, 5, 12], [9, 5, 13]),
        ] {
            self.fill(min, max, state("oak_planks"));
        }
        for pos in [[9, 5, 11], [8, 5, 11], [9, 5, 10]] {
            self.block(pos, state("oak_planks"));
        }
        for z in 3..=11 {
            self.fence([3, 6, z], &["north", "south"]);
        }
        for z in 3..=9 {
            self.fence([10, 6, z], &["north", "south"]);
        }
        for x in 4..=9 {
            self.fence([x, 6, 2], &["east", "west"]);
        }
        for x in 4..=7 {
            self.fence([x, 6, 12], &["east", "west"]);
        }
        self.fence([3, 6, 2], &["east", "south"]);
        self.fence([3, 6, 12], &["east", "north"]);
        self.fence([10, 6, 2], &["west", "south"]);
        for i in 0..=2 {
            self.fence([8 + i, 6, 12 - i], &["north", "west"]);
            if i < 2 {
                self.fence([8 + i, 6, 11 - i], &["south", "east"]);
            }
        }
        for y in 1..=7 {
            self.facing([10, y, 13], "ladder", "north");
        }
        for y in [9, 8] {
            self.fence([6, y, 7], &["east"]);
            self.fence([7, y, 7], &["west"]);
        }
        for x in [6, 7] {
            self.fence([x, 7, 7], &["north", "east", "south", "west"]);
        }
        self.fence([5, 7, 7], &["east"]);
        self.fence([8, 7, 7], &["west"]);
        for x in [6, 7] {
            self.fence(
                [x, 7, 6],
                if x == 6 {
                    &["east", "south"]
                } else {
                    &["west", "south"]
                },
            );
            self.fence(
                [x, 7, 8],
                if x == 6 {
                    &["east", "north"]
                } else {
                    &["west", "north"]
                },
            );
        }
        for pos in [
            [5, 8, 7],
            [8, 8, 7],
            [6, 8, 6],
            [6, 8, 8],
            [7, 8, 6],
            [7, 8, 8],
        ] {
            self.block(pos, state("torch"));
        }
    }

    fn portal(&mut self, rng: &mut impl RandomSource) {
        self.masonry([0, 0, 0], [10, 7, 15], false, rng);
        self.door([4, 1, 0], "GRATES");
        for (min, max) in [
            ([1, 6, 1], [1, 6, 14]),
            ([9, 6, 1], [9, 6, 14]),
            ([2, 6, 1], [8, 6, 2]),
            ([2, 6, 14], [8, 6, 14]),
            ([1, 1, 1], [2, 1, 4]),
            ([8, 1, 1], [9, 1, 4]),
        ] {
            self.masonry(min, max, false, rng);
        }
        for x in [1, 9] {
            self.fill([x, 1, 1], [x, 1, 3], state("lava"));
        }
        self.masonry([3, 1, 8], [7, 1, 12], false, rng);
        self.fill([4, 1, 9], [6, 1, 11], state("lava"));
        let ns = self.oriented("iron_bars", &[("north", "true"), ("south", "true")]);
        let ew = self.oriented("iron_bars", &[("east", "true"), ("west", "true")]);
        for z in (3..=13).step_by(2) {
            for x in [0, 10] {
                self.fill([x, 3, z], [x, 4, z], ns);
            }
        }
        for x in (2..=8).step_by(2) {
            self.fill([x, 3, 15], [x, 4, 15], ew);
        }
        for (min, max) in [
            ([4, 1, 5], [6, 1, 7]),
            ([4, 2, 6], [6, 2, 7]),
            ([4, 3, 7], [6, 3, 7]),
        ] {
            self.masonry(min, max, false, rng);
        }
        for x in 4..=6 {
            for y in 1..=3 {
                self.facing([x, y, 3 + y], "stone_brick_stairs", "south");
            }
        }
        let eyes: [bool; 12] = std::array::from_fn(|_| rng.next_float() > 0.9);
        let frames = [
            ([4, 3, 8], "south"),
            ([5, 3, 8], "south"),
            ([6, 3, 8], "south"),
            ([4, 3, 12], "north"),
            ([5, 3, 12], "north"),
            ([6, 3, 12], "north"),
            ([3, 3, 9], "east"),
            ([3, 3, 10], "east"),
            ([3, 3, 11], "east"),
            ([7, 3, 9], "west"),
            ([7, 3, 10], "west"),
            ([7, 3, 11], "west"),
        ];
        for ((pos, dir), eye) in frames.into_iter().zip(eyes) {
            self.block(
                pos,
                self.oriented(
                    "end_portal_frame",
                    &[("facing", dir), ("eye", if eye { "true" } else { "false" })],
                ),
            );
        }
        if eyes.into_iter().all(|v| v) {
            self.fill([4, 3, 9], [6, 3, 11], state("end_portal"));
        }
        if !self.flag("Mob") && self.contains(self.position([5, 3, 6])) {
            self.piece.save_data.insert("Mob", 1_i8);
            self.block([5, 3, 6], state("spawner"));
            let [x, y, z] = self.position([5, 3, 6]);
            let mut tag = Compound::new();
            tag.insert("id", "minecraft:mob_spawner");
            tag.insert("x", x);
            tag.insert("y", y);
            tag.insert("z", z);
            tag.insert("components", Compound::new());
            for (key, value) in [
                ("Delay", 20_i16),
                ("MaxNearbyEntities", 6),
                ("MaxSpawnDelay", 800),
                ("MinSpawnDelay", 200),
                ("RequiredPlayerRange", 16),
                ("SpawnCount", 4),
                ("SpawnRange", 4),
            ] {
                tag.insert(key, value);
            }
            let mut entity = Compound::new();
            entity.insert("id", "minecraft:silverfish");
            let mut spawn = Compound::new();
            spawn.insert("entity", entity);
            tag.insert("SpawnData", spawn);
            tag.insert("SpawnPotentials", List::Empty);
            self.entities.push(tag);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harpas_core::random::{AnyRandom, PositionalRandom};

    struct FixedFloat(f32, usize);
    impl RandomSource for FixedFloat {
        fn next_float(&mut self) -> f32 {
            self.1 += 1;
            self.0
        }
        fn fork_source(&mut self) -> AnyRandom {
            unreachable!()
        }
        fn fork_positional_source(&mut self) -> PositionalRandom {
            unreachable!()
        }
        fn set_seed(&mut self, _: i64) {
            unreachable!()
        }
        fn next_int(&mut self) -> i32 {
            unreachable!()
        }
        fn next_int_bounded(&mut self, _: i32) -> i32 {
            unreachable!()
        }
        fn next_long(&mut self) -> i64 {
            unreachable!()
        }
        fn next_bool(&mut self) -> bool {
            unreachable!()
        }
        fn next_double(&mut self) -> f64 {
            unreachable!()
        }
        fn next_gaussian(&mut self) -> f64 {
            unreachable!()
        }
    }

    fn portal() -> StructurePiece {
        let mut save_data = Compound::new();
        save_data.insert("EntryDoor", "OPENING");
        save_data.insert("Mob", 0_i8);
        StructurePiece {
            kind: "minecraft:shpr".into(),
            bounding_box: super::super::BoundingBox {
                min: [0, 50, 0],
                max: [10, 57, 15],
            },
            orientation: Some(Direction::South),
            gen_depth: 0,
            junctions: Vec::new(),
            jigsaw: None,
            template: None,
            save_data,
        }
    }

    #[test]
    fn all_twelve_eyes_generate_the_portal_and_a_disjoint_piece_consumes_no_draws() {
        for probability in [0.9, 0.95] {
            let mut piece = portal();
            let mut random = FixedFloat(probability, 0);
            let mut other = GeneratedChunk::new(1, 0, -64, 384, 1);
            assert!(
                place(
                    &mut piece,
                    &mut other,
                    &mut random,
                    &mut |_, _, _| unreachable!()
                )
                .is_empty()
            );
            assert_eq!(random.1, 0);
            assert_eq!(piece.save_data.get("Mob"), Some(&Nbt::Byte(0)));
            let mut chunk = GeneratedChunk::new(0, 0, -64, 384, 1);
            let entities = place(
                &mut piece,
                &mut chunk,
                &mut random,
                &mut |_, _, _| unreachable!(),
            );
            assert_eq!(entities.len(), 1);
            for x in 4..=6 {
                for z in 9..=11 {
                    assert_eq!(
                        block_states()
                            .properties(chunk.block_state(x, 53, z))
                            .unwrap()
                            .0,
                        if probability > 0.9 {
                            "minecraft:end_portal"
                        } else {
                            "minecraft:cave_air"
                        }
                    );
                }
            }
        }
    }
}
