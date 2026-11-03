//! Empty overworld chunks for standalone void-world connections.

use harpas_protocol_java::codec;
use harpas_protocol_java::v26_1::play;

/// Sections in an overworld chunk column: 384 blocks from y = -64.
const SECTIONS: usize = 24;
/// Global ID of `minecraft:air` in the block state registry.
const AIR: i32 = 0;

/// The data of an empty chunk column: every section air, with one biome.
pub(crate) fn empty_chunk(
    x: i32,
    z: i32,
    biome: i32,
) -> play::clientbound::ChunkDataAndUpdateLight {
    let mut data = Vec::new();
    for _ in 0..SECTIONS {
        // No blocks and no fluids.
        codec::write_i16(&mut data, 0);
        codec::write_i16(&mut data, 0);
        // Block states and biomes as single-valued palettes: 0 bits per entry, the value, and no data array.
        codec::write_u8(&mut data, 0);
        codec::write_varint(&mut data, AIR);
        codec::write_u8(&mut data, 0);
        codec::write_varint(&mut data, biome);
    }
    play::clientbound::ChunkDataAndUpdateLight {
        x,
        z,
        heightmaps: Vec::new(),
        data,
        block_entities: Vec::new(),
        light: play::LightData {
            sky_light_mask: Vec::new(),
            block_light_mask: Vec::new(),
            empty_sky_light_mask: Vec::new(),
            empty_block_light_mask: Vec::new(),
            sky_light: Vec::new(),
            block_light: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_sections_are_eight_bytes() {
        let chunk = empty_chunk(0, 0, 40);
        assert_eq!(chunk.data.len(), SECTIONS * 8);
        // Block count, fluid count, air as a single value, then the biome as a single value.
        assert_eq!(chunk.data[..8], [0, 0, 0, 0, 0, 0, 0, 40]);
    }
}
