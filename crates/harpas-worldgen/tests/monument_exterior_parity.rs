//! Exterior-only monument placement compared block by block with vanilla, including pass replays.
mod reference;

use std::fs::{self, File};
use std::io::{BufReader, Read};

use harpas_core::block::block_states;
use harpas_core::random::{LegacyRandom, RandomSource};
use harpas_worldgen::{
    BoundingBox, Direction, GeneratedChunk, HeightmapType, StructurePiece, StructureStart,
    StructureTemplates, WorldgenData,
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

struct Input(BufReader<File>);
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
fn foundations_reach_support_but_leave_the_entrance_open() {
    let palette = palette();
    let bricks = block_states()
        .default_state("minecraft:prismarine_bricks")
        .unwrap();
    let start = monument([0, 39, 0], Direction::South);
    for cx in [0, 1] {
        let mut chunk = GeneratedChunk::new(cx, 0, -64, 384, 1);
        for x in 0..16 {
            for z in 0..16 {
                chunk.set_block_state(x, 18, z, palette[1]);
                for y in 19..39 {
                    chunk.set_block_state(x, y, z, palette[2]);
                }
            }
        }
        start.place_monument_exterior_in_chunk(&mut chunk, 63);
        if cx == 0 {
            assert_eq!(chunk.block_state(0, 18, 0), palette[1]);
            for y in 19..=39 {
                assert_eq!(chunk.block_state(0, y, 0), bricks);
            }
        } else {
            // There is no foundation grid cell in front of the entrance at local X=27.
            assert_eq!(chunk.block_state(11, 18, 0), palette[1]);
            for y in 19..=39 {
                assert_eq!(chunk.block_state(11, y, 0), palette[2]);
            }
        }
    }
}

#[test]
fn clearing_reaches_the_halo_and_keeps_ice_and_existing_water() {
    let palette = palette();
    let start = monument([15, 39, 15], Direction::South);
    let mut chunk = GeneratedChunk::new(0, 0, -64, 384, 1);
    // World X=10 is the outermost clearing ring; X=9 lies outside it.
    chunk.set_block_state(9, 50, 15, palette[1]);
    chunk.set_block_state(10, 50, 15, palette[1]);
    chunk.set_block_state(10, 51, 15, palette[4]);
    chunk.set_block_state(15, 64, 15, palette[2]);
    let original = start.to_nbt();
    start.place_monument_exterior_in_chunk(&mut chunk, 63);
    assert_eq!(chunk.block_state(9, 50, 15), palette[1]);
    assert_eq!(chunk.block_state(10, 50, 15), palette[2]);
    assert_eq!(chunk.block_state(10, 51, 15), palette[4]);
    assert_eq!(chunk.block_state(15, 64, 15), palette[2]);
    assert_eq!(start.to_nbt(), original);
}

#[test]
fn monument_exteriors_match_vanilla() {
    let Some(reference) = Reference::from_env() else {
        eprintln!("Skipping monument exterior: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    let palette = palette();
    let mut cases = palette
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(" ")
        + "\n";
    let mut configurations = Vec::new();
    for origin in [[15, 39, 15], [-33, 39, -47]] {
        if reference::reduced_sample() && origin[0] == -33 {
            continue;
        }
        for mode in 0..5 {
            configurations.push((origin, mode, 63));
        }
    }
    for (origin, sea) in [
        ([29_999_872, 39, -29_999_872], 63),
        ([3, -70, -5], 63),
        ([3, 315, -5], 63),
        ([3, 320, -5], 63),
        ([3, 82, -5], 95),
        ([3, 39, -5], 20),
    ] {
        configurations.push((origin, 3, sea));
    }
    if !reference::reduced_sample() {
        // Shift each edge through all sixteen positions within a chunk, using negative coordinates.
        for offset in 0..16 {
            configurations.push(([offset - 48, 39, 15 - offset], 3, 63));
        }
    }
    for (origin, mode, sea) in configurations {
        for direction in [
            Direction::South,
            Direction::West,
            Direction::North,
            Direction::East,
        ] {
            // Include the clearing halo and one unaffected chunk beyond each side.
            for cz in ((origin[2] - 5) >> 4) - 1..=((origin[2] + 62) >> 4) + 1 {
                for cx in ((origin[0] - 5) >> 4) - 1..=((origin[0] + 62) >> 4) + 1 {
                    cases.push_str(&format!(
                        "{} {} {} {} {mode} {sea} -64 384 {cx} {cz}\n",
                        origin[0],
                        origin[1],
                        origin[2],
                        direction.data_value()
                    ));
                }
            }
        }
    }
    let inputs = reference.temp.0.join("cases.txt");
    let output = reference.temp.0.join("exteriors.bin");
    fs::write(&inputs, &cases).unwrap();
    reference.compile(&[(
        "MonumentExteriorProbe.java",
        include_str!("MonumentExteriorProbe.java"),
    )]);
    reference.java(
        "MonumentExteriorProbe",
        [inputs.display().to_string(), output.display().to_string()],
    );
    let mut actual = Input(BufReader::new(File::open(output).unwrap()));
    let mut failures = Vec::new();
    let mut reported = std::collections::BTreeSet::new();
    let data = WorldgenData::from_json_entries([(
        "data/minecraft/worldgen/structure/monument.json",
        r#"{"type":"minecraft:ocean_monument","biomes":[],"terrain_adaptation":"none"}"#,
    )])
    .unwrap();
    let templates = StructureTemplates::default();
    for case in cases.lines().skip(1) {
        let v: Vec<i32> = case
            .split_whitespace()
            .map(|s| s.parse().unwrap())
            .collect();
        let [ox, oy, oz, d, mode, sea, min_y, height, cx, cz]: [i32; 10] = v.try_into().unwrap();
        let direction = [
            Direction::South,
            Direction::West,
            Direction::North,
            Direction::East,
        ][d as usize];
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
                start = StructureStart::from_nbt(&start.to_nbt(), &data, &templates)
                    .unwrap()
                    .unwrap();
            }
            start.place_monument_exterior_in_chunk(&mut chunk, sea);
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
            let key = (ox, oy, oz, d, mode, sea);
            if let Some(first) = first
                && failures.len() < 20
                && reported.insert(key)
            {
                failures.push(format!(
                    "{case}, pass {pass}: {different} blocks differ; first {first:?}"
                ));
            }
            for kind in HeightmapType::ALL {
                let mut differs = false;
                for z in 0..16 {
                    for x in 0..16 {
                        differs |= actual.int() != chunk.heightmaps().get(kind, x, z);
                    }
                }
                if differs && failures.len() < 20 && reported.insert(key) {
                    failures.push(format!("{case}, pass {pass}: {kind:?} differs"));
                }
            }
            assert_eq!(
                actual.long(),
                LegacyRandom::new(123).next_long(),
                "exterior consumes no random draws"
            );
            assert_eq!(original, start.to_nbt(), "exterior retains saved records");
            assert!(chunk.fluid_postprocessing().is_empty());
        }
    }
    assert_eq!(
        actual.0.read(&mut [0]).unwrap(),
        0,
        "extra reference output"
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    eprintln!(
        "Compared {} exterior chunk placements and replays",
        (cases.lines().count() - 1) * 2
    );
}
