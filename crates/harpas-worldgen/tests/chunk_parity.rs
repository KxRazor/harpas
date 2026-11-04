//! Compares whole generated chunks, block by block and biome by biome, against an owner-provided server.

mod reference;

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use harpas_worldgen::{
    Beardifier, GeneratedChunk, HeightmapType, NoiseBasedChunkGenerator, StructureStart,
    StructureState, StructureTemplates, WorldgenData, overworld_biome_parameters,
};
use reference::Reference;

/// Includes seed 0, which seeds `Xoroshiro128++` specially, and a seed that fills all 64 bits.
const SEEDS: [i64; 5] = [0, 42, -1, 123_456_789, -4_172_144_997_902_289_642];
/// Differences shown when the test fails; all of them are counted.
const SHOWN_DIFFERENCES: usize = 40;

/// A contiguous area at the origin, a wide lattice to meet many biomes, and the far edges of the world, with chunks
/// for the biomes that the lattice misses.
fn chunks(seed: i64) -> Vec<(i32, i32)> {
    let mut chunks = Vec::new();
    if seed == 42 {
        chunks.push((-3000, -2472)); // windswept forest
    }
    for x in -4..4 {
        for z in -4..4 {
            chunks.push((x, z));
        }
    }
    let lattice = if reference::reduced_sample() { 2 } else { 8 };
    for x in -lattice..lattice {
        for z in -lattice..lattice {
            chunks.push((x * 97 + 13, z * 97 - 29));
        }
    }
    for (x, z) in [
        (1_874_999, 1_874_999),
        (-1_875_000, 1_874_999),
        (1_874_999, -1_875_000),
        (-1_875_000, -1_875_000),
    ] {
        chunks.push((x, z));
    }
    chunks
}

struct ReferenceChunk {
    position: (i32, i32),
    blocks: Vec<u8>,
    biomes: Vec<u8>,
    carving_mask: Vec<i64>,
    postprocessing: Vec<(u8, u16)>,
    heightmaps: Vec<Vec<i64>>,
}

struct Input(BufReader<File>);

impl Input {
    fn bytes(&mut self, length: usize) -> Vec<u8> {
        let mut bytes = vec![0; length];
        self.0
            .read_exact(&mut bytes)
            .expect("truncated Java result");
        bytes
    }

    fn int(&mut self) -> usize {
        u32::from_be_bytes(self.bytes(4).try_into().unwrap()) as usize
    }

    fn longs(&mut self) -> Vec<i64> {
        let count = self.int();
        self.bytes(count * 8)
            .as_chunks::<8>()
            .0
            .iter()
            .map(|word| i64::from_be_bytes(*word))
            .collect()
    }

    fn string(&mut self) -> String {
        let length = u16::from_be_bytes(self.bytes(2).try_into().unwrap());
        String::from_utf8(self.bytes(length.into())).expect("a biome name")
    }

    fn chunk(&mut self, position: (i32, i32)) -> ReferenceChunk {
        let blocks = self.bytes(16 * 16 * 384 * 2);
        let biomes = self.bytes(4 * 4 * 96 * 2);
        let carving_mask = self.longs();
        let count = self.int();
        let postprocessing = self
            .bytes(count * 3)
            .as_chunks::<3>()
            .0
            .iter()
            .map(|entry| (entry[0], u16::from_be_bytes([entry[1], entry[2]])))
            .collect();
        let heightmaps = HeightmapType::ALL.map(|_| self.longs()).to_vec();
        ReferenceChunk {
            position,
            blocks,
            biomes,
            carving_mask,
            postprocessing,
            heightmaps,
        }
    }
}

fn id(bytes: &[u8], index: usize) -> u32 {
    u32::from(u16::from_be_bytes([bytes[index * 2], bytes[index * 2 + 1]]))
}

/// Vanilla's per-section order for the fluids to update, each packed as `x | y << 4 | z << 8`.
fn postprocessing(chunk: &GeneratedChunk) -> Vec<(u8, u16)> {
    let mut entries: Vec<_> = chunk
        .fluid_postprocessing()
        .iter()
        .map(|&(x, y, z)| {
            let section = (y >> 4) - chunk.min_section_y();
            let packed = u16::from(x) | (((y & 15) as u16) << 4) | (u16::from(z) << 8);
            (section as u8, packed)
        })
        .collect();
    entries.sort_by_key(|&(section, _)| section);
    entries
}

fn describe_state(state: u32) -> String {
    match harpas_core::block::block_states().properties(state) {
        Some((name, properties)) if properties.is_empty() => name.to_owned(),
        Some((name, properties)) => {
            let properties: Vec<_> = properties
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect();
            format!("{name}[{}]", properties.join(","))
        }
        None => format!("unknown state {state}"),
    }
}

/// Describes what differs between a generated chunk and vanilla's.
fn differences(
    generated: &GeneratedChunk,
    expected: &ReferenceChunk,
    biome_names: &[String],
    data: &WorldgenData,
) -> Vec<String> {
    let (cx, cz) = expected.position;
    let mut found = Vec::new();

    let mut count = 0_usize;
    let mut first = None;
    for y in -64..320 {
        for z in 0..16 {
            for x in 0..16 {
                let index = (y + 64) as usize * 256 + z * 16 + x;
                let vanilla = id(&expected.blocks, index);
                let harpas = generated.block_state(x, y, z);
                if vanilla != harpas {
                    count += 1;
                    first.get_or_insert((x, y, z, vanilla, harpas));
                }
            }
        }
    }
    if let Some((x, y, z, vanilla, harpas)) = first {
        found.push(format!(
            "chunk ({cx}, {cz}): {count} blocks differ, first at ({}, {y}, {}): vanilla {}, Harpas {}",
            cx * 16 + x as i32,
            cz * 16 + z as i32,
            describe_state(vanilla),
            describe_state(harpas),
        ));
    }

    let mut count = 0_usize;
    let mut first = None;
    for (section_y, section) in generated.sections().iter().enumerate() {
        for qy in 0..4 {
            for qz in 0..4 {
                for qx in 0..4 {
                    let index = (section_y * 4 + qy) * 16 + qz * 4 + qx;
                    let vanilla = &biome_names[id(&expected.biomes, index) as usize];
                    let harpas = biome_name(data, section.biome(qx, qy, qz));
                    if vanilla != harpas {
                        count += 1;
                        first.get_or_insert((qx, section_y * 4 + qy, qz, vanilla, harpas));
                    }
                }
            }
        }
    }
    if let Some((qx, qy, qz, vanilla, harpas)) = first {
        found.push(format!(
            "chunk ({cx}, {cz}): {count} biomes differ, first at quart ({qx}, {qy}, {qz}) of the chunk: \
             vanilla {vanilla}, Harpas {harpas}"
        ));
    }

    let mask = generated.carving_mask().expect("a carved chunk");
    if mask.to_raw_data() != expected.carving_mask {
        found.push(format!("chunk ({cx}, {cz}): the carving masks differ"));
    }
    let fluids = postprocessing(generated);
    if fluids != expected.postprocessing {
        found.push(format!(
            "chunk ({cx}, {cz}): fluid postprocessing differs: vanilla has {}, Harpas has {}",
            expected.postprocessing.len(),
            fluids.len()
        ));
    }
    for (kind, vanilla) in HeightmapType::ALL.into_iter().zip(&expected.heightmaps) {
        if generated.heightmaps().pack(kind) != *vanilla {
            found.push(format!(
                "chunk ({cx}, {cz}): heightmap {} differs",
                kind.nbt_key()
            ));
        }
    }
    found
}

fn biome_name(data: &WorldgenData, biome: u32) -> &String {
    data.biomes
        .get_index(biome as usize)
        .expect("a biome in the registry")
        .0
}

/// Generates every chunk on all cores, compares it, and returns the differences and the biomes met.
fn compare_seed(
    data: &WorldgenData,
    seed: i64,
    output: &Path,
    positions: &[(i32, i32)],
    beardifiers: Option<&HashMap<(i32, i32), Beardifier>>,
) -> (Vec<String>, BTreeSet<String>) {
    let mut input = Input(BufReader::new(File::open(output).unwrap()));
    let biome_names: Vec<_> = (0..input.int()).map(|_| input.string()).collect();
    let expected: Vec<_> = positions.iter().map(|&at| input.chunk(at)).collect();
    assert_eq!(
        input.0.read(&mut [0]).unwrap(),
        0,
        "unexpected extra Java results"
    );

    let generator = NoiseBasedChunkGenerator::overworld(seed, data).expect("compile generator");
    let next = AtomicUsize::new(0);
    let results = Mutex::new((Vec::new(), BTreeSet::new()));
    let workers = std::thread::available_parallelism().map_or(1, usize::from);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                while let Some(chunk) = expected.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let (cx, cz) = chunk.position;
                    let beardifier = beardifiers
                        .and_then(|all| all.get(&(cx, cz)))
                        .cloned()
                        .unwrap_or_default();
                    let generated = generator
                        .generate_chunk_carved_with_beardifier(cx, cz, data, beardifier)
                        .expect("generate a chunk");
                    let found = differences(&generated, chunk, &biome_names, data);
                    let mut results = results.lock().unwrap();
                    results
                        .0
                        .extend(found.into_iter().map(|d| (chunk.position, d)));
                    for index in 0..chunk.biomes.len() / 2 {
                        results
                            .1
                            .insert(biome_names[id(&chunk.biomes, index) as usize].clone());
                    }
                }
            });
        }
    });
    let (mut found, biomes) = results.into_inner().unwrap();
    found.sort();
    (
        found
            .into_iter()
            .map(|(_, difference)| format!("seed {seed}, {difference}"))
            .collect(),
        biomes,
    )
}

#[test]
fn generated_chunks_match_vanilla_block_by_block() {
    let Some(reference) = Reference::from_env() else {
        eprintln!("Skipping vanilla chunk parity: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    let data =
        WorldgenData::from_zip_path(&reference.server_jar).expect("load vanilla worldgen data");
    reference.compile(&[
        (
            "WorldgenReferenceProbe.java",
            include_str!("WorldgenReferenceProbe.java"),
        ),
        (
            "ChunkParityProbe.java",
            include_str!("ChunkParityProbe.java"),
        ),
    ]);
    let mut compared = 0;
    let mut differences = Vec::new();
    let mut biomes = BTreeSet::new();
    let seeds = reference::sampled_seeds(&SEEDS);
    for &seed in seeds {
        let positions = chunks(seed);
        compared += positions.len();
        let output = reference.temp.0.join("chunks.bin");
        let mut args = vec![output.display().to_string(), seed.to_string()];
        args.extend(positions.iter().map(|(x, z)| format!("{x},{z}")));
        reference.java("ChunkParityProbe", args);
        let (found, met) = compare_seed(&data, seed, &output, &positions, None);
        differences.extend(found);
        biomes.extend(met);
        std::fs::remove_file(&output).unwrap();
    }
    eprintln!(
        "Compared {compared} chunks in {} seeds, meeting {} biomes",
        seeds.len(),
        biomes.len()
    );
    let missing: BTreeSet<_> = overworld_biome_parameters()
        .into_iter()
        .map(|(_, biome)| biome)
        .filter(|biome| !biomes.contains(*biome))
        .collect();
    // A reduced sample has too few chunks to meet every biome.
    assert!(
        missing.is_empty() || reference::reduced_sample(),
        "no chunk has {missing:?}"
    );
    assert!(
        differences.is_empty(),
        "{} differences from vanilla, the first {SHOWN_DIFFERENCES}:\n{}",
        differences.len(),
        differences[..differences.len().min(SHOWN_DIFFERENCES)].join("\n")
    );
}

/// Terrain around one actual start of every Overworld terrain-adaptation mode, including adjacent chunks.
/// Positions are discovered from the seed instead of pinning coordinates or committing vanilla data.
#[test]
fn structure_terrain_matches_vanilla_block_by_block() {
    let Some(reference) = Reference::from_env() else {
        eprintln!("Skipping structure terrain: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    let data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let templates = StructureTemplates::from_zip_path(&reference.server_jar).unwrap();
    reference.compile(&[
        (
            "WorldgenReferenceProbe.java",
            include_str!("WorldgenReferenceProbe.java"),
        ),
        (
            "ChunkParityProbe.java",
            include_str!("ChunkParityProbe.java"),
        ),
        (
            "StructurePlacementProbe.java",
            include_str!("StructurePlacementProbe.java"),
        ),
        (
            "StructureTerrainProbe.java",
            include_str!("StructureTerrainProbe.java"),
        ),
    ]);
    for &seed in reference::sampled_seeds(&[0, -4_172_144_997_902_289_642]) {
        let generator = NoiseBasedChunkGenerator::overworld(seed, &data).unwrap();
        let state = StructureState::new(seed, &data, &generator).unwrap();
        let mut known = HashMap::new();
        let mut examples = HashMap::<String, StructureStart>::new();
        // Only possible start chunks need height and biome checks. Scan outward to find nearby examples.
        'search: for radius in 0_i32..256 {
            for x in -radius..=radius {
                for z in -radius..=radius {
                    if x.abs().max(z.abs()) != radius {
                        continue;
                    }
                    let starts = state
                        .starts_in_chunk(&generator, &data, &templates, x, z)
                        .unwrap();
                    for start in &starts {
                        let mode = &data.structures[&start.structure].terrain_adaptation;
                        if mode != "none" {
                            examples
                                .entry(mode.clone())
                                .or_insert_with(|| start.clone());
                        }
                    }
                    known.insert((x, z), starts);
                    if examples.len() == 4 {
                        break 'search;
                    }
                }
            }
        }
        assert_eq!(
            examples.len(),
            4,
            "seed {seed} must exercise all four adapting modes, found {:?}",
            examples.keys().collect::<Vec<_>>()
        );
        let mut positions = BTreeSet::new();
        for (mode, start) in &examples {
            let piece = &start.pieces[0];
            let cx = piece.bounding_box.min[0] >> 4;
            let cz = piece.bounding_box.min[2] >> 4;
            eprintln!(
                "Seed {seed}, {mode}: {} near chunk ({cx}, {cz})",
                start.structure
            );
            for x in cx - 1..=cx + 1 {
                for z in cz - 1..=cz + 1 {
                    positions.insert((x, z));
                }
            }
        }
        let positions: Vec<_> = positions.into_iter().collect();
        let mut beardifiers = HashMap::new();
        for &(cx, cz) in &positions {
            let references = StructureState::references_from(cx, cz, |x, z| {
                if let Some(starts) = known.get(&(x, z)) {
                    return Ok(starts.clone());
                }
                let starts = state.starts_in_chunk(&generator, &data, &templates, x, z)?;
                known.insert((x, z), starts.clone());
                Ok(starts)
            })
            .unwrap();
            let starts: Vec<_> = references
                .iter()
                .map(|reference| {
                    known[&reference.source]
                        .iter()
                        .find(|s| s.structure == reference.structure)
                        .unwrap()
                })
                .collect();
            let beardifier = Beardifier::for_starts(cx, cz, &data, starts).unwrap();
            beardifiers.insert((cx, cz), beardifier);
        }
        let output = reference.temp.0.join("structure-chunks.bin");
        let mut args = vec![output.display().to_string(), seed.to_string()];
        args.extend(positions.iter().map(|(x, z)| format!("{x},{z}")));
        reference.java("StructureTerrainProbe", args);
        let (found, _) = compare_seed(&data, seed, &output, &positions, Some(&beardifiers));
        assert!(
            found.is_empty(),
            "{} differences:\n{}",
            found.len(),
            found
                .iter()
                .take(SHOWN_DIFFERENCES)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
        eprintln!(
            "Compared {} chunks with structure terrain for seed {seed}",
            positions.len()
        );
    }
}

/// Reaches every entry, including the rare ones that chunks are unlikely to meet.
#[test]
fn overworld_biome_parameters_match_vanilla() {
    let Some(reference) = Reference::from_env() else {
        eprintln!("Skipping vanilla biome parameters: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    reference.compile(&[(
        "BiomeParameterProbe.java",
        include_str!("BiomeParameterProbe.java"),
    )]);
    let output = reference.temp.0.join("parameters.txt");
    reference.java("BiomeParameterProbe", [output.display().to_string()]);
    let vanilla = std::fs::read_to_string(&output).unwrap();
    let harpas: Vec<_> = overworld_biome_parameters()
        .into_iter()
        .map(|(point, biome)| {
            let space: Vec<_> = point
                .parameter_space()
                .iter()
                .map(|parameter| format!("{} {}", parameter.min, parameter.max))
                .collect();
            format!("{} {biome}", space.join(" "))
        })
        .collect();
    let vanilla: Vec<_> = vanilla.lines().collect();
    assert_eq!(vanilla.len(), harpas.len(), "number of parameter points");
    for (index, (vanilla, harpas)) in vanilla.iter().zip(&harpas).enumerate() {
        assert_eq!(vanilla, harpas, "parameter point {index}");
    }
}
