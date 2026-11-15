//! Runtime-added material processors compared with vanilla's complete template processing pass.
mod reference;

use std::collections::{BTreeSet, HashMap};
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

fn rule(output: &str) -> Value {
    json!({"processor_type": "minecraft:rule", "rules": [{
        "input_predicate": {"predicate_type": "minecraft:always_true"},
        "location_predicate": {"predicate_type": "minecraft:always_true"},
        "output_state": {"Name": format!("minecraft:{output}")},
        "block_entity_modifier": {"type": "minecraft:append_loot", "loot_table": "minecraft:chests/test"}
    }]})
}

fn pipelines() -> Vec<Vec<Value>> {
    let age = |mossiness| json!({"processor_type": "minecraft:block_age", "mossiness": mossiness});
    let blackstone = json!({"processor_type": "minecraft:blackstone_replace"});
    let lava = json!({"processor_type": "minecraft:lava_submerged_block"});
    let mut out: Vec<_> = [-1e100, 0.0, 0.2, 1.0, 1e100]
        .into_iter()
        .map(|m| vec![age(m)])
        .collect();
    out.extend([vec![blackstone.clone()], vec![lava.clone()]]);
    let rot = json!({"processor_type": "minecraft:block_rot", "integrity": 0.5});
    let ignore =
        json!({"processor_type": "minecraft:block_ignore", "blocks": [{"Name": "minecraft:lava"}]});
    let protected = json!({"processor_type": "minecraft:protected_blocks", "value": "#minecraft:features_cannot_replace"});
    let cap = json!({"processor_type": "minecraft:capped", "delegate": rule("suspicious_gravel"), "limit": 3});
    for material in [age(0.6), blackstone.clone(), lava.clone()] {
        for other in [
            age(0.2),
            blackstone.clone(),
            lava.clone(),
            rot.clone(),
            ignore.clone(),
            protected.clone(),
            cap.clone(),
            rule("stone"),
        ] {
            out.push(vec![material.clone(), other.clone()]);
            out.push(vec![other, material.clone()]);
        }
    }
    out.push(vec![rot, age(1.0), lava, blackstone, protected, cap]);
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
fn material_processors_and_ordered_pipelines_match_vanilla() {
    let reference = Reference::required();
    let data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let pipelines = pipelines();
    let compiled: Vec<_> = pipelines
        .iter()
        .map(|entries| {
            ProcessorPipeline::compile("materials parity", entries, &data)
                .unwrap()
                .unwrap()
        })
        .collect();
    let seeds = reference::sampled_seeds(&[0, -91, 0x1234_5678_abcd_ef01]);
    let origins = [[0, 0, 0], [-17, -64, 32], [29_999_984, 319, -29_999_984]];
    let mut states = BTreeSet::new();
    // The reduced sample retains each block type plus nondefault stair, slab and wall properties.
    for id in 0..block_states().len() {
        let (name, props) = block_states().properties(id).unwrap();
        if block_states().default_state(name) == Some(id)
            || (id % 31 == 0
                && props
                    .iter()
                    .any(|(k, _)| matches!(*k, "half" | "type" | "up")))
        {
            states.insert(id);
        }
    }
    let states: Vec<_> = states.into_iter().collect();
    let mut cases = Vec::new();
    let mut expected = Vec::new();
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
            Nbt::List(if output.is_empty() {
                List::Empty
            } else {
                List::Compound(output.into_iter().map(snapshot).collect())
            }),
        )]
        .into_iter()
        .collect();
        if explicit_random {
            result.insert("random", random.next_long());
        }
        expected.push(result);
        cases.push(json!({"pipeline": index, "seed": seed, "origin": origin, "random": explicit_random, "blocks": inputs}));
    };
    // Every state appears as an incoming block for each material processor, and as terrain for lava.
    let coverage: Vec<_> = if reference::reduced_sample() {
        states.clone()
    } else {
        (0..block_states().len()).collect()
    };
    for index in 0..7 {
        for (part, chunk) in coverage.chunks(81).enumerate() {
            for explicit_random in [false, true] {
                add_case(
                    index,
                    seeds[0],
                    origins[part % 3],
                    explicit_random,
                    chunk.iter().map(|&id| (id, state("lava"))).collect(),
                );
                if index == 6 {
                    add_case(
                        index,
                        seeds[0],
                        origins[part % 3],
                        explicit_random,
                        chunk
                            .iter()
                            .enumerate()
                            .map(|(i, &id)| {
                                (state(if i % 2 == 0 { "stone" } else { "oak_stairs" }), id)
                            })
                            .collect(),
                    );
                }
            }
        }
    }
    let incoming = [
        "stone",
        "stone_bricks",
        "chiseled_stone_bricks",
        "cobblestone_stairs",
        "mossy_stone_brick_stairs",
        "stone_slab",
        "cobblestone_wall",
        "obsidian",
        "iron_bars",
        "chest",
        "gold_block",
        "air",
    ];
    let terrain = ["lava", "water", "bedrock", "stone", "air"];
    for index in 0..pipelines.len() {
        for &seed in seeds {
            for origin in origins {
                for length in [0, 1, 81] {
                    for explicit_random in [false, true] {
                        add_case(
                            index,
                            seed,
                            origin,
                            explicit_random,
                            (0..length)
                                .map(|i| {
                                    (
                                        state(incoming[(i + index) % incoming.len()]),
                                        state(terrain[i % terrain.len()]),
                                    )
                                })
                                .collect(),
                        );
                    }
                }
            }
        }
    }
    let fixtures = reference.temp.0.join("materials.json");
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
    let output = reference.temp.0.join("materials.nbt");
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
    let mut mismatches = 0;
    for (i, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
        if actual != expected {
            if mismatches < 20 {
                let blocks = |c: &Compound| match c.get("blocks").unwrap() {
                    Nbt::List(List::Compound(blocks)) => blocks.clone(),
                    Nbt::List(List::Empty) => Vec::new(),
                    _ => panic!("output blocks"),
                };
                let actual_blocks = blocks(actual);
                let expected_blocks = blocks(expected);
                let diff = actual_blocks
                    .iter()
                    .zip(&expected_blocks)
                    .position(|(a, e)| a != e);
                let describe = |b: Option<&Compound>| {
                    b.map(|b| match b.get("state") {
                        Some(Nbt::Int(id)) => format!(
                            "{:?}, nbt {:?}",
                            block_states().properties(*id as u32),
                            b.get("nbt")
                        ),
                        _ => format!("{b:?}"),
                    })
                };
                eprintln!(
                    "case {i}, pipeline {}, block {diff:?}: vanilla {:?}, Harpas {:?}; random {:?} / {:?}",
                    cases[i]["pipeline"],
                    describe(diff.and_then(|n| actual_blocks.get(n))),
                    describe(diff.and_then(|n| expected_blocks.get(n))),
                    actual.get("random"),
                    expected.get("random")
                );
            }
            mismatches += 1;
        }
    }
    assert_eq!(mismatches, 0, "mismatching material processing passes");
    eprintln!(
        "{} material pipelines match vanilla in {} cases",
        pipelines.len(),
        cases.len()
    );
}
