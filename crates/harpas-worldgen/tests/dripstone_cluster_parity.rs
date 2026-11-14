//! Dripstone cluster blocks, live heightmaps, results and random consumption compared with vanilla.
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
    if test == 26 {
        return state("air");
    }
    if test == 27 && ((f - 12..=f).contains(&py) || (c..=c + 12).contains(&py)) {
        return state("dripstone_block");
    }
    if test == 28 && py == f && x == 16 {
        return state("dirt");
    }
    if test == 29 && py < f {
        return state("air");
    }
    if test == 30 && py > f && py < c {
        return block_states()
            .state("minecraft:water", [("level", "3")])
            .unwrap()
            .0;
    }
    if test == 31 && py == f - 1 {
        return block_states()
            .state("minecraft:oak_slab", [("waterlogged", "true")])
            .unwrap()
            .0;
    }
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
    let uniform = |min: f32, max: f32| json!({"type":"minecraft:uniform", "min_inclusive": min, "max_exclusive": max});
    let base = json!({"floor_to_ceiling_search_range": 30, "height": 24, "radius": 3,
        "max_stalagmite_stalactite_height_diff": 3, "height_deviation": 3,
        "dripstone_block_layer_thickness": 2, "density": 1, "wetness": 0.5,
        "chance_of_dripstone_column_at_max_distance_from_center": 0.1,
        "max_distance_from_edge_affecting_chance_of_dripstone_column": 3,
        "max_distance_from_center_affecting_height_bias": 8});
    let mut configs = vec![base; 8];
    configs[0]["height"] =
        json!({"type":"minecraft:uniform", "min_inclusive":1, "max_inclusive":30});
    configs[0]["radius"] =
        json!({"type":"minecraft:uniform", "min_inclusive":1, "max_inclusive":5});
    configs[0]["density"] = uniform(0.3, 0.9);
    configs[0]["wetness"] = uniform(0.1, 0.7);
    configs[0]["dripstone_block_layer_thickness"] =
        json!({"type":"minecraft:uniform", "min_inclusive":0, "max_inclusive":4});
    configs[1]["wetness"] = json!(0);
    configs[2]["wetness"] = json!(2);
    configs[2]["density"] = json!(2);
    configs[2]["chance_of_dripstone_column_at_max_distance_from_center"] = json!(1);
    configs[3]["density"] = json!(0);
    configs[3]["wetness"] = json!(0);
    configs[3]["max_stalagmite_stalactite_height_diff"] = json!(0);
    configs[4]["dripstone_block_layer_thickness"] = json!(0);
    configs[4]["height"] = json!(128);
    configs[4]["height_deviation"] = json!(64);
    configs[4]["max_stalagmite_stalactite_height_diff"] = json!(64);
    configs[5]["density"] =
        json!({"type":"minecraft:trapezoid", "min":0.2, "max":1.5, "plateau":0.5});
    configs[5]["wetness"] =
        json!({"type":"minecraft:clamped_normal", "mean":0.5, "deviation":0.2, "min":0, "max":1});
    configs[5]["radius"] =
        json!({"type":"minecraft:biased_to_bottom", "min_inclusive":1, "max_inclusive":4});
    configs[6]["floor_to_ceiling_search_range"] = json!(1);
    configs[7]["floor_to_ceiling_search_range"] = json!(512);
    configs[7]["height"] = json!(1);
    configs[7]["radius"] = json!(1);
    configs[7]["max_distance_from_center_affecting_height_bias"] = json!(1);
    configs[7]["max_distance_from_edge_affecting_chance_of_dripstone_column"] = json!(1);
    configs
}

fn compare<R: RandomSource>(
    feature: &harpas_worldgen::CompiledFeature,
    lines: &mut std::iter::Peekable<std::str::Lines<'_>>,
    source: R,
    label: &str,
    test: usize,
) -> usize {
    let before = prepare(test);
    let mut world = prepare(test);
    let mut random = WorldgenRandom::new(source);
    let mut total = 0;
    for pass in 0..2 {
        let placed = place_feature(&mut world, feature, &mut random, [15, y(test), -1]);
        assert_eq!(
            lines.next(),
            Some(format!("{label} {pass} {placed} {}", random.next_int()).as_str()),
            "{label}, pass {pass}: result and random tail"
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
        for x in 10..=20 {
            for py in -64..320 {
                for z in -6..=4 {
                    let state = world.block_state(x, py, z);
                    if state != before.block_state(x, py, z) {
                        actual.push((x, py, z, state));
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
            "{label}, pass {pass}: blocks ({} actual, {} expected), missing {missing:?}, extra {extra:?}",
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
                "{label}, pass {pass}: {kind:?}"
            );
        }
        total += actual.len();
    }
    total
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn dripstone_cluster_matches_vanilla() {
    let reference = reference::Reference::required();
    let mut data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let configs = configs();
    for (i, config) in configs.iter().enumerate() {
        data.configured_features.insert(
            format!("test:case_{i}"),
            harpas_worldgen::ConfiguredFeatureDef {
                feature_type: "minecraft:dripstone_cluster".into(),
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
            "DripstoneClusterProbe.java",
            include_str!("DripstoneClusterProbe.java"),
        ),
    ]);
    let config_file = reference.temp.0.join("configs.json");
    fs::write(&config_file, serde_json::to_string(&configs).unwrap()).unwrap();
    let output = reference.temp.0.join("dripstone-cluster.txt");
    let seeds = reference::sampled_seeds(&[0, 42, -4_172_144_997_902_289_642]);
    reference.java(
        "DripstoneClusterProbe",
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
    let mut compiler = FeatureCompiler::new(&data);
    let mut placed = 0;
    for &seed in seeds {
        for legacy in [false, true] {
            for config in 0..configs.len() {
                let feature = compiler
                    .configured(&format!("test:case_{config}"))
                    .unwrap()
                    .unwrap();
                for test in 0..32 {
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
        "Compared {} dripstone cluster placements and {placed} blocks",
        seeds.len() * 2 * configs.len() * 32 * 2
    );
}
