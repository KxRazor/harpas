//! Ordinary chest block entities use the chunk's saved NBT as the authoritative inventory.

use std::io;
use std::sync::OnceLock;

use harpas_core::block::block_states;
use harpas_core::block_behavior::block_behavior;
use harpas_core::nbt::{Compound, List, Nbt};

use crate::block::BlockPos;
use crate::inventory::{CHEST_SLOTS, ItemStack};
use crate::item_data::{ItemCodec, invalid};
use crate::world::{ChunkState, World};

pub(crate) const CHEST: &str = "minecraft:chest";

pub(crate) fn is_single(state: u32) -> bool {
    block_states()
        .properties(state)
        .is_some_and(|(name, properties)| name == CHEST && properties.contains(&("type", "single")))
}

pub(crate) fn blocks_opening(state: u32) -> bool {
    block_behavior().is_solid_render(state)
}

pub(crate) fn read_items(
    tag: Option<&Compound>,
    codec: &ItemCodec,
) -> io::Result<[ItemStack; CHEST_SLOTS]> {
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

impl World {
    pub(crate) fn chest_codec(&self) -> &ItemCodec {
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
        let mut tag = self
            .chest_tag(pos)?
            .cloned()
            .unwrap_or_else(|| new_tag(pos));
        tag.insert("Items", Nbt::List(write_items(slots, self.chest_codec())?));
        let chunk = self
            .chunk_contents_mut(pos.chunk())
            .ok_or_else(|| invalid("chest chunk is no longer loaded"))?;
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
        Ok(())
    }

    pub(crate) fn chest_items(&self, pos: BlockPos) -> io::Result<[ItemStack; CHEST_SLOTS]> {
        read_items(self.chest_tag(pos)?, self.chest_codec())
    }
}
