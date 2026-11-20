//! Ordinary chest block entities use the chunk's saved NBT as the authoritative inventory.

use std::io;
use std::sync::OnceLock;

use harpas_core::block::block_states;
use harpas_core::block_behavior::block_behavior;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_core::random::{LegacyRandom, RandomSequence, RandomSource};
use harpas_worldgen::shape_update::Direction;

use crate::block::BlockPos;
use crate::inventory::{CHEST_SLOTS, ItemStack};
use crate::item_data::{ItemCodec, invalid};
use crate::loot::DUNGEON_CHEST;
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

fn read_items(tag: Option<&Compound>, codec: &ItemCodec) -> io::Result<[ItemStack; CHEST_SLOTS]> {
    let mut slots = std::array::from_fn(|_| ItemStack::empty());
    let Some(tag) = tag else { return Ok(slots) };
    if tag.get("LootTable").is_some() {
        return Err(invalid("chest has an unopened loot table"));
    }
    let items = match tag.get("Items") {
        None | Some(Nbt::List(List::Empty)) => return Ok(slots),
        Some(Nbt::List(List::Compound(items))) => items,
        _ => return Err(invalid("invalid chest item list")),
    };
    let mut seen = [false; CHEST_SLOTS];
    for item in items {
        let Some(Nbt::Byte(slot)) = item.get("Slot") else {
            return Err(invalid("chest item has no byte Slot"));
        };
        let slot = usize::try_from(*slot)
            .ok()
            .filter(|&slot| slot < CHEST_SLOTS)
            .ok_or_else(|| invalid("chest item slot is out of range"))?;
        if std::mem::replace(&mut seen[slot], true) {
            return Err(invalid("duplicate chest item slot"));
        }
        slots[slot] = codec.read(item)?;
    }
    Ok(slots)
}

fn write_items(slots: &[ItemStack], codec: &ItemCodec) -> io::Result<List> {
    let mut items = Vec::new();
    for (slot, stack) in slots
        .iter()
        .enumerate()
        .filter(|(_, stack)| !stack.is_empty())
    {
        let mut tag = codec.write(stack)?;
        tag.insert("Slot", slot as i8);
        items.push(tag);
    }
    Ok(if items.is_empty() {
        List::Empty
    } else {
        List::Compound(items)
    })
}

fn at(entity: &Compound, pos: BlockPos) -> bool {
    entity.get("x") == Some(&Nbt::Int(pos.x))
        && entity.get("y") == Some(&Nbt::Int(pos.y))
        && entity.get("z") == Some(&Nbt::Int(pos.z))
}

fn block_entities(rest: &Compound) -> io::Result<&[Compound]> {
    match rest.get("block_entities") {
        None | Some(Nbt::List(List::Empty)) => Ok(&[]),
        Some(Nbt::List(List::Compound(entities))) => Ok(entities),
        _ => Err(invalid("invalid chunk block entity list")),
    }
}

fn new_tag(pos: BlockPos) -> Compound {
    Compound::from_iter([
        ("id", Nbt::from(CHEST)),
        ("x", Nbt::Int(pos.x)),
        ("y", Nbt::Int(pos.y)),
        ("z", Nbt::Int(pos.z)),
    ])
}

/// Loot and random state are committed together after every requested chest half can be read.
#[derive(Default)]
struct ChestLoot {
    tags: Vec<(BlockPos, Compound)>,
    sequence: Option<(String, RandomSequence)>,
    world_random: Option<LegacyRandom>,
}

impl ChestLoot {
    fn commit(self, world: &mut World) {
        for (pos, tag) in self.tags {
            world.store_chest_tag(pos, tag);
        }
        if let Some((name, sequence)) = self.sequence {
            world.random_sequences.insert(name, sequence);
        }
        if let Some(random) = self.world_random {
            world.random = random;
        }
    }
}

impl World {
    fn chest_codec(&self) -> &ItemCodec {
        static DEFAULT: OnceLock<ItemCodec> = OnceLock::new();
        self.player_store.as_ref().map_or_else(
            || DEFAULT.get_or_init(ItemCodec::default),
            |store| store.codec.as_ref(),
        )
    }

    pub(crate) fn chest_tag(&self, pos: BlockPos) -> io::Result<Option<&Compound>> {
        let Some(ChunkState::Loaded(chunk)) = self.chunk(pos.chunk()) else {
            return Err(invalid("chest chunk is no longer loaded"));
        };
        let mut matching = block_entities(&chunk.rest)?
            .iter()
            .filter(|entity| at(entity, pos));
        let tag = matching.next();
        if tag.is_some_and(|tag| !matches!(tag.get("id"), Some(Nbt::String(id)) if id == CHEST))
            || matching.next().is_some()
        {
            return Err(invalid("unexpected block entity at chest position"));
        }
        Ok(tag)
    }

    pub(crate) fn save_chest(&mut self, pos: BlockPos, slots: &[ItemStack]) -> io::Result<()> {
        let tag = self.chest_with_items(pos, slots)?;
        self.store_chest_tag(pos, tag);
        Ok(())
    }

    fn chest_with_items(&self, pos: BlockPos, slots: &[ItemStack]) -> io::Result<Compound> {
        let mut tag = self
            .chest_tag(pos)?
            .cloned()
            .unwrap_or_else(|| new_tag(pos));
        tag.insert("Items", Nbt::List(write_items(slots, self.chest_codec())?));
        Ok(tag)
    }

    fn store_chest_tag(&mut self, pos: BlockPos, tag: Compound) {
        let chunk = self
            .chunk_contents_mut(pos.chunk())
            .expect("chest tag was prepared in a loaded chunk");
        let mut entities = match chunk.rest.remove("block_entities") {
            Some(Nbt::List(List::Compound(entities))) => entities,
            _ => Vec::new(), // Already checked by chest_tag.
        };
        if let Some(existing) = entities.iter_mut().find(|entity| at(entity, pos)) {
            *existing = tag;
        } else {
            entities.push(tag);
        }
        chunk
            .rest
            .insert("block_entities", Nbt::List(List::Compound(entities)));
    }

    pub(crate) fn chest_items(&self, pos: BlockPos) -> io::Result<[ItemStack; CHEST_SLOTS]> {
        read_items(self.chest_tag(pos)?, self.chest_codec())
    }

    /// Removal reads the chest being destroyed, even if it has a lock, an obstructed lid or a paired half.
    pub(crate) fn unpack_chest_items(
        &mut self,
        pos: BlockPos,
    ) -> io::Result<[ItemStack; CHEST_SLOTS]> {
        let mut loot = ChestLoot::default();
        let slots = self.prepare_chest_items(pos, self.chest_tag(pos)?, false, &mut loot)?;
        loot.commit(self);
        Ok(slots)
    }

    pub(crate) fn chest_menu(&self, chest: Chest) -> io::Result<(Vec<ItemStack>, Option<Nbt>)> {
        self.chest_menu_using(chest, |_, tag| read_items(tag, self.chest_codec()))
    }

    pub(crate) fn open_chest_menu(
        &mut self,
        chest: Chest,
        spectator: bool,
    ) -> io::Result<(Vec<ItemStack>, Option<Nbt>)> {
        let mut loot = ChestLoot::default();
        let menu = self.chest_menu_using(chest, |pos, tag| {
            self.prepare_chest_items(pos, tag, spectator, &mut loot)
        })?;
        loot.commit(self);
        Ok(menu)
    }

    fn prepare_chest_items(
        &self,
        pos: BlockPos,
        tag: Option<&Compound>,
        spectator: bool,
        loot: &mut ChestLoot,
    ) -> io::Result<[ItemStack; CHEST_SLOTS]> {
        let Some(tag) = tag.filter(|tag| tag.get("LootTable").is_some()) else {
            return read_items(tag, self.chest_codec());
        };
        if spectator {
            return Err(invalid("spectators cannot open a chest with pending loot"));
        }
        if !matches!(tag.get("LootTable"), Some(Nbt::String(id))
            if id == DUNGEON_CHEST || id == "chests/simple_dungeon")
        {
            return Err(invalid("unsupported chest loot table"));
        }
        let dungeon = self
            .loot
            .as_ref()
            .and_then(|loot| loot.dungeon.as_ref())
            .ok_or_else(|| invalid("dungeon loot table is unavailable"))?;
        let seed = match tag.get("LootTableSeed") {
            None => 0,
            Some(Nbt::Long(seed)) => *seed,
            _ => return Err(invalid("invalid chest loot seed")),
        };
        let mut seeded;
        let random: &mut dyn RandomSource = if seed != 0 {
            seeded = LegacyRandom::new(seed);
            &mut seeded
        } else if let Some(name) = &dungeon.table.random_sequence {
            let (_, sequence) = loot.sequence.get_or_insert_with(|| {
                let sequence = self
                    .random_sequences
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| RandomSequence::new(self.seed, Some(name)));
                (name.clone(), sequence)
            });
            sequence.random()
        } else {
            loot.world_random.get_or_insert_with(|| self.random.clone())
        };
        let slots = dungeon.fill(self.chest_codec(), random)?;
        let mut updated = tag.clone();
        updated.remove("LootTable");
        updated.remove("LootTableSeed");
        updated.insert("Items", Nbt::List(write_items(&slots, self.chest_codec())?));
        loot.tags.push((pos, updated));
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
                tags.push((pos, self.chest_with_items(pos, &updated[range])?));
            }
        }
        for (pos, tag) in tags {
            self.store_chest_tag(pos, tag);
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
