//! Runtime-added gravity processors compared with vanilla's complete template processing pass.
mod reference;

use std::fs;

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_core::random::{RandomSource, XoroshiroRandom};
use harpas_worldgen::processor::{ProcessorBlock, ProcessorPipeline, ProcessorWorld};
use harpas_worldgen::{HeightmapType, Rotation, WorldgenData};
use reference::Reference;
use serde_json::{Value, json};

fn state(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}

struct Terrain {
    live: bool,
    reads: Vec<Compound>,
}

impl ProcessorWorld for Terrain {
    fn block_at(&mut self, pos: [i32; 3]) -> u32 {
        let selector = pos[0]
            .wrapping_add(pos[1].wrapping_mul(3))
            .wrapping_add(pos[2])
            .rem_euclid(7);
        let state = state(match selector {
            0 => "bedrock",
            1 => "water",
            _ => "stone",
        });
        self.reads.push(
            [
                ("pos", Nbt::IntArray(pos.to_vec())),
                ("state", Nbt::Int(state as i32)),
            ]
            .into_iter()
            .collect(),
        );
        state
    }

    fn heightmap(&mut self, kind: HeightmapType, x: i32, z: i32) -> i32 {
        let ordinal = HeightmapType::ALL.iter().position(|&k| k == kind).unwrap() as i32;
        let height = ordinal * 43 - 90 + x.rem_euclid(23) - z.rem_euclid(19);
        self.reads.push(
            [
                ("heightmap", Nbt::String(kind.nbt_key().into())),
                ("pos", Nbt::IntArray(vec![x, z])),
                ("height", Nbt::Int(height)),
            ]
            .into_iter()
            .collect(),
        );
        height
    }

    fn is_server_level(&self) -> bool {
        self.live
    }
}

fn pipelines() -> Vec<Vec<Value>> {
    let gravity = json!({"processor_type": "minecraft:gravity"});
    let mut pipelines = vec![vec![gravity.clone()]];
    for kind in HeightmapType::ALL {
        for offset in [-1, 17, i32::MIN, i32::MAX] {
            pipelines.push(vec![json!({"processor_type": "minecraft:gravity",
                "heightmap": kind.nbt_key(), "offset": offset})]);
        }
    }
    let rule = json!({"processor_type": "minecraft:rule", "rules": [{
        "input_predicate": {"predicate_type": "minecraft:random_block_match", "block": "minecraft:stone", "probability": 0.6},
        "location_predicate": {"predicate_type": "minecraft:block_match", "block": "minecraft:water"},
        "output_state": {"Name": "minecraft:chest"},
        "block_entity_modifier": {"type": "minecraft:append_loot", "loot_table": "minecraft:chests/test"}
    }]});
    let protected = json!({"processor_type": "minecraft:protected_blocks", "value": "#minecraft:features_cannot_replace"});
    let rot = json!({"processor_type": "minecraft:block_rot", "integrity": 0.5});
    let ignore = json!({"processor_type": "minecraft:block_ignore", "blocks": [{"Name": "minecraft:stone"}]});
    let cap = json!({"processor_type": "minecraft:capped", "delegate": rule.clone(), "limit": 3});
    for processor in [rule, protected, rot, ignore, cap] {
        pipelines.push(vec![processor.clone(), gravity.clone()]);
        pipelines.push(vec![gravity.clone(), processor]);
    }
    pipelines.push(vec![
        gravity.clone(),
        json!({"processor_type": "minecraft:gravity", "heightmap": "OCEAN_FLOOR_WG", "offset": -1}),
        gravity,
    ]);
    pipelines
}

fn compound_list(values: Vec<Compound>) -> Nbt {
    Nbt::List(if values.is_empty() {
        List::Empty
    } else {
        List::Compound(values)
    })
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn gravity_and_ordered_pipelines_match_vanilla() {
    let reference = Reference::required();
    let data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let pipelines = pipelines();
    let origins: [[i32; 3]; 3] = [[0, 0, 0], [-17, -64, 32], [29_999_984, 319, -29_999_984]];
    let lengths: &[usize] = if reference::reduced_sample() {
        &[0, 1, 24]
    } else {
        &[0, 1, 7, 64]
    };
    let mut cases = Vec::new();
    let mut expected = Vec::new();
    for (index, entries) in pipelines.iter().enumerate() {
        let pipeline = ProcessorPipeline::compile("gravity parity", entries, &data)
            .unwrap()
            .unwrap();
        for &seed in reference::sampled_seeds(&[0, -91, 0x1234_5678_abcd_ef01]) {
            for origin in origins {
                for (rotation_index, rotation) in Rotation::ALL.into_iter().enumerate() {
                    for live in [false, true] {
                        for explicit_random in [false, true] {
                            for &length in lengths {
                                let pivot = [origin[0] + 7, origin[1] - 3, origin[2] + 2];
                                let mut inputs = Vec::new();
                                let blocks: Vec<_> = (0..length)
                                    .map(|i| {
                                        let y =
                                            [0, 1, -1, -64, 319, 400, i32::MAX, i32::MIN][i % 8];
                                        let local = [i as i32 % 9 - 4, y, i as i32 / 9 - 3];
                                        let transformed = rotation.transform(local);
                                        let pos = std::array::from_fn(|axis| {
                                            origin[axis].wrapping_add(transformed[axis])
                                        });
                                        let state = state(
                                            ["stone", "chest", "oak_stairs", "bedrock"][i % 4],
                                        );
                                        let kind = (i + index) % 3;
                                        let nbt = match kind {
                                            0 => None,
                                            1 => Some(Compound::new()),
                                            _ => Some(
                                                [
                                                    (
                                                        "keep",
                                                        Nbt::Compound(
                                                            [("nested", Nbt::Int(7))]
                                                                .into_iter()
                                                                .collect(),
                                                        ),
                                                    ),
                                                    ("LootTable", Nbt::String("test:old".into())),
                                                    ("LootTableSeed", Nbt::Long(123)),
                                                ]
                                                .into_iter()
                                                .collect(),
                                            ),
                                        };
                                        inputs.push(json!([local, state, kind]));
                                        ProcessorBlock { pos, state, nbt }
                                    })
                                    .collect();
                                let mut random = XoroshiroRandom::new(seed);
                                let mut world = Terrain {
                                    live,
                                    reads: Vec::new(),
                                };
                                let output = pipeline.process_in_world(
                                    seed,
                                    origin,
                                    pivot,
                                    &blocks,
                                    &mut world,
                                    if explicit_random {
                                        Some(&mut random)
                                    } else {
                                        None
                                    },
                                );
                                let snapshots = output
                                    .into_iter()
                                    .map(|block| {
                                        let mut result: Compound = [
                                            ("pos", Nbt::IntArray(block.pos.to_vec())),
                                            ("state", Nbt::Int(block.state as i32)),
                                        ]
                                        .into_iter()
                                        .collect();
                                        if let Some(nbt) = block.nbt {
                                            result.insert("nbt", nbt);
                                        }
                                        result
                                    })
                                    .collect();
                                expected.push(
                                    [
                                        ("blocks", compound_list(snapshots)),
                                        ("reads", compound_list(world.reads)),
                                        ("random", Nbt::Long(random.next_long())),
                                    ]
                                    .into_iter()
                                    .collect::<Compound>(),
                                );
                                cases.push(
                                    json!({"pipeline": index, "seed": seed, "origin": origin,
                                    "pivot": pivot, "rotation": rotation_index, "live": live,
                                    "random": explicit_random, "blocks": inputs}),
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    let fixtures = reference.temp.0.join("gravity.json");
    fs::write(
        &fixtures,
        json!({"pipelines": pipelines, "cases": cases}).to_string(),
    )
    .unwrap();
    reference.compile(&[
        (
            "StructurePlacementProbe.java",
            include_str!("StructurePlacementProbe.java"),
        ),
        (
            "RuleProcessorProbe.java",
            include_str!("RuleProcessorProbe.java"),
        ),
        (
            "GravityProcessorProbe.java",
            include_str!("GravityProcessorProbe.java"),
        ),
    ]);
    let output = reference.temp.0.join("gravity.nbt");
    reference.java(
        "GravityProcessorProbe",
        [fixtures.display().to_string(), output.display().to_string()],
    );
    let bytes = fs::read(output).unwrap();
    let (_, Nbt::Compound(root)) = Nbt::read_named(&mut bytes.as_slice()).unwrap() else {
        panic!("output compound")
    };
    let Some(Nbt::List(List::Compound(actual))) = root.get("results") else {
        panic!("output results")
    };
    assert_eq!(actual.len(), expected.len());
    for (i, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
        assert_eq!(
            actual,
            expected,
            "case {i}: {}, pipeline {:?}",
            cases[i],
            pipelines[cases[i]["pipeline"].as_u64().unwrap() as usize]
        );
    }
    eprintln!(
        "{} gravity pipelines match vanilla in {} cases",
        pipelines.len(),
        cases.len()
    );
}
