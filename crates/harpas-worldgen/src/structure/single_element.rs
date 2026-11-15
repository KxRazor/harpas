//! Template pool elements placed in one chunk: vanilla's `SinglePoolElement.place`, observed through its public API
//! (`single_pool_element_parity`).
//!
//! The whole template is processed first. The element ignores structure blocks, turns jigsaws into their final
//! states, then runs its processor list and, for terrain matching, gravity at `WORLD_SURFACE_WG`; a legacy element
//! ignores structure blocks and air last instead, so blocks its processors turn into air are not placed either.
//! The surviving blocks inside the chunk are turned with the piece and placed. Existing water waterlogs the blocks
//! that can hold it, then placed blocks take water from source water above them or through sides that are not
//! full, and water puts out lit candles and campfires. Block entities keep the template's data, and containers
//! draw a loot seed from the placement random. Shape updates at the template's edges follow separately.

use harpas_core::block::block_states;
use harpas_core::block_behavior::{Face, block_behavior};
use harpas_core::nbt::Compound;
use harpas_core::random::RandomSource;
use serde_json::json;

use super::shipwreck::{contains, property, set, source_water, water};
use crate::GeneratedChunk;
use crate::data::{PoolElementDef, Projection, WorldgenData, WorldgenDataError};
use crate::generator::HeightmapType;
use crate::processor::{ProcessorBlock, ProcessorPipeline, ProcessorWorld};
use crate::template::{Rotation, StructureTemplates};

/// A single or legacy single pool element placed in a chunk (`SinglePoolElement.place`).
pub struct SingleElementPlacement<'a> {
    /// Worldgen definitions, for the element's processor list.
    pub data: &'a WorldgenData,
    /// The owner's templates.
    pub templates: &'a StructureTemplates,
    /// The world seed, which seeds capped processors.
    pub seed: i64,
    /// Where the template's origin is placed.
    pub position: [i32; 3],
    /// The processors' reference position.
    pub pivot: [i32; 3],
    /// How the template is turned.
    pub rotation: Rotation,
    /// Whether existing water is kept out of placed blocks (`LiquidSettings.IGNORE_WATERLOGGING`).
    pub ignore_waterlogging: bool,
}

impl SingleElementPlacement<'_> {
    /// Places `element` in `chunk` and returns the block entities it creates there.
    ///
    /// `world` reads terrain outside the chunk and the heightmaps processors use, both as they were before the
    /// element was placed. `random` is the piece's placement random, which loot seeds draw from. Elements of other
    /// types place nothing.
    pub fn place(
        &self,
        element: &PoolElementDef,
        chunk: &mut GeneratedChunk,
        world: &mut impl ProcessorWorld,
        random: &mut impl RandomSource,
    ) -> Result<Vec<Compound>, WorldgenDataError> {
        let PoolElementDef::Single {
            location,
            processors,
            projection,
            legacy,
        } = element
        else {
            return Ok(Vec::new());
        };
        let Some(template) = self.templates.get(location) else {
            return Ok(Vec::new());
        };
        let pipeline = self.pipeline(location, processors.as_deref(), *projection, *legacy)?;
        let blocks: Vec<ProcessorBlock> = template
            .blocks(self.position)
            .iter()
            .map(|block| ProcessorBlock {
                pos: std::array::from_fn(|i| {
                    self.rotation.transform(block.pos)[i] + self.position[i]
                }),
                state: block.state,
                nbt: block.nbt.clone(),
            })
            .collect();
        let processed = pipeline.process_in_world(
            self.seed,
            self.position,
            self.pivot,
            &blocks,
            &mut ChunkWorld { chunk, world },
            None,
        );
        let mut entities = Vec::new();
        let mut placed = Vec::new();
        for block in processed {
            let pos @ [x, y, z] = block.pos;
            if !contains(chunk, pos) {
                continue;
            }
            let mut state = self.rotation.rotate_state(block.state);
            let existing = chunk.block_state((x & 15) as usize, y, (z & 15) as usize);
            if !self.ignore_waterlogging && water(existing) && holds_water(state) {
                state = waterlog(state);
            }
            set(chunk, pos, state);
            placed.push(pos);
            if let Some(nbt) = block.nbt {
                entities.push(entity(nbt, state, pos, random));
            }
        }
        if !self.ignore_waterlogging {
            fill_with_water(chunk, &placed, world);
        }
        Ok(entities)
    }

    /// The element's processors, after the ones it adds itself.
    fn pipeline(
        &self,
        location: &str,
        processors: Option<&str>,
        projection: Projection,
        legacy: bool,
    ) -> Result<ProcessorPipeline, WorldgenDataError> {
        // A legacy element ignores air after all its other processors, so blocks they turn into air stay out.
        let ignore = |names: &[&str]| {
            let blocks: Vec<_> = names.iter().map(|name| json!({"Name": name})).collect();
            json!({"processor_type": "minecraft:block_ignore", "blocks": blocks})
        };
        let mut entries = Vec::new();
        if !legacy {
            entries.push(ignore(&["minecraft:structure_block"]));
        }
        entries.push(json!({"processor_type": "minecraft:jigsaw_replacement"}));
        if let Some(name) = processors {
            let list = self.data.processor_lists.get(name).ok_or_else(|| {
                WorldgenDataError::InvalidDefinition {
                    context: location.to_string(),
                    message: format!("unknown processor list {name}"),
                }
            })?;
            entries.extend(list.iter().cloned());
        }
        if projection == Projection::TerrainMatching {
            entries.push(json!({"processor_type": "minecraft:gravity", "heightmap": "WORLD_SURFACE_WG", "offset": -1}));
        }
        if legacy {
            entries.push(ignore(&["minecraft:structure_block", "minecraft:air"]));
        }
        ProcessorPipeline::compile(location, &entries, self.data)?.ok_or_else(|| {
            WorldgenDataError::InvalidDefinition {
                context: location.to_string(),
                message: "the element uses an unported processor".to_string(),
            }
        })
    }
}

/// Placed blocks that can hold water take it from source water above or beside them, through faces that are not
/// full, until none changes. Water from above enters through any face.
fn fill_with_water(
    chunk: &mut GeneratedChunk,
    placed: &[[i32; 3]],
    world: &mut impl ProcessorWorld,
) {
    const SIDES: [(Face, [i32; 3]); 5] = [
        (Face::Up, [0, 1, 0]),
        (Face::North, [0, 0, -1]),
        (Face::East, [1, 0, 0]),
        (Face::South, [0, 0, 1]),
        (Face::West, [-1, 0, 0]),
    ];
    loop {
        let mut changed = false;
        for &pos @ [x, y, z] in placed {
            let state = chunk.block_state((x & 15) as usize, y, (z & 15) as usize);
            let dry = block_states()
                .properties(state)
                .expect("a block state")
                .1
                .contains(&("waterlogged", "false"));
            if !dry || !holds_water(state) {
                continue;
            }
            let fills = SIDES.iter().any(|&(face, [dx, dy, dz])| {
                let neighbor @ [nx, ny, nz] = [x + dx, y + dy, z + dz];
                (face == Face::Up || !block_behavior().is_face_sturdy(state, face))
                    && source_water(if contains(chunk, neighbor) {
                        chunk.block_state((nx & 15) as usize, ny, (nz & 15) as usize)
                    } else {
                        world.block_at(neighbor)
                    })
            });
            if fills {
                set(chunk, pos, waterlog(state));
                changed = true;
            }
        }
        if !changed {
            return;
        }
    }
}

/// The state holding water, which puts out a lit candle or campfire.
fn waterlog(state: u32) -> u32 {
    property(property(state, "waterlogged", "true"), "lit", "false")
}

/// Whether water can waterlog the state: one with a `waterlogged` property, other than a double slab.
fn holds_water(state: u32) -> bool {
    let properties = block_states().properties(state).expect("a block state").1;
    properties.iter().any(|&(key, _)| key == "waterlogged")
        && !properties.contains(&("type", "double"))
}

/// A placed block entity: the template's data at its world position.
///
/// A container that can hold loot draws a seed from the placement random, which it keeps only with a loot
/// table. Other block entities, such as suspicious sand, keep the seed a processor gave them.
fn entity(
    mut nbt: Compound,
    state: u32,
    pos: [i32; 3],
    random: &mut impl RandomSource,
) -> Compound {
    let name = block_states().properties(state).expect("a block state").0;
    if nbt.get("id").is_none() {
        // Processors can add data to a block that had none, such as loot for suspicious gravel.
        let id = match name {
            "minecraft:suspicious_sand" | "minecraft:suspicious_gravel" => {
                "minecraft:brushable_block"
            }
            other => other,
        };
        nbt.insert("id", id);
    }
    for (key, value) in ["x", "y", "z"].into_iter().zip(pos) {
        nbt.insert(key, value);
    }
    let randomizable = matches!(
        name.strip_prefix("minecraft:").unwrap_or(name),
        "chest"
            | "trapped_chest"
            | "barrel"
            | "dispenser"
            | "dropper"
            | "hopper"
            | "crafter"
            | "decorated_pot"
    ) || name.ends_with("shulker_box")
        || name.ends_with("copper_chest");
    if randomizable {
        let seed = random.next_long();
        if nbt.get("LootTable").is_some() {
            nbt.insert("LootTableSeed", seed);
        }
    }
    nbt
}

/// Terrain in the target chunk from the chunk itself, and elsewhere from the caller's world.
struct ChunkWorld<'a, W> {
    chunk: &'a GeneratedChunk,
    world: &'a mut W,
}

impl<W: ProcessorWorld> ProcessorWorld for ChunkWorld<'_, W> {
    fn block_at(&mut self, pos @ [x, y, z]: [i32; 3]) -> u32 {
        if contains(self.chunk, pos) {
            self.chunk
                .block_state((x & 15) as usize, y, (z & 15) as usize)
        } else {
            self.world.block_at(pos)
        }
    }

    fn heightmap(&mut self, kind: HeightmapType, x: i32, z: i32) -> i32 {
        self.world.heightmap(kind, x, z)
    }
}
