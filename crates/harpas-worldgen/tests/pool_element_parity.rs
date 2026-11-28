//! Template pool elements compared with vanilla's `StructurePoolElement.place` in one chunk of a generation region.
//!
//! Every single, legacy single and list element of every template pool (every eighth single element in the reduced
//! sample) is turned by one of the four rotations over dry, flooded, deep and fence terrain, every fifth ignoring
//! waterlogging. Every feature element whose feature is ported is placed on grass with several seeds. The blocks of
//! the chunk and of its neighbors, the block entities and the placement random tail are compared.
mod reference;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::sync::Arc;

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_core::random::{RandomSource, WorldgenRandom, XoroshiroRandom};
use harpas_worldgen::processor::ProcessorWorld;
use harpas_worldgen::{
    CompiledPlaced, FeatureCompiler, GeneratedChunk, HeightmapType, PoolElementDef,
    RegionFeatureWorld, Rotation, SingleElementPlacement, StructureTemplate, StructureTemplates,
    WorldgenData, place_placed,
};
use reference::Reference;

const CHUNK: (i32, i32) = (-1, 2);
/// The terrain feature elements stand on.
const GRASS: usize = 4;

fn state(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}

/// Layered terrain as [top Y, state] pairs from the world bottom, with air above.
fn terrains() -> [Vec<i32>; 5] {
    let id = |name| state(name) as i32;
    [
        vec![64, id("stone")],
        vec![50, id("stone"), 64, id("water")],
        vec![-60, id("deepslate")],
        // Fences would connect to the template if its edges updated their neighbors' shapes; vanilla's do not.
        vec![-60, id("deepslate"), 320, id("oak_fence")],
        vec![-60, id("deepslate"), 63, id("dirt"), 64, id("grass_block")],
    ]
}

fn terrain(layers: &[i32], y: i32) -> u32 {
    layers
        .chunks(2)
        .find(|layer| y < layer[0])
        .map_or(state("air"), |layer| layer[1] as u32)
}

struct Layers<'a>(&'a [i32]);

impl ProcessorWorld for Layers<'_> {
    fn block_at(&mut self, [_, y, _]: [i32; 3]) -> u32 {
        if (-64..320).contains(&y) {
            terrain(self.0, y)
        } else {
            state("void_air")
        }
    }

    fn heightmap(&mut self, kind: HeightmapType, _: i32, _: i32) -> i32 {
        (-64..320)
            .rev()
            .find(|&y| kind.matches(terrain(self.0, y)))
            .map_or(-64, |y| y + 1)
    }
}

fn layered_chunk(chunk_x: i32, chunk_z: i32, layers: &[i32]) -> GeneratedChunk {
    let mut chunk = GeneratedChunk::new(chunk_x, chunk_z, -64, 384, 1);
    for y in -64..320 {
        let block = terrain(layers, y);
        for z in 0..16 {
            for x in 0..16 {
                chunk.set_block_state(x, y, z, block);
            }
        }
    }
    chunk.recompute_heightmaps();
    chunk
}

struct Case {
    pool: String,
    index: usize,
    element: PoolElementDef,
    feature: Option<Arc<CompiledPlaced>>,
    position: [i32; 3],
    rotation: Rotation,
    terrain: usize,
    seed: i64,
    dry: bool,
}

impl Case {
    fn label(&self) -> String {
        format!(
            "{}[{}] {:?} at {:?} terrain {} seed {} dry {}",
            self.pool, self.index, self.rotation, self.position, self.terrain, self.seed, self.dry
        )
    }
}

fn cases(data: &WorldgenData, templates: &StructureTemplates) -> (Vec<Case>, BTreeSet<String>) {
    let step = if reference::reduced_sample() { 8 } else { 1 };
    let seeds = if reference::reduced_sample() { 2 } else { 4 };
    let mut compiler = FeatureCompiler::new(data);
    let mut unported = BTreeSet::new();
    let mut cases = Vec::new();
    let mut number = 0;
    for (pool, definition) in &data.template_pools {
        for (index, (element, _)) in definition.elements.iter().enumerate() {
            let location = match element {
                PoolElementDef::Single { location, .. } => location,
                PoolElementDef::List { elements, .. } => match &elements[0] {
                    PoolElementDef::Single { location, .. } => location,
                    _ => panic!("vanilla lists hold single elements"),
                },
                PoolElementDef::Feature { feature, .. } => {
                    let Some(compiled) = compiler.placed(feature).unwrap() else {
                        unported.insert(feature.clone());
                        continue;
                    };
                    for seed in 0..seeds {
                        number += 1;
                        cases.push(Case {
                            pool: pool.clone(),
                            index,
                            element: element.clone(),
                            feature: Some(compiled.clone()),
                            position: [
                                CHUNK.0 * 16 + 3 + number % 10,
                                64,
                                CHUNK.1 * 16 + 3 + number * 3 % 10,
                            ],
                            rotation: Rotation::None,
                            terrain: GRASS,
                            seed: i64::from(number) * 0x2545_f491 + seed,
                            dry: false,
                        });
                    }
                    continue;
                }
                PoolElementDef::Empty => continue,
            };
            number += 1;
            let list = matches!(element, PoolElementDef::List { .. });
            if number % step != 0 && !list {
                continue;
            }
            let turn = if list { number } else { number / step };
            let rotation = Rotation::ALL[(turn % 4) as usize];
            // Center the turned template on a point of the chunk, so that it crosses the chunk's edges.
            // A missing template is empty, as vanilla makes it; the element then places nothing.
            let template = templates
                .get(location)
                .cloned()
                .unwrap_or_else(StructureTemplate::empty);
            let bounds = template.bounding_box([0; 3], rotation);
            let target = [
                CHUNK.0 * 16 + number % 16,
                40 + number % 50,
                CHUNK.1 * 16 + number * 7 % 16,
            ];
            cases.push(Case {
                pool: pool.clone(),
                index,
                element: element.clone(),
                feature: None,
                position: [0, 1, 2].map(|i| target[i] - (bounds.min[i] + bounds.max[i]) / 2),
                rotation,
                terrain: turn as usize % GRASS,
                seed: i64::from(number) * 0x9e37_79b9,
                dry: turn % 5 == 4,
            });
        }
    }
    (cases, unported)
}

/// What a placement left: the target chunk's blocks that differ from the terrain by index, the blocks written to
/// its neighbors, its block entities and its states, and the placement random's next long.
struct Placed {
    changes: BTreeMap<i32, u32>,
    outside: BTreeSet<[i32; 4]>,
    entities: Vec<(u32, Compound)>,
    tail: i64,
}

fn chunk_changes(chunk: &GeneratedChunk, layers: &[i32]) -> BTreeMap<i32, u32> {
    let mut changes = BTreeMap::new();
    for y in -64..320 {
        for z in 0..16 {
            for x in 0..16 {
                let block = chunk.block_state(x, y, z);
                if block != terrain(layers, y) {
                    changes.insert(((y + 64) * 16 + z as i32) * 16 + x as i32, block);
                }
            }
        }
    }
    changes
}

fn coordinate(entity: &Compound, key: &str) -> i32 {
    match entity.get(key) {
        Some(Nbt::Int(value)) => *value,
        _ => panic!("block entity without {key}"),
    }
}

fn place(
    case: &Case,
    data: &WorldgenData,
    templates: &StructureTemplates,
    layers: &[i32],
) -> Placed {
    let mut random = WorldgenRandom::new(XoroshiroRandom::new(case.seed));
    let (chunk, outside, entities) = if let Some(feature) = &case.feature {
        let chunks = (-1..=1)
            .flat_map(|dx| (-1..=1).map(move |dz| (CHUNK.0 + dx, CHUNK.1 + dz)))
            .map(|(x, z)| layered_chunk(x, z, layers));
        let mut world = RegionFeatureWorld::new(chunks, 0).unwrap();
        place_placed(&mut world, feature, &mut random, case.position);
        let mut outside = BTreeSet::new();
        let mut target = None;
        for chunk in world.into_chunks() {
            if (chunk.chunk_x(), chunk.chunk_z()) == CHUNK {
                target = Some(chunk);
                continue;
            }
            for (index, block) in chunk_changes(&chunk, layers) {
                let (x, z, y) = (index & 15, index >> 4 & 15, (index >> 8) - 64);
                outside.insert([
                    chunk.chunk_x() * 16 + x,
                    y,
                    chunk.chunk_z() * 16 + z,
                    block as i32,
                ]);
            }
        }
        let mut chunk = target.unwrap();
        // A generation region saves a `DUMMY` placeholder for a block entity a feature's block needs, such as a
        // sculk sensor's. The probe's world creates block entities only when asked, so it has none to compare;
        // feature_blocks_parity compares the placeholders.
        let entities: Vec<_> = chunk
            .take_block_entities()
            .into_iter()
            .filter(|entity| entity.get("id") != Some(&Nbt::from("DUMMY")))
            .collect();
        (chunk, outside, entities)
    } else {
        let mut chunk = layered_chunk(CHUNK.0, CHUNK.1, layers);
        let entities = SingleElementPlacement {
            data,
            templates,
            seed: case.seed,
            position: case.position,
            pivot: pivot(case.position),
            rotation: case.rotation,
            ignore_waterlogging: case.dry,
        }
        .place(&case.element, &mut chunk, &mut Layers(layers), &mut random)
        .unwrap();
        (chunk, BTreeSet::new(), entities)
    };
    // Vanilla keeps block entities by position; compare them in Y, Z, X order.
    let mut entities: Vec<(u32, Compound)> = entities
        .into_iter()
        .map(|entity| {
            let [x, y, z] = ["x", "y", "z"].map(|key| coordinate(&entity, key));
            (
                chunk.block_state((x & 15) as usize, y, (z & 15) as usize),
                entity,
            )
        })
        .collect();
    entities.sort_by_key(|(_, e)| (coordinate(e, "y"), coordinate(e, "z"), coordinate(e, "x")));
    Placed {
        changes: chunk_changes(&chunk, layers),
        outside,
        entities,
        tail: random.next_long(),
    }
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn pool_elements_place_as_in_vanilla() {
    let reference = Reference::required();
    let data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let templates = StructureTemplates::from_zip_path(&reference.server_jar).unwrap();
    let terrains = terrains();
    let (cases, unported) = cases(&data, &templates);
    let mut input = Compound::new();
    for (number, case) in cases.iter().enumerate() {
        let mut tag = Compound::new();
        tag.insert("pool", case.pool.as_str());
        tag.insert("index", case.index as i32);
        for (key, value) in ["x", "y", "z"].into_iter().zip(case.position) {
            tag.insert(key, value);
        }
        for (key, value) in ["px", "py", "pz"].into_iter().zip(pivot(case.position)) {
            tag.insert(key, value);
        }
        tag.insert("rotation", case.rotation as i32);
        tag.insert("cx", CHUNK.0);
        tag.insert("cz", CHUNK.1);
        tag.insert("seed", case.seed);
        tag.insert("layers", Nbt::IntArray(terrains[case.terrain].clone()));
        tag.insert("ignore_waterlogging", i8::from(case.dry));
        input.insert(number.to_string(), tag);
    }
    let input_file = reference.temp.0.join("elements.nbt");
    let mut bytes = Vec::new();
    Nbt::Compound(input).write_named("", &mut bytes).unwrap();
    fs::write(&input_file, bytes).unwrap();
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
    ]);
    let output = reference.temp.0.join("elements-out.nbt");
    reference.java(
        "PoolElementProbe",
        [
            input_file.display().to_string(),
            output.display().to_string(),
        ],
    );
    let bytes = fs::read(output).unwrap();
    let (_, Nbt::Compound(results)) = Nbt::read_named(&mut bytes.as_slice()).unwrap() else {
        panic!("output compound")
    };

    let mut failures = Vec::new();
    let mut placing = 0;
    let mut normalize = Compound::new();
    let mut pending = Vec::new();
    for (number, case) in cases.iter().enumerate() {
        let Some(Nbt::Compound(vanilla)) = results.get(&number.to_string()) else {
            panic!("no result for case {number}")
        };
        let layers = &terrains[case.terrain];
        let placed = place(case, &data, &templates, layers);
        let label = case.label();
        let Some(Nbt::IntArray(pairs)) = vanilla.get("changes") else {
            panic!("no changes")
        };
        let expected: BTreeMap<i32, u32> = pairs
            .chunks(2)
            .map(|pair| (pair[0], pair[1] as u32))
            .collect();
        placing += usize::from(!expected.is_empty());
        if let Some(difference) = first_difference(&expected, &placed.changes) {
            let differing = expected
                .keys()
                .chain(placed.changes.keys())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .filter(|key| expected.get(key) != placed.changes.get(key))
                .count();
            failures.push(format!(
                "{label}: {differing} of {} blocks differ, first {difference}",
                expected.len()
            ));
            continue;
        }
        let Some(Nbt::IntArray(written)) = vanilla.get("outside") else {
            panic!("no outside blocks")
        };
        let expected_outside: BTreeSet<[i32; 4]> = written
            .chunks(4)
            .map(|block| [block[0], block[1], block[2], block[3]])
            .collect();
        if expected_outside != placed.outside {
            let only = |a: &BTreeSet<[i32; 4]>, b: &BTreeSet<[i32; 4]>| {
                a.difference(b).take(3).copied().collect::<Vec<_>>()
            };
            failures.push(format!(
                "{label}: blocks in neighboring chunks differ, vanilla only {:?}, Harpas only {:?}",
                only(&expected_outside, &placed.outside),
                only(&placed.outside, &expected_outside)
            ));
            continue;
        }
        if vanilla.get("random_tail") != Some(&Nbt::Long(placed.tail)) {
            failures.push(format!("{label}: placement random tail differs"));
            continue;
        }
        let vanilla_entities = match vanilla.get("entities") {
            Some(Nbt::List(List::Compound(list))) => list.clone(),
            _ => Vec::new(),
        };
        let pairs: Vec<Compound> = placed
            .entities
            .into_iter()
            .map(|(state, entity)| {
                let mut pair = Compound::new();
                pair.insert("state", state as i32);
                pair.insert("entity", entity);
                pair
            })
            .collect();
        let list = if pairs.is_empty() {
            List::Empty
        } else {
            List::Compound(pairs)
        };
        normalize.insert(number.to_string(), list);
        pending.push((number, label, vanilla_entities));
    }

    // Vanilla saves every field of a loaded block entity, and leaves out those equal to their defaults. Let it
    // load and save Harpas's block entities too, so that both are compared in the same form.
    let normalize_input = reference.temp.0.join("entities.nbt");
    let mut bytes = Vec::new();
    Nbt::Compound(normalize)
        .write_named("", &mut bytes)
        .unwrap();
    fs::write(&normalize_input, bytes).unwrap();
    let normalize_output = reference.temp.0.join("entities-out.nbt");
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
    for (number, label, vanilla_entities) in pending {
        let harpas = match normalized.get(&number.to_string()) {
            Some(Nbt::List(List::Compound(list))) => list.clone(),
            _ => Vec::new(),
        };
        if harpas != vanilla_entities {
            failures.push(format!(
                "{label}: block entities differ\nvanilla {vanilla_entities:?}\nHarpas  {harpas:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} elements differ from vanilla:\n{}",
        failures.len(),
        cases.len(),
        failures
            .iter()
            .take(25)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    let count =
        |kind: fn(&PoolElementDef) -> bool| cases.iter().filter(|c| kind(&c.element)).count();
    eprintln!(
        "{} pool element placements match vanilla ({} single, {} list, {} feature), {placing} placing blocks in \
         the chunk; feature elements not ported yet: {unported:?}",
        cases.len(),
        count(|e| matches!(e, PoolElementDef::Single { .. })),
        count(|e| matches!(e, PoolElementDef::List { .. })),
        count(|e| matches!(e, PoolElementDef::Feature { .. })),
    );
}

fn pivot([x, y, z]: [i32; 3]) -> [i32; 3] {
    [x + 3, y, z - 5]
}

fn first_difference(expected: &BTreeMap<i32, u32>, actual: &BTreeMap<i32, u32>) -> Option<String> {
    let describe = |state: Option<&u32>| {
        state.map_or("terrain".to_string(), |&state| {
            format!("{:?}", block_states().properties(state).unwrap())
        })
    };
    let index = expected
        .keys()
        .chain(actual.keys())
        .copied()
        .filter(|key| expected.get(key) != actual.get(key))
        .min()?;
    let (x, z, y) = (index & 15, index >> 4 & 15, (index >> 8) - 64);
    Some(format!(
        "at local {x} {y} {z}: vanilla {}, Harpas {}",
        describe(expected.get(&index)),
        describe(actual.get(&index))
    ))
}
