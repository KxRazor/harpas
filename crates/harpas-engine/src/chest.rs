//! Ordinary chest block entities use the chunk's saved NBT as the authoritative inventory.

use std::collections::HashMap;
use std::io;

use harpas_core::block::block_states;
use harpas_core::block_behavior::block_behavior;
use harpas_core::nbt::{Compound, Nbt};
use harpas_core::random::{LegacyRandom, RandomSequence, RandomSource};
use harpas_worldgen::shape_update::Direction;

use crate::block::BlockPos;
use crate::container_storage::{new_tag, read_items, write_items};
use crate::inventory::{CHEST_SLOTS, ItemStack};
use crate::item_data::{ItemCodec, invalid};
use crate::loot::LootTable;
use crate::loot::chest::ChestLootTables;
use crate::maps::{MapChest, MapData, TreasureMap, pending};
use crate::world::{ChunkState, World};

pub(crate) const CHEST: &str = "minecraft:chest";

fn properties(state: u32) -> Option<(Direction, &'static str)> {
    let (name, properties) = block_states().properties(state)?;
    if name != CHEST {
        return None;
    }
    let property = |key| properties.iter().find(|(k, _)| *k == key).map(|&(_, v)| v);
    Some((
        Direction::from_name(property("facing")?)?,
        property("type")?,
    ))
}

fn relative(pos: BlockPos, direction: Direction) -> BlockPos {
    let [x, y, z] = direction.step([pos.x, pos.y, pos.z]);
    BlockPos { x, y, z }
}

/// `ChestBlock.getStateForPlacement`. Normal placement checks clockwise first. Secondary use pairs
/// only with a compatible single chest behind the clicked face and follows that chest's facing.
pub(crate) fn placement(
    pos: BlockPos,
    face: Direction,
    facing: Direction,
    secondary_use: bool,
    block_at: impl Fn(BlockPos) -> u32,
) -> (Direction, &'static str) {
    let candidate = |direction| {
        let (facing, kind) = properties(block_at(relative(pos, direction)))?;
        (kind == "single").then_some(facing)
    };
    if secondary_use {
        let side = face.opposite();
        if let Some(partner) = candidate(side) {
            let left = partner.clockwise();
            if side == left {
                return (partner, "left");
            }
            if side == left.opposite() {
                return (partner, "right");
            }
        }
    } else {
        let left = facing.clockwise();
        if candidate(left) == Some(facing) {
            return (facing, "left");
        }
        if candidate(left.opposite()) == Some(facing) {
            return (facing, "right");
        }
    }
    (facing, "single")
}

/// The block entities opened by a chest menu, in slot order. Vanilla puts the right half first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Chest {
    first: BlockPos,
    second: Option<BlockPos>,
}

impl Chest {
    pub(crate) fn at(world: &World, pos: BlockPos) -> Option<Self> {
        let (facing, kind) = properties(world.block_state(pos)?)?;
        let single = Self {
            first: pos,
            second: None,
        };
        let clockwise = facing.clockwise();
        let (direction, other_kind) = match kind {
            "left" => (clockwise, "right"),
            "right" => (clockwise.opposite(), "left"),
            _ => return Some(single),
        };
        let other = relative(pos, direction);
        let partner = match world.block_state(other) {
            Some(state) => properties(state),
            None if matches!(world.chunk(other.chunk()), Some(ChunkState::Missing)) => None,
            None => return None,
        };
        if partner != Some((facing, other_kind)) {
            return Some(single);
        }
        let (first, second) = if kind == "right" {
            (pos, other)
        } else {
            (other, pos)
        };
        Some(Self {
            first,
            second: Some(second),
        })
    }

    pub(crate) fn positions(self) -> impl Iterator<Item = BlockPos> {
        std::iter::once(self.first).chain(self.second)
    }

    pub(crate) fn is_double(self) -> bool {
        self.second.is_some()
    }

    pub(crate) fn is_blocked(self, world: &World) -> bool {
        self.positions().any(|pos| {
            world
                .block_state(relative(pos, Direction::Up))
                .is_some_and(|state| block_behavior().is_solid_render(state))
        })
    }
}

fn read_chest_items(
    tag: Option<&Compound>,
    codec: &ItemCodec,
) -> io::Result<[ItemStack; CHEST_SLOTS]> {
    if tag.is_some_and(|tag| tag.get("LootTable").is_some()) {
        return Err(invalid("chest has an unopened loot table"));
    }
    read_items(tag, codec)
}

/// Loot and random state are committed together after every requested chest half can be read.
#[derive(Default)]
struct ChestLoot {
    tags: Vec<(BlockPos, Compound)>,
    sequences: HashMap<String, RandomSequence>,
    world_random: Option<LegacyRandom>,
    maps: Vec<MapChest>,
    search: Option<MapChest>,
    first_map_id: Option<i32>,
    new_maps: Vec<MapData>,
}

impl ChestLoot {
    fn finish<T>(self, world: &mut World, result: io::Result<T>) -> io::Result<T> {
        if let Some(chest) = self.search {
            world.maps.prepare(chest)?;
        }
        let result = result?;
        if let Some(first) = self.first_map_id {
            world.maps.publish(first, self.new_maps)?;
        }
        for (pos, tag) in self.tags {
            world.store_container_tag(pos, tag);
        }
        world.random_sequences.extend(self.sequences);
        if let Some(random) = self.world_random {
            world.random = random;
        }
        for chest in self.maps {
            world.maps.consume(chest);
        }
        Ok(result)
    }
}

/// Reads the loot table and seed of an unopened chest.
fn chest_loot_table<'a>(
    tag: &Compound,
    chests: &'a ChestLootTables,
) -> io::Result<(&'a LootTable, i64)> {
    let Some(Nbt::String(id)) = tag.get("LootTable") else {
        return Err(invalid("invalid chest loot table ID"));
    };
    let table = chests
        .get(id)
        .ok_or_else(|| invalid(format!("chest loot table {id} is unavailable")))?;
    let seed = match tag.get("LootTableSeed") {
        None => 0,
        Some(Nbt::Long(seed)) => *seed,
        _ => return Err(invalid("invalid chest loot seed")),
    };
    Ok((table, seed))
}

impl World {
    /// The random source a chest's loot draws from: its saved seed, else the table's random sequence,
    /// else the world's random. Draws are kept in `loot` until it is committed.
    fn loot_random<'a>(
        &self,
        table: &LootTable,
        seed: i64,
        loot: &'a mut ChestLoot,
        seeded: &'a mut Option<LegacyRandom>,
    ) -> &'a mut dyn RandomSource {
        if seed != 0 {
            seeded.insert(LegacyRandom::new(seed))
        } else if let Some(name) = &table.random_sequence {
            loot.sequences
                .entry(name.clone())
                .or_insert_with(|| {
                    self.random_sequences
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| RandomSequence::new(self.seed, Some(name)))
                })
                .random()
        } else {
            loot.world_random.get_or_insert_with(|| self.random.clone())
        }
    }

    pub(crate) fn chest_tag(&self, pos: BlockPos) -> io::Result<Option<&Compound>> {
        self.container_tag(pos, CHEST)
    }

    #[cfg(test)]
    pub(crate) fn save_chest(&mut self, pos: BlockPos, slots: &[ItemStack]) -> io::Result<()> {
        let tag = self.chest_with_items(pos, self.chest_tag(pos)?, slots)?;
        self.store_container_tag(pos, tag);
        Ok(())
    }

    pub(crate) fn chest_with_items(
        &self,
        pos: BlockPos,
        tag: Option<&Compound>,
        slots: &[ItemStack],
    ) -> io::Result<Compound> {
        let mut tag = tag.cloned().unwrap_or_else(|| new_tag(pos, CHEST));
        tag.insert(
            "Items",
            Nbt::List(write_items(slots, self.block_item_codec())?),
        );
        Ok(tag)
    }

    pub(crate) fn chest_items(&self, pos: BlockPos) -> io::Result<[ItemStack; CHEST_SLOTS]> {
        read_chest_items(self.chest_tag(pos)?, self.block_item_codec())
    }

    /// Removal reads the chest being destroyed, even if it has a lock, an obstructed lid or a paired half.
    pub(crate) fn unpack_chest_items(
        &mut self,
        pos: BlockPos,
    ) -> io::Result<[ItemStack; CHEST_SLOTS]> {
        let mut loot = ChestLoot::default();
        let slots = self.prepare_chest_items(pos, self.chest_tag(pos)?, false, &mut loot);
        loot.finish(self, slots)
    }

    pub(crate) fn chest_menu(&self, chest: Chest) -> io::Result<(Vec<ItemStack>, Option<Nbt>)> {
        self.chest_menu_using(chest, |_, tag| {
            read_chest_items(tag, self.block_item_codec())
        })
    }

    pub(crate) fn open_chest_menu(
        &mut self,
        chest: Chest,
        spectator: bool,
    ) -> io::Result<(Vec<ItemStack>, Option<Nbt>)> {
        let mut loot = ChestLoot::default();
        let menu = self.chest_menu_using(chest, |pos, tag| {
            self.prepare_chest_items(pos, tag, spectator, &mut loot)
        });
        loot.finish(self, menu)
    }

    fn prepare_chest_items(
        &self,
        pos: BlockPos,
        tag: Option<&Compound>,
        spectator: bool,
        loot: &mut ChestLoot,
    ) -> io::Result<[ItemStack; CHEST_SLOTS]> {
        let Some(tag) = tag.filter(|tag| tag.get("LootTable").is_some()) else {
            return read_chest_items(tag, self.block_item_codec());
        };
        if spectator {
            return Err(invalid("spectators cannot open a chest with pending loot"));
        }
        let chests = self
            .loot
            .as_ref()
            .and_then(|loot| loot.chests.as_ref())
            .ok_or_else(|| invalid("chest loot data is unavailable"))?;
        let (table, seed) = chest_loot_table(tag, chests)?;
        let map = table.has_treasure_map().then_some(MapChest { pos, seed });
        let preview = map.and_then(|chest| self.maps.prepared(chest));
        let treasure_map = if let Some(Some((_, target))) = preview {
            let first = match loot.first_map_id {
                Some(id) => id,
                None => self.maps.next_id()?,
            };
            let id = first
                .checked_add(loot.new_maps.len() as i32)
                .ok_or_else(|| io::Error::other("map IDs exhausted"))?;
            Some(TreasureMap {
                id,
                target: *target,
            })
        } else {
            None
        };
        let mut seeded = None;
        let random = self.loot_random(table, seed, loot, &mut seeded);
        let codec = self.block_item_codec();
        let (slots, requested) = chests.fill(table, codec, random, treasure_map)?;
        if requested && preview.is_none() {
            loot.search = map;
            return Err(pending());
        }
        if let (Some(treasure), Some(Some((data, _)))) = (treasure_map, preview)
            && slots.iter().any(|stack| treasure.is_on(stack, codec))
        {
            loot.first_map_id.get_or_insert(treasure.id);
            loot.new_maps.push(data.clone());
        }
        let mut updated = tag.clone();
        updated.remove("LootTable");
        updated.remove("LootTableSeed");
        updated.insert(
            "Items",
            Nbt::List(write_items(&slots, self.block_item_codec())?),
        );
        loot.tags.push((pos, updated));
        loot.maps.extend(map);
        Ok(slots)
    }

    fn chest_menu_using(
        &self,
        chest: Chest,
        mut items_at: impl FnMut(BlockPos, Option<&Compound>) -> io::Result<[ItemStack; CHEST_SLOTS]>,
    ) -> io::Result<(Vec<ItemStack>, Option<Nbt>)> {
        let mut items = Vec::with_capacity(CHEST_SLOTS * (1 + usize::from(chest.is_double())));
        let mut title = None;
        for pos in chest.positions() {
            let tag = self.chest_tag(pos)?;
            if let Some(tag) = tag {
                if tag.get("lock").is_some() {
                    return Err(invalid("chest is locked"));
                }
                if title.is_none() {
                    title = tag.get("CustomName").cloned();
                }
            }
            items.extend(items_at(pos, tag)?);
        }
        Ok((items, title))
    }

    pub(crate) fn save_chest_menu(
        &mut self,
        chest: Chest,
        before: &[ItemStack],
        updated: &[ItemStack],
    ) -> io::Result<()> {
        // Encode every changed half before writing either. A click can move an item across chunks.
        let mut tags = Vec::new();
        for (index, pos) in chest.positions().enumerate() {
            let range = index * CHEST_SLOTS..(index + 1) * CHEST_SLOTS;
            if updated[range.clone()] != before[range.clone()] {
                tags.push((
                    pos,
                    self.chest_with_items(pos, self.chest_tag(pos)?, &updated[range])?,
                ));
            }
        }
        for (pos, tag) in tags {
            self.store_container_tag(pos, tag);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_prefers_the_clockwise_single_and_sneaking_requires_a_side_click() {
        let pos = BlockPos { x: 0, y: 65, z: 0 };
        let state = |facing: Direction, kind| {
            block_states()
                .state(CHEST, [("facing", facing.name()), ("type", kind)])
                .unwrap()
                .0
        };
        let air = block_states().default_state("minecraft:air").unwrap();
        for facing in [
            Direction::North,
            Direction::East,
            Direction::South,
            Direction::West,
        ] {
            let clockwise = facing.clockwise();
            let beside = relative(pos, clockwise);
            let other = relative(pos, clockwise.opposite());
            let singles = |at| {
                if at == beside || at == other {
                    state(facing, "single")
                } else {
                    air
                }
            };
            assert_eq!(
                placement(pos, Direction::Up, facing, false, singles),
                (facing, "left")
            );
            assert_eq!(
                placement(pos, Direction::Up, facing, true, singles),
                (facing, "single")
            );
            assert_eq!(
                placement(pos, clockwise, clockwise, true, singles),
                (facing, "right")
            );
            let double = |at| {
                if at == beside {
                    state(facing, "right")
                } else {
                    air
                }
            };
            assert_eq!(
                placement(pos, Direction::Up, facing, false, double),
                (facing, "single")
            );
            let in_front = |at| {
                if at == relative(pos, facing) {
                    state(facing, "single")
                } else {
                    air
                }
            };
            assert_eq!(
                placement(pos, facing.opposite(), facing, true, in_front),
                (facing, "single")
            );
        }
    }
}
