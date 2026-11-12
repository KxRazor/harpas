//! Compares monument room graphs, fitted pieces, bounds, designs and reloads against vanilla.

mod reference;

use std::fs;
use std::io::{BufRead, BufReader};

use harpas_core::random::{LegacyRandom, RandomSource};
use harpas_worldgen::{
    BoundingBox, Direction, MonumentLayout, MonumentRoomKind, StructurePiece, StructureStart,
};
use reference::Reference;

fn kind_name(kind: MonumentRoomKind) -> &'static str {
    use MonumentRoomKind::*;
    match kind {
        Entry => "EntryRoom",
        Core => "CoreRoom",
        DoubleXY => "DoubleXYRoom",
        DoubleYZ => "DoubleYZRoom",
        DoubleZ => "DoubleZRoom",
        DoubleX => "DoubleXRoom",
        DoubleY => "DoubleYRoom",
        SimpleTop => "SimpleTopRoom",
        Simple => "SimpleRoom",
        Wing => "WingRoom",
        Penthouse => "Penthouse",
    }
}

fn describe(layout: &MonumentLayout) -> Vec<String> {
    let mut lines = vec![format!(
        "layout {} 2 {}",
        layout.orientation.data_value(),
        layout.core_room
    )];
    for room in layout.rooms.values() {
        let mut line = format!("room {}", room.index);
        for d in 0..6 {
            line.push_str(&format!(
                " {}:{}",
                room.connections[d].map_or(-1, |id| id as i32),
                i32::from(room.openings[d])
            ));
        }
        lines.push(line);
    }
    for piece in &layout.pieces {
        let b = piece.bounding_box;
        lines.push(format!(
            "piece {} {} {} {} {} {} {} {} {}",
            kind_name(piece.kind),
            piece.room.map_or(-1, |id| id as i32),
            b.min[0],
            b.min[1],
            b.min[2],
            b.max[0],
            b.max[1],
            b.max[2],
            piece.design.unwrap_or(-1)
        ));
    }
    lines
}

fn start(x: i32, z: i32, orientation: Option<Direction>, chunk: (i32, i32)) -> StructureStart {
    let bounding_box = BoundingBox {
        min: [x, 39, z],
        max: [x + 57, 61, z + 57],
    };
    StructureStart {
        structure: "minecraft:monument".into(),
        chunk,
        references: 0,
        bounding_box,
        pieces: vec![StructurePiece {
            kind: "minecraft:omb".into(),
            bounding_box,
            orientation,
            gen_depth: 0,
            junctions: Vec::new(),
            jigsaw: None,
            template: None,
            save_data: Default::default(),
        }],
    }
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn monument_layouts_match_vanilla() {
    let reference = Reference::required();
    let mut cases = String::new();
    let seeds = [0, 1, -1, 42, i64::MIN, i64::MAX, 0x1234_5678_9abc_def0];
    let count = if reference::reduced_sample() { 16 } else { 256 };
    for seed in seeds.into_iter().chain(2..count) {
        for (x, z) in [(10, 20), (-125, -333), (29_999_900, -29_999_900)] {
            for orientation in [
                Direction::South,
                Direction::West,
                Direction::North,
                Direction::East,
            ] {
                cases.push_str(&format!(
                    "0 {seed} {x} {z} {} 0 0\n",
                    orientation.data_value()
                ));
            }
        }
    }
    for seed in seeds {
        for chunk in [(0, 0), (-3, 4), (1_874_990, -1_874_990)] {
            for direction in -1..4 {
                // Deliberately retain an origin and orientation that need not match a fresh start.
                cases.push_str(&format!(
                    "1 {seed} -100 999 {direction} {} {}\n",
                    chunk.0, chunk.1
                ));
            }
        }
    }
    let inputs = reference.temp.0.join("cases.txt");
    let output = reference.temp.0.join("layouts.txt");
    fs::write(&inputs, &cases).unwrap();
    reference.compile(&[(
        "MonumentLayoutProbe.java",
        include_str!("MonumentLayoutProbe.java"),
    )]);
    reference.java(
        "MonumentLayoutProbe",
        [inputs.display().to_string(), output.display().to_string()],
    );
    let mut actual = BufReader::new(fs::File::open(output).unwrap()).lines();
    for case in cases.lines() {
        let numbers: Vec<i64> = case
            .split_whitespace()
            .map(|word| word.parse().unwrap())
            .collect();
        let [mode, seed, x, z, direction, chunk_x, chunk_z] = numbers.try_into().unwrap();
        let orientation = match direction {
            0 => Some(Direction::South),
            1 => Some(Direction::West),
            2 => Some(Direction::North),
            3 => Some(Direction::East),
            _ => None,
        };
        let mut random = LegacyRandom::new(seed);
        let layout = if mode == 0 {
            MonumentLayout::new(&mut random, x as i32, z as i32, orientation.unwrap())
        } else {
            MonumentLayout::from_start(
                seed,
                &start(
                    x as i32,
                    z as i32,
                    orientation,
                    (chunk_x as i32, chunk_z as i32),
                ),
            )
            .unwrap()
        };
        let mut expected = vec![format!("case {case}")];
        expected.extend(describe(&layout));
        if mode == 0 {
            expected.push(format!("tail {}", random.next_long()));
        }
        for line in expected {
            let vanilla = actual.next().expect("vanilla output ended early").unwrap();
            assert_eq!(line, vanilla, "case {case}");
        }
    }
    assert!(actual.next().is_none(), "extra vanilla output");
    eprintln!("Compared {} monument layouts", cases.lines().count());
}
