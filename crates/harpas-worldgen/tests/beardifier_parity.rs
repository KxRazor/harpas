//! Differential tests for structure density, including collection at chunk and kernel boundaries.

mod reference;

use std::io::{BufReader, Read};

use harpas_worldgen::{
    Beardifier, BoundingBox, JigsawJunction, JigsawPiece, PoolElementDef, Projection, Rotation,
    StructurePiece, StructureStart, WorldgenData,
};
use reference::Reference;

const MODES: [&str; 5] = ["none", "bury", "beard_thin", "beard_box", "encapsulate"];

fn piece(
    min: [i32; 3],
    max: [i32; 3],
    projection: Option<Projection>,
    delta: i32,
) -> StructurePiece {
    StructurePiece {
        kind: "test:piece".to_string(),
        bounding_box: BoundingBox { min, max },
        orientation: None,
        gen_depth: 0,
        junctions: Vec::new(),
        jigsaw: projection.map(|projection| JigsawPiece {
            element: PoolElementDef::Single {
                location: "test:piece".into(),
                processors: None,
                projection,
                legacy: false,
            },
            position: min,
            rotation: Rotation::None,
            ground_level_delta: delta,
        }),
        template: None,
        save_data: Default::default(),
    }
}

fn fixture(mode: &str) -> StructureStart {
    let mut pieces = vec![
        piece([-4, 40, -4], [8, 58, 8], None, 0),
        piece([14, 52, -3], [23, 72, 8], Some(Projection::Rigid), 3),
        piece(
            [-11, 49, 12],
            [-1, 61, 21],
            Some(Projection::TerrainMatching),
            -2,
        ),
        piece([100, 50, 0], [105, 60, 5], Some(Projection::Rigid), 0),
        piece([-17, 32, -17], [-12, 42, -12], Some(Projection::Rigid), -4),
        piece([27, -20, 27], [32, -10, 32], Some(Projection::Rigid), 40),
    ];
    // Strict junction bounds, both destination projections and a remote piece with a nearby junction.
    for (index, piece) in pieces
        .iter_mut()
        .enumerate()
        .filter(|(_, p)| p.jigsaw.is_some())
    {
        for x in [-13, -12, -11, 0, 15, 26, 27, 28] {
            piece.junctions.push(JigsawJunction {
                source: [x, 43 + index as i32, x],
                delta_y: 2,
                destination_rigid: index % 2 == 0,
            });
        }
    }
    StructureStart {
        structure: format!("test:{mode}"),
        chunk: (0, 0),
        references: 0,
        pieces,
        bounding_box: BoundingBox {
            min: [-29, -32, -29],
            max: [117, 84, 44],
        },
    }
}

fn data() -> WorldgenData {
    WorldgenData::from_json_entries(MODES.map(|mode| {
        (
            format!("data/test/worldgen/structure/{mode}.json"),
            format!(
                r#"{{"type":"minecraft:swamp_hut","biomes":[],"terrain_adaptation":"{mode}"}}"#
            ),
        )
    }))
    .unwrap()
}

fn write_start(text: &mut String, start: &StructureStart) {
    use std::fmt::Write;
    writeln!(
        text,
        "start {}",
        start.structure.strip_prefix("test:").unwrap()
    )
    .unwrap();
    for piece in &start.pieces {
        let projection = match &piece.jigsaw {
            None => "plain",
            Some(jigsaw) => match &jigsaw.element {
                PoolElementDef::Single {
                    projection: Projection::Rigid,
                    ..
                } => "rigid",
                _ => "terrain_matching",
            },
        };
        let [x, y, z] = piece.bounding_box.min;
        let [xx, yy, zz] = piece.bounding_box.max;
        let delta = piece.jigsaw.as_ref().map_or(0, |j| j.ground_level_delta);
        writeln!(
            text,
            "piece {projection} {x} {y} {z} {xx} {yy} {zz} {delta}"
        )
        .unwrap();
        for junction in &piece.junctions {
            let [x, y, z] = junction.source;
            writeln!(
                text,
                "junction {x} {y} {z} {} {}",
                junction.delta_y,
                i32::from(junction.destination_rigid)
            )
            .unwrap();
        }
    }
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn collected_structure_density_matches_vanilla() {
    use std::fmt::Write;
    let reference = Reference::required();
    reference.compile(&[("BeardifierProbe.java", include_str!("BeardifierProbe.java"))]);
    let data = data();
    let mut text = String::new();
    let mut expected = Vec::new();
    // Every mode separately, then all modes together in two addition orders.
    let mut cases: Vec<_> = MODES.iter().map(|mode| vec![fixture(mode)]).collect();
    cases.push(MODES.iter().map(|mode| fixture(mode)).collect());
    cases.push(MODES.iter().rev().map(|mode| fixture(mode)).collect());
    for starts in &cases {
        writeln!(text, "reset").unwrap();
        for start in starts {
            write_start(&mut text, start);
        }
        for (cx, cz) in [(-1, -1), (0, 0), (1, 1), (5, 0)] {
            writeln!(text, "chunk {cx} {cz}").unwrap();
            let beard = Beardifier::for_starts(cx, cz, &data, starts).unwrap();
            for x in (-36..=132).step_by(3) {
                for z in (-36..=60).step_by(3) {
                    for y in (-48..=108).step_by(2) {
                        writeln!(text, "sample {x} {y} {z}").unwrap();
                        expected.push(beard.compute(x, y, z));
                    }
                }
            }
        }
    }
    let input = reference.temp.0.join("density-input.txt");
    let output = reference.temp.0.join("density.bin");
    std::fs::write(&input, text).unwrap();
    reference.java(
        "BeardifierProbe",
        [input.display().to_string(), output.display().to_string()],
    );
    let mut input = BufReader::new(std::fs::File::open(output).unwrap());
    for (index, expected) in expected.iter().enumerate() {
        let mut bytes = [0; 8];
        input.read_exact(&mut bytes).unwrap();
        let actual = f64::from_be_bytes(bytes);
        assert_eq!(
            actual.to_bits(),
            expected.to_bits(),
            "density sample {index}: vanilla {actual}, Harpas {expected}"
        );
    }
    assert_eq!(input.read(&mut [0]).unwrap(), 0, "extra vanilla samples");
    eprintln!("Compared {} structure density samples", expected.len());
}

#[test]
fn empty_unknown_and_unsupported_structures_are_handled_explicitly() {
    let mut data = data();
    assert!(Beardifier::for_starts(0, 0, &data, &[]).unwrap().is_empty());
    let none = fixture("none");
    assert!(
        Beardifier::for_starts(0, 0, &data, [&none])
            .unwrap()
            .is_empty()
    );
    let mut start = fixture("bury");
    data.structures
        .get_mut(&start.structure)
        .unwrap()
        .terrain_adaptation = "test:unknown".into();
    assert!(Beardifier::for_starts(0, 0, &data, [&start]).is_err());
    start.structure = "test:missing".into();
    assert!(Beardifier::for_starts(0, 0, &data, [&start]).is_err());
}

#[test]
fn bury_and_encapsulate_have_their_distinct_vertical_extent() {
    let data = data();
    let mut start = fixture("bury");
    start.pieces = vec![piece([0, 40, 0], [15, 60, 15], None, 0)];
    let bury = Beardifier::for_starts(0, 0, &data, [&start]).unwrap();
    assert_eq!(bury.compute(8, 40, 8), 1.0);
    assert_eq!(bury.compute(8, 46, 8), 0.5);
    assert_eq!(bury.compute(8, 52, 8), 0.0);
    assert_eq!(bury.compute(8, 34, 8), 0.5);
    start.structure = "test:encapsulate".into();
    let encapsulate = Beardifier::for_starts(0, 0, &data, [&start]).unwrap();
    assert_eq!(encapsulate.compute(8, 40, 8), 0.8);
    assert_eq!(encapsulate.compute(8, 60, 8), 0.8);
    assert_eq!(encapsulate.compute(8, 66, 8), 0.4);
    assert_eq!(encapsulate.compute(8, 72, 8), 0.0);
}

#[test]
fn jigsaw_ground_and_terrain_matching_junctions_affect_density() {
    let data = data();
    let mut start = fixture("beard_thin");
    start.pieces = vec![piece([0, 40, 0], [15, 60, 15], Some(Projection::Rigid), 3)];
    let beard = Beardifier::for_starts(0, 0, &data, [&start]).unwrap();
    assert!(beard.compute(8, 42, 8) > 0.0);
    assert!(beard.compute(8, 43, 8) < 0.0);
    start.pieces = vec![piece(
        [0, 40, 0],
        [15, 60, 15],
        Some(Projection::TerrainMatching),
        3,
    )];
    assert!(
        Beardifier::for_starts(0, 0, &data, [&start])
            .unwrap()
            .is_empty()
    );
    start.pieces[0].junctions.push(JigsawJunction {
        source: [8, 43, 8],
        delta_y: 7,
        destination_rigid: false,
    });
    let junction = Beardifier::for_starts(0, 0, &data, [&start]).unwrap();
    assert!(junction.compute(8, 42, 8) > 0.0);
    assert!(junction.compute(8, 43, 8) < 0.0);
    assert_eq!(junction.compute(8, 55, 8), 0.0);
    assert_eq!(junction.compute(8, 1000, 8), 0.0);
}
