//! Igloo template placement, observed against vanilla's public piece API.

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_core::random::{RandomSource, WorldgenRandom, XoroshiroRandom};

use super::{StructureStart, TemplatePiece};
use crate::{Facing, GeneratedChunk, HeightmapType, Rotation, StructureTemplates};

/// Rotation pivot and unrotated offset from the structure's origin.
pub(super) fn settings(name: &str) -> Option<([i32; 3], [i32; 3])> {
    Some(match name {
        "minecraft:igloo/top" => ([3, 5, 5], [0, 0, 0]),
        "minecraft:igloo/middle" => ([1, 3, 1], [2, -3, 4]),
        "minecraft:igloo/bottom" => ([3, 6, 7], [0, -3, -2]),
        _ => return None,
    })
}

impl StructureStart {
    /// Surface columns read by igloo pieces, so workers can resolve saved or generated terrain first.
    pub fn igloo_height_samples(&self) -> impl Iterator<Item = (i32, i32)> + '_ {
        self.pieces
            .iter()
            .filter_map(|p| p.template.as_ref())
            .filter_map(height_sample)
    }

    /// Places igloo templates in one chunk and returns its block entities. `surface_height` supplies
    /// WORLD_SURFACE_WG outside this chunk, and `block_at` reads neighboring terrain for template edge
    /// connections. Entity spawning belongs to gameplay.
    pub fn place_igloo_in_chunk(
        &mut self,
        chunk: &mut GeneratedChunk,
        seed: i64,
        templates: &StructureTemplates,
        surface_height: &mut impl FnMut(i32, i32) -> i32,
        block_at: &mut impl FnMut(i32, i32, i32) -> u32,
    ) -> Vec<Compound> {
        if self.structure != "minecraft:igloo" {
            return Vec::new();
        }
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(0));
        let decoration =
            random.set_decoration_seed(seed, chunk.chunk_x() * 16, chunk.chunk_z() * 16);
        random.set_feature_seed(decoration, 3, 4);
        let mut entities = Vec::new();
        for piece in &mut self.pieces {
            if piece.kind != "minecraft:iglu"
                || !piece.bounding_box.intersects_columns(
                    chunk.chunk_x() * 16,
                    chunk.chunk_z() * 16,
                    chunk.chunk_x() * 16 + 15,
                    chunk.chunk_z() * 16 + 15,
                )
            {
                continue;
            }
            let Some(template) = &piece.template else {
                continue;
            };
            let Some((sample_x, sample_z)) = height_sample(template) else {
                continue;
            };
            let Some(blocks) = templates.get(&template.template) else {
                continue;
            };
            // All three kinds sample the top piece's rotated entrance, independent of ladder depth.
            let height = if (sample_x >> 4, sample_z >> 4) == (chunk.chunk_x(), chunk.chunk_z()) {
                chunk.heightmaps().get(
                    HeightmapType::WorldSurfaceWg,
                    (sample_x & 15) as usize,
                    (sample_z & 15) as usize,
                )
            } else {
                surface_height(sample_x, sample_z)
            };
            let mut placed = template.clone();
            placed.position[1] += height - 91;
            piece.bounding_box = blocks.bounding_box_around(
                placed.position,
                placed.rotation,
                placed.mirror,
                placed.pivot,
            );
            let mut markers = Vec::new();
            for block in blocks.blocks(placed.position) {
                let pos = position(&placed, block.pos);
                if !contains(chunk, pos) {
                    continue;
                }
                let name = block_states().properties(block.state).unwrap().0;
                if name == "minecraft:structure_block" {
                    if block.nbt.as_ref().and_then(|n| n.get("metadata"))
                        == Some(&Nbt::from("chest"))
                    {
                        markers.push(pos);
                    }
                    continue;
                }
                let state = rotate_state(block.state, placed.rotation);
                set(chunk, pos, state);
                if let Some(nbt) = &block.nbt {
                    let mut entity = normalize_entity(nbt);
                    for (key, value) in ["x", "y", "z"].into_iter().zip(pos) {
                        entity.insert(key, value);
                    }
                    entity.insert("components", Compound::new());
                    if name == "minecraft:chest" {
                        // Template placement seeds randomizable containers even before their marker
                        // supplies a loot table. A container without a table does not serialize this seed.
                        let seed = random.next_long();
                        if entity.get("LootTable").is_some() && seed != 0 {
                            entity.insert("LootTableSeed", seed);
                        }
                    }
                    entities.push(entity);
                }
            }
            for pos @ [x, y, z] in markers {
                set(chunk, pos, 0);
                if let Some(chest) = entities.iter_mut().find(|e| {
                    e.get("x") == Some(&Nbt::Int(x))
                        && e.get("y") == Some(&Nbt::Int(y - 1))
                        && e.get("z") == Some(&Nbt::Int(z))
                }) {
                    chest.remove("Items");
                    chest.insert("LootTable", "minecraft:chests/igloo_chest");
                    let loot = random.next_long();
                    chest.remove("LootTableSeed");
                    if loot != 0 {
                        chest.insert("LootTableSeed", loot);
                    }
                }
            }
            // Template edge updates see terrain in the neighboring chunk, whose template intersection
            // has not been placed by this worker. Internal connections already come from the template.
            for block in blocks.blocks(placed.position) {
                let pos @ [x, y, z] = position(&placed, block.pos);
                if !contains(chunk, pos)
                    || block_states().properties(block.state).unwrap().0 != "minecraft:iron_bars"
                {
                    continue;
                }
                let state = chunk.block_state((x & 15) as usize, y, (z & 15) as usize);
                let mut changes = Vec::new();
                for (key, dx, dz) in [
                    ("north", 0, -1),
                    ("east", 1, 0),
                    ("south", 0, 1),
                    ("west", -1, 0),
                ] {
                    if ((x + dx) >> 4, (z + dz) >> 4) != (chunk.chunk_x(), chunk.chunk_z()) {
                        let neighbor = block_at(x + dx, y, z + dz);
                        changes.push((key, if connects(neighbor) { "true" } else { "false" }));
                    }
                }
                let (name, mut props) = block_states().properties(state).unwrap();
                for (key, value) in changes {
                    if let Some(p) = props.iter_mut().find(|p| p.0 == key) {
                        p.1 = value;
                    }
                }
                set(chunk, pos, block_states().state(name, props).unwrap().0);
            }
            // Template shape updates precede the top piece's separate snow-seal write.
            for block in blocks.blocks(placed.position) {
                let pos = position(&placed, block.pos);
                if contains(chunk, pos) {
                    update_snowy_ground(chunk, pos);
                }
            }
            if template.template == "minecraft:igloo/top" {
                let pos @ [x, y, z] = position(&placed, [3, 0, 5]);
                if contains(chunk, pos) && y > chunk.min_y() {
                    let below = chunk.block_state((x & 15) as usize, y - 1, (z & 15) as usize);
                    if !block_states().is_air(below)
                        && block_states().properties(below).unwrap().0 != "minecraft:ladder"
                    {
                        set(
                            chunk,
                            pos,
                            block_states()
                                .default_state("minecraft:snow_block")
                                .unwrap(),
                        );
                    }
                }
            }
        }
        entities
    }
}

fn update_snowy_ground(chunk: &mut GeneratedChunk, [x, y, z]: [i32; 3]) {
    if y <= chunk.min_y() {
        return;
    }
    let (lx, lz) = ((x & 15) as usize, (z & 15) as usize);
    let below = chunk.block_state(lx, y - 1, lz);
    let (name, properties) = block_states().properties(below).unwrap();
    if !properties.iter().any(|&(key, _)| key == "snowy") {
        return;
    }
    let above = block_states()
        .properties(chunk.block_state(lx, y, lz))
        .unwrap()
        .0;
    let snowy = matches!(above, "minecraft:snow_block" | "minecraft:snow");
    set(
        chunk,
        [x, y - 1, z],
        block_states()
            .state(name, [("snowy", if snowy { "true" } else { "false" })])
            .unwrap()
            .0,
    );
}

fn connects(state: u32) -> bool {
    let (name, _) = block_states().properties(state).unwrap();
    // Solid terrain faces and the igloo's own masonry are the possible neighbors at this stage.
    name == "minecraft:iron_bars"
        || (HeightmapType::OceanFloor.matches(state)
            && !name.ends_with("_leaves")
            && !name.ends_with("_stairs")
            && !name.ends_with("_slab")
            && !matches!(
                name,
                "minecraft:ladder"
                    | "minecraft:chest"
                    | "minecraft:water_cauldron"
                    | "minecraft:brewing_stand"
                    | "minecraft:potted_cactus"
            )
            && !name.ends_with("_carpet")
            && !name.ends_with("_sign")
            && !name.ends_with("_bed")
            && !name.ends_with("_torch")
            && !name.ends_with("_trapdoor"))
}

fn height_sample(template: &TemplatePiece) -> Option<(i32, i32)> {
    let (_, offset) = settings(&template.template)?;
    let sample = template.rotation.transform_around([3, 0, 0], [3, 0, 5]);
    Some((
        template.position[0] - offset[0] + sample[0],
        template.position[2] - offset[2] + sample[2],
    ))
}

fn position(template: &TemplatePiece, pos: [i32; 3]) -> [i32; 3] {
    let rotated = template.rotation.transform_around(pos, template.pivot);
    std::array::from_fn(|i| rotated[i] + template.position[i])
}

fn contains(chunk: &GeneratedChunk, [x, y, z]: [i32; 3]) -> bool {
    (x >> 4, z >> 4) == (chunk.chunk_x(), chunk.chunk_z())
        && y >= chunk.min_y()
        && y < chunk.min_y() + chunk.height()
}

fn set(chunk: &mut GeneratedChunk, [x, y, z]: [i32; 3], state: u32) {
    chunk.set_block_state((x & 15) as usize, y, (z & 15) as usize, state);
}

pub(super) fn rotate_state(state: u32, rotation: Rotation) -> u32 {
    let (name, properties) = block_states().properties(state).unwrap();
    let rotate = |direction: &str| {
        let facing = match direction {
            "north" => Facing::North,
            "east" => Facing::East,
            "south" => Facing::South,
            "west" => Facing::West,
            _ => return direction.to_string(),
        };
        rotation.rotate(facing).name().to_string()
    };
    let properties: Vec<_> = properties
        .into_iter()
        .map(|(key, value)| {
            (
                if matches!(key, "north" | "east" | "south" | "west") {
                    rotate(key)
                } else {
                    key.to_string()
                },
                if key == "facing" {
                    rotate(value)
                } else {
                    value.to_string()
                },
            )
        })
        .collect();
    block_states()
        .state(
            name,
            properties.iter().map(|(k, v)| (k.as_str(), v.as_str())),
        )
        .unwrap()
        .0
}

fn normalize_entity(nbt: &Compound) -> Compound {
    let mut tag = nbt.clone();
    if tag.get("id") == Some(&Nbt::from("minecraft:sign")) {
        tag.insert("is_waxed", 0_i8);
        for key in ["front_text", "back_text"] {
            if let Some(Nbt::Compound(mut text)) = tag.remove(key) {
                // Igloo signs contain literal text components. Vanilla saves their compact string form.
                for list_key in ["messages", "filtered_messages"] {
                    if let Some(messages) = text.remove(list_key) {
                        let Nbt::List(List::Compound(messages)) = messages else {
                            text.insert(list_key, messages);
                            continue;
                        };
                        let strings = messages
                            .iter()
                            .map(|m| match m.get("text") {
                                Some(Nbt::String(s)) => s.clone(),
                                _ => String::new(),
                            })
                            .collect();
                        text.insert(list_key, List::String(strings));
                    }
                }
                if text.get("messages") == text.get("filtered_messages") {
                    text.remove("filtered_messages");
                }
                tag.insert(key, text);
            }
        }
    }
    tag
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Direction, Mirror, StructurePiece};

    // Deliberately synthetic blocks: tests do not embed Mojang's template data.
    fn fixture() -> (StructureStart, StructureTemplates) {
        let mut chest = Compound::new();
        chest.insert("Name", "minecraft:chest");
        let mut marker = Compound::new();
        marker.insert("Name", "minecraft:structure_block");
        let mut snow = Compound::new();
        snow.insert("Name", "minecraft:snow_block");
        let mut chest_data = Compound::new();
        chest_data.insert("id", "minecraft:chest");
        chest_data.insert("Items", List::Empty);
        let mut marker_data = Compound::new();
        marker_data.insert("metadata", "chest");
        let blocks =
            [(0, [1, 1, 1], chest_data), (1, [1, 2, 1], marker_data)].map(|(state, pos, nbt)| {
                let mut block = Compound::new();
                block.insert("state", state);
                block.insert("pos", List::Int(pos.to_vec()));
                block.insert("nbt", nbt);
                block
            });
        let mut root = Compound::new();
        root.insert("DataVersion", 4786);
        root.insert("size", List::Int(vec![4, 4, 6]));
        root.insert("palette", List::Compound(vec![chest, marker, snow]));
        let mut blocks = blocks.to_vec();
        let mut snow = Compound::new();
        snow.insert("state", 2);
        snow.insert("pos", List::Int(vec![2, 0, 3]));
        blocks.push(snow);
        root.insert("blocks", List::Compound(blocks));
        let templates = StructureTemplates::with_nbt("minecraft:igloo/top", &root);
        let template = TemplatePiece {
            template: "minecraft:igloo/top".into(),
            position: [15, 90, 15],
            rotation: Rotation::None,
            mirror: Mirror::None,
            pivot: [3, 5, 5],
        };
        let bounds = templates
            .get(&template.template)
            .unwrap()
            .bounding_box(template.position, template.rotation);
        let start = StructureStart {
            structure: "minecraft:igloo".into(),
            chunk: (0, 0),
            references: 0,
            bounding_box: bounds,
            pieces: vec![StructurePiece {
                kind: "minecraft:iglu".into(),
                bounding_box: bounds,
                orientation: Some(Direction::North),
                gen_depth: 0,
                junctions: Vec::new(),
                jigsaw: None,
                template: Some(template),
                save_data: Compound::new(),
            }],
        };
        (start, templates)
    }

    #[test]
    fn clips_template_writes_and_keeps_original_position_and_loot_draw_order() {
        let (mut start, templates) = fixture();
        let original = start.pieces[0].template.clone();
        let mut chunk = GeneratedChunk::new(1, 1, -64, 384, 1);
        chunk.set_block_state(
            1,
            63,
            2,
            block_states()
                .default_state("minecraft:grass_block")
                .unwrap(),
        );
        let entities = start.place_igloo_in_chunk(
            &mut chunk,
            42,
            &templates,
            &mut |x, z| {
                assert_eq!((x, z), (18, 15));
                65
            },
            &mut |_, _, _| unreachable!("synthetic template has no bars"),
        );
        assert_eq!(entities.len(), 1);
        assert_eq!(
            chunk.block_state(1, 63, 2),
            block_states()
                .state("minecraft:grass_block", [("snowy", "true")])
                .unwrap()
                .0
        );
        assert_eq!(entities[0].get("x"), Some(&Nbt::Int(16)));
        assert_eq!(entities[0].get("y"), Some(&Nbt::Int(65)));
        assert_eq!(
            entities[0].get("LootTable"),
            Some(&Nbt::from("minecraft:chests/igloo_chest"))
        );
        assert!(entities[0].get("Items").is_none());
        assert_eq!(chunk.block_state(0, 66, 0), 0);
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(0));
        let decoration = random.set_decoration_seed(42, 16, 16);
        random.set_feature_seed(decoration, 3, 4);
        random.next_long();
        assert_eq!(
            entities[0].get("LootTableSeed"),
            Some(&Nbt::Long(random.next_long()))
        );
        assert_eq!(start.pieces[0].template, original);
        assert_eq!(start.pieces[0].bounding_box.min[1], 64);
        let before = start.clone();
        let mut outside = GeneratedChunk::new(-1, -1, -64, 384, 1);
        assert!(
            start
                .place_igloo_in_chunk(
                    &mut outside,
                    42,
                    &templates,
                    &mut |_, _| panic!("outside piece"),
                    &mut |_, _, _| unreachable!("outside piece")
                )
                .is_empty()
        );
        assert_eq!(start, before);
    }

    #[test]
    fn rotates_facing_and_connection_properties_without_changing_other_values() {
        let state = block_states()
            .state("minecraft:iron_bars", [("east", "true"), ("west", "true")])
            .unwrap()
            .0;
        let rotated = rotate_state(state, Rotation::Clockwise90);
        let props = block_states().properties(rotated).unwrap().1;
        assert!(props.contains(&("north", "true")) && props.contains(&("south", "true")));
        assert!(props.contains(&("east", "false")) && props.contains(&("west", "false")));
        let state = block_states()
            .state(
                "minecraft:oak_trapdoor",
                [("facing", "east"), ("half", "top"), ("open", "true")],
            )
            .unwrap()
            .0;
        let rotated = rotate_state(state, Rotation::Clockwise90);
        let props = block_states().properties(rotated).unwrap().1;
        assert!(
            props.contains(&("facing", "south"))
                && props.contains(&("half", "top"))
                && props.contains(&("open", "true"))
        );
    }
}
