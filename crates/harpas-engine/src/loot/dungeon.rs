//! The shipped dungeon table and the placement of its rolls into an empty chest.

use std::io::{self, Read, Seek};

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, Nbt};
use harpas_core::random::RandomSource;
use serde_json::Value;
use zip::ZipArchive;

use super::{Context, LootTable};
use crate::inventory::{CHEST_SLOTS, ItemStack};
use crate::item_data::{ItemCodec, invalid};

pub(crate) const DUNGEON_CHEST: &str = "minecraft:chests/simple_dungeon";
pub(super) const BOOK_TAG: &str = "#minecraft:on_random_loot";

#[derive(Debug)]
pub(crate) struct DungeonLoot {
    pub(crate) table: LootTable,
    books: BookEnchantments,
}

impl DungeonLoot {
    pub(super) fn load(archive: &mut ZipArchive<impl Read + Seek>) -> io::Result<Option<Self>> {
        let data = match archive.by_name("data/minecraft/loot_table/chests/simple_dungeon.json") {
            Ok(entry) => serde_json::from_reader(entry).map_err(io::Error::other)?,
            Err(zip::result::ZipError::FileNotFound) => return Ok(None),
            Err(error) => return Err(io::Error::other(error)),
        };
        let mut names = Vec::new();
        book_options(archive, BOOK_TAG, &mut Vec::new(), &mut names)?;
        let mut books = Vec::new();
        for name in names {
            let path = name
                .strip_prefix("minecraft:")
                .ok_or_else(|| invalid("invalid loot enchantment ID"))?;
            let definition = json(archive, &format!("data/minecraft/enchantment/{path}.json"))?;
            let max_level = definition["max_level"]
                .as_i64()
                .and_then(|level| i32::try_from(level).ok())
                .filter(|level| *level > 0)
                .ok_or_else(|| invalid("invalid loot enchantment level"))?;
            books.push((name, max_level));
        }
        if books.is_empty() {
            return Err(invalid("dungeon loot enchantment tag is empty"));
        }
        Ok(Some(Self {
            table: LootTable::parse(&data),
            books: BookEnchantments(books),
        }))
    }

    pub(crate) fn fill(
        &self,
        codec: &ItemCodec,
        random: &mut dyn RandomSource,
    ) -> io::Result<[ItemStack; CHEST_SLOTS]> {
        let rolled = self.table.roll(Context {
            state: block_states().default_state("minecraft:air").unwrap(),
            tool: &ItemStack::empty(),
            random,
            books: Some((&self.books, codec)),
        })?;
        let mut slots: [usize; CHEST_SLOTS] = std::array::from_fn(|i| i);
        shuffle(&mut slots, random);
        let mut contents = std::array::from_fn(|_| ItemStack::empty());
        for (slot, stack) in slots.into_iter().rev().zip(spread(rolled, random)) {
            contents[slot] = stack;
        }
        Ok(contents)
    }
}

fn json(archive: &mut ZipArchive<impl Read + Seek>, path: &str) -> io::Result<Value> {
    let entry = archive.by_name(path).map_err(io::Error::other)?;
    serde_json::from_reader(entry).map_err(io::Error::other)
}

// Tag entries keep their order across nested tags and duplicate IDs.
fn book_options(
    archive: &mut ZipArchive<impl Read + Seek>,
    tag: &str,
    active: &mut Vec<String>,
    names: &mut Vec<String>,
) -> io::Result<()> {
    let path = tag
        .strip_prefix("#minecraft:")
        .ok_or_else(|| invalid("invalid loot enchantment tag"))?;
    if active.iter().any(|name| name == path) {
        return Err(invalid("recursive loot enchantment tag"));
    }
    active.push(path.to_owned());
    let data = json(
        archive,
        &format!("data/minecraft/tags/enchantment/{path}.json"),
    )?;
    let entries = data["values"]
        .as_array()
        .ok_or_else(|| invalid("invalid loot enchantment tag entries"))?;
    for value in entries {
        let name = value
            .as_str()
            .ok_or_else(|| invalid("invalid loot enchantment tag entry"))?;
        if name.starts_with('#') {
            book_options(archive, name, active, names)?;
        } else if !names.iter().any(|id| id == name) {
            names.push(name.to_owned());
        }
    }
    active.pop();
    Ok(())
}

#[derive(Debug)]
pub(super) struct BookEnchantments(Vec<(String, i32)>);

impl BookEnchantments {
    pub(super) fn enchant(
        &self,
        count: i32,
        codec: &ItemCodec,
        random: &mut dyn RandomSource,
    ) -> io::Result<ItemStack> {
        let (name, max_level) = &self.0[random.next_int_bounded(self.0.len() as i32) as usize];
        let level = if *max_level == 1 {
            1
        } else {
            random.next_int_between_inclusive(1, *max_level)
        };
        let enchantments = Compound::from_iter([(name.as_str(), Nbt::Int(level))]);
        let components =
            Compound::from_iter([("minecraft:stored_enchantments", Nbt::Compound(enchantments))]);
        codec.read(&Compound::from_iter([
            ("id", Nbt::from("minecraft:enchanted_book")),
            ("count", Nbt::Int(count)),
            ("components", Nbt::Compound(components)),
        ]))
    }
}

fn shuffle<T>(values: &mut [T], random: &mut dyn RandomSource) {
    for end in (2..=values.len()).rev() {
        values.swap(end - 1, random.next_int_bounded(end as i32) as usize);
    }
}

fn spread(rolled: Vec<ItemStack>, random: &mut dyn RandomSource) -> Vec<ItemStack> {
    let (mut stacks, mut splittable): (Vec<_>, Vec<_>) =
        rolled.into_iter().partition(|stack| stack.count == 1);
    while stacks.len() + splittable.len() < CHEST_SLOTS && !splittable.is_empty() {
        let index = match splittable.len() {
            1 => 0,
            len => random.next_int_bounded(len as i32) as usize,
        };
        let mut stack = splittable.remove(index);
        let count = match stack.count / 2 {
            1 => 1,
            max => random.next_int_between_inclusive(1, max),
        };
        stack.count -= count;
        for half in [stack.clone(), ItemStack { count, ..stack }] {
            if half.count > 1 && random.next_bool() {
                splittable.push(half);
            } else {
                stacks.push(half);
            }
        }
    }
    stacks.extend(splittable);
    shuffle(&mut stacks, random);
    stacks
}
