//! Ocean monument starts: the surrounding biomes and the single saved building piece.
//! The building's internal room layout and block placement are still to be ported.

use harpas_core::random::RandomSource;

use super::start::{BoundingBox, Direction, StructurePiece};
use crate::data::{WorldgenData, WorldgenDataError};
use crate::generator::NoiseBasedChunkGenerator;

/// Radius of `OceanMonumentStructure`'s `BiomeSource.getBiomesWithin` check, in blocks.
const BIOME_RADIUS: i32 = 29;

/// The biome at the generation point is checked separately against the structure's allowed biomes.
pub(super) fn has_valid_surroundings(
    generator: &NoiseBasedChunkGenerator,
    data: &WorldgenData,
    min_x: i32,
    min_z: i32,
) -> Result<bool, WorldgenDataError> {
    let allowed =
        data.resolve_biomes(&["#minecraft:required_ocean_monument_surrounding".to_string()])?;
    let mut noise = generator.create_noise_chunk(min_x >> 4, min_z >> 4);
    Ok(all_surrounding_biomes(
        [min_x + 9, generator.sea_level(), min_z + 9],
        |[x, y, z]| {
            let climate = noise.sample_climate(x, y, z);
            let biome = generator.biome_source().get_noise_biome(climate, &mut None);
            allowed.contains(&biome)
        },
    ))
}

/// Checks the inclusive quart grid of the whole volume, not just its horizontal plane.
fn all_surrounding_biomes(center: [i32; 3], mut allowed: impl FnMut([i32; 3]) -> bool) -> bool {
    let min = center.map(|v| (v - BIOME_RADIUS) >> 2);
    let max = center.map(|v| (v + BIOME_RADIUS) >> 2);
    for x in min[0]..=max[0] {
        for y in min[1]..=max[1] {
            for z in min[2]..=max[2] {
                if !allowed([x, y, z]) {
                    return false;
                }
            }
        }
    }
    true
}

/// The one piece stored in the start. Its rooms are internal to this piece in vanilla.
pub(super) fn piece(random: &mut impl RandomSource, min_x: i32, min_z: i32) -> StructurePiece {
    let orientation = Direction::random(random);
    StructurePiece {
        kind: "minecraft:omb",
        bounding_box: BoundingBox::oriented([min_x - 29, 39, min_z - 29], orientation, 58, 23, 58),
        orientation: Some(orientation),
        gen_depth: 0,
        junctions: Vec::new(),
        jigsaw: None,
        template: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_surroundings_include_every_quart_at_both_corners() {
        // Chunk (-3, 4): negative coordinates must round down before scanning the 3D volume.
        let center = [-39, 63, 73];
        let mut sampled = std::collections::HashSet::new();
        assert!(all_surrounding_biomes(center, |pos| {
            assert!(sampled.insert(pos));
            true
        }));
        assert_eq!(sampled.len(), 15 * 16 * 15);
        assert!(sampled.contains(&[-17, 8, 11]));
        assert!(sampled.contains(&[-3, 23, 25]));
        assert!(!sampled.contains(&[-18, 8, 11]));
        assert!(!sampled.contains(&[-3, 24, 25]));
    }

    #[test]
    fn a_bad_biome_above_below_or_at_the_horizontal_boundary_refuses_the_start() {
        let center = [9, 63, 9];
        for bad in [[2, 8, 2], [2, 23, 2], [-5, 15, -5], [9, 15, 9]] {
            assert!(!all_surrounding_biomes(center, |pos| pos != bad));
        }
        for outside in [[2, 7, 2], [2, 24, 2], [-6, 15, -5], [10, 15, 9]] {
            assert!(all_surrounding_biomes(center, |pos| pos != outside));
        }
    }
}
