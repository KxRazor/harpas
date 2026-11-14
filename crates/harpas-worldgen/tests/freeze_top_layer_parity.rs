//! Surface snow and ice, live heightmaps, random tails and all snow supports compared with vanilla.
mod reference;

use std::fs;

use harpas_core::block::block_states;
use harpas_core::random::{RandomSource, WorldgenRandom, XoroshiroRandom};
use harpas_worldgen::{
    FeatureCompiler, FeatureWorld, GeneratedChunk, HeightmapType, RegionFeatureWorld, WorldgenData,
    compile_block_survival, place_feature,
};

const HEIGHTS: [i32; 9] = [-65, -64, 62, 79, 80, 127, 255, 318, 319];

fn state(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}

fn material(index: usize) -> u32 {
    let property = |name: &str, key, value| {
        block_states()
            .state(&format!("minecraft:{name}"), [(key, value)])
            .unwrap()
            .0
    };
    match index % 24 {
        0 => state("grass_block"),
        1 => state("podzol"),
        2 => state("mycelium"),
        3 => state("water"),
        4 => property("water", "level", "1"),
        5 => property("water", "level", "8"),
        6 => state("bubble_column"),
        7 => property("oak_slab", "waterlogged", "true"),
        8 => state("oak_leaves"),
        9 => state("ice"),
        10 => state("packed_ice"),
        11 => state("blue_ice"),
        12 => state("honey_block"),
        13 => state("soul_sand"),
        14 => state("mud"),
        15 => property("snow", "layers", "8"),
        16 => property("snow", "layers", "7"),
        17 => state("barrier"),
        18 => state("glass"),
        19 => state("oak_slab"),
        20 => state("short_grass"),
        21 => state("lava"),
        22 => state("glowstone"),
        _ => state("stone"),
    }
}

fn prepare(biome: u32, ground: i32) -> RegionFeatureWorld {
    let chunks = (0..=1).flat_map(|x| {
        (-1..=0).map(move |z| {
            let mut chunk = GeneratedChunk::new(x, z, -64, 384, 1);
            chunk.freeze_worldgen_heightmaps();
            chunk
        })
    });
    let mut world = RegionFeatureWorld::new(chunks, biome).unwrap();
    for z in -2..=15 {
        for x in 14..=31 {
            let index = ((x - 14) + 18 * (z + 2)) as usize;
            if ground >= -64 {
                world.set_block_state(x, ground, z, material(index));
            }
            if (-64..319).contains(&ground) && index.is_multiple_of(7) {
                world.set_block_state(x, ground + 1, z, state("short_grass"));
            }
        }
    }
    world
}

fn snapshot(world: &RegionFeatureWorld, ground: i32) -> String {
    let hash = |hash: u64, value: u32| (hash ^ u64::from(value)).wrapping_mul(0x100000001b3);
    let (mut blocks, mut heights) = (0xcbf29ce484222325_u64, 0xcbf29ce484222325_u64);
    for y in (ground - 1).max(-64)..=(ground + 3).min(319) {
        for z in -2..=15 {
            for x in 14..=31 {
                blocks = hash(blocks, world.block_state(x, y, z));
            }
        }
    }
    for kind in HeightmapType::ALL
        .into_iter()
        .filter(|kind| !kind.is_worldgen())
    {
        for z in -2..=15 {
            for x in 14..=31 {
                heights = hash(heights, world.heightmap(kind, x, z) as u32);
            }
        }
    }
    format!("{blocks} {heights}")
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn freeze_top_layer_matches_vanilla() {
    let reference = reference::Reference::required();
    let data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let feature = FeatureCompiler::new(&data)
        .configured("minecraft:freeze_top_layer")
        .unwrap()
        .unwrap();
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
        (
            "FreezeTopLayerProbe.java",
            include_str!("FreezeTopLayerProbe.java"),
        ),
    ]);
    let output = reference.temp.0.join("freeze.txt");
    reference.java(
        "FreezeTopLayerProbe",
        [
            output.display().to_string(),
            reference.server_jar.display().to_string(),
        ],
    );
    let text = fs::read_to_string(output).unwrap();
    let mut lines = text.lines();
    for (name, _) in &data.biomes {
        let biome = data.biome_id(name).unwrap();
        for ground in HEIGHTS {
            let mut world = prepare(biome, ground);
            let mut random = WorldgenRandom::new(XoroshiroRandom::new(42));
            for replay in [false, true] {
                let result = place_feature(
                    &mut world,
                    &feature,
                    &mut random,
                    [15, if replay { 1000 } else { -1000 }, -1],
                );
                let prefix = if replay {
                    "replay".into()
                } else {
                    format!("case {name} {ground}")
                };
                assert_eq!(
                    lines.next(),
                    Some(
                        format!(
                            "{prefix} {result} {} {}",
                            random.next_int(),
                            snapshot(&world, ground)
                        )
                        .as_str()
                    ),
                    "{name} y={ground} replay={replay}"
                );
            }
        }
    }
    let rule = compile_block_survival("test", "minecraft:snow", &data)
        .unwrap()
        .unwrap();
    let mut world = prepare(0, -65);
    for id in 0..block_states().len() {
        world.set_block_state(15, 63, -1, id);
        let survives = rule.test(&world, [15, 64, -1], state("snow"));
        assert_eq!(
            lines.next(),
            Some(format!("support {id} {survives}").as_str()),
            "{:?}",
            block_states().properties(id)
        );
    }
    assert!(lines.next().is_none());
    eprintln!(
        "Compared {} surface placements and replays, and all {} snow supports",
        data.biomes.len() * HEIGHTS.len() * 2,
        block_states().len()
    );
}
