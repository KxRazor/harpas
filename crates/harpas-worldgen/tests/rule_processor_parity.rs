//! Template rules compared with vanilla's real processor, including positional random consumption.
mod reference;

use std::collections::BTreeSet;
use std::fs;

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_core::random::{LegacyRandom, RandomSource};
use harpas_worldgen::processor::{ProcessorBlock, RuleProcessor};
use harpas_worldgen::{WorldgenData, resolve_block_state};
use reference::Reference;
use serde_json::{Value, json};

fn state(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}

fn always() -> Value {
    json!({"predicate_type": "minecraft:always_true"})
}

fn rule(input: Value, location: Value, position: Value) -> Value {
    json!({"input_predicate": input, "location_predicate": location,
        "position_predicate": position, "output_state": {"Name": "minecraft:gold_block"},
        "block_entity_modifier": {"type": "minecraft:append_loot", "loot_table": "minecraft:chests/test"}})
}

fn collect_processors(json: &Value, out: &mut Vec<Value>) {
    if json["processor_type"] == "minecraft:rule" {
        out.push(json.clone());
        // A loot seed exposes every random draw leading to the selected rule, also on failures.
        let mut traced = json.clone();
        let rules = traced["rules"].as_array_mut().unwrap();
        for rule in rules.iter_mut() {
            rule["block_entity_modifier"] =
                json!({"type": "minecraft:append_loot", "loot_table": "minecraft:chests/test"});
        }
        rules.push(rule(always(), always(), always()));
        out.push(traced);
    }
    if let Some(delegate) = json.get("delegate") {
        collect_processors(delegate, out);
    }
}

fn synthetic_processors() -> Vec<Value> {
    let tests = vec![
        always(),
        json!({"predicate_type": "minecraft:block_match", "block": "minecraft:oak_log"}),
        json!({"predicate_type": "minecraft:blockstate_match", "block_state": {"Name": "minecraft:oak_log", "Properties": {"axis": "x"}}}),
        json!({"predicate_type": "minecraft:tag_match", "tag": "minecraft:logs"}),
        json!({"predicate_type": "minecraft:random_block_match", "block": "minecraft:oak_log", "probability": 0.37}),
        json!({"predicate_type": "minecraft:random_blockstate_match", "block_state": {"Name": "minecraft:oak_log", "Properties": {"axis": "x"}}, "probability": 0.61}),
    ];
    let mut rules = Vec::new();
    for input in &tests {
        for location in &tests {
            rules.push(rule(input.clone(), location.clone(), always()));
        }
    }
    for kind in ["minecraft:linear_pos", "minecraft:axis_aligned_linear_pos"] {
        for axis in ["x", "y", "z"] {
            for (min, max) in [
                (0.0, 1.0),
                (0.85, 0.15),
                (-0.2, 1.2),
                (0.0, 0.0),
                (1.0, 1.0),
            ] {
                rules.push(rule(always(), always(), json!({"predicate_type": kind,
                    "axis": axis, "min_dist": 2, "max_dist": 12, "min_chance": min, "max_chance": max})));
            }
        }
        rules.push(rule(
            always(),
            always(),
            json!({"predicate_type": kind, "max_dist": 16}),
        ));
    }
    let mut out = Vec::new();
    for first in rules {
        for modifier in [
            "minecraft:passthrough",
            "minecraft:clear",
            "minecraft:append_loot",
        ] {
            let mut first = first.clone();
            if modifier != "minecraft:append_loot" {
                first["block_entity_modifier"] = json!({"type": modifier});
            }
            out.push(json!({"processor_type": "minecraft:rule", "rules": [first, rule(always(), always(), always())]}));
        }
    }
    // Probability endpoints still draw when the state matches.
    for probability in [0.0, 1.0] {
        out.push(json!({"processor_type": "minecraft:rule", "rules": [
            rule(json!({"predicate_type": "minecraft:random_block_match", "block": "minecraft:oak_log", "probability": probability}), always(), always()),
            rule(always(), always(), always())]}));
    }
    // Outside the distance interval vanilla returns the endpoint itself. Interpolating after
    // clamping the fraction can round to a different float and change a threshold comparison.
    let threshold = LegacyRandom::new(0).next_float();
    for kind in ["minecraft:linear_pos", "minecraft:axis_aligned_linear_pos"] {
        for chance in [threshold, f32::from_bits(threshold.to_bits() - 1)] {
            let mut first = rule(
                always(),
                always(),
                json!({"predicate_type": kind,
                "axis": "x", "min_dist": 2, "max_dist": 12, "min_chance": 16.0, "max_chance": chance}),
            );
            first["block_entity_modifier"] = json!({"type": "minecraft:clear"});
            out.push(json!({"processor_type": "minecraft:rule", "rules": [first, rule(always(), always(), always())]}));
        }
    }
    out
}

fn candidates(processor: &Value, key: &str, data: &WorldgenData) -> Vec<u32> {
    let mut states = BTreeSet::from([
        state("air"),
        state("stone"),
        state("water"),
        state("oak_log"),
    ]);
    states.insert(
        block_states()
            .state("minecraft:oak_log", [("axis", "x")])
            .unwrap()
            .0,
    );
    for rule in processor["rules"].as_array().unwrap() {
        let test = &rule[key];
        if let Some(block) = test["block"].as_str() {
            let matching: Vec<_> = (0..block_states().len())
                .filter(|&id| block_states().properties(id).unwrap().0 == block)
                .collect();
            if let Some(id) = matching.first() {
                states.insert(*id);
            }
            if let Some(id) = matching.last() {
                states.insert(*id);
            }
        }
        if let Some(tag) = test["tag"].as_str() {
            let membership = data.block_tag_states(tag).unwrap();
            let matching: Vec<_> = membership
                .iter()
                .enumerate()
                .filter(|(_, yes)| **yes)
                .map(|(id, _)| id as u32)
                .collect();
            for id in matching.iter().step_by((matching.len() / 5).max(1)) {
                states.insert(*id);
            }
        }
        if let Some(block) = test.get("block_state") {
            let def = harpas_worldgen::BlockStateDef {
                name: block["Name"].as_str().unwrap().into(),
                properties: block["Properties"]
                    .as_object()
                    .map(|p| {
                        p.iter()
                            .map(|(k, v)| (k.clone(), v.as_str().unwrap().into()))
                            .collect()
                    })
                    .unwrap_or_default(),
            };
            states.insert(resolve_block_state(&def, "parity").unwrap());
        }
    }
    states.into_iter().collect()
}

fn input_nbt(kind: usize) -> Option<Compound> {
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

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn rule_processors_match_vanilla() {
    let reference = Reference::required();
    let data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    assert_eq!(data.processor_lists.len(), 40, "26.1 processor lists");
    let mut processors = Vec::new();
    for list in data.processor_lists.values() {
        for processor in list {
            collect_processors(processor, &mut processors);
        }
    }
    let shipped = processors.len() / 2;
    assert_eq!(
        shipped, 39,
        "26.1 rule processors, including capped delegates"
    );
    processors.extend(synthetic_processors());
    let mut cases = Vec::new();
    let mut expected = Vec::new();
    let samples = if reference::reduced_sample() { 6 } else { 18 };
    for (index, json) in processors.iter().enumerate() {
        let processor = RuleProcessor::compile("parity", json, &data)
            .unwrap()
            .expect("all fixture rules compile");
        for input in candidates(json, "input_predicate", &data) {
            for location in candidates(json, "location_predicate", &data) {
                for sample in 0..samples {
                    let pos = match sample % 6 {
                        0 => [0, 0, 0],
                        1 => [15, 64, -1],
                        2 => [-17, -64, 16],
                        3 => [29_999_984, 319, -29_999_984],
                        _ => [sample * 137 - 500, sample * 19 - 70, sample * -41 + 100],
                    };
                    let distance = [-20, -12, -2, -1, 0, 1, 2, 6, 11, 12, 13, 20]
                        [(sample as usize + input as usize) % 12];
                    let mut pivot = pos;
                    pivot[(sample as usize + location as usize) % 3] += distance;
                    if sample % 4 == 3 {
                        pivot = [
                            pos[0] + distance,
                            pos[1] + distance / 2,
                            pos[2] - distance / 3,
                        ];
                    }
                    let tag = (sample as usize + input as usize + location as usize) % 3;
                    let block = ProcessorBlock {
                        pos,
                        state: input,
                        nbt: input_nbt(tag),
                    };
                    let output = processor.process(&block, location, pivot);
                    let mut snapshot: Compound = [
                        ("state", Nbt::Int(output.state as i32)),
                        ("pos", Nbt::IntArray(output.pos.to_vec())),
                    ]
                    .into_iter()
                    .collect();
                    if let Some(nbt) = output.nbt {
                        snapshot.insert("nbt", nbt);
                    }
                    expected.push(snapshot);
                    cases.push(json!([index, input, location, pos, pivot, tag]));
                }
            }
        }
    }
    let fixtures = reference.temp.0.join("rules.json");
    fs::write(
        &fixtures,
        json!({"processors": processors, "cases": cases}).to_string(),
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
    ]);
    let output = reference.temp.0.join("rules.nbt");
    reference.java(
        "RuleProcessorProbe",
        [fixtures.display().to_string(), output.display().to_string()],
    );
    let bytes = fs::read(output).unwrap();
    let (_, Nbt::Compound(root)) = Nbt::read_named(&mut bytes.as_slice()).unwrap() else {
        panic!("output compound");
    };
    let Some(Nbt::List(List::Compound(actual))) = root.get("results") else {
        panic!("output results");
    };
    assert_eq!(actual.len(), expected.len());
    for (index, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
        assert_eq!(actual, expected, "case {index}: {}", cases[index]);
    }
    eprintln!(
        "{shipped} shipped rule processors and {} synthetic processors match in {} cases",
        processors.len() - shipped * 2,
        expected.len()
    );
}
