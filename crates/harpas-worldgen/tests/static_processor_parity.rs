//! Static rule NBT compared with vanilla's whole-template processing and capped finalization.
mod reference;

use std::fs;

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_core::random::{RandomSource, XoroshiroRandom};
use harpas_worldgen::WorldgenData;
use harpas_worldgen::processor::{ProcessorBlock, ProcessorPipeline};
use reference::Reference;
use serde_json::{Value, json};

fn state(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}

fn rule(modifier: Value, output: &str) -> Value {
    json!({"processor_type": "minecraft:rule", "rules": [{
        "input_predicate": {"predicate_type": "minecraft:always_true"},
        "location_predicate": {"predicate_type": "minecraft:always_true"},
        "output_state": {"Name": format!("minecraft:{output}")},
        "block_entity_modifier": modifier
    }]})
}

fn pipelines() -> Vec<Vec<Value>> {
    let mut out = Vec::new();
    for data in [
        json!({}),
        json!({"keep": {"added": true, "nested": "replacement"}, "LootTable": {"nested": 7}}),
        json!({"keep": 7, "LootTableSeed": false, "empty": {}, "text": "raíces 水"}),
        json!({"numbers": [-129, -128, 127, 128, -32769, -32768, 32767, 32768,
            i32::MIN, i32::MAX, i64::MIN, i64::MAX, 0.0, 1.0, 0.5, 0.1, 1e30, u64::MAX]}),
        json!({"lists": [[], [1, 2], [128, 129], [32768, 32769], [2147483648i64],
            [0.5, 0.25], [0.1, 0.2], [true, false], ["a", "b"], [{}, {"x": 7}],
            [[1], ["a"]], [1, "a", {}, [], {"": 7}], [1, 128],
            [{"": 7}], [{"": 7}, {}], [{"": {}, "other": 2}], [{"": {"": 8}}]]}),
    ] {
        let static_rule = rule(
            json!({"type": "minecraft:append_static", "data": data}),
            "stone",
        );
        out.push(vec![static_rule.clone()]);
        out.push(vec![json!({"processor_type": "minecraft:capped", "delegate": static_rule.clone(), "limit": 1})]);
        let mut changed = static_rule.clone();
        changed["rules"][0]["output_state"]["Name"] = json!("minecraft:chest");
        out.push(vec![
            json!({"processor_type": "minecraft:capped", "delegate": changed, "limit": 3}),
        ]);
        for other in [
            rule(json!({"type": "minecraft:clear"}), "stone"),
            rule(
                json!({"type": "minecraft:append_loot", "loot_table": "minecraft:chests/test"}),
                "stone",
            ),
            rule(
                json!({"type": "minecraft:append_static", "data": {"keep": {"last": 9}, "numbers": []}}),
                "stone",
            ),
            json!({"processor_type": "minecraft:block_rot", "integrity": 0.5}),
            json!({"processor_type": "minecraft:capped", "delegate": static_rule.clone(), "limit": 2}),
        ] {
            out.push(vec![static_rule.clone(), other.clone()]);
            out.push(vec![other, static_rule.clone()]);
        }
    }
    // A failed rule must neither merge its NBT nor prevent the fallback's loot seed draw.
    let mut failed = rule(
        json!({"type": "minecraft:append_static", "data": {"never": true}}),
        "stone",
    );
    failed["rules"][0]["input_predicate"] = json!({"predicate_type": "minecraft:random_block_match", "block": "minecraft:stone", "probability": 0.0});
    failed["rules"].as_array_mut().unwrap().push(
        rule(
            json!({"type": "minecraft:append_loot", "loot_table": "minecraft:chests/test"}),
            "stone",
        )["rules"][0]
            .clone(),
    );
    out.push(vec![failed]);
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

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn static_merges_and_capped_pipeline_order_match_vanilla() {
    let reference = Reference::required();
    let data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let pipelines = pipelines();
    let compiled: Vec<_> = pipelines
        .iter()
        .map(|entries| {
            ProcessorPipeline::compile("static parity", entries, &data)
                .unwrap()
                .unwrap()
        })
        .collect();
    let mut cases = Vec::new();
    let mut expected = Vec::new();
    for (index, pipeline) in compiled.iter().enumerate() {
        for &seed in reference::sampled_seeds(&[0, -91, 0x1234_5678_abcd_ef01]) {
            for origin in [[0, 0, 0], [-17, -64, 32], [29_999_984, 319, -29_999_984]] {
                for mode in 0..4 {
                    for length in [0, 1, 17] {
                        for explicit in [false, true] {
                            let mut inputs = Vec::new();
                            let mut blocks = Vec::new();
                            for i in 0..length {
                                let pos = [origin[0] + i, origin[1] + i % 3, origin[2] - i];
                                let kind = if mode == 3 { i as usize % 3 } else { mode };
                                inputs.push(json!([pos, state("stone"), kind, state("air")]));
                                blocks.push(ProcessorBlock {
                                    pos,
                                    state: state("stone"),
                                    nbt: tag(kind),
                                });
                            }
                            let mut random = XoroshiroRandom::new(seed);
                            let pivot = [origin[0] + 7, origin[1] - 3, origin[2] + 2];
                            let result = pipeline.process(
                                seed,
                                origin,
                                pivot,
                                &blocks,
                                |_| state("air"),
                                explicit.then_some(&mut random),
                            );
                            let snapshots: Vec<Compound> = result
                                .into_iter()
                                .map(|block| {
                                    let mut tag: Compound = [
                                        ("state", Nbt::Int(block.state as i32)),
                                        ("pos", Nbt::IntArray(block.pos.to_vec())),
                                    ]
                                    .into_iter()
                                    .collect();
                                    if let Some(nbt) = block.nbt {
                                        tag.insert("nbt", nbt);
                                    }
                                    tag
                                })
                                .collect();
                            let list = if snapshots.is_empty() {
                                List::Empty
                            } else {
                                List::Compound(snapshots)
                            };
                            let mut snapshot: Compound =
                                [("blocks", Nbt::List(list))].into_iter().collect();
                            if explicit {
                                snapshot.insert("random", random.next_long());
                            }
                            expected.push(snapshot);
                            cases.push(json!({"pipeline": index, "seed": seed, "origin": origin, "random": explicit, "blocks": inputs}));
                        }
                    }
                }
            }
        }
    }
    let fixtures = reference.temp.0.join("static.json");
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
    let output = reference.temp.0.join("static.nbt");
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
    for (index, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
        assert_eq!(actual, expected, "case {index}: {}", cases[index]);
    }
    eprintln!(
        "{} static pipelines match vanilla in {} cases",
        pipelines.len(),
        cases.len()
    );
}
