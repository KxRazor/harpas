//! Block blob blocks, results and random consumption on mixed ground, at chunk edges and world limits.
mod reference;

use std::fs;

use harpas_core::block::block_states;
use harpas_core::random::{LegacyRandom, RandomSource, WorldgenRandom, XoroshiroRandom};
use harpas_worldgen::{
    CompiledFeature, FeatureCompiler, FeatureConfigDef, FeatureWorld, GeneratedChunk,
    RegionFeatureWorld, WorldgenData, place_feature,
};
use serde_json::json;

fn state(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}

fn prepare(test: usize, y: i32) -> RegionFeatureWorld {
    let chunks =
        (-1..=1).flat_map(|x| (-2..=0).map(move |z| GeneratedChunk::new(x, z, -64, 384, 1)));
    let mut world = RegionFeatureWorld::new(chunks, 0).unwrap();
    if test == 31 {
        return world;
    }
    for z in -8i32..=6 {
        for x in 8i32..=22 {
            let top = y - 2 - (x + z).rem_euclid(3);
            world.set_block_state(x, top, z, state("dirt"));
            world.set_block_state(x, top + 1, z, state("water"));
        }
    }
    let supports = [
        "dirt",
        "grass_block",
        "podzol",
        "coarse_dirt",
        "rooted_dirt",
        "mud",
        "muddy_mangrove_roots",
        "moss_block",
        "pale_moss_block",
        "mycelium",
        "stone",
        "sand",
        "gravel",
        "water",
        "lava",
        "snow",
        "cave_air",
        "void_air",
        "air",
        "bedrock",
    ];
    world.set_block_state(
        15,
        y - 1,
        -1,
        state(if test < 20 { supports[test] } else { "dirt" }),
    );
    world.set_block_state(16, y - 1, -1, state("sand"));
    for (case, dy, block) in [
        (20, 4, "stone"),
        (21, 0, "water"),
        (22, 0, "bedrock"),
        (23, 1, "chest"),
        (28, -1, "air"),
        (29, 4, "sand"),
        (30, 4, "water"),
    ] {
        if test == case {
            world.set_block_state(15, y + dy, -1, state(block));
        }
    }
    world
}

fn blocks(world: &RegionFeatureWorld, y: i32) -> Vec<u32> {
    ((y - 8).max(-64)..(y + 10).min(320))
        .flat_map(|py| {
            (-8..=6).flat_map(move |z| (8..=22).map(move |x| world.block_state(x, py, z)))
        })
        .collect()
}

fn compare<R: RandomSource>(
    lines: &mut std::iter::Peekable<std::str::Lines<'_>>,
    feature: &CompiledFeature,
    source: R,
    label: &str,
    test: usize,
) {
    let y = match test {
        24 => -61,
        25 => -60,
        26 => 319,
        27 => 320,
        _ => 64,
    };
    let mut world = prepare(test, y);
    let before = blocks(&world, y);
    let mut random = WorldgenRandom::new(source);
    let origin = [15, y + if matches!(test, 20 | 29 | 30) { 8 } else { 0 }, -1];
    let placed = place_feature(&mut world, feature, &mut random, origin);
    assert_eq!(
        lines.next(),
        Some(format!("{label} {placed} {}", random.next_int()).as_str())
    );
    let expected: Vec<(usize, u32)> =
        std::iter::from_fn(|| lines.next_if(|line| !line.starts_with("case ")))
            .map(|line| {
                let (i, state) = line.split_once(' ').unwrap();
                (i.parse().unwrap(), state.parse().unwrap())
            })
            .collect();
    let actual: Vec<_> = blocks(&world, y)
        .into_iter()
        .enumerate()
        .filter(|&(i, state)| state != before[i])
        .collect();
    assert_eq!(actual, expected, "{label}");
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn block_blobs_match_vanilla() {
    let reference = reference::Reference::required();
    let mut data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let predicates = [
        json!({"type": "minecraft:true"}),
        json!({"type": "minecraft:matching_blocks", "blocks": "minecraft:sand", "offset": [1, 0, 0]}),
        json!({"type": "minecraft:not", "predicate": {"type": "minecraft:true"}}),
        json!({"type": "minecraft:matching_blocks", "blocks": "minecraft:sand"}),
    ];
    for (i, predicate) in predicates.into_iter().enumerate() {
        let mut feature = data.configured_features["minecraft:forest_rock"].clone();
        feature.config = FeatureConfigDef::Other(json!({
            "state": {"Name": "minecraft:oak_log", "Properties": {"axis": "x"}}, "can_place_on": predicate
        }));
        data.configured_features
            .insert(format!("test:blob{i}"), feature);
    }
    let mut compiler = FeatureCompiler::new(&data);
    let names = [
        "minecraft:forest_rock",
        "test:blob0",
        "test:blob1",
        "test:blob2",
        "test:blob3",
    ];
    reference.compile(&[
        (
            "WorldgenReferenceProbe.java",
            include_str!("WorldgenReferenceProbe.java"),
        ),
        ("PlacementProbe.java", include_str!("PlacementProbe.java")),
        (
            "ChunkParityProbe.java",
            include_str!("ChunkParityProbe.java"),
        ),
        (
            "BlockBlobFeatureProbe.java",
            include_str!("BlockBlobFeatureProbe.java"),
        ),
    ]);
    let output = reference.temp.0.join("blobs.txt");
    reference.java(
        "BlockBlobFeatureProbe",
        [
            output.display().to_string(),
            reference.server_jar.display().to_string(),
        ],
    );
    let text = fs::read_to_string(output).unwrap();
    let mut lines = text.lines().peekable();
    for seed in [0, 42, -4_172_144_997_902_289_642] {
        for legacy in [false, true] {
            for (i, name) in names.iter().enumerate() {
                let feature = compiler.configured(name).unwrap().unwrap();
                for test in 0..32 {
                    let label = format!("case {seed} {legacy} {i} {test}");
                    if legacy {
                        compare(&mut lines, &feature, LegacyRandom::new(seed), &label, test);
                    } else {
                        compare(
                            &mut lines,
                            &feature,
                            XoroshiroRandom::new(seed),
                            &label,
                            test,
                        );
                    }
                }
            }
        }
    }
    assert!(lines.next().is_none());
    eprintln!("Compared blocks, results and random tails of 960 block blob placements");
}
