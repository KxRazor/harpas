//! Where structures start: vanilla's `StructurePlacement`, `ChunkGeneratorStructureState` and, in [`start`], the
//! structures themselves.

use std::collections::{HashMap, HashSet};
use std::f64::consts::PI;

use harpas_core::math::round_f64;
use harpas_core::random::{LegacyRandom, RandomSource, WorldgenRandom};

use crate::data::{
    FrequencyReductionMethod, StructurePlacementDef, StructurePlacementKind, WorldgenData,
    WorldgenDataError,
};
use crate::generator::NoiseBasedChunkGenerator;

mod jigsaw;
mod ruined_portal;
mod start;
mod stronghold;
mod templated;

pub use start::{
    BoundingBox, Direction, JigsawJunction, JigsawPiece, StructurePiece, StructureReference,
    StructureStart, TemplatePiece,
};

/// Salt of `StructurePlacement.legacyArbitrarySaltProbabilityReducer`.
const LEGACY_TYPE_2_SALT: i32 = 10_387_320;
/// How far a ring position moves to reach a preferred biome, in blocks.
const RING_BIOME_SEARCH_RADIUS: i32 = 112;

/// The structure sets of a world and their ring positions (`ChunkGeneratorStructureState`).
#[derive(Debug, Clone)]
pub struct StructureState {
    level_seed: i64,
    placements: HashMap<String, StructurePlacementDef>,
    possible_sets: Vec<String>,
    rings: HashMap<String, HashSet<(i32, i32)>>,
    ring_positions: HashMap<String, Vec<(i32, i32)>>,
}

impl StructureState {
    /// Finds the structure sets that `generator`'s biomes allow and computes their ring positions, as vanilla
    /// does for a normal world.
    pub fn new(
        level_seed: i64,
        data: &WorldgenData,
        generator: &NoiseBasedChunkGenerator,
    ) -> Result<Self, WorldgenDataError> {
        let possible_biomes = generator.biome_source().possible_biomes();
        let mut possible_sets = Vec::new();
        let mut ring_positions = HashMap::new();
        for (name, set) in &data.structure_sets {
            let mut possible = false;
            for entry in &set.structures {
                let structure = data.structures.get(&entry.structure).ok_or_else(|| {
                    WorldgenDataError::InvalidDefinition {
                        context: name.clone(),
                        message: format!("unknown structure `{}`", entry.structure),
                    }
                })?;
                possible |= !data
                    .resolve_biomes(&structure.biomes)?
                    .is_disjoint(&possible_biomes);
            }
            if !possible {
                continue;
            }
            possible_sets.push(name.clone());
            if let StructurePlacementKind::ConcentricRings {
                distance,
                spread,
                count,
                preferred_biomes,
            } = &set.placement.kind
            {
                let preferred = data.resolve_biomes(preferred_biomes)?;
                ring_positions.insert(
                    name.clone(),
                    generate_ring_positions(
                        generator, level_seed, *distance, *spread, *count, &preferred,
                    ),
                );
            }
        }
        let rings = ring_positions
            .iter()
            .map(|(name, positions): (&String, &Vec<_>)| {
                (name.clone(), positions.iter().copied().collect())
            })
            .collect();
        let placements = data
            .structure_sets
            .iter()
            .map(|(name, set)| (name.clone(), set.placement.clone()))
            .collect();
        Ok(Self {
            level_seed,
            placements,
            possible_sets,
            rings,
            ring_positions,
        })
    }

    /// The structure sets that can generate in this world (`possibleStructureSets`), in lexicographical order.
    pub fn possible_sets(&self) -> &[String] {
        &self.possible_sets
    }

    /// The chunks of a concentric rings set, in vanilla's order (`getRingPositionsFor`).
    pub fn ring_positions(&self, set: &str) -> Option<&[(i32, i32)]> {
        self.ring_positions.get(set).map(Vec::as_slice)
    }

    /// Whether a structure of `set` may start in a chunk (`StructurePlacement.isStructureChunk`).
    ///
    /// # Panics
    ///
    /// Panics if `set` is not a structure set of the data this state was made from.
    pub fn is_structure_chunk(&self, set: &str, chunk_x: i32, chunk_z: i32) -> bool {
        let placement = &self.placements[set];
        self.is_placement_chunk(set, placement, chunk_x, chunk_z)
            && (placement.common.frequency >= 1.0
                || should_generate(self.level_seed, placement, chunk_x, chunk_z))
            && placement.common.exclusion_zone.as_ref().is_none_or(|zone| {
                !self.has_structure_chunk_in_range(
                    &zone.other_set,
                    chunk_x,
                    chunk_z,
                    zone.chunk_count,
                )
            })
    }

    fn is_placement_chunk(
        &self,
        set: &str,
        placement: &StructurePlacementDef,
        chunk_x: i32,
        chunk_z: i32,
    ) -> bool {
        match &placement.kind {
            StructurePlacementKind::RandomSpread { .. } => {
                potential_structure_chunk(self.level_seed, placement, chunk_x, chunk_z)
                    == (chunk_x, chunk_z)
            }
            StructurePlacementKind::ConcentricRings { .. } => self
                .rings
                .get(set)
                .is_some_and(|positions| positions.contains(&(chunk_x, chunk_z))),
        }
    }

    fn has_structure_chunk_in_range(
        &self,
        set: &str,
        chunk_x: i32,
        chunk_z: i32,
        range: i32,
    ) -> bool {
        (-range..=range).any(|dx| {
            (-range..=range).any(|dz| self.is_structure_chunk(set, chunk_x + dx, chunk_z + dz))
        })
    }
}

/// The candidate chunk of the region holding a chunk (`RandomSpreadStructurePlacement.getPotentialStructureChunk`).
fn potential_structure_chunk(
    level_seed: i64,
    placement: &StructurePlacementDef,
    chunk_x: i32,
    chunk_z: i32,
) -> (i32, i32) {
    let StructurePlacementKind::RandomSpread {
        spacing,
        separation,
        triangular,
    } = placement.kind
    else {
        panic!("only random spread placements have regions");
    };
    let region_x = chunk_x.div_euclid(spacing);
    let region_z = chunk_z.div_euclid(spacing);
    let mut random = WorldgenRandom::new(LegacyRandom::new(0));
    random.set_large_feature_with_salt(level_seed, region_x, region_z, placement.common.salt);
    let limit = spacing - separation;
    let mut offset = || {
        if triangular {
            (random.next_int_bounded(limit) + random.next_int_bounded(limit)) / 2
        } else {
            random.next_int_bounded(limit)
        }
    };
    let offset_x = offset();
    let offset_z = offset();
    (region_x * spacing + offset_x, region_z * spacing + offset_z)
}

/// `StructurePlacement.FrequencyReductionMethod.shouldGenerate`.
fn should_generate(
    level_seed: i64,
    placement: &StructurePlacementDef,
    chunk_x: i32,
    chunk_z: i32,
) -> bool {
    let frequency = placement.common.frequency;
    let mut random = WorldgenRandom::new(LegacyRandom::new(0));
    match placement.common.frequency_reduction_method {
        FrequencyReductionMethod::Default => {
            random.set_large_feature_with_salt(level_seed, chunk_x, chunk_z, placement.common.salt);
            random.next_float() < frequency
        }
        FrequencyReductionMethod::LegacyType1 => {
            let (region_x, region_z) = (chunk_x >> 4, chunk_z >> 4);
            random.set_seed(i64::from(region_x ^ (region_z << 4)) ^ level_seed);
            random.next_int();
            random.next_int_bounded((1.0 / frequency) as i32) == 0
        }
        FrequencyReductionMethod::LegacyType2 => {
            random.set_large_feature_with_salt(level_seed, chunk_x, chunk_z, LEGACY_TYPE_2_SALT);
            random.next_float() < frequency
        }
        FrequencyReductionMethod::LegacyType3 => {
            random.set_large_feature_seed(level_seed, chunk_x, chunk_z);
            random.next_double() < f64::from(frequency)
        }
    }
}

/// `ChunkGeneratorStructureState.generateRingPositions`. Ties between biomes depend on what vanilla's worker thread
/// searched before, so each position's search starts without a last result.
fn generate_ring_positions(
    generator: &NoiseBasedChunkGenerator,
    level_seed: i64,
    distance: i32,
    mut spread: i32,
    count: i32,
    preferred: &HashSet<u32>,
) -> Vec<(i32, i32)> {
    let mut noise_chunk = generator.create_noise_chunk(0, 0);
    let mut random = LegacyRandom::new(level_seed);
    let mut angle = random.next_double() * PI * 2.0;
    let mut position_in_circle = 0;
    let mut circle = 0;
    let mut positions = Vec::with_capacity(count as usize);
    for index in 0..count {
        let ring_distance = f64::from(4 * distance + distance * circle * 6)
            + (random.next_double() - 0.5) * (f64::from(distance) * 2.5);
        let initial_x = round_f64(angle.cos() * ring_distance) as i32;
        let initial_z = round_f64(angle.sin() * ring_distance) as i32;
        let mut biome_search = random.fork();
        let found = generator.biome_source().find_biome_horizontal(
            &mut noise_chunk,
            [(initial_x << 4) + 8, 0, (initial_z << 4) + 8],
            RING_BIOME_SEARCH_RADIUS,
            preferred,
            &mut biome_search,
        );
        positions.push(found.map_or((initial_x, initial_z), |(x, z)| (x >> 4, z >> 4)));
        angle += PI * 2.0 / f64::from(spread);
        position_in_circle += 1;
        if position_in_circle == spread {
            circle += 1;
            position_in_circle = 0;
            spread += 2 * spread / (circle + 1);
            spread = spread.min(count - index);
            angle += random.next_double() * PI * 2.0;
        }
    }
    positions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::StructurePlacementCommon;

    fn spread(spacing: i32, separation: i32, triangular: bool) -> StructurePlacementDef {
        StructurePlacementDef {
            common: StructurePlacementCommon {
                salt: 10_387_312,
                frequency: 1.0,
                frequency_reduction_method: FrequencyReductionMethod::Default,
                exclusion_zone: None,
                locate_offset: [0, 0, 0],
            },
            kind: StructurePlacementKind::RandomSpread {
                spacing,
                separation,
                triangular,
            },
        }
    }

    #[test]
    fn a_region_has_one_candidate_that_keeps_the_separation() {
        for triangular in [false, true] {
            let placement = spread(34, 8, triangular);
            for (x, z) in [(0, 0), (-1, -1), (33, 34), (-35, 1_000_000)] {
                let candidate = potential_structure_chunk(42, &placement, x, z);
                let (region_x, region_z) = (x.div_euclid(34), z.div_euclid(34));
                assert!((0..26).contains(&(candidate.0 - region_x * 34)));
                assert!((0..26).contains(&(candidate.1 - region_z * 34)));
                assert_eq!(
                    candidate,
                    potential_structure_chunk(42, &placement, region_x * 34, region_z * 34)
                );
            }
        }
    }

    #[test]
    fn frequency_reduction_keeps_about_the_given_share_of_chunks() {
        for method in [
            FrequencyReductionMethod::Default,
            FrequencyReductionMethod::LegacyType2,
            FrequencyReductionMethod::LegacyType3,
        ] {
            let mut placement = spread(1, 0, false);
            placement.common.frequency = 0.2;
            placement.common.frequency_reduction_method = method;
            let kept = (0..200)
                .flat_map(|x| (0..200).map(move |z| (x, z)))
                .filter(|&(x, z)| should_generate(42, &placement, x, z))
                .count();
            assert!((6_000..10_000).contains(&kept), "{method:?} kept {kept}");
        }
    }

    #[test]
    fn structure_sets_are_parsed_with_their_defaults() {
        let data = WorldgenData::from_json_entries([
            (
                "data/minecraft/worldgen/structure_set/a.json",
                r##"{"structures": [{"structure": "minecraft:s", "weight": 2}],
                    "placement": {"type": "minecraft:random_spread", "salt": 1, "spacing": 8,
                                  "separation": 2}}"##,
            ),
            (
                "data/minecraft/worldgen/structure_set/b.json",
                r##"{"structures": [],
                    "placement": {"type": "minecraft:concentric_rings", "salt": 0, "distance": 32,
                                  "spread": 3, "count": 128, "preferred_biomes": "#minecraft:t",
                                  "frequency": 0.5, "frequency_reduction_method": "legacy_type_1",
                                  "exclusion_zone": {"other_set": "minecraft:a", "chunk_count": 10},
                                  "locate_offset": [9, 0, 9]}}"##,
            ),
            (
                "data/minecraft/worldgen/structure/s.json",
                r##"{"type": "minecraft:swamp_hut", "biomes": ["minecraft:plains"]}"##,
            ),
        ])
        .unwrap();
        let a = &data.structure_sets["minecraft:a"];
        assert_eq!(a.structures[0].weight, 2);
        assert_eq!(a.placement.common.frequency, 1.0);
        assert_eq!(
            a.placement.kind,
            StructurePlacementKind::RandomSpread {
                spacing: 8,
                separation: 2,
                triangular: false
            }
        );
        let b = &data.structure_sets["minecraft:b"].placement;
        assert_eq!(
            b.common.frequency_reduction_method,
            FrequencyReductionMethod::LegacyType1
        );
        assert_eq!(b.common.locate_offset, [9, 0, 9]);
        assert_eq!(b.common.exclusion_zone.as_ref().unwrap().chunk_count, 10);
        assert_eq!(data.structures["minecraft:s"].biomes, ["minecraft:plains"]);
        assert_eq!(data.structures["minecraft:s"].kind, "minecraft:swamp_hut");
        assert_eq!(data.structures["minecraft:s"].terrain_adaptation, "none");
    }
}
