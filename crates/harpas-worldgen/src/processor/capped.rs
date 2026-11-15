//! Whole-template limits on per-block rules, before placement clips blocks to a chunk.

use harpas_core::random::{LegacyRandom, RandomSource, WorldgenRandom};
use serde_json::Value;

use super::{ProcessorBlock, RuleProcessor, field, invalid};
use crate::data::{IntProviderDef, WorldgenData, WorldgenDataError, parse_int_provider};
use crate::feature_provider::int_bounds;
use crate::placement::sample_int_provider;

/// A `minecraft:capped` processor with a compiled `minecraft:rule` delegate.
///
/// Per-block processing leaves blocks unchanged. This processor runs during template finalization,
/// after per-block processors and before chunk clipping; its limit counts changed blocks, not attempts.
#[derive(Debug, Clone)]
pub struct CappedProcessor {
    delegate: RuleProcessor,
    limit: IntProviderDef,
    max_limit: i32,
}

impl CappedProcessor {
    /// Compiles a capped rule processor and its positive integer limit provider.
    ///
    /// Returns `None` for another type, an unported delegate or an unported integer provider.
    /// Malformed supported definitions return an error; delegates are never partially compiled.
    pub fn compile(
        context: &str,
        json: &Value,
        data: &WorldgenData,
    ) -> Result<Option<Self>, WorldgenDataError> {
        if json.get("processor_type").and_then(Value::as_str) != Some("minecraft:capped") {
            return Ok(None);
        }
        let Some(delegate) =
            RuleProcessor::compile(context, field(context, json, "delegate")?, data)?
        else {
            return Ok(None);
        };
        let limit = parse_int_provider(context, field(context, json, "limit")?)?;
        let Some((min, max_limit)) = int_bounds(&limit) else {
            return Ok(None);
        };
        if min < 1 {
            return Err(invalid(context, "capped limit must be positive"));
        }
        Ok(Some(Self {
            delegate,
            limit,
            max_limit,
        }))
    }

    /// Applies the delegate to shuffled candidates until the sampled number of blocks changes.
    ///
    /// `blocks` contains the entire template's surviving per-block results in template order. Earlier
    /// finalizers' changes must already be present. `block_at` reads existing terrain at each candidate's
    /// current position, before any template blocks are placed. The shuffle uses the world `seed` and
    /// template `origin`, independently of placement settings and decoration randomness. `pivot` is the
    /// rule predicates' reference position, which can differ from the origin.
    /// Static merges mutate existing NBT before comparison, and alone do not spend the limit.
    pub fn finalize(
        &self,
        seed: i64,
        origin: [i32; 3],
        pivot: [i32; 3],
        blocks: &mut [ProcessorBlock],
        mut block_at: impl FnMut([i32; 3]) -> u32,
    ) {
        // A clamped provider can declare a zero maximum despite sampling a positive value when
        // its source lies below the clamp interval. Vanilla uses that declared maximum here.
        if self.max_limit == 0 || blocks.is_empty() {
            return;
        }
        let [x, y, z] = origin;
        let mut random = WorldgenRandom::new(LegacyRandom::new(seed).fork_positional().at(x, y, z));
        let limit =
            (sample_int_provider(&self.limit, &mut random).unwrap() as usize).min(blocks.len());
        let mut order: Vec<_> = (0..blocks.len()).collect();
        for size in (2..=order.len()).rev() {
            let other = random.next_int_bounded(size as i32) as usize;
            order.swap(size - 1, other);
        }
        let mut changed = 0;
        for index in order {
            let block = &mut blocks[index];
            let location = block_at(block.pos);
            let output = self.delegate.process_in_place(block, location, pivot);
            if output != *block {
                blocks[index] = output;
                changed += 1;
                if changed == limit {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harpas_core::block::block_states;
    use harpas_core::nbt::{Compound, Nbt};
    use serde_json::json;

    fn state(name: &str) -> u32 {
        block_states()
            .default_state(&format!("minecraft:{name}"))
            .unwrap()
    }

    fn config(limit: Value) -> Value {
        json!({"processor_type": "minecraft:capped", "limit": limit,
            "delegate": {"processor_type": "minecraft:rule", "rules": [{
                "input_predicate": {"predicate_type": "minecraft:block_match", "block": "minecraft:stone"},
                "location_predicate": {"predicate_type": "minecraft:always_true"},
                "output_state": {"Name": "minecraft:gold_block"}}]}})
    }

    fn compile(json: &Value) -> CappedProcessor {
        CappedProcessor::compile("test", json, &WorldgenData::default())
            .unwrap()
            .unwrap()
    }

    fn blocks() -> Vec<ProcessorBlock> {
        (0..40)
            .map(|i| ProcessorBlock {
                pos: [i, 64, -17],
                state: state(if i % 3 == 0 { "stone" } else { "dirt" }),
                nbt: None,
            })
            .collect()
    }

    #[test]
    fn counts_changes_and_uses_current_states_not_attempts() {
        let mut blocks = blocks();
        let original = blocks.clone();
        let processor = compile(&config(json!(6)));
        processor.finalize(123, [-17, 64, 32], [0; 3], &mut blocks, |_| state("air"));
        assert_eq!(
            blocks
                .iter()
                .filter(|b| b.state == state("gold_block"))
                .count(),
            6
        );
        // Replaying sees the changed states and spends a new cap on remaining candidates.
        processor.finalize(123, [-17, 64, 32], [0; 3], &mut blocks, |_| state("air"));
        assert_eq!(
            blocks
                .iter()
                .filter(|b| b.state == state("gold_block"))
                .count(),
            12
        );
        processor.finalize(123, [-17, 64, 32], [0; 3], &mut blocks, |_| state("air"));
        assert_eq!(
            blocks
                .iter()
                .filter(|b| b.state == state("gold_block"))
                .count(),
            14
        );
        for (input, output) in original.iter().zip(&blocks) {
            assert_eq!(input.pos, output.pos);
            if input.state == state("dirt") {
                assert_eq!(input, output);
            }
        }
    }

    #[test]
    fn equal_outputs_do_not_count_but_nbt_only_changes_do() {
        let mut json = config(json!(3));
        let rule = &mut json["delegate"]["rules"][0];
        rule["output_state"]["Name"] = json!("minecraft:stone");
        rule["block_entity_modifier"] = json!({"type": "minecraft:clear"});
        let mut blocks = blocks();
        let stone = blocks.iter_mut().filter(|b| b.state == state("stone"));
        for (i, block) in stone.enumerate() {
            block.nbt = Some(if i < 8 {
                Compound::new()
            } else {
                [("keep", Nbt::Int(7))].into_iter().collect()
            });
        }
        compile(&json).finalize(0, [0; 3], [0; 3], &mut blocks, |_| state("air"));
        assert_eq!(
            blocks
                .iter()
                .filter(|b| b.nbt.as_ref().is_some_and(|n| n.is_empty()))
                .count(),
            11
        );
        assert_eq!(
            blocks
                .iter()
                .filter(|b| b.nbt.as_ref().is_some_and(|n| !n.is_empty()))
                .count(),
            3
        );
    }

    #[test]
    fn empty_templates_never_read_terrain() {
        compile(&config(json!(1))).finalize(0, [0; 3], [0; 3], &mut [], |_| panic!("terrain read"));
    }

    #[test]
    fn static_merges_into_existing_nbt_do_not_spend_the_cap_but_new_tags_do() {
        let mut json = config(json!(1));
        json["delegate"]["rules"][0]["output_state"]["Name"] = json!("minecraft:stone");
        json["delegate"]["rules"][0]["block_entity_modifier"] =
            json!({"type": "minecraft:append_static", "data": {"added": true}});
        let processor = compile(&json);
        for existing in [false, true] {
            let mut blocks: Vec<_> = (0..12)
                .map(|i| ProcessorBlock {
                    pos: [i, 64, 0],
                    state: state("stone"),
                    nbt: existing.then(Compound::new),
                })
                .collect();
            processor.finalize(0, [0; 3], [0; 3], &mut blocks, |_| state("air"));
            let changed = blocks
                .iter()
                .filter(|b| {
                    b.nbt
                        .as_ref()
                        .is_some_and(|tag| tag.get("added") == Some(&Nbt::Byte(1)))
                })
                .count();
            assert_eq!(changed, if existing { 12 } else { 1 });
        }
    }

    #[test]
    fn invalid_limits_and_unported_delegates_are_not_partially_compiled() {
        for limit in [
            json!(0),
            json!(-1),
            json!({"type": "minecraft:uniform", "min_inclusive": -1, "max_inclusive": 6}),
            json!({"type": "minecraft:uniform", "min_inclusive": 6, "max_inclusive": 1}),
        ] {
            let error =
                CappedProcessor::compile("test:bad", &config(limit), &WorldgenData::default())
                    .unwrap_err();
            assert!(error.to_string().contains("test:bad"));
        }
        for (key, value) in [
            ("delegate", json!({"processor_type": "test:unknown"})),
            ("limit", json!({"type": "test:unknown"})),
        ] {
            let mut json = config(json!(3));
            json[key] = value;
            assert!(
                CappedProcessor::compile("test", &json, &WorldgenData::default())
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn clamped_limit_respects_declared_maximum_with_disjoint_source_ranges() {
        for (source, expected) in [(0, 0), (9, 6)] {
            let processor = compile(&config(
                json!({"type": "minecraft:clamped", "source": source, "min_inclusive": 3, "max_inclusive": 6}),
            ));
            let mut blocks = blocks();
            processor.finalize(0, [0; 3], [0; 3], &mut blocks, |_| state("air"));
            assert_eq!(
                blocks
                    .iter()
                    .filter(|b| b.state == state("gold_block"))
                    .count(),
                expected
            );
        }
    }
}
