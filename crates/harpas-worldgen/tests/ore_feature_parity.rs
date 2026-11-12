//! Compares the ore feature with vanilla's own OreFeature on the same decoration random source.
mod reference;

use std::collections::BTreeSet;
use std::fs;

use harpas_core::block::block_states;
use harpas_core::random::{RandomSource, WorldgenRandom, XoroshiroRandom};
use harpas_worldgen::{
    ChunkFeatureWorld, CompiledOre, FeatureWorld, OreTarget, RuleTestRule, place_ore,
};
use reference::Reference;

const CASES: [(i64, i32, i32, u32, i32); 4] = [
    (42, 32, 9, 0, 63),
    (0, 32, 9, 0, 63),
    (-1, 12, 15, 0, 63),
    (42, 31, 9, 5, 29),
];

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn ore_features_match_vanilla() {
    let reference = Reference::required();
    reference.compile(&[("OreFeatureProbe.java", include_str!("OreFeatureProbe.java"))]);
    let output = reference.temp.0.join("ore-features.txt");
    reference.java("OreFeatureProbe", [output.display().to_string()]);
    let text = fs::read_to_string(output).expect("read the vanilla ore output");

    let mut actual = Vec::new();
    for (index, &(seed, origin_y, size, discard_tenths, stone_top)) in CASES.iter().enumerate() {
        let mut chunk = harpas_worldgen::GeneratedChunk::new(0, 0, -64, 384, 1);
        let stone = block_states().default_state("minecraft:stone").unwrap();
        for y in -64..=stone_top {
            for z in 0..16 {
                for x in 0..16 {
                    chunk.set_block_state(x, y, z, stone);
                }
            }
        }
        let iron = block_states().default_state("minecraft:iron_ore").unwrap();
        let config = CompiledOre {
            size,
            discard_chance_on_air_exposure: discard_tenths as f32 / 10.0,
            targets: vec![OreTarget {
                rule: RuleTestRule::Block("minecraft:stone".to_string()),
                state: iron,
            }],
        };
        let mut world = ChunkFeatureWorld::new(&mut chunk, 0);
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(0));
        let decoration = random.set_decoration_seed(seed, 0, 0);
        random.set_feature_seed(decoration, 0, 0);
        let placed = place_ore(&mut world, &config, &mut random, [8, origin_y, 8]);
        let mut blocks = BTreeSet::new();
        for y in -64..320 {
            for z in 0..16 {
                for x in 0..16 {
                    if world.block_state(x, y, z) == iron {
                        blocks.insert((x, y, z));
                    }
                }
            }
        }
        let post = (
            random.next_int(),
            random.next_long() as u64,
            random.next_float().to_bits(),
            random.next_double().to_bits(),
        );
        actual.push((index, placed, blocks, post));
    }

    let mut expected = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        match parts.as_slice() {
            ["case", index, placed] => expected.push((
                index.parse::<usize>().unwrap(),
                placed.parse::<bool>().unwrap(),
                BTreeSet::new(),
                None,
            )),
            ["block", x, y, z] => {
                if let Some(last) = expected.last_mut() {
                    last.2
                        .insert((x.parse().unwrap(), y.parse().unwrap(), z.parse().unwrap()));
                }
            }
            ["post", a, b, c, d] => {
                if let Some(last) = expected.last_mut() {
                    last.3 = Some((
                        a.parse::<i32>().unwrap(),
                        b.parse::<u64>().unwrap(),
                        c.parse::<u32>().unwrap(),
                        d.parse::<u64>().unwrap(),
                    ));
                }
            }
            [] => {}
            other => panic!("unexpected probe line: {other:?}"),
        }
    }
    assert_eq!(expected.len(), CASES.len());
    for (index, placed, blocks, post) in actual {
        let (_, vanilla_placed, vanilla_blocks, vanilla_post) = &expected[index];
        assert_eq!(&placed, vanilla_placed, "case {index}: placement result");
        assert_eq!(&blocks, vanilla_blocks, "case {index}: blocks");
        assert_eq!(&Some(post), vanilla_post, "case {index}: random state");
    }
}
