//! Lush cave clay pools and dripleaves: blocks, heightmaps, results, random tails, survival and shapes.
mod reference;

use std::fs;

use harpas_core::block::block_states;
use harpas_core::random::{LegacyRandom, RandomSource, WorldgenRandom, XoroshiroRandom};
use harpas_worldgen::shape_update::{Direction, ShapeUpdates};
use harpas_worldgen::{
    CompiledFeature, FeatureCompiler, FeatureWorld, GeneratedChunk, HeightmapType,
    RegionFeatureWorld, WorldgenData, compile_block_survival, place_feature,
};
use serde_json::json;

const FEATURES: [&str; 4] = [
    "clay_with_dripleaves",
    "clay_pool_with_dripleaves",
    "lush_caves_clay",
    "dripleaf",
];

fn state(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}

fn property(name: &str, key: &str, value: &str) -> u32 {
    block_states()
        .state(&format!("minecraft:{name}"), [(key, value)])
        .unwrap()
        .0
}

fn y(test: usize) -> i32 {
    match test {
        24 => -64,
        25 => -63,
        26 => -60,
        27 => 317,
        28 => 319,
        29 => 320,
        _ => 64,
    }
}

fn initial(test: usize, dx: i32, dy: i32, dz: i32) -> u32 {
    if test == 33 {
        return initial(0, dx, -dy, dz);
    }
    let ground = match test {
        13 => (dx * 3 + dz * 7).rem_euclid(7) - 4,
        14 => -4,
        15 => 2,
        16 => -7,
        _ => -1,
    };
    if dy <= ground {
        if test == 5
            || test == 8 && dy == -1 && (dx + dz).rem_euclid(4) == 0
            || test == 9 && dy < -1
            || test == 10 && dx == 0
        {
            return state("air");
        }
        if test == 11 {
            return state("glass");
        }
        if test == 12 && dy == ground {
            return property("oak_slab", "type", "top");
        }
        if test == 17 && dy < -2 {
            return state("water");
        }
        if test == 18 && dy < -2 {
            return state("lava");
        }
        return state(match test {
            1 | 6 | 7 | 19 | 20 | 21 | 22 | 23 => "clay",
            2 => "moss_block",
            3 | 30 | 31 => "dirt",
            4 => "grass_block",
            _ => "stone",
        });
    }
    if dy == 0 || dy == 1 && test == 7 {
        match test {
            6 | 7 | 30 => return state("water"),
            19 | 31 => return property("water", "level", "1"),
            20 => return state("lava"),
            21 => return state("small_dripleaf"),
            22 => return state("big_dripleaf"),
            23 => return state("big_dripleaf_stem"),
            _ => {}
        }
    }
    if test == 29 && dy >= 0 || test == 32 && dy == 1 {
        return state("stone");
    }
    state("air")
}

fn world() -> RegionFeatureWorld {
    let chunks = (0..=1).flat_map(|cx| {
        (-1..=0).map(move |cz| {
            let mut chunk = GeneratedChunk::new(cx, cz, -64, 384, 1);
            chunk.freeze_worldgen_heightmaps();
            chunk
        })
    });
    RegionFeatureWorld::new(chunks, 0).unwrap()
}

fn prepare(test: usize) -> RegionFeatureWorld {
    let mut world = world();
    for dy in -9..=7 {
        for dz in -12..=12 {
            for dx in -12..=12 {
                world.set_block_state(15 + dx, y(test) + dy, -1 + dz, initial(test, dx, dy, dz));
            }
        }
    }
    world
}

fn blocks(world: &RegionFeatureWorld, y: i32) -> Vec<u32> {
    (-9..=7)
        .flat_map(|dy| {
            (-12..=12).flat_map(move |dz| {
                (-12..=12).map(move |dx| world.block_state(15 + dx, y + dy, -1 + dz))
            })
        })
        .collect()
}

fn compare<R: RandomSource>(
    feature: &CompiledFeature,
    lines: &mut std::iter::Peekable<std::str::Lines<'_>>,
    source: R,
    label: &str,
    test: usize,
) -> usize {
    let mut world = prepare(test);
    let before = blocks(&world, y(test));
    let mut random = WorldgenRandom::new(source);
    let mut total = 0;
    for pass in 0..2 {
        let placed = place_feature(&mut world, feature, &mut random, [15, y(test), -1]);
        assert_eq!(
            lines.next(),
            Some(format!("{label} {pass} {placed} {}", random.next_int()).as_str()),
            "{label}, pass {pass}: result and random tail"
        );
        let expected: Vec<(usize, u32)> =
            std::iter::from_fn(|| lines.next_if(|line| line.starts_with("block ")))
                .map(|line| {
                    let (i, s) = line[6..].split_once(' ').unwrap();
                    (i.parse().unwrap(), s.parse().unwrap())
                })
                .collect();
        let actual: Vec<_> = blocks(&world, y(test))
            .into_iter()
            .enumerate()
            .filter(|&(i, s)| s != before[i])
            .collect();
        let missing: Vec<_> = expected
            .iter()
            .filter(|block| actual.binary_search(block).is_err())
            .take(10)
            .collect();
        let extra: Vec<_> = actual
            .iter()
            .filter(|block| expected.binary_search(block).is_err())
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
            for z in -16..16 {
                for x in 0..32 {
                    hash = (hash ^ world.heightmap(kind, x, z) as u64).wrapping_mul(0x100000001b3);
                }
            }
            assert_eq!(
                lines.next(),
                Some(format!("height {hash:016x}").as_str()),
                "{label}, pass {pass}: {kind:?}"
            );
        }
        total += usize::from(placed);
    }
    total
}

fn compare_behavior(data: &WorldgenData, lines: &mut std::iter::Peekable<std::str::Lines<'_>>) {
    let mut world = world();
    let shapes = ShapeUpdates::new(data).unwrap();
    let mut plants = vec![
        state("small_dripleaf"),
        property("small_dripleaf", "half", "upper"),
        state("big_dripleaf"),
        state("big_dripleaf_stem"),
    ];
    for plant in plants.clone() {
        let (name, properties) = block_states().properties(plant).unwrap();
        let wet = properties.into_iter().map(|(key, value)| match key {
            "waterlogged" => (key, "true"),
            "facing" => (key, "east"),
            "tilt" => (key, "full"),
            _ => (key, value),
        });
        plants.push(block_states().state(name, wet).unwrap().0);
    }
    let rules: Vec<_> = plants
        .iter()
        .map(|&plant| {
            compile_block_survival("test", block_states().properties(plant).unwrap().0, data)
                .unwrap()
                .unwrap()
        })
        .collect();
    for neighbor in 0..block_states().len() {
        let mut actual = vec![neighbor];
        for (&plant, rule) in plants.iter().zip(&rules) {
            let mut mask = 0;
            for context in 0..4 {
                world.set_block_state(
                    15,
                    64,
                    -1,
                    if context == 1 { state("water") } else { plant },
                );
                world.set_block_state(
                    15,
                    63,
                    -1,
                    if context == 2 {
                        state("clay")
                    } else {
                        neighbor
                    },
                );
                world.set_block_state(
                    15,
                    65,
                    -1,
                    if context == 2 {
                        neighbor
                    } else if context == 3 {
                        state("big_dripleaf")
                    } else {
                        state("air")
                    },
                );
                if rule.test(&world, [15, 64, -1], plant) {
                    mask |= 1 << context;
                }
            }
            actual.push(mask);
            for direction in Direction::ALL {
                world.set_block_state(15, 64, -1, plant);
                world.set_block_state(15, 63, -1, state("clay"));
                world.set_block_state(15, 65, -1, state("air"));
                let [x, y, z] = direction.step([15, 64, -1]);
                world.set_block_state(x, y, z, neighbor);
                actual.push(shapes.update(&world, plant, [15, 64, -1], direction, neighbor));
                world.set_block_state(x, y, z, state("air"));
            }
        }
        let line = lines.next().unwrap().strip_prefix("behavior ").unwrap();
        let expected: Vec<u32> = line.split(' ').map(|n| n.parse().unwrap()).collect();
        assert_eq!(
            actual,
            expected,
            "survival and shape updates against {:?}",
            block_states().properties(neighbor)
        );
    }
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn lush_vegetation_matches_vanilla() {
    let reference = reference::Reference::required();
    let mut data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let mut extra = serde_json::Map::new();
    for (name, surface, ground, vegetation) in [
        (
            "slab_pool",
            "floor",
            json!({"Name": "minecraft:oak_slab", "Properties": {"type": "bottom"}}),
            0.0,
        ),
        (
            "ceiling_pool",
            "ceiling",
            json!({"Name": "minecraft:clay"}),
            1.0,
        ),
        (
            "dry_ceiling",
            "ceiling",
            json!({"Name": "minecraft:clay"}),
            1.0,
        ),
        (
            "empty_pool",
            "floor",
            json!({"Name": "minecraft:clay"}),
            0.0,
        ),
    ] {
        let kind = if name == "dry_ceiling" {
            "minecraft:vegetation_patch"
        } else {
            "minecraft:waterlogged_vegetation_patch"
        };
        let config = json!({
            "replaceable": "#minecraft:lush_ground_replaceable",
            "ground_state": {"type": "minecraft:simple_state_provider", "state": ground},
            "vegetation_feature": {"feature": {"type": "minecraft:simple_block", "config": {"to_place": {"type": "minecraft:simple_state_provider", "state": {"Name": "minecraft:melon"}}}}, "placement": []},
            "surface": surface, "depth": 1, "extra_bottom_block_chance": 0.0,
            "vertical_range": 3, "vegetation_chance": vegetation, "xz_radius": 2, "extra_edge_column_chance": 1.0
        });
        data.configured_features.insert(
            format!("test:{name}"),
            harpas_worldgen::ConfiguredFeatureDef {
                feature_type: kind.to_string(),
                config: harpas_worldgen::FeatureConfigDef::Other(config.clone()),
            },
        );
        extra.insert(name.to_string(), json!({"type": kind, "config": config}));
    }
    let mut compiler = FeatureCompiler::new(&data);
    let names: Vec<_> = FEATURES
        .iter()
        .map(|name| (name.to_string(), format!("minecraft:{name}")))
        .chain(
            extra
                .keys()
                .map(|name| (name.clone(), format!("test:{name}"))),
        )
        .collect();
    let features: Vec<_> = names
        .iter()
        .map(|(_, name)| compiler.configured(name).unwrap().unwrap())
        .collect();
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
            "LushVegetationProbe.java",
            include_str!("LushVegetationProbe.java"),
        ),
    ]);
    let output = reference.temp.0.join("lush-vegetation.txt");
    let configs = reference.temp.0.join("custom-features.json");
    fs::write(&configs, serde_json::to_string(&extra).unwrap()).unwrap();
    let seeds = reference::sampled_seeds(&[0, 42, -4_172_144_997_902_289_642]);
    reference.java(
        "LushVegetationProbe",
        [
            output.display().to_string(),
            reference.server_jar.display().to_string(),
            seeds
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(","),
            configs.display().to_string(),
        ],
    );
    let text = fs::read_to_string(output).unwrap();
    let mut lines = text.lines().peekable();
    let mut placed = 0;
    for &seed in seeds {
        for legacy in [false, true] {
            for ((name, _), feature) in names.iter().zip(&features) {
                for test in 0..34 {
                    let label = format!("case {seed} {legacy} {name} {test}");
                    placed += if legacy {
                        compare(feature, &mut lines, LegacyRandom::new(seed), &label, test)
                    } else {
                        compare(
                            feature,
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
    compare_behavior(&data, &mut lines);
    assert!(lines.next().is_none());
    assert!(placed > 0);
    eprintln!(
        "Compared {} lush vegetation placements and replays ({placed} successful), and dripleaf behavior against {} states",
        seeds.len() * 2 * names.len() * 34 * 2,
        block_states().len()
    );
}
