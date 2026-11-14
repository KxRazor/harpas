//! Large dripstone blocks, live heightmaps, results and random consumption compared with vanilla.
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
    static STATES: std::sync::OnceLock<std::collections::HashMap<&str, u32>> =
        std::sync::OnceLock::new();
    STATES.get_or_init(|| {
        [
            "air",
            "stone",
            "dirt",
            "lava",
            "water",
            "obsidian",
            "cave_air",
            "dripstone_block",
        ]
        .into_iter()
        .map(|name| {
            (
                name,
                block_states()
                    .default_state(&format!("minecraft:{name}"))
                    .unwrap(),
            )
        })
        .collect()
    })[name]
}

fn y(test: usize) -> i32 {
    match test {
        22 => -62,
        23 => 317,
        _ => 64,
    }
}
fn floor(test: usize) -> i32 {
    match test {
        7 | 8 => 62,
        9 => 34,
        10 => 35,
        22 => -64,
        23 => 314,
        _ => 40,
    }
}
fn ceiling(test: usize) -> i32 {
    match test {
        7 => 66,
        8 => 67,
        9 => 94,
        10 => 93,
        22 => -59,
        23 => 319,
        _ => 80,
    }
}

fn initial(test: usize, x: i32, py: i32, z: i32) -> u32 {
    if [x, py, z] == [15, y(test), -1] {
        if test == 17 {
            return state("stone");
        }
        if test == 18 {
            return state("cave_air");
        }
        if test == 19 {
            return block_states()
                .state("minecraft:oak_slab", [("waterlogged", "true")])
                .unwrap()
                .0;
        }
    }
    let shift = if test == 15 {
        ((x - 15).abs() + (z + 1).abs()).rem_euclid(7)
    } else {
        0
    };
    let (f, c) = (floor(test) - shift, ceiling(test) + shift);
    let depth = if test == 14 { 0 } else { 12 };
    if test != 1 && py >= f - depth && py <= f && (test != 16 || (x - 15).abs() <= 3) {
        return state(match test {
            3 => "dirt",
            11 => "lava",
            _ => "stone",
        });
    }
    if test != 2 && py >= c && py <= c + depth && (test != 16 || (x - 15).abs() <= 3) {
        return state(match test {
            4 => "dirt",
            12 => "lava",
            _ => "stone",
        });
    }
    if (test == 13 || test == 24 || test == 25) && py == 68 && [x, z] != [15, -1] {
        return state(match test {
            24 => "dripstone_block",
            25 => "stone",
            _ => "obsidian",
        });
    }
    if py > f && py < c {
        if test == 5 || test == 20 && py <= 52 {
            return state("water");
        }
        if test == 6 || test == 21 && py <= 52 {
            return state("lava");
        }
    }
    state("air")
}

fn prepare(test: usize) -> RegionFeatureWorld {
    let chunks = (-1..=2).flat_map(|cx| {
        (-2..=1).map(move |cz| {
            let mut chunk = GeneratedChunk::new(cx, cz, -64, 384, 1);
            for z in 0..16 {
                for x in 0..16 {
                    for py in (floor(test) - 12).max(-64)..=(ceiling(test) + 12).min(319) {
                        let state = initial(test, cx * 16 + x as i32, py, cz * 16 + z as i32);
                        if !block_states().is_air(state) {
                            chunk.set_block_state(x, py, z, state);
                        }
                    }
                }
            }
            chunk.freeze_worldgen_heightmaps();
            chunk
        })
    });
    RegionFeatureWorld::new(chunks, 0).unwrap()
}

fn configs() -> Vec<Value> {
    let uniform = |min: f32, max: f32| json!({"type": "minecraft:uniform", "min_inclusive": min, "max_exclusive": max});
    let base = json!({"floor_to_ceiling_search_range": 30, "column_radius": 4, "height_scale": 1,
        "max_column_radius_to_cave_height_ratio": 0.33, "stalactite_bluntness": 0.5,
        "stalagmite_bluntness": 0.7, "wind_speed": 0.2, "min_radius_for_wind": 100, "min_bluntness_for_wind": 0.6});
    let mut configs = vec![base.clone(); 8];
    configs[0]["column_radius"] =
        json!({"type":"minecraft:uniform", "min_inclusive":3, "max_inclusive":19});
    configs[0]["height_scale"] = uniform(0.4, 2.0);
    configs[0]["stalactite_bluntness"] = uniform(0.3, 0.9);
    configs[0]["stalagmite_bluntness"] = uniform(0.4, 1.0);
    configs[0]["wind_speed"] = uniform(0.0, 0.3);
    configs[0]["min_radius_for_wind"] = json!(4);
    configs[2]["column_radius"] = json!(8);
    configs[2]["height_scale"] = json!(2);
    configs[2]["stalactite_bluntness"] = json!(1);
    configs[2]["stalagmite_bluntness"] = json!(1);
    configs[2]["min_radius_for_wind"] = json!(8);
    configs[2]["min_bluntness_for_wind"] = json!(1);
    configs[3]["column_radius"] = json!(1);
    configs[4]["height_scale"] = json!(0);
    configs[5]["column_radius"] =
        json!({"type":"minecraft:biased_to_bottom", "min_inclusive":3, "max_inclusive":8});
    configs[5]["height_scale"] =
        json!({"type":"minecraft:trapezoid", "min":0.5, "max":2, "plateau":0.5});
    configs[5]["stalactite_bluntness"] =
        json!({"type":"minecraft:clamped_normal", "mean":0.7, "deviation":0.1, "min":0.3, "max":1});
    configs[6]["floor_to_ceiling_search_range"] = json!(0);
    configs[7]["min_radius_for_wind"] = json!(4);
    configs[7]["min_bluntness_for_wind"] = json!(0.5);
    configs
}

fn compare<R: RandomSource>(
    feature: &harpas_worldgen::CompiledFeature,
    lines: &mut std::iter::Peekable<std::str::Lines<'_>>,
    source: R,
    label: &str,
    test: usize,
) -> usize {
    let mut world = prepare(test);
    let mut random = WorldgenRandom::new(source);
    let placed = place_feature(&mut world, feature, &mut random, [15, y(test), -1]);
    assert_eq!(
        lines.next(),
        Some(format!("{label} {placed} {}", random.next_int()).as_str()),
        "{label}: result and random tail"
    );
    let expected: Vec<(i32, i32, i32, u32)> =
        std::iter::from_fn(|| lines.next_if(|line| line.starts_with("block ")))
            .map(|line| {
                let mut fields = line[6..]
                    .split_whitespace()
                    .map(|f| f.parse::<i64>().unwrap());
                (
                    fields.next().unwrap() as i32,
                    fields.next().unwrap() as i32,
                    fields.next().unwrap() as i32,
                    fields.next().unwrap() as u32,
                )
            })
            .collect();
    let mut actual = Vec::new();
    let dripstone = state("dripstone_block");
    for cx in -1..=2 {
        for cz in -2..=1 {
            for (section_y, section) in world.chunk(cx, cz).unwrap().sections().iter().enumerate() {
                if section
                    .block_states()
                    .palette()
                    .is_some_and(|p| !p.contains(&dripstone))
                {
                    continue;
                }
                for y in 0..16 {
                    for z in 0..16 {
                        for x in 0..16 {
                            if section.block_state(x, y, z) == dripstone
                                && initial(
                                    test,
                                    cx * 16 + x as i32,
                                    -64 + section_y as i32 * 16 + y as i32,
                                    cz * 16 + z as i32,
                                ) != dripstone
                            {
                                actual.push((
                                    cx * 16 + x as i32,
                                    -64 + section_y as i32 * 16 + y as i32,
                                    cz * 16 + z as i32,
                                    dripstone,
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    actual.sort_unstable();
    let missing: Vec<_> = expected
        .iter()
        .filter(|b| actual.binary_search(b).is_err())
        .take(12)
        .collect();
    let extra: Vec<_> = actual
        .iter()
        .filter(|b| expected.binary_search(b).is_err())
        .take(12)
        .collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{label}: blocks ({} actual, {} expected), missing {missing:?}, extra {extra:?}",
        actual.len(),
        expected.len()
    );
    for kind in HeightmapType::ALL
        .into_iter()
        .filter(|kind| !kind.is_worldgen())
    {
        let mut hash = 0xcbf29ce484222325_u64;
        for z in -32..32 {
            for x in -16..48 {
                hash = (hash ^ world.heightmap(kind, x, z) as u64).wrapping_mul(0x100000001b3);
            }
        }
        assert_eq!(
            lines.next(),
            Some(format!("height {hash:016x}").as_str()),
            "{label}: {kind:?}"
        );
    }
    actual.len()
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn large_dripstone_matches_vanilla() {
    let reference = reference::Reference::required();
    let mut data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let configs = configs();
    for (i, config) in configs.iter().enumerate() {
        data.configured_features.insert(
            format!("test:case_{i}"),
            harpas_worldgen::ConfiguredFeatureDef {
                feature_type: "minecraft:large_dripstone".into(),
                config: harpas_worldgen::FeatureConfigDef::Other(config.clone()),
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
            "LargeDripstoneProbe.java",
            include_str!("LargeDripstoneProbe.java"),
        ),
    ]);
    let config_file = reference.temp.0.join("configs.json");
    fs::write(&config_file, serde_json::to_string(&configs).unwrap()).unwrap();
    let output = reference.temp.0.join("large-dripstone.txt");
    let seeds = reference::sampled_seeds(&[0, 42, -4_172_144_997_902_289_642]);
    reference.java(
        "LargeDripstoneProbe",
        [
            output.display().to_string(),
            reference.server_jar.display().to_string(),
            config_file.display().to_string(),
            seeds
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(","),
        ],
    );
    let text = fs::read_to_string(output).unwrap();
    let mut lines = text.lines().peekable();
    for radius in [1, 2, 4, 19, 60] {
        for scale in [0.0, 0.4, 1.0, 2.0, 20.0] {
            for bluntness in [0.1, 0.7, 1.0, 10.0] {
                for d in 0..=radius * 4 {
                    let expected: f64 = lines
                        .next()
                        .unwrap()
                        .strip_prefix("profile ")
                        .unwrap()
                        .parse()
                        .unwrap();
                    let distance = (f64::from(d) / 4.0).max(bluntness) / f64::from(radius) * 0.384;
                    let actual = (scale
                        * (0.75 * distance.powf(4.0 / 3.0)
                            - distance.powf(2.0 / 3.0)
                            - distance.ln() / 3.0))
                        .max(0.0)
                        / 0.384
                        * f64::from(radius);
                    assert!(
                        (actual - expected).abs() <= 1e-10,
                        "profile {radius} {scale} {bluntness} {d}: {actual} != {expected}"
                    );
                }
            }
        }
    }
    let mut compiler = FeatureCompiler::new(&data);
    let mut placed = 0;
    for &seed in seeds {
        for legacy in [false, true] {
            for config in 0..configs.len() {
                let feature = compiler
                    .configured(&format!("test:case_{config}"))
                    .unwrap()
                    .unwrap();
                for test in 0..26 {
                    let label = format!("case {seed} {legacy} {config} {test}");
                    placed += if legacy {
                        compare(&feature, &mut lines, LegacyRandom::new(seed), &label, test)
                    } else {
                        compare(
                            &feature,
                            &mut lines,
                            XoroshiroRandom::new(seed),
                            &label,
                            test,
                        )
                    };
                }
            }
        }
    }
    assert!(lines.next().is_none());
    assert!(placed > 0);
    eprintln!(
        "Compared {} large dripstone placements and {placed} blocks",
        seeds.len() * 2 * configs.len() * 26
    );
}
