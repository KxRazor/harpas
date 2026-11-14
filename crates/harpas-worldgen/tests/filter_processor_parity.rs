//! Complete shipped processor lists and synthetic filters compared with vanilla's template pipeline.
mod reference;

use std::collections::{BTreeSet, HashMap};
use std::fs;

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_core::random::{LegacyRandom, RandomSource, XoroshiroRandom};
use harpas_worldgen::WorldgenData;
use harpas_worldgen::processor::{ProcessorBlock, ProcessorPipeline};
use reference::Reference;
use serde_json::{Value, json};

fn state(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}

fn rule(output: &str) -> Value {
    json!({"processor_type": "minecraft:rule", "rules": [{
        "input_predicate": {"predicate_type": "minecraft:always_true"},
        "location_predicate": {"predicate_type": "minecraft:always_true"},
        "output_state": {"Name": format!("minecraft:{output}")},
        "block_entity_modifier": {"type": "minecraft:append_loot", "loot_table": "minecraft:chests/test"}
    }]})
}

fn rot(integrity: f32, rottable: Option<Value>) -> Value {
    let mut out = json!({"processor_type": "minecraft:block_rot", "integrity": integrity});
    if let Some(rottable) = rottable {
        out["rottable_blocks"] = rottable;
    }
    out
}

fn pipelines(data: &WorldgenData) -> Vec<Vec<Value>> {
    assert_eq!(data.processor_lists.len(), 40);
    let mut out: Vec<_> = data.processor_lists.values().cloned().collect();
    for list in data.processor_lists.values() {
        for processor in list {
            if matches!(
                processor["processor_type"].as_str(),
                Some("minecraft:block_rot" | "minecraft:protected_blocks")
            ) {
                out.push(vec![processor.clone()]);
            }
        }
    }
    assert_eq!(out.len(), 53, "all 13 shipped filters tested separately");
    let ignore = json!({"processor_type": "minecraft:block_ignore", "blocks": [
        {"Name": "minecraft:oak_log", "Properties": {"axis": "x"}}, {"Name": "minecraft:air"},
        {"Name": "minecraft:structure_block"}]});
    let protected = json!({"processor_type": "minecraft:protected_blocks", "value": "#minecraft:features_cannot_replace"});
    let cap = json!({"processor_type": "minecraft:capped", "limit": 3, "delegate": rule("suspicious_gravel")});
    let threshold = LegacyRandom::new(0).next_float();
    for integrity in [
        0.0,
        0.05,
        0.5,
        0.95,
        1.0,
        threshold,
        f32::from_bits(threshold.to_bits() - 1),
    ] {
        for rottable in [
            None,
            Some(json!([])),
            Some(json!(["minecraft:stone", "minecraft:oak_log"])),
            Some(json!("#minecraft:logs")),
        ] {
            let rot = rot(integrity, rottable);
            out.push(vec![rot.clone()]);
            out.push(vec![
                rule("stone"),
                rot.clone(),
                rot.clone(),
                protected.clone(),
                cap.clone(),
            ]);
            out.push(vec![rule("gold_block"), rot]);
        }
    }
    for filter in [ignore.clone(), protected.clone(), rot(0.5, None)] {
        out.push(vec![filter.clone(), cap.clone()]);
        out.push(vec![cap.clone(), filter.clone()]);
        out.push(vec![filter.clone(), rule("oak_log"), filter.clone()]);
        out.push(vec![rule("oak_log"), filter]);
    }
    out.push(vec![]);
    out.push(vec![json!({"processor_type": "minecraft:nop"})]);
    out.push(vec![
        json!({"processor_type": "minecraft:block_ignore", "blocks": []}),
    ]);
    out.push(vec![
        json!({"processor_type": "minecraft:protected_blocks", "value": "#minecraft:logs"}),
        cap,
    ]);
    out.push(vec![ignore, protected]);
    out
}

fn tag(kind: usize) -> Option<Compound> {
    match kind {
        0 => None,
        1 => Some(Compound::new()),
        _ => Some(
            [
                (
                    "keep",
                    Nbt::Compound([("nested", Nbt::Int(7))].into_iter().collect()),
                ),
                ("LootTable", Nbt::String("test:old".into())),
                ("LootTableSeed", Nbt::Long(123)),
            ]
            .into_iter()
            .collect(),
        ),
    }
}

fn snapshot(block: ProcessorBlock) -> Compound {
    let mut out: Compound = [
        ("pos", Nbt::IntArray(block.pos.to_vec())),
        ("state", Nbt::Int(block.state as i32)),
    ]
    .into_iter()
    .collect();
    if let Some(nbt) = block.nbt {
        out.insert("nbt", nbt);
    }
    out
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn template_filters_and_complete_lists_match_vanilla() {
    let reference = Reference::required();
    let data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let pipelines = pipelines(&data);
    let mut states = BTreeSet::new();
    for name in [
        "air",
        "cave_air",
        "void_air",
        "stone",
        "oak_log",
        "bedrock",
        "water",
        "chest",
        "structure_block",
        "reinforced_deepslate",
        "sculk",
        "sculk_vein",
        "gravel",
        "mud_bricks",
    ] {
        states.insert(state(name));
    }
    for name in [
        "ancient_city_replaceable",
        "features_cannot_replace",
        "trail_ruins_replaceable",
        "logs",
    ] {
        for (id, yes) in data
            .block_tag_states(&format!("minecraft:{name}"))
            .unwrap()
            .into_iter()
            .enumerate()
        {
            if yes {
                states.insert(id as u32);
            }
        }
    }
    let states: Vec<_> = states.into_iter().collect();
    let seeds = reference::sampled_seeds(&[0, -91, 0x1234_5678_abcd_ef01]);
    let origins = [[0, 0, 0], [-17, -64, 32], [29_999_984, 319, -29_999_984]];
    let lengths: &[usize] = if reference::reduced_sample() {
        &[0, 1, 24]
    } else {
        &[0, 1, 7, 81]
    };
    let mut cases = Vec::new();
    let mut expected = Vec::new();
    let compiled: Vec<_> = pipelines
        .iter()
        .map(|json| {
            ProcessorPipeline::compile("parity", json, &data)
                .unwrap()
                .expect("all shipped lists and fixtures compile")
        })
        .collect();
    let mut add_case = |index: usize,
                        seed: i64,
                        origin: [i32; 3],
                        explicit_random: bool,
                        pairs: Vec<(u32, u32)>| {
        let mut blocks = Vec::new();
        let mut inputs = Vec::new();
        let mut terrain = HashMap::new();
        for (i, (input, location)) in pairs.into_iter().enumerate() {
            let pos = [
                origin[0] + i as i32 % 9,
                origin[1] + i as i32 / 27,
                origin[2] + i as i32 / 9 % 3,
            ];
            let kind = (i + index) % 3;
            inputs.push(json!([pos, input, kind, location]));
            blocks.push(ProcessorBlock {
                pos,
                state: input,
                nbt: tag(kind),
            });
            terrain.insert(pos, location);
        }
        let mut random = XoroshiroRandom::new(seed);
        let output = compiled[index].process(
            seed,
            origin,
            [origin[0] + 7, origin[1] - 3, origin[2] + 2],
            &blocks,
            |pos| terrain[&pos],
            if explicit_random {
                Some(&mut random)
            } else {
                None
            },
        );
        let mut result: Compound = [(
            "blocks",
            if output.is_empty() {
                Nbt::List(List::Empty)
            } else {
                Nbt::List(List::Compound(output.into_iter().map(snapshot).collect()))
            },
        )]
        .into_iter()
        .collect();
        if explicit_random {
            result.insert("random", random.next_long());
        }
        expected.push(result);
        cases.push(json!({"pipeline": index, "seed": seed, "origin": origin, "random": explicit_random, "blocks": inputs}));
    };
    for index in 0..pipelines.len() {
        for &seed in seeds {
            for origin in origins {
                for &length in lengths {
                    for explicit_random in [false, true] {
                        add_case(
                            index,
                            seed,
                            origin,
                            explicit_random,
                            (0..length)
                                .map(|i| {
                                    (
                                        states[(i * 13 + index) % states.len()],
                                        states[(i * 31 + index * 7) % states.len()],
                                    )
                                })
                                .collect(),
                        );
                    }
                }
            }
        }
    }
    // Every state in all four relevant tags appears on both sides of each shipped filter. Full runs
    // additionally cover the complete registry, including states outside those tags.
    let coverage: Vec<_> = if reference::reduced_sample() {
        states.clone()
    } else {
        (0..block_states().len()).collect()
    };
    for index in 40..53 {
        for (part, chunk) in coverage.chunks(81).enumerate() {
            for explicit_random in [false, true] {
                add_case(
                    index,
                    seeds[0],
                    origins[part % origins.len()],
                    explicit_random,
                    chunk.iter().map(|&id| (id, id)).collect(),
                );
            }
        }
    }
    let fixtures = reference.temp.0.join("filters.json");
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
            "CappedProcessorProbe.java",
            include_str!("CappedProcessorProbe.java"),
        ),
        (
            "FilterProcessorProbe.java",
            include_str!("FilterProcessorProbe.java"),
        ),
    ]);
    let output = reference.temp.0.join("filters.nbt");
    reference.java(
        "FilterProcessorProbe",
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
        "{} processor pipelines match vanilla in {} cases",
        pipelines.len(),
        cases.len()
    );
}
