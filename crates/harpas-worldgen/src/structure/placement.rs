//! Structure block placement after carvers. Only buried treasure is implemented so far.

use harpas_core::block::block_states;
use harpas_core::nbt::Compound;
use harpas_core::random::{RandomSource, WorldgenRandom, XoroshiroRandom};

use super::{BoundingBox, StructureStart};
use crate::{GeneratedChunk, HeightmapType};

impl StructureStart {
    /// Places implemented pieces in the target chunk, updates their saved bounds, and returns newly created
    /// block entities. Other structure types are left for vanilla's later generation stages.
    pub fn place_in_chunk(&mut self, chunk: &mut GeneratedChunk, seed: i64) -> Vec<Compound> {
        if self.structure != "minecraft:buried_treasure" {
            return Vec::new();
        }
        // In 26.1 buried treasure is the first structure in UNDERGROUND_STRUCTURES (step 3). Structures
        // have separate feature seeds, so types whose placement is not implemented consume no draws here.
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(0));
        let decoration =
            random.set_decoration_seed(seed, chunk.chunk_x() * 16, chunk.chunk_z() * 16);
        random.set_feature_seed(decoration, 0, 3);
        let mut entities = Vec::new();
        for piece in &mut self.pieces {
            if piece.kind != "minecraft:btp" {
                continue;
            }
            let [x, _, z] = piece.bounding_box.min;
            if (x >> 4, z >> 4) != (chunk.chunk_x(), chunk.chunk_z()) {
                continue;
            }
            let (lx, lz) = ((x & 15) as usize, (z & 15) as usize);
            // Treasure starts at (9,9), so its one-block shell fits entirely in its source chunk.
            if !(1..15).contains(&lx) || !(1..15).contains(&lz) {
                continue;
            }
            let states = block_states();
            let sand = states.default_state("minecraft:sand").unwrap();
            let height = chunk.heightmaps().get(HeightmapType::OceanFloorWg, lx, lz);
            for y in (chunk.min_y() + 1..=height).rev() {
                let below = chunk.block_state(lx, y - 1, lz);
                let name = states.properties(below).unwrap().0;
                if !matches!(
                    name,
                    "minecraft:sandstone"
                        | "minecraft:stone"
                        | "minecraft:andesite"
                        | "minecraft:granite"
                        | "minecraft:diorite"
                ) {
                    continue;
                }
                let current = chunk.block_state(lx, y, lz);
                let fill = if empty(current) { sand } else { current };
                // Direction.values(): down, up, north, south, west, east. Earlier writes are visible to later reads.
                for [dx, dy, dz] in [
                    [0, -1, 0],
                    [0, 1, 0],
                    [0, 0, -1],
                    [0, 0, 1],
                    [-1, 0, 0],
                    [1, 0, 0],
                ] {
                    let nx = (lx as i32 + dx) as usize;
                    let nz = (lz as i32 + dz) as usize;
                    if empty(chunk.block_state(nx, y + dy, nz)) {
                        let unsupported = empty(chunk.block_state(nx, y + dy - 1, nz));
                        chunk.set_block_state(
                            nx,
                            y + dy,
                            nz,
                            if unsupported && dy != 1 { below } else { fill },
                        );
                    }
                }
                piece.bounding_box = BoundingBox {
                    min: [x, y, z],
                    max: [x, y, z],
                };
                if y < chunk.min_y() + chunk.height()
                    && states.properties(current).unwrap().0 != "minecraft:chest"
                {
                    let facing = chest_facing(chunk, lx, y, lz);
                    let chest = states
                        .state("minecraft:chest", [("facing", facing)])
                        .unwrap()
                        .0;
                    chunk.set_block_state(lx, y, lz, chest);
                    let mut entity = Compound::new();
                    entity.insert("id", "minecraft:chest");
                    entity.insert("x", x);
                    entity.insert("y", y);
                    entity.insert("z", z);
                    entity.insert("components", Compound::new());
                    entity.insert("LootTable", "minecraft:chests/buried_treasure");
                    let loot_seed = random.next_long();
                    if loot_seed != 0 {
                        entity.insert("LootTableSeed", loot_seed);
                    }
                    entities.push(entity);
                }
                break;
            }
        }
        if let Some(first) = self.pieces.first() {
            self.bounding_box = first.bounding_box;
            for piece in &self.pieces[1..] {
                self.bounding_box.encapsulate(&piece.bounding_box);
            }
        }
        entities
    }
}

fn empty(state: u32) -> bool {
    let states = block_states();
    states.is_air(state)
        || matches!(
            states.properties(state).unwrap().0,
            "minecraft:water" | "minecraft:lava"
        )
}

/// `StructurePiece.reorient`, for the natural terrain blocks present before features.
fn chest_facing(chunk: &GeneratedChunk, x: usize, y: i32, z: usize) -> &'static str {
    // North, east, south, west, in Direction.Plane.HORIZONTAL order.
    let neighbors =
        [(x, z - 1), (x + 1, z), (x, z + 1), (x - 1, z)].map(|(x, z)| chunk.block_state(x, y, z));
    let states = block_states();
    if neighbors
        .iter()
        .any(|&s| states.properties(s).unwrap().0 == "minecraft:chest")
    {
        return "north";
    }
    let solid = neighbors.map(|state| {
        let name = states.properties(state).unwrap().0;
        HeightmapType::OceanFloor.matches(state)
            && !name.ends_with("_leaves")
            && !matches!(name, "minecraft:ice" | "minecraft:chest")
    });
    let mut facing = 0;
    if solid.iter().filter(|&&s| s).count() == 1 {
        facing = (solid.iter().position(|&s| s).unwrap() + 2) % 4;
    } else {
        if solid[facing] {
            facing = (facing + 2) % 4;
        }
        if solid[facing] {
            facing = (facing + 1) % 4;
        }
        if solid[facing] {
            facing = (facing + 2) % 4;
        }
    }
    ["north", "east", "south", "west"][facing]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StructurePiece;

    fn start() -> StructureStart {
        let bounds = BoundingBox {
            min: [-23, 90, 57],
            max: [-23, 90, 57],
        };
        StructureStart {
            structure: "minecraft:buried_treasure".into(),
            chunk: (-2, 3),
            references: 7,
            bounding_box: bounds,
            pieces: vec![StructurePiece {
                kind: "minecraft:btp".into(),
                bounding_box: bounds,
                orientation: None,
                gen_depth: 0,
                junctions: Vec::new(),
                jigsaw: None,
                template: None,
                save_data: Compound::new(),
            }],
        }
    }

    #[test]
    fn treasure_descends_past_invalid_foundations_and_preserves_its_chest_on_retry() {
        let states = block_states();
        let stone = states.default_state("minecraft:stone").unwrap();
        let deepslate = states.default_state("minecraft:deepslate").unwrap();
        let mut chunk = GeneratedChunk::new(-2, 3, -64, 384, 1);
        chunk.set_block_state(9, -64, 9, stone);
        chunk.set_block_state(9, 20, 9, deepslate);
        let mut start = start();
        let entities = start.place_in_chunk(&mut chunk, 42);
        assert_eq!(entities.len(), 1);
        assert_eq!(start.bounding_box.min, [-23, -63, 57]);
        assert_eq!(start.pieces[0].bounding_box, start.bounding_box);
        assert_eq!(start.references, 7);
        assert_eq!(chunk.block_state(9, 20, 9), deepslate);
        assert_eq!(entities[0].get("y"), Some(&harpas_core::nbt::Nbt::Int(-63)));
        let chest = chunk.block_state(9, -63, 9);
        let bounds = start.bounding_box;
        let maps = chunk.heightmaps().clone();
        assert!(start.place_in_chunk(&mut chunk, 42).is_empty());
        assert_eq!(chunk.block_state(9, -63, 9), chest);
        assert_eq!(start.bounding_box, bounds);
        assert_eq!(chunk.heightmaps(), &maps);
    }

    #[test]
    fn missing_foundations_and_other_chunks_do_not_place_treasure() {
        let mut chunk = GeneratedChunk::new(-2, 3, -64, 384, 1);
        let mut start = start();
        let before = start.clone();
        assert!(start.place_in_chunk(&mut chunk, 0).is_empty());
        assert_eq!(start, before);
        let mut neighbor = GeneratedChunk::new(-1, 3, -64, 384, 1);
        neighbor.set_block_state(
            9,
            50,
            9,
            block_states().default_state("minecraft:stone").unwrap(),
        );
        let maps = neighbor.heightmaps().clone();
        assert!(start.place_in_chunk(&mut neighbor, 0).is_empty());
        assert_eq!(start, before);
        assert_eq!(neighbor.heightmaps(), &maps);
        assert_eq!(neighbor.block_state(9, 51, 9), 0);
    }
}
