//! Compares woodland mansion blocks with vanilla's: natural mansions placed chunk by chunk on their carved
//! terrain, with the blocks they reshape beside each chunk, their chests and loot seeds.

mod reference;

use std::collections::{BTreeMap, HashMap};

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_core::random::{RandomSource, WorldgenRandom, XoroshiroRandom};
use harpas_worldgen::shape_update::ShapeUpdates;
use harpas_worldgen::{
    GeneratedChunk, NoiseBasedChunkGenerator, RegionFeatureWorld, StructureStart, StructureState,
    StructureTemplates, WorldgenData,
};
use reference::Reference;

/// The seeds whose mansions are placed, and how many of each.
fn mansions() -> &'static [(i64, usize)] {
    if reference::reduced_sample() {
        &[(-4_172_144_997_902_289_642, 1)]
    } else {
        &[(0, 2), (-4_172_144_997_902_289_642, 2)]
    }
}

fn blocks(chunk: &GeneratedChunk) -> Vec<i32> {
    let mut values = Vec::with_capacity(384 * 256);
    for y in -64..320 {
        for z in 0..16 {
            for x in 0..16 {
                values.push(chunk.block_state(x, y, z) as i32);
            }
        }
    }
    values
}

/// The first mansions of a seed, nearest the origin first.
fn find_mansions(
    seed: i64,
    count: usize,
    data: &WorldgenData,
    templates: &StructureTemplates,
    generator: &NoiseBasedChunkGenerator,
) -> Vec<StructureStart> {
    let state = StructureState::new(seed, data, generator).unwrap();
    let mut found = Vec::new();
    for r in 0_i32..1024 {
        for x in -r..=r {
            for z in -r..=r {
                if i32::max(x.abs(), z.abs()) != r
                    || !state.is_structure_chunk("minecraft:woodland_mansions", x, z)
                {
                    continue;
                }
                found.extend(
                    state
                        .starts_in_chunk(generator, data, templates, x, z)
                        .unwrap()
                        .into_iter()
                        .filter(|start| start.structure == "minecraft:mansion"),
                );
                if found.len() >= count {
                    found.truncate(count);
                    return found;
                }
            }
        }
    }
    panic!("seed {seed} has fewer than {count} mansions near the origin");
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn mansion_blocks_match_vanilla() {
    let reference = Reference::required();
    let data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let templates = StructureTemplates::from_zip_path(&reference.server_jar).unwrap();
    let updates = ShapeUpdates::new(&data).unwrap();
    let states = block_states();

    let mut inputs = Compound::new();
    let mut expected = BTreeMap::new();
    for &(seed, count) in mansions() {
        let generator = NoiseBasedChunkGenerator::overworld(seed, &data).unwrap();
        for start in find_mansions(seed, count, &data, &templates, &generator) {
            let b = start.bounding_box;
            let mut carved = HashMap::new();
            let mut carved_at = |x: i32, z: i32| -> GeneratedChunk {
                carved
                    .entry((x, z))
                    .or_insert_with(|| generator.generate_chunk_carved(x, z, &data).unwrap())
                    .clone()
            };
            for cx in (b.min[0] >> 4)..=(b.max[0] >> 4) {
                for cz in (b.min[2] >> 4)..=(b.max[2] >> 4) {
                    let name = format!("{seed} {cx},{cz}");
                    let neighbors: Vec<GeneratedChunk> = (-1..=1)
                        .flat_map(|dx| (-1..=1).map(move |dz| (dx, dz)))
                        .map(|(dx, dz)| carved_at(cx + dx, cz + dz))
                        .collect();
                    let target = carved_at(cx, cz);
                    let mut input = Compound::new();
                    input.insert("x", cx);
                    input.insert("z", cz);
                    input.insert("seed", seed);
                    input.insert("blocks", Nbt::IntArray(blocks(&target)));
                    input.insert("start", start.to_nbt());
                    let lookup: HashMap<(i32, i32), &GeneratedChunk> = neighbors
                        .iter()
                        .map(|c| ((c.chunk_x(), c.chunk_z()), c))
                        .collect();
                    let block_at = |x: i32, y: i32, z: i32| {
                        lookup[&(x >> 4, z >> 4)].block_state(
                            (x & 15) as usize,
                            y,
                            (z & 15) as usize,
                        )
                    };
                    let mut surrounding = Vec::with_capacity(384 * 18 * 18);
                    for y in -64..320 {
                        for z in -1..=16 {
                            for x in -1..=16 {
                                surrounding.push(block_at(cx * 16 + x, y, cz * 16 + z) as i32);
                            }
                        }
                    }
                    input.insert("surrounding", Nbt::IntArray(surrounding));
                    inputs.insert(name.clone(), input);
                    expected.insert(name, (seed, cx, cz, start.clone(), neighbors));
                }
            }
        }
    }
    eprintln!("Comparing {} mansion chunks", expected.len());

    reference.compile(&[
        (
            "StructurePlacementProbe.java",
            include_str!("StructurePlacementProbe.java"),
        ),
        ("MansionProbe.java", include_str!("MansionProbe.java")),
    ]);
    let input_path = reference.temp.0.join("mansion-input.nbt");
    let output_path = reference.temp.0.join("mansion-output.nbt");
    let mut bytes = Vec::new();
    Nbt::Compound(inputs).write_named("", &mut bytes).unwrap();
    std::fs::write(&input_path, bytes).unwrap();
    reference.java(
        "MansionProbe",
        [
            input_path.display().to_string(),
            output_path.display().to_string(),
        ],
    );
    let bytes = std::fs::read(output_path).unwrap();
    let (_, Nbt::Compound(actual)) = Nbt::read_named(&mut bytes.as_slice()).unwrap() else {
        panic!("output")
    };
    let int = |tag: &Compound, key: &str| match tag.get(key) {
        Some(Nbt::Int(value)) => *value,
        _ => panic!("missing {key}"),
    };
    // The mansion's place among the structures of its step, which seeds its decoration.
    let mut in_step: Vec<_> = data
        .structures
        .iter()
        .filter(|(_, s)| s.step == "surface_structures")
        .map(|(id, _)| id.as_str())
        .collect();
    in_step.sort_unstable();
    let index = in_step
        .iter()
        .position(|id| *id == "minecraft:mansion")
        .unwrap() as i32;
    assert_eq!(
        int(&actual, "index"),
        index,
        "the mansion's index in its step"
    );
    assert_eq!(int(&actual, "step"), 4, "surface_structures");

    let describe = |id: i32| format!("{:?}", states.properties(id as u32));
    let mut failures = Vec::new();
    // The blocks that differ, by vanilla's block, to see which behaviors are missing.
    let mut by_block = BTreeMap::<String, usize>::new();
    for (name, (seed, cx, cz, start, neighbors)) in expected {
        let Some(Nbt::Compound(vanilla)) = actual.get(&name) else {
            panic!("missing case {name}")
        };
        let mut world = RegionFeatureWorld::new(neighbors.clone(), 0).unwrap();
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(0));
        let decoration = random.set_decoration_seed(seed, cx * 16, cz * 16);
        random.set_feature_seed(decoration, index, 4);
        let mut entities =
            start.place_mansion(&mut world, cx, cz, &templates, &updates, &mut random);
        // Vanilla's order: `BlockPos.asLong`.
        entities.sort_by_key(|entity| {
            let [x, y, z] = ["x", "y", "z"].map(|key| match entity.get(key) {
                Some(Nbt::Int(value)) => i64::from(*value),
                _ => panic!("a block entity's {key}"),
            });
            ((x & 0x3ff_ffff) << 38) | ((z & 0x3ff_ffff) << 12) | (y & 0xfff)
        });
        let mut case = Vec::new();

        let Some(Nbt::IntArray(want)) = vanilla.get("blocks") else {
            panic!("blocks")
        };
        let got = blocks(world.chunk(cx, cz).unwrap());
        let mut diffs = Vec::new();
        for (i, (&a, &b)) in want.iter().zip(&got).enumerate() {
            if a != b {
                let block = |id: i32| states.properties(id as u32).map_or("?", |(n, _)| n);
                *by_block
                    .entry(format!("{} / {}", block(a), block(b)))
                    .or_default() += 1;
                diffs.push(format!(
                    "({},{},{}) vanilla={} Harpas={}",
                    cx * 16 + (i % 16) as i32,
                    (i / 256) as i32 - 64,
                    cz * 16 + ((i / 16) % 16) as i32,
                    describe(a),
                    describe(b)
                ));
            }
        }
        if !diffs.is_empty() {
            case.push(format!(
                "{} blocks differ: {}",
                diffs.len(),
                diffs[..diffs.len().min(12)].join("; ")
            ));
        }

        // The blocks reshaped beside the chunk.
        let mut ring = Compound::new();
        for original in &neighbors {
            if (original.chunk_x(), original.chunk_z()) == (cx, cz) {
                continue;
            }
            let now = world.chunk(original.chunk_x(), original.chunk_z()).unwrap();
            for y in -64..320 {
                for z in 0..16 {
                    for x in 0..16 {
                        let state = now.block_state(x, y, z);
                        if state != original.block_state(x, y, z) {
                            let wx = original.chunk_x() * 16 + x as i32;
                            let wz = original.chunk_z() * 16 + z as i32;
                            ring.insert(format!("{wx},{y},{wz}"), state as i32);
                        }
                    }
                }
            }
        }
        let vanilla_ring = match vanilla.get("ring_writes") {
            Some(Nbt::Compound(ring)) => ring.clone(),
            _ => Compound::new(),
        };
        // Vanilla records every write beside the chunk, even of the state already there.
        let vanilla_changed: Compound = vanilla_ring
            .iter()
            .filter(|(key, value)| {
                let [x, y, z]: [i32; 3] = key
                    .split(',')
                    .map(|v| v.parse().unwrap())
                    .collect::<Vec<_>>()
                    .try_into()
                    .unwrap();
                let original = neighbors
                    .iter()
                    .find(|c| (c.chunk_x(), c.chunk_z()) == (x >> 4, z >> 4))
                    .unwrap()
                    .block_state((x & 15) as usize, y, (z & 15) as usize);
                **value != Nbt::Int(original as i32)
            })
            .map(|(key, value)| (key, value.clone()))
            .collect();
        if vanilla_changed != ring {
            let show = |c: &Compound| {
                c.iter()
                    .take(8)
                    .map(|(k, v)| match v {
                        Nbt::Int(id) => format!("{k}={}", describe(*id)),
                        _ => k.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            case.push(format!(
                "ring: vanilla {} [{}], Harpas {} [{}]",
                vanilla_changed.len(),
                show(&vanilla_changed),
                ring.len(),
                show(&ring)
            ));
        }
        if let Some(Nbt::Compound(far)) = vanilla.get("far_writes")
            && !far.is_empty()
        {
            case.push(format!(
                "vanilla wrote beyond the ring: {:?}",
                far.iter()
                    .take(6)
                    .map(|(k, _)| k.to_string())
                    .collect::<Vec<_>>()
            ));
        }

        let vanilla_entities = match vanilla.get("block_entities") {
            Some(Nbt::List(List::Compound(list))) => list.clone(),
            _ => Vec::new(),
        };
        if vanilla_entities.len() != entities.len() {
            case.push(format!(
                "block entities: vanilla {}, Harpas {}",
                vanilla_entities.len(),
                entities.len()
            ));
        }
        for (v, h) in vanilla_entities.iter().zip(&entities) {
            if v != h {
                case.push(format!("block entity: vanilla {v:?}, Harpas {h:?}"));
                break;
            }
        }
        if let Some(Nbt::Long(tail)) = vanilla.get("tail")
            && *tail != random.next_long()
        {
            case.push("the decoration random was drawn a different number of times".to_string());
        }
        if !case.is_empty() {
            failures.push(format!("{name}:\n  {}", case.join("\n  ")));
        }
    }
    assert!(
        failures.is_empty(),
        "{} chunks differ, blocks by vanilla / Harpas block: {by_block:?}\n{}",
        failures.len(),
        failures
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
