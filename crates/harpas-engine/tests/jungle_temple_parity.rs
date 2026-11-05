//! Jungle temple block placement and metadata against vanilla.

#[allow(dead_code)]
#[path = "../../harpas-worldgen/tests/reference/mod.rs"]
mod reference;

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_engine::world::{ChunkPos, ChunkState, World};
use harpas_engine::worldgen::overworld_generator;
use harpas_worldgen::{
    Beardifier, BoundingBox, GeneratedChunk, HeightmapType, NoiseBasedChunkGenerator,
    StructurePiece, StructureStart, StructureState, StructureTemplates, WorldgenData,
};
use reference::Reference;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn temple(x: i32, z: i32, direction: harpas_worldgen::Direction) -> StructureStart {
    let bounds = BoundingBox::oriented([x, 64, z], direction, 12, 10, 15);
    let mut save_data = Compound::new();
    for (key, value) in [("Width", 12), ("Height", 10), ("Depth", 15), ("HPos", -1)] {
        save_data.insert(key, value);
    }
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
            orientation: Some(direction),
            gen_depth: 0,
            junctions: Vec::new(),
            jigsaw: None,
            template: None,
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
fn temple_blocks_and_metadata_match_vanilla() {
    let Some(reference) = Reference::from_env() else {
        eprintln!("Skipping temple placement: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    let mut inputs = Compound::new();
    let mut expected = Compound::new();
    let mut data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    // This probe compares its own structure pass; mineshaft_parity covers mine blocks.
    data.structure_sets.shift_remove("minecraft:mineshafts");
    let data = Arc::new(data);
    let templates = Arc::new(StructureTemplates::from_zip_path(&reference.server_jar).unwrap());
    use harpas_worldgen::Direction::{East, North, South, West};
    for &seed in reference::sampled_seeds(&[0, -4_172_144_997_902_289_642]) {
        for direction in [North, East, South, West] {
            for scenario in 0..11 {
                let (ox, oz) = if scenario % 2 == 0 { (0, 0) } else { (-26, 54) };
                let height = |x: i32, z: i32| {
                    if scenario == 9 {
                        -20 + (x + 2 * z).rem_euclid(7)
                    } else if scenario == 1 {
                        56 + (x + 2 * z).rem_euclid(7)
                    } else {
                        56
                    }
                };
                let terrain = |x: i32, y: i32, z: i32| {
                    if y == height(x, z) - 1 {
                        block_states().default_state("minecraft:stone").unwrap()
                    } else if scenario == 2 && (56..=63).contains(&y) {
                        block_states().default_state("minecraft:water").unwrap()
                    } else {
                        0
                    }
                };
                let bounds = temple(ox, oz, direction).bounding_box;
                for cx in (bounds.min[0] >> 4)..=(bounds.max[0] >> 4) {
                    for cz in (bounds.min[2] >> 4)..=(bounds.max[2] >> 4) {
                        let mut chunk = GeneratedChunk::new(cx, cz, -64, 384, 1);
                        for y in -64..320 {
                            for z in 0..16 {
                                for x in 0..16 {
                                    let b = terrain(cx * 16 + x as i32, y, cz * 16 + z as i32);
                                    if b != 0 {
                                        chunk.set_block_state(x, y, z, b);
                                    }
                                }
                            }
                        }
                        let mut own = vec![temple(ox, oz, direction)];
                        if scenario != 1 {
                            own[0].pieces[0].save_data.insert("HPos", 64);
                        }
                        for (i, key) in [
                            "placedTrap1",
                            "placedTrap2",
                            "placedMainChest",
                            "placedHiddenChest",
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            if scenario == [3, 5, 6, 7][i] || scenario == 8 {
                                own[0].pieces[0].save_data.insert(key, 1_i8);
                            }
                        }
                        if scenario == 9 {
                            own[0].pieces[0].save_data.insert("HPos", -1);
                        }
                        if scenario == 10 {
                            own[0].pieces[0].bounding_box.min[1] = 317;
                            own[0].pieces[0].bounding_box.max[1] = 326;
                        }
                        if scenario == 4 {
                            own[0].pieces[0].bounding_box.min[1] = -60;
                            own[0].pieces[0].bounding_box.max[1] = -51;
                            own[0].pieces[0].save_data.insert("HPos", 0);
                        }
                        let name = format!("synthetic-{seed}-{direction:?}-{scenario}-{cx}-{cz}");
                        let mut inp = input_with_terrain(&chunk, seed, &own, &terrain);
                        let entities = own[0].place_in_chunk(
                            &mut chunk,
                            seed,
                            &mut |x, z| height(x, z),
                            &mut |x, y, z| terrain(x, y, z),
                        );
                        inputs.insert(name.clone(), inp.clone());
                        expected.insert(name.clone(), snapshot(&chunk, &own, entities.clone()));
                        // Vanilla loads Harpas's saved records and block entities before a second FEATURES attempt.
                        own[0] = StructureStart::from_nbt(&own[0].to_nbt(), &data, &templates)
                            .unwrap()
                            .unwrap();
                        inp = input_with_terrain(&chunk, seed, &own, &terrain);
                        inp.insert("initial_entities", entity_list(entities.clone()));
                        inputs.insert(format!("resume-{name}"), inp);
                        let mut resumed = own[0].place_in_chunk(
                            &mut chunk,
                            seed,
                            &mut |x, z| height(x, z),
                            &mut |x, y, z| terrain(x, y, z),
                        );
                        for entity in entities {
                            let x = int(&entity, "x");
                            let y = int(&entity, "y");
                            let z = int(&entity, "z");
                            let current =
                                chunk.block_state((x & 15) as usize, y, (z & 15) as usize);
                            let kind = if entity.get("id") == Some(&Nbt::from("minecraft:chest")) {
                                "minecraft:chest"
                            } else {
                                "minecraft:dispenser"
                            };
                            if block_states().properties(current).unwrap().0 == kind
                                && !resumed
                                    .iter()
                                    .any(|e| packed_position(e) == packed_position(&entity))
                            {
                                resumed.push(entity);
                            }
                        }
                        expected.insert(format!("resume-{name}"), snapshot(&chunk, &own, resumed));
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
            "JungleTempleProbe.java",
            include_str!("../../harpas-worldgen/tests/JungleTempleProbe.java"),
        ),
    ]);
    let input_path = reference.temp.0.join("input.nbt");
    let output_path = reference.temp.0.join("output.nbt");
    let mut bytes = Vec::new();
    Nbt::Compound(inputs).write_named("", &mut bytes);
    std::fs::write(&input_path, bytes).unwrap();
    reference.java(
        "JungleTempleProbe",
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
fn input_with_terrain(
    chunk: &GeneratedChunk,
    seed: i64,
    own: &[StructureStart],
    terrain: &impl Fn(i32, i32, i32) -> u32,
) -> Compound {
    let mut tag = input(chunk, seed, own);
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
        let mut target = None;
        'search: for r in 0_i32..1024 {
            for x in -r..=r {
                for z in -r..=r {
                    if x.abs().max(z.abs()) != r
                        || !state.is_structure_chunk("minecraft:jungle_temples", x, z)
                    {
                        continue;
                    }
                    let own = state
                        .starts_in_chunk(&terrain, data, templates, x, z)
                        .unwrap();
                    if let Some(temple) = own
                        .into_iter()
                        .find(|s| s.structure == "minecraft:jungle_pyramid")
                    {
                        target = Some(temple);
                        break 'search;
                    }
                }
            }
        }
        let source = target.expect("natural jungle temple");
        eprintln!("Seed {seed}: natural temple at {:?}", source.chunk);
        let bounds = source.pieces[0].bounding_box;
        let mut starts_cache = HashMap::new();
        let mut carved = HashMap::new();
        let mut positions = Vec::new();
        for x in (bounds.min[0] >> 4)..=(bounds.max[0] >> 4) {
            for z in (bounds.min[2] >> 4)..=(bounds.max[2] >> 4) {
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
                positions.push(pos);
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
        let folder = reference.temp.0.join(format!("temple-world-{seed}"));
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
            let mut inp =
                input_with_terrain(&carved[&pos], seed, std::slice::from_ref(&source), &block);
            let mut all = starts_cache[&(pos.x, pos.z)].clone();
            if !all.iter().any(|s| s.structure == source.structure) {
                all.push(source.clone());
            }
            inp.insert("starts", starts(&all));
            inp.insert("keep_neighbor_starts", 0_i8);
            inp.insert("defer_containers", 1_i8);
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
                refs.get("minecraft:jungle_pyramid"),
                Some(&Nbt::LongArray(vec![packed]))
            );
            // Reload the exact worker records in vanilla, including deferred container flags. A complete
            // second temple pass must reproduce the saved blocks and loot, before other features run.
            let mut all = all;
            for start in &mut all {
                if start.structure == source.structure {
                    *start = if pos
                        == (ChunkPos {
                            x: source.chunk.0,
                            z: source.chunk.1,
                        }) {
                        let Some(Nbt::Compound(own)) = records.get("starts") else {
                            panic!("starts")
                        };
                        let Some(Nbt::Compound(tag)) = own.get(&source.structure) else {
                            panic!("temple")
                        };
                        StructureStart::from_nbt(tag, data, templates)
                            .unwrap()
                            .unwrap()
                    } else {
                        source.clone()
                    };
                }
            }
            inp.insert("blocks", blocks(&generated));
            inp.insert("starts", starts(&all));
            inp.insert("initial_entities", entity_list(entities));
            inputs.insert(format!("worker-resume-{name}"), inp);
            expected.insert(format!("worker-resume-{name}"), actual);
            saved.push((pos, nbt));
        }
        world.flush().unwrap();
        drop(world);
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

#[test]
fn saved_neighbor_temples_keep_their_height_and_source_record() {
    let Some(reference) = Reference::from_env() else {
        eprintln!("Skipping saved temple neighbor: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    let mut data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    // This probe compares its own structure pass; mineshaft_parity covers mine blocks.
    data.structure_sets.shift_remove("minecraft:mineshafts");
    let data = Arc::new(data);
    let templates = Arc::new(StructureTemplates::from_zip_path(&reference.server_jar).unwrap());
    let biomes: Vec<_> = data.biomes.keys().map(String::as_str).collect();
    let seed = 42;
    let pos = ChunkPos { x: 0, z: 1 };
    let mut source = temple(6, 6, harpas_worldgen::Direction::South);
    source.pieces[0].save_data.insert("HPos", 64);
    source.pieces[0].save_data.insert("placedTrap1", 1_i8);
    let original = source.to_nbt();
    let terrain = NoiseBasedChunkGenerator::overworld(seed, &data).unwrap();
    let mut baseline = HashMap::new();
    for x in 0..=1 {
        for z in 0..=1 {
            let beard = Beardifier::for_starts(x, z, &data, [&source]).unwrap();
            baseline.insert(
                ChunkPos { x, z },
                terrain
                    .generate_chunk_carved_with_beardifier(x, z, &data, beard)
                    .unwrap(),
            );
        }
    }
    // Persist actual source terrain, so the target's chest facing must read saved blocks, not regenerate them.
    baseline
        .get_mut(&ChunkPos { x: 0, z: 0 })
        .unwrap()
        .set_block_state(15, 61, 15, 0);
    let mut saved_source =
        harpas_engine::worldgen::into_anvil(baseline[&ChunkPos { x: 0, z: 0 }].clone()).unwrap();
    let mut records = Compound::new();
    records.insert("starts", starts(&[source.clone()]));
    records.insert("References", Compound::new());
    saved_source.rest.insert("structures", records);
    let source_nbt = saved_source.to_nbt(&biomes);
    let mut empty = Compound::new();
    empty.insert("Status", "minecraft:carvers");
    let mut records = Compound::new();
    records.insert("starts", Compound::new());
    records.insert("References", Compound::new());
    empty.insert("structures", records);
    let reader = |p: ChunkPos| {
        Ok(Some(if p == (ChunkPos { x: 0, z: 0 }) {
            source_nbt.clone()
        } else {
            empty.clone()
        }))
    };
    let mut generated = baseline[&pos].clone();
    let entities = source.place_in_chunk(
        &mut generated,
        seed,
        &mut |_, _| panic!("saved temple height"),
        &mut |x, y, z| {
            baseline[&ChunkPos {
                x: x >> 4,
                z: z >> 4,
            }]
                .block_state((x & 15) as usize, y, (z & 15) as usize)
        },
    );
    let mut expected = harpas_engine::worldgen::into_anvil(generated).unwrap();
    expected
        .rest
        .insert("block_entities", entity_list(entities));
    let mut refs = Compound::new();
    refs.insert("minecraft:jungle_pyramid", Nbt::LongArray(vec![0]));
    let mut records = Compound::new();
    records.insert("starts", Compound::new());
    records.insert("References", refs);
    expected.rest.insert("structures", records);
    let generator = overworld_generator(
        seed,
        Arc::clone(&data),
        Arc::clone(&templates),
        true,
        &biomes,
    )
    .unwrap();
    let actual = generator(pos, &reader).unwrap();
    assert_eq!(actual.to_nbt(&biomes), expected.to_nbt(&biomes));
    let Some(Nbt::Compound(records)) = source_nbt.get("structures") else {
        panic!("source records")
    };
    let Some(Nbt::Compound(saved)) = records.get("starts") else {
        panic!("source starts")
    };
    assert_eq!(
        saved.get("minecraft:jungle_pyramid"),
        Some(&Nbt::Compound(original))
    );
    let folder = reference.temp.0.join("saved-neighbor-world");
    let generator = Arc::new(move |target, _: &harpas_engine::world::ChunkReader<'_>| {
        generator(target, &|p| {
            Ok(Some(if p == (ChunkPos { x: 0, z: 0 }) {
                source_nbt.clone()
            } else {
                empty.clone()
            }))
        })
    });
    let mut world = World::open_with_generator(&folder, &biomes, generator).unwrap();
    wait(&mut world, &[pos]);
    let Some(ChunkState::Loaded(chunk)) = world.chunk(pos) else {
        panic!("neighbor worker")
    };
    assert_eq!(chunk.to_nbt(&biomes), expected.to_nbt(&biomes));
    world.flush().unwrap();
    drop(world);
    let mut world = World::open(&folder, &biomes).unwrap();
    wait(&mut world, &[pos]);
    let Some(ChunkState::Loaded(chunk)) = world.chunk(pos) else {
        panic!("neighbor reload")
    };
    assert_eq!(chunk.to_nbt(&biomes), expected.to_nbt(&biomes));
}
