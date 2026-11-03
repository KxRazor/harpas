//! Compares the structures that start in each chunk, and the chunks they reach, with an owner-provided server.

mod reference;

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use harpas_worldgen::{
    HeightmapType, NoiseBasedChunkGenerator, StructureStart, StructureState, WorldgenData,
};
use reference::Reference;

const SEEDS: [i64; 4] = [0, 42, -1, -4_172_144_997_902_289_642];
/// Chunks in each direction around the origin whose starts are compared.
const RADIUS: i32 = 256;

/// Block coordinates whose base heights are compared, as in `StructureStartProbe.heightCoordinates`.
fn height_coordinates() -> Vec<i32> {
    let mut coordinates: Vec<_> = (-3000..=3000).step_by(151).collect();
    coordinates.extend([29_999_000, -29_999_001]);
    coordinates
}

fn start_lines(start: &StructureStart) -> Vec<String> {
    let box_text = |b: &harpas_worldgen::BoundingBox| {
        format!(
            "{} {} {} {} {} {}",
            b.min[0], b.min[1], b.min[2], b.max[0], b.max[1], b.max[2]
        )
    };
    let mut lines = vec![format!(
        "start {} {} {} {}",
        start.structure,
        start.chunk.0,
        start.chunk.1,
        box_text(&start.bounding_box)
    )];
    for piece in &start.pieces {
        lines.push(format!(
            "piece {} {} {} {}",
            piece.kind,
            box_text(&piece.bounding_box),
            piece.orientation.map_or(-1, |o| o.data_value()),
            piece.gen_depth
        ));
    }
    lines
}

/// Returns how many lines matched and how many starts and references there were.
fn compare_seed(reference: &Reference, data: &WorldgenData, seed: i64) -> (usize, usize, usize) {
    let generator = NoiseBasedChunkGenerator::overworld(seed, data).expect("compile generator");
    let state = StructureState::new(seed, data, &generator).expect("structure state");
    let sets: Vec<_> = state
        .possible_sets()
        .iter()
        .filter(|set| state.supports_set(data, set))
        .cloned()
        .collect();
    assert!(!sets.is_empty(), "no structure set is supported");

    let mut expected = Vec::new();
    for x in height_coordinates() {
        for z in height_coordinates() {
            expected.push(format!(
                "height {x} {z} {} {}",
                generator.base_height(x, z, HeightmapType::WorldSurfaceWg),
                generator.base_height(x, z, HeightmapType::OceanFloorWg)
            ));
        }
    }
    let mut start_count = 0;
    // The chunks each start reaches, and their neighbors, so that references are compared where they exist.
    let mut reference_chunks = BTreeSet::new();
    let inner = RADIUS - 8;
    for x in -RADIUS..RADIUS {
        for z in -RADIUS..RADIUS {
            for start in state.starts_in_chunk(&generator, data, x, z).unwrap() {
                start_count += 1;
                let b = start.bounding_box;
                for cx in (b.min[0] >> 4) - 1..=(b.max[0] >> 4) + 1 {
                    for cz in (b.min[2] >> 4) - 1..=(b.max[2] >> 4) + 1 {
                        if (-inner..inner).contains(&cx) && (-inner..inner).contains(&cz) {
                            reference_chunks.insert((cx, cz));
                        }
                    }
                }
                expected.extend(start_lines(&start));
            }
        }
    }
    for set in &sets {
        for &(x, z) in state.ring_positions(set).unwrap_or_default() {
            for start in state.starts_in_chunk(&generator, data, x, z).unwrap() {
                start_count += 1;
                expected.extend(start_lines(&start));
            }
        }
    }
    let mut reference_count = 0;
    for &(x, z) in &reference_chunks {
        let mut lines: Vec<_> = state
            .references_in_chunk(&generator, data, x, z)
            .unwrap()
            .into_iter()
            .map(|reference| {
                let (sx, sz) = reference.source;
                format!("ref {x} {z} {} {sx} {sz}", reference.structure)
            })
            .collect();
        lines.sort();
        reference_count += lines.len();
        expected.extend(lines);
    }

    let chunk_file = reference.temp.0.join("reference-chunks.txt");
    let chunk_list: Vec<_> = reference_chunks
        .iter()
        .map(|(x, z)| format!("{x} {z}"))
        .collect();
    std::fs::write(&chunk_file, chunk_list.join("\n") + "\n").unwrap();
    let output = reference.temp.0.join("starts.txt");
    let mut args = vec![
        output.display().to_string(),
        seed.to_string(),
        RADIUS.to_string(),
        chunk_file.display().to_string(),
    ];
    args.extend(sets.iter().cloned());
    reference.java("StructureStartProbe", args);
    let vanilla = read_lines(&output);
    for (index, (vanilla, harpas)) in vanilla.iter().zip(&expected).enumerate() {
        assert_eq!(vanilla, harpas, "line {index} for seed {seed}");
    }
    assert_eq!(
        vanilla.len(),
        expected.len(),
        "number of lines for seed {seed}"
    );
    (expected.len(), start_count, reference_count)
}

fn read_lines(path: &Path) -> Vec<String> {
    BufReader::new(File::open(path).unwrap())
        .lines()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn structure_starts_and_references_match_vanilla() {
    let Some(reference) = Reference::from_env() else {
        eprintln!("Skipping vanilla structure starts: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    let data =
        WorldgenData::from_zip_path(&reference.server_jar).expect("load vanilla worldgen data");
    reference.compile(&[
        (
            "WorldgenReferenceProbe.java",
            include_str!("WorldgenReferenceProbe.java"),
        ),
        (
            "StructurePlacementProbe.java",
            include_str!("StructurePlacementProbe.java"),
        ),
        (
            "StructureStartProbe.java",
            include_str!("StructureStartProbe.java"),
        ),
    ]);
    for seed in SEEDS {
        let (lines, starts, references) = compare_seed(&reference, &data, seed);
        eprintln!("Seed {seed}: {lines} lines match, {starts} starts and {references} references");
    }
}
