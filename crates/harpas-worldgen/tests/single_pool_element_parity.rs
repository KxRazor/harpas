//! Single and legacy single pool elements compared with vanilla's `SinglePoolElement.place` in one chunk: every
//! such element of every template pool (every eighth in the reduced sample), each turned by one of the four
//! rotations, over dry, flooded and deep terrain, every fifth ignoring waterlogging, with its blocks, block
//! entities and placement random tail.
mod reference;

use std::collections::BTreeMap;
use std::fs;

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_core::random::{RandomSource, XoroshiroRandom};
use harpas_worldgen::processor::ProcessorWorld;
use harpas_worldgen::{
    GeneratedChunk, HeightmapType, PoolElementDef, Rotation, SingleElementPlacement,
    StructureTemplate, StructureTemplates, WorldgenData,
};
use reference::Reference;

const CHUNK: (i32, i32) = (-1, 2);

fn state(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}

/// Layered terrain as [top Y, state] pairs from the world bottom, with air above.
fn terrains() -> [Vec<i32>; 3] {
    let id = |name| state(name) as i32;
    [
        vec![64, id("stone")],
        vec![50, id("stone"), 64, id("water")],
        vec![-60, id("deepslate")],
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

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn single_pool_elements_place_as_in_vanilla() {
    let reference = Reference::required();
    let data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let templates = StructureTemplates::from_zip_path(&reference.server_jar).unwrap();
    let step = if reference::reduced_sample() { 8 } else { 1 };
    let terrains = terrains();
    let mut cases = Vec::new();
    for (pool_id, pool) in &data.template_pools {
        for (index, (element, _)) in pool.elements.iter().enumerate() {
            let PoolElementDef::Single { location, .. } = element else {
                continue;
            };
            let number = cases.len() as i32;
            if number % step != 0 {
                cases.push(None);
                continue;
            }
            let rotation = Rotation::ALL[(number / step % 4) as usize];
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
            let position = [0, 1, 2].map(|i| target[i] - (bounds.min[i] + bounds.max[i]) / 2);
            cases.push(Some((
                pool_id.clone(),
                index as i32,
                element.clone(),
                position,
                rotation,
                (number / step) as usize % terrains.len(),
                i64::from(number) * 0x9e37_79b9,
                number / step % 5 == 4,
            )));
        }
    }
    let cases: Vec<_> = cases.into_iter().flatten().collect();
    let mut input = Compound::new();
    for (case, (pool, index, _, position, rotation, terrain, seed, dry)) in cases.iter().enumerate()
    {
        let mut tag = Compound::new();
        tag.insert("pool", pool.as_str());
        tag.insert("index", *index);
        for (key, value) in ["x", "y", "z"].into_iter().zip(position) {
            tag.insert(key, *value);
        }
        for (key, value) in ["px", "py", "pz"].into_iter().zip(pivot(*position)) {
            tag.insert(key, value);
        }
        tag.insert("rotation", *rotation as i32);
        tag.insert("cx", CHUNK.0);
        tag.insert("cz", CHUNK.1);
        tag.insert("seed", *seed);
        tag.insert("layers", Nbt::IntArray(terrains[*terrain].clone()));
        tag.insert("ignore_waterlogging", i8::from(*dry));
        input.insert(case.to_string(), tag);
    }
    let input_file = reference.temp.0.join("elements.nbt");
    let mut bytes = Vec::new();
    Nbt::Compound(input).write_named("", &mut bytes);
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
            "SinglePoolElementProbe.java",
            include_str!("SinglePoolElementProbe.java"),
        ),
    ]);
    let output = reference.temp.0.join("elements-out.nbt");
    reference.java(
        "SinglePoolElementProbe",
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
    let mut placed = 0;
    let mut normalize = Compound::new();
    let mut pending = Vec::new();
    for (case, (pool, index, element, position, rotation, kind, seed, dry)) in
        cases.iter().enumerate()
    {
        let Some(Nbt::Compound(vanilla)) = results.get(&case.to_string()) else {
            panic!("no result for case {case}")
        };
        let layers = &terrains[*kind];
        let mut chunk = GeneratedChunk::new(CHUNK.0, CHUNK.1, -64, 384, 1);
        for y in -64..320 {
            let block = terrain(layers, y);
            for z in 0..16 {
                for x in 0..16 {
                    chunk.set_block_state(x, y, z, block);
                }
            }
        }
        let mut random = XoroshiroRandom::new(*seed);
        let mut entities = SingleElementPlacement {
            data: &data,
            templates: &templates,
            seed: *seed,
            position: *position,
            pivot: pivot(*position),
            rotation: *rotation,
            ignore_waterlogging: *dry,
        }
        .place(element, &mut chunk, &mut Layers(layers), &mut random)
        .unwrap();
        // Vanilla keeps block entities by position; compare them in Y, Z, X order.
        let coordinate = |entity: &Compound, key| match entity.get(key) {
            Some(Nbt::Int(value)) => *value,
            _ => panic!("block entity without {key}"),
        };
        entities.sort_by_key(|e| (coordinate(e, "y"), coordinate(e, "z"), coordinate(e, "x")));
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
        let Some(Nbt::IntArray(pairs)) = vanilla.get("changes") else {
            panic!("no changes")
        };
        let expected: BTreeMap<i32, u32> = pairs
            .chunks(2)
            .map(|pair| (pair[0], pair[1] as u32))
            .collect();
        let label =
            format!("{pool}[{index}] {rotation:?} at {position:?} terrain {kind} dry {dry}");
        placed += usize::from(!expected.is_empty());
        if let Some(difference) = first_difference(&expected, &changes) {
            failures.push(format!(
                "{label}: {} of {} blocks differ, first {difference}",
                expected
                    .keys()
                    .chain(changes.keys())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .filter(|key| expected.get(key) != changes.get(key))
                    .count(),
                expected.len()
            ));
            continue;
        }
        if vanilla.get("random_tail") != Some(&Nbt::Long(random.next_long())) {
            failures.push(format!("{label}: placement random tail differs"));
            continue;
        }
        let vanilla_entities = match vanilla.get("entities") {
            Some(Nbt::List(List::Compound(list))) => list.clone(),
            _ => Vec::new(),
        };
        let pairs: Vec<Compound> = entities
            .into_iter()
            .map(|entity| {
                let [x, y, z] = ["x", "y", "z"].map(|key| coordinate(&entity, key));
                let mut pair = Compound::new();
                pair.insert(
                    "state",
                    chunk.block_state((x & 15) as usize, y, (z & 15) as usize) as i32,
                );
                pair.insert("entity", entity);
                pair
            })
            .collect();
        let list = if pairs.is_empty() {
            List::Empty
        } else {
            List::Compound(pairs)
        };
        normalize.insert(case.to_string(), list);
        pending.push((case, label, vanilla_entities));
    }

    // Vanilla saves every field of a loaded block entity, and leaves out those equal to their defaults. Let it
    // load and save Harpas's block entities too, so that both are compared in the same form.
    let normalize_input = reference.temp.0.join("entities.nbt");
    let mut bytes = Vec::new();
    Nbt::Compound(normalize).write_named("", &mut bytes);
    fs::write(&normalize_input, bytes).unwrap();
    let normalize_output = reference.temp.0.join("entities-out.nbt");
    reference.java(
        "SinglePoolElementProbe",
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
    for (case, label, vanilla_entities) in pending {
        let harpas = match normalized.get(&case.to_string()) {
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
    eprintln!(
        "{} single pool elements match vanilla, {placed} of them placing blocks in the chunk",
        cases.len()
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
