//! Swamp hut placement against vanilla, including carved worker terrain, Anvil reloads and vanilla resumption.

#[allow(dead_code)]
#[path = "../../harpas-worldgen/tests/reference/mod.rs"]
mod reference;

use std::sync::Arc;
use std::time::{Duration, Instant};

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_engine::world::{ChunkPos, ChunkState, World};
use harpas_engine::worldgen::overworld_generator;
use harpas_worldgen::{
    Beardifier, BoundingBox, GeneratedChunk, HeightmapType, NoiseBasedChunkGenerator,
    StructurePiece, StructureStart, StructureState, StructureTemplates, WorldgenData,
};
use reference::Reference;

fn hut(x: i32, z: i32, direction: harpas_worldgen::Direction) -> StructureStart {
    let bounds = BoundingBox::oriented([x, 64, z], direction, 7, 7, 9);
    let mut save_data = Compound::new();
    for (key, value) in [("Width", 7), ("Height", 7), ("Depth", 9), ("HPos", -1)] {
        save_data.insert(key, value);
    }
    save_data.insert("Witch", 0_i8);
    save_data.insert("Cat", 0_i8);
    StructureStart {
        structure: "minecraft:swamp_hut".into(),
        chunk: (x >> 4, z >> 4),
        references: 0,
        bounding_box: bounds,
        pieces: vec![StructurePiece {
            kind: "minecraft:tesh".into(),
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

fn snapshot(chunk: &GeneratedChunk, own: &[StructureStart], entities: Vec<Compound>) -> Compound {
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
fn hut_blocks_metadata_workers_and_vanilla_resumption_match() {
    let Some(reference) = Reference::from_env() else {
        eprintln!("Skipping hut placement: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    let mut inputs = Compound::new();
    let mut expected = Compound::new();
    let states = block_states();
    let stone = states.default_state("minecraft:stone").unwrap();
    let data = Arc::new(WorldgenData::from_zip_path(&reference.server_jar).unwrap());
    let templates = Arc::new(StructureTemplates::from_zip_path(&reference.server_jar).unwrap());
    use harpas_worldgen::Direction::{East, North, South, West};
    for (di, direction) in [North, East, South, West].into_iter().enumerate() {
        // Every orientation, positive/negative chunks, sloping ground, leaves, fluid and a solid interior.
        for scenario in 0..6 {
            let (cx, cz) = if di % 2 == 0 { (-2, 3) } else { (2, -3) };
            let mut chunk = GeneratedChunk::new(cx, cz, -64, 384, 1);
            for z in 0..16 {
                for x in 0..16 {
                    let top = match scenario {
                        1 => 55 + ((x + z) % 5) as i32,
                        5 => -64,
                        _ => 55,
                    };
                    chunk.set_block_state(x, top, z, stone);
                    let (cover, height) = match scenario {
                        2 => ("oak_leaves", 80),
                        3 => ("water", 63),
                        4 => ("stone", 60),
                        _ => ("air", top),
                    };
                    for y in top + 1..=height {
                        chunk.set_block_state(
                            x,
                            y,
                            z,
                            states.default_state(&format!("minecraft:{cover}")).unwrap(),
                        );
                    }
                }
            }
            let own = vec![hut(cx * 16, cz * 16, direction)];
            fixture(
                &format!("terrain-{di}-{scenario}"),
                chunk,
                own,
                &mut inputs,
                &mut expected,
            );
        }
        for (i, material) in [
            "air",
            "water",
            "lava",
            "seagrass",
            "tall_seagrass",
            "glow_lichen",
            "bubble_column",
            "kelp",
            "vine",
            "oak_leaves",
            "stone",
            "snow",
            "powder_snow",
        ]
        .into_iter()
        .enumerate()
        {
            let mut chunk = GeneratedChunk::new(-2, 3, -64, 384, 1);
            for z in 0..16 {
                for x in 0..16 {
                    chunk.set_block_state(x, 55, z, stone);
                    for y in 56..64 {
                        chunk.set_block_state(
                            x,
                            y,
                            z,
                            states
                                .default_state(&format!("minecraft:{material}"))
                                .unwrap(),
                        );
                    }
                }
            }
            let mut own = vec![hut(-32, 48, direction)];
            own[0].pieces[0].save_data.insert("HPos", 64);
            fixture(
                &format!("supports-{di}-{i}"),
                chunk,
                own,
                &mut inputs,
                &mut expected,
            );
        }
        // A saved piece crossing four chunks keeps its ground offset after each NBT reload. Different
        // neighbor heights must not move it again; only the intersection with the target is written.
        let mut own = vec![hut(-18, 62, direction)];
        for (i, (cx, cz)) in [(-2, 3), (-1, 3), (-2, 4), (-1, 4)].into_iter().enumerate() {
            let mut chunk = GeneratedChunk::new(cx, cz, -64, 384, 1);
            for z in 0..16 {
                for x in 0..16 {
                    chunk.set_block_state(x, 55 + i as i32 * 2, z, stone);
                }
            }
            let name = format!("boundary-{di}-{i}");
            inputs.insert(name.clone(), input(&chunk, 42, &own));
            let entities = own[0].place_in_chunk(
                &mut chunk,
                42,
                &mut |_, _| unreachable!("no pyramid"),
                &mut |_, _, _| unreachable!("no pyramid"),
            );
            expected.insert(name, snapshot(&chunk, &own, entities));
            own[0] = StructureStart::from_nbt(&own[0].to_nbt(), &data, &templates)
                .unwrap()
                .unwrap();
        }
    }
    let biomes: Vec<_> = data.biomes.keys().map(String::as_str).collect();
    for &seed in reference::sampled_seeds(&[0, -4_172_144_997_902_289_642]) {
        let terrain = NoiseBasedChunkGenerator::overworld(seed, &data).unwrap();
        let state = StructureState::new(seed, &data, &terrain).unwrap();
        let mut targets = Vec::new();
        'search: for radius in 0_i32..1024 {
            for x in -radius..=radius {
                for z in -radius..=radius {
                    if x.abs().max(z.abs()) != radius
                        || !state.is_structure_chunk("minecraft:swamp_huts", x, z)
                    {
                        continue;
                    }
                    let own = state
                        .starts_in_chunk(&terrain, &data, &templates, x, z)
                        .unwrap();
                    if own.iter().any(|s| s.structure == "minecraft:swamp_hut") {
                        targets.push((ChunkPos { x, z }, own));
                    }
                    if targets.len() == 2 {
                        break 'search;
                    }
                }
            }
        }
        assert_eq!(targets.len(), 2, "hut starts for {seed}");
        eprintln!(
            "Seed {seed}: natural huts {:?}",
            targets.iter().map(|(p, _)| p).collect::<Vec<_>>()
        );
        let disabled = overworld_generator(
            seed,
            Arc::clone(&data),
            Arc::clone(&templates),
            false,
            &biomes,
        )
        .unwrap();
        let pos = targets[0].0;
        let disabled =
            disabled(pos, &|_| panic!("disabled structures must not read starts")).unwrap();
        let baseline = harpas_engine::worldgen::into_anvil(
            terrain.generate_chunk_carved(pos.x, pos.z, &data).unwrap(),
        )
        .unwrap();
        assert_eq!(disabled.to_nbt(&biomes), baseline.to_nbt(&biomes));
        let folder = reference.temp.0.join(format!("world-{seed}"));
        let generator = overworld_generator(
            seed,
            Arc::clone(&data),
            Arc::clone(&templates),
            true,
            &biomes,
        )
        .unwrap();
        let mut world = World::open_with_generator(&folder, &biomes, generator).unwrap();
        let positions: Vec<_> = targets.iter().map(|(p, _)| *p).collect();
        wait(&mut world, &positions);
        let mut saved = Vec::new();
        let mut huts = 0;
        for (pos, own) in targets {
            let mut nearby = Vec::new();
            for x in pos.x - 8..=pos.x + 8 {
                for z in pos.z - 8..=pos.z + 8 {
                    nearby.extend(
                        state
                            .starts_in_chunk(&terrain, &data, &templates, x, z)
                            .unwrap(),
                    );
                }
            }
            let beardifier = Beardifier::for_starts(pos.x, pos.z, &data, &nearby).unwrap();
            let baseline = terrain
                .generate_chunk_carved_with_beardifier(pos.x, pos.z, &data, beardifier)
                .unwrap();
            let name = format!("worker-{seed}-{},{}", pos.x, pos.z);
            inputs.insert(name.clone(), input(&baseline, seed, &own));
            let Some(ChunkState::Loaded(chunk)) = world.chunk(pos) else {
                panic!("loaded chunk");
            };
            let nbt = chunk.to_nbt(&biomes);
            let mut generated = GeneratedChunk::new(pos.x, pos.z, -64, 384, biomes.len());
            for y in -64..320 {
                for z in 0..16 {
                    for x in 0..16 {
                        let section = chunk.sections[((y + 64) / 16) as usize]
                            .section
                            .as_ref()
                            .unwrap();
                        generated.set_block_state(
                            x,
                            y,
                            z,
                            section.block_state(x, (y & 15) as usize, z),
                        );
                    }
                }
            }
            let mut actual = Compound::new();
            actual.insert("blocks", blocks(&generated));
            actual.insert("PostProcessing", nbt.get("PostProcessing").unwrap().clone());
            let Some(Nbt::Compound(structures)) = nbt.get("structures") else {
                panic!("structures");
            };
            actual.insert("starts", structures.get("starts").unwrap().clone());
            actual.insert("block_entities", nbt.get("block_entities").unwrap().clone());
            actual.insert("Heightmaps", nbt.get("Heightmaps").unwrap().clone());
            let Some(Nbt::Compound(starts)) = structures.get("starts") else {
                panic!("starts");
            };
            let Some(Nbt::Compound(start)) = starts.get("minecraft:swamp_hut") else {
                panic!("hut");
            };
            let Some(Nbt::List(List::Compound(pieces))) = start.get("Children") else {
                panic!("pieces");
            };
            assert!(matches!(pieces[0].get("HPos"), Some(Nbt::Int(y)) if *y >= 0));
            assert_eq!(pieces[0].get("Witch"), Some(&Nbt::Byte(0)));
            assert_eq!(pieces[0].get("Cat"), Some(&Nbt::Byte(0)));
            huts += 1;
            expected.insert(name, actual);
            saved.push((pos, nbt));
        }
        assert_eq!(huts, 2, "must place huts for seed {seed}");
        world.flush().unwrap();
        drop(world);
        let mut world = World::open(&folder, &biomes).unwrap();
        wait(&mut world, &positions);
        for (pos, nbt) in saved {
            let Some(ChunkState::Loaded(chunk)) = world.chunk(pos) else {
                panic!("reloaded chunk");
            };
            assert_eq!(chunk.to_nbt(&biomes), nbt, "Anvil reload {pos:?}");
        }
    }
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
            "SwampHutProbe.java",
            include_str!("../../harpas-worldgen/tests/SwampHutProbe.java"),
        ),
    ]);
    let input_path = reference.temp.0.join("huts-input.nbt");
    let output_path = reference.temp.0.join("huts-output.nbt");
    let mut bytes = Vec::new();
    Nbt::Compound(inputs).write_named("", &mut bytes);
    std::fs::write(&input_path, bytes).unwrap();
    reference.java(
        "SwampHutProbe",
        [
            input_path.display().to_string(),
            output_path.display().to_string(),
        ],
    );
    let bytes = std::fs::read(output_path).unwrap();
    let (_, Nbt::Compound(actual)) = Nbt::read_named(&mut bytes.as_slice()).unwrap() else {
        panic!("probe NBT");
    };
    for (name, tag) in expected.iter() {
        let Nbt::Compound(mut expected) = tag.clone() else {
            unreachable!();
        };
        let Some(Nbt::Compound(mut actual)) = actual.get(name).cloned() else {
            panic!("missing {name}");
        };
        let Some(Nbt::IntArray(want)) = expected.remove("blocks") else {
            panic!("blocks");
        };
        let Some(Nbt::IntArray(got)) = actual.remove("blocks") else {
            panic!("blocks");
        };
        assert_eq!(want.len(), got.len(), "{name} block count");
        for (i, (&want, &got)) in want.iter().zip(&got).enumerate() {
            assert_eq!(
                want,
                got,
                "{name} block ({},{},{})",
                i % 16,
                (i / 256) as i32 - 64,
                (i / 16) % 16
            );
        }
        // Worker Anvil records keep the four persistent heightmaps; direct fixtures compare all six.
        if name.starts_with("worker-") {
            let Some(Nbt::Compound(mut maps)) = actual.remove("Heightmaps") else {
                panic!("maps");
            };
            maps.remove("WORLD_SURFACE_WG");
            maps.remove("OCEAN_FLOOR_WG");
            actual.insert("Heightmaps", maps);
        }
        for (key, value) in expected.iter() {
            assert_eq!(Some(value), actual.get(key), "{name} {key}");
        }
        assert_eq!(expected.len(), actual.len(), "{name} metadata fields");
    }
}

fn wait(world: &mut World, positions: &[ChunkPos]) {
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

fn fixture(
    name: &str,
    mut chunk: GeneratedChunk,
    mut own: Vec<StructureStart>,
    inputs: &mut Compound,
    expected: &mut Compound,
) {
    inputs.insert(name, input(&chunk, 42, &own));
    let entities = own[0].place_in_chunk(
        &mut chunk,
        42,
        &mut |_, _| unreachable!("no pyramid"),
        &mut |_, _, _| unreachable!("no pyramid"),
    );
    expected.insert(name, snapshot(&chunk, &own, entities));
    let entities = own[0].place_in_chunk(
        &mut chunk,
        42,
        &mut |_, _| unreachable!("no pyramid"),
        &mut |_, _, _| unreachable!("no pyramid"),
    );
    let mut resumed = snapshot(&chunk, &own, entities);
    resumed.remove("PostProcessing");
    let Some(Nbt::Compound(mut input)) = inputs.remove(name) else {
        panic!("input");
    };
    input.insert("resumed", resumed);
    inputs.insert(name, input);
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

#[test]
fn workers_place_saved_neighbor_huts_and_preserve_source_records() {
    let Some(reference) = Reference::from_env() else {
        eprintln!("Skipping saved neighbor huts: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    let data = Arc::new(WorldgenData::from_zip_path(&reference.server_jar).unwrap());
    let templates = Arc::new(StructureTemplates::from_zip_path(&reference.server_jar).unwrap());
    let biomes: Vec<_> = data.biomes.keys().map(String::as_str).collect();
    let seed = 42;
    let pos = ChunkPos { x: 1, z: 1 };
    let mut saved = hut(14, 14, harpas_worldgen::Direction::South);
    saved.pieces[0].save_data.insert("HPos", 64);
    saved.pieces[0].save_data.insert("Cat", 1_i8);
    let source_tag = saved.to_nbt();
    let mut source = Compound::new();
    source.insert("Status", "minecraft:carvers");
    let mut records = Compound::new();
    records.insert("starts", starts(&[saved.clone()]));
    records.insert("References", Compound::new());
    source.insert("structures", records);
    let mut empty = source.clone();
    let mut records = Compound::new();
    records.insert("starts", Compound::new());
    records.insert("References", Compound::new());
    empty.insert("structures", records);
    let reader = |p: ChunkPos| {
        Ok(Some(if p == (ChunkPos { x: 0, z: 0 }) {
            source.clone()
        } else {
            empty.clone()
        }))
    };
    let terrain = NoiseBasedChunkGenerator::overworld(seed, &data).unwrap();
    let mut baseline = terrain.generate_chunk_carved(pos.x, pos.z, &data).unwrap();
    saved.place_in_chunk(
        &mut baseline,
        seed,
        &mut |_, _| unreachable!("no pyramid"),
        &mut |_, _, _| unreachable!("no pyramid"),
    );
    let mut expected = harpas_engine::worldgen::into_anvil(baseline).unwrap();
    let mut references = Compound::new();
    references.insert("minecraft:swamp_hut", Nbt::LongArray(vec![0]));
    let mut records = Compound::new();
    records.insert("starts", Compound::new());
    records.insert("References", references);
    expected.rest.insert("structures", records);
    let generator = overworld_generator(seed, Arc::clone(&data), templates, true, &biomes).unwrap();
    let actual = generator(pos, &reader).unwrap();
    assert_eq!(actual.to_nbt(&biomes), expected.to_nbt(&biomes));
    // The source record is not copied into the target or mutated by placing its intersection.
    let Nbt::Compound(records) = source.get("structures").unwrap() else {
        panic!("structures");
    };
    let Nbt::Compound(starts) = records.get("starts").unwrap() else {
        panic!("starts");
    };
    assert_eq!(
        starts.get("minecraft:swamp_hut"),
        Some(&Nbt::Compound(source_tag))
    );
    let folder = reference.temp.0.join("neighbor-world");
    let generator = Arc::new(move |p, _: &harpas_engine::world::ChunkReader<'_>| {
        generator(p, &|source_pos| {
            Ok(Some(if source_pos == (ChunkPos { x: 0, z: 0 }) {
                source.clone()
            } else {
                empty.clone()
            }))
        })
    });
    let mut world = World::open_with_generator(&folder, &biomes, generator).unwrap();
    wait(&mut world, &[pos]);
    let Some(ChunkState::Loaded(chunk)) = world.chunk(pos) else {
        panic!("generated neighbor");
    };
    assert_eq!(chunk.to_nbt(&biomes), expected.to_nbt(&biomes));
    world.flush().unwrap();
    drop(world);
    let mut world = World::open(&folder, &biomes).unwrap();
    wait(&mut world, &[pos]);
    let Some(ChunkState::Loaded(chunk)) = world.chunk(pos) else {
        panic!("reloaded neighbor");
    };
    assert_eq!(chunk.to_nbt(&biomes), expected.to_nbt(&biomes));
}
