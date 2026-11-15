//! Jigsaw structure blocks compared with vanilla's `StructureStart.placeInChunk`, chunk by chunk.
//!
//! Real starts of the Overworld's jigsaw structures are found for a seed (only a plains village in the reduced
//! sample) and their chunks carved with terrain adaptation. Harpas places each chunk the start reaches with
//! `StructureStart::place_jigsaw_in_chunk`; vanilla places the same saved start on the same carved chunks. The
//! chunk's blocks and its block entities as vanilla loads and saves them, whose loot seeds follow the shared
//! random source, are compared.
mod reference;

use std::collections::{BTreeSet, HashMap};
use std::fs;

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_core::random::{WorldgenRandom, XoroshiroRandom};
use harpas_worldgen::{
    Beardifier, FeatureCompiler, FeatureWorld, GeneratedChunk, JigsawPiece,
    NoiseBasedChunkGenerator, PoolElementDef, RegionFeatureWorld, StructureStart, StructureState,
    StructureTemplates, WorldgenData, decoration_slot,
};
use reference::Reference;

/// Structure sets and the jigsaw structures to find in them.
const STRUCTURES: [(&str, &str); 9] = [
    ("minecraft:villages", "minecraft:village_plains"),
    ("minecraft:villages", "minecraft:village_desert"),
    ("minecraft:villages", "minecraft:village_savanna"),
    ("minecraft:villages", "minecraft:village_snowy"),
    ("minecraft:villages", "minecraft:village_taiga"),
    ("minecraft:pillager_outposts", "minecraft:pillager_outpost"),
    ("minecraft:trail_ruins", "minecraft:trail_ruins"),
    ("minecraft:trial_chambers", "minecraft:trial_chambers"),
    ("minecraft:ancient_cities", "minecraft:ancient_city"),
];

fn find_start(
    state: &StructureState,
    terrain: &NoiseBasedChunkGenerator,
    data: &WorldgenData,
    templates: &StructureTemplates,
    (set, structure): (&str, &str),
) -> StructureStart {
    for r in 0_i32..400 {
        for x in -r..=r {
            for z in -r..=r {
                if x.abs().max(z.abs()) != r || !state.is_structure_chunk(set, x, z) {
                    continue;
                }
                let starts = state
                    .starts_in_chunk(terrain, data, templates, x, z)
                    .unwrap();
                if let Some(start) = starts.into_iter().find(|s| s.structure == structure) {
                    return start;
                }
            }
        }
    }
    panic!("no {structure} start near the origin");
}

fn blocks(chunk: &GeneratedChunk) -> Vec<i32> {
    let mut blocks = Vec::with_capacity(16 * 384 * 16);
    for y in -64..320 {
        for z in 0..16 {
            for x in 0..16 {
                blocks.push(chunk.block_state(x, y, z) as i32);
            }
        }
    }
    blocks
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn jigsaw_chunks_match_vanilla() {
    let reference = Reference::required();
    let data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let templates = StructureTemplates::from_zip_path(&reference.server_jar).unwrap();
    let seed = 0;
    let terrain = NoiseBasedChunkGenerator::overworld(seed, &data).unwrap();
    let state = StructureState::new(seed, &data, &terrain).unwrap();
    let structures = if reference::reduced_sample() {
        &STRUCTURES[..1]
    } else {
        &STRUCTURES[..]
    };
    reference.compile(&[
        (
            "StructurePlacementProbe.java",
            include_str!("StructurePlacementProbe.java"),
        ),
        (
            "RuleProcessorProbe.java",
            include_str!("RuleProcessorProbe.java"),
        ),
        (
            "CappedProcessorProbe.java",
            include_str!("CappedProcessorProbe.java"),
        ),
        (
            "PoolElementProbe.java",
            include_str!("PoolElementProbe.java"),
        ),
        (
            "JigsawChunkProbe.java",
            include_str!("JigsawChunkProbe.java"),
        ),
    ]);
    let mut starts_cache: HashMap<(i32, i32), Vec<StructureStart>> = HashMap::new();
    let mut features = FeatureCompiler::new(&data);
    let mut skipped = 0;
    let mut failures = Vec::new();
    let mut compared = 0;
    let mut placing = 0;
    for &which in structures {
        let start = find_start(&state, &terrain, &data, &templates, which);
        let bounds = start.bounding_box;
        let mut targets = Vec::new();
        for x in bounds.min[0] >> 4..=bounds.max[0] >> 4 {
            for z in bounds.min[2] >> 4..=bounds.max[2] >> 4 {
                if start.pieces.iter().any(|piece| {
                    piece
                        .bounding_box
                        .intersects_columns(x * 16, z * 16, x * 16 + 15, z * 16 + 15)
                }) {
                    targets.push((x, z));
                }
            }
        }
        if reference::reduced_sample() {
            targets
                .retain(|&(x, z)| (x - start.chunk.0).abs() <= 1 && (z - start.chunk.1).abs() <= 1);
        }
        eprintln!(
            "{} at {:?}: {} pieces, {} chunks",
            start.structure,
            start.chunk,
            start.pieces.len(),
            targets.len()
        );
        // Carve every target and its neighbors, adapting the terrain to the starts around them.
        let mut carved: HashMap<(i32, i32), GeneratedChunk> = HashMap::new();
        for &(tx, tz) in &targets {
            for x in tx - 1..=tx + 1 {
                for z in tz - 1..=tz + 1 {
                    if carved.contains_key(&(x, z)) {
                        continue;
                    }
                    let mut nearby = Vec::new();
                    for nx in x - 8..=x + 8 {
                        for nz in z - 8..=z + 8 {
                            let starts = starts_cache.entry((nx, nz)).or_insert_with(|| {
                                state
                                    .starts_in_chunk(&terrain, &data, &templates, nx, nz)
                                    .unwrap()
                            });
                            nearby.extend(starts.iter().cloned());
                        }
                    }
                    let beardifier = Beardifier::for_starts(x, z, &data, &nearby).unwrap();
                    let chunk = terrain
                        .generate_chunk_carved_with_beardifier(x, z, &data, beardifier)
                        .unwrap();
                    carved.insert((x, z), chunk);
                }
            }
        }

        let mut input = Compound::new();
        input.insert("seed", seed);
        input.insert(
            "chunks",
            carved
                .iter()
                .map(|((x, z), chunk)| (format!("{x},{z}"), Nbt::IntArray(blocks(chunk))))
                .collect::<Compound>(),
        );
        let mut saved = Compound::new();
        saved.insert("start", start.to_nbt());
        input.insert("starts", saved);
        let cases: Vec<Compound> = targets
            .iter()
            .map(|&(x, z)| {
                let mut case = Compound::new();
                case.insert("name", format!("{x},{z}"));
                case.insert("start", "start");
                case.insert("x", x);
                case.insert("z", z);
                case
            })
            .collect();
        input.insert("cases", List::Compound(cases));
        let name = start.structure.replace(':', "-");
        let input_file = reference.temp.0.join(format!("{name}.nbt"));
        let mut bytes = Vec::new();
        Nbt::Compound(input).write_named("", &mut bytes);
        fs::write(&input_file, bytes).unwrap();
        let output = reference.temp.0.join(format!("{name}-out.nbt"));
        reference.java(
            "JigsawChunkProbe",
            [
                input_file.display().to_string(),
                output.display().to_string(),
            ],
        );
        let bytes = fs::read(output).unwrap();
        let (_, Nbt::Compound(results)) = Nbt::read_named(&mut bytes.as_slice()).unwrap() else {
            panic!("output compound")
        };

        let mut normalize = Compound::new();
        let mut pending = Vec::new();
        for &(x, z) in &targets {
            let label = format!("{} chunk {x},{z}", start.structure);
            // Harpas does not place features it has not ported yet, the ancient city's sculk patches.
            let unported = start.pieces.iter().any(|piece| {
                piece
                    .bounding_box
                    .intersects_columns(x * 16, z * 16, x * 16 + 15, z * 16 + 15)
                    && matches!(&piece.jigsaw, Some(JigsawPiece {
                        element: PoolElementDef::Feature { feature, .. }, ..
                    }) if features.placed(feature).unwrap().is_none())
            });
            if unported {
                skipped += 1;
                continue;
            }
            let Some(Nbt::Compound(vanilla)) = results.get(&format!("{x},{z}")) else {
                panic!("no result for {label}")
            };
            // The chunk's decoration region: the target and its eight carved neighbors.
            let region = (x - 1..=x + 1)
                .flat_map(|cx| (z - 1..=z + 1).map(move |cz| (cx, cz)))
                .map(|key| carved[&key].clone());
            let mut world = RegionFeatureWorld::with_stored_biomes(region, seed).unwrap();
            world.start_region(seed, x, z);
            let (index, step) = decoration_slot(&data, &start.structure).unwrap();
            let mut random = WorldgenRandom::new(XoroshiroRandom::new(0));
            let decoration = random.set_decoration_seed(seed, x * 16, z * 16);
            random.set_feature_seed(decoration, index, step);
            start
                .place_jigsaw_in_region(
                    &mut world,
                    x,
                    z,
                    seed,
                    &data,
                    &templates,
                    &mut features,
                    &mut random,
                )
                .unwrap();
            let mut neighbors = BTreeSet::new();
            let mut target = None;
            for chunk in world.into_chunks() {
                let key = (chunk.chunk_x(), chunk.chunk_z());
                if key == (x, z) {
                    target = Some(chunk);
                    continue;
                }
                let carved_blocks = blocks(&carved[&key]);
                for (i, block) in blocks(&chunk).into_iter().enumerate() {
                    if block != carved_blocks[i] {
                        let i = i as i32;
                        neighbors.insert([
                            key.0 * 16 + i % 16,
                            i / 256 - 64,
                            key.1 * 16 + i / 16 % 16,
                            block,
                        ]);
                    }
                }
            }
            let mut chunk = target.unwrap();
            // Blocks placed without data hold vanilla's placeholder until their block entity is created.
            let entities: Vec<Compound> = chunk
                .take_block_entities()
                .into_iter()
                .filter(|entity| entity.get("id") != Some(&Nbt::from("DUMMY")))
                .collect();
            compared += 1;
            let Some(Nbt::IntArray(written)) = vanilla.get("neighbors") else {
                panic!("no neighbor blocks")
            };
            let expected_neighbors: BTreeSet<[i32; 4]> = written
                .chunks(4)
                .map(|b| [b[0], b[1], b[2], b[3]])
                .collect();
            if expected_neighbors != neighbors {
                let only = |a: &BTreeSet<[i32; 4]>, b: &BTreeSet<[i32; 4]>| {
                    a.difference(b).take(3).copied().collect::<Vec<_>>()
                };
                failures.push(format!(
                    "{label}: blocks written to neighbors differ, vanilla only {:?}, Harpas only {:?}",
                    only(&expected_neighbors, &neighbors),
                    only(&neighbors, &expected_neighbors)
                ));
                continue;
            }
            let Some(Nbt::IntArray(expected)) = vanilla.get("blocks") else {
                panic!("no blocks")
            };
            let before = blocks(&carved[&(x, z)]);
            placing += usize::from(*expected != before);
            let actual = blocks(&chunk);
            let differing: Vec<usize> = (0..expected.len())
                .filter(|&i| expected[i] != actual[i])
                .collect();
            if let Some(&first) = differing.first() {
                let describe =
                    |id: i32| format!("{:?}", block_states().properties(id as u32).unwrap());
                failures.push(format!(
                    "{label}: {} blocks differ, first at local {} {} {}: vanilla {}, Harpas {}, carved {}",
                    differing.len(),
                    first % 16,
                    (first / 256) as i32 - 64,
                    first / 16 % 16,
                    describe(expected[first]),
                    describe(actual[first]),
                    describe(before[first])
                ));
                continue;
            }
            let vanilla_entities = match vanilla.get("entities") {
                Some(Nbt::List(List::Compound(list))) => list.clone(),
                _ => Vec::new(),
            };
            let coordinate = |entity: &Compound, key: &str| match entity.get(key) {
                Some(Nbt::Int(value)) => *value,
                _ => panic!("block entity without {key}"),
            };
            let mut pairs: Vec<Compound> = entities
                .into_iter()
                .map(|entity| {
                    let [ex, ey, ez] = ["x", "y", "z"].map(|key| coordinate(&entity, key));
                    let mut pair = Compound::new();
                    pair.insert(
                        "state",
                        chunk.block_state((ex & 15) as usize, ey, (ez & 15) as usize) as i32,
                    );
                    pair.insert("entity", entity);
                    pair
                })
                .collect();
            pairs.sort_by_key(|pair| {
                let Some(Nbt::Compound(entity)) = pair.get("entity") else {
                    unreachable!()
                };
                (
                    coordinate(entity, "y"),
                    coordinate(entity, "z"),
                    coordinate(entity, "x"),
                )
            });
            let list = if pairs.is_empty() {
                List::Empty
            } else {
                List::Compound(pairs)
            };
            normalize.insert(format!("{x},{z}"), list);
            pending.push((format!("{x},{z}"), label, vanilla_entities));
        }
        let normalize_input = reference.temp.0.join(format!("{name}-entities.nbt"));
        let mut bytes = Vec::new();
        Nbt::Compound(normalize).write_named("", &mut bytes);
        fs::write(&normalize_input, bytes).unwrap();
        let normalize_output = reference.temp.0.join(format!("{name}-entities-out.nbt"));
        reference.java(
            "PoolElementProbe",
            [
                "normalize".to_string(),
                normalize_input.display().to_string(),
                normalize_output.display().to_string(),
            ],
        );
        let bytes = fs::read(normalize_output).unwrap();
        let (_, Nbt::Compound(normalized)) = Nbt::read_named(&mut bytes.as_slice()).unwrap() else {
            panic!("normalized compound")
        };
        for (key, label, vanilla_entities) in pending {
            let harpas = match normalized.get(&key) {
                Some(Nbt::List(List::Compound(list))) => list.clone(),
                _ => Vec::new(),
            };
            if harpas != vanilla_entities {
                failures.push(format!(
                    "{label}: block entities differ\nvanilla {vanilla_entities:?}\nHarpas  {harpas:?}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {compared} chunks differ from vanilla:\n{}",
        failures.len(),
        failures
            .iter()
            .take(25)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    eprintln!(
        "{compared} jigsaw chunks match vanilla, {placing} of them with structure blocks; {skipped} skipped \
         for features not ported yet"
    );
}
