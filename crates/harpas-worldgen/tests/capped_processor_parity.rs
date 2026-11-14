//! Whole-template capped rules compared with vanilla's processor pipeline, including chained limits.
mod reference;

use std::fs;

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_worldgen::WorldgenData;
use harpas_worldgen::processor::{CappedProcessor, ProcessorBlock, RuleProcessor};
use reference::Reference;
use serde_json::{Value, json};

fn state(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}

fn rule(input: Value, output: &str, modifier: Value) -> Value {
    json!({"input_predicate": input,
        "location_predicate": {"predicate_type": "minecraft:always_true"},
        "output_state": {"Name": format!("minecraft:{output}")}, "block_entity_modifier": modifier})
}

fn pipelines(data: &WorldgenData) -> Vec<Vec<Value>> {
    let mut out = Vec::new();
    let mut shipped = 0;
    for list in data.processor_lists.values() {
        let caps: Vec<_> = list
            .iter()
            .filter(|p| p["processor_type"] == "minecraft:capped")
            .collect();
        if !caps.is_empty() {
            shipped += caps.len();
            for cap in caps {
                out.push(vec![cap.clone()]);
            }
            out.push(list.clone());
        }
    }
    assert_eq!(shipped, 4, "26.1 capped processors");
    let always = json!({"predicate_type": "minecraft:always_true"});
    let pass = json!({"type": "minecraft:passthrough"});
    let loot = json!({"type": "minecraft:append_loot", "loot_table": "minecraft:chests/test"});
    let mut location = rule(always.clone(), "suspicious_gravel", loot.clone());
    location["location_predicate"] =
        json!({"predicate_type": "minecraft:block_match", "block": "minecraft:stone"});
    let mut distance = rule(always.clone(), "chest", loot.clone());
    distance["position_predicate"] = json!({"predicate_type": "minecraft:axis_aligned_linear_pos",
        "axis": "x", "min_dist": 2, "max_dist": 12, "min_chance": 0.9, "max_chance": 0.1});
    let delegates = [
        vec![],
        vec![rule(always.clone(), "stone", pass.clone())],
        vec![rule(
            always.clone(),
            "stone",
            json!({"type": "minecraft:clear"}),
        )],
        vec![rule(
            json!({"predicate_type": "minecraft:block_match", "block": "minecraft:stone"}),
            "stone",
            loot.clone(),
        )],
        vec![location],
        vec![
            rule(
                json!({"predicate_type": "minecraft:block_match", "block": "minecraft:dirt"}),
                "dirt",
                pass,
            ),
            rule(
                json!({"predicate_type": "minecraft:random_blockstate_match", "block_state": {"Name": "minecraft:oak_log", "Properties": {"axis": "x"}}, "probability": 0.4}),
                "gold_block",
                loot,
            ),
            distance,
        ],
    ];
    let limits = vec![
        json!(1),
        json!(5),
        json!(500),
        json!({"type": "minecraft:constant", "value": 3}),
        json!({"type": "minecraft:uniform", "min_inclusive": 1, "max_inclusive": 9}),
        json!({"type": "minecraft:uniform", "min_inclusive": 5, "max_inclusive": 5}),
        json!({"type": "minecraft:biased_to_bottom", "min_inclusive": 1, "max_inclusive": 9}),
        json!({"type": "minecraft:clamped_normal", "mean": 4.0, "deviation": 2.0, "min_inclusive": 1, "max_inclusive": 9}),
        json!({"type": "minecraft:trapezoid", "min": 1, "max": 9, "plateau": 3}),
        json!({"type": "minecraft:clamped", "source": 0, "min_inclusive": 3, "max_inclusive": 6}),
        json!({"type": "minecraft:clamped", "source": 9, "min_inclusive": 3, "max_inclusive": 6}),
        json!({"type": "minecraft:weighted_list", "distribution": [
            {"data": 1, "weight": 2}, {"data": {"type": "minecraft:uniform", "min_inclusive": 1, "max_inclusive": 8}, "weight": 3}]}),
    ];
    for rules in delegates {
        for limit in &limits {
            let cap = json!({"processor_type": "minecraft:capped", "limit": limit,
                "delegate": {"processor_type": "minecraft:rule", "rules": rules}});
            out.push(vec![cap.clone()]);
            // A second finalizer must read the first one's changes with a fresh limit and shuffle.
            out.push(vec![cap.clone(), cap]);
        }
    }
    // All per-block rules run before any finalizer, even when listed after the capped processor.
    let stone = json!({"predicate_type": "minecraft:block_match", "block": "minecraft:stone"});
    let cap = json!({"processor_type": "minecraft:capped", "limit": 3,
        "delegate": {"processor_type": "minecraft:rule", "rules": [rule(stone, "gold_block", json!({"type": "minecraft:clear"}))]}});
    let per_block = json!({"processor_type": "minecraft:rule", "rules": [rule(always, "stone", json!({"type": "minecraft:passthrough"}))]});
    out.push(vec![cap.clone(), per_block.clone()]);
    out.push(vec![per_block, cap]);
    out
}

fn input_tag(kind: usize) -> Option<Compound> {
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
fn capped_processors_match_vanilla() {
    let reference = Reference::required();
    let data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let pipelines = pipelines(&data);
    let seeds = reference::sampled_seeds(&[0, -91, 0x1234_5678_abcd_ef01]);
    let origins = [[0, 0, 0], [-17, -64, 32], [29_999_984, 319, -29_999_984]];
    let lengths: &[usize] = if reference::reduced_sample() {
        &[0, 1, 7, 24]
    } else {
        &[0, 1, 2, 7, 24, 81]
    };
    let mut states = vec![
        state("stone"),
        state("dirt"),
        state("air"),
        state("chest"),
        block_states()
            .state("minecraft:oak_log", [("axis", "x")])
            .unwrap()
            .0,
    ];
    states.extend(
        data.block_tag_states("minecraft:trail_ruins_replaceable")
            .unwrap()
            .into_iter()
            .enumerate()
            .filter(|(_, yes)| *yes)
            .map(|(id, _)| id as u32),
    );
    let mut cases = Vec::new();
    let mut expected = Vec::new();
    for (pipeline, json) in pipelines.iter().enumerate() {
        let rules: Vec<_> = json
            .iter()
            .map(|j| RuleProcessor::compile("parity", j, &data).unwrap())
            .collect();
        let caps: Vec<_> = json
            .iter()
            .map(|j| CappedProcessor::compile("parity", j, &data).unwrap())
            .collect();
        assert!(
            rules
                .iter()
                .zip(&caps)
                .all(|(r, c)| r.is_some() || c.is_some())
        );
        for &seed in seeds {
            for origin in origins {
                let pivot = [origin[0] - 7, origin[1] + 3, origin[2] + 2];
                for &length in lengths {
                    let mut blocks = Vec::new();
                    let mut inputs = Vec::new();
                    for i in 0..length {
                        let pos = [
                            origin[0] + i as i32 % 9 - 4,
                            origin[1] + i as i32 / 27 - 1,
                            origin[2] + i as i32 / 9 % 3 - 1,
                        ];
                        let state = states[(i + pipeline) % states.len()];
                        let tag = (i + pipeline) % 3;
                        let location = states[(i * 3 + pipeline) % states.len()];
                        inputs.push(json!([pos, state, tag, location]));
                        let mut block = ProcessorBlock {
                            pos,
                            state,
                            nbt: input_tag(tag),
                        };
                        for rule in rules.iter().flatten() {
                            block = rule.process(&block, location, pivot);
                        }
                        blocks.push(block);
                    }
                    for cap in caps.iter().flatten() {
                        cap.finalize(seed, origin, pivot, &mut blocks, |pos| {
                            let i = (pos[0] - origin[0] + 4)
                                + (pos[2] - origin[2] + 1) * 9
                                + (pos[1] - origin[1] + 1) * 27;
                            states[(i as usize * 3 + pipeline) % states.len()]
                        });
                    }
                    let result: Compound = [(
                        "blocks",
                        if blocks.is_empty() {
                            Nbt::List(List::Empty)
                        } else {
                            Nbt::List(List::Compound(blocks.into_iter().map(snapshot).collect()))
                        },
                    )]
                    .into_iter()
                    .collect();
                    expected.push(result);
                    cases.push(json!({"pipeline": pipeline, "seed": seed, "origin": origin, "pivot": pivot, "blocks": inputs}));
                }
            }
        }
    }
    let fixtures = reference.temp.0.join("caps.json");
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
    ]);
    let output = reference.temp.0.join("caps.nbt");
    reference.java(
        "CappedProcessorProbe",
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
    for (index, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
        assert_eq!(actual, expected, "case {index}: {}", cases[index]);
    }
    eprintln!(
        "{} capped processor pipelines match vanilla in {} cases",
        pipelines.len(),
        expected.len()
    );
}
