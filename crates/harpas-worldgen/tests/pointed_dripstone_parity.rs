//! Small dripstone blocks, live heightmaps, results and random consumption compared with vanilla.
mod reference;

use std::fs;

use harpas_core::block::block_states;
use harpas_core::random::{LegacyRandom, RandomSource, WorldgenRandom, XoroshiroRandom};
use harpas_worldgen::{
    FeatureCompiler, FeatureWorld, GeneratedChunk, HeightmapType, RegionFeatureWorld, WorldgenData,
    place_feature,
};
use serde_json::{Value, json};

fn state(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}

fn y(test: usize) -> i32 {
    match test {
        24 => -64,
        25 => -63,
        26 => 318,
        27 => 319,
        28 => -65,
        29 => 320,
        _ => 64,
    }
}

fn config(index: usize) -> Value {
    let [taller, spread, radius2, radius3] = match index {
        0 => return json!({}),
        1 => [0.0; 4],
        2 => [1.0; 4],
        3 => [1.0, 1.0, 0.0, 1.0],
        4 => [1.0, 1.0, 1.0, 0.0],
        6 => [1.0, 0.0, 0.0, 0.0],
        _ => [0.6, 0.3, 0.8, 0.4],
    };
    json!({"chance_of_taller_dripstone": taller, "chance_of_directional_spread": spread,
        "chance_of_spread_radius2": radius2, "chance_of_spread_radius3": radius3})
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
    let y = y(test);
    for z in -5_i32..=3 {
        for x in 11_i32..=19 {
            let support = state(match test {
                4 => "dripstone_block",
                5 => "deepslate",
                6 => "dirt",
                7 => "obsidian",
                _ => "stone",
            });
            if test != 1 && test != 3 {
                world.set_block_state(x, y - 1, z, support);
            }
            if test == 1 || test == 2 || test == 22 {
                world.set_block_state(x, y + 1, z, support);
            }
            if test == 20 {
                for py in y - 3..=y - 1 {
                    world.set_block_state(x, py, z, support);
                }
            }
            if test == 21 {
                world.set_block_state(
                    x,
                    y - 1,
                    z,
                    state(if (x + z).rem_euclid(3) == 0 {
                        "dirt"
                    } else {
                        "stone"
                    }),
                );
            }
        }
    }
    let at = match test {
        8 | 17 => state("water"),
        9 => {
            block_states()
                .state("minecraft:water", [("level", "5")])
                .unwrap()
                .0
        }
        10 => state("lava"),
        11 => state("stone"),
        12 => state("cave_air"),
        13 => state("void_air"),
        14 => state("kelp"),
        15 => {
            block_states()
                .state("minecraft:oak_slab", [("waterlogged", "true")])
                .unwrap()
                .0
        }
        _ => state("air"),
    };
    world.set_block_state(15, y, -1, at);
    if test == 16 || test == 17 {
        world.set_block_state(15, y + 1, -1, state("water"));
    }
    if test == 18 {
        world.set_block_state(15, y + 1, -1, state("kelp"));
    }
    if test == 19 {
        world.set_block_state(15, y + 1, -1, state("short_grass"));
    }
    if test == 23 {
        world.set_block_state(15, y - 1, -1, state("lava"));
    }
    world
}

fn blocks(world: &RegionFeatureWorld, y: i32) -> Vec<u32> {
    ((y - 4).max(-64)..=(y + 4).min(319))
        .flat_map(|py| {
            (-5..=3).flat_map(move |z| (11..=19).map(move |x| world.block_state(x, py, z)))
        })
        .collect()
}

fn compare<R: RandomSource>(
    feature: &harpas_worldgen::CompiledFeature,
    lines: &mut std::iter::Peekable<std::str::Lines<'_>>,
    source: R,
    label: &str,
    test: usize,
) {
    let mut world = prepare(test);
    let before = blocks(&world, y(test));
    let mut random = WorldgenRandom::new(source);
    let placed = place_feature(&mut world, feature, &mut random, [15, y(test), -1]);
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
    let actual: Vec<_> = blocks(&world, y(test))
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
fn pointed_dripstone_matches_vanilla() {
    let reference = reference::Reference::required();
    let mut data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    for index in 0..7 {
        data.configured_features.insert(
            format!("test:case_{index}"),
            harpas_worldgen::ConfiguredFeatureDef {
                feature_type: "minecraft:pointed_dripstone".into(),
                config: harpas_worldgen::FeatureConfigDef::Other(config(index)),
            },
        );
    }
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
            "PointedDripstoneProbe.java",
            include_str!("PointedDripstoneProbe.java"),
        ),
    ]);
    let output = reference.temp.0.join("pointed-dripstone.txt");
    reference.java(
        "PointedDripstoneProbe",
        [
            output.display().to_string(),
            reference.server_jar.display().to_string(),
        ],
    );
    let text = fs::read_to_string(output).unwrap();
    let mut lines = text.lines().peekable();
    let mut compiler = FeatureCompiler::new(&data);
    for seed in [0, 42, -4_172_144_997_902_289_642] {
        for legacy in [false, true] {
            for config in 0..6 {
                let feature = compiler
                    .configured(&format!("test:case_{config}"))
                    .unwrap()
                    .unwrap();
                for test in 0..30 {
                    let label = format!("case {seed} {legacy} {config} {test}");
                    if legacy {
                        compare(&feature, &mut lines, LegacyRandom::new(seed), &label, test);
                    } else {
                        compare(
                            &feature,
                            &mut lines,
                            XoroshiroRandom::new(seed),
                            &label,
                            test,
                        );
                    }
                }
            }
        }
    }
    let short = compiler.configured("test:case_1").unwrap().unwrap();
    let tall = compiler.configured("test:case_6").unwrap().unwrap();
    let mut world = prepare(3);
    world.set_block_state(11, 63, -5, state("stone"));
    world.set_block_state(11, 64, -5, state("stone"));
    for role in 0..4 {
        for id in 0..block_states().len() {
            world.set_block_state(15, 63, -1, state(if role < 2 { "air" } else { "stone" }));
            for py in 64..=66 {
                world.set_block_state(15, py, -1, state("air"));
            }
            let py = match role {
                0 => 63,
                2 => 64,
                _ => 65,
            };
            world.set_block_state(15, py, -1, id);
            let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
            let placed = place_feature(
                &mut world,
                if role == 3 { &tall } else { &short },
                &mut random,
                [15, 64, -1],
            );
            let mut result = format!("material {role} {id} {placed} {}", random.next_int());
            for py in 63..=66 {
                result.push_str(&format!(" {}", world.block_state(15, py, -1)));
            }
            assert_eq!(
                lines.next(),
                Some(result.as_str()),
                "role {role}: {:?}",
                block_states().properties(id)
            );
        }
    }
    assert!(lines.next().is_none());
    eprintln!(
        "Compared 1,080 small dripstone placements and {} block states in four roles",
        block_states().len()
    );
}
