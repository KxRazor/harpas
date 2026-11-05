//! Treasure placement against vanilla, including carved worker terrain, Anvil reloads and vanilla resumption.

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

fn treasure(x: i32, z: i32) -> StructureStart {
    let bounds = BoundingBox {
        min: [x, 90, z],
        max: [x, 90, z],
    };
    StructureStart {
        structure: "minecraft:buried_treasure".into(),
        chunk: (x >> 4, z >> 4),
        references: 0,
        bounding_box: bounds,
        pieces: vec![StructurePiece {
            kind: "minecraft:btp".into(),
            bounding_box: bounds,
            orientation: None,
            gen_depth: 0,
            junctions: Vec::new(),
            jigsaw: None,
            template: None,
            save_data: Compound::new(),
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
    tag.insert("starts", starts(own));
    tag
}

fn snapshot(chunk: &GeneratedChunk, own: &[StructureStart], entities: Vec<Compound>) -> Compound {
    let mut tag = Compound::new();
    tag.insert("blocks", blocks(chunk));
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
fn treasure_blocks_metadata_workers_and_vanilla_resumption_match() {
    let Some(reference) = Reference::from_env() else {
        eprintln!("Skipping treasure placement: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    let mut inputs = Compound::new();
    let mut expected = Compound::new();
    let states = block_states();
    let stone = states.default_state("minecraft:stone").unwrap();
    let flowing = states.state("minecraft:water", [("level", "1")]).unwrap().0;
    // Every accepted foundation, several rejected ones, source/flowing fluids and open shells.
    let foundations = [
        "stone",
        "sandstone",
        "andesite",
        "granite",
        "diorite",
        "deepslate",
        "bedrock",
        "dirt",
        "sand",
        "gravel",
        "tuff",
    ];
    let covers = [
        "air",
        "cave_air",
        "water",
        "lava",
        "gravel",
        "sand",
        "ice",
        "snow",
        "powder_snow",
    ];
    let mut scenarios = Vec::new();
    for foundation in foundations {
        scenarios.push((
            states
                .default_state(&format!("minecraft:{foundation}"))
                .unwrap(),
            0,
            15,
        ));
    }
    for cover in covers {
        scenarios.push((
            stone,
            states.default_state(&format!("minecraft:{cover}")).unwrap(),
            0,
        ));
    }
    for mask in 0..16 {
        scenarios.push((stone, flowing, mask));
    }
    scenarios.push((
        stone,
        states.state("minecraft:lava", [("level", "3")]).unwrap().0,
        0,
    ));
    scenarios.push((stone, 0, 0)); // Foundation at the build limit: no out-of-world chest.
    for (index, (foundation, cover, mask)) in scenarios.into_iter().enumerate() {
        let (cx, cz) = if index % 2 == 0 { (-2, 3) } else { (2, -3) };
        let mut chunk = GeneratedChunk::new(cx, cz, -64, 384, 1);
        // A foundation only under the center exercises filling unsupported air and fluid neighbors.
        let foundation_y = if index == 37 { 319 } else { 50 };
        chunk.set_block_state(9, foundation_y, 9, foundation);
        chunk.set_block_state(9, foundation_y + 1, 9, cover);
        for (bit, (x, z)) in [(9, 8), (10, 9), (9, 10), (8, 9)].into_iter().enumerate() {
            if mask & (1 << bit) != 0 {
                chunk.set_block_state(x, 51, z, stone);
            } else {
                chunk.set_block_state(
                    x,
                    51,
                    z,
                    if (20..36).contains(&index) {
                        states.default_state("minecraft:ice").unwrap()
                    } else {
                        flowing
                    },
                );
            }
        }
        let mut own = vec![treasure(cx * 16 + 9, cz * 16 + 9)];
        let seed = [0, 42, -1, i64::MIN][index % 4];
        let name = format!("synthetic-{index}");
        inputs.insert(name.clone(), input(&chunk, seed, &own));
        let entities = own[0].place_in_chunk(
            &mut chunk,
            seed,
            &mut |_, _| unreachable!("no pyramid"),
            &mut |_, _, _| unreachable!("no pyramid"),
        );
        expected.insert(name, snapshot(&chunk, &own, entities));
    }
    // Actual chunk workers: discover starts from each seed and use the independently generated carved
    // terrain as input to vanilla's placement. Terrain/carver parity has its own block-by-block harness.
    let mut data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    // This probe compares treasure placement alone. A shipwreck can occupy the same chunk;
    // shipwreck_parity covers that composition against vanilla's shipwreck pass separately.
    data.structure_sets.shift_remove("minecraft:shipwrecks");
    data.structure_sets.shift_remove("minecraft:mineshafts");
    let data = Arc::new(data);
    let templates = Arc::new(StructureTemplates::from_zip_path(&reference.server_jar).unwrap());
    let biomes: Vec<_> = data.biomes.keys().map(String::as_str).collect();
    for &seed in reference::sampled_seeds(&[0, -4_172_144_997_902_289_642]) {
        let terrain = NoiseBasedChunkGenerator::overworld(seed, &data).unwrap();
        let state = StructureState::new(seed, &data, &terrain).unwrap();
        let mut targets = Vec::new();
        'search: for radius in 0_i32..128 {
            for x in -radius..=radius {
                for z in -radius..=radius {
                    if x.abs().max(z.abs()) != radius {
                        continue;
                    }
                    let own = state
                        .starts_in_chunk(&terrain, &data, &templates, x, z)
                        .unwrap();
                    if own
                        .iter()
                        .any(|s| s.structure == "minecraft:buried_treasure")
                    {
                        targets.push((ChunkPos { x, z }, own));
                    }
                    if targets.len() == 4 {
                        break 'search;
                    }
                }
            }
        }
        assert_eq!(targets.len(), 4, "treasure starts for {seed}");
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
        let mut chests = 0;
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
            let Some(Nbt::Compound(structures)) = nbt.get("structures") else {
                panic!("structures");
            };
            actual.insert("starts", structures.get("starts").unwrap().clone());
            actual.insert("block_entities", nbt.get("block_entities").unwrap().clone());
            actual.insert("Heightmaps", nbt.get("Heightmaps").unwrap().clone());
            if matches!(nbt.get("block_entities"), Some(Nbt::List(List::Compound(v))) if !v.is_empty())
            {
                chests += 1;
            }
            expected.insert(name, actual);
            saved.push((pos, nbt));
        }
        assert!(chests > 0, "must place a chest for seed {seed}");
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
    ]);
    let input_path = reference.temp.0.join("treasures-input.nbt");
    let output_path = reference.temp.0.join("treasures-output.nbt");
    let mut bytes = Vec::new();
    Nbt::Compound(inputs).write_named("", &mut bytes);
    std::fs::write(&input_path, bytes).unwrap();
    reference.java(
        "BuriedTreasureProbe",
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
        assert_eq!(expected, actual, "{name} metadata");
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
