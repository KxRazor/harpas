//! Jigsaw structure blocks in one chunk: vanilla's `StructureStart.placeInChunk` for pool element pieces, compared
//! with vanilla on carved chunks of real starts (`jigsaw_chunk_parity`).
//!
//! The pieces that reach the chunk place their pool elements in order, sharing the random source vanilla seeds
//! from the chunk's decoration seed and the structure's index in its decoration step. Template blocks are
//! clipped to the chunk. A feature element writes into the eight neighboring chunks too; those writes stay in a
//! scratch copy for the rest of the pass, since only the chunk itself is kept, until structures run in the
//! FEATURES schedule. Terrain more than a chunk away reads as air and its heights as the world's bottom, as in a
//! generation region whose farther chunks have not started their terrain yet.

use std::collections::HashMap;

use harpas_core::nbt::{Compound, Nbt};
use harpas_core::random::{WorldgenRandom, XoroshiroRandom};

use super::{BoundingBox, SingleElementPlacement, StructureStart};
use crate::configured::{FeatureCompiler, place_placed};
use crate::data::{PoolElementDef, WorldgenData, WorldgenDataError};
use crate::feature::{FeatureWorld, RegionFeatureWorld};
use crate::generator::{GeneratedChunk, HeightmapType};
use crate::processor::ProcessorWorld;
use crate::template::StructureTemplates;

/// `GenerationStep.Decoration`, in vanilla's order.
const STEPS: [&str; 11] = [
    "raw_generation",
    "lakes",
    "local_modifications",
    "underground_structures",
    "surface_structures",
    "strongholds",
    "underground_ores",
    "underground_decoration",
    "fluid_springs",
    "vegetal_decoration",
    "top_layer_modification",
];

/// A structure's decoration step and its index among the structures of that step, in registry order, which
/// together seed the random source its blocks are placed with.
pub(crate) fn decoration_slot(data: &WorldgenData, structure: &str) -> Option<(i32, i32)> {
    let step = &data.structures.get(structure)?.step;
    let ordinal = STEPS.iter().position(|name| name == step)?;
    let index = data
        .structures
        .iter()
        .filter(|(id, other)| other.step == *step && id.as_str() < structure)
        .count();
    Some((index as i32, ordinal as i32))
}

impl StructureStart {
    /// Places the pool elements of the jigsaw pieces that reach `chunk`, and returns the block entities they
    /// create there.
    ///
    /// `neighbor` gives the carved chunks around the target, which pieces read and feature elements write.
    /// Starts of other structures place nothing.
    pub fn place_jigsaw_in_chunk<'n>(
        &self,
        chunk: &mut GeneratedChunk,
        seed: i64,
        data: &WorldgenData,
        templates: &StructureTemplates,
        neighbor: impl Fn(i32, i32) -> Option<&'n GeneratedChunk>,
    ) -> Result<Vec<Compound>, WorldgenDataError> {
        let Some(first) = self.pieces.first() else {
            return Ok(Vec::new());
        };
        if data
            .structures
            .get(&self.structure)
            .is_none_or(|structure| structure.jigsaw.is_none())
        {
            return Ok(Vec::new());
        }
        let Some((index, step)) = decoration_slot(data, &self.structure) else {
            return Ok(Vec::new());
        };
        let (chunk_x, chunk_z) = (chunk.chunk_x(), chunk.chunk_z());
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(0));
        let decoration = random.set_decoration_seed(seed, chunk_x * 16, chunk_z * 16);
        random.set_feature_seed(decoration, index, step);
        let pivot = pivot(&first.bounding_box);
        let mut region = Region {
            scratch: HashMap::new(),
            neighbor,
            min_y: chunk.min_y(),
        };
        let mut features = FeatureCompiler::new(data);
        let mut entities = Vec::new();
        for piece in &self.pieces {
            let Some(jigsaw) = &piece.jigsaw else {
                continue;
            };
            if !piece.bounding_box.intersects_columns(
                chunk_x * 16,
                chunk_z * 16,
                chunk_x * 16 + 15,
                chunk_z * 16 + 15,
            ) {
                continue;
            }
            if let PoolElementDef::Feature { feature, .. } = &jigsaw.element {
                // A feature that is not ported yet, the ancient city's sculk patches, places nothing.
                let Some(compiled) = features.placed(feature)? else {
                    continue;
                };
                region.with_feature_world(chunk, seed, |world| {
                    place_placed(world, &compiled, &mut random, jigsaw.position);
                });
                continue;
            }
            entities.extend(
                SingleElementPlacement {
                    data,
                    templates,
                    seed,
                    position: jigsaw.position,
                    pivot,
                    rotation: jigsaw.rotation,
                    ignore_waterlogging: piece.save_data.get("liquid_settings")
                        == Some(&Nbt::from("ignore_waterlogging")),
                }
                .place(&jigsaw.element, chunk, &mut region, &mut random)?,
            );
        }
        Ok(entities)
    }
}

/// The processors' reference position: the center of the first piece's columns at its lowest Y.
fn pivot(bounds: &BoundingBox) -> [i32; 3] {
    let center = |axis: usize| bounds.min[axis] + (bounds.max[axis] - bounds.min[axis] + 1) / 2;
    [center(0), bounds.min[1], center(2)]
}

/// The chunks around the target: the carved neighbors, or their scratch copies once a feature wrote to them.
struct Region<F> {
    scratch: HashMap<(i32, i32), GeneratedChunk>,
    neighbor: F,
    min_y: i32,
}

impl<'n, F: Fn(i32, i32) -> Option<&'n GeneratedChunk>> Region<F> {
    fn chunk<'a>(&'a self, x: i32, z: i32) -> Option<&'a GeneratedChunk>
    where
        'n: 'a,
    {
        let key = (x >> 4, z >> 4);
        self.scratch
            .get(&key)
            .or_else(|| (self.neighbor)(key.0, key.1))
    }

    /// Runs `place` in the 3x3 chunks around `target`, then takes the target back and keeps the neighbors'
    /// writes for the rest of the pass.
    fn with_feature_world(
        &mut self,
        target: &mut GeneratedChunk,
        seed: i64,
        place: impl FnOnce(&mut RegionFeatureWorld),
    ) {
        let (x, z) = (target.chunk_x(), target.chunk_z());
        let placeholder = GeneratedChunk::new(x, z, target.min_y(), target.height(), 1);
        let mut chunks = vec![std::mem::replace(target, placeholder)];
        for dx in -1..=1 {
            for dz in -1..=1 {
                let key = (x + dx, z + dz);
                if key == (x, z) {
                    continue;
                }
                let copy = self
                    .scratch
                    .remove(&key)
                    .or_else(|| (self.neighbor)(key.0, key.1).cloned());
                chunks.extend(copy);
            }
        }
        let mut world =
            RegionFeatureWorld::with_stored_biomes(chunks, seed).expect("chunks of one height");
        world.start_region(seed, x, z);
        place(&mut world);
        for chunk in world.into_chunks() {
            if (chunk.chunk_x(), chunk.chunk_z()) == (x, z) {
                *target = chunk;
            } else {
                self.scratch
                    .insert((chunk.chunk_x(), chunk.chunk_z()), chunk);
            }
        }
    }
}

impl<'n, F: Fn(i32, i32) -> Option<&'n GeneratedChunk>> ProcessorWorld for Region<F> {
    fn block_at(&mut self, [x, y, z]: [i32; 3]) -> u32 {
        self.chunk(x, z).map_or(0, |chunk| {
            chunk.block_state((x & 15) as usize, y, (z & 15) as usize)
        })
    }

    fn heightmap(&mut self, kind: HeightmapType, x: i32, z: i32) -> i32 {
        self.chunk(x, z).map_or(self.min_y, |chunk| {
            chunk
                .heightmaps()
                .get(kind, (x & 15) as usize, (z & 15) as usize)
        })
    }
}
