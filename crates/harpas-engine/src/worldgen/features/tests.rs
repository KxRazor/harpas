use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use harpas_core::block::block_states;
use harpas_worldgen::carver::CarvingMask;
use harpas_worldgen::{
    BiomeDef, BlockStateDef, ConfiguredFeatureDef, FeatureConfigDef, FeatureRefDef,
    HeightProviderDef, IntProviderDef, OreFeatureDef, OreTargetDef, PlacedFeatureDef,
    PlacementModifierDef, RuleTestDef, TemperatureModifier, VerticalAnchor,
};

use super::*;
use crate::world::ChunkReader;
use crate::worldgen::into_anvil_with_mask;

const SEED: i64 = 42;
const BIOMES: &[&str] = &["minecraft:plains"];

struct Folder(PathBuf);

impl Folder {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "harpas-features-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn storage(&self) -> ChunkStorage {
        ChunkStorage::new(
            self.0.clone(),
            BIOMES.iter().map(|name| (*name).to_owned()).collect(),
        )
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn stone_chunk(pos: ChunkPos) -> GeneratedChunk {
    let mut chunk = GeneratedChunk::new(pos.x, pos.z, -64, 384, 1);
    let stone = block_states().default_state("minecraft:stone").unwrap();
    for y in -64..64 {
        for z in 0..16 {
            for x in 0..16 {
                chunk.set_block_state(x, y, z, stone);
            }
        }
    }
    chunk
}

/// Iron veins that only replace stone, so they place the same blocks in any order.
fn feature_index() -> FeatureIndex {
    let ore = ConfiguredFeatureDef {
        feature_type: "minecraft:ore".to_string(),
        config: FeatureConfigDef::Ore(OreFeatureDef {
            size: 12,
            discard_chance_on_air_exposure: 0.0,
            targets: vec![OreTargetDef {
                target: RuleTestDef::BlockMatch("minecraft:stone".to_string()),
                state: BlockStateDef::simple("minecraft:iron_ore"),
            }],
        }),
    };
    let placed = PlacedFeatureDef {
        feature: FeatureRefDef::Reference("example:ore".to_string()),
        placement: vec![
            PlacementModifierDef::Count(IntProviderDef::Constant(24)),
            PlacementModifierDef::InSquare,
            PlacementModifierDef::HeightRange(HeightProviderDef::Uniform {
                min_inclusive: VerticalAnchor::Absolute(0),
                max_inclusive: VerticalAnchor::Absolute(63),
            }),
            PlacementModifierDef::Biome,
        ],
    };
    let mut features = vec![Vec::new(); harpas_worldgen::STEP_COUNT];
    features[6] = vec!["example:ore_placed".to_string()];
    let mut data = WorldgenData::default();
    data.configured_features
        .insert("example:ore".to_string(), ore);
    data.placed_features
        .insert("example:ore_placed".to_string(), placed);
    data.biomes.insert(
        "minecraft:plains".to_string(),
        BiomeDef {
            has_precipitation: true,
            temperature: 0.8,
            temperature_modifier: TemperatureModifier::None,
            downfall: 0.4,
            carvers: Vec::new(),
            features,
        },
    );
    FeatureIndex::new(&data, &[0]).unwrap()
}

/// Counts how many times each chunk is generated through carvers.
type Counts = Arc<Mutex<HashMap<ChunkPos, usize>>>;

fn decorator(capacity: usize) -> (Decorator, Counts) {
    let counts = Counts::default();
    let generated = Arc::clone(&counts);
    let generator: Arc<ChunkGenerator> = Arc::new(move |pos: ChunkPos, _: &ChunkReader<'_>| {
        *generated.lock().unwrap().entry(pos).or_default() += 1;
        into_anvil_with_mask(stone_chunk(pos), CarvingMask::new(-64, 384).to_raw_data())
    });
    (
        Decorator::new(generator, SEED, feature_index(), 1, capacity),
        counts,
    )
}

/// The blocks of every chunk in `area` after decorating every chunk of `targets` directly.
fn expected(area: &[ChunkPos], targets: &[ChunkPos]) -> HashMap<ChunkPos, GeneratedChunk> {
    let index = feature_index();
    let chunks = area.iter().map(|&pos| stone_chunk(pos));
    let mut world = RegionFeatureWorld::with_stored_biomes(chunks, SEED).unwrap();
    for target in targets {
        apply_biome_decoration(&mut world, &index, SEED, target.x, target.z).unwrap();
    }
    world
        .into_chunks()
        .map(|chunk| {
            (
                ChunkPos {
                    x: chunk.chunk_x(),
                    z: chunk.chunk_z(),
                },
                chunk,
            )
        })
        .collect()
}

fn square(center: ChunkPos, radius: i32) -> Vec<ChunkPos> {
    let mut positions = Vec::new();
    for x in -radius..=radius {
        for z in -radius..=radius {
            positions.push(ChunkPos {
                x: center.x + x,
                z: center.z + z,
            });
        }
    }
    positions
}

fn assert_blocks(chunk: &Chunk, expected: &GeneratedChunk) {
    let iron = block_states().default_state("minecraft:iron_ore").unwrap();
    let mut ores = 0;
    for section in &chunk.sections {
        let Some(blocks) = &section.section else {
            continue;
        };
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    let block_y = i32::from(section.y) * 16 + y as i32;
                    let state = blocks.block_state(x, y, z);
                    assert_eq!(
                        state,
                        expected.block_state(x, block_y, z),
                        "block {x}, {block_y}, {z} of chunk {}, {}",
                        expected.chunk_x(),
                        expected.chunk_z()
                    );
                    ores += usize::from(state == iron);
                }
            }
        }
    }
    assert!(ores > 0, "no ore was placed");
}

fn saved_status(storage: &ChunkStorage, pos: ChunkPos) -> Option<String> {
    storage
        .read(pos)
        .unwrap()
        .and_then(|nbt| status(&nbt).map(str::to_owned))
}

#[test]
fn a_final_chunk_holds_the_features_of_its_3x3_chunks() {
    let folder = Folder::new();
    let storage = folder.storage();
    let (decorator, counts) = decorator(CAPACITY);
    let pos = ChunkPos { x: 0, z: 0 };
    let generated = decorator.complete(pos, &storage).unwrap();
    assert!(generated.changed);
    assert_eq!(status(&generated.chunk.rest), Some(FEATURES));
    let blocks = expected(&square(pos, 2), &square(pos, 1));
    assert_blocks(&generated.chunk, &blocks[&pos]);
    let counts = counts.lock().unwrap();
    assert_eq!(counts.len(), 25, "the 5x5 chunks around it are generated");
    assert!(counts.values().all(|&count| count == 1));
    // The chunk left the store; its 24 neighbors wait for their own neighbors.
    assert_eq!(decorator.held(), 24);
}

#[test]
fn concurrent_workers_decorate_each_chunk_once() {
    let folder = Folder::new();
    let storage = folder.storage();
    let (decorator, counts) = decorator(CAPACITY);
    let wanted = square(ChunkPos { x: 3, z: -2 }, 2);
    let results = Mutex::new(HashMap::new());
    thread::scope(|scope| {
        for worker in 0..4 {
            let (decorator, storage, wanted, results) = (&decorator, &storage, &wanted, &results);
            scope.spawn(move || {
                for &pos in wanted.iter().skip(worker).step_by(4) {
                    let generated = decorator.complete(pos, storage).unwrap();
                    results.lock().unwrap().insert(pos, generated.chunk);
                }
            });
        }
    });
    let decorated: BTreeSet<_> = wanted
        .iter()
        .flat_map(|&pos| square(pos, 1))
        .map(|pos| (pos.x, pos.z))
        .collect();
    let decorated: Vec<_> = decorated
        .into_iter()
        .map(|(x, z)| ChunkPos { x, z })
        .collect();
    let blocks = expected(&square(ChunkPos { x: 3, z: -2 }, 4), &decorated);
    let results = results.into_inner().unwrap();
    for pos in &wanted {
        assert_blocks(&results[pos], &blocks[pos]);
    }
    assert!(counts.lock().unwrap().values().all(|&count| count == 1));
}

#[test]
fn proto_chunks_saved_when_memory_runs_short_resume_unchanged() {
    let folder = Folder::new();
    let storage = folder.storage();
    let (decorator, counts) = decorator(8);
    let wanted = [
        ChunkPos { x: 0, z: 0 },
        ChunkPos { x: 1, z: 0 },
        ChunkPos { x: 2, z: 1 },
    ];
    let chunks: Vec<_> = wanted
        .iter()
        .map(|&pos| decorator.complete(pos, &storage).unwrap().chunk)
        .collect();
    assert!(decorator.held() <= 8 + 25);
    let decorated: Vec<_> = {
        let set: BTreeSet<_> = wanted
            .iter()
            .flat_map(|&pos| square(pos, 1))
            .map(|pos| (pos.x, pos.z))
            .collect();
        set.into_iter().map(|(x, z)| ChunkPos { x, z }).collect()
    };
    let blocks = expected(&square(ChunkPos { x: 1, z: 0 }, 4), &decorated);
    for (pos, chunk) in wanted.iter().zip(&chunks) {
        assert_blocks(chunk, &blocks[pos]);
    }
    assert!(
        counts.lock().unwrap().values().all(|&count| count == 1),
        "saved proto-chunks are read back instead of generated again"
    );
}

#[test]
fn a_new_session_resumes_saved_proto_chunks_and_final_chunks() {
    let folder = Folder::new();
    let storage = folder.storage();
    let first = ChunkPos { x: 0, z: 0 };
    let next = ChunkPos { x: 1, z: 0 };
    {
        let (decorator, _) = decorator(CAPACITY);
        let generated = decorator.complete(first, &storage).unwrap();
        // The logic thread saves the chunks it receives.
        storage.write(&[(first, &generated.chunk)]).unwrap();
        decorator.flush(&storage).unwrap();
        assert_eq!(decorator.held(), 0);
    }
    for pos in square(first, 1) {
        assert_eq!(saved_status(&storage, pos).as_deref(), Some(FEATURES));
    }
    assert_eq!(
        saved_status(&storage, ChunkPos { x: 2, z: 2 }).as_deref(),
        Some(CARVERS)
    );

    let (decorator, counts) = decorator(CAPACITY);
    let again = decorator.complete(first, &storage).unwrap();
    assert!(!again.changed, "a final chunk is returned as saved");
    assert!(counts.lock().unwrap().is_empty());
    assert_eq!(decorator.held(), 0, "final chunks are not held");

    let generated = decorator.complete(next, &storage).unwrap();
    let mut decorated = square(first, 1);
    decorated.extend(square(next, 1).into_iter().filter(|pos| pos.x == 2));
    let blocks = expected(&square(first, 3), &decorated);
    assert_blocks(&generated.chunk, &blocks[&next]);
    let counts = counts.lock().unwrap();
    assert_eq!(
        counts.len(),
        5,
        "only the column of chunks that was never generated is generated"
    );
    assert!(counts.keys().all(|pos| pos.x == 3));
}

#[test]
fn a_failed_generation_releases_its_chunk() {
    let folder = Folder::new();
    let storage = folder.storage();
    let failed = Arc::new(AtomicBool::new(false));
    let remaining = Arc::clone(&failed);
    let generator: Arc<ChunkGenerator> = Arc::new(move |pos: ChunkPos, _: &ChunkReader<'_>| {
        if !remaining.swap(true, Ordering::Relaxed) {
            return Err(io::Error::other("generation failed"));
        }
        into_anvil_with_mask(stone_chunk(pos), CarvingMask::new(-64, 384).to_raw_data())
    });
    let decorator = Decorator::new(generator, SEED, feature_index(), 1, CAPACITY);
    let pos = ChunkPos { x: 0, z: 0 };
    assert!(decorator.complete(pos, &storage).is_err());
    let generated = decorator.complete(pos, &storage).unwrap();
    let blocks = expected(&square(pos, 2), &square(pos, 1));
    assert_blocks(&generated.chunk, &blocks[&pos]);
}
