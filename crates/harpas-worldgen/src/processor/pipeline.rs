//! Ordered per-block filters followed by whole-template finalizers.

use harpas_core::block::block_states;
use harpas_core::math::position_seed;
use harpas_core::random::{LegacyRandom, RandomSource};
use serde_json::Value;

use super::{CappedProcessor, ProcessorBlock, RuleProcessor, field, invalid, string};
use crate::data::{WorldgenData, WorldgenDataError, parse_block_state_def, parse_holder_set};
use crate::surface::resolve_block_state;

/// A complete, ordered processor list, compiled without silently dropping unsupported entries.
///
/// Supports `rule`, rule-delegating `capped`, `block_rot`, `protected_blocks`, `block_ignore` and `nop`.
/// Processing precedes template block placement, waterlogging, shape updates and chunk clipping.
#[derive(Debug, Clone)]
pub struct ProcessorPipeline {
    processors: Vec<Processor>,
}

#[derive(Debug, Clone)]
enum Processor {
    Rule(RuleProcessor),
    Capped(CappedProcessor),
    Rot {
        rottable: Option<Vec<bool>>,
        integrity: f32,
    },
    Protected(Vec<bool>),
    Ignore(Vec<bool>),
    Nop,
}

impl ProcessorPipeline {
    /// Resolves states and tags once for every processor, retaining the supplied order.
    ///
    /// Returns `None` if any entry is unsupported, and an error for malformed supported definitions.
    pub fn compile(
        context: &str,
        entries: &[Value],
        data: &WorldgenData,
    ) -> Result<Option<Self>, WorldgenDataError> {
        let mut processors = Vec::with_capacity(entries.len());
        for json in entries {
            processors.push(match string(context, json, "processor_type")? {
                "minecraft:rule" => {
                    let Some(rule) = RuleProcessor::compile(context, json, data)? else {
                        return Ok(None);
                    };
                    Processor::Rule(rule)
                }
                "minecraft:capped" => {
                    let Some(cap) = CappedProcessor::compile(context, json, data)? else {
                        return Ok(None);
                    };
                    Processor::Capped(cap)
                }
                "minecraft:block_rot" => {
                    let integrity = field(context, json, "integrity")?
                        .as_f64()
                        .map(|v| v as f32)
                        .filter(|v| (0.0..=1.0).contains(v))
                        .ok_or_else(|| invalid(context, "invalid integrity"))?;
                    Processor::Rot {
                        rottable: json
                            .get("rottable_blocks")
                            .map(|value| membership(context, value, data))
                            .transpose()?,
                        integrity,
                    }
                }
                "minecraft:protected_blocks" => {
                    let tag = string(context, json, "value")?
                        .strip_prefix('#')
                        .ok_or_else(|| invalid(context, "protected blocks must name a tag"))?;
                    let tag = if tag.contains(':') {
                        tag.to_string()
                    } else {
                        format!("minecraft:{tag}")
                    };
                    Processor::Protected(data.block_tag_states(&tag)?)
                }
                "minecraft:block_ignore" => {
                    let blocks = field(context, json, "blocks")?
                        .as_array()
                        .ok_or_else(|| invalid(context, "ignored blocks must be an array"))?;
                    let mut names = Vec::with_capacity(blocks.len());
                    for block in blocks {
                        let def = parse_block_state_def(context, block)?;
                        resolve_block_state(&def, context)?;
                        names.push(Value::String(def.name));
                    }
                    Processor::Ignore(membership(context, &Value::Array(names), data)?)
                }
                "minecraft:nop" => Processor::Nop,
                _ => return Ok(None),
            });
        }
        Ok(Some(Self { processors }))
    }

    /// Processes all template blocks in order, then runs capped finalizers in list order.
    ///
    /// `blocks` holds original template states at transformed world positions, before state rotation.
    /// `block_at` reads terrain before placement. A supplied placement-settings random source is shared
    /// by all rot processors; `None` uses a fresh positional legacy source per call, as vanilla does.
    /// World `seed` and template `origin` independently seed capped finalization. `pivot` is the rule
    /// predicates' reference position. Neither chunk bounds nor block placement belong in this pass.
    pub fn process(
        &self,
        seed: i64,
        origin: [i32; 3],
        pivot: [i32; 3],
        blocks: &[ProcessorBlock],
        mut block_at: impl FnMut([i32; 3]) -> u32,
        mut settings_random: Option<&mut dyn RandomSource>,
    ) -> Vec<ProcessorBlock> {
        let mut output = Vec::with_capacity(blocks.len());
        'blocks: for original in blocks {
            let mut block = original.clone();
            for processor in &self.processors {
                match processor {
                    Processor::Rule(rule) => {
                        block = rule.process(&block, block_at(block.pos), pivot);
                    }
                    Processor::Protected(states) if states[block_at(block.pos) as usize] => {
                        continue 'blocks;
                    }
                    Processor::Ignore(states) if states[block.state as usize] => continue 'blocks,
                    Processor::Rot {
                        rottable,
                        integrity,
                    } if rottable
                        .as_ref()
                        .is_none_or(|states| states[original.state as usize]) =>
                    {
                        let draw = match settings_random.as_deref_mut() {
                            Some(random) => random.next_float(),
                            None => {
                                let [x, y, z] = block.pos;
                                LegacyRandom::new(position_seed(x, y, z)).next_float()
                            }
                        };
                        if draw > *integrity {
                            continue 'blocks;
                        }
                    }
                    _ => {}
                }
            }
            output.push(block);
        }
        for processor in &self.processors {
            if let Processor::Capped(cap) = processor {
                cap.finalize(seed, origin, pivot, &mut output, &mut block_at);
            }
        }
        output
    }
}

fn membership(
    context: &str,
    value: &Value,
    data: &WorldgenData,
) -> Result<Vec<bool>, WorldgenDataError> {
    let registry = block_states();
    let mut states = vec![false; registry.len() as usize];
    for name in parse_holder_set(context, value)? {
        if let Some(tag) = name.strip_prefix('#') {
            return data.block_tag_states(tag);
        }
        if registry.default_state(&name).is_none() {
            return Err(invalid(context, format!("unknown block {name}")));
        }
        for (id, member) in states.iter_mut().enumerate() {
            *member |= registry.properties(id as u32).unwrap().0 == name;
        }
    }
    Ok(states)
}

#[cfg(test)]
mod tests {
    use super::*;
    use harpas_core::nbt::{Compound, Nbt};
    use harpas_core::random::XoroshiroRandom;
    use serde_json::json;

    fn state(name: &str) -> u32 {
        block_states()
            .default_state(&format!("minecraft:{name}"))
            .unwrap()
    }

    fn compile(entries: &[Value], data: &WorldgenData) -> ProcessorPipeline {
        ProcessorPipeline::compile("test", entries, data)
            .unwrap()
            .unwrap()
    }

    fn block(name: &str, pos: [i32; 3]) -> ProcessorBlock {
        ProcessorBlock {
            pos,
            state: state(name),
            nbt: Some([("keep", Nbt::Int(7))].into_iter().collect()),
        }
    }

    fn rule(output: &str) -> Value {
        json!({"processor_type": "minecraft:rule", "rules": [{
            "input_predicate": {"predicate_type": "minecraft:always_true"},
            "location_predicate": {"predicate_type": "minecraft:always_true"},
            "output_state": {"Name": format!("minecraft:{output}")}
        }]})
    }

    #[test]
    fn ignore_matches_block_type_after_rules_and_leaves_other_air_and_nbt_intact() {
        let ignore = json!({"processor_type": "minecraft:block_ignore", "blocks": [
            {"Name": "minecraft:oak_log", "Properties": {"axis": "x"}}, {"Name": "minecraft:air"}]});
        let blocks = [
            block("oak_log", [0; 3]),
            block("air", [1; 3]),
            block("cave_air", [2; 3]),
            block("stone", [3; 3]),
        ];
        let data = WorldgenData::default();
        let pipeline = compile(std::slice::from_ref(&ignore), &data);
        assert_eq!(
            pipeline.process(0, [0; 3], [0; 3], &blocks, |_| state("air"), None),
            blocks[2..]
        );
        let pipeline = compile(&[rule("oak_log"), ignore.clone()], &data);
        assert!(
            pipeline
                .process(0, [0; 3], [0; 3], &blocks, |_| state("air"), None)
                .is_empty()
        );
        let pipeline = compile(&[ignore, rule("oak_log")], &data);
        let output = pipeline.process(0, [0; 3], [0; 3], &blocks, |_| state("air"), None);
        assert_eq!(output.len(), 2);
        assert!(output.iter().all(|b| b.state == state("oak_log")));
        assert_eq!(blocks[0].state, state("oak_log"));
    }

    #[test]
    fn protected_blocks_read_existing_terrain_and_all_tagged_states() {
        let data = WorldgenData::from_json_entries([(
            "data/test/tags/block/protected.json",
            r#"{"values":["minecraft:oak_log"]}"#,
        )])
        .unwrap();
        let pipeline = compile(
            &[
                rule("gold_block"),
                json!({"processor_type": "minecraft:protected_blocks", "value": "#test:protected"}),
            ],
            &data,
        );
        let blocks = [
            block("oak_log", [-17, 64, 32]),
            block("stone", [-16, 64, 32]),
        ];
        let horizontal_log = block_states()
            .state("minecraft:oak_log", [("axis", "x")])
            .unwrap()
            .0;
        let output = pipeline.process(
            0,
            [0; 3],
            [0; 3],
            &blocks,
            |pos| {
                if pos[0] == -16 {
                    horizontal_log
                } else {
                    state("air")
                }
            },
            None,
        );
        assert_eq!(
            output,
            [ProcessorBlock {
                state: state("gold_block"),
                ..blocks[0].clone()
            }]
        );
    }

    #[test]
    fn rot_uses_original_membership_and_retains_the_float_threshold() {
        let threshold = LegacyRandom::new(0).next_float();
        let input = block("stone", [0; 3]);
        for (integrity, survives) in [
            (threshold, true),
            (f32::from_bits(threshold.to_bits() - 1), false),
        ] {
            let pipeline = compile(
                &[
                    rule("gold_block"),
                    json!({"processor_type": "minecraft:block_rot", "integrity": integrity, "rottable_blocks": ["minecraft:stone"]}),
                ],
                &WorldgenData::default(),
            );
            let output = pipeline.process(
                0,
                [0; 3],
                [0; 3],
                std::slice::from_ref(&input),
                |_| state("air"),
                None,
            );
            assert_eq!(!output.is_empty(), survives);
            if survives {
                assert_eq!(
                    output[0],
                    ProcessorBlock {
                        state: state("gold_block"),
                        ..input.clone()
                    }
                );
            }
        }
    }

    #[test]
    fn rejected_blocks_stop_later_processors_and_empty_rot_sets_do_not_draw() {
        let pipeline = compile(
            &[
                json!({"processor_type": "minecraft:block_rot", "integrity": 0.0, "rottable_blocks": []}),
                json!({"processor_type": "minecraft:block_ignore", "blocks": [{"Name": "minecraft:stone"}]}),
                json!({"processor_type": "minecraft:block_rot", "integrity": 0.5}),
                rule("gold_block"),
            ],
            &WorldgenData::default(),
        );
        let mut random = XoroshiroRandom::new(42);
        assert!(
            pipeline
                .process(
                    0,
                    [0; 3],
                    [0; 3],
                    &[block("stone", [0; 3])],
                    |_| panic!("discarded block read terrain"),
                    Some(&mut random)
                )
                .is_empty()
        );
        assert_eq!(random.next_long(), XoroshiroRandom::new(42).next_long());
    }

    #[test]
    fn unsupported_entries_and_bad_filters_never_compile_partially() {
        let data = WorldgenData::default();
        for unknown in [
            json!({"processor_type": "test:unknown"}),
            json!({"processor_type": "minecraft:capped", "delegate": {"processor_type": "test:unknown"}, "limit": 1}),
        ] {
            assert!(
                ProcessorPipeline::compile("test", &[rule("stone"), unknown], &data)
                    .unwrap()
                    .is_none()
            );
        }
        for invalid in [
            json!({"processor_type": "minecraft:block_rot"}),
            json!({"processor_type": "minecraft:block_rot", "integrity": -0.1}),
            json!({"processor_type": "minecraft:block_rot", "integrity": 1.1}),
            json!({"processor_type": "minecraft:block_rot", "integrity": "bad"}),
            json!({"processor_type": "minecraft:block_rot", "integrity": 0.5, "rottable_blocks": ["test:unknown"]}),
            json!({"processor_type": "minecraft:block_rot", "integrity": 0.5, "rottable_blocks": ["#test:tag"]}),
            json!({"processor_type": "minecraft:block_ignore", "blocks": "minecraft:stone"}),
            json!({"processor_type": "minecraft:block_ignore", "blocks": ["minecraft:stone"]}),
            json!({"processor_type": "minecraft:protected_blocks", "value": "test:protected"}),
        ] {
            assert!(
                ProcessorPipeline::compile("test:bad", std::slice::from_ref(&invalid), &data)
                    .unwrap_err()
                    .to_string()
                    .contains("test:bad"),
                "{invalid}"
            );
        }
    }

    #[test]
    fn empty_lists_and_nop_preserve_complete_input() {
        let blocks = [
            block("chest", [15, 319, -17]),
            ProcessorBlock {
                pos: [-17, -64, 32],
                state: state("stone"),
                nbt: Some(Compound::new()),
            },
        ];
        for entries in [vec![], vec![json!({"processor_type": "minecraft:nop"})]] {
            assert_eq!(
                compile(&entries, &WorldgenData::default()).process(
                    0,
                    [0; 3],
                    [0; 3],
                    &blocks,
                    |_| panic!("noop read terrain"),
                    None
                ),
                blocks
            );
        }
    }
}
