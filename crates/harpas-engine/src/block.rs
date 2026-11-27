//! Changing the blocks of a loaded chunk as vanilla's `LevelChunk.setBlockState` does, and players breaking
//! them as vanilla's `ServerPlayerGameMode` does.
//!
//! A change updates the chunk's four final heightmaps (`Heightmap.update`) and removes the block entity of a
//! block that is replaced by another block. It is recorded for the tick's "sending block changes" step, which
//! sends it to every player who has the chunk, as vanilla's `ChunkHolder.broadcastChanges`.
//!
//! Shape updates of the neighbors are in [`crate::neighbor`], and the light around a change is computed again
//! as [`crate::light`] describes.

use std::sync::OnceLock;

use harpas_anvil::Chunk;
use harpas_core::block::block_states;
use harpas_core::block_behavior::block_behavior;
use harpas_core::nbt::{List, Nbt};
use harpas_worldgen::generator::HeightmapType;
use harpas_worldgen::placement::{Fluid, fluid};

use crate::world::ChunkPos;

/// The lowest block of the overworld.
pub(crate) const MIN_Y: i32 = -64;
/// The overworld's height in blocks.
pub(crate) const HEIGHT: i32 = 384;
/// The highest block of the overworld (`getMaxY`).
pub(crate) const MAX_Y: i32 = MIN_Y + HEIGHT - 1;

/// A block's position in the world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockPos {
    /// East-west coordinate.
    pub x: i32,
    /// Height.
    pub y: i32,
    /// North-south coordinate.
    pub z: i32,
}

impl BlockPos {
    /// The chunk column that holds this block.
    pub fn chunk(self) -> ChunkPos {
        ChunkPos {
            x: self.x >> 4,
            z: self.z >> 4,
        }
    }

    /// The squared distance from `point` to the nearest point of this block's cube, as vanilla's
    /// `AABB.distanceToSqr` for `new AABB(pos)`.
    pub(crate) fn distance_squared(self, point: [f64; 3]) -> f64 {
        [self.x, self.y, self.z]
            .into_iter()
            .zip(point)
            .map(|(min, p)| {
                let min = f64::from(min);
                let d = (min - p).max(p - (min + 1.0)).max(0.0);
                d * d
            })
            .sum()
    }
}

/// What a player did to the block it is breaking: the first three statuses of Java's Player Action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigAction {
    /// Started breaking the block (`START_DESTROY_BLOCK`).
    Start,
    /// Stopped breaking the block before it broke (`ABORT_DESTROY_BLOCK`).
    Abort,
    /// Finished breaking the block (`STOP_DESTROY_BLOCK`).
    Finish,
}

/// The block that `removeBlock` leaves in place of `state`: the block of the fluid it holds, so that breaking
/// a waterlogged block or seagrass leaves water (`FluidState.createLegacyBlock`), and air otherwise.
pub(crate) fn removed(state: u32) -> u32 {
    static STATES: OnceLock<[u32; 3]> = OnceLock::new();
    let [air, water, lava] = *STATES.get_or_init(|| {
        let states = block_states();
        ["minecraft:air", "minecraft:water", "minecraft:lava"]
            .map(|name| states.default_state(name).expect("a vanilla block"))
    });
    match fluid(state) {
        Fluid::Water => water,
        Fluid::Lava => lava,
        // Only liquid blocks hold flowing fluids, and a player cannot target one.
        Fluid::Empty | Fluid::FlowingWater | Fluid::FlowingLava => air,
    }
}

/// The state of `void_air`, which vanilla reads outside the world's height.
pub(crate) fn void_air() -> u32 {
    static STATE: OnceLock<u32> = OnceLock::new();
    *STATE.get_or_init(|| {
        block_states()
            .default_state("minecraft:void_air")
            .expect("a vanilla block")
    })
}

/// How much of `state` a player whose destroy speed is `speed` breaks each tick, 1 being the whole block
/// (`BlockBehaviour.getDestroyProgress`): the speed divided by the block's hardness, and by 30, or by 100 for a
/// block that needs a tool the player does not hold. A block of hardness -1 never breaks, and one of hardness 0
/// breaks at once.
pub fn destroy_progress(state: u32, speed: f32, can_harvest: bool) -> f32 {
    let states = block_states();
    let hardness = states.hardness(state);
    if hardness == -1.0 {
        return 0.0;
    }
    let divisor = if can_harvest { 30.0 } else { 100.0 };
    speed / hardness / divisor
}

/// The height of the water at a block, from its bottom (`FluidState.getHeight`), or none for a block without
/// water. Water under more water fills its block; otherwise it is as high as its amount, out of 9.
pub(crate) fn water_height(state: u32, above: u32) -> Option<f32> {
    let is_water = |state| matches!(fluid(state), Fluid::Water | Fluid::FlowingWater);
    if !is_water(state) {
        return None;
    }
    if is_water(above) {
        return Some(1.0);
    }
    // A source holds 8; a flowing level of 1 to 7 holds 8 less that; falling water, 8 and above, holds 8.
    let (name, properties) = block_states().properties(state)?;
    let level = properties
        .iter()
        .find(|(key, _)| *key == "level")
        .and_then(|(_, value)| value.parse::<i32>().ok())
        .filter(|_| name == "minecraft:water")
        .unwrap_or(0);
    let amount = if (1..8).contains(&level) {
        8 - level
    } else {
        8
    };
    Some(amount as f32 / 9.0)
}

/// Whether replacing `old` with `new` can change the light around it (`LightEngine.hasDifferentLightProperties`):
/// they differ in how much light they let through or give off, or either occludes light with its shape.
pub(crate) fn changes_light(old: u32, new: u32) -> bool {
    let states = block_states();
    let behavior = block_behavior();
    old != new
        && (states.light_dampening(old) != states.light_dampening(new)
            || states.light_emission(old) != states.light_emission(new)
            || behavior.uses_light_shape(old)
            || behavior.uses_light_shape(new))
}

/// Whether the block is a `GameMasterBlock`, which only an operator in Creative may break: command, structure,
/// jigsaw and test blocks.
pub(crate) fn is_game_master_block(state: u32) -> bool {
    let Some((name, _)) = block_states().properties(state) else {
        return false;
    };
    matches!(
        name,
        "minecraft:command_block"
            | "minecraft:chain_command_block"
            | "minecraft:repeating_command_block"
            | "minecraft:structure_block"
            | "minecraft:jigsaw"
            | "minecraft:test_block"
            | "minecraft:test_instance_block"
    )
}

/// The block state at `pos` in `chunk`, which must hold it; air where the chunk has no section.
pub(crate) fn get(chunk: &Chunk, pos: BlockPos) -> u32 {
    chunk
        .sections
        .iter()
        .find(|section| i32::from(section.y) == pos.y >> 4)
        .and_then(|section| section.section.as_ref())
        .map_or(0, |section| {
            section.block_state(
                (pos.x & 15) as usize,
                (pos.y & 15) as usize,
                (pos.z & 15) as usize,
            )
        })
}

/// The first air above the world's surface in this column, from the live heightmap.
pub(crate) fn surface_height(chunk: &Chunk, x: i32, z: i32) -> i32 {
    let kind = HeightmapType::WorldSurface;
    if let Some(Nbt::Compound(maps)) = chunk.rest.get("Heightmaps")
        && let Some(Nbt::LongArray(packed)) = maps.get(kind.nbt_key())
        && packed.len() == HEIGHT_LONGS
    {
        return packed_height(packed, ((z & 15) << 4 | (x & 15)) as usize);
    }
    (MIN_Y..=MAX_Y)
        .rev()
        .find(|&y| kind.matches(get(chunk, BlockPos { x, y, z })))
        .map_or(MIN_Y, |y| y + 1)
}

/// Sets the block state at `pos` in `chunk`, which must hold it, as `LevelChunk.setBlockState`: updates the
/// final heightmaps and removes the block entity of a replaced block. Returns the old state, or none when the
/// state did not change or the chunk has no section there.
pub(crate) fn set(chunk: &mut Chunk, pos: BlockPos, state: u32) -> Option<u32> {
    let section = chunk
        .sections
        .iter_mut()
        .find(|section| i32::from(section.y) == pos.y >> 4)?
        .section
        .as_mut()?;
    let (x, y, z) = (
        (pos.x & 15) as usize,
        (pos.y & 15) as usize,
        (pos.z & 15) as usize,
    );
    let old = section.set_block_state(x, y, z, state);
    if old == state {
        return None;
    }
    update_heightmaps(chunk, pos, state);
    let states = block_states();
    let block = |id| states.properties(id).map(|(name, _)| name);
    if block_behavior().has_block_entity(old) && block(old) != block(state) {
        remove_block_entity(chunk, pos);
    }
    Some(old)
}

/// Vanilla's `Heightmap.update` for each heightmap the chunk has saved.
fn update_heightmaps(chunk: &mut Chunk, pos: BlockPos, state: u32) {
    let Some(Nbt::Compound(mut maps)) = chunk.rest.remove("Heightmaps") else {
        return;
    };
    for kind in HeightmapType::FINAL {
        let Some(Nbt::LongArray(packed)) = maps.get(kind.nbt_key()) else {
            continue;
        };
        let Some(mut heights) = unpack(packed) else {
            continue;
        };
        let index = ((pos.z & 15) << 4 | (pos.x & 15)) as usize;
        let first_available = heights[index];
        if pos.y <= first_available - 2 {
            continue;
        }
        let height = if kind.matches(state) {
            if pos.y < first_available {
                continue;
            }
            pos.y + 1
        } else if first_available - 1 == pos.y {
            (MIN_Y..pos.y)
                .rev()
                .find(|&y| kind.matches(get(chunk, BlockPos { y, ..pos })))
                .map_or(MIN_Y, |y| y + 1)
        } else {
            continue;
        };
        heights[index] = height;
        maps.insert(kind.nbt_key(), Nbt::LongArray(pack(&heights)));
    }
    chunk.rest.insert("Heightmaps", Nbt::Compound(maps));
}

/// Bits per height: enough for the heights 0 to [`HEIGHT`] above [`MIN_Y`].
const HEIGHT_BITS: usize = (usize::BITS - (HEIGHT as usize).leading_zeros()) as usize;
const HEIGHTS_PER_LONG: usize = 64 / HEIGHT_BITS;
const HEIGHT_LONGS: usize = 256_usize.div_ceil(HEIGHTS_PER_LONG);

/// The first available height of each column, from a heightmap saved as vanilla's `SimpleBitStorage`; none
/// for a heightmap of another length, which vanilla ignores too.
fn unpack(packed: &[i64]) -> Option<[i32; 256]> {
    if packed.len() != HEIGHT_LONGS {
        return None;
    }
    Some(std::array::from_fn(|i| packed_height(packed, i)))
}

fn packed_height(packed: &[i64], index: usize) -> i32 {
    let mask = (1_u64 << HEIGHT_BITS) - 1;
    let long = packed[index / HEIGHTS_PER_LONG] as u64;
    let value = (long >> ((index % HEIGHTS_PER_LONG) * HEIGHT_BITS)) & mask;
    MIN_Y + value as i32
}

fn pack(heights: &[i32; 256]) -> Vec<i64> {
    let mut packed = vec![0_i64; HEIGHT_LONGS];
    for (i, &height) in heights.iter().enumerate() {
        let value = (height - MIN_Y).clamp(0, HEIGHT) as u64;
        packed[i / HEIGHTS_PER_LONG] |= (value << ((i % HEIGHTS_PER_LONG) * HEIGHT_BITS)) as i64;
    }
    packed
}

fn remove_block_entity(chunk: &mut Chunk, pos: BlockPos) {
    let Some(Nbt::List(List::Compound(mut entities))) = chunk.rest.remove("block_entities") else {
        return;
    };
    entities.retain(|entity| {
        let coordinate = |name| match entity.get(name) {
            Some(Nbt::Int(n)) => Some(*n),
            _ => None,
        };
        [coordinate("x"), coordinate("y"), coordinate("z")]
            != [Some(pos.x), Some(pos.y), Some(pos.z)]
    });
    let entities = if entities.is_empty() {
        List::Empty
    } else {
        List::Compound(entities)
    };
    chunk.rest.insert("block_entities", Nbt::List(entities));
}

#[cfg(test)]
mod tests {
    use harpas_anvil::ChunkSection;
    use harpas_core::chunk::{PalettedContainer, Section, Strategy};
    use harpas_core::nbt::Compound;

    use super::*;

    fn state(name: &str) -> u32 {
        block_states().default_state(name).unwrap()
    }

    /// A chunk of stone from the bottom of the world up to y = -49, with heightmaps saved.
    fn chunk() -> Chunk {
        let states = block_states();
        let blocks = PalettedContainer::filled(
            Strategy::block_states(states.global_bits()),
            state("minecraft:stone"),
        );
        let biomes = PalettedContainer::filled(Strategy::biomes(1), 0);
        let mut maps = Compound::new();
        for kind in HeightmapType::FINAL {
            maps.insert(kind.nbt_key(), Nbt::LongArray(pack(&[-48; 256])));
        }
        let mut rest = Compound::new();
        rest.insert("Heightmaps", Nbt::Compound(maps));
        Chunk {
            sections: vec![ChunkSection {
                y: -4,
                section: Some(Section::new(blocks, biomes)),
                rest: Compound::new(),
            }],
            rest,
        }
    }

    fn height(chunk: &Chunk, kind: HeightmapType, x: i32, z: i32) -> i32 {
        let Some(Nbt::Compound(maps)) = chunk.rest.get("Heightmaps") else {
            panic!("no heightmaps")
        };
        let Some(Nbt::LongArray(packed)) = maps.get(kind.nbt_key()) else {
            panic!("no {}", kind.nbt_key())
        };
        unpack(packed).unwrap()[((z & 15) << 4 | (x & 15)) as usize]
    }

    #[test]
    fn heights_pack_as_vanilla_stores_the_overworld() {
        assert_eq!((HEIGHT_BITS, HEIGHT_LONGS), (9, 37));
        let heights = std::array::from_fn(|i| MIN_Y + i as i32);
        assert_eq!(unpack(&pack(&heights)), Some(heights));
        assert_eq!(unpack(&[0; 36]), None);
    }

    #[test]
    fn removing_the_top_block_lowers_the_heightmaps_to_the_next_block() {
        let mut chunk = chunk();
        let air = state("minecraft:air");
        let top = BlockPos { x: 3, y: -49, z: 5 };
        assert_eq!(set(&mut chunk, top, air), Some(state("minecraft:stone")));
        for kind in HeightmapType::FINAL {
            assert_eq!(height(&chunk, kind, 3, 5), -49);
            assert_eq!(height(&chunk, kind, 4, 5), -48);
        }
        // A block below the top changes no height.
        set(&mut chunk, BlockPos { y: -60, ..top }, air);
        assert_eq!(height(&chunk, HeightmapType::WorldSurface, 3, 5), -49);
        // Setting the same state is no change.
        assert_eq!(set(&mut chunk, top, air), None);
    }

    #[test]
    fn a_column_emptied_to_the_bottom_has_the_lowest_height() {
        let mut chunk = chunk();
        for y in (MIN_Y..-48).rev() {
            set(
                &mut chunk,
                BlockPos { x: 0, y, z: 0 },
                state("minecraft:air"),
            );
        }
        assert_eq!(height(&chunk, HeightmapType::MotionBlocking, 0, 0), MIN_Y);
    }

    #[test]
    fn water_left_by_a_waterlogged_block_still_blocks_motion_heightmaps() {
        let mut chunk = chunk();
        let top = BlockPos { x: 0, y: -49, z: 0 };
        set(&mut chunk, top, removed(state("minecraft:seagrass")));
        assert_eq!(get(&chunk, top), state("minecraft:water"));
        assert_eq!(height(&chunk, HeightmapType::MotionBlocking, 0, 0), -48);
        assert_eq!(height(&chunk, HeightmapType::WorldSurface, 0, 0), -48);
        assert_eq!(height(&chunk, HeightmapType::OceanFloor, 0, 0), -49);
    }

    #[test]
    fn replacing_a_block_with_a_block_entity_removes_its_entity() {
        let mut chunk = chunk();
        let chest = BlockPos { x: 1, y: -50, z: 2 };
        set(&mut chunk, chest, state("minecraft:chest"));
        let entity = |pos: BlockPos| {
            let mut entity = Compound::new();
            entity.insert("id", "minecraft:chest");
            entity.insert("x", pos.x);
            entity.insert("y", pos.y);
            entity.insert("z", pos.z);
            entity
        };
        let other = BlockPos { x: 2, ..chest };
        chunk.rest.insert(
            "block_entities",
            Nbt::List(List::Compound(vec![entity(chest), entity(other)])),
        );
        set(&mut chunk, chest, state("minecraft:air"));
        assert_eq!(
            chunk.rest.get("block_entities"),
            Some(&Nbt::List(List::Compound(vec![entity(other)])))
        );
    }

    #[test]
    fn breaking_leaves_the_fluid_a_block_held() {
        assert_eq!(removed(state("minecraft:stone")), state("minecraft:air"));
        let waterlogged = block_states()
            .state(
                "minecraft:oak_slab",
                [("type", "bottom"), ("waterlogged", "true")],
            )
            .unwrap()
            .0;
        assert_eq!(removed(waterlogged), state("minecraft:water"));
        assert!(is_game_master_block(state("minecraft:command_block")));
        assert!(!is_game_master_block(state("minecraft:barrier")));
    }

    #[test]
    fn destroy_progress_follows_hardness_and_the_tool_a_block_needs() {
        assert_eq!(
            destroy_progress(state("minecraft:dirt"), 1.0, true),
            1.0 / 0.5 / 30.0
        );
        assert_eq!(
            destroy_progress(state("minecraft:stone"), 1.0, false),
            1.0 / 1.5 / 100.0
        );
        assert_eq!(
            destroy_progress(state("minecraft:bedrock"), 1.0, false),
            0.0
        );
        assert!(destroy_progress(state("minecraft:short_grass"), 0.2, true) >= 1.0);
    }

    #[test]
    fn water_is_as_high_as_its_amount_unless_more_water_is_above() {
        let water = |level: &str| {
            block_states()
                .state("minecraft:water", [("level", level)])
                .unwrap()
                .0
        };
        let air = state("minecraft:air");
        assert_eq!(water_height(water("0"), air), Some(8.0 / 9.0));
        assert_eq!(water_height(water("3"), air), Some(5.0 / 9.0));
        assert_eq!(water_height(water("8"), air), Some(8.0 / 9.0));
        assert_eq!(water_height(water("3"), water("0")), Some(1.0));
        assert_eq!(
            water_height(state("minecraft:seagrass"), air),
            Some(8.0 / 9.0)
        );
        assert_eq!(water_height(state("minecraft:stone"), air), None);
    }

    #[test]
    fn distance_to_a_block_is_measured_to_its_nearest_face() {
        let pos = BlockPos { x: 0, y: 0, z: 0 };
        assert_eq!(pos.distance_squared([0.5, 0.5, 0.5]), 0.0);
        assert_eq!(pos.distance_squared([3.0, 0.5, 0.5]), 4.0);
        assert_eq!(pos.distance_squared([-1.0, 2.0, 0.5]), 2.0);
    }
}
