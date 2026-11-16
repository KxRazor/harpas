//! Fossils (`FossilFeature`): a bone template and its ore overlay, buried below the ocean floor.
//!
//! The feature draws a rotation and one of its fossils, finds the lowest `OCEAN_FLOOR_WG` height under the
//! template centered on the origin, and buries it 15 to 24 blocks below, but at least 10 blocks above the bottom
//! of the world. It gives up when more than the allowed corners of the template's box are air, water or lava.
//! Otherwise both templates are placed through their processor lists, clipped to the chunk's decoration region,
//! with the feature's random source as the placement random: each placement first draws its palette, then its
//! rotting processors draw from the same source, block by block.

use std::sync::Arc;

use harpas_core::block::block_states;
use harpas_core::random::{RandomSource, WorldgenRandom};
use serde_json::Value;

use crate::data::{WorldgenData, WorldgenDataError};
use crate::feature::FeatureWorld;
use crate::generator::HeightmapType;
use crate::processor::{ProcessorBlock, ProcessorPipeline, ProcessorWorld};
use crate::structure::BoundingBox;
use crate::template::{Rotation, StructureTemplate, StructureTemplates};

/// A compiled `minecraft:fossil` configuration.
pub struct Fossil {
    templates: Arc<StructureTemplates>,
    /// Each fossil's template with its overlay, chosen together.
    structures: Vec<(String, String)>,
    fossil_processors: ProcessorPipeline,
    overlay_processors: ProcessorPipeline,
    max_empty_corners: i32,
}

impl std::fmt::Debug for Fossil {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fossil")
            .field("structures", &self.structures)
            .field("max_empty_corners", &self.max_empty_corners)
            .finish_non_exhaustive()
    }
}

impl Fossil {
    /// Compiles a fossil configuration; None when one of its processor lists is not ported.
    pub(crate) fn compile(
        name: &str,
        config: &Value,
        data: &WorldgenData,
        templates: Arc<StructureTemplates>,
    ) -> Result<Option<Self>, WorldgenDataError> {
        let invalid = |message: String| WorldgenDataError::InvalidDefinition {
            context: name.to_string(),
            message,
        };
        let names = |key: &str| -> Result<Vec<String>, WorldgenDataError> {
            config
                .get(key)
                .and_then(Value::as_array)
                .ok_or_else(|| invalid(format!("fossil has no {key}")))?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(qualified)
                        .ok_or_else(|| invalid(format!("{key} holds a non-string")))
                })
                .collect()
        };
        let fossils = names("fossil_structures")?;
        let overlays = names("overlay_structures")?;
        if fossils.is_empty() || fossils.len() != overlays.len() {
            return Err(invalid(
                "fossil and overlay structure lists differ in length or are empty".to_string(),
            ));
        }
        let processors = |key: &str| -> Result<Option<ProcessorPipeline>, WorldgenDataError> {
            let entries = match config.get(key) {
                Some(Value::String(list)) => data
                    .processor_lists
                    .get(&qualified(list))
                    .ok_or_else(|| invalid(format!("unknown processor list {list}")))?
                    .clone(),
                Some(Value::Object(list)) => list
                    .get("processors")
                    .and_then(Value::as_array)
                    .ok_or_else(|| invalid(format!("{key} has no processors")))?
                    .clone(),
                Some(Value::Array(list)) => list.clone(),
                _ => return Err(invalid(format!("fossil has no {key}"))),
            };
            ProcessorPipeline::compile(name, &entries, data)
        };
        let (Some(fossil_processors), Some(overlay_processors)) = (
            processors("fossil_processors")?,
            processors("overlay_processors")?,
        ) else {
            return Ok(None);
        };
        let max_empty_corners = config
            .get("max_empty_corners_allowed")
            .and_then(Value::as_i64)
            .ok_or_else(|| invalid("fossil has no max_empty_corners_allowed".to_string()))?
            as i32;
        Ok(Some(Self {
            templates,
            structures: fossils.into_iter().zip(overlays).collect(),
            fossil_processors,
            overlay_processors,
            max_empty_corners,
        }))
    }

    fn template(&self, name: &str) -> StructureTemplate {
        // A missing template is created empty, as vanilla's template manager does.
        self.templates
            .get(name)
            .cloned()
            .unwrap_or_else(StructureTemplate::empty)
    }
}

/// Places a fossil at `origin` (`FossilFeature.place`); returns whether it was placed.
pub(crate) fn place_fossil<R: RandomSource>(
    world: &mut impl FeatureWorld,
    fossil: &Fossil,
    random: &mut WorldgenRandom<R>,
    [x, y, z]: [i32; 3],
) -> bool {
    let rotation = Rotation::random(random);
    let index = random.next_int_bounded(fossil.structures.len() as i32) as usize;
    let (fossil_name, overlay_name) = &fossil.structures[index];
    let template = fossil.template(fossil_name);
    let overlay = fossil.template(overlay_name);
    // The chunk holding the origin and one chunk around it, over the world's full height.
    let (chunk_x, chunk_z) = (x.div_euclid(16) * 16, z.div_euclid(16) * 16);
    let clip = BoundingBox {
        min: [chunk_x - 16, world.min_y(), chunk_z - 16],
        max: [
            chunk_x + 31,
            world.min_y() + world.height() - 1,
            chunk_z + 31,
        ],
    };
    let [size_x, _, size_z] = match rotation {
        Rotation::Clockwise90 | Rotation::Counterclockwise90 => {
            [template.size[2], template.size[1], template.size[0]]
        }
        _ => template.size,
    };
    let corner = [x - size_x / 2, z - size_z / 2];
    let mut floor = y;
    for dx in 0..size_x {
        for dz in 0..size_z {
            floor = floor.min(world.heightmap(
                HeightmapType::OceanFloorWg,
                corner[0] + dx,
                corner[1] + dz,
            ));
        }
    }
    let depth = (floor - 15 - random.next_int_bounded(10)).max(world.min_y() + 10);
    let position = zero_position(
        [corner[0], depth, corner[1]],
        rotation,
        template.size[0],
        template.size[2],
    );
    if empty_corners(world, template.bounding_box(position, rotation)) > fossil.max_empty_corners {
        return false;
    }
    place_template(
        world,
        &template,
        &fossil.fossil_processors,
        position,
        rotation,
        clip,
        random,
    );
    place_template(
        world,
        &overlay,
        &fossil.overlay_processors,
        position,
        rotation,
        clip,
        random,
    );
    true
}

/// `StructureTemplate.getZeroPositionWithTransform` without a mirror: the origin that puts the turned template's
/// lowest corner at `position`.
fn zero_position([x, y, z]: [i32; 3], rotation: Rotation, size_x: i32, size_z: i32) -> [i32; 3] {
    let (size_x, size_z) = (size_x - 1, size_z - 1);
    match rotation {
        Rotation::None => [x, y, z],
        Rotation::Clockwise90 => [x + size_z, y, z],
        Rotation::Clockwise180 => [x + size_x, y, z + size_z],
        Rotation::Counterclockwise90 => [x, y, z + size_x],
    }
}

/// How many of the box's eight corners hold air, water or lava (`FossilFeature.countEmptyCorners`).
fn empty_corners(world: &impl FeatureWorld, bounds: BoundingBox) -> i32 {
    let states = block_states();
    let mut count = 0;
    for x in [bounds.min[0], bounds.max[0]] {
        for y in [bounds.min[1], bounds.max[1]] {
            for z in [bounds.min[2], bounds.max[2]] {
                let state = world.block_state(x, y, z);
                if states.is_air(state)
                    || matches!(
                        states.properties(state).expect("a block state").0,
                        "minecraft:water" | "minecraft:lava"
                    )
                {
                    count += 1;
                }
            }
        }
    }
    count
}

/// `StructureTemplate.placeInWorld` with the feature's random as the placement random: the palette draw, the
/// processors, then the blocks inside `clip`. Fossil templates hold no block entities and no blocks that take
/// water or change shape against their neighbors.
fn place_template<R: RandomSource>(
    world: &mut impl FeatureWorld,
    template: &StructureTemplate,
    processors: &ProcessorPipeline,
    position: [i32; 3],
    rotation: Rotation,
    clip: BoundingBox,
    random: &mut WorldgenRandom<R>,
) {
    let blocks = template.blocks(position);
    if blocks.is_empty() {
        return;
    }
    // `StructurePlaceSettings.getRandomPalette` draws even when the template has one palette.
    random.next_int_bounded(template.palette_count() as i32);
    let blocks: Vec<ProcessorBlock> = blocks
        .iter()
        .map(|block| ProcessorBlock {
            pos: std::array::from_fn(|i| rotation.transform(block.pos)[i] + position[i]),
            state: block.state,
            nbt: block.nbt.clone(),
        })
        .collect();
    let processed = processors.process_in_world(
        world.seed(),
        position,
        position,
        &blocks,
        &mut WorldReader(&*world),
        Some(random),
    );
    for block in processed {
        let [x, y, z] = block.pos;
        if (0..3).all(|i| (clip.min[i]..=clip.max[i]).contains(&block.pos[i])) {
            world.set_block_state(x, y, z, rotation.rotate_state(block.state));
        }
    }
}

/// Processor reads from a feature world.
struct WorldReader<'a, W>(&'a W);

impl<W: FeatureWorld> ProcessorWorld for WorldReader<'_, W> {
    fn block_at(&mut self, [x, y, z]: [i32; 3]) -> u32 {
        self.0.block_state(x, y, z)
    }

    fn heightmap(&mut self, kind: HeightmapType, x: i32, z: i32) -> i32 {
        self.0.heightmap(kind, x, z)
    }
}

/// A resource location with vanilla's default namespace.
fn qualified(name: &str) -> String {
    if name.contains(':') {
        name.to_string()
    } else {
        format!("minecraft:{name}")
    }
}
