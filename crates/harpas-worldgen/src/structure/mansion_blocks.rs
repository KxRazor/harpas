//! Woodland mansion blocks in the decoration region: vanilla's `WoodlandMansionPiece.postProcess`, which places a
//! template with `StructureTemplate.placeInWorld`, and `WoodlandMansionStructure.afterPlace`.
//!
//! Each piece that reaches the chunk places its template's blocks inside the chunk, structure blocks aside, in
//! the template's order: reflected, then turned. A block that can hold water takes the source water it replaces,
//! then the source water beside or above it, until none is left; the template's own liquids do not count. The
//! template's shape then updates against its surroundings, as it has no known shape: both sides of every face
//! at its edge, which can reach into the neighboring chunks, then every placed block against all its neighbors.
//! Data markers follow: chests with the mansion's loot table inside the chunk, and air where the mobs stand.
//! Once every piece is placed, cobblestone fills down from each column of the mansion's lowest floor to the
//! ground. Mobs are spawned by gameplay.

use std::collections::HashMap;

use harpas_core::block::block_states;
use harpas_core::block_behavior::block_behavior;
use harpas_core::nbt::{Compound, Nbt};
use harpas_core::random::RandomSource;

use super::start::{StructurePiece, StructureStart, TemplatePiece};
use crate::feature::FeatureWorld;
use crate::shape_update::{Direction, ShapeUpdates, VoxelShape, update_shape_at_edge};
use crate::template::{Facing, StructureTemplates};

/// The piece type of every mansion piece.
const PIECE: &str = "minecraft:wmp";
/// The loot table of the chests that the data markers place.
const LOOT_TABLE: &str = "minecraft:chests/woodland_mansion";
/// The directions `Block.updateFromNeighbourShapes` updates a block against, in its order.
const UPDATE_SHAPE_ORDER: [Direction; 6] = [
    Direction::West,
    Direction::East,
    Direction::North,
    Direction::South,
    Direction::Down,
    Direction::Up,
];
/// The neighbors whose source water fills a block that can hold it, in vanilla's order.
const FILL_FROM: [Direction; 5] = [
    Direction::Up,
    Direction::North,
    Direction::East,
    Direction::South,
    Direction::West,
];

impl StructureStart {
    /// Places the mansion's pieces that reach a chunk, then its foundations there, in the decoration region, and
    /// returns the block entities it leaves in the chunk. `random` is the decoration's random source, seeded as
    /// vanilla's decoration does for `minecraft:mansion`, which chest loot seeds draw from.
    pub fn place_mansion(
        &self,
        world: &mut impl FeatureWorld,
        chunk_x: i32,
        chunk_z: i32,
        templates: &StructureTemplates,
        updates: &ShapeUpdates,
        random: &mut impl RandomSource,
    ) -> Vec<Compound> {
        if self.structure != "minecraft:mansion" {
            return Vec::new();
        }
        let chunk = ChunkBox::new(chunk_x, chunk_z);
        let mut entities = HashMap::new();
        for piece in &self.pieces {
            if piece.kind != PIECE || !chunk.intersects(piece) {
                continue;
            }
            let Some(template) = &piece.template else {
                continue;
            };
            place_piece(
                world,
                &chunk,
                template,
                templates,
                updates,
                random,
                &mut entities,
            );
        }
        self.foundations(world, &chunk);
        let behavior = block_behavior();
        let mut entities: Vec<_> = entities
            .into_iter()
            .filter(|&([x, y, z], _)| behavior.has_block_entity(world.block_state(x, y, z)))
            .collect();
        entities.sort_by_key(|&([x, y, z], _)| (y, z, x));
        entities.into_iter().map(|(_, entity)| entity).collect()
    }

    /// `WoodlandMansionStructure.afterPlace`: below every column of the chunk where the lowest floor of a piece
    /// holds a block, cobblestone replaces air and liquids down to the first other block.
    fn foundations(&self, world: &mut impl FeatureWorld, chunk: &ChunkBox) {
        let bounds = self.bounding_box;
        let floor = bounds.min[1];
        let cobblestone = block_states()
            .default_state("minecraft:cobblestone")
            .unwrap();
        let behavior = block_behavior();
        let states = block_states();
        for x in chunk.min[0]..=chunk.min[0] + 15 {
            for z in chunk.min[1]..=chunk.min[1] + 15 {
                let inside = |p: [i32; 3]| {
                    (0..3).all(|i| bounds.min[i] <= p[i] && p[i] <= bounds.max[i])
                        && self.pieces.iter().any(|piece| {
                            let b = piece.bounding_box;
                            (0..3).all(|i| b.min[i] <= p[i] && p[i] <= b.max[i])
                        })
                };
                if states.is_air(world.block_state(x, floor, z)) || !inside([x, floor, z]) {
                    continue;
                }
                for y in (world.min_y() + 1..floor).rev() {
                    let state = world.block_state(x, y, z);
                    if !states.is_air(state) && !behavior.is_liquid(state) {
                        break;
                    }
                    world.set_block_state(x, y, z, cobblestone);
                }
            }
        }
    }
}

/// The columns of the chunk being decorated, which bound what a piece places (`box` in `postProcess`).
struct ChunkBox {
    min: [i32; 2],
}

impl ChunkBox {
    const fn new(chunk_x: i32, chunk_z: i32) -> Self {
        Self {
            min: [chunk_x * 16, chunk_z * 16],
        }
    }

    const fn contains(&self, [x, _, z]: [i32; 3]) -> bool {
        x >= self.min[0] && x <= self.min[0] + 15 && z >= self.min[1] && z <= self.min[1] + 15
    }

    fn intersects(&self, piece: &StructurePiece) -> bool {
        piece.bounding_box.intersects_columns(
            self.min[0],
            self.min[1],
            self.min[0] + 15,
            self.min[1] + 15,
        )
    }
}

/// The world position of a template's block: reflected, then turned, around the template's pivot.
fn position(template: &TemplatePiece, pos: [i32; 3]) -> [i32; 3] {
    let p = template
        .rotation
        .transform_around(template.mirror.apply(pos), template.pivot);
    std::array::from_fn(|i| p[i] + template.position[i])
}

/// Whether a block state holds a fluid source (`FluidState.isSource`), and whether that fluid is water.
fn fluid_source(state: u32) -> Option<bool> {
    let (name, properties) = block_states().properties(state)?;
    match name {
        "minecraft:water" => properties.contains(&("level", "0")).then_some(true),
        "minecraft:lava" => properties.contains(&("level", "0")).then_some(false),
        "minecraft:bubble_column"
        | "minecraft:kelp"
        | "minecraft:kelp_plant"
        | "minecraft:seagrass"
        | "minecraft:tall_seagrass" => Some(true),
        _ => properties
            .contains(&("waterlogged", "true"))
            .then_some(true),
    }
}

/// Whether a block can take water (`LiquidBlockContainer`) and does not hold it yet.
fn dry_container(state: u32) -> bool {
    block_states()
        .properties(state)
        .is_some_and(|(_, properties)| properties.contains(&("waterlogged", "false")))
}

fn waterlogged(state: u32) -> u32 {
    crate::shape_update::with_property(state, "waterlogged", "true")
}

/// Places one piece's template in the chunk (`StructureTemplate.placeInWorld`), then handles its data markers
/// (`WoodlandMansionPiece.handleDataMarker`).
fn place_piece(
    world: &mut impl FeatureWorld,
    chunk: &ChunkBox,
    template: &TemplatePiece,
    templates: &StructureTemplates,
    updates: &ShapeUpdates,
    random: &mut impl RandomSource,
    entities: &mut HashMap<[i32; 3], Compound>,
) {
    let Some(blocks) = templates.get(&template.template) else {
        return;
    };
    let states = block_states();
    let mut placed = Vec::new();
    let mut markers = Vec::new();
    // Placed blocks that can still take water from beside them, and placed blocks that are liquid sources.
    let mut fill = Vec::new();
    let mut liquids = Vec::new();
    for block in blocks.blocks(template.position) {
        let pos @ [x, y, z] = position(template, block.pos);
        let name = states.properties(block.state).unwrap().0;
        if name == "minecraft:structure_block" {
            if let Some(Nbt::String(marker)) = block.nbt.as_ref().and_then(|n| n.get("metadata")) {
                markers.push((pos, marker.clone()));
            }
            continue;
        }
        if !chunk.contains(pos) || y < world.min_y() || y >= world.min_y() + world.height() {
            continue;
        }
        let fluid = fluid_source(world.block_state(x, y, z));
        let mut state = template
            .rotation
            .rotate_state(template.mirror.mirror_state(block.state));
        world.set_block_state(x, y, z, state);
        entities.remove(&pos);
        placed.push(pos);
        if let Some(nbt) = &block.nbt {
            let mut entity = nbt.clone();
            for (key, value) in ["x", "y", "z"].into_iter().zip(pos) {
                entity.insert(key, value);
            }
            entity.insert("components", Compound::new());
            // A banner without patterns does not save the empty list.
            if matches!(entity.get("patterns"), Some(Nbt::List(list)) if list.is_empty()) {
                entity.remove("patterns");
            }
            if matches!(name, "minecraft:chest" | "minecraft:trapped_chest") {
                // Template placement seeds randomizable containers; a container without a loot table does not
                // save the seed.
                let seed = random.next_long();
                if entity.get("LootTable").is_some() && seed != 0 {
                    entity.insert("LootTableSeed", seed);
                }
            }
            entities.insert(pos, entity);
        }
        if fluid_source(state).is_some() {
            liquids.push(pos);
        } else if dry_container(state) {
            if fluid == Some(true) {
                state = waterlogged(state);
                world.set_block_state(x, y, z, state);
            }
            if fluid.is_none() {
                fill.push(pos);
            }
        }
    }
    // Containers take source water from beside or above them, but not from the template's own liquids.
    let mut changed = true;
    while changed && !fill.is_empty() {
        changed = false;
        fill.retain(|&[x, y, z]| {
            let source = FILL_FROM.into_iter().find_map(|direction| {
                let n @ [nx, ny, nz] = direction.step([x, y, z]);
                let source = fluid_source(world.block_state(nx, ny, nz))?;
                (!liquids.contains(&n)).then_some(source)
            });
            let Some(water) = source else {
                return true;
            };
            let state = world.block_state(x, y, z);
            if water && dry_container(state) {
                world.set_block_state(x, y, z, waterlogged(state));
            }
            changed = true;
            false
        });
    }
    if let (Some(min), Some(max)) = (corner(&placed, i32::min), corner(&placed, i32::max)) {
        let mut shape = VoxelShape::new(std::array::from_fn(|i| max[i] - min[i] + 1));
        for pos in &placed {
            shape.fill(std::array::from_fn(|i| pos[i] - min[i]));
        }
        update_shape_at_edge(world, updates, &shape, min);
        for &pos @ [x, y, z] in &placed {
            let state = world.block_state(x, y, z);
            let mut updated = state;
            for direction in UPDATE_SHAPE_ORDER {
                let [nx, ny, nz] = direction.step(pos);
                let neighbor = world.block_state(nx, ny, nz);
                updated = updates.update(world, updated, pos, direction, neighbor);
            }
            if updated != state {
                world.set_block_state(x, y, z, updated);
            }
        }
    }
    for (pos @ [x, y, z], marker) in markers {
        let facing = match marker.as_str() {
            "ChestWest" => Facing::West,
            "ChestEast" => Facing::East,
            "ChestSouth" => Facing::South,
            "ChestNorth" | "Chest" => Facing::North,
            "Mage" | "Warrior" | "Group of Allays" => {
                // The mobs follow with gameplay; their marker becomes air inside the chunk.
                if chunk.contains(pos) {
                    world.set_block_state(x, y, z, states.default_state("minecraft:air").unwrap());
                }
                continue;
            }
            _ => continue,
        };
        let chest = states.default_state("minecraft:chest").unwrap();
        if !chunk.contains(pos)
            || states.properties(world.block_state(x, y, z)).unwrap().0 == "minecraft:chest"
        {
            continue;
        }
        let facing = if marker == "Chest" {
            facing
        } else {
            template.rotation.rotate(facing)
        };
        let state = crate::shape_update::with_property(chest, "facing", facing.name());
        world.set_block_state(x, y, z, state);
        let mut entity = Compound::new();
        entity.insert("id", "minecraft:chest");
        for (key, value) in ["x", "y", "z"].into_iter().zip(pos) {
            entity.insert(key, value);
        }
        entity.insert("components", Compound::new());
        entity.insert("LootTable", LOOT_TABLE);
        let seed = random.next_long();
        if seed != 0 {
            entity.insert("LootTableSeed", seed);
        }
        entities.insert(pos, entity);
    }
}

/// The lowest or highest corner of a set of positions.
fn corner(positions: &[[i32; 3]], pick: fn(i32, i32) -> i32) -> Option<[i32; 3]> {
    let first = *positions.first()?;
    Some(
        positions
            .iter()
            .fold(first, |a, b| std::array::from_fn(|i| pick(a[i], b[i]))),
    )
}
