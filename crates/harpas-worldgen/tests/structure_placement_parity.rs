//! Compares where structures may start with an owner-provided server.

mod reference;

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use harpas_worldgen::{NoiseBasedChunkGenerator, StructureState, WorldgenData};
use reference::Reference;

const SEEDS: [i64; 5] = [0, 42, -1, 123_456_789, -4_172_144_997_902_289_642];
/// Chunks in each direction around the origin.
fn radius() -> i32 {
    if reference::reduced_sample() {
        128
    } else {
        1024
    }
}
/// Centers of smaller areas, far from the origin, near the edges of the world and where regions wrap.
const WINDOWS: [(i32, i32); 3] = [
    (1_874_900, -1_874_900),
    (-1_874_900, 1_874_900),
    (40_000, -70_000),
];
/// Chunks in each direction around a window's center, as in `StructurePlacementProbe.WINDOW`.
const WINDOW: i32 = 64;

fn starts(state: &StructureState, set: &str, center: (i32, i32), radius: i32) -> Vec<String> {
    let mut lines = Vec::new();
    for x in center.0 - radius..center.0 + radius {
        for z in center.1 - radius..center.1 + radius {
            if state.is_structure_chunk(set, x, z) {
                lines.push(format!("start {set} {x} {z}"));
            }
        }
    }
    lines
}

fn compare_seed(data: &WorldgenData, seed: i64, output: &Path) -> usize {
    let generator = NoiseBasedChunkGenerator::overworld(seed, data).expect("compile generator");
    let state = StructureState::new(seed, data, &generator).expect("structure state");
    let mut compared = 0;
    let mut lines = BufReader::new(File::open(output).unwrap()).lines();
    let mut check = |expected: String| {
        let vanilla = lines.next().expect("missing Java result").unwrap();
        assert_eq!(vanilla, expected, "structure placement for seed {seed}");
        compared += 1;
    };
    for set in state.possible_sets() {
        check(format!("set {set}"));
        for (x, z) in state.ring_positions(set).unwrap_or_default() {
            check(format!("ring {set} {x} {z}"));
        }
        for line in starts(&state, set, (0, 0), radius()) {
            check(line);
        }
        for window in WINDOWS {
            for line in starts(&state, set, window, WINDOW) {
                check(line);
            }
        }
    }
    assert!(lines.next().is_none(), "unexpected extra Java results");
    compared
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn structure_starts_match_vanilla() {
    let reference = Reference::required();
    let data =
        WorldgenData::from_zip_path(&reference.server_jar).expect("load vanilla worldgen data");
    reference.compile(&[(
        "StructurePlacementProbe.java",
        include_str!("StructurePlacementProbe.java"),
    )]);
    for &seed in reference::sampled_seeds(&SEEDS) {
        let output = reference.temp.0.join("placement.txt");
        let mut args = vec![
            output.display().to_string(),
            seed.to_string(),
            radius().to_string(),
        ];
        args.extend(WINDOWS.iter().map(|(x, z)| format!("{x},{z}")));
        reference.java("StructurePlacementProbe", args);
        let compared = compare_seed(&data, seed, &output);
        eprintln!("Seed {seed}: {compared} placement results match");
    }
}
