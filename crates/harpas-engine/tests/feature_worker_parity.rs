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
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_engine::world::{ChunkPos, ChunkState, World};
use harpas_engine::worldgen::{decorated, overworld_generator};
use harpas_worldgen::{FeatureIndex, NoiseBasedChunkGenerator, StructureTemplates, WorldgenData};
use reference::Reference;

const SEEDS: [i64; 2] = [42, -4_172_144_997_902_289_642];
/// Far apart, so no two share a decorated chunk; after the first three come a bamboo jungle, an old growth pine
/// taiga with forest rocks, a desert well crossing a chunk edge, a warm ocean reef and an underwater magma
/// deposit on a chunk edge in the last seed, followed by snowy plains, a frozen river and two dripstone caves,
/// including clusters with long pointed columns, a lush cave with dripleaves, clay pools and a monster room
/// crossing a chunk edge, then a geode placed by the chunk to its west.
const CHUNKS: [(i32, i32); 14] = [
    (0, 0),
    (41, -26),
    (-203, -151),
    (204, -236),
    (-400, 368),
    (-348, -421),
    (-600, 552),
    (-120, -18),
    (-198, -198),
    (-200, -175),
    (-600, -348),
    (-600, -240),
    (-528, -480),
    (-6, -2),
];

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

fn vanilla(reference: &Reference, seed: i64, supported: &Path) -> Vec<(Blocks, Vec<Compound>)> {
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
            let mut entities = Vec::new();
            while let Some(line) = lines.next_if(|line| !line.starts_with("chunk")) {
                if let Some(hex) = line.strip_prefix("entity ") {
                    let bytes: Vec<_> = hex
                        .as_bytes()
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|pair| {
                            u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap()
                        })
                        .collect();
                    let Nbt::Compound(entity) = Nbt::read_network(&mut bytes.as_slice()).unwrap()
                    else {
                        panic!("expected block entity compound")
                    };
                    entities.push(entity);
                    continue;
                }
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
            entities.sort_by_key(|e| format!("{e:?}"));
            (blocks, entities)
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

fn feature_entities(chunk: &Chunk, rooms_only: bool) -> Vec<Compound> {
    let Some(Nbt::List(List::Compound(entities))) = chunk.rest.get("block_entities") else {
        return Vec::new();
    };
    let mut saved: Vec<_> = entities
        .iter()
        .filter(|entity| {
            entity.get("id") == Some(&Nbt::from("minecraft:mob_spawner"))
                || entity.get("LootTable") == Some(&Nbt::from("minecraft:chests/simple_dungeon"))
                || !rooms_only
                    && entity.get("LootTable")
                        == Some(&Nbt::from("minecraft:archaeology/desert_well"))
        })
        .cloned()
        .collect();
    saved.sort_by_key(|entity| format!("{entity:?}"));
    saved
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
    let mut lush = [false; 3];
    for &seed in reference::sampled_seeds(&SEEDS) {
        let expected = vanilla(&reference, seed, &supported_list);
        let folder = reference.temp.0.join(format!("world-{seed}"));
        fs::create_dir_all(&folder).unwrap();
        let terrain = NoiseBasedChunkGenerator::overworld(seed, &data).unwrap();
        let mut world = open(&folder, seed, &data, &templates);
        let mut finals = Vec::new();
        for (&(x, z), (expected, expected_entities)) in CHUNKS.iter().zip(&expected) {
            let pos = ChunkPos { x, z };
            let chunk = load(&mut world, pos);
            assert_eq!(status(&chunk), Some("minecraft:features"));
            let carved = terrain.generate_chunk_carved(x, z, &data).unwrap();
            let actual = changed(&chunk, &carved);
            if seed == SEEDS[1] {
                for &(_, _, _, state) in expected {
                    if let Some((name, properties)) = block_states().properties(state) {
                        lush[0] |= name == "minecraft:small_dripleaf";
                        lush[1] |= name == "minecraft:big_dripleaf";
                        lush[2] |= name.contains("dripleaf")
                            && properties.contains(&("waterlogged", "true"));
                    }
                }
            }
            if seed == SEEDS[1] && (x, z) == (-600, 552) {
                let corals = data.block_tag_states("minecraft:coral_blocks").unwrap();
                assert!(
                    expected
                        .iter()
                        .any(|&(_, _, _, state)| corals[state as usize]),
                    "the warm ocean worker sample must generate and save coral blocks"
                );
            }
            if seed == SEEDS[1] && (x, z) == (-600, -240) {
                assert!(
                    expected.iter().any(|&(_, _, _, state)| {
                        block_states()
                            .properties(state)
                            .is_some_and(|(name, properties)| {
                                name == "minecraft:pointed_dripstone"
                                    && properties.iter().any(|&(key, value)| {
                                        key == "thickness"
                                            && matches!(value, "base" | "middle" | "tip_merge")
                                    })
                            })
                    }),
                    "the dripstone cave sample must place cluster columns longer than two blocks or merged tips"
                );
            }
            let required = match (x, z) {
                (204, -236) => Some("minecraft:bamboo"),
                (-400, 368) => Some("minecraft:mossy_cobblestone"),
                (-348, -421) => Some("minecraft:suspicious_sand"),
                (-120, -18) => Some("minecraft:magma_block"),
                (-198, -198) => Some("minecraft:snow"),
                (-200, -175) => Some("minecraft:ice"),
                (-600, -348) => Some("minecraft:pointed_dripstone"),
                (-600, -240) => Some("minecraft:dripstone_block"),
                (-528, -480) => Some("minecraft:spawner"),
                (-6, -2) => Some("minecraft:budding_amethyst"),
                _ => None,
            };
            if seed == SEEDS[1]
                && let Some(block) = required
            {
                assert!(
                    expected.iter().any(|&(_, _, _, state)| {
                        block_states()
                            .properties(state)
                            .is_some_and(|(name, _)| name == block)
                    }),
                    "the worker sample {x}, {z} must generate and save {block}"
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
            assert_eq!(
                &feature_entities(&chunk, true),
                expected_entities,
                "{x}, {z}: monster room block entities"
            );
            let entities = feature_entities(&chunk, false);
            if seed == SEEDS[1] && (x, z) == (-348, -421) {
                assert!(
                    !entities.is_empty(),
                    "the worker must save the well's archaeology data"
                );
                for entity in &entities {
                    assert_eq!(
                        entity.get("id"),
                        Some(&Nbt::from("minecraft:brushable_block"))
                    );
                    assert!(matches!(entity.get("LootTableSeed"), Some(Nbt::Long(_))));
                }
            }
            finals.push((pos, actual, carved, entities));
            compared += 1;
        }
        world.flush().unwrap();
        drop(world);

        let mut world = open(&folder, seed, &data, &templates);
        for (pos, actual, carved, entities) in &finals {
            let chunk = load(&mut world, *pos);
            assert_eq!(&changed(&chunk, carved), actual, "{pos:?} after reopening");
            assert_eq!(
                &feature_entities(&chunk, false),
                entities,
                "{pos:?} feature block entities after reopening"
            );
        }
        // Reopened final chunks are read as saved, so nothing waits to be saved.
        world.flush().unwrap();
        drop(world);
        fs::remove_dir_all(&folder).unwrap();
    }
    assert!(
        lush.into_iter().all(|placed| placed),
        "the worker samples must place and save both dripleaves and waterlog them"
    );
    eprintln!("Compared {compared} final chunks decorated on the chunk workers");
}
