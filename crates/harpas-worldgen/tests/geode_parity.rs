//! Geode blocks, live heightmaps, results and random consumption compared with vanilla's own feature.
mod reference;

use std::fs;

use harpas_core::block::block_states;
use harpas_core::random::{LegacyRandom, RandomSource, WorldgenRandom, XoroshiroRandom};
use harpas_worldgen::{
    FeatureCompiler, FeatureConfigDef, FeatureWorld, GeneratedChunk, HeightmapType,
    RegionFeatureWorld, WorldgenData, place_feature,
};
use serde_json::{Map, Value, json};

const SEEDS: [i64; 3] = [0, 42, -4_172_144_997_902_289_642];

fn origin(test: usize) -> [i32; 3] {
    [
        match test {
            14 => -17,
            15 => 29_999_980,
            _ => 15,
        },
        match test {
            8 => -64,
            9 => -60,
            10 => 313,
            11 => 320,
            _ => 64,
        },
        -1,
    ]
}

fn state(name: &str) -> u32 {
    static STATES: std::sync::OnceLock<std::collections::HashMap<&str, u32>> =
        std::sync::OnceLock::new();
    STATES.get_or_init(|| {
        [
            "air", "cave_air", "stone", "water", "lava", "bedrock", "chest",
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

fn initial(test: usize, dx: i32, dy: i32, dz: i32) -> u32 {
    let hash = (dx * 3 + dy * 7 + dz * 11).rem_euclid(13);
    state(match test {
        1 => "air",
        2 => "water",
        3 => "lava",
        4 => "bedrock",
        5 if hash < 3 => "bedrock",
        5 if hash == 3 => "chest",
        6 if hash < 2 => "lava",
        7 if dx == dy && dy == dz => "cave_air",
        12 if hash < 3 => "water",
        12 if hash == 3 => "air",
        13 if dy > 4 => "water",
        _ => "stone",
    })
}

fn prepare(test: usize, seed: i64) -> RegionFeatureWorld {
    let [x, y, z] = origin(test);
    let chunks = ((x - 18) >> 4..=(x + 18) >> 4).flat_map(|cx| {
        ((z - 18) >> 4..=(z + 18) >> 4).map(move |cz| {
            let mut chunk = GeneratedChunk::new(cx, cz, -64, 384, 1);
            chunk.freeze_worldgen_heightmaps();
            chunk
        })
    });
    let mut world = RegionFeatureWorld::new(chunks, 0).unwrap();
    world.start_region(seed ^ 0x0123_4567_89ab_cdef, x >> 4, z >> 4);
    for dy in -18..=18 {
        for dz in -18..=18 {
            for dx in -18..=18 {
                world.set_block_state(x + dx, y + dy, z + dz, initial(test, dx, dy, dz));
            }
        }
    }
    world
}

fn blocks(world: &RegionFeatureWorld, [x, y, z]: [i32; 3]) -> Vec<u32> {
    (-17..=17)
        .flat_map(|dy| {
            (-17..=17).flat_map(move |dz| {
                (-17..=17).map(move |dx| world.block_state(x + dx, y + dy, z + dz))
            })
        })
        .collect()
}

fn provider(block: &str) -> Value {
    json!({"type": "minecraft:simple_state_provider", "state": {"Name": format!("minecraft:{block}")}})
}

fn configurations(base: &Value) -> Map<String, Value> {
    (0..10).map(|i| {
        let mut config = base.clone();
        match i {
            1 => {
                config["crack"]["generate_crack_chance"] = json!(0.0);
                config["use_alternate_layer0_chance"] = json!(0.0);
                config["placements_require_layer0_alternate"] = json!(false);
                config["use_potential_placements_chance"] = json!(1.0);
            }
            2 => {
                config["crack"] = json!({"generate_crack_chance": 1.0, "base_crack_size": 1.0, "crack_point_offset": 0});
                config["distribution_points"] = json!(2);
                config["outer_wall_distance"] = json!(3);
                config["point_offset"] = json!(0);
                config["noise_multiplier"] = json!(0.0);
                config["layers"] = json!({"filling": 0.8, "inner_layer": 1.4, "middle_layer": 2.2, "outer_layer": 3.0});
            }
            3..=5 => {
                config["crack"]["generate_crack_chance"] = json!(0.0);
                config["use_alternate_layer0_chance"] = json!(1.0);
                config["use_potential_placements_chance"] = json!(1.0);
                config["blocks"]["filling_provider"] = provider("water");
                if i > 3 { config["blocks"]["filling_provider"]["state"]["Properties"] = json!({"level": if i == 4 { "1" } else { "8" }}); }
            }
            6 => {
                config["blocks"]["cannot_replace"] = json!("#minecraft:base_stone_overworld");
                config["use_alternate_layer0_chance"] = json!(1.0);
                config["use_potential_placements_chance"] = json!(1.0);
            }
            7 => {
                let object = config.as_object_mut().unwrap();
                for key in ["outer_wall_distance", "distribution_points", "point_offset", "min_gen_offset", "max_gen_offset", "noise_multiplier", "use_potential_placements_chance", "use_alternate_layer0_chance", "placements_require_layer0_alternate"] { object.remove(key); }
                config["layers"] = json!({"filling": -1, "outer_layer": "invalid"});
                config["crack"] = json!({"base_crack_size": 99, "crack_point_offset": -1});
            }
            8 => {
                config["blocks"]["filling_provider"] = json!({"type": "minecraft:weighted_state_provider", "entries": [
                    {"data": {"Name": "minecraft:air"}, "weight": 2}, {"data": {"Name": "minecraft:water"}, "weight": 1}]});
                config["use_alternate_layer0_chance"] = json!(0.5);
                config["noise_multiplier"] = json!(0.2);
                config["invalid_blocks_threshold"] = json!(20);
            }
            9 => {
                config["min_gen_offset"] = json!(8);
                config["max_gen_offset"] = json!(-8);
                config["distribution_points"] = json!(3);
                config["invalid_blocks_threshold"] = json!(-1);
                config["crack"]["generate_crack_chance"] = json!(0.0);
                config["use_alternate_layer0_chance"] = json!(0.0);
                config["placements_require_layer0_alternate"] = json!(false);
                config["use_potential_placements_chance"] = json!(1.0);
                config["blocks"]["inner_placements"] = json!([
                    {"Name": "minecraft:glowstone"},
                    {"Name": "minecraft:oak_stairs", "Properties": {"facing": "west"}},
                    {"Name": "minecraft:hopper", "Properties": {"facing": "north"}}
                ]);
            }
            _ => {}
        }
        (format!("geode_{i}"), json!({"type": "minecraft:geode", "config": config}))
    }).collect()
}

#[derive(Default)]
struct Coverage {
    placed: usize,
    rejected: usize,
    wet: bool,
    directions: std::collections::BTreeSet<String>,
}

fn compare<R: RandomSource>(
    feature: &harpas_worldgen::CompiledFeature,
    source: R,
    seed: i64,
    test: usize,
    label: &str,
    lines: &mut std::iter::Peekable<std::str::Lines<'_>>,
    coverage: &mut Coverage,
) {
    let mut world = prepare(test, seed);
    let origin = origin(test);
    let before = blocks(&world, origin);
    let mut random = WorldgenRandom::new(source);
    for pass in 0..2 {
        let result = place_feature(&mut world, feature, &mut random, origin);
        assert_eq!(
            lines.next(),
            Some(format!("{label} {pass} {result} {}", random.next_int()).as_str()),
            "{label}, pass {pass}: result and random tail"
        );
        if result {
            coverage.placed += 1;
        } else {
            coverage.rejected += 1;
        }
        let expected: Vec<(usize, u32)> =
            std::iter::from_fn(|| lines.next_if(|line| line.starts_with("block ")))
                .map(|line| {
                    let (i, s) = line[6..].split_once(' ').unwrap();
                    (i.parse().unwrap(), s.parse().unwrap())
                })
                .collect();
        let actual: Vec<_> = blocks(&world, origin)
            .into_iter()
            .enumerate()
            .filter(|&(i, s)| s != before[i])
            .collect();
        for &(_, state) in &expected {
            if let Some((name, properties)) = block_states().properties(state)
                && name.contains("amethyst")
            {
                coverage.wet |= properties.contains(&("waterlogged", "true"));
                for (key, value) in properties {
                    if key == "facing" {
                        coverage.directions.insert(value.into());
                    }
                }
            }
        }
        let missing: Vec<_> = expected
            .iter()
            .filter(|b| actual.binary_search(b).is_err())
            .take(10)
            .collect();
        let extra: Vec<_> = actual
            .iter()
            .filter(|b| expected.binary_search(b).is_err())
            .take(10)
            .collect();
        assert!(
            actual == expected,
            "{label}, pass {pass}: blocks (Harpas {}, vanilla {})\nmissing {missing:?}\nextra {extra:?}",
            actual.len(),
            expected.len()
        );
        for kind in HeightmapType::ALL
            .into_iter()
            .filter(|kind| !kind.is_worldgen())
        {
            let mut hash = 0xcbf29ce484222325_u64;
            for dz in -18..=18 {
                for dx in -18..=18 {
                    hash = (hash ^ world.heightmap(kind, origin[0] + dx, origin[2] + dz) as u64)
                        .wrapping_mul(0x100000001b3);
                }
            }
            assert_eq!(
                lines.next(),
                Some(format!("height {hash:016x}").as_str()),
                "{label}, pass {pass}: {kind:?}"
            );
        }
    }
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn geodes_match_vanilla() {
    let reference = reference::Reference::required();
    let mut data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let FeatureConfigDef::Other(base) =
        &data.configured_features["minecraft:amethyst_geode"].config
    else {
        panic!("geode JSON")
    };
    let configs = configurations(base);
    let extra = WorldgenData::from_json_entries(configs.iter().map(|(name, value)| {
        (
            format!("data/test/worldgen/configured_feature/{name}.json"),
            value.to_string(),
        )
    }))
    .unwrap();
    data.configured_features.extend(extra.configured_features);
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
        ("GeodeProbe.java", include_str!("GeodeProbe.java")),
    ]);
    let input = reference.temp.0.join("geodes.json");
    fs::write(&input, serde_json::to_string(&configs).unwrap()).unwrap();
    let output = reference.temp.0.join("geodes.txt");
    let seeds = reference::sampled_seeds(&SEEDS);
    reference.java(
        "GeodeProbe",
        [
            output.display().to_string(),
            reference.server_jar.display().to_string(),
            seeds
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(","),
            input.display().to_string(),
        ],
    );
    let text = fs::read_to_string(output).unwrap();
    let mut lines = text.lines().peekable();
    let mut compiler = FeatureCompiler::new(&data);
    let mut coverage = Coverage::default();
    for &seed in seeds {
        for legacy in [false, true] {
            for name in configs.keys() {
                let feature = compiler
                    .configured(&format!("test:{name}"))
                    .unwrap()
                    .unwrap();
                for test in 0..16 {
                    let label = format!("case {seed} {legacy} {name} {test}");
                    if legacy {
                        compare(
                            &feature,
                            LegacyRandom::new(seed),
                            seed,
                            test,
                            &label,
                            &mut lines,
                            &mut coverage,
                        );
                    } else {
                        compare(
                            &feature,
                            XoroshiroRandom::new(seed),
                            seed,
                            test,
                            &label,
                            &mut lines,
                            &mut coverage,
                        );
                    }
                }
            }
        }
    }
    assert!(lines.next().is_none());
    assert!(coverage.placed > 0 && coverage.rejected > 0);
    assert!(coverage.wet, "the samples must place waterlogged crystals");
    assert_eq!(
        coverage.directions.len(),
        6,
        "crystals must cover all directions"
    );
    eprintln!(
        "Compared {} geode placements and replays ({} placed, {} rejected), ten configurations, wet crystals and all six directions",
        coverage.placed + coverage.rejected,
        coverage.placed,
        coverage.rejected
    );
}
