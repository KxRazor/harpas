//! The FEATURES stage on the chunk workers: each final chunk is compared with vanilla decorating its 3x3
//! chunks in the same order, then the world is saved and reopened, and the chunk must load unchanged.

#[allow(dead_code)]
#[path = "../../harpas-worldgen/tests/reference/mod.rs"]
mod reference;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use harpas_anvil::Chunk;
use harpas_core::block::block_states;
use harpas_core::nbt::Nbt;
use harpas_engine::world::{ChunkPos, ChunkState, World};
use harpas_engine::worldgen::{decorated, overworld_generator};
use harpas_worldgen::{FeatureIndex, NoiseBasedChunkGenerator, StructureTemplates, WorldgenData};
use reference::Reference;

const SEEDS: [i64; 2] = [42, -4_172_144_997_902_289_642];
/// Far apart, so no two share a decorated chunk; the last is a bamboo jungle of the last seed.
const CHUNKS: [(i32, i32); 4] = [(0, 0), (41, -26), (-203, -151), (204, -236)];

type Blocks = BTreeSet<(i32, i32, i32, u32)>;

fn load(world: &mut World, pos: ChunkPos) -> Arc<Chunk> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        world.update([pos]).unwrap();
        match world.chunk(pos) {
            Some(ChunkState::Loaded(chunk)) => return Arc::clone(chunk),
            Some(ChunkState::Failed(error)) => panic!("chunk {pos:?} failed: {error}"),
            _ => {}
        }
        assert!(Instant::now() < deadline, "timed out loading {pos:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn changed(chunk: &Chunk, carved: &harpas_worldgen::GeneratedChunk) -> Blocks {
    let mut blocks = BTreeSet::new();
    for section in &chunk.sections {
        let Some(states) = &section.section else {
            continue;
        };
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    let block_y = i32::from(section.y) * 16 + y as i32;
                    let state = states.block_state(x, y, z);
                    if state != carved.block_state(x, block_y, z) {
                        blocks.insert((
                            carved.chunk_x() * 16 + x as i32,
                            block_y,
                            carved.chunk_z() * 16 + z as i32,
                            state,
                        ));
                    }
                }
            }
        }
    }
    blocks
}

fn vanilla(reference: &Reference, seed: i64, supported: &Path) -> Vec<Blocks> {
    let output = reference.temp.0.join(format!("final-features-{seed}.txt"));
    let mut args = vec![
        output.display().to_string(),
        reference.server_jar.display().to_string(),
        seed.to_string(),
        supported.display().to_string(),
        "final".to_string(),
    ];
    args.extend(CHUNKS.iter().map(|(x, z)| format!("{x},{z}")));
    reference.java("FeatureBlocksProbe", args);
    let text = fs::read_to_string(&output).expect("read the vanilla feature output");
    fs::remove_file(&output).unwrap();
    let mut lines = text.lines().peekable();
    CHUNKS
        .iter()
        .map(|&(x, z)| {
            assert_eq!(lines.next(), Some(format!("chunk {x} {z}").as_str()));
            let mut blocks = BTreeSet::new();
            while let Some(line) = lines.next_if(|line| !line.starts_with("chunk")) {
                let mut fields = line.split(' ').map(|field| field.parse::<i64>().unwrap());
                let mut field = || fields.next().expect("block field");
                blocks.insert((
                    field() as i32,
                    field() as i32,
                    field() as i32,
                    field() as u32,
                ));
            }
            assert!(!blocks.is_empty(), "vanilla placed no features in {x}, {z}");
            blocks
        })
        .collect()
}

fn open(
    folder: &Path,
    seed: i64,
    data: &Arc<WorldgenData>,
    templates: &Arc<StructureTemplates>,
) -> World {
    let biomes: Vec<_> = data.biomes.keys().map(String::as_str).collect();
    let generator = overworld_generator(
        seed,
        Arc::clone(data),
        Arc::clone(templates),
        false,
        &biomes,
    )
    .unwrap();
    let source = decorated(generator, seed, data).unwrap();
    World::open_with_source(folder, &biomes, source).unwrap()
}

fn status(chunk: &Chunk) -> Option<&str> {
    match chunk.rest.get("Status") {
        Some(Nbt::String(status)) => Some(status),
        _ => None,
    }
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn final_chunks_from_the_workers_match_vanilla_and_reload_unchanged() {
    let reference = Reference::required();
    let data = Arc::new(
        WorldgenData::from_zip_path(&reference.server_jar).expect("load vanilla worldgen data"),
    );
    let templates = Arc::new(
        StructureTemplates::from_zip_path(&reference.server_jar).expect("load vanilla templates"),
    );
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
            "PlacementProbe.java",
            include_str!("../../harpas-worldgen/tests/PlacementProbe.java"),
        ),
        (
            "FeatureBlocksProbe.java",
            include_str!("../../harpas-worldgen/tests/FeatureBlocksProbe.java"),
        ),
    ]);
    // Vanilla places the features the workers place, and no-ops at the others' indices.
    let index = FeatureIndex::overworld(&data).expect("compile the feature index");
    let supported: Vec<_> = data
        .placed_features
        .keys()
        .enumerate()
        .filter(|&(placed, _)| index.compiled(placed).is_some())
        .map(|(_, name)| name.as_str())
        .collect();
    let supported_list = reference.temp.0.join("supported.txt");
    fs::write(&supported_list, supported.join("\n")).unwrap();
    let mut compared = 0;
    for &seed in reference::sampled_seeds(&SEEDS) {
        let expected = vanilla(&reference, seed, &supported_list);
        let folder = reference.temp.0.join(format!("world-{seed}"));
        fs::create_dir_all(&folder).unwrap();
        let terrain = NoiseBasedChunkGenerator::overworld(seed, &data).unwrap();
        let mut world = open(&folder, seed, &data, &templates);
        let mut finals = Vec::new();
        for (&(x, z), expected) in CHUNKS.iter().zip(&expected) {
            let pos = ChunkPos { x, z };
            let chunk = load(&mut world, pos);
            assert_eq!(status(&chunk), Some("minecraft:features"));
            let carved = terrain.generate_chunk_carved(x, z, &data).unwrap();
            let actual = changed(&chunk, &carved);
            if seed == SEEDS[1] && (x, z) == (204, -236) {
                assert!(
                    expected.iter().any(|&(_, _, _, state)| {
                        block_states()
                            .properties(state)
                            .is_some_and(|(name, _)| name == "minecraft:bamboo")
                    }),
                    "the worker sample must generate and save bamboo"
                );
            }
            let missing: Vec<_> = expected.difference(&actual).take(20).collect();
            let extra: Vec<_> = actual.difference(expected).take(20).collect();
            assert!(
                missing.is_empty() && extra.is_empty(),
                "seed {seed} chunk {x}, {z}: {} blocks from vanilla, {} from Harpas\n\
                 missing {missing:?}\nextra {extra:?}",
                expected.len(),
                actual.len(),
            );
            finals.push((pos, actual, carved));
            compared += 1;
        }
        world.flush().unwrap();
        drop(world);

        let mut world = open(&folder, seed, &data, &templates);
        for (pos, actual, carved) in &finals {
            let chunk = load(&mut world, *pos);
            assert_eq!(&changed(&chunk, carved), actual, "{pos:?} after reopening");
        }
        // Reopened final chunks are read as saved, so nothing waits to be saved.
        world.flush().unwrap();
        drop(world);
        fs::remove_dir_all(&folder).unwrap();
    }
    eprintln!("Compared {compared} final chunks decorated on the chunk workers");
}
