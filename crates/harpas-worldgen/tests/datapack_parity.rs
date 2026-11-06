//! Loads vanilla's datapack and runs it through every stage of generation. Checks Harpas alone, against facts about
//! the data and the terrain; `vanilla_parity` and `chunk_parity` compare the results with vanilla's.

#[allow(dead_code)]
mod reference;

use harpas_core::chunk::{PalettedContainer, Section, Strategy};
use harpas_worldgen::{
    CompiledDensityGraph, FeatureConfigDef, FeatureIndex, FeatureRefDef, HeightProviderDef,
    HeightmapType, IntProviderDef, MultiNoiseBiomeSource, NoiseBasedChunkGenerator, NoiseChunk,
    PlacementModifierDef, SurfaceRule, VerticalAnchor, WorldgenData, carver,
};
use reference::VanillaData;

#[test]
fn loads_and_evaluates_the_26_1_datapack() {
    let Some(vanilla) = VanillaData::find() else {
        eprintln!("Skipping the vanilla datapack: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };

    let data = WorldgenData::from_zip_path(&vanilla.path).expect("load 26.1.zip");
    assert_eq!(data.biomes.len(), 65);
    assert_eq!(data.noises.len(), 62);
    assert_eq!(data.density_functions.len(), 35);
    assert_eq!(data.configured_carvers.len(), 4);
    for name in data.configured_carvers.keys() {
        carver::CarverConfig::compile(name, &data)
            .unwrap_or_else(|error| panic!("compile carver {name}: {error}"));
    }

    let ore = &data.configured_features["minecraft:ore_iron"];
    assert_eq!(ore.feature_type, "minecraft:ore");
    let FeatureConfigDef::Ore(ore) = &ore.config else {
        panic!("minecraft:ore_iron must parse as an ore feature");
    };
    assert!(ore.size > 0);
    assert_eq!(ore.targets.len(), 2);
    assert!(
        ore.targets
            .iter()
            .all(|target| target.state.name.ends_with("iron_ore"))
    );
    let placed_ore = &data.placed_features["minecraft:ore_iron_middle"];
    assert!(matches!(
        placed_ore.feature,
        FeatureRefDef::Reference(ref reference) if reference == "minecraft:ore_iron"
    ));
    assert_eq!(
        placed_ore.placement.first(),
        Some(&PlacementModifierDef::Count(IntProviderDef::Constant(10)))
    );
    assert!(
        placed_ore
            .placement
            .contains(&PlacementModifierDef::InSquare)
    );
    assert_eq!(
        placed_ore
            .placement
            .iter()
            .find_map(|modifier| match modifier {
                PlacementModifierDef::HeightRange(height) => Some(height),
                _ => None,
            }),
        Some(&HeightProviderDef::Trapezoid {
            min_inclusive: VerticalAnchor::Absolute(-24),
            max_inclusive: VerticalAnchor::Absolute(56),
            plateau: 0,
        })
    );
    assert!(placed_ore.placement.contains(&PlacementModifierDef::Biome));

    for (name, placed) in &data.placed_features {
        if let FeatureRefDef::Reference(reference) = &placed.feature {
            assert!(
                data.configured_features.contains_key(reference),
                "placed feature {name} references missing configured feature {reference}"
            );
        }
    }

    let index = FeatureIndex::new(&data).expect("compile the feature index");
    assert_eq!(
        index.sizes(),
        (data.biomes.len(), data.placed_features.len())
    );
    let iron_middle = data
        .placed_features
        .get_index_of("minecraft:ore_iron_middle")
        .expect("iron ore placed feature");
    let compiled = index.ore(iron_middle).expect("iron ore compiles");
    assert_eq!(compiled.size, 9);
    assert_eq!(compiled.targets.len(), 2);
    assert!(
        index
            .allowed_biomes(iron_middle)
            .is_some_and(|biomes| !biomes.is_empty())
    );
    for (name, biome) in &data.biomes {
        for (step, features) in biome.features.iter().enumerate() {
            for feature in features {
                assert!(
                    data.placed_features.contains_key(feature),
                    "biome {name} step {step} references missing placed feature {feature}"
                );
            }
        }
    }
    assert!(
        data.block_tags
            .contains_key("minecraft:overworld_carver_replaceables")
    );
    let replaceable = data
        .block_tag_states("minecraft:overworld_carver_replaceables")
        .expect("resolve vanilla carver replaceables");
    let states = harpas_core::block::block_states();
    assert!(replaceable[states.default_state("minecraft:stone").unwrap() as usize]);
    assert!(!replaceable[states.default_state("minecraft:bedrock").unwrap() as usize]);
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
    let biome_source =
        MultiNoiseBiomeSource::from_parameter_list(plist, &data).expect("overworld biome source");
    let mut section = Section::new(
        PalettedContainer::filled(Strategy::block_states(15), 0),
        PalettedContainer::filled(Strategy::biomes(7), 0),
    );
    let mut last_result = None;
    biome_source.fill_section_biomes(&mut section, 0, 4, 0, &mut noise_chunk, &mut last_result);
    let biome_id = section.biome(0, 0, 0);
    assert!(data.biome_name(biome_id).is_some());

    // Compile surface rules for all 7 noise_settings definitions in 26.1.zip.
    for (name, ns) in &data.noise_settings {
        let (ns_graph, _) = CompiledDensityGraph::compile_router(
            123_456_789,
            ns.legacy_random_source,
            &ns.noise_router,
            &data.noises,
            &data.density_functions,
        )
        .unwrap_or_else(|e| panic!("compile router for {name}: {e}"));
        SurfaceRule::compile(&ns.surface_rule, &ns_graph, &data)
            .unwrap_or_else(|e| panic!("compile surface_rule for {name}: {e}"));
    }

    // Generate a full Overworld chunk (0, 0) up to the surface stage.
    let generator =
        NoiseBasedChunkGenerator::overworld(123_456_789, &data).expect("overworld chunk generator");
    let generated = generator.generate_chunk_surface(0, 0, &data);
    let states = harpas_core::block::block_states();
    let bedrock = states.default_state("minecraft:bedrock").unwrap();
    let deepslate = states.default_state("minecraft:deepslate").unwrap();
    let air = states.default_state("minecraft:air").unwrap();

    for lz in 0..16 {
        for lx in 0..16 {
            // y = -64 is always 100% bedrock in Overworld.
            assert_eq!(generated.block_state(lx, -64, lz), bedrock);
            // Surface heightmap is within valid Overworld range and above y = -60.
            let surf_y = generated
                .heightmaps()
                .get(HeightmapType::WorldSurfaceWg, lx, lz);
            assert!(
                (-50..=320).contains(&surf_y),
                "unexpected surface height {surf_y} at ({lx}, {lz})"
            );
            // Above WORLD_SURFACE_WG is air.
            if surf_y < 320 {
                assert_eq!(generated.block_state(lx, surf_y, lz), air);
            }
        }
    }
    // Check that deepslate replaces stone below y = 0.
    let mut deepslate_count = 0;
    for lz in 0..16 {
        for lx in 0..16 {
            if generated.block_state(lx, -32, lz) == deepslate {
                deepslate_count += 1;
            }
        }
    }
    assert!(deepslate_count > 100, "expected deepslate at y = -32");
    assert_eq!(
        generated
            .heightmaps()
            .pack(HeightmapType::MotionBlocking)
            .len(),
        37
    );
}
