//! Which structures start in a chunk and the pieces they start with: vanilla's `ChunkGenerator.createStructures`,
//! `Structure.generate` and the pieces of each structure type.

use harpas_core::random::{LegacyRandom, RandomSource, WorldgenRandom};

use super::StructureState;
use crate::data::{StructureSetEntryDef, WorldgenData, WorldgenDataError};
use crate::generator::{HeightmapType, NoiseBasedChunkGenerator};

/// How far the bounding box of a structure that adapts the terrain reaches past its pieces
/// (`Structure.adjustBoundingBox`).
const TERRAIN_ADAPTATION_MARGIN: i32 = 12;
/// How many chunks around a chunk may hold starts that reach it (`ChunkGenerator.createReferences`).
const REFERENCE_RANGE: i32 = 8;

/// A box of blocks, with inclusive bounds (`BoundingBox`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundingBox {
    /// Lowest X, Y and Z.
    pub min: [i32; 3],
    /// Highest X, Y and Z.
    pub max: [i32; 3],
}

impl BoundingBox {
    /// A box from `x, y, z` facing `orientation`, `width` across, `height` up and `depth` along the direction it
    /// faces (`StructurePiece.makeBoundingBox`).
    pub fn oriented(
        [x, y, z]: [i32; 3],
        orientation: Direction,
        width: i32,
        height: i32,
        depth: i32,
    ) -> Self {
        let (size_x, size_z) = match orientation {
            Direction::North | Direction::South => (width, depth),
            Direction::East | Direction::West => (depth, width),
        };
        Self {
            min: [x, y, z],
            max: [x + size_x - 1, y + height - 1, z + size_z - 1],
        }
    }

    /// Whether the box overlaps the columns from `min_x, min_z` to `max_x, max_z` (`BoundingBox.intersects`).
    pub const fn intersects_columns(&self, min_x: i32, min_z: i32, max_x: i32, max_z: i32) -> bool {
        self.max[0] >= min_x && self.min[0] <= max_x && self.max[2] >= min_z && self.min[2] <= max_z
    }

    fn encapsulate(&mut self, other: &Self) {
        for axis in 0..3 {
            self.min[axis] = self.min[axis].min(other.min[axis]);
            self.max[axis] = self.max[axis].max(other.max[axis]);
        }
    }

    fn inflated(&self, by: i32) -> Self {
        Self {
            min: self.min.map(|value| value - by),
            max: self.max.map(|value| value + by),
        }
    }
}

/// A horizontal direction, as vanilla lists them in `Direction.Plane.HORIZONTAL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Towards negative Z.
    North,
    /// Towards positive X.
    East,
    /// Towards positive Z.
    South,
    /// Towards negative X.
    West,
}

impl Direction {
    const HORIZONTAL: [Self; 4] = [Self::North, Self::East, Self::South, Self::West];

    /// The value saved for this direction (`Direction.get2DDataValue`).
    pub const fn data_value(self) -> i32 {
        match self {
            Self::South => 0,
            Self::West => 1,
            Self::North => 2,
            Self::East => 3,
        }
    }

    /// `StructurePiece.getRandomHorizontalDirection`.
    fn random(random: &mut impl RandomSource) -> Self {
        Self::HORIZONTAL[random.next_int_bounded(4) as usize]
    }
}

/// One piece of a structure, as saved in its start (`StructurePiece`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructurePiece {
    /// The piece type, such as `minecraft:tedp` for a desert pyramid.
    pub kind: &'static str,
    /// The blocks the piece covers.
    pub bounding_box: BoundingBox,
    /// The direction the piece faces, if it has one.
    pub orientation: Option<Direction>,
    /// How many pieces lie between this one and the first.
    pub gen_depth: i32,
}

/// A structure that starts in a chunk (`StructureStart`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructureStart {
    /// The structure's resource location.
    pub structure: String,
    /// The chunk it starts in.
    pub chunk: (i32, i32),
    /// Its pieces, in the order they were made.
    pub pieces: Vec<StructurePiece>,
    /// The blocks its pieces cover, widened for structures that adapt the terrain (`StructureStart.getBoundingBox`).
    pub bounding_box: BoundingBox,
}

/// A start in another chunk that reaches a chunk, which vanilla saves in the chunk's `References`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructureReference {
    /// The structure's resource location.
    pub structure: String,
    /// The chunk where the structure starts.
    pub source: (i32, i32),
}

/// A structure type whose starts Harpas can make.
enum StructureKind {
    BuriedTreasure,
    /// Single pieces of a fixed size: desert pyramids, jungle temples and swamp huts.
    ScatteredFeature {
        piece: &'static str,
        width: i32,
        height: i32,
        depth: i32,
        /// Whether the lowest corner must be at or above the sea level (`SinglePieceStructure`).
        above_sea: bool,
    },
}

impl StructureKind {
    fn from_type(kind: &str) -> Option<Self> {
        let scattered = |piece, width, height, depth, above_sea| Self::ScatteredFeature {
            piece,
            width,
            height,
            depth,
            above_sea,
        };
        Some(match kind {
            "minecraft:buried_treasure" => Self::BuriedTreasure,
            "minecraft:desert_pyramid" => scattered("minecraft:tedp", 21, 15, 21, true),
            "minecraft:jungle_temple" => scattered("minecraft:tejp", 12, 10, 15, true),
            "minecraft:swamp_hut" => scattered("minecraft:tesh", 7, 7, 9, false),
            _ => return None,
        })
    }
}

impl StructureState {
    /// Whether Harpas can make the starts of every structure in a set.
    pub fn supports_set(&self, data: &WorldgenData, set: &str) -> bool {
        data.structure_sets.get(set).is_some_and(|set| {
            set.structures.iter().all(|entry| {
                data.structures
                    .get(&entry.structure)
                    .is_some_and(|structure| StructureKind::from_type(&structure.kind).is_some())
            })
        })
    }

    /// The structures that start in a chunk, at most one for each structure set, in set order
    /// (`ChunkGenerator.createStructures`). Sets with structure types that Harpas cannot make yet are skipped.
    pub fn starts_in_chunk(
        &self,
        generator: &NoiseBasedChunkGenerator,
        data: &WorldgenData,
        chunk_x: i32,
        chunk_z: i32,
    ) -> Result<Vec<StructureStart>, WorldgenDataError> {
        let mut starts = Vec::new();
        for name in self.possible_sets() {
            if !self.supports_set(data, name) || !self.is_structure_chunk(name, chunk_x, chunk_z) {
                continue;
            }
            let entries = &data.structure_sets[name].structures;
            if let [entry] = entries.as_slice() {
                starts.extend(try_generate(
                    generator,
                    data,
                    self.level_seed,
                    entry,
                    chunk_x,
                    chunk_z,
                )?);
                continue;
            }
            let mut options: Vec<_> = entries.iter().collect();
            let mut random = WorldgenRandom::new(LegacyRandom::new(0));
            random.set_large_feature_seed(self.level_seed, chunk_x, chunk_z);
            let mut total: i32 = options.iter().map(|entry| entry.weight).sum();
            while !options.is_empty() {
                let mut choice = random.next_int_bounded(total);
                let index = options
                    .iter()
                    .position(|entry| {
                        choice -= entry.weight;
                        choice < 0
                    })
                    .unwrap_or(options.len());
                let selected = options[index];
                if let Some(start) =
                    try_generate(generator, data, self.level_seed, selected, chunk_x, chunk_z)?
                {
                    starts.push(start);
                    break;
                }
                options.remove(index);
                total -= selected.weight;
            }
        }
        Ok(starts)
    }

    /// The starts of the chunks within reach whose bounding boxes overlap a chunk (`ChunkGenerator.createReferences`).
    pub fn references_in_chunk(
        &self,
        generator: &NoiseBasedChunkGenerator,
        data: &WorldgenData,
        chunk_x: i32,
        chunk_z: i32,
    ) -> Result<Vec<StructureReference>, WorldgenDataError> {
        let (min_x, min_z) = (chunk_x * 16, chunk_z * 16);
        let mut references = Vec::new();
        for source_x in chunk_x - REFERENCE_RANGE..=chunk_x + REFERENCE_RANGE {
            for source_z in chunk_z - REFERENCE_RANGE..=chunk_z + REFERENCE_RANGE {
                for start in self.starts_in_chunk(generator, data, source_x, source_z)? {
                    if start
                        .bounding_box
                        .intersects_columns(min_x, min_z, min_x + 15, min_z + 15)
                    {
                        references.push(StructureReference {
                            structure: start.structure,
                            source: (source_x, source_z),
                        });
                    }
                }
            }
        }
        Ok(references)
    }
}

/// `ChunkGenerator.tryGenerateStructure` and `Structure.generate`: finds the structure's generation point, checks
/// the biome there and makes its pieces.
fn try_generate(
    generator: &NoiseBasedChunkGenerator,
    data: &WorldgenData,
    level_seed: i64,
    entry: &StructureSetEntryDef,
    chunk_x: i32,
    chunk_z: i32,
) -> Result<Option<StructureStart>, WorldgenDataError> {
    let structure = &data.structures[&entry.structure];
    let kind = StructureKind::from_type(&structure.kind).ok_or_else(|| {
        WorldgenDataError::InvalidDefinition {
            context: entry.structure.clone(),
            message: format!("unsupported structure type `{}`", structure.kind),
        }
    })?;
    let mut random = WorldgenRandom::new(LegacyRandom::new(0));
    random.set_large_feature_seed(level_seed, chunk_x, chunk_z);
    let (min_x, min_z) = (chunk_x * 16, chunk_z * 16);
    let first_occupied = |x, z, heightmap| generator.base_height(x, z, heightmap) - 1;
    let (position, pieces) = match kind {
        StructureKind::BuriedTreasure => {
            let y = first_occupied(min_x + 8, min_z + 8, HeightmapType::OceanFloorWg);
            let piece = StructurePiece {
                kind: "minecraft:btp",
                bounding_box: BoundingBox {
                    min: [min_x + 9, 90, min_z + 9],
                    max: [min_x + 9, 90, min_z + 9],
                },
                orientation: None,
                gen_depth: 0,
            };
            ([min_x + 8, y, min_z + 8], vec![piece])
        }
        StructureKind::ScatteredFeature {
            piece,
            width,
            height,
            depth,
            above_sea,
        } => {
            if above_sea {
                let lowest = [(0, 0), (0, depth), (width, 0), (width, depth)]
                    .into_iter()
                    .map(|(dx, dz)| {
                        first_occupied(min_x + dx, min_z + dz, HeightmapType::WorldSurfaceWg)
                    })
                    .min()
                    .expect("four corners");
                if lowest < generator.sea_level() {
                    return Ok(None);
                }
            }
            let y = first_occupied(min_x + 8, min_z + 8, HeightmapType::WorldSurfaceWg);
            let orientation = Direction::random(&mut random);
            let piece = StructurePiece {
                kind: piece,
                bounding_box: BoundingBox::oriented(
                    [min_x, 64, min_z],
                    orientation,
                    width,
                    height,
                    depth,
                ),
                orientation: Some(orientation),
                gen_depth: 0,
            };
            ([min_x + 8, y, min_z + 8], vec![piece])
        }
    };
    if !is_valid_biome(generator, data, &structure.biomes, position)? {
        return Ok(None);
    }
    let mut bounding_box = pieces[0].bounding_box;
    for piece in &pieces[1..] {
        bounding_box.encapsulate(&piece.bounding_box);
    }
    if structure.terrain_adaptation != "none" {
        bounding_box = bounding_box.inflated(TERRAIN_ADAPTATION_MARGIN);
    }
    Ok(Some(StructureStart {
        structure: entry.structure.clone(),
        chunk: (chunk_x, chunk_z),
        pieces,
        bounding_box,
    }))
}

/// `Structure.isValidBiome`: the biome at the generation point, searched without a last result because vanilla's
/// depends on what its worker thread searched before.
fn is_valid_biome(
    generator: &NoiseBasedChunkGenerator,
    data: &WorldgenData,
    biomes: &[String],
    [x, y, z]: [i32; 3],
) -> Result<bool, WorldgenDataError> {
    let mut noise_chunk = generator.create_noise_chunk(x >> 4, z >> 4);
    let target = noise_chunk.sample_climate(x >> 2, y >> 2, z >> 2);
    let biome = generator.biome_source().get_noise_biome(target, &mut None);
    Ok(data.resolve_biomes(biomes)?.contains(&biome))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_oriented_box_swaps_width_and_depth_along_x() {
        let north = BoundingBox::oriented([0, 64, 0], Direction::North, 12, 10, 15);
        assert_eq!(north.max, [11, 73, 14]);
        let east = BoundingBox::oriented([0, 64, 0], Direction::East, 12, 10, 15);
        assert_eq!(east.max, [14, 73, 11]);
    }

    #[test]
    fn directions_save_their_2d_values_and_are_drawn_in_vanilla_order() {
        let values = Direction::HORIZONTAL.map(Direction::data_value);
        assert_eq!(values, [2, 3, 0, 1]);
        let mut random = LegacyRandom::new(42);
        let drawn: Vec<_> = (0..8).map(|_| Direction::random(&mut random)).collect();
        let mut expected = LegacyRandom::new(42);
        let indices: Vec<_> = (0..8)
            .map(|_| Direction::HORIZONTAL[expected.next_int_bounded(4) as usize])
            .collect();
        assert_eq!(drawn, indices);
    }

    #[test]
    fn a_box_intersects_columns_it_overlaps() {
        let b = BoundingBox {
            min: [10, 0, 10],
            max: [20, 5, 20],
        };
        assert!(b.intersects_columns(0, 0, 15, 15));
        assert!(b.intersects_columns(20, 20, 30, 30));
        assert!(!b.intersects_columns(21, 0, 30, 15));
    }
}
