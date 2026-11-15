//! Desert pyramid block placement and metadata against vanilla.

#[allow(dead_code)]
#[path = "../../harpas-worldgen/tests/reference/mod.rs"]
mod reference;

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_engine::world::{ChunkPos, ChunkState, World};
use harpas_engine::worldgen::{decorated_with_structures, overworld_generator, overworld_starts};
use harpas_worldgen::{
    Beardifier, BoundingBox, GeneratedChunk, HeightmapType, NoiseBasedChunkGenerator,
    StructurePiece, StructureStart, StructureState, StructureTemplates, WorldgenData,
};
use reference::Reference;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn pyramid(x: i32, z: i32, direction: harpas_worldgen::Direction) -> StructureStart {
    let bounds = BoundingBox::oriented([x, 64, z], direction, 21, 15, 21);
    let mut save_data = Compound::new();
    for (key, value) in [("Width", 21), ("Height", 15), ("Depth", 21), ("HPos", -1)] {
        save_data.insert(key, value);
    }
    for i in 0..4 {
        save_data.insert(format!("hasPlacedChest{i}"), 0_i8);
    }
    StructureStart {
        structure: "minecraft:desert_pyramid".into(),
        chunk: (x >> 4, z >> 4),
        references: 0,
        bounding_box: bounds,
        pieces: vec![StructurePiece {
            kind: "minecraft:tedp".into(),
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
    let b = own[0].pieces[0].bounding_box;
    tag.insert("originX", b.min[0]);
    tag.insert("originZ", b.min[2]);
    tag.insert("heights", Nbt::IntArray(vec![56; 441]));
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
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn pyramid_blocks_and_metadata_match_vanilla() {
    let reference = Reference::required();
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
            for scenario in 0..5 {
                let (ox, oz) = if scenario % 2 == 0 { (0, 0) } else { (-26, 54) };
                let height = |x: i32, z: i32| {
                    if scenario == 1 {
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
                for cx in (ox >> 4)..=((ox + 20) >> 4) {
                    for cz in (oz >> 4)..=((oz + 20) >> 4) {
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
                        let mut own = vec![pyramid(ox, oz, direction)];
                        if scenario != 1 {
                            own[0].pieces[0].save_data.insert("HPos", 64);
                        }
                        if scenario == 3 {
                            own[0].pieces[0].save_data.insert("hasPlacedChest0", 1_i8);
                        }
                        if scenario == 4 {
                            own[0].pieces[0].bounding_box.min[1] = -60;
                            own[0].pieces[0].bounding_box.max[1] = -46;
                            own[0].pieces[0].save_data.insert("HPos", 0);
                        }
                        let name = format!("synthetic-{seed}-{direction:?}-{scenario}-{cx}-{cz}");
                        let mut inp = input_with_terrain(&chunk, seed, &own, &height, &terrain);
                        let entities = own[0].place_in_chunk(
                            &mut chunk,
                            seed,
                            &mut |x, z| height(x, z),
                            &mut |x, y, z| terrain(x, y, z),
                        );
                        inputs.insert(name.clone(), inp);
                        expected.insert(name.clone(), snapshot(&chunk, &own, entities.clone()));
                        // Vanilla loads Harpas's saved records and block entities before a second FEATURES attempt.
                        own[0] = StructureStart::from_nbt(&own[0].to_nbt(), &data, &templates)
                            .unwrap()
                            .unwrap();
                        inp = input_with_terrain(&chunk, seed, &own, &height, &terrain);
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
                                "minecraft:suspicious_sand"
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
            "DesertPyramidProbe.java",
            include_str!("../../harpas-worldgen/tests/DesertPyramidProbe.java"),
        ),
    ]);
    let input_path = reference.temp.0.join("input.nbt");
    let output_path = reference.temp.0.join("output.nbt");
    let mut bytes = Vec::new();
    Nbt::Compound(inputs).write_named("", &mut bytes);
    std::fs::write(&input_path, bytes).unwrap();
    reference.java(
        "DesertPyramidProbe",
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
    height: &impl Fn(i32, i32) -> i32,
    terrain: &impl Fn(i32, i32, i32) -> u32,
) -> Compound {
    let mut tag = input(chunk, seed, own);
    let b = own[0].pieces[0].bounding_box;
    let mut heights = Vec::new();
    for z in b.min[2]..=b.max[2] {
        for x in b.min[0]..=b.max[0] {
            heights.push(height(x, z));
        }
    }
    tag.insert("heights", Nbt::IntArray(heights));
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
                        || !state.is_structure_chunk("minecraft:desert_pyramids", x, z)
                    {
                        continue;
                    }
                    let own = state
                        .starts_in_chunk(&terrain, data, templates, x, z)
                        .unwrap();
                    if let Some(pyramid) = own
                        .into_iter()
                        .find(|s| s.structure == "minecraft:desert_pyramid")
                    {
                        target = Some(pyramid);
                        break 'search;
                    }
                }
            }
        }
        let source = target.expect("natural desert pyramid");
        eprintln!("Seed {seed}: natural pyramid at {:?}", source.chunk);
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
        let height = |x: i32, z: i32| {
            carved[&ChunkPos {
                x: x >> 4,
                z: z >> 4,
            }]
                .heightmaps()
                .get(
                    HeightmapType::MotionBlockingNoLeaves,
                    (x & 15) as usize,
                    (z & 15) as usize,
                )
        };
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
        let folder = reference.temp.0.join(format!("pyramid-world-{seed}"));
        // Pyramids are placed in decoration. Decorate without features, so that the workers' chunks are the
        // carved terrain with their structures, as vanilla's probe places them. The pyramid's shared height
        // comes from the carved terrain, as the probe's source chunk places it first.
        let mut world = World::open_with_source(
            &folder,
            &biomes,
            structures_only_source(seed, data, templates, None),
        )
        .unwrap();
        wait(&mut world, &positions);
        let mut saved = Vec::new();
        for &pos in &positions {
            let Some(ChunkState::Loaded(chunk)) = world.chunk(pos) else {
                panic!("worker chunk")
            };
            let nbt = chunk.to_nbt(&biomes);
            let mut inp = input_with_terrain(
                &carved[&pos],
                seed,
                std::slice::from_ref(&source),
                &height,
                &block,
            );
            let mut all = starts_cache[&(pos.x, pos.z)].clone();
            if !all.iter().any(|s| s.structure == source.structure) {
                all.push(source.clone());
            }
            inp.insert("starts", starts(&all));
            inp.insert("source_first", 1_i8);
            inp.insert("keep_neighbor_starts", 0_i8);
            let name = format!("worker-{seed}-{},{}", pos.x, pos.z);
            inputs.insert(name.clone(), inp);
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
            expected.insert(name, actual);
            let Some(Nbt::Compound(refs)) = records.get("References") else {
                panic!("references")
            };
            let packed =
                i64::from(source.chunk.0 as u32) | (i64::from(source.chunk.1 as u32) << 32);
            assert_eq!(
                refs.get("minecraft:desert_pyramid"),
                Some(&Nbt::LongArray(vec![packed]))
            );
            // The chunk is saved decorated, so vanilla never repeats the pass on it: no replay to compare.
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
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn saved_neighbor_pyramids_keep_their_height_and_source_record() {
    let reference = Reference::required();
    let mut data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    // This probe compares its own structure pass; mineshaft_parity covers mine blocks.
    data.structure_sets.shift_remove("minecraft:mineshafts");
    let data = Arc::new(data);
    let templates = Arc::new(StructureTemplates::from_zip_path(&reference.server_jar).unwrap());
    let biomes: Vec<_> = data.biomes.keys().map(String::as_str).collect();
    let seed = 42;
    let pos = ChunkPos { x: 0, z: 1 };
    let mut source = pyramid(6, 6, harpas_worldgen::Direction::South);
    source.pieces[0].save_data.insert("HPos", 64);
    source.pieces[0].save_data.insert("hasPlacedChest0", 1_i8);
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
        .set_block_state(14, 53, 15, 0);
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
    // The generator saves the target at carvers with the pyramid's reference but none of its blocks.
    let mut carved = harpas_engine::worldgen::into_anvil(generated.clone()).unwrap();
    let mut refs = Compound::new();
    refs.insert("minecraft:desert_pyramid", Nbt::LongArray(vec![0]));
    let mut records = Compound::new();
    records.insert("starts", Compound::new());
    records.insert("References", refs);
    carved.rest.insert("structures", records);
    let data = structures_only(&data);
    let generator = overworld_generator(
        seed,
        Arc::clone(&data),
        Arc::clone(&templates),
        true,
        &biomes,
    )
    .unwrap();
    let actual = generator(pos, &reader).unwrap();
    assert_eq!(actual.to_nbt(&biomes), carved.to_nbt(&biomes));
    // Decoration places the saved pyramid's intersection at its saved height; chests read the saved source.
    let entities = source.place_in_chunk(
        &mut generated,
        seed,
        &mut |_, _| panic!("saved pyramid height"),
        &mut |x, y, z| {
            baseline[&ChunkPos {
                x: x >> 4,
                z: z >> 4,
            }]
                .block_state((x & 15) as usize, y, (z & 15) as usize)
        },
    );
    let want = blocks(&generated);
    let mut want_entities = entities;
    want_entities.sort_by_key(packed_position);
    let Some(Nbt::Compound(records)) = source_nbt.get("structures") else {
        panic!("source records")
    };
    let Some(Nbt::Compound(saved)) = records.get("starts") else {
        panic!("source starts")
    };
    assert_eq!(
        saved.get("minecraft:desert_pyramid"),
        Some(&Nbt::Compound(original))
    );
    let folder = reference.temp.0.join("saved-neighbor-world");
    let region = harpas_anvil::dimension_dir(&folder, "minecraft:overworld").join("region");
    std::fs::create_dir_all(&region).unwrap();
    // Saved as decorated, so the source is not decorated again here: the target's decoration reads its saved
    // blocks whichever chunk the workers decorate first.
    let mut decorated_source = source_nbt.clone();
    decorated_source.insert("Status", "minecraft:features");
    harpas_anvil::write_chunks(&region, 0, 0, &[((0, 0), &decorated_source)]).unwrap();
    let reader = Arc::new(move |p: ChunkPos| -> std::io::Result<Option<Compound>> {
        Ok(Some(if p == (ChunkPos { x: 0, z: 0 }) {
            source_nbt.clone()
        } else {
            empty.clone()
        }))
    });
    let source = structures_only_source(seed, &data, &templates, Some(reader));
    let mut world = World::open_with_source(&folder, &biomes, source).unwrap();
    wait(&mut world, &[pos]);
    let Some(ChunkState::Loaded(chunk)) = world.chunk(pos) else {
        panic!("neighbor worker")
    };
    let nbt = chunk.to_nbt(&biomes);
    assert_same_blocks(&blocks(&loaded_blocks(chunk, pos, biomes.len())), &want);
    let mut entities = match nbt.get("block_entities").unwrap() {
        Nbt::List(List::Compound(v)) => v.clone(),
        Nbt::List(List::Empty) => Vec::new(),
        _ => panic!("entities"),
    };
    entities.sort_by_key(packed_position);
    assert_eq!(entities, want_entities);
    assert_eq!(nbt.get("structures"), carved.rest.get("structures"));
    world.flush().unwrap();
    drop(world);
    let mut world = World::open(&folder, &biomes).unwrap();
    wait(&mut world, &[pos]);
    let Some(ChunkState::Loaded(chunk)) = world.chunk(pos) else {
        panic!("neighbor reload")
    };
    assert_eq!(chunk.to_nbt(&biomes), nbt);
}

/// Worldgen data whose biomes list no features, so that decoration places only structures.
fn structures_only(data: &WorldgenData) -> Arc<WorldgenData> {
    let mut data = data.clone();
    for biome in data.biomes.values_mut() {
        biome.features = vec![Vec::new(); biome.features.len()];
    }
    Arc::new(data)
}

type SavedReader = Arc<dyn Fn(ChunkPos) -> std::io::Result<Option<Compound>> + Send + Sync>;

/// Workers that generate chunks and decorate them with structures only. With `saved`, generation and the
/// decoration's starts read saved chunks from it instead of the world.
fn structures_only_source(
    seed: i64,
    data: &WorldgenData,
    templates: &Arc<StructureTemplates>,
    saved: Option<SavedReader>,
) -> Arc<dyn harpas_engine::world::ChunkSource> {
    let data = structures_only(data);
    let biomes: Vec<_> = data.biomes.keys().map(String::as_str).collect();
    let generator = overworld_generator(
        seed,
        Arc::clone(&data),
        Arc::clone(templates),
        true,
        &biomes,
    )
    .unwrap();
    let starts = overworld_starts(seed, Arc::clone(&data), Arc::clone(templates)).unwrap();
    let (generator, starts): (Arc<harpas_engine::world::ChunkGenerator>, _) = match saved {
        Some(saved) => {
            let read = Arc::clone(&saved);
            (
                Arc::new(move |pos, _: &harpas_engine::world::ChunkReader<'_>| {
                    generator(pos, &|p| read(p))
                }),
                Arc::new(move |pos, _: &harpas_engine::world::ChunkReader<'_>| {
                    starts(pos, &|p| saved(p))
                }) as Arc<harpas_engine::worldgen::StartSource>,
            )
        }
        None => (generator, starts),
    };
    decorated_with_structures(generator, starts, seed, data, Arc::clone(templates)).unwrap()
}

/// The blocks of a loaded chunk.
fn loaded_blocks(chunk: &harpas_anvil::Chunk, pos: ChunkPos, biome_count: usize) -> GeneratedChunk {
    let mut generated = GeneratedChunk::new(pos.x, pos.z, -64, 384, biome_count);
    for section in &chunk.sections {
        if let Some(blocks) = &section.section {
            let i = (i32::from(section.y) - generated.min_section_y()) as usize;
            generated.sections_mut()[i] = blocks.clone();
        }
    }
    generated
}

/// Asserts that two block arrays from [`blocks`] match, naming the first positions that differ.
fn assert_same_blocks(actual: &Nbt, expected: &Nbt) {
    let (Nbt::IntArray(actual), Nbt::IntArray(expected)) = (actual, expected) else {
        panic!("block arrays")
    };
    let diffs: Vec<_> = actual
        .iter()
        .zip(expected)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .take(20)
        .map(|(i, (&a, &b))| {
            format!(
                "({},{},{}) actual={:?} expected={:?}",
                i % 16,
                (i / 256) as i32 - 64,
                (i / 16) % 16,
                block_states().properties(a as u32),
                block_states().properties(b as u32)
            )
        })
        .collect();
    assert!(diffs.is_empty(), "{}", diffs.join("\n"));
}
