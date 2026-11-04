//! Connecting the terrain generator to the engine's chunk workers and Anvil storage.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;

use harpas_anvil::{Chunk, ChunkSection, DATA_VERSION};
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_worldgen::{
    GeneratedChunk, HeightmapType, NoiseBasedChunkGenerator, StructureTemplates, WorldgenData,
};

use crate::world::ChunkGenerator;

mod structures;

/// Compiles an Overworld generator whose biome IDs match the registry used to read and send chunks.
pub fn overworld_generator(
    seed: i64,
    data: Arc<WorldgenData>,
    templates: Arc<StructureTemplates>,
    generate_structures: bool,
    biomes: &[&str],
) -> io::Result<Arc<ChunkGenerator>> {
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
    let generator = NoiseBasedChunkGenerator::overworld(seed, &data).map_err(io::Error::other)?;
    let structure_source = generate_structures
        .then(|| structures::Structures::new(seed, &data, &generator, templates))
        .transpose()?;
    Ok(Arc::new(move |pos, read| {
        let mut structures = if let Some(structures) = &structure_source {
            structures.for_chunk(pos, &generator, &data, read)?
        } else {
            structures::ChunkStructures::empty()
        };
        let mut generated = generator
            .generate_chunk_carved_with_beardifier(
                pos.x,
                pos.z,
                &data,
                std::mem::take(&mut structures.beardifier),
            )
            .map_err(io::Error::other)?;
        let terrain_chunks = if let Some(source) = &structure_source {
            source.terrain_for_placements(&generated, &structures, &generator, &data, read)?
        } else {
            HashMap::new()
        };
        let entities = structures.place_blocks(&mut generated, seed, &terrain_chunks);
        let mut chunk = into_anvil(generated)?;
        if !entities.is_empty() {
            chunk
                .rest
                .insert("block_entities", List::Compound(entities));
        }
        chunk.rest.insert("structures", structures.into_nbt());
        Ok(chunk)
    }))
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
