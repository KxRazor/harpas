//! Compares the blocks of every placed feature Harpas places with vanilla's own `applyBiomeDecoration`.
mod reference;

use std::collections::BTreeSet;
use std::fs;

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
/// disks and ice patches are placed.
const CHUNKS: [(i32, i32); 24] = [
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
fn feature_blocks_match_vanilla() {
    let Some(reference) = Reference::from_env() else {
        eprintln!("Skipping feature blocks: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
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
    eprintln!(
        "Compared {placed} blocks around {compared} decorated chunks, from {} placed features",
        supported.len()
    );
}
