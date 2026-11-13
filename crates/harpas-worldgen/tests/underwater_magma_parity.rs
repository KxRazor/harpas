//! Underwater magma blocks, live heightmaps, results and random consumption compared with vanilla.
mod reference;

use std::fs;

use harpas_core::block::block_states;
use harpas_core::random::{LegacyRandom, RandomSource, WorldgenRandom, XoroshiroRandom};
use harpas_worldgen::{
    FeatureCompiler, FeatureWorld, GeneratedChunk, HeightmapType, RegionFeatureWorld, WorldgenData,
    place_feature,
};
use serde_json::json;

fn state(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}

fn floor(test: usize) -> i32 {
    match test {
        28 => -64,
        29 => -63,
        30 => 318,
        31 => 319,
        32 => -65,
        _ => 44,
    }
}

fn distance(test: usize) -> i32 {
    match test {
        5 => 0,
        6 => 5,
        7 | 30 | 31 => 1,
        8 => 7,
        9 => 8,
        _ => 4,
    }
}

fn config(test: usize) -> serde_json::Value {
    json!({
        "floor_search_range": match test { 3 => 0, 4 => 1, 8 | 9 => 8, 34 => 512, _ => 5 },
        "placement_radius_around_floor": match test { 10 => 0, 11 => 2, _ => 1 },
        "placement_probability_per_valid_position": match test { 1 => 0.0, 2 | 35 => 1.0, _ => 0.5 }
    })
}

fn prepare(test: usize) -> RegionFeatureWorld {
    let chunks = (0..=1).flat_map(|x| {
        (-1..=0).map(move |z| {
            let mut chunk = GeneratedChunk::new(x, z, -64, 384, 1);
            chunk.freeze_worldgen_heightmaps();
            chunk
        })
    });
    let mut world = RegionFeatureWorld::new(chunks, 0).unwrap();
    let (floor, top) = (floor(test), floor(test) + distance(test));
    let wet_slab = block_states()
        .state(
            "minecraft:oak_slab",
            [("waterlogged", "true"), ("type", "bottom")],
        )
        .unwrap()
        .0;
    for y in (floor - 4).max(-64)..=(top + 4).min(319) {
        for z in -5_i32..=3 {
            for x in 11_i32..=19 {
                let surface = floor
                    + if test == 26 {
                        (x + z).rem_euclid(3) - 1
                    } else {
                        0
                    };
                let mut block = state(if y <= surface { "stone" } else { "water" });
                if y <= surface {
                    block = match test {
                        22 => wet_slab,
                        23 => state("lava"),
                        24 => state("short_grass"),
                        25 => state("magma_block"),
                        _ => block,
                    };
                    if test == 20 && (x * 3 + z * 7).rem_euclid(5) == 0 {
                        block = state("water");
                    }
                    if test == 21 && y == floor - 1 {
                        block = state("cave_air");
                    }
                    if test == 35 && x == 16 {
                        block = state("air");
                    }
                } else if test == 16 {
                    block = block_states()
                        .state("minecraft:water", [("level", "5")])
                        .unwrap()
                        .0;
                }
                if test == 27 || test == 34 {
                    block = state("water");
                }
                world.set_block_state(x, y, z, block);
            }
        }
    }
    let origin = match test {
        12 => Some(state("air")),
        13 => Some(state("lava")),
        14 => Some(wet_slab),
        15 => Some(state("kelp")),
        17 => Some(state("bubble_column")),
        _ => None,
    };
    if let Some(block) = origin {
        world.set_block_state(15, top, -1, block);
    }
    if test == 18 || test == 19 {
        world.set_block_state(
            15,
            top - 2,
            -1,
            state(if test == 18 { "air" } else { "kelp" }),
        );
    }
    world
}

fn blocks(world: &RegionFeatureWorld, test: usize) -> Vec<u32> {
    ((floor(test) - 4).max(-64)..=(floor(test) + distance(test) + 4).min(319))
        .flat_map(|y| {
            (-5..=3).flat_map(move |z| (11..=19).map(move |x| world.block_state(x, y, z)))
        })
        .collect()
}

fn compare<R: RandomSource>(
    data: &WorldgenData,
    lines: &mut std::iter::Peekable<std::str::Lines<'_>>,
    source: R,
    label: &str,
    test: usize,
) {
    let feature = FeatureCompiler::new(data)
        .configured(&format!("test:case_{test}"))
        .unwrap()
        .unwrap();
    let mut world = prepare(test);
    let before = blocks(&world, test);
    let mut random = WorldgenRandom::new(source);
    let placed = place_feature(
        &mut world,
        &feature,
        &mut random,
        [15, floor(test) + distance(test), -1],
    );
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
    let actual: Vec<_> = blocks(&world, test)
        .into_iter()
        .enumerate()
        .filter(|&(i, state)| state != before[i])
        .collect();
    assert_eq!(actual, expected, "{label}: blocks");
    for kind in HeightmapType::ALL
        .into_iter()
        .filter(|kind| !kind.is_worldgen())
    {
        for z in -5..=3 {
            for x in 11..=19 {
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
fn underwater_magma_matches_vanilla() {
    let reference = reference::Reference::required();
    let data = WorldgenData::from_json_entries((0..37).map(|test| {
        let mut config = config(test);
        if test == 36 {
            config["placement_radius_around_floor"] = json!(0);
            config["placement_probability_per_valid_position"] = json!(1.0);
        }
        (
            format!("data/test/worldgen/configured_feature/case_{test}.json"),
            json!({"type": "minecraft:underwater_magma", "config": config}).to_string(),
        )
    }))
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
            "UnderwaterMagmaProbe.java",
            include_str!("UnderwaterMagmaProbe.java"),
        ),
    ]);
    let output = reference.temp.0.join("magma.txt");
    reference.java(
        "UnderwaterMagmaProbe",
        [
            output.display().to_string(),
            reference.server_jar.display().to_string(),
        ],
    );
    let text = fs::read_to_string(output).unwrap();
    let mut lines = text.lines().peekable();
    for seed in [0, 42, -4_172_144_997_902_289_642] {
        for legacy in [false, true] {
            for test in 0..36 {
                let label = format!("case {seed} {legacy} {test}");
                if legacy {
                    compare(&data, &mut lines, LegacyRandom::new(seed), &label, test);
                } else {
                    compare(&data, &mut lines, XoroshiroRandom::new(seed), &label, test);
                }
            }
        }
    }
    let feature = FeatureCompiler::new(&data)
        .configured("test:case_36")
        .unwrap()
        .unwrap();
    let mut world = prepare(0);
    for (role, [dx, dy, dz]) in [
        [0, 0, 0],
        [0, -1, 0],
        [0, 0, -1],
        [1, 0, 0],
        [0, 0, 1],
        [-1, 0, 0],
    ]
    .into_iter()
    .enumerate()
    {
        for id in 0..block_states().len() {
            world.set_block_state(15, 44, -1, state("stone"));
            world.set_block_state(15 + dx, 44 + dy, -1 + dz, id);
            let placed = place_feature(
                &mut world,
                &feature,
                &mut WorldgenRandom::new(XoroshiroRandom::new(42)),
                [15, 48, -1],
            );
            assert_eq!(
                lines.next(),
                Some(format!("material {role} {id} {placed}").as_str()),
                "role {role}: {:?}",
                block_states().properties(id)
            );
            world.set_block_state(15 + dx, 44 + dy, -1 + dz, state("stone"));
        }
    }
    assert!(lines.next().is_none());
    eprintln!(
        "Compared 216 underwater magma placements and all block states as destinations and five closing neighbors"
    );
}
