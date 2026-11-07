//! Compares the FEATURES stage's order and seeds with vanilla's own `applyBiomeDecoration` on real terrain.
mod reference;

use std::collections::BTreeSet;
use std::fs;

use harpas_worldgen::{
    FeatureIndex, NoiseBasedChunkGenerator, RegionFeatureWorld, WorldgenData,
    apply_biome_decoration,
};
use reference::Reference;

/// Includes seed 0, which seeds `Xoroshiro128++` specially, and a seed that fills all 64 bits.
const SEEDS: [i64; 3] = [0, 42, -4_172_144_997_902_289_642];
/// Far apart, to meet ocean, mountain, badlands and mixed-biome chunks, on both sides of the origin.
const CHUNKS: [(i32, i32); 6] = [
    (0, 0),
    (-3, 7),
    (41, -26),
    (128, 64),
    (-203, -151),
    (600, 312),
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
    let before = chunks.clone();
    let mut world = RegionFeatureWorld::with_stored_biomes(chunks, seed).expect("region");
    apply_biome_decoration(&mut world, index, seed, cx, cz).expect("apply features");
    let mut changed = BTreeSet::new();
    for chunk in &before {
        let (x, z) = (chunk.chunk_x(), chunk.chunk_z());
        let after = world.chunk(x, z).expect("region chunk");
        for y in -64..320 {
            for lz in 0..16 {
                for lx in 0..16 {
                    let state = after.block_state(lx, y, lz);
                    if state != chunk.block_state(lx, y, lz) {
                        changed.insert((x * 16 + lx as i32, y, z * 16 + lz as i32, state));
                    }
                }
            }
        }
    }
    changed
}

#[test]
fn feature_stage_matches_vanilla() {
    let Some(reference) = Reference::from_env() else {
        eprintln!("Skipping the feature stage: HARPAS_VANILLA_SERVER_DIR is not set");
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
        (
            "FeatureStageProbe.java",
            include_str!("FeatureStageProbe.java"),
        ),
    ]);
    let index = FeatureIndex::overworld(&data).expect("compile the feature index");
    let names: Vec<_> = data.placed_features.keys().collect();
    let mut compared = 0;
    let mut placed = 0;
    for &seed in reference::sampled_seeds(&SEEDS) {
        let output = reference.temp.0.join("features.txt");
        let mut args = vec![output.display().to_string(), seed.to_string()];
        args.extend(CHUNKS.iter().map(|(x, z)| format!("{x},{z}")));
        reference.java("FeatureStageProbe", args);
        let text = fs::read_to_string(&output).expect("read the vanilla feature output");
        let mut lines = text.lines().peekable();

        let mut step = 0;
        while let Some(line) = lines.next_if(|line| line.starts_with("step")) {
            let expected: Vec<_> = line.split_whitespace().skip(1).collect();
            let actual: Vec<_> = index
                .step_order(step)
                .unwrap_or_default()
                .iter()
                .map(|&placed| names[placed].as_str())
                .collect();
            assert_eq!(actual, expected, "order of generation step {step}");
            step += 1;
        }
        assert_eq!(step, 11, "generation steps");

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
            assert!(!expected.is_empty(), "vanilla placed no ores in {chunk:?}");
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
    eprintln!("Compared {placed} ore blocks around {compared} decorated chunks");
}
