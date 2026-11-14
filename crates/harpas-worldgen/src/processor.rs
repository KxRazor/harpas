//! Template rules, block filters and capped substitutions, observed through vanilla's processor APIs.
//!
//! This is the processing part of jigsaw placement. Processor lists retain their other processor
//! types in [`WorldgenData`]; a list with an unported processor does not compile partially.

mod capped;
mod pipeline;
pub use capped::CappedProcessor;
pub use pipeline::ProcessorPipeline;

use harpas_core::math::position_seed;
use harpas_core::nbt::{Compound, Nbt};
use harpas_core::random::{LegacyRandom, RandomSource};
use serde_json::Value;

use crate::data::{WorldgenData, WorldgenDataError, parse_block_state_def, parse_rule_test};
use crate::feature::{RuleTestRule, compile_rule_test};
use crate::surface::resolve_block_state;

/// A template block after its position has been transformed into world coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessorBlock {
    /// World position; processors that run earlier may have moved it.
    pub pos: [i32; 3],
    /// State before the template's mirror and rotation are applied to its properties.
    pub state: u32,
    /// Saved block entity data, when present.
    pub nbt: Option<Compound>,
}

/// The ordered substitutions of a `minecraft:rule` template processor.
#[derive(Debug, Clone)]
pub struct RuleProcessor {
    rules: Vec<Rule>,
}

#[derive(Debug, Clone)]
struct Rule {
    input: RuleTestRule,
    location: RuleTestRule,
    position: PositionTest,
    state: u32,
    modifier: EntityModifier,
}

#[derive(Debug, Clone)]
enum EntityModifier {
    Passthrough,
    Clear,
    AppendLoot(String),
}

#[derive(Debug, Clone)]
enum PositionTest {
    Always,
    Linear {
        axis: Option<usize>,
        min_dist: i32,
        max_dist: i32,
        min_chance: f32,
        max_chance: f32,
    },
}

impl RuleProcessor {
    /// Compiles a rule processor from its JSON, resolving states and block tags once.
    ///
    /// Returns `None` for another processor type or an unported predicate or block entity modifier.
    /// Malformed supported definitions return an error. No rules are silently dropped.
    pub fn compile(
        context: &str,
        json: &Value,
        data: &WorldgenData,
    ) -> Result<Option<Self>, WorldgenDataError> {
        if json.get("processor_type").and_then(Value::as_str) != Some("minecraft:rule") {
            return Ok(None);
        }
        let entries = json
            .get("rules")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid(context, "rule processor has no rules"))?;
        let mut rules = Vec::with_capacity(entries.len());
        for entry in entries {
            let predicate = |key| {
                compile_rule_test(
                    context,
                    &parse_rule_test(context, field(context, entry, key)?)?,
                    data,
                )
            };
            let (Some(input), Some(location)) = (
                predicate("input_predicate")?,
                predicate("location_predicate")?,
            ) else {
                return Ok(None);
            };
            let Some(position) = compile_position(context, entry.get("position_predicate"))? else {
                return Ok(None);
            };
            let modifier = entry.get("block_entity_modifier");
            let modifier = match modifier
                .map(|value| string(context, value, "type"))
                .transpose()?
                .unwrap_or("minecraft:passthrough")
            {
                "minecraft:passthrough" => EntityModifier::Passthrough,
                "minecraft:clear" => EntityModifier::Clear,
                "minecraft:append_loot" => {
                    let table = string(context, modifier.unwrap(), "loot_table")?;
                    EntityModifier::AppendLoot(if table.contains(':') {
                        table.into()
                    } else {
                        format!("minecraft:{table}")
                    })
                }
                _ => return Ok(None),
            };
            rules.push(Rule {
                input,
                location,
                position,
                state: resolve_block_state(
                    &parse_block_state_def(context, field(context, entry, "output_state")?)?,
                    context,
                )?,
                modifier,
            });
        }
        Ok(Some(Self { rules }))
    }

    /// Processes a block, stopping at the first matching rule (`RuleProcessor.processBlock`).
    ///
    /// `location_state` is the world's existing state at `block.pos`. `pivot` is the structure's
    /// placement reference position, used by distance predicates. The processor creates its own legacy
    /// random source from the transformed block position; decoration randomness is not consumed.
    pub fn process(
        &self,
        block: &ProcessorBlock,
        location_state: u32,
        pivot: [i32; 3],
    ) -> ProcessorBlock {
        let [x, y, z] = block.pos;
        let mut random = LegacyRandom::new(position_seed(x, y, z));
        for rule in &self.rules {
            if rule.input.matches(block.state, &mut random)
                && rule.location.matches(location_state, &mut random)
                && rule.position.test(block.pos, pivot, &mut random)
            {
                let nbt = match &rule.modifier {
                    EntityModifier::Passthrough => block.nbt.clone(),
                    EntityModifier::Clear => Some(Compound::new()),
                    EntityModifier::AppendLoot(table) => {
                        let mut tag = block.nbt.clone().unwrap_or_default();
                        tag.insert("LootTable", Nbt::String(table.clone()));
                        tag.insert("LootTableSeed", Nbt::Long(random.next_long()));
                        Some(tag)
                    }
                };
                return ProcessorBlock {
                    pos: block.pos,
                    state: rule.state,
                    nbt,
                };
            }
        }
        block.clone()
    }
}

impl PositionTest {
    fn test(&self, pos: [i32; 3], pivot: [i32; 3], random: &mut impl RandomSource) -> bool {
        match *self {
            Self::Always => true,
            Self::Linear {
                axis,
                min_dist,
                max_dist,
                min_chance,
                max_chance,
            } => {
                let delta: [i32; 3] =
                    std::array::from_fn(|i| pos[i].wrapping_sub(pivot[i]).wrapping_abs());
                let distance = axis.map_or_else(
                    || delta.into_iter().fold(0i32, i32::wrapping_add),
                    |axis| delta[axis],
                );
                let fraction =
                    (distance as f32 - min_dist as f32) / (max_dist as f32 - min_dist as f32);
                // Return the endpoint itself outside the range. Clamping the fraction before lerp
                // can round differently, changing the result when a float lands on the threshold.
                let chance = if fraction < 0.0 {
                    min_chance
                } else if fraction > 1.0 {
                    max_chance
                } else {
                    min_chance + fraction * (max_chance - min_chance)
                };
                random.next_float() <= chance
            }
        }
    }
}

fn compile_position(
    context: &str,
    json: Option<&Value>,
) -> Result<Option<PositionTest>, WorldgenDataError> {
    let Some(json) = json else {
        return Ok(Some(PositionTest::Always));
    };
    let kind = string(context, json, "predicate_type")?;
    if kind == "minecraft:always_true" {
        return Ok(Some(PositionTest::Always));
    }
    if !matches!(
        kind,
        "minecraft:linear_pos" | "minecraft:axis_aligned_linear_pos"
    ) {
        return Ok(None);
    }
    let int = |key, default| -> Result<i32, WorldgenDataError> {
        json.get(key).map_or(Ok(default), |v| {
            v.as_i64()
                .and_then(|n| i32::try_from(n).ok())
                .ok_or_else(|| invalid(context, format!("invalid `{key}`")))
        })
    };
    let float = |key, default| -> Result<f32, WorldgenDataError> {
        json.get(key).map_or(Ok(default), |v| {
            v.as_f64()
                .map(|n| n as f32)
                .filter(|n| n.is_finite())
                .ok_or_else(|| invalid(context, format!("invalid `{key}`")))
        })
    };
    let min_dist = int("min_dist", 0)?;
    let max_dist = int("max_dist", 0)?;
    if min_dist >= max_dist {
        return Err(invalid(context, "min_dist must be less than max_dist"));
    }
    let axis = if kind == "minecraft:axis_aligned_linear_pos" {
        Some(
            match json.get("axis").and_then(Value::as_str).unwrap_or("y") {
                "x" => 0,
                "y" => 1,
                "z" => 2,
                _ => return Err(invalid(context, "invalid position predicate axis")),
            },
        )
    } else {
        None
    };
    Ok(Some(PositionTest::Linear {
        axis,
        min_dist,
        max_dist,
        min_chance: float("min_chance", 0.0)?,
        max_chance: float("max_chance", 0.0)?,
    }))
}

fn invalid(context: &str, message: impl Into<String>) -> WorldgenDataError {
    WorldgenDataError::InvalidDefinition {
        context: context.into(),
        message: message.into(),
    }
}

fn field<'a>(context: &str, json: &'a Value, key: &str) -> Result<&'a Value, WorldgenDataError> {
    json.get(key)
        .ok_or_else(|| invalid(context, format!("missing `{key}`")))
}

fn string<'a>(context: &str, json: &'a Value, key: &str) -> Result<&'a str, WorldgenDataError> {
    field(context, json, key)?
        .as_str()
        .ok_or_else(|| invalid(context, format!("invalid `{key}`")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use harpas_core::block::block_states;
    use serde_json::json;

    fn state(name: &str) -> u32 {
        block_states()
            .default_state(&format!("minecraft:{name}"))
            .unwrap()
    }

    fn rule(input: Value, output: &str) -> Value {
        json!({
            "input_predicate": input,
            "location_predicate": {"predicate_type": "minecraft:always_true"},
            "output_state": {"Name": format!("minecraft:{output}")}
        })
    }

    fn compile(rules: Vec<Value>) -> RuleProcessor {
        RuleProcessor::compile(
            "test",
            &json!({"processor_type": "minecraft:rule", "rules": rules}),
            &WorldgenData::default(),
        )
        .unwrap()
        .unwrap()
    }

    #[test]
    fn lists_retain_order_and_unported_processors() {
        let json = json!({"processors": [
            {"processor_type": "minecraft:capped", "limit": 6},
            {"processor_type": "minecraft:rule", "rules": []}
        ]});
        let data = WorldgenData::from_json_entries([(
            "data/test/worldgen/processor_list/nested/test.json",
            json.to_string(),
        )])
        .unwrap();
        assert_eq!(
            data.processor_lists["test:nested/test"],
            json["processors"].as_array().unwrap().clone()
        );
        assert!(
            WorldgenData::from_json_entries([("data/test/worldgen/processor_list/bad.json", "{}")])
                .is_err()
        );
    }

    #[test]
    fn first_match_uses_current_state_and_existing_terrain_and_preserves_nbt() {
        let mut first = rule(
            json!({"predicate_type": "minecraft:block_match", "block": "minecraft:stone"}),
            "sand",
        );
        first["location_predicate"] =
            json!({"predicate_type": "minecraft:block_match", "block": "minecraft:water"});
        let processor = compile(vec![
            first,
            rule(
                json!({"predicate_type": "minecraft:always_true"}),
                "gold_block",
            ),
        ]);
        let block = ProcessorBlock {
            pos: [-17, 65, 32],
            state: state("stone"),
            nbt: Some([("keep", Nbt::Int(7))].into_iter().collect()),
        };
        let output = processor.process(&block, state("water"), [0; 3]);
        assert_eq!(
            output,
            ProcessorBlock {
                state: state("sand"),
                ..block.clone()
            }
        );
        assert_eq!(
            processor.process(&block, state("air"), [0; 3]).state,
            state("gold_block")
        );
        let empty = compile(vec![]);
        assert_eq!(empty.process(&block, state("air"), [0; 3]), block);
    }

    #[test]
    fn failed_block_match_does_not_consume_its_probability_draw() {
        let failed = rule(
            json!({"predicate_type": "minecraft:random_block_match", "block": "minecraft:dirt", "probability": 1.0}),
            "sand",
        );
        let mut fallback = rule(json!({"predicate_type": "minecraft:always_true"}), "chest");
        fallback["block_entity_modifier"] =
            json!({"type": "minecraft:append_loot", "loot_table": "chests/test"});
        let processor = compile(vec![failed, fallback]);
        let pos = [-17, 65, 32];
        let original: Compound = [("keep", Nbt::Int(7))].into_iter().collect();
        let block = ProcessorBlock {
            pos,
            state: state("stone"),
            nbt: Some(original.clone()),
        };
        let output = processor.process(&block, state("air"), [0; 3]);
        let mut expected = original;
        expected.insert("LootTable", "minecraft:chests/test");
        expected.insert(
            "LootTableSeed",
            LegacyRandom::new(position_seed(pos[0], pos[1], pos[2])).next_long(),
        );
        assert_eq!(output.nbt, Some(expected));
        assert_eq!(
            block.nbt.as_ref().unwrap().len(),
            1,
            "input NBT was mutated"
        );
    }

    #[test]
    fn clear_uses_an_empty_compound_and_unmatched_rules_leave_nbt_untouched() {
        let mut entry = rule(
            json!({"predicate_type": "minecraft:block_match", "block": "minecraft:stone"}),
            "sand",
        );
        entry["block_entity_modifier"] = json!({"type": "minecraft:clear"});
        let processor = compile(vec![entry]);
        let block = ProcessorBlock {
            pos: [0, 64, 0],
            state: state("stone"),
            nbt: Some([("discard", Nbt::Int(7))].into_iter().collect()),
        };
        assert_eq!(
            processor.process(&block, state("air"), [0; 3]).nbt,
            Some(Compound::new())
        );
        let unmatched = ProcessorBlock {
            state: state("dirt"),
            ..block
        };
        assert_eq!(
            processor.process(&unmatched, state("air"), [0; 3]),
            unmatched
        );
    }

    #[test]
    fn unsupported_rule_parts_do_not_compile_as_partial_processors() {
        let known = rule(json!({"predicate_type": "minecraft:always_true"}), "sand");
        for key in [
            "input_predicate",
            "location_predicate",
            "position_predicate",
            "block_entity_modifier",
        ] {
            let mut unknown = known.clone();
            unknown[key] = json!({"predicate_type": "test:unknown", "type": "test:unknown"});
            assert!(
                RuleProcessor::compile(
                    "test",
                    &json!({"processor_type": "minecraft:rule", "rules": [known.clone(), unknown]}),
                    &WorldgenData::default()
                )
                .unwrap()
                .is_none(),
                "{key}"
            );
        }
    }

    #[test]
    fn malformed_supported_rules_report_the_definition() {
        for position in [
            json!({"predicate_type": "minecraft:linear_pos"}),
            json!({"predicate_type": "minecraft:linear_pos", "min_dist": 4, "max_dist": 4}),
            json!({"predicate_type": "minecraft:axis_aligned_linear_pos", "axis": "w", "max_dist": 16}),
        ] {
            let mut entry = rule(json!({"predicate_type": "minecraft:always_true"}), "sand");
            entry["position_predicate"] = position;
            let error = RuleProcessor::compile(
                "test:bad",
                &json!({"processor_type": "minecraft:rule", "rules": [entry]}),
                &WorldgenData::default(),
            )
            .unwrap_err();
            assert!(error.to_string().contains("test:bad"));
        }
    }

    #[test]
    fn distance_chances_use_exact_endpoints_outside_the_interval() {
        let threshold = LegacyRandom::new(0).next_float();
        for (chance, expected) in [
            (threshold, "sand"),
            (f32::from_bits(threshold.to_bits() - 1), "gold_block"),
        ] {
            let mut first = rule(json!({"predicate_type": "minecraft:always_true"}), "sand");
            first["position_predicate"] = json!({"predicate_type": "minecraft:linear_pos",
                "min_dist": 2, "max_dist": 12, "min_chance": 16.0, "max_chance": chance});
            let processor = compile(vec![
                first,
                rule(
                    json!({"predicate_type": "minecraft:always_true"}),
                    "gold_block",
                ),
            ]);
            let block = ProcessorBlock {
                pos: [0; 3],
                state: state("stone"),
                nbt: None,
            };
            assert_eq!(
                processor.process(&block, state("air"), [-20, 0, 0]).state,
                state(expected)
            );
        }
    }
}
