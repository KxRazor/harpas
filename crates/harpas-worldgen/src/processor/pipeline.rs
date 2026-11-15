//! Ordered per-block filters followed by whole-template finalizers.

use harpas_core::block::block_states;
use harpas_core::math::position_seed;
use harpas_core::random::{LegacyRandom, RandomSource};
use serde_json::Value;

use super::material::{BlockAge, blackstone, full_shape};
use super::{CappedProcessor, ProcessorBlock, RuleProcessor, field, invalid, string};
use crate::data::{WorldgenData, WorldgenDataError, parse_block_state_def, parse_holder_set};
use crate::generator::HeightmapType;
use crate::surface::resolve_block_state;

/// Terrain read by template processors, before any of the template's blocks are placed.
pub trait ProcessorWorld {
    /// Existing block state at the current, possibly processor-adjusted world position.
    fn block_at(&mut self, pos: [i32; 3]) -> u32;

    /// First available Y in a column, as returned by vanilla's `LevelReader.getHeight`.
    fn heightmap(&mut self, kind: HeightmapType, x: i32, z: i32) -> i32;

    /// Whether the incoming state's outline fills a block (`Block.isShapeFullBlock(getShape)`).
    ///
    /// The default uses generation shapes without active block entities. Worlds with dynamic shapes,
    /// such as an open shulker box, can override this query at the current transformed position.
    fn is_shape_full_block(&mut self, state: u32, _pos: [i32; 3]) -> bool {
        full_shape(state)
    }

    /// Whether placement targets a live level (`ServerLevel`) instead of a generation region.
    ///
    /// Gravity substitutes the live heightmaps for their worldgen variants in a live level.
    fn is_server_level(&self) -> bool {
        false
    }
}

/// A complete, ordered processor list, compiled without silently dropping unsupported entries.
///
/// Supports `rule`, rule-delegating `capped`, `block_rot`, `protected_blocks`, `block_ignore`,
/// `gravity`, `block_age`, `blackstone_replace`, `lava_submerged_block` and `nop`.
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
    Age(BlockAge),
    Blackstone,
    LavaSubmerged,
    Gravity {
        heightmap: HeightmapType,
        offset: i32,
    },
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
                "minecraft:gravity" => {
                    let heightmap = match json.get("heightmap") {
                        None => HeightmapType::WorldSurfaceWg,
                        Some(value) => HeightmapType::ALL
                            .into_iter()
                            .find(|kind| value.as_str() == Some(kind.nbt_key()))
                            .ok_or_else(|| invalid(context, "unknown gravity heightmap"))?,
                    };
                    let offset = match json.get("offset") {
                        None => 0,
                        Some(value) => value
                            .as_i64()
                            .and_then(|v| i32::try_from(v).ok())
                            .ok_or_else(|| invalid(context, "invalid gravity offset"))?,
                    };
                    Processor::Gravity { heightmap, offset }
                }
                "minecraft:block_age" => {
                    let mossiness = field(context, json, "mossiness")?
                        .as_f64()
                        .map(|v| v as f32)
                        .ok_or_else(|| invalid(context, "invalid mossiness"))?;
                    Processor::Age(BlockAge::new(mossiness, data)?)
                }
                "minecraft:blackstone_replace" => Processor::Blackstone,
                "minecraft:lava_submerged_block" => Processor::LavaSubmerged,
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
    /// by all rot and age processors; `None` uses a fresh positional legacy source per call, as vanilla does.
    /// World `seed` and template `origin` independently seed capped finalization. `pivot` is the rule
    /// predicates' reference position. Neither chunk bounds nor block placement belong in this pass.
    ///
    /// # Panics
    ///
    /// If a surviving block reaches a gravity processor. Use [`Self::process_in_world`] for lists
    /// that require heightmaps.
    pub fn process(
        &self,
        seed: i64,
        origin: [i32; 3],
        pivot: [i32; 3],
        blocks: &[ProcessorBlock],
        block_at: impl FnMut([i32; 3]) -> u32,
        settings_random: Option<&mut dyn RandomSource>,
    ) -> Vec<ProcessorBlock> {
        self.process_in_world(
            seed,
            origin,
            pivot,
            blocks,
            &mut BlockReader(block_at),
            settings_random,
        )
    }

    /// Processes a complete template using block and heightmap reads from the supplied world.
    ///
    /// Like [`Self::process`], but also supports gravity. Gravity preserves each block's original
    /// local Y (`blocks[i].pos[1] - origin[1]`), independently of earlier processors' movement.
    /// Later filters, rules, positional random sources and finalizers read the adjusted position.
    pub fn process_in_world(
        &self,
        seed: i64,
        origin: [i32; 3],
        pivot: [i32; 3],
        blocks: &[ProcessorBlock],
        world: &mut impl ProcessorWorld,
        mut settings_random: Option<&mut dyn RandomSource>,
    ) -> Vec<ProcessorBlock> {
        let mut output = Vec::with_capacity(blocks.len());
        'blocks: for original in blocks {
            let mut block = original.clone();
            for processor in &self.processors {
                match processor {
                    Processor::Rule(rule) => {
                        let location = world.block_at(block.pos);
                        block = rule.process_in_place(&mut block, location, pivot);
                    }
                    Processor::Protected(states) if states[world.block_at(block.pos) as usize] => {
                        continue 'blocks;
                    }
                    Processor::Ignore(states) if states[block.state as usize] => continue 'blocks,
                    Processor::Age(age) => {
                        block.state = match settings_random.as_deref_mut() {
                            Some(random) => age.process(block.state, random),
                            None => {
                                let [x, y, z] = block.pos;
                                age.process(
                                    block.state,
                                    &mut LegacyRandom::new(position_seed(x, y, z)),
                                )
                            }
                        };
                    }
                    Processor::Blackstone => block.state = blackstone(block.state),
                    Processor::LavaSubmerged => {
                        let terrain = world.block_at(block.pos);
                        if block_states().properties(terrain).unwrap().0 == "minecraft:lava"
                            && !world.is_shape_full_block(block.state, block.pos)
                        {
                            block.state = block_states().default_state("minecraft:lava").unwrap();
                        }
                    }
                    Processor::Gravity { heightmap, offset } => {
                        let heightmap = match (*heightmap, world.is_server_level()) {
                            (HeightmapType::WorldSurfaceWg, true) => HeightmapType::WorldSurface,
                            (HeightmapType::OceanFloorWg, true) => HeightmapType::OceanFloor,
                            (kind, _) => kind,
                        };
                        let height = world.heightmap(heightmap, block.pos[0], block.pos[2]);
                        block.pos[1] = height
                            .wrapping_add(*offset)
                            .wrapping_add(original.pos[1].wrapping_sub(origin[1]));
                    }
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
                cap.finalize(seed, origin, pivot, &mut output, |pos| world.block_at(pos));
            }
        }
        output
    }
}

struct BlockReader<F>(F);

impl<F: FnMut([i32; 3]) -> u32> ProcessorWorld for BlockReader<F> {
    fn block_at(&mut self, pos: [i32; 3]) -> u32 {
        self.0(pos)
    }

    fn heightmap(&mut self, _: HeightmapType, _: i32, _: i32) -> i32 {
        panic!("gravity requires ProcessorPipeline::process_in_world")
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

    #[test]
    fn gravity_uses_original_local_height_and_live_heightmaps_without_drawing_random() {
        struct Terrain {
            live: bool,
            queries: Vec<HeightmapType>,
        }
        impl ProcessorWorld for Terrain {
            fn block_at(&mut self, _: [i32; 3]) -> u32 {
                panic!("gravity read a block")
            }
            fn heightmap(&mut self, kind: HeightmapType, x: i32, z: i32) -> i32 {
                assert_eq!([x, z], [-17, 32]);
                self.queries.push(kind);
                match kind {
                    HeightmapType::WorldSurfaceWg => 64,
                    HeightmapType::WorldSurface => 90,
                    HeightmapType::OceanFloorWg => -30,
                    HeightmapType::OceanFloor => 10,
                    _ => panic!("unexpected heightmap"),
                }
            }
            fn is_server_level(&self) -> bool {
                self.live
            }
        }
        let pipeline = compile(
            &[
                json!({"processor_type": "minecraft:gravity"}),
                json!({"processor_type": "minecraft:gravity", "heightmap": "OCEAN_FLOOR_WG", "offset": -1}),
            ],
            &WorldgenData::default(),
        );
        let input = block("oak_stairs", [-17, 77, 32]);
        for (live, y, queries) in [
            (
                false,
                -24,
                [HeightmapType::WorldSurfaceWg, HeightmapType::OceanFloorWg],
            ),
            (
                true,
                16,
                [HeightmapType::WorldSurface, HeightmapType::OceanFloor],
            ),
        ] {
            let mut world = Terrain {
                live,
                queries: Vec::new(),
            };
            let mut random = XoroshiroRandom::new(42);
            let output = pipeline.process_in_world(
                123,
                [-20, 70, 30],
                [999, -64, 999],
                std::slice::from_ref(&input),
                &mut world,
                Some(&mut random),
            );
            assert_eq!(
                output,
                [ProcessorBlock {
                    pos: [-17, y, 32],
                    ..input.clone()
                }]
            );
            assert_eq!(world.queries, queries);
            assert_eq!(random.next_long(), XoroshiroRandom::new(42).next_long());
        }
    }

    #[test]
    fn filtered_blocks_and_empty_templates_do_not_need_gravity_heightmaps() {
        let pipeline = compile(
            &[
                json!({"processor_type": "minecraft:block_ignore", "blocks": [{"Name": "minecraft:stone"}]}),
                json!({"processor_type": "minecraft:gravity"}),
            ],
            &WorldgenData::default(),
        );
        for blocks in [vec![], vec![block("stone", [0; 3])]] {
            assert!(
                pipeline
                    .process(0, [0; 3], [0; 3], &blocks, |_| panic!("read terrain"), None)
                    .is_empty()
            );
        }
    }

    #[test]
    fn malformed_gravity_definitions_report_their_context() {
        for (key, value) in [
            ("heightmap", json!("unknown")),
            ("heightmap", json!(17)),
            ("heightmap", Value::Null),
            ("offset", json!(1.5)),
            ("offset", json!(i64::from(i32::MAX) + 1)),
            ("offset", json!(i64::from(i32::MIN) - 1)),
            ("offset", json!("-1")),
            ("offset", Value::Null),
        ] {
            let mut entry = json!({"processor_type": "minecraft:gravity"});
            entry[key] = value;
            assert!(
                ProcessorPipeline::compile("bad-gravity", &[entry], &WorldgenData::default())
                    .unwrap_err()
                    .to_string()
                    .contains("bad-gravity")
            );
        }
    }

    #[test]
    fn material_substitutions_preserve_position_and_nbt_but_reset_unlisted_properties() {
        let pipeline = compile(
            &[json!({"processor_type": "minecraft:blackstone_replace"})],
            &WorldgenData::default(),
        );
        let mut input = block("cobblestone_stairs", [-17, 319, 32]);
        input.state = block_states()
            .state(
                "minecraft:cobblestone_stairs",
                [
                    ("facing", "west"),
                    ("half", "top"),
                    ("shape", "inner_left"),
                    ("waterlogged", "true"),
                ],
            )
            .unwrap()
            .0;
        let output = pipeline.process(
            123,
            [99; 3],
            [0; 3],
            std::slice::from_ref(&input),
            |_| panic!("blackstone read terrain"),
            None,
        );
        let expected = block_states()
            .state(
                "minecraft:blackstone_stairs",
                [("facing", "west"), ("half", "top")],
            )
            .unwrap()
            .0;
        assert_eq!(
            output,
            [ProcessorBlock {
                state: expected,
                ..input.clone()
            }]
        );
        let pipeline = compile(
            &[json!({"processor_type": "minecraft:lava_submerged_block"})],
            &WorldgenData::default(),
        );
        assert_eq!(
            pipeline.process(
                0,
                [0; 3],
                [0; 3],
                std::slice::from_ref(&input),
                |_| state("lava"),
                None
            ),
            [ProcessorBlock {
                state: state("lava"),
                ..input
            }]
        );
    }

    #[test]
    fn aging_uses_tag_membership_and_the_shared_settings_random() {
        let data = WorldgenData::from_json_entries([
            ("data/minecraft/tags/block/stairs.json", r#"{"values":[]}"#),
            (
                "data/minecraft/tags/block/slabs.json",
                r#"{"values":["minecraft:oak_slab"]}"#,
            ),
            ("data/minecraft/tags/block/walls.json", r#"{"values":[]}"#),
        ])
        .unwrap();
        let pipeline = compile(
            &[json!({"processor_type": "minecraft:block_age", "mossiness": 1.0})],
            &data,
        );
        let mut input = block("oak_slab", [-17, -64, 32]);
        input.state = block_states()
            .state(
                "minecraft:oak_slab",
                [("type", "top"), ("waterlogged", "true")],
            )
            .unwrap()
            .0;
        let mut random = XoroshiroRandom::new(42);
        let output = pipeline.process(
            123,
            [99; 3],
            [0; 3],
            std::slice::from_ref(&input),
            |_| panic!("age read terrain"),
            Some(&mut random),
        );
        let expected = block_states()
            .state(
                "minecraft:mossy_stone_brick_slab",
                [("type", "top"), ("waterlogged", "true")],
            )
            .unwrap()
            .0;
        assert_eq!(
            output,
            [ProcessorBlock {
                state: expected,
                ..input
            }]
        );
        let mut expected_random = XoroshiroRandom::new(42);
        expected_random.next_float();
        assert_eq!(random.next_long(), expected_random.next_long());
        let untagged = block("stone_slab", [0; 3]);
        assert_eq!(
            pipeline.process(
                0,
                [0; 3],
                [0; 3],
                std::slice::from_ref(&untagged),
                |_| panic!("age read terrain"),
                None
            ),
            [untagged]
        );
    }

    #[test]
    fn malformed_aging_definitions_report_their_context() {
        for value in [
            None,
            Some(Value::Null),
            Some(json!("0.2")),
            Some(json!(false)),
            Some(json!([])),
        ] {
            let mut entry = json!({"processor_type": "minecraft:block_age"});
            if let Some(value) = value {
                entry["mossiness"] = value;
            }
            assert!(
                ProcessorPipeline::compile("bad-aging", &[entry], &WorldgenData::default())
                    .unwrap_err()
                    .to_string()
                    .contains("bad-aging")
            );
        }
    }

    #[test]
    fn lava_queries_dynamic_outline_shapes_at_the_gravity_adjusted_position() {
        struct Terrain;
        impl ProcessorWorld for Terrain {
            fn block_at(&mut self, pos: [i32; 3]) -> u32 {
                assert_eq!(pos, [3, 75, 4]);
                state("lava")
            }
            fn heightmap(&mut self, _: HeightmapType, _: i32, _: i32) -> i32 {
                70
            }
            fn is_shape_full_block(&mut self, incoming: u32, pos: [i32; 3]) -> bool {
                assert_eq!(incoming, state("shulker_box"));
                assert_eq!(pos, [3, 75, 4]);
                false // An open shulker box's outline extends beyond a single block.
            }
        }
        let lava = json!({"processor_type": "minecraft:lava_submerged_block"});
        let data = WorldgenData::default();
        let input = block("shulker_box", [3, 8, 4]);
        let pipeline = compile(
            &[
                json!({"processor_type": "minecraft:gravity", "offset": 2}),
                lava.clone(),
            ],
            &data,
        );
        let output = pipeline.process_in_world(
            0,
            [0, 5, 0],
            [0; 3],
            std::slice::from_ref(&input),
            &mut Terrain,
            None,
        );
        assert_eq!(
            output,
            [ProcessorBlock {
                pos: [3, 75, 4],
                state: state("lava"),
                ..input.clone()
            }]
        );
        assert_eq!(
            compile(&[lava], &data).process(
                0,
                [0; 3],
                [0; 3],
                std::slice::from_ref(&input),
                |_| state("lava"),
                None
            ),
            [input]
        );
    }
}
