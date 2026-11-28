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

use super::{Context, Function, LootTable, MAX_WORK};
use crate::inventory::{CHEST_SLOTS, ItemStack};
use crate::item_data::{ItemCodec, invalid};
use crate::maps::TreasureMap;

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
            "chests/shipwreck_map",
            "chests/underwater_ruin_small",
            "chests/underwater_ruin_big",
        ] {
            let data = match archive.by_name(&format!("data/minecraft/loot_table/{name}.json")) {
                Ok(entry) => serde_json::from_reader(entry).map_err(io::Error::other),
                Err(zip::result::ZipError::FileNotFound) => continue,
                Err(error) => Err(io::Error::other(error)),
            };
            match data.and_then(|data| LootTable::parse(&data)) {
                Ok(table) => {
                    tables.insert(name, table);
                }
                Err(error) => warn!("Cannot load chest loot table {name}: {error}"),
            }
        }
        let enchantments = if tables
            .values()
            .any(|table| table.has_function(&Function::EnchantRandomly))
        {
            match load_enchantments(archive) {
                Ok(enchantments) => enchantments,
                Err(error) => {
                    warn!("Cannot load chest loot enchantments: {error}");
                    tables.retain(|_, table| !table.has_function(&Function::EnchantRandomly));
                    LootEnchantments(Vec::new())
                }
            }
        } else {
            LootEnchantments(Vec::new())
        };
        Ok((!tables.is_empty()).then_some(Self {
            tables,
            enchantments,
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
    ) -> io::Result<([ItemStack; CHEST_SLOTS], bool)> {
        let mut context = Context {
            state: Some(block_states().default_state("minecraft:air").unwrap()),
            tool: &ItemStack::empty(),
            looting_level: 0,
            random: &mut *random,
            chest: Some((&self.enchantments, codec)),
            treasure_map,
            treasure_requested: false,
            remaining_work: MAX_WORK,
        };
        let rolled = table.roll(&mut context)?;
        let requested = context.treasure_requested;
        let mut slots: [usize; CHEST_SLOTS] = std::array::from_fn(|i| i);
        shuffle(&mut slots, random);
        let mut contents = std::array::from_fn(|_| ItemStack::empty());
        for (slot, stack) in slots.into_iter().rev().zip(spread(rolled, random)) {
            contents[slot] = stack;
        }
        Ok((contents, requested))
    }
}

fn json(archive: &mut ZipArchive<impl Read + Seek>, path: &str) -> io::Result<Value> {
    let entry = archive.by_name(path).map_err(|error| match error {
        zip::result::ZipError::FileNotFound => {
            io::Error::new(io::ErrorKind::NotFound, path.to_owned())
        }
        _ => io::Error::other(error),
    })?;
    serde_json::from_reader(entry).map_err(io::Error::other)
}

fn resource(id: &str) -> io::Result<(&str, &str)> {
    let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", id));
    let name_byte =
        |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_.-".contains(&byte);
    if namespace.is_empty()
        || !namespace.bytes().all(name_byte)
        || !path
            .split('/')
            .all(|part| !matches!(part, "" | "." | "..") && part.bytes().all(name_byte))
    {
        return Err(invalid(format!("invalid loot registry ID {id}")));
    }
    Ok((namespace, path))
}

fn load_enchantments(archive: &mut ZipArchive<impl Read + Seek>) -> io::Result<LootEnchantments> {
    let mut names = Vec::new();
    collect_tag(
        archive,
        "enchantment",
        ENCHANTMENT_TAG,
        true,
        &mut Vec::new(),
        &mut names,
    )?;
    let mut enchantments = Vec::new();
    for (name, required) in names {
        let (namespace, path) = resource(&name)?;
        let definition = match json(
            archive,
            &format!("data/{namespace}/enchantment/{path}.json"),
        ) {
            Err(error) if !required && error.kind() == io::ErrorKind::NotFound => continue,
            result => result?,
        };
        let max_level = super::integer(&definition["max_level"])?;
        if !(1..=255).contains(&max_level) {
            return Err(invalid("invalid loot enchantment level"));
        }
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
    Ok(LootEnchantments(enchantments))
}

// Tag entries keep their order across nested tags and duplicate IDs.
fn collect_tag(
    archive: &mut ZipArchive<impl Read + Seek>,
    registry: &str,
    tag: &str,
    required: bool,
    active: &mut Vec<String>,
    names: &mut Vec<(String, bool)>,
) -> io::Result<()> {
    let name = tag
        .strip_prefix('#')
        .ok_or_else(|| invalid(format!("invalid {registry} tag {tag}")))?;
    let (namespace, path) = resource(name)?;
    let name = format!("{namespace}:{path}");
    if active.len() >= 64 || active.contains(&name) {
        return Err(invalid(format!(
            "recursive or too deeply nested {registry} tag {tag}"
        )));
    }
    let data = match json(
        archive,
        &format!("data/{namespace}/tags/{registry}/{path}.json"),
    ) {
        Err(error) if !required && error.kind() == io::ErrorKind::NotFound => return Ok(()),
        result => result?,
    };
    let entries = super::array(super::required(&data, "values")?)?;
    active.push(name);
    for value in entries {
        let (name, required) = if let Some(name) = value.as_str() {
            (name, true)
        } else {
            let name = super::string(value, "id")?;
            let required = value
                .get("required")
                .map(|flag| {
                    flag.as_bool()
                        .ok_or_else(|| invalid("invalid loot tag required flag"))
                })
                .transpose()?
                .unwrap_or(true);
            (name, required)
        };
        if name.starts_with('#') {
            collect_tag(archive, registry, name, required, active, names)?;
        } else {
            let (namespace, path) = resource(name)?;
            let name = format!("{namespace}:{path}");
            if let Some((_, was_required)) = names.iter_mut().find(|(id, _)| *id == name) {
                *was_required |= required;
            } else {
                names.push((name, required));
            }
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
            collect_tag(archive, "item", tag, true, &mut Vec::new(), &mut names)?;
        }
        Value::String(name) => names.push((name.clone(), true)),
        Value::Array(entries) => {
            for entry in entries {
                let name = entry
                    .as_str()
                    .ok_or_else(|| invalid("invalid supported enchantment item"))?;
                names.push((name.to_owned(), true));
            }
        }
        _ => return Err(invalid("invalid supported enchantment items")),
    }
    let mut supported = HashSet::new();
    for (name, required) in names {
        if let Some(id) = super::item_id(&name) {
            supported.insert(id);
        } else if required {
            return Err(invalid(format!("unknown item {name}")));
        }
    }
    Ok(supported)
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
    pub(super) fn len(&self) -> usize {
        self.0.len()
    }

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

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use zip::write::SimpleFileOptions;

    use super::*;

    #[test]
    fn missing_enchantments_only_disable_dependent_tables() {
        let load = |tag: &str| {
            let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
            for (path, json) in [
                (
                    "loot_table/chests/simple_dungeon",
                    r##"{"pools":[{"rolls":1,"entries":[{"type":"item","name":"minecraft:book","functions":[{"function":"enchant_randomly","options":"#minecraft:on_random_loot"}]}]}]}"##,
                ),
                (
                    "loot_table/chests/buried_treasure",
                    r#"{"pools":[{"rolls":1,"entries":[{"type":"item","name":"minecraft:diamond"}]}]}"#,
                ),
                (
                    "loot_table/chests/desert_pyramid",
                    r#"{"pools":[{"rolls":false,"entries":[]}]}"#,
                ),
                ("tags/enchantment/on_random_loot", tag),
                (
                    "enchantment/sharpness",
                    r##"{"max_level":5,"supported_items":"#minecraft:loot_test"}"##,
                ),
                (
                    "tags/item/loot_test",
                    r#"{"values":[{"id":"minecraft:iron_sword"},{"id":"custom:missing","required":false}]}"#,
                ),
            ] {
                zip.start_file(
                    format!("data/minecraft/{path}.json"),
                    SimpleFileOptions::default(),
                )
                .unwrap();
                zip.write_all(json.as_bytes()).unwrap();
            }
            let mut archive = ZipArchive::new(zip.finish().unwrap()).unwrap();
            ChestLootTables::load(&mut archive).unwrap().unwrap()
        };
        let tables = load(
            r##"{"values":["minecraft:sharpness",{"id":"#custom:missing","required":false},{"id":"custom:missing","required":false}]}"##,
        );
        assert!(tables.get("chests/simple_dungeon").is_some());
        assert_eq!(tables.enchantments.0.len(), 1);
        assert!(
            tables.enchantments.0[0]
                .supported
                .contains(&items().id("minecraft:iron_sword").unwrap())
        );
        for tag in [
            r#"{"values":[{"id":"custom:missing"}]}"#,
            r#"{"values":[]}"#,
            r##"{"values":[{"id":"#custom:missing","required":false},{"id":"custom:missing","required":false}]}"##,
        ] {
            let tables = load(tag);
            assert!(tables.get("chests/simple_dungeon").is_none(), "{tag}");
            assert!(tables.get("chests/buried_treasure").is_some());
            assert!(tables.get("chests/desert_pyramid").is_none());
        }
    }
}
