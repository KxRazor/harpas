//! Vanilla-exact world generation for Minecraft 26.1
//!
//! Design: `docs/world.md`.

pub mod biome;
pub mod data;
pub mod density;
pub mod noise_chunk;
pub mod spline;

pub use biome::{
    BiomeManager, MultiNoiseBiomeSource, Parameter, ParameterPoint, RTree, TargetPoint,
    nether_biome_parameters, overworld_biome_parameters, quantize_coord, unquantize_coord,
};
pub use data::{
    BiomeDef, BiomeParameterEntryDef, BiomeSourceDef, BlockStateDef, DimensionDef,
    MultiNoiseParameterListDef, NoiseSamplingSettings, NoiseSettingsDef, TemperatureModifier,
    WorldPresetDef, WorldgenData, WorldgenDataError,
};
pub use density::{
    BinaryOp, CompiledDensityGraph, CompiledNode, CompiledNoiseRouter, DensityFunctionDef,
    DensityOp, MarkerKind, NodeId, NoiseRouterDef, RarityValueMapper, UnaryOp,
};
pub use noise_chunk::NoiseChunk;
pub use spline::{CubicSpline, CubicSplineDef, SplinePointDef};

#[cfg(test)]
mod tests {
    use std::path::Path;

    use harpas_core::chunk::{PalettedContainer, Section, Strategy};
    use indexmap::IndexMap;

    use super::*;

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
    fn loads_and_evaluates_provisioned_26_1_datapack_when_present() {
        let zip_path = Path::new("../../test-environment/.harpas/vanilla-data/26.1.zip");
        if !zip_path.exists() {
            return;
        }

        let data = WorldgenData::from_zip_path(zip_path).expect("load 26.1.zip");
        assert_eq!(data.biomes.len(), 65);
        assert_eq!(data.noises.len(), 62);
        assert_eq!(data.density_functions.len(), 35);
        assert!(data.noise_settings.contains_key("minecraft:overworld"));

        let overworld = &data.noise_settings["minecraft:overworld"];
        assert_eq!(overworld.noise.min_y, -64);
        assert_eq!(overworld.noise.height, 384);
        assert_eq!(overworld.noise.cell_width(), 4);
        assert_eq!(overworld.noise.cell_height(), 8);
        assert_eq!(overworld.sea_level, 63);

        let (graph, router) = CompiledDensityGraph::compile_router(
            123_456_789,
            overworld.legacy_random_source,
            &overworld.noise_router,
            &data.noises,
            &data.density_functions,
        )
        .expect("compile overworld router");

        // Verify DAG deduplication: Overworld router has 8 FlatCache and 8 Interpolated slots.
        assert_eq!(graph.flat_cache_args().len(), 8);
        assert_eq!(graph.interpolator_args().len(), 8);

        let mut noise_chunk = NoiseChunk::new(&graph, &router, overworld.noise, 0, 0);
        let surf = noise_chunk.preliminary_surface_level(8, 8);
        assert!(
            (-64..=320).contains(&surf),
            "unexpected preliminary surface level {surf}"
        );
        // Second call hits the per-quart cache and returns the exact same value.
        assert_eq!(noise_chunk.preliminary_surface_level(11, 11), surf);

        // Verify cell interpolation across the first cell column.
        noise_chunk.initialize_for_first_cell_x();
        noise_chunk.advance_cell_x(0);
        noise_chunk.select_cell_yz(16, 0); // cell_y = -8 + 16 = 8 (block_y = 64)
        noise_chunk.update_for_y(0, 0.0);
        noise_chunk.update_for_x(0, 0.0);
        noise_chunk.update_for_z(0, 0.0);
        let d_interp = noise_chunk.eval_node(router.final_density);
        assert!(d_interp.is_finite());
        noise_chunk.stop_interpolation();

        // At a cell corner (in_cell = 0,0,0), interpolated evaluation equals exact corner evaluation!
        let d_exact = noise_chunk.eval_point(router.final_density, 0, 64, 0);
        assert_eq!(d_interp.to_bits(), d_exact.to_bits());

        // Verify MultiNoiseBiomeSource and section biome filling.
        let plist = &data.parameter_lists["minecraft:overworld"];
        let biome_source = MultiNoiseBiomeSource::from_parameter_list(plist, &data)
            .expect("overworld biome source");
        let mut section = Section::new(
            PalettedContainer::filled(Strategy::block_states(15), 0),
            PalettedContainer::filled(Strategy::biomes(7), 0),
        );
        let mut last_result = None;
        biome_source.fill_section_biomes(&mut section, 0, 4, 0, &mut noise_chunk, &mut last_result);
        let biome_id = section.biome(0, 0, 0);
        assert!(data.biome_name(biome_id).is_some());
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
}
