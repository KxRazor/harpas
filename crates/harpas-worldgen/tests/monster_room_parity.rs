//! Monster room blocks, live heightmaps, block entity NBT, results and random tails against vanilla.
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

fn y(test: usize) -> i32 {
    match test {
        33 => -63,
        34 => -64,
        35 => 315,
        36 => 316,
        _ => 64,
    }
}

fn initial(test: usize, dx: i32, dy: i32, dz: i32) -> u32 {
    let materials = [
        "glass",
        "oak_leaves",
        "ice",
        "oak_slab",
        "cobweb",
        "soul_sand",
        "end_portal_frame",
        "dirt",
    ];
    let block = if (16..24).contains(&test) {
        materials[test - 16]
    } else {
        "stone"
    };
    if dy == -2 && (test == 5 || test == 31 && (dx + dz).rem_euclid(2) == 0) {
        return state("air");
    }
    if dy == 4 && (test == 3 && dx == 0 && dz == 0 || test == 13) {
        return state(if test == 13 { "water" } else { "air" });
    }
    if dy == -1 && (test == 4 && dx == 0 && dz == 0 || test == 14) {
        return state(if test == 14 { "water" } else { "air" });
    }
    if (0..=3).contains(&dy) {
        if test == 24 && [dx, dy, dz] == [0, 0, 0] {
            return state("chest");
        }
        if test == 25 && [dx, dy, dz] == [0, 0, 0] {
            return state("spawner");
        }
        if test == 26 && [dx, dy, dz] == [0, 0, 0] {
            return state("barrel");
        }
        if (test == 27 && dx.abs() <= 1 || test == 28 && dx.abs() >= 3) && dy == 0 && dz.abs() == 2
        {
            return state("chest");
        }
        if test == 29 && dy == 1 && dz == 0 {
            return state("spawner");
        }
        if test == 30 && dy == 2 {
            return state("short_grass");
        }
        if test == 32 && dy == 1 {
            return state("water");
        }
        if test >= 40 && [dx, dy, dz] == [1, 1, 1] {
            return state(
                [
                    "bedrock",
                    "end_portal_frame",
                    "reinforced_deepslate",
                    "trial_spawner",
                    "vault",
                    "trapped_chest",
                    "ender_chest",
                    "structure_block",
                ][test - 40],
            );
        }
    }
    if test == 37 && dy == 2 && dx == -4 {
        return state("air");
    }
    let corridor = test != 1
        && (test != 2 || dx <= 0)
        && (dz == 0
            || matches!(test, 6 | 7 | 38 | 39) && dz == 1
            || test == 7 && dz == -1
            || matches!(test, 38 | 39) && dx == 0 && (test == 39 || dz < 0));
    if corridor && (dy == 0 || dy == 1 && test != 8) {
        return state(match test {
            9 => "water",
            10 => "lava",
            11 => "cave_air",
            12 => "void_air",
            _ => "air",
        });
    }
    state(block)
}

fn prepare(test: usize) -> RegionFeatureWorld {
    let mut world = RegionFeatureWorld::new(
        (0..=1).flat_map(|cx| (-1..=0).map(move |cz| GeneratedChunk::new(cx, cz, -64, 384, 1))),
        0,
    )
    .unwrap();
    for dy in -2..=4 {
        for dz in -6..=6 {
            for dx in -6..=6 {
                world.set_block_state(15 + dx, y(test) + dy, -1 + dz, initial(test, dx, dy, dz));
            }
        }
    }
    let chunks = world.into_chunks().map(|mut chunk| {
        chunk.freeze_worldgen_heightmaps();
        chunk
    });
    RegionFeatureWorld::new(chunks, 0).unwrap()
}

fn blocks(world: &RegionFeatureWorld, y: i32) -> Vec<u32> {
    (-2..=4)
        .flat_map(|dy| {
            (-6..=6).flat_map(move |dz| {
                (-6..=6).map(move |dx| world.block_state(15 + dx, y + dy, -1 + dz))
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
        assert_eq!(actual, expected, "{label}, pass {pass}: blocks");
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
        let mut expected_entities: Vec<_> =
            std::iter::from_fn(|| lines.next_if(|line| line.starts_with("entity ")))
                .map(|line| {
                    let bytes: Vec<_> = line.as_bytes()[7..]
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|pair| {
                            u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap()
                        })
                        .collect();
                    Nbt::read_network(&mut bytes.as_slice()).unwrap()
                })
                .collect();
        let mut actual_entities: Vec<_> = (0..=1)
            .flat_map(|cx| (-1..=0).map(move |cz| (cx, cz)))
            .flat_map(|(cx, cz)| world.chunk(cx, cz).unwrap().block_entities())
            .map(|e| Nbt::Compound(e.clone()))
            .collect();
        let key = |e: &Nbt| format!("{e:?}");
        actual_entities.sort_by_key(key);
        expected_entities.sort_by_key(key);
        assert_eq!(
            actual_entities, expected_entities,
            "{label}, pass {pass}: block entities"
        );
        total += usize::from(placed);
    }
    total
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn monster_rooms_match_vanilla() {
    let reference = reference::Reference::required();
    let data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let feature = FeatureCompiler::new(&data)
        .configured("minecraft:monster_room")
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
            "MonsterRoomProbe.java",
            include_str!("MonsterRoomProbe.java"),
        ),
    ]);
    let output = reference.temp.0.join("monster-rooms.txt");
    let seeds = reference::sampled_seeds(&[0, 42, -4_172_144_997_902_289_642]);
    reference.java(
        "MonsterRoomProbe",
        [
            output.display().to_string(),
            seeds
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(","),
        ],
    );
    let text = fs::read_to_string(output).unwrap();
    let mut lines = text.lines().peekable();
    let mut placed = 0;
    for &seed in seeds {
        for legacy in [false, true] {
            for test in 0..48 {
                let label = format!("case {seed} {legacy} {test}");
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
    assert!(lines.next().is_none());
    assert!(placed > 0);
    eprintln!(
        "Compared {} monster room placements and replays ({placed} successful)",
        seeds.len() * 2 * 48 * 2
    );
}
