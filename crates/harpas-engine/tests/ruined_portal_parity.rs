//! Complete ruined portal placement against vanilla, including writes outside the template bounds.
#[allow(dead_code)]
#[path = "../../harpas-worldgen/tests/reference/mod.rs"]
mod reference;
use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_engine::world::{ChunkPos, ChunkState, World};
use harpas_engine::worldgen::overworld_generator;
use harpas_worldgen::{Beardifier, NoiseBasedChunkGenerator, StructureState};
use harpas_worldgen::{
    GeneratedChunk, HeightmapType, Mirror, Rotation, RuinedPortalGenerator, StructurePiece,
    StructureStart, StructureTemplates, TemplatePiece, WorldgenData,
};
use reference::Reference;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn portal(
    templates: &StructureTemplates,
    name: &str,
    rotation: Rotation,
    mirror: Mirror,
    scenario: usize,
) -> StructureStart {
    let size = templates.get(name).unwrap().size;
    let position = match scenario {
        0 | 1 => [0, 50, 0],
        6 => [-18, -62, 30],
        7 => [-18, 318, 30],
        _ => [-18, 50, 30],
    };
    let template = TemplatePiece {
        template: name.into(),
        position,
        rotation,
        mirror,
        pivot: [size[0] / 2, 0, size[2] / 2],
    };
    let bounding_box = templates.get(name).unwrap().bounding_box_around(
        position,
        rotation,
        mirror,
        template.pivot,
    );
    let mut properties = Compound::new();
    for (key, value) in [
        ("air_pocket", scenario % 2 == 1),
        ("cold", scenario == 2),
        ("overgrown", scenario == 3),
        ("vines", scenario == 3),
        ("replace_with_blackstone", scenario == 4),
    ] {
        properties.insert(key, i8::from(value));
    }
    properties.insert(
        "mossiness",
        match scenario {
            4 => 0.0_f32,
            8 => 1.0,
            _ => 0.2,
        },
    );
    let mut save_data = Compound::new();
    save_data.insert("Properties", properties);
    save_data.insert(
        "VerticalPlacement",
        match scenario {
            1 => "on_ocean_floor",
            5 | 9 => "partly_buried",
            6 => "underground",
            7 => "in_mountain",
            _ => "on_land_surface",
        },
    );
    StructureStart {
        structure: match scenario {
            1 => "minecraft:ruined_portal_ocean",
            2 => "minecraft:ruined_portal_mountain",
            3 => "minecraft:ruined_portal_jungle",
            4 => "minecraft:ruined_portal_desert",
            8 => "minecraft:ruined_portal_swamp",
            _ => "minecraft:ruined_portal",
        }
        .into(),
        chunk: (position[0] >> 4, position[2] >> 4),
        references: 0,
        bounding_box,
        pieces: vec![StructurePiece {
            kind: "minecraft:rupo".into(),
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
fn ruined_portal_blocks_and_metadata_match_vanilla() {
    let Some(reference) = Reference::from_env() else {
        return;
    };
    let mut data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    // This probe compares its own structure pass; mineshaft_parity covers mine blocks.
    data.structure_sets.shift_remove("minecraft:mineshafts");
    let data = Arc::new(data);
    let templates = Arc::new(StructureTemplates::from_zip_path(&reference.server_jar).unwrap());
    let generator = RuinedPortalGenerator::new(&data).unwrap();
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
            "RuinedPortalProbe.java",
            include_str!("../../harpas-worldgen/tests/RuinedPortalProbe.java"),
        ),
    ]);
    for &seed in reference::sampled_seeds(&[0, -4_172_144_997_902_289_642]) {
        let mut inputs = Compound::new();
        let mut expected = Compound::new();
        let mut names: Vec<_> = templates
            .ids()
            .filter(|n| n.starts_with("minecraft:ruined_portal/"))
            .collect();
        names.sort_unstable();
        for (ti, &name) in names.iter().enumerate() {
            for scenario in 0..10 {
                let rotation = Rotation::ALL[(ti + scenario) % 4];
                let mirror = if scenario < 4 {
                    Mirror::None
                } else {
                    Mirror::FrontBack
                };
                let source = portal(&templates, name, rotation, mirror, scenario);
                let height = |x: i32, z: i32| match scenario {
                    6 => -60,
                    7 => 318,
                    8 => 50 + (x + z).rem_euclid(5),
                    9 => 60,
                    _ => 50,
                };
                let terrain = |x: i32, y: i32, z: i32| {
                    block_states()
                        .default_state(if y < height(x, z) {
                            "minecraft:stone"
                        } else if scenario == 5 && y < 63 {
                            "minecraft:lava"
                        } else if scenario == 1 && y < 63 {
                            "minecraft:water"
                        } else {
                            "minecraft:air"
                        })
                        .unwrap()
                };
                let plan =
                    generator.prepare(&source, seed, &templates, -64, 384, &mut |x, y, z| {
                        terrain(x, y, z)
                    });
                let bounds = source.ruined_portal_terrain_bounds().unwrap();
                // Compare every affected chunk, including spread beyond the saved piece bounds.
                for cx in (bounds.min[0] >> 4)..=(bounds.max[0] >> 4) {
                    for cz in (bounds.min[2] >> 4)..=(bounds.max[2] >> 4) {
                        let mut chunk = GeneratedChunk::new(cx, cz, -64, 384, 1);
                        for y in -64..320 {
                            for z in 0..16 {
                                for x in 0..16 {
                                    chunk.set_block_state(
                                        x,
                                        y,
                                        z,
                                        terrain(cx * 16 + x as i32, y, cz * 16 + z as i32),
                                    );
                                }
                            }
                        }
                        let case = format!("synthetic-{seed}-{ti}-{scenario}-{cx}-{cz}");
                        inputs.insert(
                            case.clone(),
                            input_with_terrain(
                                &chunk,
                                seed,
                                std::slice::from_ref(&source),
                                &terrain,
                            ),
                        );
                        let entities = plan.place_in_chunk(&mut chunk);
                        expected.insert(
                            case.clone(),
                            snapshot(&chunk, std::slice::from_ref(&source), entities.clone()),
                        );
                        if ti == 0 || ti == 3 {
                            let source =
                                StructureStart::from_nbt(&source.to_nbt(), &data, &templates)
                                    .unwrap()
                                    .unwrap();
                            let mut inp = input_with_terrain(
                                &chunk,
                                seed,
                                std::slice::from_ref(&source),
                                &terrain,
                            );
                            inp.insert("initial_entities", entity_list(entities.clone()));
                            inputs.insert(format!("resume-{case}"), inp);
                            let replay = generator.prepare(
                                &source,
                                seed,
                                &templates,
                                -64,
                                384,
                                &mut |x, y, z| {
                                    if (x >> 4, z >> 4) == (cx, cz) {
                                        chunk.block_state((x & 15) as usize, y, (z & 15) as usize)
                                    } else {
                                        terrain(x, y, z)
                                    }
                                },
                            );
                            let new = replay.place_in_chunk(&mut chunk);
                            let entities = merge_entities(&chunk, new, entities);
                            expected.insert(
                                format!("resume-{case}"),
                                snapshot(&chunk, std::slice::from_ref(&source), entities),
                            );
                        }
                    }
                }
            }
        }
        compare(&reference, inputs, expected);
    }
    let mut inputs = Compound::new();
    let mut expected = Compound::new();
    worker_cases(&reference, &data, &templates, &mut inputs, &mut expected);
    saved_neighbor_case(&reference, &data, &templates, &mut inputs, &mut expected);
    compare(&reference, inputs, expected);
}

fn compare(reference: &Reference, inputs: Compound, expected: Compound) {
    eprintln!("Comparing {} portal chunks", inputs.len());
    let input_path = reference.temp.0.join("input.nbt");
    let output_path = reference.temp.0.join("output.nbt");
    let mut bytes = Vec::new();
    Nbt::Compound(inputs).write_named("", &mut bytes);
    std::fs::write(&input_path, bytes).unwrap();
    reference.java(
        "RuinedPortalProbe",
        [
            input_path.display().to_string(),
            output_path.display().to_string(),
        ],
    );
    let bytes = std::fs::read(output_path).unwrap();
    let (_, Nbt::Compound(actual)) = Nbt::read_named(&mut bytes.as_slice()).unwrap() else {
        panic!("output")
    };
    assert_eq!(actual.len(), expected.len(), "portal case count");
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
        let survives = (id == "minecraft:brushable_block"
            && matches!(
                name,
                "minecraft:suspicious_sand" | "minecraft:suspicious_gravel"
            ))
            || name == id
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
    terrain: &impl Fn(i32, i32, i32) -> u32,
) -> Compound {
    let mut tag = input(chunk, seed, own);
    let mut columns = Compound::new();
    for start in own {
        if let Some(bounds) = start.ruined_portal_terrain_bounds() {
            for x in bounds.min[0]..=bounds.max[0] {
                for z in bounds.min[2]..=bounds.max[2] {
                    columns.insert(
                        format!("{x},{z}"),
                        Nbt::IntArray((-64..320).map(|y| terrain(x, y, z) as i32).collect()),
                    );
                }
            }
        }
    }
    tag.insert("columns", columns);
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
        let mut targets = Vec::new();
        'search: for r in 0_i32..128 {
            for x in -r..=r {
                for z in -r..=r {
                    if x.abs().max(z.abs()) != r
                        || !state.is_structure_chunk("minecraft:ruined_portals", x, z)
                    {
                        continue;
                    }
                    let own = state
                        .starts_in_chunk(&terrain, data, templates, x, z)
                        .unwrap();
                    for start in own {
                        if start.ruined_portal_terrain_bounds().is_some() {
                            targets.push(start);
                        }
                        if targets.len() == if reference::reduced_sample() { 1 } else { 2 } {
                            break 'search;
                        }
                    }
                }
            }
        }
        assert!(!targets.is_empty(), "natural ruined portal");
        for source in targets {
            eprintln!(
                "Seed {seed}: natural ruined portal at {:?} ({} pieces)",
                source.chunk,
                source.pieces.len()
            );
            let bounds = source.ruined_portal_terrain_bounds().unwrap();
            let mut starts_cache = HashMap::new();
            let mut carved = HashMap::new();
            let mut positions = Vec::new();
            let min_x = bounds.min[0];
            let min_z = bounds.min[2];
            let max_x = bounds.max[0];
            let max_z = bounds.max[2];
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
            let folder = reference.temp.0.join(format!(
                "ruined-portal-world-{seed}-{}-{},{}",
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
                let baseline = carved[&pos].clone();
                if !all.iter().any(|s| s.structure == source.structure) {
                    all.push(source.clone());
                }
                let mut inp = input_with_terrain(&baseline, seed, &all, &block);
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
                if source.bounding_box.intersects_columns(
                    pos.x * 16,
                    pos.z * 16,
                    pos.x * 16 + 15,
                    pos.z * 16 + 15,
                ) {
                    assert_eq!(
                        refs.get(&source.structure),
                        Some(&Nbt::LongArray(vec![packed]))
                    );
                } else {
                    assert!(
                        refs.get(&source.structure).is_none(),
                        "spread does not enlarge saved references"
                    );
                }
                // Reload worker records and compare a fresh portal pass with vanilla. This is a pass-level
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
                            panic!("ruined portal")
                        };
                        *start = StructureStart::from_nbt(tag, data, templates)
                            .unwrap()
                            .unwrap();
                    }
                }
                let mut resumed_input = input_with_terrain(&generated, seed, &all, &block);
                resumed_input.insert("keep_neighbor_starts", 0_i8);
                resumed_input.insert("initial_entities", entity_list(entities.clone()));
                let mut resumed = generated.clone();
                let mut resumed_entities = Vec::new();
                for start in &mut all {
                    let plan = RuinedPortalGenerator::new(data).unwrap().prepare(
                        start,
                        seed,
                        templates,
                        -64,
                        384,
                        &mut |x, y, z| {
                            if (x >> 4, z >> 4) == (pos.x, pos.z) {
                                generated.block_state((x & 15) as usize, y, (z & 15) as usize)
                            } else {
                                block(x, y, z)
                            }
                        },
                    );
                    resumed_entities.retain(|e| !plan.removes_block_entity(e));
                    resumed_entities.extend(plan.place_in_chunk(&mut resumed));
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
        let mut source = portal(
            templates,
            "minecraft:ruined_portal/portal_1",
            Rotation::None,
            Mirror::None,
            if adjusted { 3 } else { 0 },
        );
        let template = source.pieces[0].template.as_mut().unwrap();
        template.position = [6, 50, 14];
        let bounds = templates
            .get(&template.template)
            .unwrap()
            .bounding_box_around(
                template.position,
                template.rotation,
                template.mirror,
                template.pivot,
            );
        source.pieces[0].bounding_box = bounds;
        source.bounding_box = bounds;
        source.chunk = (0, 0);
        let terrain = NoiseBasedChunkGenerator::overworld(seed, data).unwrap();
        let mut carved = HashMap::new();
        let bounds = source.ruined_portal_terrain_bounds().unwrap();
        for x in (bounds.min[0] >> 4)..=(bounds.max[0] >> 4) {
            for z in (bounds.min[2] >> 4)..=(bounds.max[2] >> 4) {
                let pos = ChunkPos { x, z };
                let beard = Beardifier::for_starts(x, z, data, [&source]).unwrap();
                carved.insert(
                    pos,
                    terrain
                        .generate_chunk_carved_with_beardifier(x, z, data, beard)
                        .unwrap(),
                );
            }
        }
        // Saved blocks inside the template must drive protected-block, lava and fluid processing.
        for z in 0..16 {
            for x in 0..16 {
                for y in 49..60 {
                    let state = block_states()
                        .default_state(if (x + z) % 3 == 0 {
                            "minecraft:bedrock"
                        } else if adjusted {
                            "minecraft:lava"
                        } else {
                            "minecraft:water"
                        })
                        .unwrap();
                    carved
                        .get_mut(&origin)
                        .unwrap()
                        .set_block_state(x, y, z, state);
                }
            }
        }
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
        let mut inp = input_with_terrain(
            &carved[&target],
            seed,
            std::slice::from_ref(&source),
            &block,
        );
        inp.insert("keep_neighbor_starts", 0_i8);
        let entities = match nbt.get("block_entities").unwrap() {
            Nbt::List(List::Compound(v)) => v.clone(),
            Nbt::List(List::Empty) => Vec::new(),
            _ => panic!("entities"),
        };
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
            .join(format!("saved-ruined-portal-neighbor-{adjusted}"));
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
