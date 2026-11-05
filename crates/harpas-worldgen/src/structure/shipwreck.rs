//! Shipwreck template placement, observed through vanilla's public piece API.

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, Nbt};
use harpas_core::random::{RandomSource, WorldgenRandom, XoroshiroRandom};

use super::{StructurePiece, StructureStart, TemplatePiece};
use crate::{GeneratedChunk, HeightmapType, Rotation, StructureTemplate, StructureTemplates};

impl StructureStart {
    /// Columns sampled for height adjustment. Vanilla samples the unrotated template footprint at
    /// its origin, even when the rotated hull occupies different columns.
    pub fn shipwreck_height_samples<'a>(
        &'a self,
        templates: &'a StructureTemplates,
    ) -> impl Iterator<Item = (i32, i32)> + 'a {
        self.pieces
            .iter()
            .filter(|p| p.kind == "minecraft:shipwreck")
            .filter_map(|p| {
                let template = p.template.as_ref()?;
                let size = templates.get(&template.template)?.size;
                Some((template.position, size))
            })
            .flat_map(|([x, _, z], size)| {
                (z..z + size[2]).flat_map(move |z| (x..x + size[0]).map(move |x| (x, z)))
            })
    }

    /// Chooses one source-first height before workers place intersections in neighboring chunks.
    /// Existing adjusted origins are retained. The caller shares this prepared start across workers.
    pub fn prepare_shipwreck_height(
        &mut self,
        seed: i64,
        templates: &StructureTemplates,
        height_at: &mut impl FnMut(HeightmapType, i32, i32) -> i32,
    ) {
        let index = match self.structure.as_str() {
            "minecraft:shipwreck" => 17,
            "minecraft:shipwreck_beached" => 18,
            _ => return,
        };
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(0));
        let decoration = random.set_decoration_seed(seed, self.chunk.0 * 16, self.chunk.1 * 16);
        random.set_feature_seed(decoration, index, 4);
        for piece in &mut self.pieces {
            if piece.kind != "minecraft:shipwreck" {
                continue;
            }
            if let Some(blocks) = piece
                .template
                .as_ref()
                .and_then(|t| templates.get(&t.template))
            {
                adjust_height(piece, blocks, &mut random, height_at);
            }
        }
    }

    /// Places shipwreck blocks in one chunk. Neighbor heights and blocks come from saved terrain when
    /// available. The piece retains its adjusted template position for vanilla to resume later.
    pub fn place_shipwreck_in_chunk(
        &mut self,
        chunk: &mut GeneratedChunk,
        seed: i64,
        templates: &StructureTemplates,
        height_at: &mut impl FnMut(HeightmapType, i32, i32) -> i32,
        block_at: &mut impl FnMut(i32, i32, i32) -> u32,
    ) -> Vec<Compound> {
        self.place_prepared_shipwreck_in_chunk(chunk, seed, templates, height_at, block_at, false)
    }

    /// Places a prepared worker start. `initial_burial` consumes the original source chunk's burial
    /// draw even though its shared height has already been chosen. Replays and neighboring chunks
    /// pass false, retaining vanilla's subsequent loot draw order.
    pub fn place_prepared_shipwreck_in_chunk(
        &mut self,
        chunk: &mut GeneratedChunk,
        seed: i64,
        templates: &StructureTemplates,
        height_at: &mut impl FnMut(HeightmapType, i32, i32) -> i32,
        block_at: &mut impl FnMut(i32, i32, i32) -> u32,
        initial_burial: bool,
    ) -> Vec<Compound> {
        let index = match self.structure.as_str() {
            "minecraft:shipwreck" => 17,
            "minecraft:shipwreck_beached" => 18,
            _ => return Vec::new(),
        };
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(0));
        let decoration =
            random.set_decoration_seed(seed, chunk.chunk_x() * 16, chunk.chunk_z() * 16);
        random.set_feature_seed(decoration, index, 4);
        if initial_burial {
            random.next_int_bounded(3);
        }
        let mut entities = Vec::new();
        for piece in &mut self.pieces {
            if piece.kind != "minecraft:shipwreck"
                || !piece.bounding_box.intersects_columns(
                    chunk.chunk_x() * 16,
                    chunk.chunk_z() * 16,
                    chunk.chunk_x() * 16 + 15,
                    chunk.chunk_z() * 16 + 15,
                )
            {
                continue;
            }
            let Some(blocks) = piece
                .template
                .as_ref()
                .and_then(|t| templates.get(&t.template))
            else {
                continue;
            };
            if blocks.size[0] > 32
                || blocks.size[1] > 32
                || blocks.size[0] <= 0
                || blocks.size[2] <= 0
            {
                continue;
            }
            adjust_height(piece, blocks, &mut random, &mut |kind, x, z| {
                if (x >> 4, z >> 4) == (chunk.chunk_x(), chunk.chunk_z()) {
                    chunk
                        .heightmaps()
                        .get(kind, (x & 15) as usize, (z & 15) as usize)
                } else {
                    height_at(kind, x, z)
                }
            });
            let template = piece.template.as_ref().unwrap();
            let mut placed = Vec::new();
            let mut markers = Vec::new();
            for block in blocks.blocks(template.position) {
                let pos @ [x, y, z] = position(template, block.pos);
                if !contains(chunk, pos) {
                    continue;
                }
                let name = block_states().properties(block.state).unwrap().0;
                if name == "minecraft:structure_block" {
                    if let Some(Nbt::String(marker)) =
                        block.nbt.as_ref().and_then(|n| n.get("metadata"))
                    {
                        markers.push((pos, marker.as_str()));
                    }
                    continue;
                }
                let mut state = super::igloo::rotate_state(block.state, template.rotation);
                if matches!(
                    template.rotation,
                    Rotation::Clockwise90 | Rotation::Counterclockwise90
                ) {
                    let (_, props) = block_states().properties(state).unwrap();
                    if let Some((_, axis @ ("x" | "z"))) = props.iter().find(|p| p.0 == "axis") {
                        state = property(state, "axis", if *axis == "x" { "z" } else { "x" });
                    }
                }
                let old = chunk.block_state((x & 15) as usize, y, (z & 15) as usize);
                if water(old) {
                    state = property(state, "waterlogged", "true");
                }
                set(chunk, pos, state);
                placed.push(pos);
                if let Some(nbt) = &block.nbt {
                    let mut entity = nbt.clone();
                    for (key, value) in ["x", "y", "z"].into_iter().zip(pos) {
                        entity.insert(key, value);
                    }
                    entity.insert("components", Compound::new());
                    if name == "minecraft:chest" {
                        let loot = random.next_long();
                        if entity.get("LootTable").is_some() && loot != 0 {
                            entity.insert("LootTableSeed", loot);
                        }
                    }
                    entities.push(entity);
                }
            }
            for ([x, y, z], marker) in markers {
                let table = match marker {
                    "map_chest" => "minecraft:chests/shipwreck_map",
                    "treasure_chest" => "minecraft:chests/shipwreck_treasure",
                    "supply_chest" => "minecraft:chests/shipwreck_supply",
                    _ => continue,
                };
                if let Some(chest) = entities.iter_mut().find(|e| {
                    e.get("x") == Some(&Nbt::Int(x))
                        && e.get("y") == Some(&Nbt::Int(y - 1))
                        && e.get("z") == Some(&Nbt::Int(z))
                }) {
                    chest.remove("Items");
                    chest.insert("LootTable", table);
                    chest.remove("LootTableSeed");
                    let loot = random.next_long();
                    if loot != 0 {
                        chest.insert("LootTableSeed", loot);
                    }
                }
            }
            finish_template(chunk, &placed, block_at);
        }
        entities
    }
}

/// Completes template waterlogging and edge shape updates after placing its blocks and markers.
pub(super) fn finish_template(
    chunk: &mut GeneratedChunk,
    placed: &[[i32; 3]],
    block_at: &mut impl FnMut(i32, i32, i32) -> u32,
) {
    // Containers whose original position was dry can take source water from above or a
    // horizontal neighbor. Repeat until no more containers fill, as template placement does.
    loop {
        let mut changed = false;
        for &pos @ [x, y, z] in placed {
            let state = chunk.block_state((x & 15) as usize, y, (z & 15) as usize);
            if !block_states()
                .properties(state)
                .unwrap()
                .1
                .contains(&("waterlogged", "false"))
            {
                continue;
            }
            let fills = [(0, 1, 0), (0, 0, -1), (1, 0, 0), (0, 0, 1), (-1, 0, 0)]
                .into_iter()
                .any(|(dx, dy, dz)| {
                    let neighbor =
                        if ((x + dx) >> 4, (z + dz) >> 4) == (chunk.chunk_x(), chunk.chunk_z()) {
                            chunk.block_state(
                                ((x + dx) & 15) as usize,
                                y + dy,
                                ((z + dz) & 15) as usize,
                            )
                        } else {
                            block_at(x + dx, y + dy, z + dz)
                        };
                    source_water(neighbor)
                });
            if fills {
                set(chunk, pos, property(state, "waterlogged", "true"));
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // Template edge updates also affect existing blocks beside the placed voxel shape. A
    // replay can move the hull upward and leave old stairs or fences just outside that shape.
    let mut affected = placed.to_vec();
    let mut seen: std::collections::HashSet<_> = placed.iter().copied().collect();
    for &[x, y, z] in placed {
        for [dx, dy, dz] in [
            [0, -1, 0],
            [0, 1, 0],
            [0, 0, -1],
            [1, 0, 0],
            [0, 0, 1],
            [-1, 0, 0],
        ] {
            let pos = [x + dx, y + dy, z + dz];
            if contains(chunk, pos) && seen.insert(pos) {
                affected.push(pos);
            }
        }
    }
    // Resolve stair faces before fences test them; the stored palettes deliberately contain
    // placeholder stair shapes that vanilla recomputes when placing the template.
    for &pos @ [x, y, z] in &affected {
        if block_states()
            .properties(chunk.block_state((x & 15) as usize, y, (z & 15) as usize))
            .unwrap()
            .0
            .ends_with("_stairs")
        {
            let state = shape(chunk, pos, block_at);
            set(chunk, pos, state);
        }
    }
    for pos in affected {
        let state = shape(chunk, pos, block_at);
        set(chunk, pos, state);
    }
}

fn adjust_height(
    piece: &mut StructurePiece,
    blocks: &StructureTemplate,
    random: &mut impl RandomSource,
    height_at: &mut impl FnMut(HeightmapType, i32, i32) -> i32,
) {
    if piece.save_data.get("height_adjusted") == Some(&Nbt::Byte(1))
        || blocks.size[0] <= 0
        || blocks.size[2] <= 0
        || blocks.size[0] > 32
        || blocks.size[1] > 32
    {
        return;
    }
    let template = piece.template.as_mut().unwrap();
    let beached = piece.save_data.get("isBeached") == Some(&Nbt::Byte(1));
    let kind = if beached {
        HeightmapType::WorldSurfaceWg
    } else {
        HeightmapType::OceanFloorWg
    };
    let mut sum = 0;
    let mut lowest = i32::MAX;
    for z in template.position[2]..template.position[2] + blocks.size[2] {
        for x in template.position[0]..template.position[0] + blocks.size[0] {
            let height = height_at(kind, x, z);
            sum += height;
            lowest = lowest.min(height);
        }
    }
    template.position[1] = if beached {
        lowest - blocks.size[1] / 2 - random.next_int_bounded(3)
    } else {
        sum / (blocks.size[0] * blocks.size[2])
    };
    piece.save_data.insert("height_adjusted", 1_i8);
    piece.bounding_box = blocks.bounding_box_around(
        template.position,
        template.rotation,
        template.mirror,
        template.pivot,
    );
}

pub(super) fn position(template: &TemplatePiece, pos: [i32; 3]) -> [i32; 3] {
    let p = template.rotation.transform_around(pos, template.pivot);
    std::array::from_fn(|i| p[i] + template.position[i])
}

pub(super) fn contains(chunk: &GeneratedChunk, [x, y, z]: [i32; 3]) -> bool {
    (x >> 4, z >> 4) == (chunk.chunk_x(), chunk.chunk_z())
        && y >= chunk.min_y()
        && y < chunk.min_y() + chunk.height()
}

pub(super) fn set(chunk: &mut GeneratedChunk, [x, y, z]: [i32; 3], state: u32) {
    chunk.set_block_state((x & 15) as usize, y, (z & 15) as usize, state);
}

pub(super) fn water(state: u32) -> bool {
    let (name, props) = block_states().properties(state).unwrap();
    matches!(
        name,
        "minecraft:water"
            | "minecraft:bubble_column"
            | "minecraft:kelp"
            | "minecraft:kelp_plant"
            | "minecraft:seagrass"
            | "minecraft:tall_seagrass"
    ) || props.contains(&("waterlogged", "true"))
}

fn source_water(state: u32) -> bool {
    let (name, props) = block_states().properties(state).unwrap();
    water(state) && (name != "minecraft:water" || props.contains(&("level", "0")))
}

pub(super) fn property(state: u32, key: &str, value: &str) -> u32 {
    let (name, mut props) = block_states().properties(state).unwrap();
    let Some(prop) = props.iter_mut().find(|p| p.0 == key) else {
        return state;
    };
    prop.1 = value;
    block_states().state(name, props).unwrap().0
}

fn shape(
    chunk: &GeneratedChunk,
    [x, y, z]: [i32; 3],
    block_at: &mut impl FnMut(i32, i32, i32) -> u32,
) -> u32 {
    shape_at([x, y, z], &mut |[sx, sy, sz]| {
        if (sx >> 4, sz >> 4) == (chunk.chunk_x(), chunk.chunk_z()) {
            chunk.block_state((sx & 15) as usize, sy, (sz & 15) as usize)
        } else {
            block_at(sx, sy, sz)
        }
    })
}

pub(super) fn shape_at([x, y, z]: [i32; 3], block_at: &mut impl FnMut([i32; 3]) -> u32) -> u32 {
    let state = block_at([x, y, z]);
    let read = &mut |dx, dy, dz| block_at([x + dx, y + dy, z + dz]);
    let (name, props) = block_states().properties(state).unwrap();
    let get = |key| props.iter().find(|p| p.0 == key).unwrap().1;
    if name.ends_with("_fence") {
        let mut result = state;
        for (key, dx, dz) in [
            ("north", 0, -1),
            ("east", 1, 0),
            ("south", 0, 1),
            ("west", -1, 0),
        ] {
            let neighbor = read(dx, 0, dz);
            let (n, p) = block_states().properties(neighbor).unwrap();
            let face = match key {
                "north" => "south",
                "east" => "west",
                "south" => "north",
                "west" => "east",
                _ => unreachable!(),
            };
            let stair_face = if n.ends_with("_stairs") {
                let facing = p.iter().find(|p| p.0 == "facing").unwrap().1;
                let shape = p.iter().find(|p| p.0 == "shape").unwrap().1;
                let (dx, dz) = direction(facing);
                (!shape.starts_with("outer") && facing == face)
                    || (shape == "inner_left" && direction(face) == (dz, -dx))
                    || (shape == "inner_right" && direction(face) == (-dz, dx))
            } else {
                false
            };
            let connects = n.ends_with("_fence")
                || stair_face
                || (HeightmapType::OceanFloor.matches(neighbor)
                    && !n.ends_with("_stairs")
                    && !n.ends_with("_slab")
                    && !n.ends_with("_trapdoor")
                    && !n.ends_with("_door")
                    && n != "minecraft:chest");
            result = property(result, key, if connects { "true" } else { "false" });
        }
        return result;
    }
    if name.ends_with("_stairs") {
        let facing = get("facing");
        let half = get("half");
        let (dx, dz) = direction(facing);
        let stair_facing = |s| {
            let (n, p) = block_states().properties(s).unwrap();
            (n.ends_with("_stairs") && p.contains(&("half", half)))
                .then(|| p.iter().find(|p| p.0 == "facing").unwrap().1)
        };
        for (front, nx, nz) in [(true, dx, dz), (false, -dx, -dz)] {
            if let Some(other) = stair_facing(read(nx, 0, nz)) {
                let (ox, oz) = direction(other);
                if (dx == 0) != (ox == 0) {
                    let side = if front {
                        read(-ox, 0, -oz)
                    } else {
                        read(ox, 0, oz)
                    };
                    if stair_facing(side) != Some(facing) {
                        let left = (ox, oz) == (dz, -dx);
                        return property(
                            state,
                            "shape",
                            match (front, left) {
                                (true, true) => "outer_left",
                                (true, false) => "outer_right",
                                (false, true) => "inner_left",
                                (false, false) => "inner_right",
                            },
                        );
                    }
                }
            }
        }
        return property(state, "shape", "straight");
    }
    if name.ends_with("_door") {
        let upper = get("half") == "upper";
        let neighbor = read(0, if upper { -1 } else { 1 }, 0);
        let (n, p) = block_states().properties(neighbor).unwrap();
        if n != name || !p.contains(&("half", if upper { "lower" } else { "upper" })) {
            return 0;
        }
        let mut result = state;
        for key in ["facing", "open", "hinge", "powered"] {
            result = property(result, key, p.iter().find(|p| p.0 == key).unwrap().1);
        }
        return result;
    }
    state
}

fn direction(name: &str) -> (i32, i32) {
    match name {
        "north" => (0, -1),
        "east" => (1, 0),
        "south" => (0, 1),
        "west" => (-1, 0),
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Direction, Mirror, StructurePiece};
    use harpas_core::nbt::List;

    fn fixture(beached: bool) -> (StructureStart, StructureTemplates) {
        let mut root = Compound::new();
        root.insert("DataVersion", 4786);
        root.insert("size", List::Int(vec![2, 3, 2]));
        let palette = [
            "minecraft:chest",
            "minecraft:structure_block",
            "minecraft:oak_log",
        ]
        .map(|name| {
            let mut state = Compound::new();
            state.insert("Name", name);
            state
        });
        root.insert("palette", List::Compound(palette.to_vec()));
        let mut chest = Compound::new();
        chest.insert("id", "minecraft:chest");
        chest.insert("Items", List::Empty);
        let mut marker = Compound::new();
        marker.insert("metadata", "supply_chest");
        root.insert(
            "blocks",
            List::Compound(
                [
                    (0, [0, 0, 0], Some(chest)),
                    (1, [0, 1, 0], Some(marker)),
                    (2, [1, 0, 0], None),
                ]
                .map(|(state, pos, nbt)| {
                    let mut block = Compound::new();
                    block.insert("state", state);
                    block.insert("pos", List::Int(pos.to_vec()));
                    if let Some(nbt) = nbt {
                        block.insert("nbt", nbt);
                    }
                    block
                })
                .to_vec(),
            ),
        );
        let templates = StructureTemplates::with_nbt("test:ship", &root);
        let template = TemplatePiece {
            template: "test:ship".into(),
            position: [15, 90, 15],
            rotation: Rotation::None,
            mirror: Mirror::None,
            pivot: [4, 0, 15],
        };
        let bounding_box = templates
            .get("test:ship")
            .unwrap()
            .bounding_box(template.position, template.rotation);
        let mut save_data = Compound::new();
        save_data.insert("isBeached", i8::from(beached));
        save_data.insert("height_adjusted", 0_i8);
        (
            StructureStart {
                structure: if beached {
                    "minecraft:shipwreck_beached"
                } else {
                    "minecraft:shipwreck"
                }
                .into(),
                chunk: (0, 0),
                references: 0,
                bounding_box,
                pieces: vec![StructurePiece {
                    kind: "minecraft:shipwreck".into(),
                    bounding_box,
                    orientation: Some(Direction::North),
                    gen_depth: 0,
                    junctions: vec![],
                    jigsaw: None,
                    template: Some(template),
                    save_data,
                }],
            },
            templates,
        )
    }

    #[test]
    fn samples_the_whole_footprint_clips_blocks_and_retains_adjusted_height() {
        let (mut start, templates) = fixture(false);
        let mut chunk = GeneratedChunk::new(0, 0, -64, 384, 1);
        let water = block_states().default_state("minecraft:water").unwrap();
        let stone = block_states().default_state("minecraft:stone").unwrap();
        chunk.set_block_state(15, 49, 15, stone);
        chunk.set_block_state(15, 51, 15, water);
        let mut reads = Vec::new();
        let entities = start.place_shipwreck_in_chunk(
            &mut chunk,
            42,
            &templates,
            &mut |kind, x, z| {
                assert_eq!(kind, HeightmapType::OceanFloorWg);
                reads.push((x, z));
                52
            },
            &mut |_, _, _| panic!("no edge-shape blocks"),
        );
        assert_eq!(reads.len(), 3);
        assert_eq!(start.pieces[0].template.as_ref().unwrap().position[1], 51);
        assert_eq!(entities.len(), 1);
        assert_eq!(
            entities[0].get("LootTable"),
            Some(&Nbt::from("minecraft:chests/shipwreck_supply"))
        );
        assert_eq!(
            chunk.block_state(15, 52, 15),
            0,
            "marker leaves terrain unchanged"
        );
        assert!(
            block_states()
                .properties(chunk.block_state(15, 51, 15))
                .unwrap()
                .1
                .contains(&("waterlogged", "true"))
        );
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(0));
        let decoration = random.set_decoration_seed(42, 0, 0);
        random.set_feature_seed(decoration, 17, 4);
        random.next_long();
        assert_eq!(
            entities[0].get("LootTableSeed"),
            Some(&Nbt::Long(random.next_long()))
        );
        let tag = start.pieces[0].to_nbt();
        assert_eq!(tag.get("height_adjusted"), Some(&Nbt::Byte(1)));
        let before = blocks_at_chest(&chunk);
        let replay = start.place_shipwreck_in_chunk(
            &mut chunk,
            42,
            &templates,
            &mut |_, _, _| panic!("saved height must not be sampled again"),
            &mut |_, _, _| panic!("no edge-shape blocks"),
        );
        assert_eq!(replay, entities);
        assert_eq!(blocks_at_chest(&chunk), before);
        let mut outside = GeneratedChunk::new(-2, -2, -64, 384, 1);
        assert!(
            start
                .place_shipwreck_in_chunk(
                    &mut outside,
                    42,
                    &templates,
                    &mut |_, _, _| unreachable!(),
                    &mut |_, _, _| unreachable!()
                )
                .is_empty()
        );
    }

    fn blocks_at_chest(chunk: &GeneratedChunk) -> Vec<u32> {
        (49..54).map(|y| chunk.block_state(15, y, 15)).collect()
    }

    #[test]
    fn dry_containers_take_water_from_an_adjacent_saved_chunk() {
        for source in [false, true] {
            let (mut start, templates) = fixture(false);
            let mut chunk = GeneratedChunk::new(0, 0, -64, 384, 1);
            let stone = block_states().default_state("minecraft:stone").unwrap();
            let water = block_states()
                .state(
                    "minecraft:water",
                    [("level", if source { "0" } else { "1" })],
                )
                .unwrap()
                .0;
            chunk.set_block_state(15, 49, 15, stone);
            start.place_shipwreck_in_chunk(
                &mut chunk,
                42,
                &templates,
                &mut |_, _, _| 50,
                &mut |x, y, z| if (x, y, z) == (16, 50, 15) { water } else { 0 },
            );
            assert_eq!(
                block_states()
                    .properties(chunk.block_state(15, 50, 15))
                    .unwrap()
                    .1
                    .contains(&("waterlogged", "true")),
                source
            );
        }
    }

    #[test]
    fn prepared_source_height_preserves_the_initial_burial_and_loot_draw_order() {
        let (mut direct, templates) = fixture(true);
        let mut prepared = direct.clone();
        let mut first = GeneratedChunk::new(0, 0, -64, 384, 1);
        first.set_block_state(
            15,
            69,
            15,
            block_states().default_state("minecraft:stone").unwrap(),
        );
        let mut second = first.clone();
        let expected = direct.place_shipwreck_in_chunk(
            &mut first,
            42,
            &templates,
            &mut |_, _, _| 65,
            &mut |_, _, _| 0,
        );
        prepared.prepare_shipwreck_height(42, &templates, &mut |_, _, _| 65);
        let actual = prepared.place_prepared_shipwreck_in_chunk(
            &mut second,
            42,
            &templates,
            &mut |_, _, _| panic!("prepared height must not be sampled again"),
            &mut |_, _, _| 0,
            true,
        );
        assert_eq!(actual, expected);
        assert_eq!(prepared.to_nbt(), direct.to_nbt());
        assert_eq!(blocks_at_chest(&first), blocks_at_chest(&second));
    }

    #[test]
    fn template_edges_update_existing_stairs_outside_the_placed_blocks() {
        let (mut start, templates) = fixture(false);
        let mut chunk = GeneratedChunk::new(0, 0, -64, 384, 1);
        chunk.set_block_state(
            15,
            49,
            15,
            block_states().default_state("minecraft:stone").unwrap(),
        );
        let stair = block_states()
            .state(
                "minecraft:oak_stairs",
                [
                    ("facing", "north"),
                    ("half", "top"),
                    ("shape", "outer_right"),
                ],
            )
            .unwrap()
            .0;
        chunk.set_block_state(14, 50, 15, stair);
        start.place_shipwreck_in_chunk(
            &mut chunk,
            42,
            &templates,
            &mut |_, _, _| 50,
            &mut |_, _, _| 0,
        );
        assert_eq!(
            chunk.block_state(14, 50, 15),
            property(stair, "shape", "straight")
        );
    }

    #[test]
    fn a_rotated_hull_still_samples_heights_at_the_unrotated_origin() {
        let templates = StructureTemplates::with_sizes([("test:ship", [9, 9, 28])]);
        let (mut start, _) = fixture(false);
        let template = start.pieces[0].template.as_mut().unwrap();
        template.position = [0, 90, 0];
        template.rotation = Rotation::Clockwise90;
        start.pieces[0].bounding_box = templates.get("test:ship").unwrap().bounding_box_around(
            [0, 90, 0],
            Rotation::Clockwise90,
            crate::Mirror::None,
            [4, 0, 15],
        );
        let samples: Vec<_> = start.shipwreck_height_samples(&templates).collect();
        assert_eq!(samples.len(), 252);
        assert_eq!(samples[0], (0, 0));
        assert_eq!(samples[251], (8, 27));
        assert!(
            !samples.contains(&(-8, 11)),
            "rotated hull corner is outside the height samples"
        );
    }

    #[test]
    fn beached_height_uses_lowest_surface_and_consumes_the_burial_draw_once() {
        let (mut start, templates) = fixture(true);
        let mut chunk = GeneratedChunk::new(1, 1, -64, 384, 1);
        chunk.set_block_state(
            0,
            69,
            0,
            block_states().default_state("minecraft:stone").unwrap(),
        );
        start.place_shipwreck_in_chunk(
            &mut chunk,
            42,
            &templates,
            &mut |kind, _, _| {
                assert_eq!(kind, HeightmapType::WorldSurfaceWg);
                65
            },
            &mut |_, _, _| 0,
        );
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(0));
        let decoration = random.set_decoration_seed(42, 16, 16);
        random.set_feature_seed(decoration, 18, 4);
        assert_eq!(
            start.pieces[0].template.as_ref().unwrap().position[1],
            64 - random.next_int_bounded(3)
        );
        let pos = start.pieces[0].template.as_ref().unwrap().position;
        start.place_shipwreck_in_chunk(
            &mut chunk,
            42,
            &templates,
            &mut |_, _, _| panic!("already adjusted"),
            &mut |_, _, _| 0,
        );
        assert_eq!(start.pieces[0].template.as_ref().unwrap().position, pos);
    }
}
