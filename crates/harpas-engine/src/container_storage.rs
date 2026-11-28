//! Saved inventory slots and block entity NBT shared by persistent menus.

use std::io;

use harpas_core::nbt::{Compound, List, Nbt};

use crate::block::BlockPos;
use crate::inventory::ItemStack;
use crate::item_data::{ItemCodec, invalid};
use crate::world::{ChunkState, World};

pub(crate) fn read_items<const SLOTS: usize>(
    tag: Option<&Compound>,
    codec: &ItemCodec,
) -> io::Result<[ItemStack; SLOTS]> {
    let mut slots = std::array::from_fn(|_| ItemStack::empty());
    let Some(tag) = tag else { return Ok(slots) };
    let items = match tag.get("Items") {
        None | Some(Nbt::List(List::Empty)) => return Ok(slots),
        Some(Nbt::List(List::Compound(items))) => items,
        _ => return Err(invalid("invalid container item list")),
    };
    let mut seen = [false; SLOTS];
    for item in items {
        let Some(Nbt::Byte(slot)) = item.get("Slot") else {
            return Err(invalid("container item has no byte Slot"));
        };
        let slot = usize::try_from(*slot)
            .ok()
            .filter(|&slot| slot < SLOTS)
            .ok_or_else(|| invalid("container item slot is out of range"))?;
        if std::mem::replace(&mut seen[slot], true) {
            return Err(invalid("duplicate container item slot"));
        }
        slots[slot] = codec.read(item)?;
    }
    Ok(slots)
}

pub(crate) fn write_items(slots: &[ItemStack], codec: &ItemCodec) -> io::Result<List> {
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

pub(crate) fn new_tag(pos: BlockPos, kind: &str) -> Compound {
    Compound::from_iter([
        ("id", Nbt::from(kind)),
        ("x", Nbt::Int(pos.x)),
        ("y", Nbt::Int(pos.y)),
        ("z", Nbt::Int(pos.z)),
    ])
}

impl World {
    pub(crate) fn block_item_codec(&self) -> &ItemCodec {
        self.player_store
            .as_ref()
            .map_or_else(|| ItemCodec::fallback(), |store| store.codec.as_ref())
    }

    pub(crate) fn container_tag(&self, pos: BlockPos, kind: &str) -> io::Result<Option<&Compound>> {
        let tag = self.block_entity_tag(pos)?;
        if tag.is_some_and(|tag| !matches!(tag.get("id"), Some(Nbt::String(id)) if id == kind)) {
            return Err(invalid("unexpected block entity at container position"));
        }
        Ok(tag)
    }

    pub(crate) fn block_entity_tag(&self, pos: BlockPos) -> io::Result<Option<&Compound>> {
        let Some(ChunkState::Loaded(chunk)) = self.chunk(pos.chunk()) else {
            return Err(invalid("container chunk is no longer loaded"));
        };
        let mut matching = block_entities(&chunk.rest)?
            .iter()
            .filter(|entity| at(entity, pos));
        let tag = matching.next();
        if matching.next().is_some() {
            return Err(invalid("duplicate block entities at container position"));
        }
        Ok(tag)
    }

    pub(crate) fn store_container_tag(&mut self, pos: BlockPos, tag: Compound) {
        let chunk = self
            .chunk_contents_mut(pos.chunk())
            .expect("container tag was prepared in a loaded chunk");
        let mut entities = match chunk.rest.remove("block_entities") {
            Some(Nbt::List(List::Compound(entities))) => entities,
            _ => Vec::new(), // Already checked by container_tag.
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
}
