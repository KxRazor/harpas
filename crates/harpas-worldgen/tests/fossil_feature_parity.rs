//! Fossils compared block by block with vanilla's own fossil features, in a decoration region of test terrain.
mod reference;

use harpas_worldgen::BlockGetter;
use std::fs;
use std::sync::Arc;

use harpas_core::block::block_states;
use harpas_core::random::{LegacyRandom, RandomSource, WorldgenRandom, XoroshiroRandom};
use harpas_worldgen::{
    CompiledFeature, FeatureCompiler, GeneratedChunk, RegionFeatureWorld, StructureTemplates,
    WorldgenData, place_feature,
};

fn state(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}

/// The probe's terrains, kept in step with `FossilFeatureProbe.terrain`.
fn terrain(scenario: usize, x: i32, y: i32, z: i32) -> u32 {
    if y < -59 {
        return state("bedrock");
    }
    let rock = state(if y < 0 { "deepslate" } else { "stone" });
    let (air, water) = (state("air"), state("water"));
    match scenario {
        0 => {
            if y <= 40 {
                rock
            } else if y <= 62 {
                water
            } else {
                air
            }
        }
        1 => {
            if y <= 70 {
                rock
            } else {
                air
            }
        }
        2 => {
            if y > 70 {
                air
            } else if (35..=58).contains(&y) && z < 8 {
                state("cave_air")
            } else {
                rock
            }
        }
        3 => {
            if y <= -45 {
                rock
            } else {
                air
            }
        }
        4 => {
            if y <= 40 + (x + z).rem_euclid(25) {
                rock
            } else if y <= 62 {
                water
            } else {
                air
            }
        }
        _ => {
            if y > 70 {
                air
            } else if (20..=45).contains(&y) && x < 8 {
                state("lava")
            } else {
                rock
            }
        }
    }
}

fn region(scenario: usize) -> RegionFeatureWorld {
    let chunks = (-1..=1).flat_map(|cx| {
        (-1..=1).map(move |cz| {
            let mut chunk = GeneratedChunk::new(cx, cz, -64, 384, 1);
            for lz in 0..16 {
                for lx in 0..16 {
                    for y in -64..320 {
                        let block = terrain(scenario, cx * 16 + lx as i32, y, cz * 16 + lz as i32);
                        if !block_states().is_air(block) {
                            chunk.set_block_state(lx, y, lz, block);
                        }
                    }
                }
            }
            // Proto-chunks that completed carvers no longer update their worldgen heightmaps.
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
    for y in -64..320 {
        for z in -16..32 {
            for x in -16..32 {
                let block = world.block_state(x, y, z);
                if block != terrain(scenario, x, y, z) {
                    actual.push(format!("block {x} {y} {z} {block}"));
                }
            }
        }
    }
    assert_eq!(actual, expected, "{label}: blocks");
    placed
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn fossils_match_vanilla() {
    let reference = reference::Reference::required();
    let data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let templates = Arc::new(StructureTemplates::from_zip_path(&reference.server_jar).unwrap());
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
            "FossilFeatureProbe.java",
            include_str!("FossilFeatureProbe.java"),
        ),
    ]);
    let output = reference.temp.0.join("fossils.txt");
    reference.java("FossilFeatureProbe", [output.display().to_string()]);
    let text = fs::read_to_string(output).unwrap();
    let mut lines = text.lines().peekable();
    let mut compiler = FeatureCompiler::with_templates(&data, templates);
    let (mut cases, mut placed) = (0, 0);
    for name in ["fossil_coal", "fossil_diamonds"] {
        let feature = compiler
            .configured(&format!("minecraft:{name}"))
            .unwrap()
            .expect("fossils compile with templates");
        for scenario in 0..6 {
            for [x, z] in [[3, 12], [8, 8], [14, 1]] {
                for y in [100, -20] {
                    for seed in [0, -4_172_144_997_902_289_642] {
                        for legacy in [false, true] {
                            let label =
                                format!("case {name} {scenario} {x} {z} {y} {seed} {legacy}");
                            let origin = [x, y, z];
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
    }
    assert!(lines.next().is_none());
    assert!(
        placed > 0 && placed < cases,
        "both placed and refused fossils"
    );
    eprintln!("Compared the blocks and random tails of {cases} fossil placements, {placed} placed");
}
