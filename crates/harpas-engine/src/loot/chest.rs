//! The supported vanilla chest tables and the placement of their rolls into empty chests.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek};

use harpas_core::block::block_states;
use harpas_core::item::items;
use harpas_core::nbt::{Compound, Nbt};
use harpas_core::random::RandomSource;
use log::warn;
use serde_json::Value;
use zip::ZipArchive;

use super::{Context, LootTable};
use crate::inventory::{CHEST_SLOTS, ItemStack};
use crate::item_data::{ItemCodec, invalid};
use crate::maps::{MAP_TABLE, TreasureMap};

pub(super) const ENCHANTMENT_TAG: &str = "#minecraft:on_random_loot";

#[derive(Debug)]
pub(crate) struct ChestLootTables {
    tables: HashMap<&'static str, LootTable>,
    enchantments: LootEnchantments,
}

impl ChestLootTables {
    pub(super) fn load(archive: &mut ZipArchive<impl Read + Seek>) -> io::Result<Option<Self>> {
        let mut tables = HashMap::new();
        for name in [
            "chests/simple_dungeon",
            "chests/desert_pyramid",
            "chests/buried_treasure",
            "chests/shipwreck_treasure",
            "chests/shipwreck_supply",
            MAP_TABLE,
        ] {
            let data = match archive.by_name(&format!("data/minecraft/loot_table/{name}.json")) {
                Ok(entry) => serde_json::from_reader(entry).map_err(io::Error::other),
                Err(zip::result::ZipError::FileNotFound) => continue,
                Err(error) => Err(io::Error::other(error)),
            };
            match data {
                Ok(data) => {
                    tables.insert(name, LootTable::parse(&data));
                }
                Err(error) => warn!("Cannot load chest loot table {name}: {error}"),
            }
        }
        if tables.is_empty() {
            return Ok(None);
        }
        let mut names = Vec::new();
        collect_tag(
            archive,
            "enchantment",
            ENCHANTMENT_TAG,
            &mut Vec::new(),
            &mut names,
        )?;
        let mut enchantments = Vec::new();
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
            let supported = definition
                .get("supported_items")
                .map(|value| supported_items(archive, value))
                .transpose()?
                .unwrap_or_default();
            enchantments.push(LootEnchantment {
                name,
                max_level,
                supported,
            });
        }
        if enchantments.is_empty() {
            return Err(invalid("chest loot enchantment tag is empty"));
        }
        Ok(Some(Self {
            tables,
            enchantments: LootEnchantments(enchantments),
        }))
    }

    pub(crate) fn get(&self, id: &str) -> Option<&LootTable> {
        self.tables.get(id.strip_prefix("minecraft:").unwrap_or(id))
    }

    pub(crate) fn fill(
        &self,
        table: &LootTable,
        codec: &ItemCodec,
        random: &mut dyn RandomSource,
        treasure_map: Option<TreasureMap>,
    ) -> io::Result<[ItemStack; CHEST_SLOTS]> {
        let rolled = table.roll(Context {
            state: block_states().default_state("minecraft:air").unwrap(),
            tool: &ItemStack::empty(),
            random,
            chest: Some((&self.enchantments, codec)),
            treasure_map,
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
fn collect_tag(
    archive: &mut ZipArchive<impl Read + Seek>,
    registry: &str,
    tag: &str,
    active: &mut Vec<String>,
    names: &mut Vec<String>,
) -> io::Result<()> {
    let path = tag
        .strip_prefix("#minecraft:")
        .ok_or_else(|| invalid(format!("invalid {registry} tag {tag}")))?;
    if active.iter().any(|name| name == path) {
        return Err(invalid(format!("recursive {registry} tag {tag}")));
    }
    active.push(path.to_owned());
    let data = json(
        archive,
        &format!("data/minecraft/tags/{registry}/{path}.json"),
    )?;
    let entries = data["values"]
        .as_array()
        .ok_or_else(|| invalid(format!("invalid entries for {registry} tag {tag}")))?;
    for value in entries {
        let name = value
            .as_str()
            .ok_or_else(|| invalid(format!("invalid entry in {registry} tag {tag}")))?;
        if name.starts_with('#') {
            collect_tag(archive, registry, name, active, names)?;
        } else if !names.iter().any(|id| id == name) {
            names.push(name.to_owned());
        }
    }
    active.pop();
    Ok(())
}

fn supported_items(
    archive: &mut ZipArchive<impl Read + Seek>,
    value: &Value,
) -> io::Result<HashSet<u32>> {
    let mut names = Vec::new();
    match value {
        Value::String(tag) if tag.starts_with('#') => {
            collect_tag(archive, "item", tag, &mut Vec::new(), &mut names)?;
        }
        Value::String(name) => names.push(name.clone()),
        Value::Array(entries) => {
            for entry in entries {
                let name = entry
                    .as_str()
                    .ok_or_else(|| invalid("invalid supported enchantment item"))?;
                names.push(name.to_owned());
            }
        }
        _ => return Err(invalid("invalid supported enchantment items")),
    }
    names
        .iter()
        .map(|name| {
            items()
                .id(name)
                .ok_or_else(|| invalid(format!("unknown item {name}")))
        })
        .collect()
}

#[derive(Debug)]
struct LootEnchantment {
    name: String,
    max_level: i32,
    supported: HashSet<u32>,
}

#[derive(Debug)]
pub(super) struct LootEnchantments(Vec<LootEnchantment>);

impl LootEnchantments {
    pub(super) fn enchant(
        &self,
        stack: &mut ItemStack,
        codec: &ItemCodec,
        random: &mut dyn RandomSource,
    ) -> io::Result<()> {
        let book = stack.name() == Some("minecraft:book");
        // Loot enchantments use supported items, including gear outside an enchantment's primary items.
        let options: Vec<_> = self
            .0
            .iter()
            .filter(|enchantment| book || enchantment.supported.contains(&stack.item))
            .collect();
        if options.is_empty() {
            return Ok(());
        }
        let enchantment = options[random.next_int_bounded(options.len() as i32) as usize];
        let level = if enchantment.max_level == 1 {
            1
        } else {
            random.next_int_between_inclusive(1, enchantment.max_level)
        };
        let enchantments = Nbt::Compound(Compound::from_iter([(enchantment.name.as_str(), level)]));
        let component = if book {
            stack.item = items().id("minecraft:enchanted_book").unwrap();
            "minecraft:stored_enchantments"
        } else {
            "minecraft:enchantments"
        };
        codec.set_saved_component(stack, component, &enchantments)
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
