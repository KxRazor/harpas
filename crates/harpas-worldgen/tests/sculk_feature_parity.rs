//! Sculk patches and sculk veins compared block by block with vanilla's own features, in test caves.
mod reference;

use harpas_worldgen::BlockGetter;
use std::fs;

use harpas_core::block::block_states;
use harpas_core::random::{LegacyRandom, RandomSource, WorldgenRandom, XoroshiroRandom};
use harpas_worldgen::{
    CompiledFeature, FeatureCompiler, GeneratedChunk, RegionFeatureWorld, WorldgenData,
    place_feature,
};

fn state(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}

/// The probe's terrains, kept in step with `SculkFeatureProbe.terrain`.
fn terrain(scenario: usize, x: i32, y: i32, z: i32) -> u32 {
    if y >= 0 {
        return state("air");
    }
    if y < -59 {
        return state("bedrock");
    }
    let cave = (-12..=28).contains(&x) && (-12..=28).contains(&z) && (-40..=-30).contains(&y);
    if cave {
        if scenario == 1 && y <= -39 {
            return state("water");
        }
        if scenario == 3 && x.rem_euclid(4) == 0 && z.rem_euclid(4) == 0 {
            return state("deepslate");
        }
        if scenario == 5 && y == -40 && (x * 7 + z * 3).rem_euclid(11) == 0 {
            return state("sculk_sensor");
        }
        return state("air");
    }
    if scenario == 2 && y == -41 {
        let floors = [
            "deepslate",
            "tuff",
            "dirt",
            "cobblestone",
            "stone",
            "gravel",
        ];
        return state(floors[(x + 2 * z).rem_euclid(floors.len() as i32) as usize]);
    }
    if scenario == 5 && y == -41 && (x + z).rem_euclid(5) == 0 {
        return state("sculk");
    }
    state("deepslate")
}

fn region(scenario: usize) -> RegionFeatureWorld {
    let chunks = (-1..=1).flat_map(|cx| {
        (-1..=1).map(move |cz| {
            let mut chunk = GeneratedChunk::new(cx, cz, -64, 384, 1);
            for lz in 0..16 {
                for lx in 0..16 {
                    for y in -64..0 {
                        let block = terrain(scenario, cx * 16 + lx as i32, y, cz * 16 + lz as i32);
                        if !block_states().is_air(block) {
                            chunk.set_block_state(lx, y, lz, block);
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

fn compare<R: RandomSource>(
    lines: &mut std::iter::Peekable<std::str::Lines<'_>>,
    feature: &CompiledFeature,
    source: R,
    label: &str,
    scenario: usize,
    origin: [i32; 3],
) -> bool {
    let mut world = region(scenario);
    let mut random = WorldgenRandom::new(source);
    let placed = place_feature(&mut world, feature, &mut random, origin);
    assert_eq!(
        lines.next(),
        Some(format!("{label} {placed} {}", random.next_int()).as_str())
    );
    let expected: Vec<&str> =
        std::iter::from_fn(|| lines.next_if(|line| line.starts_with("block "))).collect();
    let mut actual = Vec::new();
    for y in -64..0 {
        for z in -16..32 {
            for x in -16..32 {
                let block = world.block_state(x, y, z);
                if block != terrain(scenario, x, y, z) {
                    actual.push(format!("block {x} {y} {z} {block}"));
                }
            }
        }
    }
    if actual != expected {
        let missing: Vec<_> = expected
            .iter()
            .filter(|line| !actual.iter().any(|a| a == *line))
            .take(10)
            .collect();
        let extra: Vec<_> = actual
            .iter()
            .filter(|line| !expected.contains(&line.as_str()))
            .take(10)
            .collect();
        panic!(
            "{label}: {} blocks differ from vanilla's {}\nonly vanilla: {missing:?}\nonly Harpas: {extra:?}",
            actual.len(),
            expected.len()
        );
    }
    placed
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn sculk_patches_and_veins_match_vanilla() {
    let reference = reference::Reference::required();
    let data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
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
            "StructurePlacementProbe.java",
            include_str!("StructurePlacementProbe.java"),
        ),
        (
            "SculkFeatureProbe.java",
            include_str!("SculkFeatureProbe.java"),
        ),
    ]);
    let output = reference.temp.0.join("sculk.txt");
    reference.java("SculkFeatureProbe", [output.display().to_string()]);
    let text = fs::read_to_string(output).unwrap();
    let mut lines = text.lines().peekable();
    let mut compiler = FeatureCompiler::new(&data);
    let (mut cases, mut placed) = (0, 0);
    for name in [
        "sculk_patch_deep_dark",
        "sculk_patch_ancient_city",
        "sculk_vein",
    ] {
        let feature = compiler
            .configured(&format!("minecraft:{name}"))
            .unwrap()
            .expect("sculk features compile");
        for scenario in 0..6 {
            for origin in [[8, -40, 8], [3, -40, 12], [8, -35, 8], [8, -41, 8]] {
                for seed in [0, -4_172_144_997_902_289_642] {
                    for legacy in [false, true] {
                        let [x, y, z] = origin;
                        let label = format!("case {name} {scenario} {x} {y} {z} {seed} {legacy}");
                        let did = if legacy {
                            compare(
                                &mut lines,
                                &feature,
                                LegacyRandom::new(seed),
                                &label,
                                scenario,
                                origin,
                            )
                        } else {
                            compare(
                                &mut lines,
                                &feature,
                                XoroshiroRandom::new(seed),
                                &label,
                                scenario,
                                origin,
                            )
                        };
                        cases += 1;
                        placed += usize::from(did);
                    }
                }
            }
        }
    }
    assert!(lines.next().is_none());
    assert!(
        placed > 0 && placed < cases,
        "both placed and refused patches"
    );
    eprintln!("Compared the blocks and random tails of {cases} sculk placements, {placed} placed");
}
