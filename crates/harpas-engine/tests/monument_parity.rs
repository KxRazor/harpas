//! Ocean monuments placed on chunk workers, compared with the direct placement API and Anvil.
//!
//! The world generation API is compared block by block with vanilla in
//! `harpas-worldgen/tests/monument_interior_parity.rs`. This harness checks the engine wiring: the
//! monument is dispatched to every chunk intersecting the saved bounds, with the five-block exterior
//! clearing clipped to each, chunks outside those bounds left carved, saved records keeping the single
//! building and its references, request order unchanged, and the saved chunks surviving a region round
//! trip.

#[allow(dead_code)]
#[path = "../../harpas-worldgen/tests/reference/mod.rs"]
mod reference;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use harpas_anvil::{Chunk, Region};
use harpas_core::nbt::{Compound, Nbt};
use harpas_engine::world::{ChunkPos, ChunkReader};
use harpas_engine::worldgen::overworld_generator;
use harpas_worldgen::{
    Beardifier, BoundingBox, Direction, GeneratedChunk, HeightmapType, NoiseBasedChunkGenerator,
    StructurePiece, StructureStart, StructureTemplates, WorldgenData,
};
use reference::VanillaData;

fn monument(origin: [i32; 3], direction: Direction) -> StructureStart {
    let b = BoundingBox {
        min: origin,
        max: [origin[0] + 57, origin[1] + 22, origin[2] + 57],
    };
    StructureStart {
        structure: "minecraft:monument".into(),
        chunk: (origin[0] >> 4, origin[2] >> 4),
        references: 0,
        bounding_box: b,
        pieces: vec![StructurePiece {
            kind: "minecraft:omb".into(),
            bounding_box: b,
            orientation: Some(direction),
            gen_depth: 0,
            junctions: Vec::new(),
            jigsaw: None,
            template: None,
            save_data: Default::default(),
        }],
    }
}

/// The proto-chunk a worker reads to find the saved start. It only needs a status and the start:
/// its terrain is regenerated for the target chunk.
fn saved_chunk(start: &StructureStart) -> Compound {
    let mut starts = Compound::new();
    starts.insert(start.structure.clone(), Nbt::Compound(start.to_nbt()));
    let mut structures = Compound::new();
    structures.insert("starts", Nbt::Compound(starts));
    structures.insert("References", Nbt::Compound(Compound::new()));
    let mut chunk = Compound::new();
    chunk.insert("Status", "minecraft:carvers");
    chunk.insert("structures", Nbt::Compound(structures));
    chunk
}

fn structure_records(chunk: &Chunk) -> &Compound {
    let Some(Nbt::Compound(structures)) = chunk.rest.get("structures") else {
        panic!("generated chunk has no structures compound");
    };
    structures
}

fn references(chunk: &Chunk) -> Compound {
    match structure_records(chunk).get("References") {
        Some(Nbt::Compound(references)) => references.clone(),
        other => panic!("references: {other:?}"),
    }
}

fn starts(chunk: &Chunk) -> Compound {
    match structure_records(chunk).get("starts") {
        Some(Nbt::Compound(starts)) => starts.clone(),
        other => panic!("starts: {other:?}"),
    }
}

fn packed(pos: (i32, i32)) -> i64 {
    i64::from(pos.0 as u32) | (i64::from(pos.1 as u32) << 32)
}

fn sections(chunk: &Chunk, min_y: i32, height: i32, biomes: usize) -> GeneratedChunk {
    let mut generated = GeneratedChunk::new(0, 0, min_y, height, biomes);
    for section in &chunk.sections {
        let index = i32::from(section.y) - generated.min_section_y();
        let Ok(index) = usize::try_from(index) else {
            continue;
        };
        let Some(target) = generated.sections_mut().get_mut(index) else {
            continue;
        };
        if let Some(blocks) = &section.section {
            *target = blocks.clone();
        }
    }
    generated.recompute_heightmaps();
    generated
}

fn assert_blocks_match(actual: &Chunk, expected: &GeneratedChunk, pos: ChunkPos) {
    for (index, _section) in expected.sections().iter().enumerate() {
        let y = expected.min_section_y() + index as i32;
        let Some(section) = actual
            .sections
            .iter()
            .find(|section| i32::from(section.y) == y)
            .and_then(|section| section.section.as_ref())
        else {
            panic!("{pos:?}: missing section {y}");
        };
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    assert_eq!(
                        section.block_state(x, y, z),
                        expected.sections()[index].block_state(x, y, z),
                        "{pos:?}: block {x} {y} {z} in section {y}"
                    );
                }
            }
        }
    }
}

#[test]
fn carved_monuments_on_workers_match_the_direct_api_and_survive_anvil() {
    let Some(vanilla) = VanillaData::find() else {
        eprintln!("Skipping monument workers: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    let mut data = WorldgenData::from_zip_path(&vanilla.path).unwrap();
    // Isolate this structure; the direct API below places only the monument.
    data.structure_sets.retain(|_, set| {
        set.structures
            .iter()
            .any(|s| s.structure == "minecraft:monument")
    });
    let data = Arc::new(data);
    let templates = Arc::new(StructureTemplates::from_zip_path(&vanilla.path).unwrap());
    let biomes: Vec<_> = data.biomes.keys().map(String::as_str).collect();
    let seed = 0;
    // Vanilla's StructureStart.placeInChunk invokes a piece only when its bounding box intersects
    // the chunk (observed from the reference server), so a chunk outside the saved bounds stays
    // carved even though the five-block clearing would reach it if the pass ran. An origin at a
    // chunk corner leaves chunk (-1,-1) in that situation.
    let start = monument([0, 39, 0], Direction::South);
    let source = start.chunk;
    let saved = saved_chunk(&start);
    let read: &ChunkReader<'_> =
        &|pos: ChunkPos| Ok(((pos.x, pos.z) == source).then(|| saved.clone()));
    let generator = overworld_generator(
        seed,
        Arc::clone(&data),
        Arc::clone(&templates),
        true,
        &biomes,
    )
    .unwrap();
    let terrain = NoiseBasedChunkGenerator::overworld(seed, &data).unwrap();
    let sea_level = terrain.sea_level();
    let b = start.bounding_box;
    let min_x = (b.min[0] - 5) >> 4;
    let max_x = (b.max[0] + 5) >> 4;
    let min_z = (b.min[2] - 5) >> 4;
    let max_z = (b.max[2] + 5) >> 4;
    let positions: Vec<_> = (min_x..=max_x)
        .flat_map(|x| (min_z..=max_z).map(move |z| ChunkPos { x, z }))
        .collect();

    let mut expected = HashMap::new();
    let mut outside = 0;
    for &pos in &positions {
        let beardifier =
            Beardifier::for_starts(pos.x, pos.z, &data, [&start]).expect("monument beardifier");
        let mut direct = terrain
            .generate_chunk_carved_with_beardifier(pos.x, pos.z, &data, beardifier)
            .expect("carved chunk");
        let carved = direct.clone();
        let intersects =
            b.intersects_columns(pos.x * 16, pos.z * 16, pos.x * 16 + 15, pos.z * 16 + 15);
        if intersects {
            start.place_monument_in_chunk(&mut direct, seed, sea_level);
        } else {
            outside += 1;
            assert_eq!(
                direct.sections(),
                carved.sections(),
                "{pos:?}: the direct pass must not run outside the saved bounds"
            );
        }
        expected.insert((pos.x, pos.z), direct);
    }
    assert!(
        outside > 0,
        "the sample must include chunks outside the saved bounds"
    );

    let mut first = HashMap::new();
    for reverse in [false, true] {
        let mut order = positions.clone();
        if reverse {
            order.reverse();
        }
        for pos in order {
            let chunk = generator(pos, read).expect("worker chunk");
            let nbt = chunk.to_nbt(&biomes);
            if reverse {
                assert_eq!(first[&(pos.x, pos.z)], nbt, "reversed request {pos:?}");
            } else {
                first.insert((pos.x, pos.z), nbt);
            }
            let expected = &expected[&(pos.x, pos.z)];
            assert_blocks_match(&chunk, expected, pos);
            let actual_sections =
                sections(&chunk, expected.min_y(), expected.height(), biomes.len());
            // Chunks keep only the final heightmaps once carvers have run; the worldgen ones are not saved.
            for kind in HeightmapType::FINAL {
                for z in 0..16 {
                    for x in 0..16 {
                        assert_eq!(
                            expected.heightmaps().get(kind, x, z),
                            actual_sections.heightmaps().get(kind, x, z),
                            "{pos:?}: {kind:?} at {x}, {z}"
                        );
                    }
                }
            }

            let saved_starts = starts(&chunk);
            let saved_references = references(&chunk);
            let intersects =
                b.intersects_columns(pos.x * 16, pos.z * 16, pos.x * 16 + 15, pos.z * 16 + 15);
            let mut want_references = Compound::new();
            if intersects {
                want_references.insert("minecraft:monument", Nbt::LongArray(vec![packed(source)]));
            }
            assert_eq!(saved_references, want_references, "{pos:?}: references");
            if (pos.x, pos.z) == source {
                assert_eq!(
                    saved_starts.get("minecraft:monument"),
                    Some(&Nbt::Compound(start.to_nbt())),
                    "{pos:?}: saved building record"
                );
            } else {
                assert!(saved_starts.is_empty(), "{pos:?}: copied start");
            }
        }
    }

    // The saved chunks must read back unchanged from region files.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "harpas-monument-region-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let mut regions = HashMap::new();
    for (&(x, z), nbt) in &first {
        let (region_x, region_z) = harpas_anvil::region_of(x, z);
        let region = regions
            .entry((region_x, region_z))
            .or_insert_with(|| Region::open(&dir, region_x, region_z).unwrap());
        region.set_chunk(x, z, nbt);
    }
    for region in regions.values_mut() {
        region.save().unwrap();
    }
    for (&(x, z), nbt) in &first {
        let Some(tag) = harpas_anvil::read_chunk(&dir, x, z).unwrap() else {
            panic!("missing saved chunk {x}, {z}");
        };
        let (reloaded, warnings) = Chunk::read(tag, &biomes).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(&reloaded.to_nbt(&biomes), nbt, "region round trip {x}, {z}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
