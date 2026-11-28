//! Village block piles compared with vanilla on controlled supports, obstructions and world limits.
mod reference;

use harpas_worldgen::BlockGetter;
use std::fs;

use harpas_core::block::block_states;
use harpas_core::random::{LegacyRandom, RandomSource, WorldgenRandom, XoroshiroRandom};
use harpas_worldgen::{
    FeatureCompiler, FeatureConfigDef, FeatureWorld, GeneratedChunk, HeightmapType,
    RegionFeatureWorld, WorldgenData, place_feature,
};
use serde_json::{Value, json};

const SEEDS: [i64; 3] = [0, 42, -4_172_144_997_902_289_642];

fn state(name: &str, properties: &[(&str, &str)]) -> u32 {
    block_states()
        .state(&format!("minecraft:{name}"), properties.iter().copied())
        .unwrap()
        .0
}

fn cases() -> Vec<Value> {
    let supports = [
        ("stone", vec![]),
        ("dirt_path", vec![]),
        ("air", vec![]),
        ("water", vec![]),
        ("lava", vec![]),
        ("glass", vec![]),
        ("oak_leaves", vec![]),
        ("oak_slab", vec![("type", "bottom")]),
        ("oak_slab", vec![("type", "top")]),
        ("oak_stairs", vec![]),
        ("snow", vec![]),
        ("snow", vec![("layers", "8")]),
        ("soul_sand", vec![]),
        ("mud", vec![]),
        ("farmland", vec![]),
        ("oak_fence", vec![]),
        ("honey_block", vec![]),
        ("chest", vec![]),
        ("barrier", vec![]),
        ("cactus", vec![]),
        ("scaffolding", vec![]),
        ("piston", vec![("extended", "true")]),
        ("hopper", vec![]),
        ("mangrove_roots", vec![]),
        ("powder_snow", vec![]),
        ("packed_ice", vec![]),
    ];
    (0..42)
        .map(|test| {
            let origin = [
                match test {
                    38 => -17,
                    39 => 29_999_984,
                    _ => 15,
                },
                match test {
                    32 => -60,
                    33 => -59,
                    34 => 319,
                    35 => 320,
                    36 => -64,
                    _ => 64,
                },
                -1,
            ];
            let mut cells = Vec::new();
            for dz in -4..=4 {
                for dx in -4..=4 {
                    let support = if test < supports.len() {
                        state(supports[test].0, &supports[test].1)
                    } else if test == 37 {
                        state(
                            if (dx + dz) % 2 == 0 {
                                "stone"
                            } else {
                                "dirt_path"
                            },
                            &[],
                        )
                    } else {
                        state("stone", &[])
                    };
                    cells.push(json!([dx, -1, dz, support]));
                    let target = match test {
                        26 => Some("water"),
                        27 => Some("lava"),
                        28 => Some("stone"),
                        29 => Some("short_grass"),
                        30 => Some("cave_air"),
                        31 => Some("void_air"),
                        40 if (dx + dz) % 2 == 0 => Some("stone"),
                        41 if dx == 0 || dz == 0 => Some("water"),
                        _ => None,
                    };
                    if let Some(target) = target {
                        cells.push(json!([dx, 0, dz, state(target, &[])]));
                    }
                }
            }
            // ProtoChunk reads an all-air section as normal air, even when its palette holds cave/void air.
            // Keep each touched section nonempty before taking the baseline, outside the pile's radius.
            if matches!(test, 30 | 31) {
                for dx in [-4, 4] {
                    for dz in [-4, 4] {
                        cells.push(json!([dx, 0, dz, state("stone", &[])]));
                    }
                }
            }
            json!({"origin": origin, "cells": cells})
        })
        .collect()
}

fn prepare(case: &Value) -> RegionFeatureWorld {
    let [x, y, z] = origin(case);
    let chunks = ((x - 4) >> 4..=(x + 4) >> 4).flat_map(|cx| {
        ((z - 4) >> 4..=(z + 4) >> 4).map(move |cz| {
            let mut chunk = GeneratedChunk::new(cx, cz, -64, 384, 1);
            chunk.freeze_worldgen_heightmaps();
            chunk
        })
    });
    let mut world = RegionFeatureWorld::new(chunks, 0).unwrap();
    for cell in case["cells"].as_array().unwrap() {
        world.set_block_state(
            x + cell[0].as_i64().unwrap() as i32,
            y + cell[1].as_i64().unwrap() as i32,
            z + cell[2].as_i64().unwrap() as i32,
            cell[3].as_u64().unwrap() as u32,
        );
    }
    world
}

fn origin(case: &Value) -> [i32; 3] {
    std::array::from_fn(|i| case["origin"][i].as_i64().unwrap() as i32)
}

fn blocks(world: &RegionFeatureWorld, [x, y, z]: [i32; 3]) -> Vec<u32> {
    (-1..=2)
        .flat_map(|dy| {
            (-4..=4).flat_map(move |dz| {
                (-4..=4).map(move |dx| world.block_state(x + dx, y + dy, z + dz))
            })
        })
        .collect()
}

fn compare<R: RandomSource>(
    feature: &harpas_worldgen::CompiledFeature,
    source: R,
    case: &Value,
    label: &str,
    lines: &mut std::iter::Peekable<std::str::Lines<'_>>,
) -> usize {
    let mut world = prepare(case);
    let origin = origin(case);
    let before = blocks(&world, origin);
    let mut random = WorldgenRandom::new(source);
    let mut count = 0;
    for pass in 0..2 {
        let placed = place_feature(&mut world, feature, &mut random, origin);
        assert_eq!(
            lines.next(),
            Some(format!("{label} {pass} {placed} {}", random.next_int()).as_str()),
            "{label}: result and random tail"
        );
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
        assert_eq!(actual, expected, "{label}, pass {pass}: blocks");
        count += actual.len();
        for kind in HeightmapType::ALL
            .into_iter()
            .filter(|kind| !kind.is_worldgen())
        {
            let mut hash = 0xcbf29ce484222325_u64;
            for dz in -4..=4 {
                for dx in -4..=4 {
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
    count
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn block_piles_match_vanilla() {
    let reference = reference::Reference::required();
    let mut data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let mut names: Vec<_> = ["hay", "ice", "melon", "pumpkin", "snow"]
        .map(|s| format!("minecraft:pile_{s}"))
        .to_vec();
    let mut extra = data.configured_features[&names[0]].clone();
    extra.config = FeatureConfigDef::Other(json!({"state_provider": {
        "type": "minecraft:rotated_block_provider",
        "state": {"Name": "minecraft:iron_chain", "Properties": {"axis": "z", "waterlogged": "true"}}
    }}));
    data.configured_features
        .insert("test:rotated".into(), extra);
    names.push("test:rotated".into());
    let configs: Vec<_> = names
        .iter()
        .map(|name| {
            let f = &data.configured_features[name];
            let FeatureConfigDef::Other(config) = &f.config else {
                panic!("pile config")
            };
            json!({"type": f.feature_type, "config": config})
        })
        .collect();
    let cases = cases();
    let fixtures = reference.temp.0.join("fixtures.json");
    fs::write(
        &fixtures,
        json!({"features": configs, "cases": cases}).to_string(),
    )
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
        ("BlockPileProbe.java", include_str!("BlockPileProbe.java")),
    ]);
    let output = reference.temp.0.join("piles.txt");
    reference.java(
        "BlockPileProbe",
        [
            output.display().to_string(),
            reference.server_jar.display().to_string(),
            fixtures.display().to_string(),
        ],
    );
    let text = reference::read_text(output);
    let mut lines = text.lines().peekable();
    let mut compiler = FeatureCompiler::new(&data);
    let mut changed = 0;
    for seed in SEEDS {
        for legacy in [false, true] {
            for (i, name) in names.iter().enumerate() {
                let feature = compiler.configured(name).unwrap().expect("pile compiles");
                for (test, case) in cases.iter().enumerate() {
                    let label = format!("case {seed} {legacy} {i} {test}");
                    changed += if legacy {
                        compare(&feature, LegacyRandom::new(seed), case, &label, &mut lines)
                    } else {
                        compare(
                            &feature,
                            XoroshiroRandom::new(seed),
                            case,
                            &label,
                            &mut lines,
                        )
                    };
                }
            }
        }
    }
    assert!(lines.next().is_none());
    assert!(changed > 10_000, "fixtures must place blocks");
    eprintln!(
        "Compared 3,024 block pile placements, including repeated placement, live heightmaps and random tails"
    );
}
