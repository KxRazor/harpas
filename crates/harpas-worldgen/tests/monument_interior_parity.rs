//! Interior rooms and complete monument passes compared against vanilla, including saved replays.
mod reference;

use std::fs::{self, File};
use std::io::{BufReader, Read};

use flate2::read::ZlibDecoder;
use harpas_core::block::block_states;
use harpas_core::random::{LegacyRandom, RandomSource};
use harpas_worldgen::{
    BoundingBox, Direction, GeneratedChunk, HeightmapType, MonumentLayout, StructurePiece,
    StructureStart, StructureTemplates, WorldgenData,
};
use reference::Reference;

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

fn palette() -> Vec<u32> {
    [
        "air",
        "stone",
        "water",
        "ice",
        "packed_ice",
        "blue_ice",
        "lava",
        "kelp",
        "kelp_plant",
        "seagrass",
        "tall_seagrass",
        "bubble_column",
        "cave_air",
        "void_air",
        "sand",
        "gravel",
        "glass",
        "oak_leaves",
        "snow",
        "powder_snow",
        "short_grass",
        "glow_lichen",
        "prismarine",
    ]
    .map(|name| {
        block_states()
            .default_state(&format!("minecraft:{name}"))
            .unwrap()
    })
    .into_iter()
    .chain([
        block_states()
            .state("minecraft:water", [("level", "7")])
            .unwrap()
            .0,
        block_states()
            .state("minecraft:oak_slab", [("waterlogged", "true")])
            .unwrap()
            .0,
    ])
    .collect()
}

fn terrain(x: i32, y: i32, z: i32, base_y: i32, mode: i32, sea: i32, palette: &[u32]) -> u32 {
    if mode == 1 {
        return palette[1];
    }
    if mode == 2 {
        return palette[0];
    }
    if mode == 3 && (base_y - 16..=base_y + 30).contains(&y) {
        return palette[(x + 3 * z + 7 * y).rem_euclid(palette.len() as i32) as usize];
    }
    let floor = base_y - 9
        + if mode == 4 {
            (x + z).rem_euclid(19) - 9
        } else {
            0
        };
    palette[if y < floor {
        1
    } else if y < sea {
        2
    } else {
        0
    }]
}

struct Input(BufReader<ZlibDecoder<BufReader<File>>>);
impl Input {
    fn int(&mut self) -> i32 {
        let mut bytes = [0; 4];
        self.0.read_exact(&mut bytes).unwrap();
        i32::from_be_bytes(bytes)
    }
    fn long(&mut self) -> i64 {
        let mut bytes = [0; 8];
        self.0.read_exact(&mut bytes).unwrap();
        i64::from_be_bytes(bytes)
    }
}

#[test]
fn monument_interiors_match_vanilla() {
    let Some(reference) = Reference::from_env() else {
        eprintln!("Skipping monument interiors: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    let palette = palette();
    let mut cases = palette
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(" ")
        + "\n";
    let reduced = reference::reduced_sample();
    let mut covered = std::collections::BTreeSet::new();
    let mut oriented = std::collections::BTreeSet::new();
    // Individual child passes isolate geometry hidden by a later neighboring room or exterior clear.
    for seed in 0..16 {
        for direction in [
            Direction::South,
            Direction::West,
            Direction::North,
            Direction::East,
        ] {
            let start = monument([-33, 39, -47], direction);
            let layout = MonumentLayout::from_start(seed, &start).unwrap();
            for (i, piece) in layout.pieces.iter().enumerate() {
                covered.insert(format!("{:?}:{:?}", piece.kind, piece.design));
                let fresh =
                    oriented.insert(format!("{:?}:{:?}:{direction:?}", piece.kind, piece.design));
                if reduced && seed >= 4 && !fresh {
                    continue;
                }
                let b = piece.bounding_box;
                for mode in 0..=3 {
                    // Include the chunk just outside child bounds to check intersection gating.
                    for cz in (b.min[2] >> 4)..=(b.max[2] >> 4) + 1 {
                        for cx in (b.min[0] >> 4)..=(b.max[0] >> 4) + 1 {
                            cases.push_str(&format!(
                                "{seed} -33 39 -47 {} {mode} 63 -16 96 {cx} {cz} {i} {}\n",
                                direction.data_value(),
                                seed * 31 + 123
                            ));
                        }
                    }
                }
            }
        }
    }
    assert_eq!(
        covered.len(),
        14,
        "all eleven kinds, three simple designs and both wings: {covered:?}"
    );
    assert_eq!(oriented.len(), 56, "every design in every orientation");
    let mut configurations = vec![
        ([15, 39, 15], 0, 63),
        ([-33, 39, -47], 3, 63),
        ([3, 315, -5], 3, 63),
        ([3, 320, -5], 3, 63),
        ([3, -70, -5], 3, 63),
        ([3, 82, -5], 3, 95),
        ([3, 39, -5], 3, 20),
        ([29_999_872, 39, -29_999_872], 3, 63),
    ];
    if !reduced {
        for offset in 0..16 {
            configurations.push(([offset - 48, 39, 15 - offset], 3, 63));
        }
    }
    for (origin, mode, sea) in configurations {
        for seed in [0, 0x0123_4567_89ab_cdef_i64] {
            for direction in [
                Direction::South,
                Direction::West,
                Direction::North,
                Direction::East,
            ] {
                for cz in ((origin[2] - 5) >> 4)..=((origin[2] + 62) >> 4) {
                    for cx in ((origin[0] - 5) >> 4)..=((origin[0] + 62) >> 4) {
                        for selection in [-1, -2] {
                            cases.push_str(&format!("{seed} {} {} {} {} {mode} {sea} -64 384 {cx} {cz} {selection} {}\n",origin[0],origin[1],origin[2],direction.data_value(),seed.wrapping_add(123)));
                        }
                    }
                }
            }
        }
    }
    let inputs = reference.temp.0.join("cases.txt");
    let output = reference.temp.0.join("interiors.bin.zlib");
    fs::write(&inputs, &cases).unwrap();
    reference.compile(&[
        (
            "MonumentExteriorProbe.java",
            include_str!("MonumentExteriorProbe.java"),
        ),
        (
            "MonumentInteriorProbe.java",
            include_str!("MonumentInteriorProbe.java"),
        ),
    ]);
    reference.java(
        "MonumentInteriorProbe",
        [inputs.display().to_string(), output.display().to_string()],
    );
    let mut actual = Input(BufReader::new(ZlibDecoder::new(BufReader::new(
        File::open(output).unwrap(),
    ))));
    let data = WorldgenData::from_json_entries([(
        "data/minecraft/worldgen/structure/monument.json",
        r#"{"type":"minecraft:ocean_monument","biomes":[],"terrain_adaptation":"none"}"#,
    )])
    .unwrap();
    let mut failures = Vec::new();
    let mut reported = std::collections::BTreeSet::new();
    for case in cases.lines().skip(1) {
        let values: Vec<i64> = case
            .split_whitespace()
            .map(|v| v.parse().unwrap())
            .collect();
        let [
            seed,
            ox,
            oy,
            oz,
            dir,
            mode,
            sea,
            min_y,
            height,
            cx,
            cz,
            selection,
            decoration,
        ]: [i64; 13] = values.try_into().unwrap();
        let [ox, oy, oz, mode, sea, min_y, height, cx, cz] =
            [ox, oy, oz, mode, sea, min_y, height, cx, cz].map(|v| v as i32);
        let direction = [
            Direction::South,
            Direction::West,
            Direction::North,
            Direction::East,
        ][dir as usize];
        let mut start = monument([ox, oy, oz], direction);
        start.references = 7;
        start.pieces[0].save_data.insert("unknown", 123);
        let original = start.to_nbt();
        let mut chunk = GeneratedChunk::new(cx, cz, min_y, height, 1);
        for y in min_y..min_y + height {
            for z in 0..16 {
                for x in 0..16 {
                    chunk.set_block_state(
                        x,
                        y,
                        z,
                        terrain(
                            cx * 16 + x as i32,
                            y,
                            cz * 16 + z as i32,
                            oy,
                            mode,
                            sea,
                            &palette,
                        ),
                    );
                }
            }
        }
        for pass in 0..2 {
            if pass == 1 {
                start = StructureStart::from_nbt(
                    &start.to_nbt(),
                    &data,
                    &StructureTemplates::default(),
                )
                .unwrap()
                .unwrap();
            }
            let mut layout = MonumentLayout::from_start(seed, &start).unwrap();
            for piece in &mut layout.pieces {
                piece.bounding_box.min[1] += oy - 39;
                piece.bounding_box.max[1] += oy - 39;
            }
            let key = if selection >= 0 {
                format!(
                    "{:?}:{:?}",
                    layout.pieces[selection as usize].kind,
                    layout.pieces[selection as usize].design
                )
            } else {
                format!("whole:{selection}")
            };
            if selection >= 0 {
                layout.pieces = vec![layout.pieces[selection as usize].clone()];
            }
            let mut random = LegacyRandom::new(decoration + pass);
            if selection == -1 {
                start.place_monument_exterior_in_chunk(&mut chunk, sea);
            }
            layout.place_rooms_in_chunk(&mut chunk, sea, &mut random);
            let mut index = 0;
            let mut different = 0;
            let mut first = None;
            loop {
                let length = actual.int();
                if length == 0 {
                    break;
                }
                assert!(length > 0);
                let expected = actual.int() as u32;
                for _ in 0..length {
                    let (x, y, z) = (index % 16, min_y + index / 256, (index / 16) % 16);
                    let found = chunk.block_state(x as usize, y, z as usize);
                    if found != expected {
                        different += 1;
                        first.get_or_insert((x, y, z, expected, found));
                    }
                    index += 1;
                }
            }
            assert_eq!(index, height * 256);
            if let Some(first) = first
                && failures.len() < 30
                && reported.insert(key.clone())
            {
                failures.push(format!(
                    "{key}: {case}, pass {pass}: {different} blocks differ; first {first:?}"
                ));
            }
            for kind in HeightmapType::ALL {
                let mut differs = false;
                for z in 0..16 {
                    for x in 0..16 {
                        differs |= actual.int() != chunk.heightmaps().get(kind, x, z);
                    }
                }
                if differs && failures.len() < 30 && reported.insert(key.clone()) {
                    failures.push(format!("{key}: {case}, pass {pass}: {kind:?} differs"));
                }
            }
            let tail = actual.long();
            let found = random.next_long();
            if tail != found && failures.len() < 30 && reported.insert(format!("rng:{key}")) {
                failures.push(format!(
                    "{key}: {case}, pass {pass}: random tail {tail} != {found}"
                ));
            }
            assert_eq!(original, start.to_nbt());
            assert!(chunk.fluid_postprocessing().is_empty());
        }
    }
    assert_eq!(actual.0.read(&mut [0]).unwrap(), 0);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    eprintln!(
        "Compared {} interior and complete monument chunk placements and replays",
        (cases.lines().count() - 1) * 2
    );
}
