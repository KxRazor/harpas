//! Bamboo's blocks, result, random consumption and survival, compared with the owner's vanilla server.
mod reference;

use std::fs;

use harpas_core::block::block_states;
use harpas_core::random::{RandomSource, WorldgenRandom, XoroshiroRandom};
use harpas_worldgen::shape_update::{Direction, ShapeUpdates};
use harpas_worldgen::{
    FeatureCompiler, FeatureWorld, GeneratedChunk, RegionFeatureWorld, WorldgenData,
    compile_block_survival, place_feature,
};
use reference::Reference;

fn state(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}

fn prepare(test: usize, y: i32) -> RegionFeatureWorld {
    let chunks =
        (-1..=1).flat_map(|x| (-2..=0).map(move |z| GeneratedChunk::new(x, z, -64, 384, 0)));
    let mut world = RegionFeatureWorld::new(chunks, 0).unwrap();
    let ground = [
        "dirt",
        "mud",
        "sand",
        "stone",
        "grass_block",
        "mycelium",
        "moss_block",
        "gravel",
    ];
    for z in -6i32..=4 {
        for x in 10i32..=20 {
            let top = y - 1 - (x + z).rem_euclid(3);
            let kind = (x * 3 + z * 7).rem_euclid(ground.len() as i32) as usize;
            world.set_block_state(x, top, z, state(ground[kind]));
            if kind == 0 {
                world.set_block_state(x, top + 1, z, state("short_grass"));
            }
        }
    }
    let support = match test {
        18 => "stone",
        19 => "bamboo",
        20 => "bamboo_sapling",
        _ => "dirt",
    };
    world.set_block_state(15, y - 1, -1, state(support));
    let at = match test {
        21 => "water",
        22 => "short_grass",
        23 => "cave_air",
        24 => "void_air",
        _ => "air",
    };
    world.set_block_state(15, y, -1, state(at));
    if test < 17 {
        world.set_block_state(15, y + test as i32, -1, state("stone"));
    }
    world
}

fn blocks(world: &RegionFeatureWorld, y: i32) -> Vec<u32> {
    ((y - 3).max(-64)..(y + 17).min(320))
        .flat_map(|py| {
            (-6..=4).flat_map(move |z| (10..=20).map(move |x| world.block_state(x, py, z)))
        })
        .collect()
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn bamboo_matches_vanilla() {
    let reference = Reference::required();
    let mut data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    for (name, probability) in [("test:none", 0.0), ("test:some", 0.2), ("test:all", 1.0)] {
        let mut feature = data.configured_features["minecraft:bamboo_some_podzol"].clone();
        feature.config = harpas_worldgen::FeatureConfigDef::Other(
            serde_json::json!({"probability": probability}),
        );
        data.configured_features.insert(name.to_string(), feature);
    }
    let mut compiler = FeatureCompiler::new(&data);
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
            "BambooFeatureProbe.java",
            include_str!("BambooFeatureProbe.java"),
        ),
    ]);
    let output = reference.temp.0.join("bamboo.txt");
    reference.java(
        "BambooFeatureProbe",
        [
            output.display().to_string(),
            reference.server_jar.display().to_string(),
        ],
    );
    let text = fs::read_to_string(output).unwrap();
    let mut lines = text.lines().peekable();
    for seed in [0, 42, -4_172_144_997_902_289_642] {
        for (name, probability) in [
            ("test:none", "0.0"),
            ("test:some", "0.2"),
            ("test:all", "1.0"),
        ] {
            let feature = compiler.configured(name).unwrap().unwrap();
            for test in 0..27 {
                let y = match test {
                    25 => 319,
                    26 => -63,
                    _ => 64,
                };
                let mut world = prepare(test, y);
                let before = blocks(&world, y);
                let mut random = WorldgenRandom::new(XoroshiroRandom::new(seed));
                let placed = place_feature(&mut world, &feature, &mut random, [15, y, -1]);
                let case = format!("case {seed} {probability} {test}");
                assert_eq!(
                    lines.next(),
                    Some(format!("{case} {placed} {}", random.next_int()).as_str())
                );
                let expected: Vec<(usize, u32)> = std::iter::from_fn(|| {
                    lines
                        .next_if(|line| !line.starts_with("case ") && !line.starts_with("support "))
                })
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
                assert_eq!(actual, expected, "{case}");
            }
        }
    }
    let mut world = prepare(17, 64);
    let bamboo = compile_block_survival("test", "minecraft:bamboo", &data)
        .unwrap()
        .unwrap();
    let sapling = compile_block_survival("test", "minecraft:bamboo_sapling", &data)
        .unwrap()
        .unwrap();
    for id in 0..block_states().len() {
        world.set_block_state(15, 63, -1, id);
        let a = bamboo.test(&world, [15, 64, -1], state("bamboo"));
        let b = sapling.test(&world, [15, 64, -1], state("bamboo_sapling"));
        assert_eq!(lines.next(), Some(format!("support {id} {a} {b}").as_str()));
    }
    let updates = ShapeUpdates::new(&data).unwrap();
    let stalks = (0..block_states().len())
        .filter(|&id| {
            block_states()
                .properties(id)
                .is_some_and(|(name, _)| name == "minecraft:bamboo")
        })
        .chain([state("bamboo_sapling")]);
    for stalk in stalks {
        for support in [state("air"), state("dirt")] {
            for neighbor in [
                state("air"),
                state("bamboo"),
                block_states()
                    .state("minecraft:bamboo", [("age", "1")])
                    .unwrap()
                    .0,
                state("bamboo_sapling"),
                state("stone"),
            ] {
                for (i, direction) in Direction::ALL.into_iter().enumerate() {
                    for clear in Direction::ALL {
                        let [x, y, z] = clear.step([15, 64, -1]);
                        world.set_block_state(x, y, z, state("air"));
                    }
                    world.set_block_state(15, 63, -1, support);
                    world.set_block_state(15, 64, -1, stalk);
                    let [x, y, z] = direction.step([15, 64, -1]);
                    world.set_block_state(x, y, z, neighbor);
                    let actual = updates.update(&world, stalk, [15, 64, -1], direction, neighbor);
                    assert_eq!(
                        lines.next(),
                        Some(format!("shape {stalk} {support} {neighbor} {i} {actual}").as_str())
                    );
                }
            }
        }
    }
    assert!(lines.next().is_none());
}
