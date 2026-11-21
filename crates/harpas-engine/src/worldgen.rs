//! Connecting the terrain generator to the engine's chunk workers and Anvil storage.

use std::io;
use std::sync::Arc;

use harpas_anvil::{Chunk, ChunkSection, DATA_VERSION};
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_worldgen::{
    GeneratedChunk, HeightmapType, NoiseBasedChunkGenerator, StructureTemplates, WorldgenData,
};

use crate::block::BlockPos;
use crate::world::{ChunkGenerator, ChunkPos, ChunkReader, ChunkSource, ChunkStorage};

mod features;
mod structures;
mod treasure;

pub use features::{decorated, decorated_with_structures};

pub use structures::DecorationStarts;

/// The structure starts whose pieces reach a chunk, which its decoration places, with what they read around it.
pub type StartSource =
    dyn Fn(ChunkPos, &ChunkReader<'_>) -> io::Result<DecorationStarts> + Send + Sync;

type TreasureSource =
    dyn Fn(BlockPos, &ChunkStorage) -> io::Result<Option<(crate::MapData, [i32; 2])>> + Send + Sync;

/// The Overworld's structure starts for decoration, read from saved chunks where they have them.
pub fn overworld_starts(
    seed: i64,
    data: Arc<WorldgenData>,
    templates: Arc<StructureTemplates>,
) -> io::Result<Arc<StartSource>> {
    let generator =
        Arc::new(NoiseBasedChunkGenerator::overworld(seed, &data).map_err(io::Error::other)?);
    let structures = Arc::new(structures::Structures::new(
        seed, &data, &generator, templates,
    )?);
    Ok(starts_source(generator, structures, data))
}

fn starts_source(
    generator: Arc<NoiseBasedChunkGenerator>,
    structures: Arc<structures::Structures>,
    data: Arc<WorldgenData>,
) -> Arc<StartSource> {
    Arc::new(move |pos, read| structures.referenced(pos, &generator, &data, read))
}

/// The Overworld's generator of `carvers` chunks and, when it generates structures, where decoration finds their
/// starts. Both share one noise generator and one structure state, whose ring positions take seconds to compute.
pub struct Overworld {
    /// Generates chunks through carvers, with their structure starts and references.
    pub generator: Arc<ChunkGenerator>,
    /// The structure starts whose pieces reach a chunk, when the world generates structures.
    pub starts: Option<Arc<StartSource>>,
    treasure: Option<Arc<TreasureSource>>,
}

impl Overworld {
    /// Adds FEATURES decoration and enables exploration maps to use the same structure and biome generator.
    pub fn decorate(
        self,
        seed: i64,
        data: Arc<WorldgenData>,
        templates: Arc<StructureTemplates>,
    ) -> io::Result<Arc<dyn ChunkSource>> {
        if let Some(starts) = self.starts {
            features::with_structures(self.generator, starts, seed, data, templates, self.treasure)
        } else {
            decorated(self.generator, seed, &data, templates)
        }
    }
}

/// Compiles the Overworld's generator, whose biome IDs match the registry used to read and send chunks, and with
/// `generate_structures` the structure starts its decoration places.
pub fn overworld(
    seed: i64,
    data: Arc<WorldgenData>,
    templates: Arc<StructureTemplates>,
    generate_structures: bool,
    biomes: &[&str],
) -> io::Result<Overworld> {
    if data.biomes.len() != biomes.len()
        || !data
            .biomes
            .keys()
            .map(String::as_str)
            .eq(biomes.iter().copied())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "worldgen biome IDs do not match the server's biome registry",
        ));
    }
    let generator =
        Arc::new(NoiseBasedChunkGenerator::overworld(seed, &data).map_err(io::Error::other)?);
    let structure_source = generate_structures
        .then(|| structures::Structures::new(seed, &data, &generator, Arc::clone(&templates)))
        .transpose()?
        .map(Arc::new);
    let starts = structure_source.as_ref().map(|structures| {
        starts_source(
            Arc::clone(&generator),
            Arc::clone(structures),
            Arc::clone(&data),
        )
    });
    let treasure = structure_source.as_ref().map(|structures| {
        let (structures, generator, data) = (
            Arc::clone(structures),
            Arc::clone(&generator),
            Arc::clone(&data),
        );
        Arc::new(move |pos, storage: &ChunkStorage| {
            treasure::build(&structures, &generator, &data, pos, storage)
        }) as Arc<TreasureSource>
    });
    let chunks: Arc<ChunkGenerator> = Arc::new(move |pos, read| {
        let mut structures = if let Some(structures) = &structure_source {
            structures.for_chunk(pos, &generator, &data, read)?
        } else {
            structures::ChunkStructures::empty()
        };
        // Structures place their blocks in decoration; carvers adapts the terrain around them.
        let generated = generator
            .generate_chunk_carved_with_beardifier(
                pos.x,
                pos.z,
                &data,
                std::mem::take(&mut structures.beardifier),
            )
            .map_err(io::Error::other)?;
        let mut chunk = into_anvil(generated)?;
        chunk.rest.insert("structures", structures.into_nbt());
        Ok(chunk)
    });
    Ok(Overworld {
        generator: chunks,
        starts,
        treasure,
    })
}

/// Compiles an Overworld generator whose biome IDs match the registry used to read and send chunks.
pub fn overworld_generator(
    seed: i64,
    data: Arc<WorldgenData>,
    templates: Arc<StructureTemplates>,
    generate_structures: bool,
    biomes: &[&str],
) -> io::Result<Arc<ChunkGenerator>> {
    Ok(overworld(seed, data, templates, generate_structures, biomes)?.generator)
}

/// Converts a chunk that completed carvers into an Anvil proto-chunk that vanilla can resume generating.
pub fn into_anvil(chunk: GeneratedChunk) -> io::Result<Chunk> {
    let mask = chunk
        .carving_mask()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "chunk has not completed carvers",
            )
        })?
        .to_raw_data();
    into_anvil_with_mask(chunk, mask)
}

fn empty_structures() -> Compound {
    let mut tag = Compound::new();
    tag.insert("starts", Compound::new());
    tag.insert("References", Compound::new());
    tag
}

fn into_anvil_with_mask(chunk: GeneratedChunk, mask: Vec<i64>) -> io::Result<Chunk> {
    let min_section_y = chunk.min_section_y();
    let mut rest = Compound::new();
    rest.insert("DataVersion", DATA_VERSION);
    rest.insert("xPos", chunk.chunk_x());
    rest.insert("zPos", chunk.chunk_z());
    rest.insert("yPos", min_section_y);
    rest.insert("Status", "minecraft:carvers");
    rest.insert("LastUpdate", 0_i64);
    rest.insert("InhabitedTime", 0_i64);
    rest.insert("carving_mask", Nbt::LongArray(mask));
    let mut maps = Compound::new();
    for kind in [
        HeightmapType::WorldSurface,
        HeightmapType::OceanFloor,
        HeightmapType::MotionBlocking,
        HeightmapType::MotionBlockingNoLeaves,
    ] {
        maps.insert(
            kind.nbt_key(),
            Nbt::LongArray(chunk.heightmaps().pack(kind)),
        );
    }
    rest.insert("Heightmaps", maps);
    let mut post = vec![Vec::new(); chunk.sections().len()];
    for &(x, y, z) in chunk.fluid_postprocessing() {
        let index = usize::try_from((y >> 4) - min_section_y)
            .ok()
            .filter(|&index| index < post.len())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "fluid position outside chunk")
            })?;
        let packed = (u16::from(x) | (((y & 15) as u16) << 4) | (u16::from(z) << 8)) as i16;
        post[index].push(packed);
    }
    rest.insert(
        "PostProcessing",
        Nbt::List(List::List(
            post.into_iter()
                .map(|entries| {
                    if entries.is_empty() {
                        List::Empty
                    } else {
                        List::Short(entries)
                    }
                })
                .collect(),
        )),
    );
    for key in ["block_entities", "entities", "block_ticks", "fluid_ticks"] {
        rest.insert(key, Nbt::List(List::Empty));
    }
    rest.insert("structures", empty_structures());
    let sections = chunk
        .into_sections()
        .into_iter()
        .enumerate()
        .map(|(index, section)| {
            let y = i8::try_from(min_section_y + index as i32).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "section Y does not fit in Anvil",
                )
            })?;
            Ok(ChunkSection {
                y,
                section: Some(section),
                rest: Compound::new(),
            })
        })
        .collect::<io::Result<_>>()?;
    Ok(Chunk { sections, rest })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harpas_core::block::block_states;
    use harpas_worldgen::carver::CarvingMask;

    #[test]
    fn carved_chunks_round_trip_sections_heightmaps_and_mask_as_proto_chunks() {
        let mut generated = GeneratedChunk::new(2, -3, -64, 384, 2);
        let stone = block_states().default_state("minecraft:stone").unwrap();
        generated.set_block_state(3, 70, 4, stone);
        generated.sections_mut()[8].set_biome(1, 2, 3, 1);
        let mut mask = CarvingMask::new(-64, 384);
        mask.insert(3, 70, 4);
        let expected = mask.to_raw_data();
        let chunk = into_anvil_with_mask(generated, expected.clone()).unwrap();
        let biomes = ["minecraft:plains", "minecraft:forest"];
        let (decoded, warnings) = Chunk::read(chunk.to_nbt(&biomes), &biomes).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(
            decoded.rest.get("Status"),
            Some(&Nbt::from("minecraft:carvers"))
        );
        assert_eq!(
            decoded.rest.get("carving_mask"),
            Some(&Nbt::LongArray(expected))
        );
        let Some(Nbt::Compound(maps)) = decoded.rest.get("Heightmaps") else {
            panic!("heightmaps are missing")
        };
        assert_eq!(maps.len(), 4);
        assert_eq!(
            decoded.sections[8]
                .section
                .as_ref()
                .unwrap()
                .block_state(3, 6, 4),
            stone
        );
        assert_eq!(
            decoded.sections[8].section.as_ref().unwrap().biome(1, 2, 3),
            1
        );
        assert_eq!(decoded.sections[0].y, -4);
    }

    #[test]
    fn unfinished_chunks_are_not_serialized_as_carved() {
        let chunk = GeneratedChunk::new(0, 0, -64, 384, 1);
        assert!(into_anvil(chunk).is_err());
    }
}
