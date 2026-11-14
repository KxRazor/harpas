//! Compares the blocks of every placed feature Harpas places with vanilla's own `applyBiomeDecoration`.
mod reference;

use std::collections::BTreeSet;
use std::fs;

use harpas_core::block::block_states;
use harpas_worldgen::{
    FeatureIndex, FeatureWorld, NoiseBasedChunkGenerator, RegionFeatureWorld, WorldgenData,
    apply_biome_decoration,
};
use reference::Reference;

/// Includes seed 0, which seeds `Xoroshiro128++` specially, and a seed that fills all 64 bits.
const SEEDS: [i64; 3] = [0, 42, -4_172_144_997_902_289_642];
/// The chunks of `placement_parity`, mixed chunks on both sides of the origin and then, in the last seed, a
/// desert, swamp, mangrove swamp, badlands, cherry grove, plains, snowy plains, birch forest, jungle, dark
/// forest, savanna and pale garden; then a beach, frozen river, ice spikes, ocean, river and warm ocean, where
/// disks and ice patches are placed; then a grove of the last seed and a river of seed 0, where trees grow, and a
/// meadow of the last seed, where trees grow with bee nests, a mangrove swamp of the last seed shallow enough
/// for mangroves, two chunks of the last seed over lush caves, where rooted azaleas grow, and a warm ocean of the
/// last seed with sea pickles, two frozen oceans of the last seed with packed ice icebergs, snow on them and blue
/// ice, two chunks of the last seed with an underground and a surface lava lake, a bamboo jungle and an old
/// growth pine taiga with forest rocks, a desert well crossing a chunk edge, a warm ocean reef and an
/// underwater magma deposit on a chunk edge. Snowy plains and the frozen river also exercise surface snow
/// and ice in the last seed, followed by two dripstone caves with small and large formations and clusters.
/// The lush caves also exercise clay pools and dripleaves; (-528,-480) has a monster room crossing a chunk edge.
const CHUNKS: [(i32, i32); 42] = [
    (0, 0),
    (-3, 7),
    (41, -26),
    (128, 64),
    (-203, -151),
    (600, 312),
    (-66, 30),
    (-36, -36),
    (126, 72),
    (162, -108),
    (-156, 12),
    (-42, 6),
    (-198, -198),
    (-108, -108),
    (84, 42),
    (-102, -114),
    (-42, -12),
    (-168, -102),
    (-40, 10),
    (-200, -175),
    (380, -170),
    (-25, -35),
    (-35, 5),
    (0, 30),
    (-528, -480),
    (-346, 387),
    (-428, -423),
    (116, 70),
    (-86, 16),
    (42, -116),
    (12, 40),
    (315, 333),
    (294, 336),
    (-119, -11),
    (-393, 14),
    (204, -236),
    (-400, 368),
    (-348, -421),
    (-600, 552),
    (-120, -18),
    (-600, -348),
    (-600, -240),
];

fn changed_blocks(
    data: &WorldgenData,
    generator: &NoiseBasedChunkGenerator,
    index: &FeatureIndex,
    seed: i64,
    (cx, cz): (i32, i32),
) -> BTreeSet<(i32, i32, i32, u32)> {
    let chunks = (-1..=1).flat_map(|dx| (-1..=1).map(move |dz| (cx + dx, cz + dz)));
    let chunks: Vec<_> = chunks
        .map(|(x, z)| generator.generate_chunk_carved(x, z, data).unwrap())
        .collect();
    let before = RegionFeatureWorld::with_stored_biomes(chunks.clone(), seed).expect("region");
    let mut world = RegionFeatureWorld::with_stored_biomes(chunks, seed).expect("region");
    apply_biome_decoration(&mut world, index, seed, cx, cz).expect("apply features");
    let mut changed = BTreeSet::new();
    for y in -64..320 {
        for z in (cz - 1) * 16..(cz + 2) * 16 {
            for x in (cx - 1) * 16..(cx + 2) * 16 {
                let state = world.block_state(x, y, z);
                if state != before.block_state(x, y, z) {
                    changed.insert((x, y, z, state));
                }
            }
        }
    }
    changed
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn feature_blocks_match_vanilla() {
    let reference = Reference::required();
    let data =
        WorldgenData::from_zip_path(&reference.server_jar).expect("load vanilla worldgen data");
    reference.compile(&[
        (
            "WorldgenReferenceProbe.java",
            include_str!("WorldgenReferenceProbe.java"),
        ),
        (
            "ChunkParityProbe.java",
            include_str!("ChunkParityProbe.java"),
        ),
        ("PlacementProbe.java", include_str!("PlacementProbe.java")),
        (
            "FeatureBlocksProbe.java",
            include_str!("FeatureBlocksProbe.java"),
        ),
    ]);
    let index = FeatureIndex::overworld(&data).expect("compile the feature index");
    let supported: Vec<_> = data
        .placed_features
        .keys()
        .enumerate()
        .filter(|&(placed, _)| index.compiled(placed).is_some())
        .map(|(_, name)| name.as_str())
        .collect();
    let list = reference.temp.0.join("supported.txt");
    fs::write(&list, supported.join("\n")).unwrap();

    let mut compared = 0;
    let mut placed = 0;
    let mut lush = [false; 3];
    for &seed in reference::sampled_seeds(&SEEDS) {
        let output = reference.temp.0.join("feature-blocks.txt");
        let mut args = vec![
            output.display().to_string(),
            reference.server_jar.display().to_string(),
            seed.to_string(),
            list.display().to_string(),
        ];
        args.extend(CHUNKS.iter().map(|(x, z)| format!("{x},{z}")));
        reference.java("FeatureBlocksProbe", args);
        let text = fs::read_to_string(&output).expect("read the vanilla feature output");
        let mut lines = text.lines().peekable();

        let generator =
            NoiseBasedChunkGenerator::overworld(seed, &data).expect("compile generator");
        for &chunk in &CHUNKS {
            assert_eq!(
                lines.next(),
                Some(format!("chunk {} {}", chunk.0, chunk.1).as_str())
            );
            let mut expected = BTreeSet::new();
            while let Some(line) = lines.next_if(|line| !line.starts_with("chunk")) {
                let mut fields = line.split(' ').map(|field| field.parse::<i64>().unwrap());
                let mut field = || fields.next().expect("block field");
                expected.insert((
                    field() as i32,
                    field() as i32,
                    field() as i32,
                    field() as u32,
                ));
            }
            let actual = changed_blocks(&data, &generator, &index, seed, chunk);
            if seed == SEEDS[2] {
                for &(_, _, _, state) in &expected {
                    if let Some((name, properties)) = block_states().properties(state) {
                        lush[0] |= name == "minecraft:small_dripleaf";
                        lush[1] |= name == "minecraft:big_dripleaf";
                        lush[2] |= name.contains("dripleaf")
                            && properties.contains(&("waterlogged", "true"));
                    }
                }
            }
            if seed == SEEDS[2] && chunk == (-600, 552) {
                let corals = data.block_tag_states("minecraft:coral_blocks").unwrap();
                assert!(
                    expected
                        .iter()
                        .any(|&(_, _, _, state)| corals[state as usize]),
                    "the warm ocean sample must place coral blocks"
                );
            }
            if seed == SEEDS[2] && chunk == (-600, -240) {
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
            let required = match chunk {
                (204, -236) => Some("minecraft:bamboo"),
                (-400, 368) => Some("minecraft:mossy_cobblestone"),
                (-348, -421) => Some("minecraft:suspicious_sand"),
                (-120, -18) => Some("minecraft:magma_block"),
                (-198, -198) => Some("minecraft:snow"),
                (-200, -175) => Some("minecraft:ice"),
                (-600, -348) => Some("minecraft:pointed_dripstone"),
                (-600, -240) => Some("minecraft:dripstone_block"),
                (-528, -480) => Some("minecraft:spawner"),
                _ => None,
            };
            if seed == SEEDS[2]
                && let Some(block) = required
            {
                assert!(
                    expected.iter().any(|&(_, _, _, state)| {
                        block_states()
                            .properties(state)
                            .is_some_and(|(name, _)| name == block)
                    }),
                    "the {chunk:?} sample must place {block}"
                );
            }
            let missing: Vec<_> = expected.difference(&actual).take(20).collect();
            let extra: Vec<_> = actual.difference(&expected).take(20).collect();
            assert!(
                missing.is_empty() && extra.is_empty(),
                "seed {seed} chunk {chunk:?}: {} blocks from vanilla, {} from Harpas\n\
                 missing {missing:?}\nextra {extra:?}",
                expected.len(),
                actual.len(),
            );
            compared += 1;
            placed += expected.len();
        }
        fs::remove_file(&output).unwrap();
    }
    assert!(
        lush.into_iter().all(|placed| placed),
        "the samples must place both dripleaves and waterlog them"
    );
    eprintln!(
        "Compared {placed} blocks around {compared} decorated chunks, from {} placed features",
        supported.len()
    );
}
