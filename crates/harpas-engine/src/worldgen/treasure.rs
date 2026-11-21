//! Buried treasure maps, from shipwreck and ocean ruin chests, look for a buried treasure start and sample
//! biomes around it.

use std::collections::HashMap;
use std::io;

use harpas_core::chunk::Section;
use harpas_worldgen::{NoiseBasedChunkGenerator, WorldgenData};

use super::structures::Structures;
use crate::MapData;
use crate::block::BlockPos;
use crate::world::{ChunkPos, ChunkStorage};

const SET: &str = "minecraft:buried_treasures";
const RADIUS: i32 = 50;

pub(super) fn build(
    structures: &Structures,
    generator: &NoiseBasedChunkGenerator,
    data: &WorldgenData,
    origin: BlockPos,
    storage: &ChunkStorage,
) -> io::Result<Option<(MapData, [i32; 2])>> {
    let Some(set) = data.structure_sets.get(SET) else {
        return Ok(None);
    };
    // Buried treasure uses random spread with spacing 1. Vanilla returns the first valid candidate in
    // the perimeter's X-major order, including starts with existing references.
    let mut found = None;
    'search: for radius in 0..=RADIUS {
        for dx in -radius..=radius {
            for dz in -radius..=radius {
                if dx.abs().max(dz.abs()) != radius {
                    continue;
                }
                let pos = ChunkPos {
                    x: (origin.x >> 4) + dx,
                    z: (origin.z >> 4) + dz,
                };
                if structures
                    .buried_treasure_at(pos, SET, generator, data, &|pos| storage.read(pos))?
                {
                    let [x, _, z] = set.placement.common.locate_offset;
                    found = Some([(pos.x << 4) + x, (pos.z << 4) + z]);
                    break 'search;
                }
            }
        }
    }
    let Some(target) = found else { return Ok(None) };

    let water = data
        .resolve_biomes(&["#minecraft:water_on_map_outlines".to_owned()])
        .map_err(io::Error::other)?;
    let mut saved: HashMap<(i32, i32, i32), Section> = HashMap::new();
    // At scale 1, the image and biome zoom sample only sections -1 and 0 of these columns.
    let corner = target.map(|block| (block + 64).div_euclid(256) * 256 - 64);
    for x in ((corner[0] - 2) >> 4)..=((corner[0] + 254) >> 4) {
        for z in ((corner[1] - 2) >> 4)..=((corner[1] + 254) >> 4) {
            if let Some(nbt) = storage.read(ChunkPos { x, z })? {
                for section in storage.decode(nbt)?.sections {
                    if let (-1 | 0, Some(section_data)) = (section.y, section.section) {
                        saved.insert((x, i32::from(section.y), z), section_data);
                    }
                }
            }
        }
    }
    let mut last = None;
    let mut biomes = HashMap::new();
    let preview = MapData::treasure_preview(target, |x, z| {
        let biome = generator.biome_manager().get_biome(x, 0, z, |x, y, z| {
            if let Some(section) = saved.get(&(x >> 2, y >> 2, z >> 2)) {
                return section.biome((x & 3) as usize, (y & 3) as usize, (z & 3) as usize);
            }
            *biomes.entry((x, y, z)).or_insert_with(|| {
                let climate =
                    generator
                        .graph()
                        .sample_climate_stateless(generator.router(), x, y, z);
                generator.biome_source().get_noise_biome(climate, &mut last)
            })
        });
        water.contains(&biome)
    });
    Ok(Some((preview, target)))
}
