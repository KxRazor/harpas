//! Chunk workers, structure terrain and Anvil persistence: compares terrain and structure records against
//! vanilla, then checks that saved chunks reload unchanged.

#[allow(dead_code)]
#[path = "../../harpas-worldgen/tests/reference/mod.rs"]
mod reference;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use harpas_core::nbt::Nbt;
use harpas_engine::world::{ChunkPos, ChunkState, World};
use harpas_engine::worldgen::overworld_generator;
use harpas_worldgen::{StructureTemplates, WorldgenData};
use reference::VanillaData;

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn workers_generate_and_persist_a_real_chunk() {
    let vanilla = VanillaData::required();
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let folder = std::env::temp_dir().join(format!(
        "harpas-generated-chunk-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&folder).unwrap();
    let data = Arc::new(WorldgenData::from_zip_path(&vanilla.path).unwrap());
    let biomes: Vec<_> = data.biomes.keys().map(String::as_str).collect();
    let templates = Arc::new(StructureTemplates::from_zip_path(&vanilla.path).unwrap());
    let generator = overworld_generator(42, Arc::clone(&data), templates, true, &biomes).unwrap();
    let pos = ChunkPos { x: 0, z: 0 };
    let mut world = World::open_with_generator(&folder, &biomes, generator).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let expected = loop {
        world.update([pos]).unwrap();
        if let Some(ChunkState::Loaded(chunk)) = world.chunk(pos) {
            break chunk.to_nbt(&biomes);
        }
        assert!(Instant::now() < deadline, "chunk generation timed out");
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(
        expected.get("Status"),
        Some(&Nbt::from("minecraft:carvers"))
    );
    world.flush().unwrap();
    drop(world);
    let mut world = World::open(&folder, &biomes).unwrap();
    let actual = loop {
        world.update([pos]).unwrap();
        if let Some(ChunkState::Loaded(chunk)) = world.chunk(pos) {
            break chunk.to_nbt(&biomes);
        }
        assert!(Instant::now() < deadline, "saved chunk did not reload");
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(actual, expected);
    drop(world);
    std::fs::remove_dir_all(folder).unwrap();
}

/// Uses the actual worker entry point, then checks its terrain and saved structure records against vanilla.
#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn workers_apply_structure_terrain_and_persist_starts_and_references() {
    use harpas_core::nbt::Compound;
    use harpas_worldgen::{NoiseBasedChunkGenerator, StructureState};
    use reference::Reference;
    use std::collections::{BTreeMap, BTreeSet};

    let reference = Reference::required();
    let mut data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    // Terrain adaptation is compared here; mine block placement has its own harness.
    data.structure_sets.shift_remove("minecraft:mineshafts");
    let data = Arc::new(data);
    let templates = Arc::new(StructureTemplates::from_zip_path(&reference.server_jar).unwrap());
    let seed = 0;
    let terrain = NoiseBasedChunkGenerator::overworld(seed, &data).unwrap();
    let state = StructureState::new(seed, &data, &terrain).unwrap();
    let mut examples = BTreeMap::new();
    'search: for radius in 0_i32..256 {
        for x in -radius..=radius {
            for z in -radius..=radius {
                if x.abs().max(z.abs()) != radius {
                    continue;
                }
                for start in state
                    .starts_in_chunk(&terrain, &data, &templates, x, z)
                    .unwrap()
                {
                    let mode = &data.structures[&start.structure].terrain_adaptation;
                    if mode != "none" {
                        examples.entry(mode.clone()).or_insert(start);
                    }
                }
                if examples.len() == 4 {
                    break 'search;
                }
            }
        }
    }
    assert_eq!(examples.len(), 4, "must exercise all four adapting modes");
    let mut positions = BTreeSet::new();
    for start in examples.values() {
        positions.insert(start.chunk);
        let bounds = start.pieces[0].bounding_box;
        let (x, z) = (bounds.max[0] >> 4, bounds.max[2] >> 4);
        positions.extend([(x, z), (x + 1, z)]);
    }
    reference.compile(&[
        (
            "WorldgenReferenceProbe.java",
            include_str!("../../harpas-worldgen/tests/WorldgenReferenceProbe.java"),
        ),
        (
            "ChunkParityProbe.java",
            include_str!("../../harpas-worldgen/tests/ChunkParityProbe.java"),
        ),
        (
            "StructurePlacementProbe.java",
            include_str!("../../harpas-worldgen/tests/StructurePlacementProbe.java"),
        ),
        (
            "StructureTerrainProbe.java",
            include_str!("../../harpas-worldgen/tests/StructureTerrainProbe.java"),
        ),
    ]);
    let output = reference.temp.0.join("worker-chunks.bin");
    let mut args = vec![output.display().to_string(), seed.to_string()];
    args.extend(positions.iter().map(|(x, z)| format!("{x},{z}")));
    args.push("--without-mineshafts".to_owned());
    reference.java("StructureTerrainProbe", args);
    let bytes = std::fs::read(output.with_extension("bin.nbt")).unwrap();
    let (_, Nbt::Compound(expected_structures)) = Nbt::read_named(&mut bytes.as_slice()).unwrap()
    else {
        panic!("structures NBT");
    };
    let mut input = ChunkInput(std::io::BufReader::new(
        std::fs::File::open(output).unwrap(),
    ));
    let biome_names: Vec<_> = (0..input.int()).map(|_| input.string()).collect();
    let biomes: Vec<_> = data.biomes.keys().map(String::as_str).collect();
    let generator = overworld_generator(
        seed,
        Arc::clone(&data),
        Arc::clone(&templates),
        true,
        &biomes,
    )
    .unwrap();
    let folder = reference.temp.0.join("world");
    let mut world = World::open_with_generator(&folder, &biomes, generator).unwrap();
    let wanted: Vec<_> = positions.iter().map(|&(x, z)| ChunkPos { x, z }).collect();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        world.update(wanted.iter().copied()).unwrap();
        if wanted
            .iter()
            .all(|pos| matches!(world.chunk(*pos), Some(ChunkState::Loaded(_))))
        {
            break;
        }
        for pos in &wanted {
            if let Some(ChunkState::Failed(error)) = world.chunk(*pos) {
                panic!("chunk {pos:?}: {error}");
            }
        }
        assert!(Instant::now() < deadline, "structure generation timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
    let mut snapshots = Vec::new();
    for pos in &wanted {
        let Some(ChunkState::Loaded(chunk)) = world.chunk(*pos) else {
            unreachable!()
        };
        input.compare(chunk, &biomes, &biome_names);
        let Some(Nbt::Compound(mut structures)) = chunk.rest.get("structures").cloned() else {
            panic!("structures");
        };
        let Some(Nbt::Compound(mut references)) = structures.remove("References") else {
            panic!("References");
        };
        let names: Vec<_> = references.iter().map(|(key, _)| key.to_string()).collect();
        for name in names {
            let Some(Nbt::LongArray(mut sources)) = references.remove(&name) else {
                panic!("reference array");
            };
            sources.sort_unstable();
            references.insert(name, Nbt::LongArray(sources));
        }
        structures.insert("References", references);
        assert_eq!(
            expected_structures.get(&format!("{},{}", pos.x, pos.z)),
            Some(&Nbt::Compound(structures)),
            "structures in {pos:?}"
        );
        snapshots.push(chunk.to_nbt(&biomes));
    }
    use std::io::Read;
    assert_eq!(input.0.read(&mut [0]).unwrap(), 0);
    world.flush().unwrap();
    drop(world);
    let mut world = World::open(&folder, &biomes).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        world.update(wanted.iter().copied()).unwrap();
        if wanted
            .iter()
            .all(|pos| matches!(world.chunk(*pos), Some(ChunkState::Loaded(_))))
        {
            break;
        }
        assert!(Instant::now() < deadline, "reload timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
    for (pos, expected) in wanted.iter().zip(&snapshots) {
        let Some(ChunkState::Loaded(chunk)) = world.chunk(*pos) else {
            unreachable!()
        };
        assert_eq!(&chunk.to_nbt(&biomes), expected, "reloaded {pos:?}");
    }

    // Existing empty structure records are authoritative, even where this seed would create a start.
    let saved_empty = overworld_generator(
        seed,
        Arc::clone(&data),
        Arc::clone(&templates),
        true,
        &biomes,
    )
    .unwrap();
    let mut empty = Compound::new();
    empty.insert("starts", Compound::new());
    empty.insert("References", Compound::new());
    let mut saved_neighbor = Compound::new();
    saved_neighbor.insert("Status", "minecraft:carvers");
    saved_neighbor.insert("structures", empty.clone());
    let disabled = overworld_generator(
        seed,
        Arc::clone(&data),
        Arc::clone(&templates),
        false,
        &biomes,
    )
    .unwrap();
    let pos = wanted[0];
    let without = disabled(pos, &|_| panic!("disabled structures must not read starts")).unwrap();
    assert_eq!(without.rest.get("structures"), Some(&Nbt::Compound(empty)));
    let from_saved = saved_empty(pos, &|_| Ok(Some(saved_neighbor.clone()))).unwrap();
    assert_eq!(from_saved.to_nbt(&biomes), without.to_nbt(&biomes));
    assert!(
        wanted.iter().zip(&snapshots).any(|(pos, snapshot)| {
            let chunk = disabled(*pos, &|_| {
                panic!("disabled structures must not read starts")
            })
            .unwrap();
            chunk.to_nbt(&biomes).get("sections") != snapshot.get("sections")
        }),
        "structure adaptation must change actual terrain"
    );

    // A fresh cache must load a saved start, including its claimed locator-reference count.
    let start = examples.values().next().unwrap();
    let mut tag = start.to_nbt();
    tag.insert("references", 13);
    let loaded = harpas_worldgen::StructureStart::from_nbt(&tag, &data, &templates)
        .unwrap()
        .unwrap();
    let source = ChunkPos {
        x: start.chunk.0,
        z: start.chunk.1,
    };
    let mut starts = Compound::new();
    starts.insert(start.structure.clone(), tag);
    let mut structures = Compound::new();
    structures.insert("starts", starts);
    let mut saved_start = Compound::new();
    saved_start.insert("Status", "minecraft:carvers");
    saved_start.insert("structures", structures);
    let restored = overworld_generator(seed, Arc::clone(&data), templates, true, &biomes).unwrap();
    let actual = restored(source, &|pos| {
        Ok(Some(if pos == source {
            saved_start.clone()
        } else {
            saved_neighbor.clone()
        }))
    })
    .unwrap();
    let beardifier =
        harpas_worldgen::Beardifier::for_starts(source.x, source.z, &data, [&loaded]).unwrap();
    let expected = harpas_engine::worldgen::into_anvil(
        terrain
            .generate_chunk_carved_with_beardifier(source.x, source.z, &data, beardifier)
            .unwrap(),
    )
    .unwrap();
    assert!(
        actual.to_nbt(&biomes).get("sections") == expected.to_nbt(&biomes).get("sections"),
        "saved start terrain differs"
    );
    let Some(Nbt::Compound(structures)) = actual.rest.get("structures") else {
        panic!("structures");
    };
    let Some(Nbt::Compound(starts)) = structures.get("starts") else {
        panic!("starts");
    };
    assert_eq!(
        starts.get(&start.structure),
        Some(&Nbt::Compound(loaded.to_nbt()))
    );
    assert_eq!(loaded.references, 13);
}

struct ChunkInput(std::io::BufReader<std::fs::File>);

impl ChunkInput {
    fn bytes<const N: usize>(&mut self) -> [u8; N] {
        use std::io::Read;
        let mut bytes = [0; N];
        self.0.read_exact(&mut bytes).unwrap();
        bytes
    }
    fn short(&mut self) -> usize {
        u16::from_be_bytes(self.bytes()) as usize
    }
    fn int(&mut self) -> usize {
        u32::from_be_bytes(self.bytes()) as usize
    }
    fn longs(&mut self) -> Vec<i64> {
        (0..self.int())
            .map(|_| i64::from_be_bytes(self.bytes()))
            .collect()
    }
    fn string(&mut self) -> String {
        use std::io::Read;
        let mut bytes = vec![0; self.short()];
        self.0.read_exact(&mut bytes).unwrap();
        String::from_utf8(bytes).unwrap()
    }
    fn compare(&mut self, chunk: &harpas_anvil::Chunk, biomes: &[&str], vanilla_biomes: &[String]) {
        use harpas_core::nbt::List;
        use harpas_worldgen::HeightmapType;
        for y in -64..320 {
            for z in 0..16 {
                for x in 0..16 {
                    let section = chunk.sections[((y + 64) / 16) as usize]
                        .section
                        .as_ref()
                        .unwrap();
                    assert_eq!(
                        self.short() as u32,
                        section.block_state(x, (y & 15) as usize, z),
                        "block ({x},{y},{z})"
                    );
                }
            }
        }
        for qy in 0..96 {
            for qz in 0..4 {
                for qx in 0..4 {
                    let section = chunk.sections[qy / 4].section.as_ref().unwrap();
                    assert_eq!(
                        vanilla_biomes[self.short()],
                        biomes[section.biome(qx, qy % 4, qz) as usize]
                    );
                }
            }
        }
        assert_eq!(
            chunk.rest.get("carving_mask"),
            Some(&Nbt::LongArray(self.longs()))
        );
        let mut post = vec![Vec::new(); 24];
        for _ in 0..self.int() {
            let section = self.bytes::<1>()[0] as usize;
            post[section].push(self.short() as i16);
        }
        let post = List::List(
            post.into_iter()
                .map(|entries| {
                    if entries.is_empty() {
                        List::Empty
                    } else {
                        List::Short(entries)
                    }
                })
                .collect(),
        );
        assert_eq!(chunk.rest.get("PostProcessing"), Some(&Nbt::List(post)));
        let Some(Nbt::Compound(maps)) = chunk.rest.get("Heightmaps") else {
            panic!("heightmaps");
        };
        for kind in HeightmapType::ALL {
            let values = self.longs();
            if let Some(map) = maps.get(kind.nbt_key()) {
                assert_eq!(map, &Nbt::LongArray(values), "{}", kind.nbt_key());
            }
        }
    }
}
