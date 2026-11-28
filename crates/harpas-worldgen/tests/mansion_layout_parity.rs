//! Compares woodland mansion layouts with vanilla's: the corridors and rooms of each floor, cell by cell, and
//! how much of the random they draw.

mod reference;

use harpas_core::random::{LegacyRandom, RandomSource};
use harpas_worldgen::mansion_layout_grids;
use reference::Reference;

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn mansion_layouts_match_vanilla() {
    let reference = Reference::required();
    let count = if reference::reduced_sample() {
        256
    } else {
        4096
    };
    let seeds: Vec<i64> = [-1, i64::MIN, i64::MAX, 0x1234_5678_9abc_def0]
        .into_iter()
        .chain(0..count)
        .collect();
    reference.compile(&[(
        "MansionLayoutProbe.java",
        include_str!("MansionLayoutProbe.java"),
    )]);
    let output = reference.temp.0.join("mansion-layouts.txt");
    let mut args = vec![output.display().to_string()];
    args.extend(seeds.iter().map(i64::to_string));
    reference.java("MansionLayoutProbe", args);
    let vanilla = reference::read_text(&output);
    let mut vanilla = vanilla.lines();

    let mut failures = Vec::new();
    let mut third_floors = 0;
    for seed in seeds {
        let mut random = LegacyRandom::new(seed);
        let grids = mansion_layout_grids(&mut random);
        third_floors += usize::from(grids[1].contains(&1));
        let names = ["base", "third", "rooms", "rooms", "rooms"];
        let mut expected = vec![format!("seed {seed}")];
        for (name, grid) in names.into_iter().zip(&grids) {
            let cells: Vec<_> = grid.iter().map(i32::to_string).collect();
            expected.push(format!("{name} {}", cells.join(" ")));
        }
        expected.push(format!("tail {}", random.next_long()));
        for (index, line) in expected.into_iter().enumerate() {
            let vanilla = vanilla.next().expect("a line for every seed");
            if vanilla != line && failures.len() < 8 {
                failures.push(format!(
                    "seed {seed} line {index}:\n vanilla {vanilla}\n Harpas  {line}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(vanilla.next().is_none(), "vanilla wrote more layouts");
    assert!(third_floors > 0, "the sample must reach third floors");
}
