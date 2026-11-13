//! Desert well placement, heightmaps, archaeology NBT and random tails compared with vanilla.
mod reference;

use std::fs;

use harpas_core::block::block_states;
use harpas_core::nbt::Nbt;
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

fn prepare(test: usize, y: i32) -> RegionFeatureWorld {
    let chunks = (0..=1).flat_map(|x| {
        (-1..=0).map(move |z| {
            let mut chunk = GeneratedChunk::new(x, z, -64, 384, 1);
            chunk.freeze_worldgen_heightmaps();
            chunk
        })
    });
    let mut world = RegionFeatureWorld::new(chunks, 0).unwrap();
    if test == 10 {
        return world;
    }
    for z in -4..=2 {
        for x in 12..=18 {
            for py in y - 3..=y {
                world.set_block_state(x, py, z, state("sand"));
            }
        }
    }
    let supports = [
        "red_sand",
        "sandstone",
        "dirt",
        "water",
        "suspicious_sand",
        "gravel",
    ];
    if (4..=9).contains(&test) {
        world.set_block_state(15, y, -1, state(supports[test - 4]));
    }
    if (11..=16).contains(&test) {
        let x = if test == 16 { 12 } else { 13 };
        world.set_block_state(
            x,
            y - 1,
            -3,
            state(match test {
                13 => "cave_air",
                14 => "sand",
                _ => "air",
            }),
        );
        world.set_block_state(
            x,
            y - 2,
            -3,
            state(match test {
                11 => "sand",
                13 => "void_air",
                15 => "water",
                _ => "air",
            }),
        );
    }
    if (17..=21).contains(&test) {
        world.set_block_state(
            15,
            y + 1,
            -1,
            state(["stone", "short_grass", "water", "cave_air", "void_air"][test - 17]),
        );
    }
    if test == 29 {
        world.set_block_state(17, y + 1, -1, state("chest"));
        world.set_block_state(18, y + 2, -1, state("chest"));
        world.set_block_state(14, y + 2, -2, state("oak_log"));
    }
    if test == 30 {
        for py in y - 3..y {
            world.set_block_state(15, py, -1, state("air"));
        }
    }
    if test == 31 {
        for z in -3i32..=1 {
            for x in 13i32..=17 {
                world.set_block_state(
                    x,
                    y - 1,
                    z,
                    state(if (x + z).rem_euclid(2) == 0 {
                        "air"
                    } else {
                        "water"
                    }),
                );
            }
        }
    }
    world
}

fn blocks(world: &RegionFeatureWorld, y: i32) -> Vec<u32> {
    ((y - 4).max(-64)..(y + 10).min(320))
        .flat_map(|py| {
            (-4..=2).flat_map(move |z| (12..=18).map(move |x| world.block_state(x, py, z)))
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
        22..=25 => test as i32 - 85,
        26 => 318,
        27 | 28 => 319,
        _ => 64,
    };
    let mut world = prepare(test, y);
    let before = blocks(&world, y);
    let mut random = WorldgenRandom::new(source);
    let dy = match test {
        1 | 20 | 21 => 8,
        2 | 28 => 1,
        3 => -1,
        _ => 0,
    };
    let placed = place_feature(&mut world, feature, &mut random, [15, y + dy, -1]);
    assert_eq!(
        lines.next(),
        Some(format!("{label} {placed} {}", random.next_int()).as_str())
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
        for z in -4..=2 {
            for x in 12..=18 {
                assert_eq!(
                    lines.next(),
                    Some(format!("height {}", world.heightmap(kind, x, z)).as_str()),
                    "{label}: {kind:?} at {x}, {z}"
                );
            }
        }
    }
    let mut expected_entities: Vec<_> =
        std::iter::from_fn(|| lines.next_if(|line| line.starts_with("entity ")))
            .map(|line| {
                let bytes: Vec<_> = line.as_bytes()[7..]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                    .collect();
                Nbt::read_network(&mut bytes.as_slice()).unwrap()
            })
            .collect();
    let mut actual_entities: Vec<_> = (0..=1)
        .flat_map(|x| (-1..=0).map(move |z| (x, z)))
        .flat_map(|(x, z)| world.chunk(x, z).unwrap().block_entities())
        .map(|e| Nbt::Compound(e.clone()))
        .collect();
    let key = |entity: &Nbt| format!("{entity:?}");
    actual_entities.sort_by_key(key);
    expected_entities.sort_by_key(key);
    assert_eq!(
        actual_entities, expected_entities,
        "{label}: block entities"
    );
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn desert_wells_match_vanilla() {
    let reference = reference::Reference::required();
    let data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let feature = FeatureCompiler::new(&data)
        .configured("minecraft:desert_well")
        .unwrap()
        .unwrap();
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
            "DesertWellFeatureProbe.java",
            include_str!("DesertWellFeatureProbe.java"),
        ),
    ]);
    let output = reference.temp.0.join("wells.txt");
    reference.java(
        "DesertWellFeatureProbe",
        [
            output.display().to_string(),
            reference.server_jar.display().to_string(),
        ],
    );
    let text = fs::read_to_string(output).unwrap();
    let mut lines = text.lines().peekable();
    for seed in [0, 1, 3, 42, -4_172_144_997_902_289_642] {
        for legacy in [false, true] {
            for test in 0..32 {
                let label = format!("case {seed} {legacy} {test}");
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
    assert!(lines.next().is_none());
    eprintln!(
        "Compared blocks, heightmaps, block entities and random tails of 320 desert well placements"
    );
}
