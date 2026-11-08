//! Vanilla-exact world generation for Minecraft 26.1
//!
//! Design: `docs/world.md`.

pub mod aquifer;
pub mod beardifier;
pub mod biome;
pub mod carver;
pub mod configured;
pub mod data;
pub mod density;
pub mod fallen_tree;
pub mod feature;
pub mod generator;
pub mod huge_mushroom;
pub mod java_hash;
pub mod light;
pub mod noise_chunk;
pub mod ore_vein;
pub mod placement;
pub mod seed;
pub mod shape_update;
pub mod spline;
pub mod structure;
pub mod surface;
pub mod survival;
pub mod template;
pub mod tree;
pub mod tree_decorator;

pub use aquifer::{
    Aquifer, FluidStatus, GlobalFluidPicker, NoiseBasedAquifer, WAY_BELOW_MIN_Y, clamped_map, map,
    quantize, similarity,
};
pub use beardifier::Beardifier;
pub use biome::{
    BiomeManager, MultiNoiseBiomeSource, Parameter, ParameterPoint, RTree, TargetPoint,
    nether_biome_parameters, overworld_biome_parameters, quantize_coord, unquantize_coord,
};
pub use configured::{
    CompiledFeature, CompiledPlaced, FeatureCompiler, StateProvider, place_feature, place_placed,
};
pub use data::{
    BiomeDef, BiomeParameterEntryDef, BiomeSourceDef, BlockDirection, BlockPredicateDef,
    BlockStateDef, BlockTagDef, BlockTagEntry, ConfiguredCarverDef, ConfiguredFeatureDef,
    DimensionDef, ExclusionZoneDef, FeatureConfigDef, FeatureRefDef, FrequencyReductionMethod,
    HeightProviderDef, IntProviderDef, JigsawDef, MultiNoiseParameterListDef,
    NoiseSamplingSettings, NoiseSettingsDef, OceanRuinDef, OreFeatureDef, OreTargetDef,
    PlacedFeatureDef, PlacementModifierDef, PoolAliasDef, PoolElementDef, Projection,
    RuinedPortalPlacement, RuinedPortalSetupDef, RuleTestDef, StartHeightDef, StructureDef,
    StructurePlacementCommon, StructurePlacementDef, StructurePlacementKind, StructureSetDef,
    StructureSetEntryDef, TemperatureModifier, TemplatePoolDef, WorldPresetDef, WorldgenData,
    WorldgenDataError,
};
pub use density::{
    BinaryOp, CompiledDensityGraph, CompiledNode, CompiledNoiseRouter, DensityFunctionDef,
    DensityOp, MarkerKind, NodeId, NoiseRouterDef, PositionalRandom, RarityValueMapper, UnaryOp,
};
pub use feature::{
    ChunkFeatureWorld, CompiledOre, FeatureIndex, FeatureWorld, OreTarget, RegionFeatureWorld,
    RuleTestRule, STEP_COUNT, apply_biome_decoration, place_ore,
};
pub use generator::{GeneratedChunk, HeightmapType, Heightmaps, NoiseBasedChunkGenerator};
pub use light::{ChunkLight, LIGHT_BYTES, LightSection, LightWorld, light_chunk};
pub use noise_chunk::NoiseChunk;
pub use ore_vein::{OreVeinifier, VeinType};
pub use placement::{
    BlockPredicate, CompiledModifier, Fluid, StateSet, compile_placement, compile_predicate, fluid,
    place_positions, sample_height_provider, sample_int_provider,
};
pub use seed::parse_level_seed;
pub use spline::{CubicSpline, CubicSplineDef, SplinePointDef};
pub use structure::{
    BoundingBox, Direction, JigsawJunction, JigsawPiece, MonumentLayout, MonumentRoom,
    MonumentRoomKind, MonumentRoomPiece, RuinedPortalBlocks, RuinedPortalGenerator, StructurePiece,
    StructureReference, StructureStart, StructureState, TemplatePiece, place_mineshafts_in_chunk,
};
pub use surface::{
    BiomeTemperatureSampler, CaveSurface, SurfaceChunkAccess, SurfaceCondition, SurfaceRule,
    SurfaceSystem, VerticalAnchor, generate_bands, java_round, resolve_block_state,
};
pub use survival::{SurvivalRule, compile_block_survival, compile_survival};
pub use template::{
    Facing, JigsawBlock, JointType, Mirror, Rotation, StructureTemplate, StructureTemplates,
};

#[cfg(test)]
mod tests {
    use indexmap::IndexMap;

    use super::*;

    #[test]
    fn a_tie_between_different_values_is_ambiguous() {
        let point = ParameterPoint {
            temperature: Parameter::point(0.0),
            humidity: Parameter::point(0.0),
            continentalness: Parameter::point(0.0),
            erosion: Parameter::point(0.0),
            depth: Parameter::point(0.0),
            weirdness: Parameter::point(0.0),
            offset: 0,
        };
        let target = TargetPoint {
            temperature: 100,
            humidity: 0,
            continentalness: 0,
            erosion: 0,
            depth: 0,
            weirdness: 0,
        };
        let tree = RTree::new(vec![(point, 1), (point, 2)]);
        assert_eq!(tree.unambiguous_search(target), None);
        let tree = RTree::new(vec![(point, 1), (point, 1)]);
        assert_eq!(tree.unambiguous_search(target), Some(&1));
        let far = ParameterPoint {
            temperature: Parameter::point(1.0),
            ..point
        };
        let tree = RTree::new(vec![(far, 2), (point, 1)]);
        assert_eq!(tree.unambiguous_search(target), Some(&1));
    }

    #[test]
    fn overworld_biome_builder_generates_7593_points_with_all_54_overworld_biomes() {
        let points = overworld_biome_parameters();
        assert_eq!(points.len(), 7593);

        let mut unique = std::collections::BTreeSet::new();
        for (_, biome) in &points {
            unique.insert(*biome);
        }
        // Vanilla 26.1 Overworld has 54 distinct biomes (including cherry_grove and pale_garden).
        assert_eq!(unique.len(), 54);
        assert!(unique.contains("minecraft:plains"));
        assert!(unique.contains("minecraft:cherry_grove"));
        assert!(unique.contains("minecraft:pale_garden"));
        assert!(unique.contains("minecraft:deep_dark"));
        assert!(unique.contains("minecraft:lush_caves"));
        assert!(unique.contains("minecraft:dripstone_caves"));
    }

    #[test]
    fn biome_manager_obfuscate_seed_and_voronoi_zoom_are_deterministic() {
        // SHA-256 of 0i64 little-endian (`[0; 8]`) starts with `af 55 70 f5 a1 81 0b 7a`.
        let zoom_0 = BiomeManager::obfuscate_seed(0);
        assert_eq!(
            zoom_0,
            i64::from_le_bytes([0xaf, 0x55, 0x70, 0xf5, 0xa1, 0x81, 0x0b, 0x7a])
        );

        let mgr = BiomeManager::from_world_seed(12345);
        let mut queried_quarts = Vec::new();
        let b = mgr.get_biome(19, 67, -41, |qx, qy, qz| {
            queried_quarts.push((qx, qy, qz));
            ((qx & 0xff) as u32) ^ (((qy & 0xff) as u32) << 8) ^ (((qz & 0xff) as u32) << 16)
        });
        assert_eq!(queried_quarts.len(), 1);
        let (qx, qy, qz) = queried_quarts[0];
        // `(19 - 2) >> 2 = 4`, `(67 - 2) >> 2 = 16`, `(-41 - 2) >> 2 = -11`.
        assert!((4..=5).contains(&qx));
        assert!((16..=17).contains(&qy));
        assert!((-11..=-10).contains(&qz));
        assert_eq!(
            b,
            mgr.get_biome(19, 67, -41, |x, y, z| {
                ((x & 0xff) as u32) ^ (((y & 0xff) as u32) << 8) ^ (((z & 0xff) as u32) << 16)
            })
        );
    }

    #[test]
    fn cubic_spline_evaluates_in_f32_and_extends_linearly_outside_knots() {
        let mut noises = IndexMap::new();
        let mut dfs = IndexMap::new();
        dfs.insert(
            "minecraft:test_coord".to_string(),
            DensityFunctionDef::YClampedGradient {
                from_y: -64,
                to_y: 64,
                from_value: -1.0,
                to_value: 1.0,
            },
        );
        let spline_def = DensityFunctionDef::Spline(CubicSplineDef::Multipoint {
            coordinate: Box::new(DensityFunctionDef::Reference(
                "minecraft:test_coord".to_string(),
            )),
            points: vec![
                SplinePointDef {
                    location: -0.5,
                    value: CubicSplineDef::Constant(2.0),
                    derivative: 1.0,
                },
                SplinePointDef {
                    location: 0.5,
                    value: CubicSplineDef::Constant(4.0),
                    derivative: -0.5,
                },
            ],
        });

        let (graph, root) =
            CompiledDensityGraph::compile_single(42, false, &spline_def, &noises, &dfs)
                .expect("compile spline");
        // Below -0.5 (at y = -64 -> coord = -1.0): linear_extend(-1.0) = 2.0 + 1.0 * (-1.0 - (-0.5)) = 1.5.
        assert_eq!(graph.eval_point_stateless(root, 0, -64, 0), 1.5);
        // Above 0.5 (at y = 64 -> coord = 1.0): linear_extend(1.0) = 4.0 + (-0.5) * (1.0 - 0.5) = 3.75.
        assert_eq!(graph.eval_point_stateless(root, 0, 64, 0), 3.75);
        // Midpoint at y = 0 -> coord = 0.0 -> t = 0.5.
        let mid = graph.eval_point_stateless(root, 0, 0, 0);
        // v0=2, v1=4, d0=1, d1=-0.5, span=1 -> a = 1 - 2 = -1, b = 0.5 + 2 = 2.5
        // lerp(0.5, 2, 4) + 0.25 * lerp(0.5, -1, 2.5) = 3.0 + 0.25 * 0.75 = 3.1875.
        assert_eq!(mid, f64::from(3.1875_f32));
        let _ = &mut noises;
    }

    #[test]
    fn loads_configured_carvers_and_nested_block_tags() {
        let data = WorldgenData::from_json_entries([
            (
                "data/example/worldgen/configured_carver/test.json",
                r##"{"type":"minecraft:cave","config":{
                    "probability":0.25, "y":{"type":"minecraft:uniform",
                        "min_inclusive":{"above_bottom":2},"max_inclusive":{"absolute":30}},
                    "yScale":0.5, "lava_level":{"below_top":10}, "replaceable":"#example:replaceable",
                    "horizontal_radius_multiplier":1.0, "vertical_radius_multiplier":1.0,
                    "floor_level":{"type":"minecraft:uniform","min_inclusive":-1.0,"max_exclusive":-0.5}
                }}"##,
            ),
            (
                "data/example/tags/block/replaceable.json",
                r##"{"values":["minecraft:stone", "#example:nested", {"id":"example:optional", "required":false}]}"##,
            ),
            (
                "data/example/tags/block/nested.json",
                r#"{"replace":true,"values":["minecraft:dirt"]}"#,
            ),
        ])
        .unwrap();
        assert_eq!(
            data.configured_carvers["example:test"].carver_type,
            "minecraft:cave"
        );
        assert_eq!(
            data.configured_carvers["example:test"].config["probability"],
            0.25
        );
        assert!(data.block_tags["example:nested"].replace);
        let values = &data.block_tags["example:replaceable"].values;
        assert_eq!(values[1].id, "#example:nested");
        assert!(!values[2].required);
        let resolved = data.block_tag_states("example:replaceable").unwrap();
        let states = harpas_core::block::block_states();
        assert!(resolved[states.default_state("minecraft:stone").unwrap() as usize]);
        assert!(resolved[states.default_state("minecraft:dirt").unwrap() as usize]);
        assert!(!resolved[states.default_state("minecraft:bedrock").unwrap() as usize]);
        let compiled = carver::CarverConfig::compile("example:test", &data).unwrap();
        assert_eq!(compiled.probability, 0.25);
        assert_eq!(compiled.lava_level, VerticalAnchor::BelowTop(10));
        assert_eq!(compiled.replaceable, resolved);
        assert!(matches!(
            compiled.shape,
            carver::CarverShape::Cave { nether: false, .. }
        ));
        let mut unsupported = data.clone();
        unsupported.configured_carvers["example:test"].config["y"]["type"] =
            serde_json::json!("example:unknown");
        assert!(carver::CarverConfig::compile("example:test", &unsupported).is_err());

        let cycle = WorldgenData::from_json_entries([
            (
                "data/example/tags/block/a.json",
                r##"{"values":["#example:b"]}"##,
            ),
            (
                "data/example/tags/block/b.json",
                r##"{"values":["#example:a"]}"##,
            ),
        ])
        .unwrap();
        assert!(cycle.block_tag_states("example:a").is_err());
    }

    #[test]
    fn loads_feature_data_and_biome_feature_lists() {
        let data = WorldgenData::from_json_entries([
            (
                "data/example/worldgen/configured_feature/ore_test.json",
                r#"{"type":"minecraft:ore","config":{
                    "size":4,
                    "discard_chance_on_air_exposure":0.5,
                    "targets":[
                        {"target":{"predicate_type":"minecraft:tag_match","tag":"example:stone_ore_replaceables"},"state":{"Name":"minecraft:gold_ore"}},
                        {"target":{"predicate_type":"minecraft:block_match","block":"minecraft:deepslate"},"state":{"Name":"minecraft:deepslate_gold_ore"}},
                        {"target":{"predicate_type":"minecraft:blockstate_match","block_state":{"Name":"minecraft:stone","Properties":{"snowy":"false"}}},"state":{"Name":"minecraft:coal_ore"}},
                        {"target":{"predicate_type":"minecraft:random_block_match","block":"minecraft:tuff","probability":0.25},"state":{"Name":"minecraft:copper_ore"}},
                        {"target":{"predicate_type":"minecraft:always_true"},"state":{"Name":"minecraft:iron_ore"}}
                    ]
                }}"#,
            ),
            (
                "data/example/worldgen/configured_feature/tree_test.json",
                r#"{"type":"minecraft:tree","config":{"foo":"bar"}}"#,
            ),
            (
                "data/example/worldgen/placed_feature/ore_test.json",
                r#"{"feature":"example:ore_test","placement":[
                    {"type":"minecraft:count","count":9},
                    {"type":"minecraft:in_square"},
                    {"type":"minecraft:height_range","height":{"type":"minecraft:trapezoid","min_inclusive":{"absolute":-64},"max_inclusive":{"absolute":0},"plateau":4}},
                    {"type":"minecraft:biome"},
                    {"type":"minecraft:unknown_modifier","value":1}
                ]}"#,
            ),
            (
                "data/example/worldgen/placed_feature/inline_test.json",
                r#"{"feature":{"type":"minecraft:ore","config":{"size":1,"targets":[{"target":{"predicate_type":"minecraft:always_true"},"state":{"Name":"minecraft:coal_ore"}}]}},"placement":[]}"#,
            ),
            (
                "data/example/worldgen/biome/test.json",
                r#"{"has_precipitation":true,"temperature":0.8,"downfall":0.4,"features":[["minecraft:ore_iron"],[],["example:foo"]]}"#,
            ),
            (
                "data/example/worldgen/biome/empty.json",
                r#"{"has_precipitation":false,"temperature":0.0,"downfall":0.0}"#,
            ),
        ])
        .unwrap();

        let ore = &data.configured_features["example:ore_test"];
        assert_eq!(ore.feature_type, "minecraft:ore");
        let FeatureConfigDef::Ore(config) = &ore.config else {
            panic!("expected a parsed ore config");
        };
        assert_eq!(config.size, 4);
        assert_eq!(config.discard_chance_on_air_exposure, 0.5);
        assert_eq!(config.targets.len(), 5);
        assert_eq!(
            config.targets[0].target,
            RuleTestDef::TagMatch("example:stone_ore_replaceables".to_string())
        );
        assert_eq!(config.targets[0].state.name, "minecraft:gold_ore");
        assert_eq!(
            config.targets[1].target,
            RuleTestDef::BlockMatch("minecraft:deepslate".to_string())
        );
        assert_eq!(
            config.targets[2].target,
            RuleTestDef::BlockStateMatch(BlockStateDef {
                name: "minecraft:stone".to_string(),
                properties: [("snowy".to_string(), "false".to_string())]
                    .into_iter()
                    .collect(),
            })
        );
        assert_eq!(
            config.targets[3].target,
            RuleTestDef::RandomBlockMatch {
                block: "minecraft:tuff".to_string(),
                probability: 0.25,
            }
        );
        assert_eq!(config.targets[4].target, RuleTestDef::AlwaysTrue);

        let FeatureConfigDef::Other(tree) = &data.configured_features["example:tree_test"].config
        else {
            panic!("an unsupported feature type keeps its raw config");
        };
        assert_eq!(tree["foo"], "bar");

        let placed = &data.placed_features["example:ore_test"];
        assert_eq!(
            placed.feature,
            FeatureRefDef::Reference("example:ore_test".to_string())
        );
        assert_eq!(
            placed.placement[0],
            PlacementModifierDef::Count(IntProviderDef::Constant(9))
        );
        assert_eq!(placed.placement[1], PlacementModifierDef::InSquare);
        assert_eq!(
            placed.placement[2],
            PlacementModifierDef::HeightRange(HeightProviderDef::Trapezoid {
                min_inclusive: VerticalAnchor::Absolute(-64),
                max_inclusive: VerticalAnchor::Absolute(0),
                plateau: 4,
            })
        );
        assert_eq!(placed.placement[3], PlacementModifierDef::Biome);
        assert!(matches!(
            &placed.placement[4],
            PlacementModifierDef::Other { modifier_type, .. }
                if modifier_type == "minecraft:unknown_modifier"
        ));
        assert!(matches!(
            &data.placed_features["example:inline_test"].feature,
            FeatureRefDef::Inline(feature)
                if feature.feature_type == "minecraft:ore"
        ));

        assert_eq!(
            data.biomes["example:test"].features,
            vec![
                vec!["minecraft:ore_iron".to_string()],
                Vec::new(),
                vec!["example:foo".to_string()],
            ]
        );
        assert!(data.biomes["example:empty"].features.is_empty());

        let malformed = WorldgenData::from_json_entries([(
            "data/example/worldgen/placed_feature/bad.json",
            r#"{"feature":"example:ore_test","placement":[{"type":"minecraft:height_range","height":{"type":"minecraft:constant","value":{"absolute":"nope"}}}]}"#,
        )]);
        assert!(matches!(
            malformed,
            Err(WorldgenDataError::InvalidDefinition { .. })
        ));
    }

    #[test]
    fn covers_cache_all_in_cell_end_islands_old_blended_and_json_entries_loader() {
        let entries = [
            (
                "data/minecraft/worldgen/noise/test_shift.json",
                r#"{"firstOctave": -3, "amplitudes": [1.0, 1.0]}"#,
            ),
            (
                "data/minecraft/worldgen/biome/nether_wastes.json",
                r#"{"has_precipitation": false, "temperature": 2.0, "downfall": 0.0, "carvers": "minecraft:hell_cave"}"#,
            ),
            (
                "data/minecraft/worldgen/biome/soul_sand_valley.json",
                r#"{"has_precipitation": false, "temperature": 2.0, "downfall": 0.0, "carvers": {"air": ["minecraft:hell_cave"]}}"#,
            ),
            (
                "data/minecraft/worldgen/biome/crimson_forest.json",
                r#"{"has_precipitation": false, "temperature": 2.0, "downfall": 0.0}"#,
            ),
            (
                "data/minecraft/worldgen/biome/warped_forest.json",
                r#"{"has_precipitation": false, "temperature": 2.0, "downfall": 0.0}"#,
            ),
            (
                "data/minecraft/worldgen/biome/basalt_deltas.json",
                r#"{"has_precipitation": false, "temperature": 2.0, "downfall": 0.0}"#,
            ),
            (
                "data/minecraft/worldgen/multi_noise_biome_source_parameter_list/custom.json",
                r#"{
                    "biomes": [
                        {
                            "biome": "minecraft:nether_wastes",
                            "parameters": {
                                "temperature": [-1.0, 0.0],
                                "humidity": 0.0,
                                "continentalness": 0.0,
                                "erosion": 0.0,
                                "depth": 0.0,
                                "weirdness": 0.0,
                                "offset": 0.0
                            }
                        },
                        {
                            "biome": "minecraft:crimson_forest",
                            "parameters": {
                                "temperature": [0.0, 1.0],
                                "humidity": 0.0,
                                "continentalness": 0.0,
                                "erosion": 0.0,
                                "depth": 0.0,
                                "weirdness": 0.0,
                                "offset": 0.0
                            }
                        }
                    ]
                }"#,
            ),
            (
                "data/minecraft/worldgen/density_function/test_cell.json",
                r#"{
                    "type": "minecraft:cache_all_in_cell",
                    "argument": {
                        "type": "minecraft:add",
                        "argument1": { "type": "minecraft:end_islands" },
                        "argument2": {
                            "type": "minecraft:add",
                            "argument1": {
                                "type": "minecraft:old_blended_noise",
                                "xz_scale": 0.25,
                                "y_scale": 0.125,
                                "xz_factor": 80.0,
                                "y_factor": 160.0,
                                "smear_scale_multiplier": 8.0
                            },
                            "argument2": {
                                "type": "minecraft:weird_scaled_sampler",
                                "input": { "type": "minecraft:shift", "argument": "minecraft:test_shift" },
                                "noise": "minecraft:test_shift",
                                "rarity_value_mapper": "type_2"
                            }
                        }
                    }
                }"#,
            ),
        ];

        let data = WorldgenData::from_json_entries(entries).expect("load json entries");
        assert_eq!(data.biomes.len(), 5);
        assert_eq!(
            data.biomes["minecraft:nether_wastes"].carvers,
            vec!["minecraft:hell_cave"]
        );
        assert_eq!(
            data.biomes["minecraft:soul_sand_valley"].carvers,
            vec!["minecraft:hell_cave"]
        );

        let nether_src = MultiNoiseBiomeSource::from_parameter_list(
            &MultiNoiseParameterListDef::Preset("minecraft:nether".to_string()),
            &data,
        )
        .expect("nether preset");
        let mut last = None;
        let b = nether_src.get_noise_biome(
            TargetPoint::from_floats(0.4, 0.0, 0.0, 0.0, 0.0, 0.0),
            &mut last,
        );
        assert_eq!(data.biome_name(b), Some("minecraft:crimson_forest"));

        let custom_src = MultiNoiseBiomeSource::from_parameter_list(
            &data.parameter_lists["minecraft:custom"],
            &data,
        )
        .expect("custom biome list");
        let mut last = None;
        let b2 = custom_src.get_noise_biome(
            TargetPoint::from_floats(-0.5, 0.0, 0.0, 0.0, 0.0, 0.0),
            &mut last,
        );
        assert_eq!(data.biome_name(b2), Some("minecraft:nether_wastes"));

        let zero = DensityFunctionDef::Constant(0.0);
        let router_def = NoiseRouterDef {
            barrier_noise: zero.clone(),
            fluid_level_floodedness_noise: zero.clone(),
            fluid_level_spread_noise: zero.clone(),
            lava_noise: zero.clone(),
            temperature: zero.clone(),
            vegetation: zero.clone(),
            continents: zero.clone(),
            erosion: zero.clone(),
            depth: zero.clone(),
            ridges: zero.clone(),
            preliminary_surface_level: zero.clone(),
            final_density: DensityFunctionDef::Reference("minecraft:test_cell".to_string()),
            vein_toggle: zero.clone(),
            vein_ridged: zero.clone(),
            vein_gap: zero,
        };
        let (graph, router) = CompiledDensityGraph::compile_router(
            777,
            true,
            &router_def,
            &data.noises,
            &data.density_functions,
        )
        .expect("compile legacy router");
        assert_eq!(graph.cache_all_in_cell_args().len(), 1);

        let settings = NoiseSamplingSettings {
            min_y: 0,
            height: 16,
            size_horizontal: 1,
            size_vertical: 2,
        };
        let mut chunk = NoiseChunk::new(&graph, &router, settings, 0, 0);
        chunk.initialize_for_first_cell_x();
        chunk.advance_cell_x(0);
        chunk.select_cell_yz(0, 0);
        chunk.update_for_y(3, 3.0 / 8.0);
        chunk.update_for_x(2, 2.0 / 4.0);
        chunk.update_for_z(1, 1.0 / 4.0);
        let cached_val = chunk.eval_node(router.final_density);
        chunk.stop_interpolation();
        let direct_val = chunk.eval_point(router.final_density, 2, 3, 1);
        assert_eq!(cached_val.to_bits(), direct_val.to_bits());
    }

    #[test]
    fn synthetic_generator_aquifer_veins_and_surface_rules() {
        let noise_entry = r#"{"firstOctave": -4, "amplitudes": [1.0, 1.0]}"#;
        let entries = [
            (
                "data/minecraft/worldgen/noise/clay_bands_offset.json",
                noise_entry,
            ),
            ("data/minecraft/worldgen/noise/surface.json", noise_entry),
            (
                "data/minecraft/worldgen/noise/surface_secondary.json",
                noise_entry,
            ),
            (
                "data/minecraft/worldgen/noise/badlands_pillar.json",
                noise_entry,
            ),
            (
                "data/minecraft/worldgen/noise/badlands_pillar_roof.json",
                noise_entry,
            ),
            (
                "data/minecraft/worldgen/noise/badlands_surface.json",
                noise_entry,
            ),
            (
                "data/minecraft/worldgen/noise/iceberg_pillar.json",
                noise_entry,
            ),
            (
                "data/minecraft/worldgen/noise/iceberg_pillar_roof.json",
                noise_entry,
            ),
            (
                "data/minecraft/worldgen/noise/iceberg_surface.json",
                noise_entry,
            ),
            (
                "data/minecraft/worldgen/biome/eroded_badlands.json",
                r#"{"has_precipitation": false, "temperature": 2.0, "downfall": 0.0}"#,
            ),
            (
                "data/minecraft/worldgen/biome/frozen_ocean.json",
                r#"{"has_precipitation": false, "temperature": 0.0, "temperature_modifier": "frozen", "downfall": 0.5}"#,
            ),
            (
                "data/minecraft/worldgen/biome/deep_frozen_ocean.json",
                r#"{"has_precipitation": false, "temperature": 0.5, "temperature_modifier": "frozen", "downfall": 0.5}"#,
            ),
            (
                "data/minecraft/worldgen/biome/swamp.json",
                r#"{"has_precipitation": true, "temperature": 0.8, "downfall": 0.9}"#,
            ),
            (
                "data/minecraft/worldgen/biome/snowy_plains.json",
                r#"{"has_precipitation": true, "temperature": 0.0, "downfall": 0.5}"#,
            ),
            (
                "data/minecraft/worldgen/multi_noise_biome_source_parameter_list/test_biomes.json",
                r#"{
                    "biomes": [
                        {
                            "biome": "minecraft:eroded_badlands",
                            "parameters": {
                                "temperature": [0.5, 1.0],
                                "humidity": 0.0,
                                "continentalness": 0.0,
                                "erosion": 0.0,
                                "depth": 0.0,
                                "weirdness": 0.0,
                                "offset": 0.0
                            }
                        },
                        {
                            "biome": "minecraft:frozen_ocean",
                            "parameters": {
                                "temperature": [-1.0, -0.5],
                                "humidity": 0.0,
                                "continentalness": 0.0,
                                "erosion": 0.0,
                                "depth": 0.0,
                                "weirdness": 0.0,
                                "offset": 0.0
                            }
                        },
                        {
                            "biome": "minecraft:swamp",
                            "parameters": {
                                "temperature": [-0.5, 0.0],
                                "humidity": 0.0,
                                "continentalness": 0.0,
                                "erosion": 0.0,
                                "depth": 0.0,
                                "weirdness": 0.0,
                                "offset": 0.0
                            }
                        },
                        {
                            "biome": "minecraft:snowy_plains",
                            "parameters": {
                                "temperature": [0.0, 0.5],
                                "humidity": 0.0,
                                "continentalness": 0.0,
                                "erosion": 0.0,
                                "depth": 0.0,
                                "weirdness": 0.0,
                                "offset": 0.0
                            }
                        }
                    ]
                }"#,
            ),
            (
                "data/minecraft/worldgen/noise_settings/test_overworld.json",
                r#"{
                    "sea_level": 0,
                    "disable_mob_generation": false,
                    "aquifers_enabled": true,
                    "ore_veins_enabled": true,
                    "legacy_random_source": false,
                    "default_block": { "Name": "minecraft:stone" },
                    "default_fluid": { "Name": "minecraft:water", "Properties": { "level": "0" } },
                    "noise": {
                        "min_y": -64,
                        "height": 128,
                        "size_horizontal": 1,
                        "size_vertical": 2
                    },
                    "noise_router": {
                        "barrier": 0.0,
                        "fluid_level_floodedness": 0.5,
                        "fluid_level_spread": 0.0,
                        "lava": -1.0,
                        "temperature": { "type": "minecraft:shifted_noise", "noise": "minecraft:surface", "xz_scale": 0.25, "y_scale": 0.0, "shift_x": 0.0, "shift_y": 0.0, "shift_z": 0.0 },
                        "vegetation": 0.0,
                        "continents": 0.0,
                        "erosion": 0.0,
                        "depth": { "type": "minecraft:y_clamped_gradient", "from_y": -64, "to_y": 64, "from_value": 1.5, "to_value": -1.5 },
                        "ridges": 0.0,
                        "preliminary_surface_level": -8.0,
                        "final_density": {
                            "type": "minecraft:add",
                            "argument1": { "type": "minecraft:y_clamped_gradient", "from_y": -64, "to_y": 32, "from_value": 1.0, "to_value": -1.0 },
                            "argument2": { "type": "minecraft:noise", "noise": "minecraft:surface", "xz_scale": 1.0, "y_scale": 1.0 }
                        },
                        "vein_toggle": 0.55,
                        "vein_ridged": -0.5,
                        "vein_gap": 0.5
                    },
                    "spawn_target": [],
                    "surface_rule": {
                        "type": "minecraft:sequence",
                        "sequence": [
                            {
                                "type": "minecraft:condition",
                                "if_true": {
                                    "type": "minecraft:vertical_gradient",
                                    "random_name": "minecraft:bedrock_floor",
                                    "true_at_and_below": { "above_bottom": 0 },
                                    "false_at_and_above": { "above_bottom": 5 }
                                },
                                "then_run": { "type": "minecraft:block", "result_state": { "Name": "minecraft:bedrock" } }
                            },
                            {
                                "type": "minecraft:condition",
                                "if_true": {
                                    "type": "minecraft:not",
                                    "invert": {
                                        "type": "minecraft:vertical_gradient",
                                        "random_name": "minecraft:bedrock_roof",
                                        "true_at_and_below": { "below_top": 5 },
                                        "false_at_and_above": { "below_top": 0 }
                                    }
                                },
                                "then_run": { "type": "minecraft:block", "result_state": { "Name": "minecraft:bedrock" } }
                            },
                            {
                                "type": "minecraft:condition",
                                "if_true": { "type": "minecraft:above_preliminary_surface" },
                                "then_run": {
                                    "type": "minecraft:sequence",
                                    "sequence": [
                                        {
                                            "type": "minecraft:condition",
                                            "if_true": {
                                                "type": "minecraft:stone_depth",
                                                "offset": 0,
                                                "add_surface_depth": false,
                                                "secondary_depth_range": 0,
                                                "surface_type": "floor"
                                            },
                                            "then_run": {
                                                "type": "minecraft:sequence",
                                                "sequence": [
                                                    {
                                                        "type": "minecraft:condition",
                                                        "if_true": { "type": "minecraft:biome", "biome_is": ["minecraft:eroded_badlands"] },
                                                        "then_run": { "type": "minecraft:bandlands" }
                                                    },
                                                    {
                                                        "type": "minecraft:condition",
                                                        "if_true": { "type": "minecraft:hole" },
                                                        "then_run": { "type": "minecraft:block", "result_state": { "Name": "minecraft:coarse_dirt" } }
                                                    },
                                                    {
                                                        "type": "minecraft:condition",
                                                        "if_true": { "type": "minecraft:steep" },
                                                        "then_run": { "type": "minecraft:block", "result_state": { "Name": "minecraft:calcite" } }
                                                    },
                                                    {
                                                        "type": "minecraft:condition",
                                                        "if_true": { "type": "minecraft:temperature" },
                                                        "then_run": { "type": "minecraft:block", "result_state": { "Name": "minecraft:snow_block" } }
                                                    },
                                                    {
                                                        "type": "minecraft:condition",
                                                        "if_true": {
                                                            "type": "minecraft:water",
                                                            "offset": -1,
                                                            "surface_depth_multiplier": 0,
                                                            "add_stone_depth": false
                                                        },
                                                        "then_run": {
                                                            "type": "minecraft:condition",
                                                            "if_true": {
                                                                "type": "minecraft:y_above",
                                                                "anchor": { "absolute": -16 },
                                                                "surface_depth_multiplier": 0,
                                                                "add_stone_depth": false
                                                            },
                                                            "then_run": {
                                                                "type": "minecraft:condition",
                                                                "if_true": {
                                                                    "type": "minecraft:noise_threshold",
                                                                    "noise": "minecraft:surface",
                                                                    "min_threshold": -2.0,
                                                                    "max_threshold": 2.0
                                                                },
                                                                "then_run": { "type": "minecraft:block", "result_state": { "Name": "minecraft:grass_block", "Properties": { "snowy": "false" } } }
                                                            }
                                                        }
                                                    }
                                                ]
                                            }
                                        },
                                        {
                                            "type": "minecraft:condition",
                                            "if_true": {
                                                "type": "minecraft:stone_depth",
                                                "offset": 0,
                                                "add_surface_depth": true,
                                                "secondary_depth_range": 6,
                                                "surface_type": "ceiling"
                                            },
                                            "then_run": { "type": "minecraft:block", "result_state": { "Name": "minecraft:sandstone" } }
                                        }
                                    ]
                                }
                            }
                        ]
                    }
                }"#,
            ),
        ];

        let data = WorldgenData::from_json_entries(entries).expect("load synthetic worldgen data");
        let generator = NoiseBasedChunkGenerator::new(
            12345,
            "minecraft:test_overworld",
            &data.parameter_lists["minecraft:test_biomes"],
            &data,
        )
        .expect("build synthetic generator");

        let mut chunk = generator.generate_chunk_surface(0, 0, &data);
        let states = harpas_core::block::block_states();
        let bedrock = states.default_state("minecraft:bedrock").unwrap();
        // Bottom y = -64 must have bedrock placed by vertical_gradient.
        assert_eq!(chunk.block_state(0, -64, 0), bedrock);

        // Check heightmap packing and recomputation.
        let packed = chunk.heightmaps().pack(HeightmapType::WorldSurfaceWg);
        assert!(!packed.is_empty());
        let old_surface = chunk.heightmaps().get(HeightmapType::WorldSurface, 4, 4);
        chunk.recompute_heightmaps();
        assert_eq!(
            chunk.heightmaps().get(HeightmapType::WorldSurface, 4, 4),
            old_surface
        );
    }
}
