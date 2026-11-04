//! Igloo template blocks, saved records and block entities against vanilla.

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

fn igloo(
    templates: &StructureTemplates,
    x: i32,
    z: i32,
    rotation: Rotation,
    depth: i32,
) -> StructureStart {
    let mut pieces = Vec::new();
    let mut add = |name: &str, position: [i32; 3], pivot| {
        let template = TemplatePiece {
            template: format!("minecraft:igloo/{name}"),
            position,
            rotation,
            mirror: Mirror::None,
            pivot,
        };
        let bounding_box = templates
            .get(&template.template)
            .unwrap()
            .bounding_box_around(position, rotation, Mirror::None, pivot);
        pieces.push(StructurePiece {
            kind: "minecraft:iglu".into(),
            bounding_box,
            orientation: Some(harpas_worldgen::Direction::North),
            gen_depth: 0,
            junctions: Vec::new(),
            jigsaw: None,
            template: Some(template),
            save_data: Compound::new(),
        });
    };
    if depth > 0 {
        add("bottom", [x, 87 - depth * 3, z - 2], [3, 6, 7]);
        for i in 0..depth - 1 {
            add("middle", [x + 2, 87 - i * 3, z + 4], [1, 3, 1]);
        }
    }
    add("top", [x, 90, z], [3, 5, 5]);
    let mut bounds = pieces[0].bounding_box;
    for p in &pieces[1..] {
        for axis in 0..3 {
            bounds.min[axis] = bounds.min[axis].min(p.bounding_box.min[axis]);
            bounds.max[axis] = bounds.max[axis].max(p.bounding_box.max[axis]);
        }
    }
    StructureStart {
        structure: "minecraft:igloo".into(),
        chunk: (x >> 4, z >> 4),
        references: 0,
        bounding_box: bounds,
        pieces,
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
fn igloo_blocks_and_metadata_match_vanilla() {
    let Some(reference) = Reference::from_env() else {
        eprintln!("Skipping igloo placement: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    let data = Arc::new(WorldgenData::from_zip_path(&reference.server_jar).unwrap());
    let templates = Arc::new(StructureTemplates::from_zip_path(&reference.server_jar).unwrap());
    let mut inputs = Compound::new();
    let mut expected = Compound::new();
    for &seed in reference::sampled_seeds(&[0, -4_172_144_997_902_289_642]) {
        for rotation in Rotation::ALL {
            for scenario in 0..8 {
                let (ox, oz) = if scenario % 2 == 0 { (0, 0) } else { (-18, 30) };
                let depth = match scenario {
                    0 | 1 => 0,
                    2 | 3 | 6 => 4,
                    _ => 11,
                };
                let height = |x: i32, z: i32| match scenario {
                    1 | 3 => 65 + (x + 2 * z).rem_euclid(7),
                    6 => -40,
                    7 => 318,
                    _ => 65,
                };
                let terrain = |x: i32, y: i32, z: i32| {
                    if y < height(x, z) {
                        block_states().default_state("minecraft:stone").unwrap()
                    } else if scenario == 4 && y < 70 {
                        block_states().default_state("minecraft:water").unwrap()
                    } else {
                        0
                    }
                };
                let surface = |x: i32, z: i32| if scenario == 4 { 70 } else { height(x, z) };
                let source = igloo(&templates, ox, oz, rotation, depth);
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
                        let name = format!("synthetic-{seed}-{rotation:?}-{scenario}-{cx}-{cz}");
                        let inp = input_with_terrain(&chunk, seed, &own, &terrain, &surface);
                        let entities = own[0].place_igloo_in_chunk(
                            &mut chunk,
                            seed,
                            &templates,
                            &mut |x, z| surface(x, z),
                            &mut |x, y, z| terrain(x, y, z),
                        );
                        inputs.insert(name.clone(), inp);
                        expected.insert(name.clone(), snapshot(&chunk, &own, entities.clone()));
                        own[0] = StructureStart::from_nbt(&own[0].to_nbt(), &data, &templates)
                            .unwrap()
                            .unwrap();
                        let mut inp = input_with_terrain(&chunk, seed, &own, &terrain, &surface);
                        inp.insert("initial_entities", entity_list(entities.clone()));
                        inputs.insert(format!("resume-{name}"), inp);
                        let resumed = own[0].place_igloo_in_chunk(
                            &mut chunk,
                            seed,
                            &templates,
                            &mut |x, z| surface(x, z),
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
            "IglooProbe.java",
            include_str!("../../harpas-worldgen/tests/IglooProbe.java"),
        ),
    ]);
    let input_path = reference.temp.0.join("input.nbt");
    let output_path = reference.temp.0.join("output.nbt");
    let mut bytes = Vec::new();
    Nbt::Compound(inputs).write_named("", &mut bytes);
    std::fs::write(&input_path, bytes).unwrap();
    reference.java(
        "IglooProbe",
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
    terrain: &impl Fn(i32, i32, i32) -> u32,
    surface: &impl Fn(i32, i32) -> i32,
) -> Compound {
    let mut tag = input(chunk, seed, own);
    let mut columns = Compound::new();
    for start in own {
        for piece in &start.pieces {
            if let Some(template) = &piece.template
                && template.template == "minecraft:igloo/top"
            {
                let p = template
                    .rotation
                    .transform_around([3, 0, 5], template.pivot);
                let x = p[0] + template.position[0];
                let z = p[2] + template.position[2];
                columns.insert(
                    format!("{x},{z}"),
                    Nbt::IntArray((-64..320).map(|y| terrain(x, y, z) as i32).collect()),
                );
            }
        }
    }
    tag.insert("extra_columns", columns);
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
    let mut heights = Vec::new();
    for z in -16..32 {
        for x in -16..32 {
            heights.push(surface(chunk.chunk_x() * 16 + x, chunk.chunk_z() * 16 + z));
        }
    }
    tag.insert("heights", Nbt::IntArray(heights));
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
                        || !state.is_structure_chunk("minecraft:igloos", x, z)
                    {
                        continue;
                    }
                    let own = state
                        .starts_in_chunk(&terrain, data, templates, x, z)
                        .unwrap();
                    if let Some(igloo) = own.into_iter().find(|s| s.structure == "minecraft:igloo")
                    {
                        let variant = usize::from(igloo.pieces.len() > 1);
                        if targets[variant].is_none() {
                            targets[variant] = Some(igloo);
                        }
                        if targets.iter().all(Option::is_some) {
                            break 'search;
                        }
                    }
                }
            }
        }
        for source in targets {
            let source = source.expect("natural igloo variant");
            eprintln!(
                "Seed {seed}: natural igloo at {:?} ({} pieces)",
                source.chunk,
                source.pieces.len()
            );
            let bounds = source.bounding_box;
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
            let surface = |x: i32, z: i32| {
                carved
                    .get(&ChunkPos {
                        x: x >> 4,
                        z: z >> 4,
                    })
                    .map_or_else(
                        || terrain.base_height(x, z, HeightmapType::WorldSurfaceWg),
                        |c| {
                            c.heightmaps().get(
                                HeightmapType::WorldSurfaceWg,
                                (x & 15) as usize,
                                (z & 15) as usize,
                            )
                        },
                    )
            };
            let folder = reference
                .temp
                .0
                .join(format!("igloo-world-{seed}-{}", source.pieces.len()));
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
                let mut inp = input_with_terrain(
                    &carved[&pos],
                    seed,
                    std::slice::from_ref(&source),
                    &block,
                    &surface,
                );
                let mut all = starts_cache[&(pos.x, pos.z)].clone();
                if !all.iter().any(|s| s.structure == source.structure) {
                    all.push(source.clone());
                }
                inp.insert("starts", starts(&all));
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
                    refs.get("minecraft:igloo"),
                    Some(&Nbt::LongArray(vec![packed]))
                );
                // Reload worker records and compare a fresh igloo pass with vanilla. This is a pass-level
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
                            panic!("igloo")
                        };
                        *start = StructureStart::from_nbt(tag, data, templates)
                            .unwrap()
                            .unwrap();
                    }
                }
                let mut resumed_input =
                    input_with_terrain(&generated, seed, &all, &block, &surface);
                resumed_input.insert("keep_neighbor_starts", 0_i8);
                resumed_input.insert("initial_entities", entity_list(entities.clone()));
                let mut resumed = generated.clone();
                let mut resumed_entities = Vec::new();
                for start in &mut all {
                    resumed_entities.extend(start.place_igloo_in_chunk(
                        &mut resumed,
                        seed,
                        templates,
                        &mut |x, z| surface(x, z),
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
    let source = igloo(templates, 6, 14, Rotation::None, 4);
    let terrain = NoiseBasedChunkGenerator::overworld(seed, data).unwrap();
    let mut carved = HashMap::new();
    for pos in [origin, target] {
        let beard = Beardifier::for_starts(pos.x, pos.z, data, [&source]).unwrap();
        carved.insert(
            pos,
            terrain
                .generate_chunk_carved_with_beardifier(pos.x, pos.z, data, beard)
                .unwrap(),
        );
    }
    // Raise the saved entrance column above the seed-derived surface. Target placement must read it.
    let stone = block_states().default_state("minecraft:stone").unwrap();
    carved
        .get_mut(&origin)
        .unwrap()
        .set_block_state(9, 200, 14, stone);
    let mut saved_source = harpas_engine::worldgen::into_anvil(carved[&origin].clone()).unwrap();
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
        overworld_generator(seed, Arc::clone(data), Arc::clone(templates), true, &biomes).unwrap();
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
    let surface = |x: i32, z: i32| {
        carved
            .get(&ChunkPos {
                x: x >> 4,
                z: z >> 4,
            })
            .map_or_else(
                || terrain.base_height(x, z, HeightmapType::WorldSurfaceWg),
                |c| {
                    c.heightmaps().get(
                        HeightmapType::WorldSurfaceWg,
                        (x & 15) as usize,
                        (z & 15) as usize,
                    )
                },
            )
    };
    let mut inp = input_with_terrain(
        &carved[&target],
        seed,
        std::slice::from_ref(&source),
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
        "saved-neighbor igloo placed no block entities"
    );
    let mut snapshot = snapshot(&generated, &[], entities);
    snapshot.insert("Heightmaps", nbt.get("Heightmaps").unwrap().clone());
    snapshot.insert("PostProcessing", nbt.get("PostProcessing").unwrap().clone());
    inputs.insert("worker-saved-neighbor", inp);
    expected.insert("worker-saved-neighbor", snapshot);
    let Some(Nbt::Compound(records)) = nbt.get("structures") else {
        panic!("records")
    };
    assert_eq!(records.get("starts"), Some(&Nbt::Compound(Compound::new())));
    let Some(Nbt::Compound(refs)) = records.get("References") else {
        panic!("refs")
    };
    assert_eq!(refs.get("minecraft:igloo"), Some(&Nbt::LongArray(vec![0])));
    let folder = reference.temp.0.join("saved-igloo-neighbor");
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
