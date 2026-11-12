//! Stronghold placement, saved records and metadata compared with vanilla's public placement APIs.
#[allow(dead_code)]
#[path = "../../harpas-worldgen/tests/reference/mod.rs"]
mod reference;
use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_engine::world::{ChunkPos, ChunkState, World};
use harpas_engine::worldgen::overworld_generator;
use harpas_worldgen::{Beardifier, NoiseBasedChunkGenerator, StructureState};
use harpas_worldgen::{
    BoundingBox, Direction, GeneratedChunk, HeightmapType, StructurePiece, StructureStart,
    StructureTemplates, WorldgenData,
};
use reference::Reference;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn stronghold(
    kind: &str,
    variant: i32,
    direction: Direction,
    origin: [i32; 3],
    door: &str,
) -> StructureStart {
    let size = match kind {
        "shph" => [9, 5, 11],
        "shrc" => [11, 7, 11],
        "shssd" => [5, 11, 8],
        "shsd" | "shstart" => [5, 11, 5],
        "sh5c" => [10, 9, 11],
        "shli" => [14, if variant == 1 { 6 } else { 11 }, 15],
        "shpr" => [11, 8, 16],
        "shfc" => [5, 5, 3],
        "shlt" | "shrt" => [5, 5, 5],
        _ => [5, 5, 7],
    };
    let bounds = BoundingBox::oriented(origin, direction, size[0], size[1], size[2]);
    let mut save_data = Compound::new();
    save_data.insert("EntryDoor", door);
    match kind {
        "shs" => {
            save_data.insert("Left", i8::from(variant == 1));
            save_data.insert("Right", i8::from(variant == 1));
        }
        "sh5c" => {
            for key in ["leftLow", "leftHigh", "rightLow", "rightHigh"] {
                save_data.insert(key, i8::from(variant == 1));
            }
        }
        "shstart" | "shsd" => {
            save_data.insert("Source", i8::from(kind == "shstart"));
        }
        "shcc" => {
            save_data.insert("Chest", 0_i8);
        }
        "shpr" => {
            save_data.insert("Mob", 0_i8);
        }
        "shli" => {
            save_data.insert("Tall", i8::from(variant != 1));
        }
        "shrc" => {
            save_data.insert("Type", variant);
        }
        "shfc" => {
            save_data.insert("Steps", 3);
        }
        _ => {}
    }
    StructureStart {
        structure: "minecraft:stronghold".into(),
        chunk: (origin[0] >> 4, origin[2] >> 4),
        references: 0,
        bounding_box: bounds,
        pieces: vec![StructurePiece {
            kind: format!("minecraft:{kind}"),
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
#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn stronghold_blocks_and_metadata_match_vanilla() {
    let reference = Reference::required();
    let data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let templates = StructureTemplates::from_zip_path(&reference.server_jar).unwrap();
    compile_probe(&reference);
    for &seed in reference::sampled_seeds(&[0, -4_172_144_997_902_289_642]) {
        for direction in [
            Direction::North,
            Direction::East,
            Direction::South,
            Direction::West,
        ] {
            let mut inputs = Compound::new();
            let mut expected = Compound::new();
            let cases = [
                ("shs", 0),
                ("shs", 1),
                ("shph", 0),
                ("shlt", 0),
                ("shrt", 0),
                ("shrc", 0),
                ("shrc", 1),
                ("shrc", 2),
                ("shrc", 3),
                ("shrc", 4),
                ("shssd", 0),
                ("shsd", 0),
                ("shstart", 0),
                ("sh5c", 0),
                ("sh5c", 1),
                ("shcc", 0),
                ("shli", 0),
                ("shli", 1),
                ("shpr", 0),
                ("shfc", 0),
            ];
            for &(kind, variant) in &cases {
                for scenario in 0..8 {
                    let terrain = |x: i32, y: i32, z: i32| {
                        block_states()
                            .default_state(match scenario {
                                4 => "minecraft:air",
                                5 => "minecraft:water",
                                6 => "minecraft:lava",
                                7 if (x + 2 * y + 3 * z).rem_euclid(5) < 2 => "minecraft:cave_air",
                                _ => "minecraft:stone",
                            })
                            .unwrap()
                    };
                    let origin = if scenario < 4 {
                        [-5, 50, 13]
                    } else {
                        [
                            0,
                            if scenario == 5 {
                                -68
                            } else if scenario == 6 {
                                316
                            } else {
                                50
                            },
                            0,
                        ]
                    };
                    let source = stronghold(
                        kind,
                        variant,
                        direction,
                        origin,
                        ["OPENING", "WOOD_DOOR", "IRON_DOOR", "GRATES"][scenario.min(3)],
                    );
                    let b = source.bounding_box;
                    for cx in b.min[0] >> 4..=b.max[0] >> 4 {
                        for cz in b.min[2] >> 4..=b.max[2] >> 4 {
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
                            let mut own = vec![source.clone()];
                            let name =
                                format!("{kind}-{variant}-{direction:?}-{scenario}-{cx}-{cz}");
                            inputs.insert(
                                name.clone(),
                                input_with_terrain(&chunk, seed, &own, &terrain),
                            );
                            let entities = own[0].place_in_chunk(
                                &mut chunk,
                                seed,
                                &mut |_, _| 320,
                                &mut |x, y, z| terrain(x, y, z),
                            );
                            expected.insert(name.clone(), snapshot(&chunk, &own, entities.clone()));
                            own[0] = StructureStart::from_nbt(&own[0].to_nbt(), &data, &templates)
                                .unwrap()
                                .unwrap();
                            let mut inp = input_with_terrain(&chunk, seed, &own, &terrain);
                            inp.insert("initial_entities", entity_list(entities.clone()));
                            inputs.insert(format!("resume-{name}"), inp);
                            let mut resumed = own[0].place_in_chunk(
                                &mut chunk,
                                seed,
                                &mut |_, _| 320,
                                &mut |x, y, z| terrain(x, y, z),
                            );
                            for entity in entities {
                                let (x, y, z) =
                                    (int(&entity, "x"), int(&entity, "y"), int(&entity, "z"));
                                let current =
                                    chunk.block_state((x & 15) as usize, y, (z & 15) as usize);
                                let kind =
                                    if entity.get("id") == Some(&Nbt::from("minecraft:chest")) {
                                        "minecraft:chest"
                                    } else {
                                        "minecraft:spawner"
                                    };
                                if block_states().properties(current).unwrap().0 == kind
                                    && !resumed
                                        .iter()
                                        .any(|e| packed_position(e) == packed_position(&entity))
                                {
                                    resumed.push(entity);
                                }
                            }
                            expected
                                .insert(format!("resume-{name}"), snapshot(&chunk, &own, resumed));
                        }
                    }
                }
            }
            eprintln!(
                "Comparing {} stronghold placements/replays, seed {seed}, direction {direction:?}",
                inputs.len()
            );
            compare(&reference, inputs, expected);
        }
    }
}

fn compare(reference: &Reference, inputs: Compound, expected: Compound) {
    let input_path = reference.temp.0.join("input.nbt");
    let output_path = reference.temp.0.join("output.nbt");
    let mut bytes = Vec::new();
    Nbt::Compound(inputs).write_named("", &mut bytes);
    std::fs::write(&input_path, bytes).unwrap();
    reference.java(
        "StrongholdProbe",
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
                if k == "Heightmaps" {
                    let Nbt::Compound(want) = v else {
                        panic!("maps");
                    };
                    let Some(Nbt::Compound(got)) = actual.get(k) else {
                        panic!("maps");
                    };
                    for (kind, w) in want.iter() {
                        let Nbt::LongArray(w) = w else {
                            panic!("map");
                        };
                        let Some(Nbt::LongArray(g)) = got.get(kind) else {
                            panic!("map");
                        };
                        let diffs: Vec<_> = (0..256)
                            .filter_map(|i| {
                                let a = ((w[i / 7] as u64 >> ((i % 7) * 9)) & 511) as i32 - 64;
                                let b = ((g[i / 7] as u64 >> ((i % 7) * 9)) & 511) as i32 - 64;
                                (a != b).then(|| format!("({},{}): {a}/{b}", i % 16, i / 16))
                            })
                            .take(8)
                            .collect();
                        if !diffs.is_empty() {
                            failures.push(format!("{name} {kind} Rust/vanilla: {diffs:?}"));
                        }
                    }
                } else {
                    failures.push(format!(
                        "{name} {k}: Rust={v:?} vanilla={:?}",
                        actual.get(k)
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn compile_probe(reference: &Reference) {
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
            "StrongholdProbe.java",
            include_str!("../../harpas-worldgen/tests/StrongholdProbe.java"),
        ),
    ]);
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn natural_strongholds_on_workers_match_vanilla_and_survive_anvil_reload() {
    let reference = Reference::required();
    let mut data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    // Isolate this structure pass; other harnesses compare the remaining structure types.
    data.structure_sets.retain(|_, set| {
        set.structures
            .iter()
            .any(|s| s.structure == "minecraft:stronghold")
    });
    let data = Arc::new(data);
    let templates = Arc::new(StructureTemplates::from_zip_path(&reference.server_jar).unwrap());
    compile_probe(&reference);
    let biomes: Vec<_> = data.biomes.keys().map(String::as_str).collect();
    for &seed in reference::sampled_seeds(&[0, -4_172_144_997_902_289_642]) {
        let terrain = NoiseBasedChunkGenerator::overworld(seed, &data).unwrap();
        let state = StructureState::new(seed, &data, &terrain).unwrap();
        let &(x, z) = state
            .ring_positions("minecraft:strongholds")
            .unwrap()
            .first()
            .unwrap();
        let source = state
            .starts_in_chunk(&terrain, &data, &templates, x, z)
            .unwrap()
            .into_iter()
            .find(|s| s.structure == "minecraft:stronghold")
            .unwrap();
        let mut positions = vec![ChunkPos { x, z }];
        for kind in [
            "minecraft:shpr",
            "minecraft:shli",
            "minecraft:shcc",
            "minecraft:sh5c",
            "minecraft:shrc",
        ] {
            if let Some(p) = source.pieces.iter().find(|p| p.kind == kind) {
                let b = p.bounding_box;
                positions.push(ChunkPos {
                    x: ((b.min[0] + b.max[0]) / 2) >> 4,
                    z: ((b.min[2] + b.max[2]) / 2) >> 4,
                });
                positions.push(ChunkPos {
                    x: b.min[0] >> 4,
                    z: b.min[2] >> 4,
                });
                positions.push(ChunkPos {
                    x: b.max[0] >> 4,
                    z: b.max[2] >> 4,
                });
            }
        }
        positions.sort_by_key(|p| (p.x, p.z));
        positions.dedup();
        let mut carved = HashMap::new();
        let carve = |pos: ChunkPos| {
            terrain
                .generate_chunk_carved_with_beardifier(
                    pos.x,
                    pos.z,
                    &data,
                    Beardifier::for_starts(pos.x, pos.z, &data, [&source]).unwrap(),
                )
                .unwrap()
        };
        for &pos in &positions {
            let chunk = carve(pos);
            for [x, _, z] in source.stronghold_chest_neighbors(&chunk) {
                let neighbor = ChunkPos {
                    x: x >> 4,
                    z: z >> 4,
                };
                carved.entry(neighbor).or_insert_with(|| carve(neighbor));
            }
            carved.insert(pos, chunk);
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
        let mut first = HashMap::new();
        let mut inputs = Compound::new();
        let mut expected = Compound::new();
        for reverse in [false, true] {
            let folder = reference
                .temp
                .0
                .join(format!("stronghold-{seed}-{reverse}"));
            let generator = overworld_generator(
                seed,
                Arc::clone(&data),
                Arc::clone(&templates),
                true,
                &biomes,
            )
            .unwrap();
            let mut world = World::open_with_generator(&folder, &biomes, generator).unwrap();
            let mut order = positions.clone();
            if reverse {
                order.reverse();
            }
            // Request successive prefixes so each source is encountered in the opposite order.
            for count in 1..=order.len() {
                wait(&mut world, &order[..count]);
            }
            let mut saved = Vec::new();
            for &pos in &positions {
                let Some(ChunkState::Loaded(chunk)) = world.chunk(pos) else {
                    panic!("worker chunk");
                };
                let nbt = chunk.to_nbt(&biomes);
                if reverse {
                    assert_eq!(first[&pos], nbt, "reversed request {pos:?}");
                } else {
                    first.insert(pos, nbt.clone());
                    let mut generated = GeneratedChunk::new(pos.x, pos.z, -64, 384, biomes.len());
                    for section in &chunk.sections {
                        if let Some(blocks) = &section.section {
                            let i = (i32::from(section.y) - generated.min_section_y()) as usize;
                            generated.sections_mut()[i] = blocks.clone();
                        }
                    }
                    generated.recompute_heightmaps();
                    let Some(Nbt::Compound(records)) = nbt.get("structures") else {
                        panic!("structures");
                    };
                    let mut result = Compound::new();
                    result.insert("blocks", blocks(&generated));
                    result.insert("starts", records.get("starts").unwrap().clone());
                    result.insert("Heightmaps", nbt.get("Heightmaps").unwrap().clone());
                    result.insert("PostProcessing", nbt.get("PostProcessing").unwrap().clone());
                    let mut entities = match nbt.get("block_entities").unwrap() {
                        Nbt::List(List::Compound(v)) => v.clone(),
                        Nbt::List(List::Empty) => Vec::new(),
                        _ => panic!("entities"),
                    };
                    entities.sort_by_key(packed_position);
                    result.insert("block_entities", entity_list(entities));
                    let mut inp = input_with_terrain(
                        &carved[&pos],
                        seed,
                        std::slice::from_ref(&source),
                        &block,
                    );
                    inp.insert("keep_neighbor_starts", 0_i8);
                    inp.insert("defer_containers", 1_i8);
                    let name = format!("worker-{seed}-{},{}", pos.x, pos.z);
                    inputs.insert(name.clone(), inp);
                    expected.insert(name, result);
                    let Some(Nbt::Compound(refs)) = records.get("References") else {
                        panic!("references");
                    };
                    let packed =
                        i64::from(source.chunk.0 as u32) | (i64::from(source.chunk.1 as u32) << 32);
                    assert_eq!(
                        refs.get("minecraft:stronghold"),
                        Some(&Nbt::LongArray(vec![packed]))
                    );
                    if (pos.x, pos.z) == source.chunk {
                        assert_eq!(
                            records.get("starts"),
                            Some(&Nbt::Compound(starts(std::slice::from_ref(&source)))),
                            "deferred flags/source record"
                        );
                    }
                }
                saved.push((pos, nbt));
            }
            world.flush().unwrap();
            drop(world);
            let mut world = World::open(&folder, &biomes).unwrap();
            wait(&mut world, &positions);
            for (pos, nbt) in saved {
                let Some(ChunkState::Loaded(chunk)) = world.chunk(pos) else {
                    panic!("Anvil reload");
                };
                assert_eq!(chunk.to_nbt(&biomes), nbt, "Anvil reload {pos:?}");
            }
        }
        eprintln!(
            "Comparing {} natural stronghold worker chunks for seed {seed}",
            inputs.len()
        );
        compare(&reference, inputs, expected);
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
fn saved_stronghold_neighbors_supply_chest_facing_without_moving_the_source() {
    let reference = Reference::required();
    let data = Arc::new(WorldgenData::from_zip_path(&reference.server_jar).unwrap());
    let templates = Arc::new(StructureTemplates::from_zip_path(&reference.server_jar).unwrap());
    let biomes: Vec<_> = data.biomes.keys().map(String::as_str).collect();
    let seed = 42;
    let source = stronghold("shcc", 0, Direction::South, [13, 50, 3], "IRON_DOOR");
    let origin = ChunkPos { x: 0, z: 0 };
    let target = ChunkPos { x: 1, z: 0 };
    let terrain = NoiseBasedChunkGenerator::overworld(seed, &data).unwrap();
    let mut carved = HashMap::new();
    for pos in [origin, target] {
        let beard = Beardifier::for_starts(pos.x, pos.z, &data, [&source]).unwrap();
        carved.insert(
            pos,
            terrain
                .generate_chunk_carved_with_beardifier(pos.x, pos.z, &data, beard)
                .unwrap(),
        );
    }
    carved.get_mut(&origin).unwrap().set_block_state(
        15,
        52,
        6,
        block_states().default_state("minecraft:glass").unwrap(),
    );
    let mut saved = harpas_engine::worldgen::into_anvil(carved[&origin].clone()).unwrap();
    let mut records = Compound::new();
    records.insert("starts", starts(std::slice::from_ref(&source)));
    records.insert("References", Compound::new());
    saved.rest.insert("structures", records);
    let saved_nbt = saved.to_nbt(&biomes);
    let mut empty = Compound::new();
    empty.insert("Status", "minecraft:carvers");
    let mut records = Compound::new();
    records.insert("starts", Compound::new());
    records.insert("References", Compound::new());
    empty.insert("structures", records);
    let reader = |p: ChunkPos| {
        Ok(Some(if p == origin {
            saved_nbt.clone()
        } else {
            empty.clone()
        }))
    };
    let generator = overworld_generator(
        seed,
        Arc::clone(&data),
        Arc::clone(&templates),
        true,
        &biomes,
    )
    .unwrap();
    let actual = generator(target, &reader).unwrap();
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
    let mut placed = carved[&target].clone();
    let mut start = source.clone();
    let entities = start.place_in_chunk(
        &mut placed,
        seed,
        &mut |_, _| unreachable!(),
        &mut |x, y, z| block(x, y, z),
    );
    let mut want = harpas_engine::worldgen::into_anvil(placed.clone()).unwrap();
    want.rest
        .insert("block_entities", entity_list(entities.clone()));
    let mut records = Compound::new();
    records.insert("starts", Compound::new());
    let mut refs = Compound::new();
    refs.insert("minecraft:stronghold", Nbt::LongArray(vec![0]));
    records.insert("References", refs);
    want.rest.insert("structures", records);
    assert_eq!(actual.to_nbt(&biomes), want.to_nbt(&biomes));
    let mut inp = input_with_terrain(
        &carved[&target],
        seed,
        std::slice::from_ref(&source),
        &block,
    );
    inp.insert("keep_neighbor_starts", 0_i8);
    inp.insert("defer_containers", 1_i8);
    let mut result = snapshot(&placed, &[], entities);
    let Some(Nbt::Compound(mut maps)) = result.remove("Heightmaps") else {
        panic!("maps");
    };
    maps.remove("WORLD_SURFACE_WG");
    maps.remove("OCEAN_FLOOR_WG");
    result.insert("Heightmaps", maps);
    let mut inputs = Compound::new();
    inputs.insert("worker-saved-neighbor", inp);
    let mut expected = Compound::new();
    expected.insert("worker-saved-neighbor", result);
    compile_probe(&reference);
    compare(&reference, inputs, expected);
    let folder = reference.temp.0.join("saved-stronghold-neighbor");
    let generator = Arc::new(move |pos, _: &harpas_engine::world::ChunkReader<'_>| {
        generator(pos, &|p| {
            Ok(Some(if p == origin {
                saved_nbt.clone()
            } else {
                empty.clone()
            }))
        })
    });
    let mut world = World::open_with_generator(&folder, &biomes, generator).unwrap();
    wait(&mut world, &[target]);
    world.flush().unwrap();
    drop(world);
    let mut world = World::open(&folder, &biomes).unwrap();
    wait(&mut world, &[target]);
    let Some(ChunkState::Loaded(chunk)) = world.chunk(target) else {
        panic!("reload");
    };
    assert_eq!(chunk.to_nbt(&biomes), want.to_nbt(&biomes));
}

#[test]
fn stronghold_flags_recreate_containers_on_a_carvers_retry() {
    let mut start = stronghold("shpr", 0, Direction::South, [0, 50, 0], "OPENING");
    let mut chunk = GeneratedChunk::new(0, 0, -64, 384, 1);
    let entities = start.place_in_chunk(
        &mut chunk,
        42,
        &mut |_, _| unreachable!(),
        &mut |_, _, _| unreachable!(),
    );
    assert_eq!(entities.len(), 1);
    assert_eq!(start.pieces[0].save_data.get("Mob"), Some(&Nbt::Byte(1)));
    let Some(Nbt::Compound(spawn)) = entities[0].get("SpawnData") else {
        panic!("spawn");
    };
    let Some(Nbt::Compound(entity)) = spawn.get("entity") else {
        panic!("entity");
    };
    assert_eq!(entity.get("id"), Some(&Nbt::from("minecraft:silverfish")));
    // Retaining Mob=true after an incomplete feature stage would leave the excavated room without a spawner.
    assert!(
        start
            .place_in_chunk(
                &mut chunk,
                42,
                &mut |_, _| unreachable!(),
                &mut |_, _, _| unreachable!()
            )
            .is_empty()
    );
    assert_ne!(
        block_states()
            .properties(chunk.block_state(5, 53, 6))
            .unwrap()
            .0,
        "minecraft:spawner"
    );
    start.pieces[0].save_data.insert("Mob", 0_i8);
    assert_eq!(
        start
            .place_in_chunk(
                &mut chunk,
                42,
                &mut |_, _| unreachable!(),
                &mut |_, _, _| unreachable!()
            )
            .len(),
        1
    );
    assert_eq!(
        block_states()
            .properties(chunk.block_state(5, 53, 6))
            .unwrap()
            .0,
        "minecraft:spawner"
    );
}
