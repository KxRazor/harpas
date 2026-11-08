//! Shipwreck template blocks, saved records and block entities against vanilla.

#[allow(dead_code)]
#[path = "../../harpas-worldgen/tests/reference/mod.rs"]
mod reference;

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_engine::world::{ChunkPos, ChunkState, World};
use harpas_engine::worldgen::overworld_generator;
use harpas_worldgen::{
    Beardifier, GeneratedChunk, HeightmapType, Mirror, NoiseBasedChunkGenerator, Rotation,
    StructurePiece, StructureStart, StructureState, StructureTemplates, TemplatePiece,
    WorldgenData,
};
use reference::Reference;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn shipwreck(
    templates: &StructureTemplates,
    x: i32,
    z: i32,
    rotation: Rotation,
    name: &str,
    beached: bool,
) -> StructureStart {
    let template = TemplatePiece {
        template: format!("minecraft:shipwreck/{name}"),
        position: [x, 90, z],
        rotation,
        mirror: Mirror::None,
        pivot: [4, 0, 15],
    };
    let bounding_box = templates
        .get(&template.template)
        .unwrap()
        .bounding_box_around(template.position, rotation, Mirror::None, template.pivot);
    let mut save_data = Compound::new();
    save_data.insert("isBeached", i8::from(beached));
    save_data.insert("height_adjusted", 0_i8);
    StructureStart {
        structure: if beached {
            "minecraft:shipwreck_beached"
        } else {
            "minecraft:shipwreck"
        }
        .into(),
        chunk: (x >> 4, z >> 4),
        references: 0,
        bounding_box,
        pieces: vec![StructurePiece {
            kind: "minecraft:shipwreck".into(),
            bounding_box,
            orientation: Some(harpas_worldgen::Direction::North),
            gen_depth: 0,
            junctions: Vec::new(),
            jigsaw: None,
            template: Some(template),
            save_data,
        }],
    }
}

fn blocks(chunk: &GeneratedChunk) -> Nbt {
    let mut values = Vec::with_capacity(384 * 256);
    for y in -64..320 {
        for z in 0..16 {
            for x in 0..16 {
                values.push(chunk.block_state(x, y, z) as i32);
            }
        }
    }
    Nbt::IntArray(values)
}

fn starts(starts: &[StructureStart]) -> Compound {
    starts
        .iter()
        .map(|s| (s.structure.clone(), Nbt::Compound(s.to_nbt())))
        .collect()
}

fn input(chunk: &GeneratedChunk, seed: i64, own: &[StructureStart]) -> Compound {
    let mut tag = Compound::new();
    tag.insert("x", chunk.chunk_x());
    tag.insert("z", chunk.chunk_z());
    tag.insert("seed", seed);
    tag.insert("blocks", blocks(chunk));
    tag.insert("PostProcessing", postprocessing(chunk));
    tag.insert("starts", starts(own));
    tag
}

fn snapshot(
    chunk: &GeneratedChunk,
    own: &[StructureStart],
    mut entities: Vec<Compound>,
) -> Compound {
    entities.sort_by_key(packed_position);
    let mut tag = Compound::new();
    tag.insert("blocks", blocks(chunk));
    tag.insert("PostProcessing", postprocessing(chunk));
    tag.insert("starts", starts(own));
    tag.insert(
        "block_entities",
        if entities.is_empty() {
            List::Empty
        } else {
            List::Compound(entities)
        },
    );
    let mut maps = Compound::new();
    for kind in HeightmapType::ALL {
        maps.insert(
            kind.nbt_key(),
            Nbt::LongArray(chunk.heightmaps().pack(kind)),
        );
    }
    tag.insert("Heightmaps", maps);
    tag
}

#[test]
fn shipwreck_blocks_and_metadata_match_vanilla() {
    let Some(reference) = Reference::from_env() else {
        eprintln!("Skipping shipwreck placement: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    let mut data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    // This probe compares its own structure pass; mineshaft_parity covers mine blocks.
    data.structure_sets.shift_remove("minecraft:mineshafts");
    let data = Arc::new(data);
    let templates = Arc::new(StructureTemplates::from_zip_path(&reference.server_jar).unwrap());
    let mut inputs = Compound::new();
    let mut expected = Compound::new();
    for &seed in reference::sampled_seeds(&[0, -4_172_144_997_902_289_642]) {
        let mut names: Vec<_> = templates
            .ids()
            .filter(|id| id.starts_with("minecraft:shipwreck/"))
            .collect();
        names.sort_unstable();
        for (ti, name) in names.iter().enumerate() {
            for scenario in 0..4 {
                let rotation = Rotation::ALL[(ti + scenario) % 4];
                let beached = scenario % 2 == 1;
                let (ox, oz) = if scenario < 2 { (0, 0) } else { (-18, 30) };
                let height = |x: i32, z: i32| match scenario {
                    0 => 45,
                    1 => 65 + (x + 2 * z).rem_euclid(7),
                    2 => -60 + (x + z).rem_euclid(5),
                    _ => 318,
                };
                let terrain = |x: i32, y: i32, z: i32| {
                    if y < height(x, z) {
                        block_states().default_state("minecraft:stone").unwrap()
                    } else if scenario % 2 == 0 && y < 63 {
                        block_states().default_state("minecraft:water").unwrap()
                    } else {
                        0
                    }
                };
                let surface = |kind, x: i32, z: i32| {
                    if kind == HeightmapType::WorldSurfaceWg && scenario % 2 == 0 {
                        63
                    } else {
                        height(x, z)
                    }
                };
                let source = shipwreck(
                    &templates,
                    ox,
                    oz,
                    rotation,
                    name.strip_prefix("minecraft:shipwreck/").unwrap(),
                    beached,
                );
                let bounds = source.bounding_box;
                for cx in (bounds.min[0] >> 4)..=(bounds.max[0] >> 4) {
                    for cz in (bounds.min[2] >> 4)..=(bounds.max[2] >> 4) {
                        let mut chunk = GeneratedChunk::new(cx, cz, -64, 384, 1);
                        for y in -64..320 {
                            for z in 0..16 {
                                for x in 0..16 {
                                    let block = terrain(cx * 16 + x as i32, y, cz * 16 + z as i32);
                                    if block != 0 {
                                        chunk.set_block_state(x, y, z, block);
                                    }
                                }
                            }
                        }
                        let mut own = vec![source.clone()];
                        let name =
                            format!("synthetic-{seed}-{ti}-{rotation:?}-{scenario}-{cx}-{cz}");
                        let inp =
                            input_with_terrain(&chunk, seed, &own, &templates, &terrain, &surface);
                        let entities = own[0].place_shipwreck_in_chunk(
                            &mut chunk,
                            seed,
                            &templates,
                            &mut |kind, x, z| surface(kind, x, z),
                            &mut |x, y, z| terrain(x, y, z),
                        );
                        inputs.insert(name.clone(), inp);
                        expected.insert(name.clone(), snapshot(&chunk, &own, entities.clone()));
                        own[0] = StructureStart::from_nbt(&own[0].to_nbt(), &data, &templates)
                            .unwrap()
                            .unwrap();
                        let mut inp =
                            input_with_terrain(&chunk, seed, &own, &templates, &terrain, &surface);
                        inp.insert("initial_entities", entity_list(entities.clone()));
                        inputs.insert(format!("resume-{name}"), inp);
                        let resumed = own[0].place_shipwreck_in_chunk(
                            &mut chunk,
                            seed,
                            &templates,
                            &mut |kind, x, z| surface(kind, x, z),
                            &mut |x, y, z| terrain(x, y, z),
                        );
                        let entities = merge_entities(&chunk, resumed, entities);
                        expected.insert(format!("resume-{name}"), snapshot(&chunk, &own, entities));
                    }
                }
            }
        }
    }
    eprintln!(
        "Comparing {} synthetic placements and replays",
        inputs.len()
    );

    worker_cases(&reference, &data, &templates, &mut inputs, &mut expected);
    saved_neighbor_case(&reference, &data, &templates, &mut inputs, &mut expected);
    eprintln!(
        "Comparing {} total placements and replays, including workers and saved neighbors",
        inputs.len()
    );
    reference.compile(&[
        (
            "StructurePlacementProbe.java",
            include_str!("../../harpas-worldgen/tests/StructurePlacementProbe.java"),
        ),
        (
            "BuriedTreasureProbe.java",
            include_str!("../../harpas-worldgen/tests/BuriedTreasureProbe.java"),
        ),
        (
            "ShipwreckProbe.java",
            include_str!("../../harpas-worldgen/tests/ShipwreckProbe.java"),
        ),
    ]);
    let input_path = reference.temp.0.join("input.nbt");
    let output_path = reference.temp.0.join("output.nbt");
    let mut bytes = Vec::new();
    Nbt::Compound(inputs).write_named("", &mut bytes);
    std::fs::write(&input_path, bytes).unwrap();
    reference.java(
        "ShipwreckProbe",
        [
            input_path.display().to_string(),
            output_path.display().to_string(),
        ],
    );
    let bytes = std::fs::read(output_path).unwrap();
    let (_, Nbt::Compound(actual)) = Nbt::read_named(&mut bytes.as_slice()).unwrap() else {
        panic!("output")
    };
    let mut failures = Vec::new();
    for (name, tag) in expected.iter() {
        let Nbt::Compound(mut expected) = tag.clone() else {
            unreachable!()
        };
        let Some(Nbt::Compound(mut actual)) = actual.get(name).cloned() else {
            panic!("missing case")
        };
        if name.starts_with("worker-") {
            let Some(Nbt::Compound(mut maps)) = actual.remove("Heightmaps") else {
                panic!("maps")
            };
            maps.remove("WORLD_SURFACE_WG");
            maps.remove("OCEAN_FLOOR_WG");
            actual.insert("Heightmaps", maps);
        }
        let Some(Nbt::IntArray(want)) = expected.remove("blocks") else {
            panic!("blocks")
        };
        let Some(Nbt::IntArray(got)) = actual.remove("blocks") else {
            panic!("blocks")
        };
        assert_eq!(want.len(), got.len(), "{name} block count");
        assert_eq!(expected.len(), actual.len(), "{name} metadata field count");
        let mut diffs = Vec::new();
        for (i, (&a, &b)) in want.iter().zip(&got).enumerate() {
            if a != b && diffs.len() < 30 {
                diffs.push(format!(
                    "({},{},{}) Rust={:?} vanilla={:?}",
                    i % 16,
                    (i / 256) as i32 - 64,
                    (i / 16) % 16,
                    block_states().properties(a as u32),
                    block_states().properties(b as u32)
                ));
            }
        }
        if !diffs.is_empty() {
            failures.push(format!("{name}: {}", diffs.join("\n")));
        }
        for (k, v) in expected.iter() {
            if Some(v) != actual.get(k) {
                failures.push(format!(
                    "{name} {k}: Rust={v:?} vanilla={:?}",
                    actual.get(k)
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
fn postprocessing(chunk: &GeneratedChunk) -> Nbt {
    let mut sections = vec![Vec::new(); chunk.sections().len()];
    for &(x, y, z) in chunk.fluid_postprocessing() {
        sections[((y >> 4) - chunk.min_section_y()) as usize]
            .push((u16::from(x) | ((y as u16 & 15) << 4) | (u16::from(z) << 8)) as i16);
    }
    Nbt::List(List::List(
        sections
            .into_iter()
            .map(|v| {
                if v.is_empty() {
                    List::Empty
                } else {
                    List::Short(v)
                }
            })
            .collect(),
    ))
}

fn int(entity: &Compound, key: &str) -> i32 {
    let Some(Nbt::Int(v)) = entity.get(key) else {
        panic!("missing {key}")
    };
    *v
}
fn packed_position(entity: &Compound) -> i64 {
    ((i64::from(int(entity, "x")) & 0x3ff_ffff) << 38)
        | ((i64::from(int(entity, "z")) & 0x3ff_ffff) << 12)
        | (i64::from(int(entity, "y")) & 0xfff)
}
fn entity_list(entities: Vec<Compound>) -> Nbt {
    if entities.is_empty() {
        Nbt::List(List::Empty)
    } else {
        Nbt::List(List::Compound(entities))
    }
}

fn merge_entities(
    chunk: &GeneratedChunk,
    mut new: Vec<Compound>,
    old: Vec<Compound>,
) -> Vec<Compound> {
    for entity in old {
        let x = int(&entity, "x");
        let y = int(&entity, "y");
        let z = int(&entity, "z");
        let (name, _) = block_states()
            .properties(chunk.block_state((x & 15) as usize, y, (z & 15) as usize))
            .unwrap();
        let id = match entity.get("id") {
            Some(Nbt::String(id)) => id.as_str(),
            _ => panic!("block entity id"),
        };
        let survives = name == id
            || (id == "minecraft:bed" && name.ends_with("_bed"))
            || (id == "minecraft:sign" && name.ends_with("_sign"));
        if survives
            && !new
                .iter()
                .any(|e| packed_position(e) == packed_position(&entity))
        {
            new.push(entity);
        }
    }
    new
}
fn input_with_terrain(
    chunk: &GeneratedChunk,
    seed: i64,
    own: &[StructureStart],
    templates: &StructureTemplates,
    terrain: &impl Fn(i32, i32, i32) -> u32,
    height: &impl Fn(HeightmapType, i32, i32) -> i32,
) -> Compound {
    let mut tag = input(chunk, seed, own);
    let mut heights = Compound::new();
    for start in own {
        for (x, z) in start.shipwreck_height_samples(templates) {
            heights.insert(
                format!("{x},{z}"),
                Nbt::IntArray(vec![
                    height(HeightmapType::WorldSurfaceWg, x, z),
                    height(HeightmapType::OceanFloorWg, x, z),
                ]),
            );
        }
    }
    tag.insert("heights", heights);
    let mut surrounding = Vec::new();
    for y in -64..320 {
        for z in -1..=16 {
            for x in -1..=16 {
                surrounding
                    .push(terrain(chunk.chunk_x() * 16 + x, y, chunk.chunk_z() * 16 + z) as i32);
            }
        }
    }
    tag.insert("surrounding", Nbt::IntArray(surrounding));
    tag
}

fn worker_cases(
    reference: &Reference,
    data: &Arc<WorldgenData>,
    templates: &Arc<StructureTemplates>,
    inputs: &mut Compound,
    expected: &mut Compound,
) {
    let biomes: Vec<_> = data.biomes.keys().map(String::as_str).collect();
    for &seed in reference::sampled_seeds(&[0, -4_172_144_997_902_289_642]) {
        let terrain = NoiseBasedChunkGenerator::overworld(seed, data).unwrap();
        let state = StructureState::new(seed, data, &terrain).unwrap();
        let mut targets: [Option<StructureStart>; 2] = [None, None];
        'search: for r in 0_i32..1024 {
            for x in -r..=r {
                for z in -r..=r {
                    if x.abs().max(z.abs()) != r
                        || !state.is_structure_chunk("minecraft:shipwrecks", x, z)
                    {
                        continue;
                    }
                    let own = state
                        .starts_in_chunk(&terrain, data, templates, x, z)
                        .unwrap();
                    if let Some(shipwreck) = own.into_iter().find(|s| {
                        matches!(
                            s.structure.as_str(),
                            "minecraft:shipwreck" | "minecraft:shipwreck_beached"
                        )
                    }) {
                        let variant =
                            usize::from(shipwreck.structure == "minecraft:shipwreck_beached");
                        if targets[variant].is_none() {
                            targets[variant] = Some(shipwreck);
                        }
                        if targets.iter().all(Option::is_some) {
                            break 'search;
                        }
                    }
                }
            }
        }
        let mut targets: Vec<_> = targets
            .into_iter()
            .map(|s| s.expect("natural shipwreck variant"))
            .collect();
        // This natural treasure chunk also intersects a shipwreck. Keep it as an integration case:
        // the treasure pass runs first, then vanilla places the shipwreck over that terrain.
        let mut overlap = None;
        for x in -38..=-36 {
            for z in 24..=26 {
                let starts = state
                    .starts_in_chunk(&terrain, data, templates, x, z)
                    .unwrap();
                overlap = overlap.or_else(|| {
                    starts.into_iter().find(|s| {
                        matches!(
                            s.structure.as_str(),
                            "minecraft:shipwreck" | "minecraft:shipwreck_beached"
                        ) && s.bounding_box.intersects_columns(
                            -37 * 16,
                            25 * 16,
                            -37 * 16 + 15,
                            25 * 16 + 15,
                        )
                    })
                });
            }
        }
        if seed == 0 {
            targets.push(overlap.expect("shipwreck overlapping the natural treasure"));
        }
        for source in targets {
            eprintln!(
                "Seed {seed}: natural shipwreck at {:?} ({} pieces)",
                source.chunk,
                source.pieces.len()
            );
            let bounds = source.bounding_box;
            let mut starts_cache = HashMap::new();
            let mut carved = HashMap::new();
            let mut positions = Vec::new();
            let template = source.pieces[0].template.as_ref().unwrap();
            let size = templates.get(&template.template).unwrap().size;
            let min_x = (bounds.min[0] - 1).min(template.position[0]);
            let min_z = (bounds.min[2] - 1).min(template.position[2]);
            let max_x = (bounds.max[0] + 1).max(template.position[0] + size[0] - 1);
            let max_z = (bounds.max[2] + 1).max(template.position[2] + size[2] - 1);
            for x in (min_x >> 4)..=(max_x >> 4) {
                for z in (min_z >> 4)..=(max_z >> 4) {
                    let mut nearby = Vec::new();
                    for nx in x - 8..=x + 8 {
                        for nz in z - 8..=z + 8 {
                            let starts = starts_cache.entry((nx, nz)).or_insert_with(|| {
                                state
                                    .starts_in_chunk(&terrain, data, templates, nx, nz)
                                    .unwrap()
                            });
                            nearby.extend(starts.iter().cloned());
                        }
                    }
                    let beardifier = Beardifier::for_starts(x, z, data, &nearby).unwrap();
                    let generated = terrain
                        .generate_chunk_carved_with_beardifier(x, z, data, beardifier)
                        .unwrap();
                    let pos = ChunkPos { x, z };
                    carved.insert(pos, generated);
                    if bounds.intersects_columns(x * 16, z * 16, x * 16 + 15, z * 16 + 15) {
                        positions.push(pos);
                    }
                }
            }
            let block = |x: i32, y: i32, z: i32| {
                carved
                    .get(&ChunkPos {
                        x: x >> 4,
                        z: z >> 4,
                    })
                    .map_or(0, |c| {
                        c.block_state((x & 15) as usize, y, (z & 15) as usize)
                    })
            };
            let surface = |kind, x: i32, z: i32| {
                carved
                    .get(&ChunkPos {
                        x: x >> 4,
                        z: z >> 4,
                    })
                    .map_or_else(
                        || terrain.base_height(x, z, kind),
                        |c| {
                            c.heightmaps()
                                .get(kind, (x & 15) as usize, (z & 15) as usize)
                        },
                    )
            };
            let mut prepared_source = source.clone();
            prepared_source
                .prepare_shipwreck_height(seed, templates, &mut |kind, x, z| surface(kind, x, z));
            assert!(bounds.intersects_columns(
                source.chunk.0 * 16,
                source.chunk.1 * 16,
                source.chunk.0 * 16 + 15,
                source.chunk.1 * 16 + 15
            ));
            let folder = reference.temp.0.join(format!(
                "shipwreck-world-{seed}-{}-{},{}",
                source.structure.replace(':', "-"),
                source.chunk.0,
                source.chunk.1
            ));
            let generator =
                overworld_generator(seed, Arc::clone(data), Arc::clone(templates), true, &biomes)
                    .unwrap();
            let mut world = World::open_with_generator(&folder, &biomes, generator).unwrap();
            wait(&mut world, &positions);
            let mut saved = Vec::new();
            for &pos in &positions {
                let Some(ChunkState::Loaded(chunk)) = world.chunk(pos) else {
                    panic!("worker chunk")
                };
                let nbt = chunk.to_nbt(&biomes);
                let mut all = starts_cache[&(pos.x, pos.z)].clone();
                let mut baseline = carved[&pos].clone();
                let mut initial_entities = Vec::new();
                for start in &mut all {
                    if start.structure == "minecraft:buried_treasure" {
                        initial_entities.extend(start.place_in_chunk(
                            &mut baseline,
                            seed,
                            &mut |_, _| {
                                unreachable!("treasure does not sample neighboring heights")
                            },
                            &mut |_, _, _| {
                                unreachable!("treasure does not read neighboring blocks")
                            },
                        ));
                    }
                }
                if !all.iter().any(|s| s.structure == source.structure) {
                    all.push(prepared_source.clone());
                }
                let mut inp =
                    input_with_terrain(&baseline, seed, &all, templates, &block, &surface);
                inp.insert("initial_entities", entity_list(initial_entities));
                inp.insert("keep_neighbor_starts", 0_i8);

                let name = format!("worker-{seed}-{},{}", pos.x, pos.z);
                inputs.insert(name.clone(), inp.clone());
                let mut generated = GeneratedChunk::new(pos.x, pos.z, -64, 384, biomes.len());
                for section in &chunk.sections {
                    if let Some(blocks) = &section.section {
                        let i = (i32::from(section.y) - generated.min_section_y()) as usize;
                        generated.sections_mut()[i] = blocks.clone();
                    }
                }
                generated.recompute_heightmaps();
                let Some(Nbt::Compound(records)) = nbt.get("structures") else {
                    panic!("structures")
                };
                let mut actual = Compound::new();
                actual.insert("blocks", blocks(&generated));
                actual.insert("starts", records.get("starts").unwrap().clone());
                actual.insert("Heightmaps", nbt.get("Heightmaps").unwrap().clone());
                actual.insert("PostProcessing", nbt.get("PostProcessing").unwrap().clone());
                let mut entities = match nbt.get("block_entities").unwrap() {
                    Nbt::List(List::Compound(v)) => v.clone(),
                    Nbt::List(List::Empty) => Vec::new(),
                    _ => panic!("entities"),
                };
                entities.sort_by_key(packed_position);
                actual.insert("block_entities", entity_list(entities.clone()));
                expected.insert(name.clone(), actual.clone());
                let Some(Nbt::Compound(refs)) = records.get("References") else {
                    panic!("references")
                };
                let packed =
                    i64::from(source.chunk.0 as u32) | (i64::from(source.chunk.1 as u32) << 32);
                assert_eq!(
                    refs.get(&source.structure),
                    Some(&Nbt::LongArray(vec![packed]))
                );
                // Reload worker records and compare a fresh shipwreck pass with vanilla. This is a pass-level
                // resumption check; the complete FEATURES stage is still outside this task.
                let mut all = all;
                for start in &mut all {
                    if start.structure == source.structure
                        && pos
                            == (ChunkPos {
                                x: source.chunk.0,
                                z: source.chunk.1,
                            })
                    {
                        let Some(Nbt::Compound(own)) = records.get("starts") else {
                            panic!("starts")
                        };
                        let Some(Nbt::Compound(tag)) = own.get(&source.structure) else {
                            panic!("shipwreck")
                        };
                        *start = StructureStart::from_nbt(tag, data, templates)
                            .unwrap()
                            .unwrap();
                    }
                }
                let mut resumed_input =
                    input_with_terrain(&generated, seed, &all, templates, &block, &surface);
                resumed_input.insert("keep_neighbor_starts", 0_i8);
                resumed_input.insert("initial_entities", entity_list(entities.clone()));
                let mut resumed = generated.clone();
                let mut resumed_entities = Vec::new();
                for start in &mut all {
                    resumed_entities.extend(start.place_shipwreck_in_chunk(
                        &mut resumed,
                        seed,
                        templates,
                        &mut |kind, x, z| surface(kind, x, z),
                        &mut |x, y, z| block(x, y, z),
                    ));
                }
                let own: Vec<_> = all
                    .into_iter()
                    .filter(|s| s.chunk == (pos.x, pos.z))
                    .collect();
                let mut resumed_snapshot = snapshot(
                    &resumed,
                    &own,
                    merge_entities(&resumed, resumed_entities, entities),
                );
                if let Some(Nbt::Compound(mut maps)) = resumed_snapshot.remove("Heightmaps") {
                    maps.remove("WORLD_SURFACE_WG");
                    maps.remove("OCEAN_FLOOR_WG");
                    resumed_snapshot.insert("Heightmaps", maps);
                }
                inputs.insert(format!("worker-resume-{name}"), resumed_input);
                expected.insert(format!("worker-resume-{name}"), resumed_snapshot);
                saved.push((pos, nbt));
            }
            world.flush().unwrap();
            drop(world);
            let reverse_folder = folder.with_extension("reverse");
            let generator =
                overworld_generator(seed, Arc::clone(data), Arc::clone(templates), true, &biomes)
                    .unwrap();
            let mut reversed_world =
                World::open_with_generator(&reverse_folder, &biomes, generator).unwrap();
            let reversed: Vec<_> = positions.iter().rev().copied().collect();
            wait(&mut reversed_world, &reversed);
            for (pos, nbt) in &saved {
                let Some(ChunkState::Loaded(chunk)) = reversed_world.chunk(*pos) else {
                    panic!("reverse worker order")
                };
                assert_eq!(
                    chunk.to_nbt(&biomes),
                    *nbt,
                    "worker request order {seed} {pos:?}"
                );
            }
            drop(reversed_world);
            let mut world = World::open(&folder, &biomes).unwrap();
            wait(&mut world, &positions);
            for (pos, nbt) in saved {
                let Some(ChunkState::Loaded(chunk)) = world.chunk(pos) else {
                    panic!("Anvil reload")
                };
                assert_eq!(chunk.to_nbt(&biomes), nbt, "Anvil reload {pos:?}");
            }
        }
    }
}

fn wait(world: &mut World, positions: &[ChunkPos]) {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        world.update(positions.iter().copied()).unwrap();
        if positions
            .iter()
            .all(|p| matches!(world.chunk(*p), Some(ChunkState::Loaded(_))))
        {
            return;
        }
        for pos in positions {
            if let Some(ChunkState::Failed(error)) = world.chunk(*pos) {
                panic!("worker {pos:?}: {error}");
            }
        }
        assert!(Instant::now() < deadline, "worker timeout");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn saved_neighbor_case(
    reference: &Reference,
    data: &Arc<WorldgenData>,
    templates: &Arc<StructureTemplates>,
    inputs: &mut Compound,
    expected: &mut Compound,
) {
    let biomes: Vec<_> = data.biomes.keys().map(String::as_str).collect();
    let seed = 42;
    let target = ChunkPos { x: 0, z: 1 };
    let origin = ChunkPos { x: 0, z: 0 };
    for adjusted in [false, true] {
        let mut source = shipwreck(templates, 6, 14, Rotation::None, "rightsideup_full", false);
        if adjusted {
            let piece = &mut source.pieces[0];
            let template = piece.template.as_mut().unwrap();
            template.position[1] = 95;
            piece.save_data.insert("height_adjusted", 1_i8);
            piece.bounding_box = templates
                .get(&template.template)
                .unwrap()
                .bounding_box_around(
                    template.position,
                    template.rotation,
                    template.mirror,
                    template.pivot,
                );
            source.bounding_box = piece.bounding_box;
        }
        let terrain = NoiseBasedChunkGenerator::overworld(seed, data).unwrap();
        let mut carved = HashMap::new();
        for pos in [
            origin,
            target,
            ChunkPos { x: 0, z: 2 },
            ChunkPos { x: 1, z: 0 },
            ChunkPos { x: 1, z: 1 },
            ChunkPos { x: 1, z: 2 },
        ] {
            let beard = Beardifier::for_starts(pos.x, pos.z, data, [&source]).unwrap();
            carved.insert(
                pos,
                terrain
                    .generate_chunk_carved_with_beardifier(pos.x, pos.z, data, beard)
                    .unwrap(),
            );
        }
        // Raise saved ocean-floor columns to make the footprint's average differ from regenerated terrain.
        let stone = block_states().default_state("minecraft:stone").unwrap();
        let saved = carved.get_mut(&origin).unwrap();
        for z in 0..16 {
            for x in 0..16 {
                saved.set_block_state(x, 200, z, stone);
            }
        }
        // A carved chunk's worldgen heightmaps no longer follow block changes, but a saved one gets them from
        // its blocks again when it loads, in vanilla and in the workers.
        saved.recompute_heightmaps();
        let mut saved_source =
            harpas_engine::worldgen::into_anvil(carved[&origin].clone()).unwrap();
        let mut records = Compound::new();
        records.insert("starts", starts(std::slice::from_ref(&source)));
        records.insert("References", Compound::new());
        saved_source.rest.insert("structures", records);
        let source_nbt = saved_source.to_nbt(&biomes);
        let mut empty = Compound::new();
        empty.insert("Status", "minecraft:carvers");
        let mut records = Compound::new();
        records.insert("starts", Compound::new());
        records.insert("References", Compound::new());
        empty.insert("structures", records);
        let generator =
            overworld_generator(seed, Arc::clone(data), Arc::clone(templates), true, &biomes)
                .unwrap();
        let result = generator(target, &|p| {
            Ok(Some(if p == origin {
                source_nbt.clone()
            } else {
                empty.clone()
            }))
        })
        .unwrap();
        let nbt = result.to_nbt(&biomes);
        let mut generated = GeneratedChunk::new(target.x, target.z, -64, 384, biomes.len());
        for section in &result.sections {
            if let Some(blocks) = &section.section {
                let index = (i32::from(section.y) - generated.min_section_y()) as usize;
                generated.sections_mut()[index] = blocks.clone();
            }
        }
        generated.recompute_heightmaps();
        let block = |x: i32, y: i32, z: i32| {
            carved
                .get(&ChunkPos {
                    x: x >> 4,
                    z: z >> 4,
                })
                .map_or(0, |c| {
                    c.block_state((x & 15) as usize, y, (z & 15) as usize)
                })
        };
        let surface = |kind, x: i32, z: i32| {
            carved
                .get(&ChunkPos {
                    x: x >> 4,
                    z: z >> 4,
                })
                .map_or_else(
                    || terrain.base_height(x, z, kind),
                    |c| {
                        c.heightmaps()
                            .get(kind, (x & 15) as usize, (z & 15) as usize)
                    },
                )
        };
        let mut inp = input_with_terrain(
            &carved[&target],
            seed,
            std::slice::from_ref(&source),
            templates,
            &block,
            &surface,
        );
        inp.insert("keep_neighbor_starts", 0_i8);
        let entities = match nbt.get("block_entities").unwrap() {
            Nbt::List(List::Compound(v)) => v.clone(),
            Nbt::List(List::Empty) => Vec::new(),
            _ => panic!("entities"),
        };
        assert!(
            !entities.is_empty(),
            "saved-neighbor shipwreck placed no block entities"
        );
        let mut snapshot = snapshot(&generated, &[], entities);
        snapshot.insert("Heightmaps", nbt.get("Heightmaps").unwrap().clone());
        snapshot.insert("PostProcessing", nbt.get("PostProcessing").unwrap().clone());
        inputs.insert(format!("worker-saved-neighbor-{adjusted}"), inp);
        expected.insert(format!("worker-saved-neighbor-{adjusted}"), snapshot);
        let Some(Nbt::Compound(records)) = nbt.get("structures") else {
            panic!("records")
        };
        assert_eq!(records.get("starts"), Some(&Nbt::Compound(Compound::new())));
        let Some(Nbt::Compound(refs)) = records.get("References") else {
            panic!("refs")
        };
        assert_eq!(refs.get(&source.structure), Some(&Nbt::LongArray(vec![0])));
        let folder = reference
            .temp
            .0
            .join(format!("saved-shipwreck-neighbor-{adjusted}"));
        let expected_nbt = nbt.clone();
        let generator = Arc::new(move |pos, _: &harpas_engine::world::ChunkReader<'_>| {
            generator(pos, &|p| {
                Ok(Some(if p == origin {
                    source_nbt.clone()
                } else {
                    empty.clone()
                }))
            })
        });
        let mut world = World::open_with_generator(&folder, &biomes, generator).unwrap();
        wait(&mut world, &[target]);
        let Some(ChunkState::Loaded(chunk)) = world.chunk(target) else {
            panic!("neighbor worker")
        };
        assert_eq!(chunk.to_nbt(&biomes), expected_nbt);
        world.flush().unwrap();
        drop(world);
        let mut world = World::open(&folder, &biomes).unwrap();
        wait(&mut world, &[target]);
        let Some(ChunkState::Loaded(chunk)) = world.chunk(target) else {
            panic!("neighbor reload")
        };
        assert_eq!(chunk.to_nbt(&biomes), expected_nbt);
    }
}
