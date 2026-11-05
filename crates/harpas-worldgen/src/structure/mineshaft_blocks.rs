//! Mineshaft blocks, reconstructed from observations of vanilla's piece placement APIs.

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_core::random::{RandomSource, WorldgenRandom, XoroshiroRandom};

use super::{BoundingBox, Direction, StructurePiece, StructureStart};
use crate::{GeneratedChunk, HeightmapType};

impl StructureStart {
    /// Places a single mineshaft start and returns its new block entities. Use [`place_mineshafts_in_chunk`]
    /// when multiple starts share a chunk. The predicate reads the zoomed biome at world coordinates and
    /// tests membership of `minecraft:mineshaft_blocking`, including biomes in saved neighboring chunks.
    pub fn place_mineshaft_in_chunk(
        &mut self,
        chunk: &mut GeneratedChunk,
        seed: i64,
        blocking_biome: &mut impl FnMut(i32, i32, i32) -> bool,
    ) -> Vec<Compound> {
        place_mineshafts_in_chunk([self], chunk, seed, blocking_biome)
    }
    /// The world positions whose zoomed biomes each intersecting mineshaft piece checks before placement.
    /// Workers can resolve them against saved neighbors without generating neighboring terrain.
    pub fn mineshaft_biome_samples<'a>(
        &'a self,
        chunk: &'a GeneratedChunk,
    ) -> impl Iterator<Item = [i32; 3]> + 'a {
        let enabled = matches!(
            self.structure.as_str(),
            "minecraft:mineshaft" | "minecraft:mineshaft_mesa"
        );
        self.pieces
            .iter()
            .filter(move |_| enabled)
            .filter_map(move |p| clipped_bounds(p, chunk).map(biome_position))
    }
}

/// Places mineshafts in a new chunk's vanilla reference-set order, sharing each structure type's random
/// stream across all starts. Call once per chunk, with every intersecting normal and mesa mineshaft.
pub fn place_mineshafts_in_chunk<'a>(
    starts: impl IntoIterator<Item = &'a mut StructureStart>,
    chunk: &mut GeneratedChunk,
    seed: i64,
    blocking_biome: &mut impl FnMut(i32, i32, i32) -> bool,
) -> Vec<Compound> {
    let mut starts: Vec<_> = starts
        .into_iter()
        .filter(|s| {
            matches!(
                s.structure.as_str(),
                "minecraft:mineshaft" | "minecraft:mineshaft_mesa"
            )
        })
        .collect();
    let mut entities = Vec::new();
    for (structure, index) in [("minecraft:mineshaft", 1), ("minecraft:mineshaft_mesa", 2)] {
        let mut matching: Vec<_> = starts
            .iter_mut()
            .filter(|s| s.structure == structure)
            .collect();
        matching.sort_by_key(|s| s.chunk);
        let order = reference_order(matching.iter().map(|s| s.chunk));
        matching.sort_by_key(|s| {
            order
                .iter()
                .position(|p| *p == s.chunk)
                .expect("referenced start")
        });
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(0));
        let decoration =
            random.set_decoration_seed(seed, chunk.chunk_x() * 16, chunk.chunk_z() * 16);
        random.set_feature_seed(decoration, index, 3);
        for start in matching {
            for piece in &mut start.pieces {
                if clipped_bounds(piece, chunk).is_none() {
                    continue;
                }
                let mut mine = Mine {
                    piece,
                    chunk,
                    entities: Vec::new(),
                };
                if mine.invalid(blocking_biome) {
                    continue;
                }
                match mine.piece.kind.as_str() {
                    "minecraft:mscorridor" => mine.corridor(&mut random),
                    "minecraft:msroom" => mine.room(),
                    "minecraft:mscrossing" => mine.crossing(),
                    "minecraft:msstairs" => mine.stairs(),
                    _ => unreachable!(),
                }
                entities.extend(mine.entities);
            }
        }
    }
    // Another intersecting mine can excavate an earlier spawner or replace it at the same position.
    let mut seen = std::collections::HashSet::new();
    entities.retain(|entity| {
        let [x, y, z] = ["x", "y", "z"].map(|key| match entity.get(key) {
            Some(Nbt::Int(n)) => *n,
            _ => unreachable!("generated spawner position"),
        });
        chunk.block_state((x & 15) as usize, y, (z & 15) as usize) == state("spawner")
            && seen.insert([x, y, z])
    });
    entities
}

// The iteration order of a chunk's reference set: zero first, then occupied hash slots backwards.
// Insertion follows the structure-reference scan (source X, then Z). Rehashing preserves linear probing.
fn reference_order(positions: impl IntoIterator<Item = (i32, i32)>) -> Vec<(i32, i32)> {
    fn insert(table: &mut [i64], value: i64) {
        let mut hash = (value as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        hash ^= hash >> 32;
        hash ^= hash >> 16;
        let mut slot = hash as usize & (table.len() - 1);
        while table[slot] != 0 {
            slot = (slot + 1) & (table.len() - 1);
        }
        table[slot] = value;
    }
    let mut table = vec![0; 32];
    let mut zero = false;
    let mut size = 0;
    for (x, z) in positions {
        let packed = i64::from(x as u32) | (i64::from(z as u32) << 32);
        if packed == 0 {
            zero = true;
        } else {
            insert(&mut table, packed);
        }
        size += 1;
        if size > table.len() * 3 / 4 {
            let old = std::mem::replace(&mut table, vec![0; size.next_power_of_two() * 2]);
            for value in old.into_iter().rev().filter(|v| *v != 0) {
                insert(&mut table, value);
            }
        }
    }
    let mut result = Vec::new();
    if zero {
        result.push((0, 0));
    }
    result.extend(
        table
            .into_iter()
            .rev()
            .filter(|v| *v != 0)
            .map(|v| (v as i32, (v >> 32) as i32)),
    );
    result
}

fn clipped_bounds(piece: &StructurePiece, chunk: &GeneratedChunk) -> Option<BoundingBox> {
    let b = piece.bounding_box;
    let (cx, cz) = (chunk.chunk_x() * 16, chunk.chunk_z() * 16);
    if !matches!(
        piece.kind.as_str(),
        "minecraft:msroom" | "minecraft:mscrossing" | "minecraft:msstairs" | "minecraft:mscorridor"
    ) || !b.intersects_columns(cx, cz, cx + 15, cz + 15)
        || b.max[1] < chunk.min_y()
        || b.min[1] >= chunk.min_y() + chunk.height()
    {
        return None;
    }
    Some(BoundingBox {
        min: [
            (b.min[0] - 1).max(cx),
            (b.min[1] - 1).max(chunk.min_y()),
            (b.min[2] - 1).max(cz),
        ],
        max: [
            (b.max[0] + 1).min(cx + 15),
            (b.max[1] + 1).min(chunk.min_y() + chunk.height() - 1),
            (b.max[2] + 1).min(cz + 15),
        ],
    })
}

fn biome_position(b: BoundingBox) -> [i32; 3] {
    std::array::from_fn(|i| (b.min[i] + b.max[i]) / 2)
}

fn state(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}

// The terrain and structure shapes which the mineshaft's support checks can encounter. Supporting a
// floor and being solid-rendered are different: a top slab or glass supports a floor but not these rails.
fn full_cube(block: u32) -> bool {
    let (name, props) = block_states().properties(block).unwrap();
    HeightmapType::OceanFloor.matches(block)
        && !name.ends_with("_stairs")
        && (!name.ends_with("_slab") || props.contains(&("type", "double")))
        && !name.ends_with("_fence")
        && !name.ends_with("_wall")
        && !name.ends_with("_leaves")
        && !name.ends_with("_trapdoor")
        && !name.ends_with("_door")
        && !matches!(
            name,
            "minecraft:chest"
                | "minecraft:trapped_chest"
                | "minecraft:ender_chest"
                | "minecraft:iron_bars"
                | "minecraft:iron_chain"
                | "minecraft:farmland"
                | "minecraft:dirt_path"
                | "minecraft:lantern"
                | "minecraft:pointed_dripstone"
                | "minecraft:soul_sand"
        )
}

fn sturdy_vertical(block: u32, up: bool) -> bool {
    if full_cube(block) {
        return true;
    }
    let (name, props) = block_states().properties(block).unwrap();
    if name.ends_with("_slab") {
        return props.contains(&("type", if up { "top" } else { "bottom" }));
    }
    if name.ends_with("_stairs") {
        return props.contains(&("half", if up { "top" } else { "bottom" }));
    }
    if name.ends_with("_trapdoor") {
        return props.contains(&("open", "false"))
            && props.contains(&("half", if up { "top" } else { "bottom" }));
    }
    false
}

fn solid_render(block: u32) -> bool {
    let name = block_states().properties(block).unwrap().0;
    full_cube(block)
        && !name.ends_with("glass")
        && !matches!(
            name,
            "minecraft:ice"
                | "minecraft:frosted_ice"
                | "minecraft:slime_block"
                | "minecraft:honey_block"
        )
}

struct Mine<'a> {
    piece: &'a mut StructurePiece,
    chunk: &'a mut GeneratedChunk,
    entities: Vec<Compound>,
}

impl Mine<'_> {
    fn position(&self, [x, y, z]: [i32; 3]) -> [i32; 3] {
        let b = self.piece.bounding_box;
        let (x, z) = match self.piece.orientation {
            Some(Direction::North) => (b.min[0] + x, b.max[2] - z),
            Some(Direction::South) => (b.min[0] + x, b.min[2] + z),
            Some(Direction::West) => (b.max[0] - z, b.min[2] + x),
            Some(Direction::East) => (b.min[0] + z, b.min[2] + x),
            None => return [x, y, z],
        };
        [x, b.min[1] + y, z]
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
            let existing = self
                .chunk
                .block_state((x & 15) as usize, y, (z & 15) as usize);
            let name = block_states().properties(existing).unwrap().0;
            let protected = if self.piece.save_data.get("MST") == Some(&Nbt::Int(1)) {
                matches!(
                    name,
                    "minecraft:dark_oak_planks"
                        | "minecraft:dark_oak_log"
                        | "minecraft:dark_oak_fence"
                )
            } else {
                matches!(
                    name,
                    "minecraft:oak_planks" | "minecraft:oak_log" | "minecraft:oak_fence"
                )
            };
            if protected {
                return;
            }
            self.chunk
                .set_block_state((x & 15) as usize, y, (z & 15) as usize, block);
            if block_states()
                .properties(block)
                .unwrap()
                .0
                .ends_with("_fence")
                || block_states().properties(block).unwrap().0 == "minecraft:wall_torch"
            {
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

    fn invalid(&self, blocking_biome: &mut impl FnMut(i32, i32, i32) -> bool) -> bool {
        let bounds = clipped_bounds(self.piece, self.chunk).expect("intersecting mineshaft piece");
        let (min, max) = (bounds.min, bounds.max);
        let [x, y, z] = biome_position(bounds);
        if blocking_biome(x, y, z) {
            return true;
        }
        let liquid = |x: i32, y: i32, z: i32| {
            matches!(
                block_states()
                    .properties(
                        self.chunk
                            .block_state((x & 15) as usize, y, (z & 15) as usize)
                    )
                    .unwrap()
                    .0,
                "minecraft:water" | "minecraft:lava"
            )
        };
        for x in min[0]..=max[0] {
            for z in min[2]..=max[2] {
                if liquid(x, min[1], z) || liquid(x, max[1], z) {
                    return true;
                }
            }
        }
        for x in min[0]..=max[0] {
            for y in min[1]..=max[1] {
                if liquid(x, y, min[2]) || liquid(x, y, max[2]) {
                    return true;
                }
            }
        }
        for z in min[2]..=max[2] {
            for y in min[1]..=max[1] {
                if liquid(min[0], y, z) || liquid(max[0], y, z) {
                    return true;
                }
            }
        }
        false
    }

    fn planks(&self) -> u32 {
        state(if self.piece.save_data.get("MST") == Some(&Nbt::Int(1)) {
            "dark_oak_planks"
        } else {
            "oak_planks"
        })
    }

    fn room(&mut self) {
        let b = self.piece.bounding_box;
        self.fill(
            [b.min[0], b.min[1] + 1, b.min[2]],
            [b.max[0], (b.min[1] + 3).min(b.max[1]), b.max[2]],
            state("cave_air"),
        );
        if let Some(Nbt::List(List::IntArray(entrances))) =
            self.piece.save_data.get("Entrances").cloned()
        {
            for bounds in entrances {
                if let [x0, _, z0, x1, y1, z1] = bounds.as_slice() {
                    self.fill([*x0, *y1 - 2, *z0], [*x1, *y1, *z1], state("cave_air"));
                }
            }
        }
        let min_y = b.min[1] + 4;
        let spans = [
            b.max[0] - b.min[0] + 1,
            b.max[1] - min_y + 1,
            b.max[2] - b.min[2] + 1,
        ]
        .map(|n| n as f32);
        for y in min_y..=b.max[1] {
            let dy = (y - min_y) as f32 / spans[1];
            for x in b.min[0]..=b.max[0] {
                let dx = (x - b.min[0]) as f32 / (spans[0] * 0.5) - 1.0;
                for z in b.min[2]..=b.max[2] {
                    let dz = (z - b.min[2]) as f32 / (spans[2] * 0.5) - 1.0;
                    if dx * dx + dy * dy + dz * dz <= 1.05 {
                        self.block([x, y, z], state("cave_air"));
                    }
                }
            }
        }
    }

    fn crossing(&mut self) {
        let b = self.piece.bounding_box;
        let cave_air = state("cave_air");
        for (low, high) in if self.piece.save_data.get("tf") == Some(&Nbt::Byte(1)) {
            vec![(b.min[1], b.min[1] + 2), (b.max[1] - 2, b.max[1])]
        } else {
            vec![(b.min[1], b.max[1])]
        } {
            self.fill(
                [b.min[0] + 1, low, b.min[2]],
                [b.max[0] - 1, high, b.max[2]],
                cave_air,
            );
            self.fill(
                [b.min[0], low, b.min[2] + 1],
                [b.max[0], high, b.max[2] - 1],
                cave_air,
            );
        }
        if self.piece.save_data.get("tf") == Some(&Nbt::Byte(1)) {
            self.fill(
                [b.min[0] + 1, b.min[1] + 3, b.min[2] + 1],
                [b.max[0] - 1, b.min[1] + 3, b.max[2] - 1],
                cave_air,
            );
        }
        for x in [b.min[0] + 1, b.max[0] - 1] {
            for z in [b.min[2] + 1, b.max[2] - 1] {
                if !block_states().is_air(self.read([x, b.max[1] + 1, z])) {
                    self.fill([x, b.min[1], z], [x, b.max[1], z], self.planks());
                }
            }
        }
        for x in b.min[0]..=b.max[0] {
            for z in b.min[2]..=b.max[2] {
                let pos = [x, b.min[1] - 1, z];
                if self.interior(pos) && !sturdy_vertical(self.read(pos), true) {
                    self.raw_block(pos, self.planks());
                }
            }
        }
    }

    fn interior(&self, [x, y, z]: [i32; 3]) -> bool {
        // Vanilla tests the block above the candidate, including that block's clipping bounds.
        let [x, y, z] = self.position([x, y + 1, z]);
        self.contains([x, y, z])
            && y < self.chunk.heightmaps().get(
                HeightmapType::OceanFloorWg,
                (x & 15) as usize,
                (z & 15) as usize,
            )
    }

    fn direction(&self, value: &str) -> &'static str {
        let i = match value {
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
        })[i]
    }

    fn support(&mut self, z: i32, random: &mut impl RandomSource) {
        if (0..=2).any(|x| block_states().is_air(self.read([x, 3, z]))) {
            return;
        }
        let name = if self.piece.save_data.get("MST") == Some(&Nbt::Int(1)) {
            "minecraft:dark_oak_fence"
        } else {
            "minecraft:oak_fence"
        };
        for (x, facing) in [(0, "west"), (2, "east")] {
            let fence = block_states()
                .state(name, [(self.direction(facing), "true")])
                .unwrap()
                .0;
            self.fill([x, 0, z], [x, 1, z], fence);
        }
        if random.next_int_bounded(4) == 0 {
            self.block([0, 2, z], self.planks());
            self.block([2, 2, z], self.planks());
        } else {
            self.fill([0, 2, z], [2, 2, z], self.planks());
            for (offset, facing) in [(-1, "north"), (1, "south")] {
                if random.next_float() < 0.05 {
                    let torch = block_states()
                        .state("minecraft:wall_torch", [("facing", self.direction(facing))])
                        .unwrap()
                        .0;
                    self.block([1, 2, z + offset], torch);
                }
            }
        }
    }

    fn cobweb(&mut self, pos: [i32; 3], probability: f32, random: &mut impl RandomSource) {
        if !self.interior(pos) || random.next_float() >= probability {
            return;
        }
        let [x, y, z] = pos;
        let count = [
            [0, -1, 0],
            [0, 1, 0],
            [0, 0, -1],
            [0, 0, 1],
            [-1, 0, 0],
            [1, 0, 0],
        ]
        .into_iter()
        .filter(|d| HeightmapType::OceanFloor.matches(self.read([x + d[0], y + d[1], z + d[2]])))
        .count();
        if count >= 2 {
            self.block(pos, state("cobweb"));
        }
    }

    // The support searches alternate below and above the plank, choosing the first usable anchor.
    // These writes bypass the piece's protected-wood predicate, as vanilla's column helpers do.
    fn anchor(&mut self, x: i32, z: i32) {
        if self.read([x, -1, z]) != self.planks() {
            return;
        }
        let mut below = true;
        let mut above = true;
        for distance in 1..=51 {
            if below && distance <= 21 {
                let pos = [x, -1 - distance, z];
                let world_y = self.position(pos)[1];
                let block = self.read(pos);
                if world_y <= self.chunk.min_y() {
                    below = false;
                } else if HeightmapType::OceanFloor.matches(block) {
                    for y in (-distance)..=-2 {
                        self.raw_block([x, y, z], self.log());
                    }
                    return;
                }
            }
            if above {
                let pos = [x, -1 + distance, z];
                let world_y = self.position(pos)[1];
                let block = self.read(pos);
                if world_y >= self.chunk.min_y() + self.chunk.height() - 1 {
                    above = false;
                } else if HeightmapType::OceanFloor.matches(block) {
                    let fence = if self.piece.save_data.get("MST") == Some(&Nbt::Int(1)) {
                        "dark_oak_fence"
                    } else {
                        "oak_fence"
                    };
                    self.raw_block([x, 0, z], state(fence));
                    for y in 1..distance - 1 {
                        self.raw_block([x, y, z], state("iron_chain"));
                    }
                    return;
                }
            }
            if !below && !above {
                return;
            }
        }
    }

    fn log(&self) -> u32 {
        state(if self.piece.save_data.get("MST") == Some(&Nbt::Int(1)) {
            "dark_oak_log"
        } else {
            "oak_log"
        })
    }

    fn raw_block(&mut self, pos: [i32; 3], state: u32) {
        let [x, y, z] = self.position(pos);
        if self.contains([x, y, z]) {
            self.chunk
                .set_block_state((x & 15) as usize, y, (z & 15) as usize, state);
        }
    }

    fn corridor(&mut self, random: &mut impl RandomSource) {
        let Some(Nbt::Int(sections)) = self.piece.save_data.get("Num") else {
            return;
        };
        let sections = *sections;
        let far = sections * 5 - 1;
        self.fill([0, 0, 0], [2, 1, far], state("cave_air"));
        for x in 0..=2 {
            for z in 0..=far {
                if random.next_float() <= 0.8 {
                    self.block([x, 2, z], state("cave_air"));
                }
            }
        }
        let spiders = self.piece.save_data.get("sc") == Some(&Nbt::Byte(1));
        if spiders {
            for y in 0..=1 {
                for x in 0..=2 {
                    for z in 0..=far {
                        if random.next_float() <= 0.6 && self.interior([x, y, z]) {
                            self.block([x, y, z], state("cobweb"));
                        }
                    }
                }
            }
        }
        for section in 0..sections {
            let z = 2 + section * 5;
            self.support(z, random);
            for (offset, probability) in [(-1, 0.1), (1, 0.1), (-2, 0.05), (2, 0.05)] {
                for x in [0, 2] {
                    self.cobweb([x, 2, z + offset], probability, random);
                }
            }
            for (x, offset) in [(2, -1), (0, 1)] {
                if random.next_int_bounded(100) == 0 {
                    let pos = [x, 0, z + offset];
                    if self.contains(self.position(pos))
                        && block_states().is_air(self.read(pos))
                        && !block_states().is_air(self.read([x, -1, z + offset]))
                    {
                        let shape = if random.next_bool() {
                            "north_south"
                        } else {
                            "east_west"
                        };
                        let shape = if matches!(
                            self.piece.orientation,
                            Some(Direction::East | Direction::West)
                        ) {
                            if shape == "north_south" {
                                "east_west"
                            } else {
                                "north_south"
                            }
                        } else {
                            shape
                        };
                        self.block(
                            pos,
                            block_states()
                                .state("minecraft:rail", [("shape", shape)])
                                .unwrap()
                                .0,
                        );
                        random.next_long(); // The chest minecart and its loot metadata follow with entities in M4.
                    }
                }
            }
            if spiders && self.piece.save_data.get("hps") != Some(&Nbt::Byte(1)) {
                let pos = [1, 0, z - 1 + random.next_int_bounded(3)];
                if self.interior(pos) {
                    self.piece.save_data.insert("hps", 1_i8);
                    // Vanilla writes the spawner directly, independently of the protected wood test.
                    let [x, y, z] = self.position(pos);
                    self.chunk.set_block_state(
                        (x & 15) as usize,
                        y,
                        (z & 15) as usize,
                        state("spawner"),
                    );
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
                    entity.insert("id", "minecraft:cave_spider");
                    let mut spawn = Compound::new();
                    spawn.insert("entity", entity);
                    tag.insert("SpawnData", spawn);
                    tag.insert("SpawnPotentials", List::Empty);
                    self.entities.push(tag);
                }
            }
        }
        for x in 0..=2 {
            for z in 0..=far {
                if self.interior([x, -1, z]) && !sturdy_vertical(self.read([x, -1, z]), true) {
                    self.raw_block([x, -1, z], self.planks());
                }
            }
        }
        for section in 0..sections {
            for x in [0, 2] {
                self.anchor(x, 2 + section * 5);
            }
        }
        if self.piece.save_data.get("hr") == Some(&Nbt::Byte(1)) {
            let shape = if matches!(
                self.piece.orientation,
                Some(Direction::East | Direction::West)
            ) {
                "east_west"
            } else {
                "north_south"
            };
            let rail = block_states()
                .state("minecraft:rail", [("shape", shape)])
                .unwrap()
                .0;
            for z in 0..=far {
                if solid_render(self.read([1, -1, z])) {
                    let probability = if self.interior([1, 0, z]) { 0.7 } else { 0.9 };
                    if random.next_float() < probability {
                        self.block([1, 0, z], rail);
                    }
                }
            }
        }
    }

    fn stairs(&mut self) {
        let air = state("cave_air");
        self.fill([0, 5, 0], [2, 7, 1], air);
        self.fill([0, 0, 7], [2, 2, 8], air);
        for step in 0..5 {
            self.fill(
                [0, 5 - step - i32::from(step < 4), 2 + step],
                [2, 7 - step, 2 + step],
                air,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corridor() -> StructureStart {
        let bounds = BoundingBox {
            min: [3, 20, 3],
            max: [5, 22, 12],
        };
        StructureStart {
            structure: "minecraft:mineshaft".into(),
            chunk: (0, 0),
            references: 7,
            bounding_box: bounds,
            pieces: vec![StructurePiece {
                kind: "minecraft:mscorridor".into(),
                bounding_box: bounds,
                orientation: Some(Direction::South),
                gen_depth: 2,
                junctions: Vec::new(),
                jigsaw: None,
                template: None,
                save_data: [
                    ("MST", Nbt::Int(0)),
                    ("Num", Nbt::Int(2)),
                    ("hr", Nbt::Byte(0)),
                    ("sc", Nbt::Byte(1)),
                    ("hps", Nbt::Byte(0)),
                ]
                .into_iter()
                .collect(),
            }],
        }
    }

    fn terrain() -> GeneratedChunk {
        let mut chunk = GeneratedChunk::new(0, 0, -64, 384, 1);
        for y in 10..40 {
            for z in 0..16 {
                for x in 0..16 {
                    chunk.set_block_state(x, y, z, state("stone"));
                }
            }
        }
        chunk
    }

    #[test]
    fn blocked_biomes_and_liquid_shells_leave_pieces_and_flags_untouched() {
        for liquid in [None, Some("water"), Some("lava")] {
            let mut mine = corridor();
            let original = mine.clone();
            let mut chunk = terrain();
            if let Some(liquid) = liquid {
                chunk.set_block_state(2, 21, 3, state(liquid));
            }
            let maps = chunk.heightmaps().clone();
            assert!(
                mine.place_mineshaft_in_chunk(&mut chunk, 42, &mut |_, _, _| liquid.is_none())
                    .is_empty()
            );
            assert_eq!(mine, original);
            assert_eq!(chunk.heightmaps(), &maps);
            assert_eq!(chunk.block_state(4, 20, 5), state("stone"));
            assert!(chunk.fluid_postprocessing().is_empty());
        }
    }

    #[test]
    fn deferred_spawner_flag_recreates_the_spawner_on_a_carvers_replay() {
        let mut mine = corridor();
        let mut chunk = terrain();
        let first = mine.place_mineshaft_in_chunk(&mut chunk, 42, &mut |_, _, _| false);
        assert_eq!(first.len(), 1);
        assert_eq!(
            first[0].get("id"),
            Some(&Nbt::from("minecraft:mob_spawner"))
        );
        assert_eq!(mine.pieces[0].save_data.get("hps"), Some(&Nbt::Byte(1)));
        assert_eq!(mine.references, 7);
        assert!(
            mine.place_mineshaft_in_chunk(&mut chunk, 42, &mut |_, _, _| false)
                .is_empty()
        );
        mine.pieces[0].save_data.insert("hps", 0_i8);
        let recreated = mine.place_mineshaft_in_chunk(&mut chunk, 42, &mut |_, _, _| false);
        assert_eq!(recreated, first);
    }
}
