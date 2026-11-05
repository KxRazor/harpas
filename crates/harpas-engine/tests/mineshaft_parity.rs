//! Mineshaft piece placement against the owner's vanilla server, including reload and repeat placement.

#[allow(dead_code)]
#[path = "../../harpas-worldgen/tests/reference/mod.rs"]
mod reference;

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_worldgen::{
    BoundingBox, Direction, GeneratedChunk, HeightmapType, StructurePiece, StructureStart,
};
use reference::Reference;

fn start(kind: &str, b: BoundingBox, orientation: Option<Direction>, mesa: bool) -> StructureStart {
    let mut save_data = Compound::new();
    save_data.insert("MST", i32::from(mesa));
    if kind == "mscorridor" {
        save_data.insert("hr", 1_i8);
        save_data.insert("sc", 0_i8);
        save_data.insert("hps", 0_i8);
        save_data.insert("Num", 2);
    }
    if kind == "msroom" {
        save_data.insert(
            "Entrances",
            List::IntArray(vec![vec![
                b.min[0],
                b.min[1] + 1,
                b.min[2],
                b.min[0] + 2,
                b.min[1] + 3,
                b.min[2] + 1,
            ]]),
        );
    }
    if kind == "mscrossing" {
        save_data.insert("D", 0_i8);
        save_data.insert("tf", i8::from(b.max[1] - b.min[1] > 3));
    }
    StructureStart {
        structure: if mesa {
            "minecraft:mineshaft_mesa"
        } else {
            "minecraft:mineshaft"
        }
        .into(),
        chunk: (b.min[0] >> 4, b.min[2] >> 4),
        references: 0,
        bounding_box: b,
        pieces: vec![StructurePiece {
            kind: format!("minecraft:{kind}"),
            bounding_box: b,
            orientation,
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

fn starts(own: &[StructureStart]) -> Compound {
    own.iter()
        .map(|s| {
            (
                format!("{}@{},{}", s.structure, s.chunk.0, s.chunk.1),
                Nbt::Compound(s.to_nbt()),
            )
        })
        .collect()
}

fn snapshot(chunk: &GeneratedChunk, own: &[StructureStart]) -> Compound {
    let mut tag = Compound::new();
    tag.insert("blocks", blocks(chunk));
    tag.insert("starts", starts(own));
    tag.insert("block_entities", List::Empty);
    let mut maps = Compound::new();
    for kind in HeightmapType::ALL {
        maps.insert(
            kind.nbt_key(),
            Nbt::LongArray(chunk.heightmaps().pack(kind)),
        );
    }
    tag.insert("Heightmaps", maps);
    let mut post = vec![Vec::new(); 24];
    for &(x, y, z) in chunk.fluid_postprocessing() {
        post[((y + 64) / 16) as usize]
            .push((u16::from(x) | ((y as u16 & 15) << 4) | (u16::from(z) << 8)) as i16);
    }
    tag.insert(
        "PostProcessing",
        List::List(
            post.into_iter()
                .map(|v| {
                    if v.is_empty() {
                        List::Empty
                    } else {
                        List::Short(v)
                    }
                })
                .collect(),
        ),
    );
    tag
}

fn case_input(
    chunk: &GeneratedChunk,
    seed: i64,
    own: &[StructureStart],
    blocking: &mut impl FnMut(i32, i32, i32) -> bool,
) -> Compound {
    let mut input = snapshot(chunk, own);
    input.insert("x", chunk.chunk_x());
    input.insert("z", chunk.chunk_z());
    input.insert("seed", seed);
    let mut biomes = Compound::new();
    for sample @ [x, y, z] in own.iter().flat_map(|s| s.mineshaft_biome_samples(chunk)) {
        biomes.insert(
            format!("{},{},{}", sample[0], sample[1], sample[2]),
            i8::from(blocking(x, y, z)),
        );
    }
    input.insert("blockedBiomes", biomes);
    input
}

fn fixture(
    name: &str,
    mut chunk: GeneratedChunk,
    mut own: Vec<StructureStart>,
    blocked: bool,
    inputs: &mut Compound,
    expected: &mut Compound,
) {
    let input = case_input(&chunk, 42, &own, &mut |_, _, _| blocked);
    let mut entities = Vec::new();
    entities.extend(harpas_worldgen::place_mineshafts_in_chunk(
        &mut own,
        &mut chunk,
        42,
        &mut |_, _, _| blocked,
    ));
    let mut result = snapshot(&chunk, &own);
    if !entities.is_empty() {
        result.insert("block_entities", List::Compound(entities));
    }

    let resumed_entities =
        harpas_worldgen::place_mineshafts_in_chunk(&mut own, &mut chunk, 42, &mut |_, _, _| {
            blocked
        });
    let mut resumed = snapshot(&chunk, &own);
    if !resumed_entities.is_empty() {
        resumed.insert("block_entities", List::Compound(resumed_entities));
    }
    result.insert("resumed", resumed);
    inputs.insert(name, input);
    expected.insert(name, result);
}

#[test]
fn mineshaft_blocks_metadata_clipping_and_replays_match_vanilla() {
    let Some(reference) = Reference::from_env() else {
        eprintln!("Skipping mineshafts: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    let mut inputs = Compound::new();
    let mut expected = Compound::new();
    let states = block_states();
    for mesa in [false, true] {
        for (kind, width, height, depth, orientation) in [
            ("mscorridor", 3, 3, 10, Some(Direction::South)),
            ("mscorridor", 3, 3, 10, Some(Direction::North)),
            ("mscorridor", 3, 3, 10, Some(Direction::East)),
            ("mscorridor", 3, 3, 10, Some(Direction::West)),
            ("msroom", 9, 7, 11, None),
            ("mscrossing", 5, 3, 5, None),
            ("mscrossing", 5, 7, 5, None),
            ("msstairs", 3, 8, 9, Some(Direction::North)),
            ("msstairs", 3, 8, 9, Some(Direction::East)),
            ("msstairs", 3, 8, 9, Some(Direction::South)),
            ("msstairs", 3, 8, 9, Some(Direction::West)),
        ] {
            for scenario in 0..12 {
                let mut chunk = GeneratedChunk::new(-2, 3, -64, 384, 1);
                let material = match scenario {
                    1 => "air",
                    2 => "water",
                    3 => "lava",
                    4 => "oak_planks",
                    5 => "oak_log",
                    _ => "stone",
                };
                let material = states
                    .default_state(&format!("minecraft:{material}"))
                    .unwrap();
                for y in 45..70 {
                    for z in 0..16 {
                        for x in 0..16 {
                            chunk.set_block_state(x, y, z, material);
                        }
                    }
                }
                if scenario >= 8 {
                    let (floor, roof) = match scenario {
                        8 => (30, 80),
                        9 => (45, 55),
                        10 => (29, 110),
                        _ => (-65, 53),
                    };
                    for y in -64..120 {
                        for z in 0..16 {
                            for x in 0..16 {
                                chunk.set_block_state(
                                    x,
                                    y,
                                    z,
                                    if y <= floor || y >= roof {
                                        states.default_state("minecraft:stone").unwrap()
                                    } else {
                                        0
                                    },
                                );
                            }
                        }
                    }
                }
                let b = if let Some(o) = orientation {
                    BoundingBox::oriented([-29, 50, 51], o, width, height, depth)
                } else {
                    BoundingBox {
                        min: [-29, 50, 51],
                        max: [-29 + width - 1, 50 + height - 1, 51 + depth - 1],
                    }
                };
                let mut own = start(kind, b, orientation, mesa);
                if kind == "mscorridor" && scenario == 7 {
                    own.pieces[0].save_data.insert("sc", 1_i8);
                    own.pieces[0].save_data.insert("hr", 0_i8);
                }
                fixture(
                    &format!("{mesa}-{kind}-{height}-{orientation:?}-{scenario}"),
                    chunk,
                    vec![own],
                    scenario == 6,
                    &mut inputs,
                    &mut expected,
                );
            }
        }
    }
    for mesa in [false, true] {
        for (kind, width, height, depth, orientation) in [
            ("msroom", 9, 7, 11, None),
            ("mscrossing", 5, 7, 5, None),
            ("msstairs", 3, 8, 9, Some(Direction::East)),
            ("mscorridor", 3, 3, 20, Some(Direction::South)),
        ] {
            for y in [-65, -64, -62, 50, 315, 319, 320] {
                for (cx, cz) in [(-2, 3), (-1, 3), (-2, 4), (-1, 4)] {
                    let mut chunk = GeneratedChunk::new(cx, cz, -64, 384, 1);
                    for by in (y - 4).max(-64)..=(y + 15).min(319) {
                        for z in 0..16 {
                            for x in 0..16 {
                                chunk.set_block_state(
                                    x,
                                    by,
                                    z,
                                    states.default_state("minecraft:stone").unwrap(),
                                );
                            }
                        }
                    }
                    let bounds = if let Some(o) = orientation {
                        BoundingBox::oriented([-18, y, 62], o, width, height, depth)
                    } else {
                        BoundingBox {
                            min: [-18, y, 62],
                            max: [-18 + width - 1, y + height - 1, 62 + depth - 1],
                        }
                    };
                    let mut mine = start(kind, bounds, orientation, mesa);
                    if kind == "mscorridor" {
                        mine.pieces[0].save_data.insert("Num", 4);
                    }
                    fixture(
                        &format!("clip-{mesa}-{kind}-{y}-{cx}-{cz}"),
                        chunk,
                        vec![mine],
                        false,
                        &mut inputs,
                        &mut expected,
                    );
                }
            }
        }
    }
    for mesa in [false, true] {
        for kind in ["mscorridor", "mscrossing"] {
            for (index, (name, property)) in [
                ("air", None),
                ("cobweb", None),
                ("oak_leaves", None),
                ("oak_fence", None),
                ("dark_oak_fence", None),
                ("glass", None),
                ("dirt_path", None),
                ("farmland", None),
                ("iron_chain", None),
                ("chest", None),
                ("oak_slab", Some(("type", "bottom"))),
                ("oak_slab", Some(("type", "top"))),
                ("oak_slab", Some(("type", "double"))),
                ("oak_stairs", Some(("half", "bottom"))),
                ("oak_stairs", Some(("half", "top"))),
                ("oak_trapdoor", None),
            ]
            .into_iter()
            .enumerate()
            {
                let mut chunk = GeneratedChunk::new(-2, 3, -64, 384, 1);
                let floor = states
                    .state(&format!("minecraft:{name}"), property)
                    .unwrap()
                    .0;
                for y in 45..70 {
                    for z in 0..16 {
                        for x in 0..16 {
                            chunk.set_block_state(
                                x,
                                y,
                                z,
                                if y == 49 {
                                    floor
                                } else {
                                    states.default_state("minecraft:stone").unwrap()
                                },
                            );
                        }
                    }
                }
                let orientation = if kind == "mscorridor" {
                    Some(Direction::South)
                } else {
                    None
                };
                let bounds = BoundingBox {
                    min: [-29, 50, 51],
                    max: [
                        if kind == "mscorridor" { -27 } else { -25 },
                        52,
                        if kind == "mscorridor" { 60 } else { 55 },
                    ],
                };
                fixture(
                    &format!("floor-{mesa}-{kind}-{index}"),
                    chunk,
                    vec![start(kind, bounds, orientation, mesa)],
                    false,
                    &mut inputs,
                    &mut expected,
                );
            }
        }
    }
    // Overlapping references must share their random stream, including reference-set rehashing.
    for count in [2, 8, 25, 40] {
        let mut chunk = GeneratedChunk::new(-2, 3, -64, 384, 1);
        for y in 45..70 {
            for z in 0..16 {
                for x in 0..16 {
                    chunk.set_block_state(
                        x,
                        y,
                        z,
                        states.default_state("minecraft:stone").unwrap(),
                    );
                }
            }
        }
        let mut own = Vec::new();
        for n in 0..count {
            let mut mine = start(
                "mscorridor",
                BoundingBox::oriented([-29, 50, 51], Direction::South, 3, 3, 10),
                Some(Direction::South),
                false,
            );
            mine.chunk = (n % 8 - 4, n / 8 - 2);
            own.push(mine);
        }
        fixture(
            &format!("shared-random-{count}"),
            chunk,
            own,
            false,
            &mut inputs,
            &mut expected,
        );
    }
    // A later reference can clear an earlier spawner, or create a new one at the same position.
    for spiders in [false, true] {
        let mut chunk = GeneratedChunk::new(-2, 3, -64, 384, 1);
        for y in 45..70 {
            for z in 0..16 {
                for x in 0..16 {
                    chunk.set_block_state(
                        x,
                        y,
                        z,
                        states.default_state("minecraft:stone").unwrap(),
                    );
                }
            }
        }
        let mut first = start(
            "mscorridor",
            BoundingBox::oriented([-29, 50, 51], Direction::South, 3, 3, 10),
            Some(Direction::South),
            false,
        );
        first.chunk = (0, 0);
        first.pieces[0].save_data.insert("sc", 1_i8);
        first.pieces[0].save_data.insert("hr", 0_i8);
        let mut second = first.clone();
        second.chunk = (1, 0);
        second.pieces[0].save_data.insert("sc", i8::from(spiders));
        fixture(
            &format!("overlapping-spawners-{spiders}"),
            chunk,
            vec![first, second],
            false,
            &mut inputs,
            &mut expected,
        );
    }
    compare_cases(&reference, inputs, expected);
}

fn compare_cases(reference: &Reference, inputs: Compound, expected: Compound) {
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
            "MineshaftProbe.java",
            include_str!("../../harpas-worldgen/tests/MineshaftProbe.java"),
        ),
    ]);
    let input = reference.temp.0.join("input.nbt");
    let output = reference.temp.0.join("output.nbt");
    let mut bytes = Vec::new();
    Nbt::Compound(inputs).write_named("", &mut bytes);
    std::fs::write(&input, bytes).unwrap();
    reference.java(
        "MineshaftProbe",
        [input.display().to_string(), output.display().to_string()],
    );
    let bytes = std::fs::read(output).unwrap();
    let (_, Nbt::Compound(actual)) = Nbt::read_named(&mut bytes.as_slice()).unwrap() else {
        panic!("probe result");
    };
    let mut differences = 0;
    for (name, want) in expected.iter() {
        let Some(Nbt::Compound(got)) = actual.get(name) else {
            panic!("missing {name}");
        };
        let Nbt::Compound(want) = want else {
            unreachable!();
        };
        for (key, value) in want.iter() {
            if let Nbt::IntArray(want) = value {
                let Some(Nbt::IntArray(got)) = got.get(key) else {
                    panic!("{name} {key}");
                };
                for (i, (a, b)) in want.iter().zip(got).enumerate() {
                    if a != b {
                        eprintln!(
                            "{name} {key} ({},{},{}) {a} != {b}",
                            i % 16,
                            i / 256,
                            (i / 16) % 16
                        );
                        differences += 1;
                        break;
                    }
                }
            } else if Some(value) != got.get(key) {
                eprintln!("{name} {key} metadata differs");
                differences += 1;
            }
        }
    }
    assert_eq!(differences, 0);
}

#[test]
fn workers_generate_mines_of_both_types_and_preserve_anvil_data() {
    use harpas_engine::world::{ChunkPos, ChunkState, World};
    use harpas_engine::worldgen::overworld_generator;
    use harpas_worldgen::{
        NoiseBasedChunkGenerator, StructureState, StructureTemplates, WorldgenData,
    };
    use std::collections::{BTreeMap, BTreeSet, HashMap};
    use std::sync::Arc;
    let Some(reference) = Reference::from_env() else {
        eprintln!("Skipping mineshaft workers: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    let mut data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    data.structure_sets.retain(|_, set| {
        set.structures.iter().all(|s| {
            matches!(
                s.structure.as_str(),
                "minecraft:mineshaft" | "minecraft:mineshaft_mesa" | "minecraft:buried_treasure"
            )
        })
    });
    let data = Arc::new(data);
    let templates = Arc::new(StructureTemplates::from_zip_path(&reference.server_jar).unwrap());
    let names: Vec<_> = data.biomes.keys().map(String::as_str).collect();
    let blocking = data
        .resolve_biomes(&["#minecraft:mineshaft_blocking".into()])
        .unwrap();
    let mut inputs = Compound::new();
    let mut expected = Compound::new();
    for &seed in reference::sampled_seeds(&[-4_172_144_997_902_289_642, 0, 42]) {
        let terrain = NoiseBasedChunkGenerator::overworld(seed, &data).unwrap();
        let state = StructureState::new(seed, &data, &terrain).unwrap();
        let mut examples = BTreeMap::new();
        let mut known = HashMap::new();
        'search: for radius in 0_i32..if seed == -4_172_144_997_902_289_642 {
            0
        } else {
            256
        } {
            for x in -radius..=radius {
                for z in -radius..=radius {
                    if x.abs().max(z.abs()) != radius {
                        continue;
                    }
                    let own = state
                        .starts_in_chunk(&terrain, &data, &templates, x, z)
                        .unwrap();
                    for mine in own
                        .iter()
                        .filter(|s| s.structure != "minecraft:buried_treasure")
                    {
                        examples
                            .entry(mine.structure.clone())
                            .or_insert_with(|| mine.clone());
                    }
                    known.insert((x, z), own);
                    if examples.len() == 2 {
                        break 'search;
                    }
                }
            }
        }
        if seed != -4_172_144_997_902_289_642 {
            assert_eq!(examples.len(), 2, "both mineshaft types for seed {seed}");
        }
        let mut positions = BTreeSet::new();
        if seed == -4_172_144_997_902_289_642 {
            positions.insert((-26, -28));
        }
        for mine in examples.values() {
            eprintln!("seed {seed} {} starts at {:?}", mine.structure, mine.chunk);
            positions.insert(mine.chunk);
            let corridor = mine
                .pieces
                .iter()
                .find(|p| p.kind == "minecraft:mscorridor")
                .expect("corridor");
            positions.insert((
                corridor.bounding_box.max[0] >> 4,
                corridor.bounding_box.max[2] >> 4,
            ));
        }
        let generator = overworld_generator(
            seed,
            Arc::clone(&data),
            Arc::clone(&templates),
            true,
            &names,
        )
        .unwrap();
        let reversed = overworld_generator(
            seed,
            Arc::clone(&data),
            Arc::clone(&templates),
            true,
            &names,
        )
        .unwrap();
        let mut saved = BTreeMap::new();
        let mut changed = 0;
        for &(x, z) in &positions {
            let pos = ChunkPos { x, z };
            let mut nearby = Vec::new();
            for sx in x - 8..=x + 8 {
                for sz in z - 8..=z + 8 {
                    let own = known.entry((sx, sz)).or_insert_with(|| {
                        state
                            .starts_in_chunk(&terrain, &data, &templates, sx, sz)
                            .unwrap()
                    });
                    nearby.extend(
                        own.iter()
                            .filter(|s| {
                                s.bounding_box.intersects_columns(
                                    x * 16,
                                    z * 16,
                                    x * 16 + 15,
                                    z * 16 + 15,
                                )
                            })
                            .cloned(),
                    );
                }
            }
            if seed == -4_172_144_997_902_289_642 {
                assert!(
                    nearby
                        .iter()
                        .any(|s| s.structure == "minecraft:buried_treasure")
                );
                assert!(nearby.iter().any(|s| s.structure == "minecraft:mineshaft"));
            }
            let base = terrain.generate_chunk_carved(x, z, &data).unwrap();
            let mut noise = terrain.create_noise_chunk(x, z);
            let input = case_input(&base, seed, &nearby, &mut |bx, by, bz| {
                let id = terrain.biome_manager().get_biome(bx, by, bz, |qx, qy, qz| {
                    terrain.biome_source().neighbor_biome(
                        &mut noise,
                        qx,
                        qy.clamp(-16, 79),
                        qz,
                        -16,
                        79,
                    )
                });
                blocking.contains(&id)
            });
            let actual = generator(pos, &|_| Ok(None)).unwrap();
            let mut placed = GeneratedChunk::new(x, z, -64, 384, data.biomes.len());
            for section in &actual.sections {
                if let Some(blocks) = &section.section {
                    placed.sections_mut()[(i32::from(section.y) + 4) as usize] = blocks.clone();
                }
            }
            placed.recompute_heightmaps();
            if blocks(&base) != blocks(&placed) {
                changed += 1;
            }
            let mut result = snapshot(&placed, &[]);
            result.remove("starts");
            result.insert(
                "PostProcessing",
                actual
                    .rest
                    .get("PostProcessing")
                    .cloned()
                    .unwrap_or_else(|| Nbt::List(List::List(vec![List::Empty; 24]))),
            );
            result.insert(
                "block_entities",
                actual
                    .rest
                    .get("block_entities")
                    .cloned()
                    .unwrap_or(Nbt::List(List::Empty)),
            );
            let name = format!("worker-{seed}-{x}-{z}");
            inputs.insert(name.clone(), input);
            expected.insert(name, result);
            saved.insert((x, z), actual.to_nbt(&names));
        }
        assert!(
            changed > 0,
            "workers must place mine blocks for seed {seed}"
        );
        for (&(x, z), want) in saved.iter().rev() {
            assert_eq!(
                &reversed(ChunkPos { x, z }, &|_| Ok(None))
                    .unwrap()
                    .to_nbt(&names),
                want,
                "reversed requests {seed} {x} {z}"
            );
        }
        let folder = reference.temp.0.join(format!("world-{seed}"));
        let mut world = World::open_with_generator(&folder, &names, generator).unwrap();
        let positions: Vec<_> = positions
            .into_iter()
            .map(|(x, z)| ChunkPos { x, z })
            .collect();
        wait(&mut world, &positions);
        for pos in &positions {
            let Some(ChunkState::Loaded(chunk)) = world.chunk(*pos) else {
                panic!("loaded chunk");
            };
            assert_eq!(
                chunk.to_nbt(&names),
                saved[&(pos.x, pos.z)],
                "worker result {seed} {pos:?}"
            );
        }
        world.flush().unwrap();
        drop(world);
        let mut world = World::open(&folder, &names).unwrap();
        wait(&mut world, &positions);
        for pos in positions {
            let Some(ChunkState::Loaded(chunk)) = world.chunk(pos) else {
                panic!("reloaded chunk");
            };
            assert_eq!(
                chunk.to_nbt(&names),
                saved[&(pos.x, pos.z)],
                "Anvil reload {seed} {pos:?}"
            );
        }
    }
    compare_cases(&reference, inputs, expected);
}

fn wait(world: &mut harpas_engine::world::World, positions: &[harpas_engine::world::ChunkPos]) {
    use harpas_engine::world::ChunkState;
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        world.update(positions.iter().copied()).unwrap();
        if positions
            .iter()
            .all(|p| matches!(world.chunk(*p), Some(ChunkState::Loaded(_))))
        {
            break;
        }
        for pos in positions {
            if let Some(ChunkState::Failed(error)) = world.chunk(*pos) {
                panic!("{pos:?}: {error}");
            }
        }
        assert!(Instant::now() < deadline, "worker timeout");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn workers_use_saved_mine_records_and_neighbor_biomes() {
    use harpas_engine::world::ChunkPos;
    use harpas_engine::worldgen::overworld_generator;
    use harpas_worldgen::{
        BiomeManager, NoiseBasedChunkGenerator, StructureTemplates, WorldgenData,
    };
    use std::sync::Arc;
    let Some(reference) = Reference::from_env() else {
        eprintln!("Skipping saved mineshafts: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    let data = Arc::new(WorldgenData::from_zip_path(&reference.server_jar).unwrap());
    let templates = Arc::new(StructureTemplates::from_zip_path(&reference.server_jar).unwrap());
    let names: Vec<_> = data.biomes.keys().map(String::as_str).collect();
    let target = ChunkPos { x: 1, z: 1 };
    let source = ChunkPos { x: 0, z: 0 };
    let mut mine = start(
        "mscorridor",
        BoundingBox::oriented([15, 20, 14], Direction::South, 3, 3, 20),
        Some(Direction::South),
        false,
    );
    mine.pieces[0].save_data.insert("Num", 4);
    mine.pieces[0].save_data.insert("sc", 1_i8);
    mine.pieces[0].save_data.insert("hr", 0_i8);
    let mut selected = None;
    for seed in 0..64 {
        let terrain = NoiseBasedChunkGenerator::overworld(seed, &data).unwrap();
        let base = terrain
            .generate_chunk_carved(target.x, target.z, &data)
            .unwrap();
        let [x, y, z] = mine.mineshaft_biome_samples(&base).next().unwrap();
        let mut neighbor = target;
        BiomeManager::from_world_seed(seed).get_biome(x, y, z, |qx, _, qz| {
            neighbor = ChunkPos {
                x: qx >> 2,
                z: qz >> 2,
            };
            0
        });
        if neighbor == target {
            continue;
        }
        let mut placed = base.clone();
        let entities = mine
            .clone()
            .place_mineshaft_in_chunk(&mut placed, seed, &mut |_, _, _| false);
        if !entities.is_empty() {
            selected = Some((seed, terrain, base, placed, neighbor, entities));
            break;
        }
    }
    let (seed, terrain, base, placed, neighbor, entities) =
        selected.expect("saved-neighbor biome sample and spider corridor");
    let mut starts = Compound::new();
    starts.insert("minecraft:mineshaft", mine.to_nbt());
    let mut records = Compound::new();
    records.insert("starts", starts);
    records.insert("References", Compound::new());
    let mut source_tag = Compound::new();
    source_tag.insert("Status", "minecraft:carvers");
    source_tag.insert("structures", records);
    let mut empty = source_tag.clone();
    let mut records = Compound::new();
    records.insert("starts", Compound::new());
    records.insert("References", Compound::new());
    empty.insert("structures", records);
    let mut neighbor_chunk = terrain
        .generate_chunk_carved(neighbor.x, neighbor.z, &data)
        .unwrap();
    for section in neighbor_chunk.sections_mut() {
        for y in 0..4 {
            for z in 0..4 {
                for x in 0..4 {
                    section.set_biome(x, y, z, data.biome_id("minecraft:deep_dark").unwrap());
                }
            }
        }
    }
    let mut blocked_neighbor = harpas_engine::worldgen::into_anvil(neighbor_chunk)
        .unwrap()
        .to_nbt(&names);
    blocked_neighbor.insert(
        "structures",
        if neighbor == source {
            source_tag.get("structures").unwrap().clone()
        } else {
            empty.get("structures").unwrap().clone()
        },
    );
    for blocked in [false, true] {
        let generator = overworld_generator(
            seed,
            Arc::clone(&data),
            Arc::clone(&templates),
            true,
            &names,
        )
        .unwrap();
        let actual = generator(target, &|p| {
            Ok(Some(if blocked && p == neighbor {
                blocked_neighbor.clone()
            } else if p == source {
                source_tag.clone()
            } else {
                empty.clone()
            }))
        })
        .unwrap();
        let baseline = if blocked {
            base.clone()
        } else {
            placed.clone()
        };
        let mut expected = harpas_engine::worldgen::into_anvil(baseline).unwrap();
        let mut references = Compound::new();
        references.insert("minecraft:mineshaft", Nbt::LongArray(vec![0]));
        let mut records = Compound::new();
        records.insert("starts", Compound::new());
        records.insert("References", references);
        expected.rest.insert("structures", records);
        if !blocked {
            expected
                .rest
                .insert("block_entities", List::Compound(entities.clone()));
        }
        assert_eq!(
            actual.to_nbt(&names),
            expected.to_nbt(&names),
            "saved neighbor blocked={blocked}"
        );
    }
    assert_eq!(
        mine.pieces[0].save_data.get("hps"),
        Some(&Nbt::Byte(0)),
        "saved source flags remain deferred"
    );
}
