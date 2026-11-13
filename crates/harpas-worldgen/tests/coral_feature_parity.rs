//! Reef blocks, live heightmaps, results and random consumption compared with vanilla's coral features.
mod reference;

use std::fs;

use harpas_core::block::block_states;
use harpas_core::random::{LegacyRandom, RandomSource, WorldgenRandom, XoroshiroRandom};
use harpas_worldgen::{
    CompiledFeature, FeatureCompiler, FeatureWorld, GeneratedChunk, HeightmapType,
    RegionFeatureWorld, WorldgenData, place_feature,
};

fn state(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}

fn y(test: usize) -> i32 {
    match test {
        20 => -64,
        21 => -63,
        22 => 318,
        23 => 319,
        24 => 320,
        25 => -65,
        _ => 48,
    }
}

fn prepare(test: usize, y: i32) -> RegionFeatureWorld {
    let chunks = (0..=1).flat_map(|x| {
        (-1..=0).map(move |z| {
            let mut chunk = GeneratedChunk::new(x, z, -64, 384, 1);
            chunk.freeze_worldgen_heightmaps();
            chunk
        })
    });
    let mut world = RegionFeatureWorld::new(chunks, 0).unwrap();
    let fill = match test {
        1 => state("air"),
        2 => state("lava"),
        3 => {
            block_states()
                .state("minecraft:water", [("level", "5")])
                .unwrap()
                .0
        }
        4 => {
            block_states()
                .state(
                    "minecraft:oak_slab",
                    [("waterlogged", "true"), ("type", "bottom")],
                )
                .unwrap()
                .0
        }
        _ => state("water"),
    };
    for py in (y - 4).max(-64)..(y + 12).min(320) {
        for z in -10..=8 {
            for x in 6..=24 {
                let mut block = fill;
                if (test == 16 && py == y + 3)
                    || (test == 17 && (x * 3 + py * 5 + z * 7).rem_euclid(5) == 0)
                {
                    block = state("stone");
                }
                if test == 18 && py >= y + 2 {
                    block = state("air");
                }
                if test == 19 && (x * 3 + py * 5 + z * 7).rem_euclid(11) == 0 {
                    block = state("brain_coral");
                }
                world.set_block_state(x, py, z, block);
            }
        }
    }
    let origins = [
        "tube_coral",
        "tube_coral_fan",
        "tube_coral_wall_fan",
        "tube_coral_block",
        "dead_tube_coral",
        "kelp",
        "seagrass",
        "stone",
    ];
    if (5..=12).contains(&test) {
        world.set_block_state(15, y, -1, state(origins[test - 5]));
    }
    if (13..=15).contains(&test) {
        world.set_block_state(
            15,
            y + 1,
            -1,
            state(["air", "tube_coral", "seagrass"][test - 13]),
        );
    }
    world
}

fn blocks(world: &RegionFeatureWorld, y: i32) -> Vec<u32> {
    ((y - 4).max(-64)..(y + 12).min(320))
        .flat_map(|py| {
            (-10..=8).flat_map(move |z| (6..=24).map(move |x| world.block_state(x, py, z)))
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
    let y = y(test);
    let mut world = prepare(test, y);
    let before = blocks(&world, y);
    let mut random = WorldgenRandom::new(source);
    let placed = place_feature(&mut world, feature, &mut random, [15, y, -1]);
    assert_eq!(
        lines.next(),
        Some(format!("{label} {placed} {}", random.next_int()).as_str()),
        "{label}: result and random tail"
    );
    let expected: Vec<(usize, u32)> =
        std::iter::from_fn(|| lines.next_if(|line| line.starts_with("block ")))
            .map(|line| {
                let (i, state) = line[6..].split_once(' ').unwrap();
                (i.parse().unwrap(), state.parse().unwrap())
            })
            .collect();
    let actual: Vec<_> = blocks(&world, y)
        .into_iter()
        .enumerate()
        .filter(|&(i, state)| state != before[i])
        .collect();
    assert_eq!(actual, expected, "{label}: blocks");
    for kind in HeightmapType::ALL
        .into_iter()
        .filter(|kind| !kind.is_worldgen())
    {
        for z in -10..=8 {
            for x in 6..=24 {
                assert_eq!(
                    lines.next(),
                    Some(format!("height {}", world.heightmap(kind, x, z)).as_str()),
                    "{label}: {kind:?} at {x}, {z}"
                );
            }
        }
    }
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn coral_reefs_match_vanilla() {
    let reference = reference::Reference::required();
    let data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let vegetation = FeatureCompiler::new(&data)
        .configured("minecraft:warm_ocean_vegetation")
        .unwrap()
        .unwrap();
    let CompiledFeature::SimpleRandomSelector(features) = vegetation.as_ref() else {
        panic!("warm_ocean_vegetation must select the three reef shapes");
    };
    assert_eq!(features.len(), 3);
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
            "CoralFeatureProbe.java",
            include_str!("CoralFeatureProbe.java"),
        ),
    ]);
    let output = reference.temp.0.join("corals.txt");
    reference.java(
        "CoralFeatureProbe",
        [
            output.display().to_string(),
            reference.server_jar.display().to_string(),
        ],
    );
    let text = fs::read_to_string(output).unwrap();
    let mut lines = text.lines().peekable();
    for seed in [0, 42, -4_172_144_997_902_289_642] {
        for legacy in [false, true] {
            for (shape, feature) in features.iter().enumerate() {
                for test in 0..26 {
                    let label = format!("case {seed} {legacy} {shape} {test}");
                    if legacy {
                        compare(
                            &mut lines,
                            &feature.feature,
                            LegacyRandom::new(seed),
                            &label,
                            test,
                        );
                    } else {
                        compare(
                            &mut lines,
                            &feature.feature,
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
    eprintln!(
        "Compared blocks, live heightmaps, results and random tails of 468 coral reef placements"
    );
}
