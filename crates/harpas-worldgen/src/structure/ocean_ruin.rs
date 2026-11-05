//! Ocean ruin placement, checked through vanilla's public piece and processor APIs.

use harpas_core::block::block_states;
use harpas_core::math::position_seed;
use harpas_core::nbt::{Compound, Nbt};
use harpas_core::random::{LegacyRandom, RandomSource, WorldgenRandom, XoroshiroRandom};

use super::StructureStart;
use super::shipwreck::{contains, finish_template, position, property, set, water};
use crate::{GeneratedChunk, HeightmapType, StructureTemplates};

impl StructureStart {
    /// Columns used by ocean-floor fitting and template edge updates, including saved neighbors.
    pub fn ocean_ruin_terrain_samples(&self) -> impl Iterator<Item = (i32, i32)> + '_ {
        self.pieces
            .iter()
            .filter(|p| p.kind == "minecraft:orp")
            .flat_map(|p| {
                let bounds = p.bounding_box;
                (bounds.min[0] - 2..=bounds.max[0] + 2)
                    .flat_map(move |x| (bounds.min[2] - 2..=bounds.max[2] + 2).map(move |z| (x, z)))
            })
    }

    /// Places cold or warm ruins in one chunk. Neighbors supply carved or saved terrain; drowned
    /// spawning follows in M4. Returns the resulting chest and archaeology block entities.
    pub fn place_ocean_ruin_in_chunk(
        &mut self,
        chunk: &mut GeneratedChunk,
        seed: i64,
        templates: &StructureTemplates,
        height_at: &mut impl FnMut(HeightmapType, i32, i32) -> i32,
        block_at: &mut impl FnMut(i32, i32, i32) -> u32,
    ) -> Vec<Compound> {
        let index = match self.structure.as_str() {
            "minecraft:ocean_ruin_cold" => 7,
            "minecraft:ocean_ruin_warm" => 8,
            _ => return Vec::new(),
        };
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(0));
        let decoration =
            random.set_decoration_seed(seed, chunk.chunk_x() * 16, chunk.chunk_z() * 16);
        random.set_feature_seed(decoration, index, 4);
        let mut entities = Vec::new();
        for piece in &mut self.pieces {
            if piece.kind != "minecraft:orp"
                || !piece.bounding_box.intersects_columns(
                    chunk.chunk_x() * 16,
                    chunk.chunk_z() * 16,
                    chunk.chunk_x() * 16 + 15,
                    chunk.chunk_z() * 16 + 15,
                )
            {
                continue;
            }
            let Some(template) = &mut piece.template else {
                continue;
            };
            let Some(blocks) = templates.get(&template.template) else {
                continue;
            };
            let [x, _, z] = template.position;
            let height = if (x >> 4, z >> 4) == (chunk.chunk_x(), chunk.chunk_z()) {
                chunk.heightmaps().get(
                    HeightmapType::OceanFloorWg,
                    (x & 15) as usize,
                    (z & 15) as usize,
                )
            } else {
                height_at(HeightmapType::OceanFloorWg, x, z)
            };
            template.position[1] = height;
            let far = position(template, [blocks.size[0] - 1, 0, blocks.size[2] - 1]);
            let mut lowest = height - 1;
            let mut deep = 0;
            for sx in x.min(far[0])..=x.max(far[0]) {
                for sz in z.min(far[2])..=z.max(far[2]) {
                    let mut y = height - 1;
                    while y > chunk.min_y() + 1 {
                        let state = if (sx >> 4, sz >> 4) == (chunk.chunk_x(), chunk.chunk_z()) {
                            chunk.block_state((sx & 15) as usize, y, (sz & 15) as usize)
                        } else {
                            block_at(sx, y, sz)
                        };
                        let name = block_states().properties(state).unwrap().0;
                        if !block_states().is_air(state)
                            && !water(state)
                            && !matches!(
                                name,
                                "minecraft:ice"
                                    | "minecraft:packed_ice"
                                    | "minecraft:blue_ice"
                                    | "minecraft:frosted_ice"
                            )
                        {
                            break;
                        }
                        y -= 1;
                    }
                    lowest = lowest.min(y);
                    if y < height - 3 {
                        deep += 1;
                    }
                }
            }
            if lowest < height - 3 && deep > (x - far[0]).abs() - 2 {
                template.position[1] = lowest + 1;
            }
            piece.bounding_box = blocks.bounding_box_around(
                template.position,
                template.rotation,
                template.mirror,
                template.pivot,
            );
            let integrity = match piece.save_data.get("Integrity") {
                Some(Nbt::Float(f)) => *f,
                _ => 1.0,
            };
            let cold = piece.save_data.get("BiomeType") == Some(&Nbt::from("COLD"));
            let large = piece.save_data.get("IsLarge") == Some(&Nbt::Byte(1));
            // Deterioration and capped archaeology process the entire piece before clipping to this chunk.
            let mut processed = Vec::new();
            for block in blocks.blocks(template.position) {
                let pos = position(template, block.pos);
                let [x, y, z] = pos;
                let mut rot = LegacyRandom::new(position_seed(x, y, z));
                let name = block_states().properties(block.state).unwrap().0;
                if rot.next_float() > integrity
                    || matches!(name, "minecraft:air" | "minecraft:structure_block")
                {
                    continue;
                }
                processed.push((
                    pos,
                    super::igloo::rotate_state(block.state, template.rotation),
                    None,
                ));
            }
            let [x, y, z] = template.position;
            let mut capped = LegacyRandom::new(seed).fork_positional().at(x, y, z);
            let mut order: Vec<_> = (0..processed.len()).collect();
            for i in (2..=order.len()).rev() {
                let j = capped.next_int_bounded(i as i32) as usize;
                order.swap(i - 1, j);
            }
            let material = if cold {
                "minecraft:gravel"
            } else {
                "minecraft:sand"
            };
            let mut count = 0;
            for i in order {
                let (pos, state, entity) = &mut processed[i];
                let [x, y, z] = *pos;
                if block_states().properties(*state).unwrap().0 != material {
                    continue;
                }
                *state = block_states()
                    .default_state(if cold {
                        "minecraft:suspicious_gravel"
                    } else {
                        "minecraft:suspicious_sand"
                    })
                    .unwrap();
                let mut loot = LegacyRandom::new(position_seed(x, y, z));
                *entity = Some(loot_entity(
                    *pos,
                    "brushable_block",
                    if cold {
                        "archaeology/ocean_ruin_cold"
                    } else {
                        "archaeology/ocean_ruin_warm"
                    },
                    loot.next_long(),
                ));
                count += 1;
                if count == 5 {
                    break;
                }
            }
            let mut placed = Vec::new();
            for (pos @ [x, y, z], mut state, entity) in processed {
                if !contains(chunk, pos) {
                    continue;
                }
                if water(chunk.block_state((x & 15) as usize, y, (z & 15) as usize)) {
                    state = property(state, "waterlogged", "true");
                }
                entities.retain(|old: &Compound| {
                    old.get("x") != Some(&Nbt::Int(x))
                        || old.get("y") != Some(&Nbt::Int(y))
                        || old.get("z") != Some(&Nbt::Int(z))
                });
                set(chunk, pos, state);
                if let Some(entity) = entity {
                    replace_entity(&mut entities, entity);
                }
                placed.push(pos);
            }
            finish_template(chunk, &placed, block_at);
            // Markers are handled independently of integrity. Chest markers become new containers;
            // drowned markers are deferred with entity spawning.
            for block in blocks.blocks(template.position) {
                let Some(Nbt::String(marker)) = block.nbt.as_ref().and_then(|n| n.get("metadata"))
                else {
                    continue;
                };
                let pos @ [x, y, z] = position(template, block.pos);
                if !contains(chunk, pos) {
                    continue;
                }
                if marker == "chest" {
                    let old = chunk.block_state((x & 15) as usize, y, (z & 15) as usize);
                    let chest = property(
                        block_states().default_state("minecraft:chest").unwrap(),
                        "waterlogged",
                        if water(old) { "true" } else { "false" },
                    );
                    set(chunk, pos, chest);
                    replace_entity(
                        &mut entities,
                        loot_entity(
                            pos,
                            "chest",
                            if large {
                                "chests/underwater_ruin_big"
                            } else {
                                "chests/underwater_ruin_small"
                            },
                            random.next_long(),
                        ),
                    );
                }
            }
        }
        entities
    }
}

fn loot_entity(pos: [i32; 3], id: &str, table: &str, seed: i64) -> Compound {
    let mut entity = Compound::new();
    entity.insert("id", format!("minecraft:{id}"));
    for (key, value) in ["x", "y", "z"].into_iter().zip(pos) {
        entity.insert(key, value);
    }
    entity.insert("components", Compound::new());
    entity.insert("LootTable", format!("minecraft:{table}"));
    if seed != 0 {
        entity.insert("LootTableSeed", seed);
    }
    entity
}

fn replace_entity(entities: &mut Vec<Compound>, entity: Compound) {
    entities.retain(|old| {
        !["x", "y", "z"]
            .iter()
            .all(|key| old.get(key) == entity.get(key))
    });
    entities.push(entity);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Direction, Mirror, Rotation, StructurePiece, TemplatePiece};
    use harpas_core::nbt::List;

    fn fixture(integrity: f32) -> (StructureStart, StructureTemplates) {
        let mut root = Compound::new();
        root.insert("DataVersion", 4786);
        root.insert("size", List::Int(vec![4, 2, 4]));
        root.insert(
            "palette",
            List::Compound(
                ["minecraft:gravel", "minecraft:structure_block"]
                    .map(|name| {
                        let mut tag = Compound::new();
                        tag.insert("Name", name);
                        tag
                    })
                    .to_vec(),
            ),
        );
        let mut blocks = Vec::new();
        for z in 0..4 {
            for x in 0..4 {
                let mut block = Compound::new();
                block.insert("state", 0);
                block.insert("pos", List::Int(vec![x, 0, z]));
                blocks.push(block);
            }
        }
        let mut marker = Compound::new();
        marker.insert("metadata", "chest");
        let mut block = Compound::new();
        block.insert("state", 1);
        block.insert("pos", List::Int(vec![1, 1, 1]));
        block.insert("nbt", marker);
        blocks.push(block);
        root.insert("blocks", List::Compound(blocks));
        let templates = StructureTemplates::with_nbt("test:ruin", &root);
        let template = TemplatePiece {
            template: "test:ruin".into(),
            position: [14, 90, 14],
            rotation: Rotation::None,
            mirror: Mirror::None,
            pivot: [0, 0, 0],
        };
        let bounds = templates
            .get("test:ruin")
            .unwrap()
            .bounding_box(template.position, template.rotation);
        let mut save_data = Compound::new();
        save_data.insert("Integrity", integrity);
        save_data.insert("BiomeType", "COLD");
        save_data.insert("IsLarge", 0_i8);
        (
            StructureStart {
                structure: "minecraft:ocean_ruin_cold".into(),
                chunk: (0, 0),
                references: 0,
                bounding_box: bounds,
                pieces: vec![StructurePiece {
                    kind: "minecraft:orp".into(),
                    bounding_box: bounds,
                    orientation: Some(Direction::North),
                    gen_depth: 0,
                    junctions: Vec::new(),
                    jigsaw: None,
                    template: Some(template),
                    save_data,
                }],
            },
            templates,
        )
    }

    fn terrain(_: i32, y: i32, _: i32) -> u32 {
        block_states()
            .default_state(if y < 50 {
                "minecraft:stone"
            } else if y < 63 {
                "minecraft:water"
            } else {
                "minecraft:air"
            })
            .unwrap()
    }

    #[test]
    fn archaeology_is_capped_for_the_whole_piece_before_chunk_clipping() {
        let (source, templates) = fixture(1.0);
        let mut entities = Vec::new();
        for cx in 0..2 {
            for cz in 0..2 {
                let mut chunk = GeneratedChunk::new(cx, cz, -64, 384, 1);
                for y in 49..63 {
                    for z in 0..16 {
                        for x in 0..16 {
                            chunk.set_block_state(x, y, z, terrain(0, y, 0));
                        }
                    }
                }
                let mut start = source.clone();
                let placed = start.place_ocean_ruin_in_chunk(
                    &mut chunk,
                    42,
                    &templates,
                    &mut |_, _, _| 50,
                    &mut terrain,
                );
                assert!(
                    placed
                        .iter()
                        .all(|e| matches!(e.get("x"), Some(Nbt::Int(x)) if x >> 4 == cx)
                            && matches!(e.get("z"), Some(Nbt::Int(z)) if z >> 4 == cz))
                );
                assert_eq!(start.pieces[0].template.as_ref().unwrap().position[1], 50);
                entities.extend(placed);
                if (cx, cz) == (0, 0) {
                    let chest = chunk.block_state(15, 51, 15);
                    assert!(
                        block_states()
                            .properties(chest)
                            .unwrap()
                            .1
                            .contains(&("waterlogged", "true"))
                    );
                }
            }
        }
        assert_eq!(
            entities
                .iter()
                .filter(|e| e.get("id") == Some(&Nbt::from("minecraft:brushable_block")))
                .count(),
            5
        );
        assert_eq!(
            entities
                .iter()
                .filter(|e| e.get("id") == Some(&Nbt::from("minecraft:chest")))
                .count(),
            1
        );
        assert!(entities.iter().all(|e| e.get("LootTableSeed").is_some()));
    }

    #[test]
    fn later_template_layers_remove_overwritten_archaeology_entities() {
        let (source, templates) = fixture(1.0);
        // A layer whose archaeology targets sand replaces the first layer's gravel and suspicious
        // variants. Neighboring chunks sample the same unmodified origin height for both layers.
        let mut discarded_count = 0;
        for (cx, cz) in [(0, 1), (1, 0), (1, 1)] {
            let mut chunk = GeneratedChunk::new(cx, cz, -64, 384, 1);
            for z in 0..16 {
                for x in 0..16 {
                    chunk.set_block_state(x, 49, z, terrain(0, 49, 0));
                }
            }
            let discarded = source.clone().place_ocean_ruin_in_chunk(
                &mut chunk.clone(),
                42,
                &templates,
                &mut |_, _, _| 50,
                &mut terrain,
            );
            discarded_count += discarded.len();
            let mut start = source.clone();
            let mut second = start.pieces[0].clone();
            second.save_data.insert("BiomeType", "WARM");
            start.pieces.push(second);
            let entities = start.place_ocean_ruin_in_chunk(
                &mut chunk,
                42,
                &templates,
                &mut |_, _, _| 50,
                &mut terrain,
            );
            assert!(entities.is_empty());
            for z in 14..18 {
                for x in 14..18 {
                    if (x >> 4, z >> 4) == (cx, cz) {
                        assert_eq!(
                            block_states()
                                .properties(chunk.block_state(
                                    (x & 15) as usize,
                                    50,
                                    (z & 15) as usize
                                ))
                                .unwrap()
                                .0,
                            "minecraft:gravel"
                        );
                    }
                }
            }
        }
        assert!(
            discarded_count > 0,
            "the first layer must create archaeology entities here"
        );
    }

    #[test]
    fn chest_markers_survive_zero_integrity() {
        let (mut start, templates) = fixture(0.0);
        let mut chunk = GeneratedChunk::new(0, 0, -64, 384, 1);
        chunk.set_block_state(14, 49, 14, terrain(0, 49, 0));
        let entities = start.place_ocean_ruin_in_chunk(
            &mut chunk,
            42,
            &templates,
            &mut |_, _, _| 50,
            &mut terrain,
        );
        assert_eq!(entities.len(), 1);
        assert_eq!(
            entities[0].get("LootTable"),
            Some(&Nbt::from("minecraft:chests/underwater_ruin_small"))
        );
        assert_eq!(chunk.block_state(14, 50, 14), 0);
    }
}
