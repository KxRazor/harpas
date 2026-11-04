//! Which structures start in a chunk and the pieces they start with: vanilla's `ChunkGenerator.createStructures`,
//! `Structure.generate` and the pieces of each structure type.

use harpas_core::nbt::Compound;
use harpas_core::random::{LegacyRandom, RandomSource, WorldgenRandom};

use super::{StructureState, jigsaw, mineshaft, monument, ruined_portal, stronghold, templated};
use crate::data::{
    JigsawDef, OceanRuinDef, PoolElementDef, RuinedPortalSetupDef, StructureDef,
    StructureSetEntryDef, WorldgenData, WorldgenDataError,
};
use crate::generator::{HeightmapType, NoiseBasedChunkGenerator};
use crate::template::StructureTemplates;

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
    /// The box between two opposite corners (`BoundingBox.fromCorners`).
    pub(super) fn from_corners(a: [i32; 3], b: [i32; 3]) -> Self {
        Self {
            min: [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])],
            max: [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])],
        }
    }

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

    /// Whether two boxes share a block (`BoundingBox.intersects`).
    pub(super) const fn intersects(&self, other: &Self) -> bool {
        self.max[0] >= other.min[0]
            && self.min[0] <= other.max[0]
            && self.max[1] >= other.min[1]
            && self.min[1] <= other.max[1]
            && self.max[2] >= other.min[2]
            && self.min[2] <= other.max[2]
    }

    /// The block at the middle of the box (`BoundingBox.getCenter`).
    pub(super) const fn center(&self) -> [i32; 3] {
        [
            self.min[0] + (self.max[0] - self.min[0] + 1) / 2,
            self.min[1] + (self.max[1] - self.min[1] + 1) / 2,
            self.min[2] + (self.max[2] - self.min[2] + 1) / 2,
        ]
    }

    pub(super) fn encapsulate(&mut self, other: &Self) {
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

/// How far to move pieces so that they end below the sea level, by `margin` blocks at least
/// (`StructurePiecesBuilder.moveBelowSeaLevel`).
pub(super) fn below_sea_level_offset(
    bounds: &BoundingBox,
    random: &mut impl RandomSource,
    sea_level: i32,
    min_y: i32,
    margin: i32,
) -> i32 {
    let max_y = sea_level - margin;
    let mut top = bounds.max[1] - bounds.min[1] + 1 + min_y + 1;
    if top < max_y {
        top += random.next_int_bounded(max_y - top);
    }
    top - bounds.max[1]
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
    pub(super) fn random(random: &mut impl RandomSource) -> Self {
        Self::HORIZONTAL[random.next_int_bounded(4) as usize]
    }
}

/// One piece of a structure, as saved in its start (`StructurePiece`).
#[derive(Debug, Clone, PartialEq)]
pub struct StructurePiece {
    /// The piece type, such as `minecraft:tedp` for a desert pyramid.
    pub kind: String,
    /// The blocks the piece covers.
    pub bounding_box: BoundingBox,
    /// The direction the piece faces, if it has one.
    pub orientation: Option<Direction>,
    /// How many pieces lie between this one and the first.
    pub gen_depth: i32,
    /// Where a jigsaw piece connects to its neighbors, which the terrain blends towards.
    pub junctions: Vec<JigsawJunction>,
    /// What a jigsaw piece places and where (`PoolElementStructurePiece`).
    pub jigsaw: Option<JigsawPiece>,
    /// What a piece made of one template places and where (`TemplateStructurePiece`).
    pub template: Option<TemplatePiece>,
    /// Piece-specific NBT needed to resume placement, including unknown fields of saved pieces.
    pub save_data: Compound,
}

/// The template a piece places, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplatePiece {
    /// The template's resource location.
    pub template: String,
    /// Where its origin is placed.
    pub position: [i32; 3],
    /// How it is turned.
    pub rotation: crate::template::Rotation,
    /// How it is reflected.
    pub mirror: crate::template::Mirror,
    /// The point it is turned around.
    pub pivot: [i32; 3],
}

/// The pool element a jigsaw piece places, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JigsawPiece {
    /// The pool element.
    pub element: PoolElementDef,
    /// Where its template's origin is placed.
    pub position: [i32; 3],
    /// How its template is turned.
    pub rotation: crate::template::Rotation,
    /// The height of the ground above the bottom of the piece, which terrain adaptation follows.
    pub ground_level_delta: i32,
}

/// Where a jigsaw piece connects to another (`JigsawJunction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JigsawJunction {
    /// The X, ground Y and Z of the connection.
    pub source: [i32; 3],
    /// How much higher the other piece is.
    pub delta_y: i32,
    /// Whether the other piece is rigid rather than terrain matching.
    pub destination_rigid: bool,
}

/// A structure that starts in a chunk (`StructureStart`).
#[derive(Debug, Clone, PartialEq)]
pub struct StructureStart {
    /// The structure's resource location.
    pub structure: String,
    /// The chunk it starts in.
    pub chunk: (i32, i32),
    /// How many locator references have been claimed (not the number of chunks it overlaps).
    pub references: i32,
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
enum StructureKind<'d> {
    BuriedTreasure,
    Stronghold,
    OceanMonument,
    Jigsaw(&'d JigsawDef),
    Igloo,
    Shipwreck {
        beached: bool,
    },
    OceanRuin(OceanRuinDef),
    RuinedPortal(&'d [RuinedPortalSetupDef]),
    Mineshaft {
        mesa: bool,
    },
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

impl<'d> StructureKind<'d> {
    fn of(structure: &'d StructureDef) -> Option<Self> {
        let scattered = |piece, width, height, depth, above_sea| Self::ScatteredFeature {
            piece,
            width,
            height,
            depth,
            above_sea,
        };
        Some(match structure.kind.as_str() {
            "minecraft:buried_treasure" => Self::BuriedTreasure,
            "minecraft:jigsaw" => match &structure.jigsaw {
                Some(jigsaw) if jigsaw::is_supported(jigsaw) => Self::Jigsaw(jigsaw),
                _ => return None,
            },
            "minecraft:stronghold" => Self::Stronghold,
            "minecraft:ocean_monument" => Self::OceanMonument,
            "minecraft:igloo" => Self::Igloo,
            "minecraft:shipwreck" => Self::Shipwreck {
                beached: structure.is_beached,
            },
            "minecraft:ocean_ruin" => Self::OceanRuin(structure.ocean_ruin?),
            "minecraft:mineshaft" => Self::Mineshaft {
                mesa: structure.is_mesa_mineshaft,
            },
            "minecraft:ruined_portal" if !structure.ruined_portal_setups.is_empty() => {
                Self::RuinedPortal(&structure.ruined_portal_setups)
            }
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
                    .is_some_and(|structure| StructureKind::of(structure).is_some())
            })
        })
    }

    /// The structures that start in a chunk, at most one for each structure set, in set order
    /// (`ChunkGenerator.createStructures`). Sets with structure types that Harpas cannot make yet are skipped.
    pub fn starts_in_chunk(
        &self,
        generator: &NoiseBasedChunkGenerator,
        data: &WorldgenData,
        templates: &StructureTemplates,
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
                    templates,
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
                if let Some(start) = try_generate(
                    generator,
                    data,
                    templates,
                    self.level_seed,
                    selected,
                    chunk_x,
                    chunk_z,
                )? {
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
    /// This makes the starts of every chunk in reach again; [`Self::references_from`] takes them from elsewhere.
    pub fn references_in_chunk(
        &self,
        generator: &NoiseBasedChunkGenerator,
        data: &WorldgenData,
        templates: &StructureTemplates,
        chunk_x: i32,
        chunk_z: i32,
    ) -> Result<Vec<StructureReference>, WorldgenDataError> {
        Self::references_from(chunk_x, chunk_z, |x, z| {
            self.starts_in_chunk(generator, data, templates, x, z)
        })
    }

    /// The references of a chunk, with the starts of each chunk in reach from `starts_of`.
    pub fn references_from(
        chunk_x: i32,
        chunk_z: i32,
        mut starts_of: impl FnMut(i32, i32) -> Result<Vec<StructureStart>, WorldgenDataError>,
    ) -> Result<Vec<StructureReference>, WorldgenDataError> {
        let (min_x, min_z) = (chunk_x * 16, chunk_z * 16);
        let mut references = Vec::new();
        for source_x in chunk_x - REFERENCE_RANGE..=chunk_x + REFERENCE_RANGE {
            for source_z in chunk_z - REFERENCE_RANGE..=chunk_z + REFERENCE_RANGE {
                for start in starts_of(source_x, source_z)? {
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
#[allow(clippy::too_many_arguments)]
fn try_generate(
    generator: &NoiseBasedChunkGenerator,
    data: &WorldgenData,
    templates: &StructureTemplates,
    level_seed: i64,
    entry: &StructureSetEntryDef,
    chunk_x: i32,
    chunk_z: i32,
) -> Result<Option<StructureStart>, WorldgenDataError> {
    let structure = &data.structures[&entry.structure];
    let kind =
        StructureKind::of(structure).ok_or_else(|| WorldgenDataError::InvalidDefinition {
            context: entry.structure.clone(),
            message: format!("unsupported structure type `{}`", structure.kind),
        })?;
    let mut random = WorldgenRandom::new(LegacyRandom::new(0));
    random.set_large_feature_seed(level_seed, chunk_x, chunk_z);
    let (min_x, min_z) = (chunk_x * 16, chunk_z * 16);
    let first_occupied = |x, z, heightmap| generator.base_height(x, z, heightmap) - 1;
    let mut jigsaw_start = None;
    // The pieces of structures that make them while finding their generation point.
    let mut made_pieces = None;
    let position = match kind {
        StructureKind::Jigsaw(jigsaw) => {
            let Some(start) = jigsaw::start(
                generator,
                data,
                templates,
                jigsaw,
                level_seed,
                &mut random,
                chunk_x,
                chunk_z,
            )?
            else {
                return Ok(None);
            };
            jigsaw_start.insert(start).position
        }
        StructureKind::BuriedTreasure => [
            min_x + 8,
            first_occupied(min_x + 8, min_z + 8, HeightmapType::OceanFloorWg),
            min_z + 8,
        ],
        StructureKind::Stronghold => [min_x, 0, min_z],
        StructureKind::OceanMonument => {
            if !monument::has_valid_surroundings(generator, data, min_x, min_z)? {
                return Ok(None);
            }
            [
                min_x + 8,
                first_occupied(min_x + 8, min_z + 8, HeightmapType::OceanFloorWg),
                min_z + 8,
            ]
        }
        StructureKind::RuinedPortal(setups) => {
            let (position, piece) = ruined_portal::start(
                generator,
                data,
                templates,
                setups,
                &mut random,
                min_x,
                min_z,
            )?;
            made_pieces = Some(vec![piece]);
            position
        }
        StructureKind::Mineshaft { mesa } => {
            let (position, pieces) = mineshaft::start(generator, &mut random, mesa, min_x, min_z);
            made_pieces = Some(pieces);
            position
        }
        StructureKind::Igloo | StructureKind::Shipwreck { beached: true } => [
            min_x + 8,
            first_occupied(min_x + 8, min_z + 8, HeightmapType::WorldSurfaceWg),
            min_z + 8,
        ],
        StructureKind::Shipwreck { beached: false } | StructureKind::OceanRuin(_) => [
            min_x + 8,
            first_occupied(min_x + 8, min_z + 8, HeightmapType::OceanFloorWg),
            min_z + 8,
        ],
        StructureKind::ScatteredFeature {
            width,
            depth,
            above_sea,
            ..
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
            [
                min_x + 8,
                first_occupied(min_x + 8, min_z + 8, HeightmapType::WorldSurfaceWg),
                min_z + 8,
            ]
        }
    };
    if !is_valid_biome(generator, data, &structure.biomes, position)? {
        return Ok(None);
    }
    let pieces = match kind {
        StructureKind::Jigsaw(_) => jigsaw_start.expect("a jigsaw start").pieces(&mut random)?,
        StructureKind::BuriedTreasure => vec![StructurePiece {
            kind: "minecraft:btp".to_string(),
            bounding_box: BoundingBox {
                min: [min_x + 9, 90, min_z + 9],
                max: [min_x + 9, 90, min_z + 9],
            },
            orientation: None,
            gen_depth: 0,
            junctions: Vec::new(),
            jigsaw: None,
            template: None,
            save_data: Default::default(),
        }],
        StructureKind::Stronghold => stronghold::pieces(
            level_seed,
            chunk_x,
            chunk_z,
            generator.sea_level(),
            generator.min_y(),
        ),
        StructureKind::OceanMonument => vec![monument::piece(&mut random, min_x, min_z)],
        StructureKind::Igloo => templated::igloo(templates, &mut random, min_x, min_z)?,
        StructureKind::Shipwreck { beached } => {
            templated::shipwreck(templates, &mut random, beached, min_x, min_z)?
        }
        StructureKind::OceanRuin(ruin) => {
            templated::ocean_ruin(templates, &mut random, ruin, min_x, min_z)?
        }
        StructureKind::RuinedPortal(_) | StructureKind::Mineshaft { .. } => {
            made_pieces.expect("pieces made with the generation point")
        }
        StructureKind::ScatteredFeature {
            piece,
            width,
            height,
            depth,
            ..
        } => {
            let orientation = Direction::random(&mut random);
            let mut save_data = Compound::new();
            for (key, value) in [
                ("Width", width),
                ("Height", height),
                ("Depth", depth),
                ("HPos", -1),
            ] {
                save_data.insert(key, value);
            }
            let flags: &[&str] = match piece {
                "minecraft:tedp" => &[
                    "hasPlacedChest0",
                    "hasPlacedChest1",
                    "hasPlacedChest2",
                    "hasPlacedChest3",
                ],
                "minecraft:tejp" => &[
                    "placedMainChest",
                    "placedHiddenChest",
                    "placedTrap1",
                    "placedTrap2",
                ],
                "minecraft:tesh" => &["Witch", "Cat"],
                _ => &[],
            };
            for flag in flags {
                save_data.insert(*flag, 0_i8);
            }
            vec![StructurePiece {
                kind: piece.to_string(),
                bounding_box: BoundingBox::oriented(
                    [min_x, 64, min_z],
                    orientation,
                    width,
                    height,
                    depth,
                ),
                orientation: Some(orientation),
                gen_depth: 0,
                junctions: Vec::new(),
                jigsaw: None,
                template: None,
                save_data,
            }]
        }
    };
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
        references: 0,
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
