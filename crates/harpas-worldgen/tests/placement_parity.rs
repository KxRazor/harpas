//! Compares every placed feature's placement modifiers with vanilla's own, on real carved terrain.
mod reference;

use std::collections::{BTreeMap, HashSet};
use std::fs;

use harpas_core::random::{WorldgenRandom, XoroshiroRandom};
use harpas_worldgen::{
    FeatureIndex, FeatureWorld, NoiseBasedChunkGenerator, RegionFeatureWorld, WorldgenData,
    place_positions,
};
use reference::Reference;

/// Includes seed 0, which seeds `Xoroshiro128++` specially, and a seed that fills all 64 bits.
const SEEDS: [i64; 3] = [0, 42, -4_172_144_997_902_289_642];
/// Far apart, to meet ocean, mountain, badlands, lush cave and mixed-biome chunks, on both sides of the origin.
/// The second group lies in a desert, swamp, mangrove swamp, badlands, cherry grove, plains, snowy plains,
/// birch forest, jungle, dark forest, savanna and pale garden of the last seed, where the survival rules of
/// trees, cactus and sugar cane decide which positions pass.
const CHUNKS: [(i32, i32); 18] = [
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
];

/// The positions of every placed feature in one chunk, by generation step and position in the step.
type FeaturePositions = BTreeMap<(usize, usize), Vec<[i32; 3]>>;

/// Every position the placed feature at `index` of `step` yields in the chunk.
fn positions(
    world: &mut RegionFeatureWorld,
    index: &FeatureIndex,
    decoration: i64,
    origin: [i32; 3],
    (step, position): (usize, usize),
) -> Option<Vec<[i32; 3]>> {
    let placed = index.step_order(step)?[position];
    let modifiers = index.modifiers(placed)?;
    let mut random = WorldgenRandom::new(XoroshiroRandom::new(0));
    random.set_feature_seed(decoration, position as i32, step as i32);
    let mut found = Vec::new();
    place_positions(
        world,
        index.allowed_biomes(placed).expect("allowed biomes"),
        modifiers,
        &mut random,
        origin,
        &mut |_, _, position| found.push(position),
    );
    Some(found)
}

#[test]
fn placement_modifiers_match_vanilla() {
    let Some(reference) = Reference::from_env() else {
        eprintln!("Skipping placement modifiers: HARPAS_VANILLA_SERVER_DIR is not set");
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
    ]);
    let index = FeatureIndex::overworld(&data).expect("compile the feature index");

    // Every placed feature's modifiers compile.
    let names: Vec<_> = data.placed_features.keys().collect();
    for (placed, name) in names.iter().enumerate() {
        assert!(
            index.modifiers(placed).is_some(),
            "{name} has placement modifiers Harpas does not apply"
        );
    }

    let mut compared = (HashSet::new(), 0, 0);
    for &seed in reference::sampled_seeds(&SEEDS) {
        let output = reference.temp.0.join("placements.txt");
        let mut args = vec![
            output.display().to_string(),
            reference.server_jar.display().to_string(),
            seed.to_string(),
        ];
        args.extend(CHUNKS.iter().map(|(x, z)| format!("{x},{z}")));
        reference.java("PlacementProbe", args);
        let text = fs::read_to_string(&output).expect("read the vanilla placement output");

        let mut expected: BTreeMap<(i32, i32), FeaturePositions> = BTreeMap::new();
        let mut current = None;
        for line in text.lines() {
            let fields: Vec<&str> = line.split(' ').collect();
            match fields[0] {
                "chunk" => {
                    let chunk = (fields[1].parse().unwrap(), fields[2].parse().unwrap());
                    current = Some((chunk, None));
                    expected.insert(chunk, BTreeMap::new());
                }
                "feature" => {
                    let feature = (fields[1].parse().unwrap(), fields[2].parse().unwrap());
                    let (chunk, slot) = current.as_mut().expect("chunk before feature");
                    *slot = Some(feature);
                    expected.get_mut(chunk).unwrap().insert(feature, Vec::new());
                }
                _ => {
                    let (chunk, slot) = current.as_ref().expect("chunk before position");
                    let position = [0, 1, 2].map(|axis| fields[axis].parse().unwrap());
                    expected
                        .get_mut(chunk)
                        .unwrap()
                        .get_mut(&slot.expect("feature before position"))
                        .unwrap()
                        .push(position);
                }
            }
        }

        let generator =
            NoiseBasedChunkGenerator::overworld(seed, &data).expect("compile generator");
        for &(cx, cz) in &CHUNKS {
            let chunks = (-1..=1).flat_map(|dx| (-1..=1).map(move |dz| (cx + dx, cz + dz)));
            let chunks: Vec<_> = chunks
                .map(|(x, z)| generator.generate_chunk_carved(x, z, &data).unwrap())
                .collect();
            let mut world = RegionFeatureWorld::with_stored_biomes(chunks, seed).expect("region");
            let origin = [cx * 16, world.min_y(), cz * 16];
            let mut random = WorldgenRandom::new(XoroshiroRandom::new(0));
            let decoration = random.set_decoration_seed(seed, origin[0], origin[2]);
            let features = &expected[&(cx, cz)];
            for (step, count) in (0..11).map(|step| (step, index.step_order(step).unwrap().len())) {
                assert_eq!(
                    features.range((step, 0)..(step + 1, 0)).count(),
                    count,
                    "features in step {step}"
                );
            }
            for (&feature, vanilla) in features {
                let Some(actual) = positions(&mut world, &index, decoration, origin, feature)
                else {
                    continue;
                };
                let placed = index.step_order(feature.0).unwrap()[feature.1];
                assert_eq!(
                    &actual, vanilla,
                    "seed {seed} chunk ({cx}, {cz}): positions of {}",
                    names[placed]
                );
                compared.0.insert(placed);
                compared.1 += 1;
                compared.2 += vanilla.len();
            }
        }
        fs::remove_file(&output).unwrap();
    }
    assert!(compared.2 > 0, "vanilla placed nothing");
    eprintln!(
        "Compared {} positions from {} feature runs of {} placed features",
        compared.2,
        compared.1,
        compared.0.len()
    );
}
