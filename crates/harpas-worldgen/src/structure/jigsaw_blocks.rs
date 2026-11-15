//! Jigsaw structure blocks in one chunk's decoration: vanilla's `StructureStart.placeInChunk` for pool element
//! pieces, compared with vanilla on carved chunks of real starts (`jigsaw_chunk_parity`).
//!
//! The pieces that reach the chunk place their pool elements in order, sharing the random source the decoration
//! seeds for the structure. Template blocks are clipped to the chunk; feature elements write into the whole
//! region, as other features do. Terrain beyond the region reads as air and its heights as the world's bottom,
//! as in a generation region whose farther chunks have not started their terrain yet.

use harpas_core::nbt::Nbt;
use harpas_core::random::{RandomSource, WorldgenRandom};

use super::{BoundingBox, SingleElementPlacement, StructureStart};
use crate::configured::{FeatureCompiler, place_placed};
use crate::data::{PoolElementDef, WorldgenData, WorldgenDataError};
use crate::feature::{FeatureWorld, RegionFeatureWorld};
use crate::generator::HeightmapType;
use crate::processor::ProcessorWorld;
use crate::template::StructureTemplates;

/// `GenerationStep.Decoration`, in vanilla's order.
pub(crate) const STEPS: [&str; 11] = [
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
/// together seed the random source its blocks are placed with (`WorldgenRandom.setFeatureSeed`).
pub fn decoration_slot(data: &WorldgenData, structure: &str) -> Option<(i32, i32)> {
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
    /// Whether this is a start of a jigsaw structure, whose blocks [`Self::place_jigsaw_in_region`] places.
    pub fn is_jigsaw(&self) -> bool {
        self.pieces.iter().any(|piece| piece.jigsaw.is_some())
    }

    /// Places the pool elements of the jigsaw pieces that reach the chunk at `chunk_x`, `chunk_z`, which `world`
    /// holds with its neighbors, and stores the block entities they create in that chunk.
    ///
    /// `random` is the decoration's source, seeded for this structure. A feature that is not ported yet, the
    /// ancient city's sculk patches, places nothing.
    #[allow(clippy::too_many_arguments)]
    pub fn place_jigsaw_in_region<R: RandomSource>(
        &self,
        world: &mut RegionFeatureWorld,
        chunk_x: i32,
        chunk_z: i32,
        seed: i64,
        data: &WorldgenData,
        templates: &StructureTemplates,
        features: &mut FeatureCompiler,
        random: &mut WorldgenRandom<R>,
    ) -> Result<(), WorldgenDataError> {
        let Some(first) = self.pieces.first() else {
            return Ok(());
        };
        let pivot = pivot(&first.bounding_box);
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
                if let Some(compiled) = features.placed(feature)? {
                    place_placed(world, &compiled, random, jigsaw.position);
                }
                continue;
            }
            let Some(mut chunk) = world.take(chunk_x, chunk_z) else {
                return Ok(());
            };
            let placed = SingleElementPlacement {
                data,
                templates,
                seed,
                position: jigsaw.position,
                pivot,
                rotation: jigsaw.rotation,
                ignore_waterlogging: piece.save_data.get("liquid_settings")
                    == Some(&Nbt::from("ignore_waterlogging")),
            }
            .place(&jigsaw.element, &mut chunk, &mut Region(world), random);
            world.put_back(chunk);
            for entity in placed? {
                let [x, y, z] = ["x", "y", "z"].map(|key| match entity.get(key) {
                    Some(Nbt::Int(value)) => *value,
                    _ => unreachable!("placed block entities have a position"),
                });
                world.set_block_entity(x, y, z, entity);
            }
        }
        Ok(())
    }
}

/// The processors' reference position: the center of the first piece's columns at its lowest Y.
fn pivot(bounds: &BoundingBox) -> [i32; 3] {
    let center = |axis: usize| bounds.min[axis] + (bounds.max[axis] - bounds.min[axis] + 1) / 2;
    [center(0), bounds.min[1], center(2)]
}

/// The rest of the region while a template is placed in its chunk.
struct Region<'a>(&'a RegionFeatureWorld);

impl ProcessorWorld for Region<'_> {
    fn block_at(&mut self, [x, y, z]: [i32; 3]) -> u32 {
        self.0.block_state(x, y, z)
    }

    fn heightmap(&mut self, kind: HeightmapType, x: i32, z: i32) -> i32 {
        self.0.heightmap(kind, x, z)
    }
}
