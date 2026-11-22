//! Built-in registries and food defaults generated locally from the owner's verified Minecraft server jar.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use harpas_core::item::items;
use harpas_engine::inventory::ItemStack;
use harpas_engine::item_data::FoodProperties;
use serde_json::{Map, Value, json};

use crate::vanilla_data::{SERVER_JAR_SHA1, VERSION};

/// The IDs are fixed for a Minecraft version, but are not distributed with Harpas.
#[derive(Debug, Default)]
pub(crate) struct Registry(HashMap<String, i32>);

impl Registry {
    pub(crate) fn id(&self, name: &str) -> Option<i32> {
        self.0.get(name).copied()
    }

    #[cfg(test)]
    pub(crate) fn synthetic(entries: &[(&str, i32)]) -> Self {
        Self(
            entries
                .iter()
                .map(|(name, id)| ((*name).to_owned(), *id))
                .collect(),
        )
    }

    fn entries(value: &Value) -> io::Result<Self> {
        let entries = value
            .as_object()
            .ok_or_else(|| invalid("registry entries are missing"))?;
        let mut ids = HashSet::with_capacity(entries.len());
        let mut result = HashMap::with_capacity(entries.len());
        for (name, value) in entries {
            let number = value.as_i64().and_then(|id| i32::try_from(id).ok());
            let Some(id) = number.filter(|id| *id >= 0 && ids.insert(*id)) else {
                return Err(invalid(format!("invalid or duplicate ID for {name}")));
            };
            result.insert(name.clone(), id);
        }
        if !result.contains_key("minecraft:sign") {
            return Err(invalid(
                "block entity registry does not contain minecraft:sign",
            ));
        }
        Ok(Self(result))
    }

    fn from_report(report: &Value) -> io::Result<Self> {
        let entries = report["minecraft:block_entity_type"]["entries"]
            .as_object()
            .ok_or_else(|| invalid("the report has no block entity type registry"))?;
        let entries: serde_json::Map<_, _> = entries
            .iter()
            .map(|(name, entry)| (name.clone(), entry["protocol_id"].clone()))
            .collect();
        Self::entries(&Value::Object(entries))
    }
}

fn registry_path(server: &Path) -> PathBuf {
    server
        .join(".harpas")
        .join("vanilla-data")
        .join(format!("{VERSION}-block-entity-types.json"))
}

fn builtin_path(server: &Path) -> PathBuf {
    server
        .join(".harpas")
        .join("vanilla-data")
        .join(format!("{VERSION}-registries.json"))
}

#[derive(Default)]
pub(crate) struct Builtin {
    /// The entries of each built-in registry in the order of their numeric IDs.
    pub(crate) registries: HashMap<String, Vec<String>>,
    /// Absent in installations generated before the item component reports were read.
    pub(crate) foods: Option<HashMap<u32, FoodProperties>>,
}

impl Builtin {
    pub(crate) fn has_food_remainders(&self) -> bool {
        self.foods
            .as_ref()
            .is_some_and(|foods| foods.values().any(|food| food.remainder.is_some()))
    }
}

/// The built-in registries of the data generator's report, each entry at its `protocol_id`.
fn builtin_from_report(report: &Value) -> io::Result<HashMap<String, Vec<String>>> {
    let registries = report
        .as_object()
        .ok_or_else(|| invalid("the report is not an object"))?;
    let mut result = HashMap::with_capacity(registries.len());
    for (registry, value) in registries {
        let entries = value["entries"]
            .as_object()
            .ok_or_else(|| invalid(format!("{registry} has no entries")))?;
        let mut ordered = vec![None; entries.len()];
        for (name, entry) in entries {
            let slot = entry["protocol_id"]
                .as_u64()
                .and_then(|id| usize::try_from(id).ok())
                .and_then(|id| ordered.get_mut(id))
                .filter(|slot| slot.is_none())
                .ok_or_else(|| {
                    invalid(format!("invalid or duplicate ID for {name} in {registry}"))
                })?;
            *slot = Some(name.clone());
        }
        let ordered = ordered.into_iter().map(Option::unwrap).collect();
        result.insert(registry.clone(), ordered);
    }
    Ok(result)
}

/// Returns None for an installation made without a local Java runtime, or before the registries were stored.
pub(crate) fn load_builtin(server: &Path) -> io::Result<Option<Builtin>> {
    let path = builtin_path(server);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let data: Value = serde_json::from_slice(&bytes).map_err(invalid)?;
    if data["version"] != VERSION || data["server_sha1"] != SERVER_JAR_SHA1 {
        return Err(invalid(
            "built-in registries are for a different server jar",
        ));
    }
    Ok(Some(Builtin {
        registries: serde_json::from_value(data["registries"].clone()).map_err(invalid)?,
        foods: data.get("foods").map(food_defaults).transpose()?,
    }))
}

fn food_reports(directory: &Path) -> io::Result<Map<String, Value>> {
    let mut foods = Map::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
            continue;
        };
        let report: Value = serde_json::from_reader(File::open(&path)?).map_err(invalid)?;
        let components = &report["components"];
        let (food, consumable) = (
            &components["minecraft:food"],
            &components["minecraft:consumable"],
        );
        if food.is_object()
            && consumable.is_object()
            && consumable
                .get("on_consume_effects")
                .is_none_or(|effects| effects.as_array().is_some_and(Vec::is_empty))
            && components
                .get("minecraft:suspicious_stew_effects")
                .is_none()
            && components.get("minecraft:use_cooldown").is_none()
        {
            let mut defaults = json!({"food": food, "consumable": consumable});
            if let Some(remainder) = components.get("minecraft:use_remainder") {
                defaults["use_remainder"] = remainder.clone();
            }
            foods.insert(format!("minecraft:{name}"), defaults);
        }
    }
    if !foods.contains_key("minecraft:apple") {
        return Err(invalid("the item reports have no apple food defaults"));
    }
    Ok(foods)
}

fn food_defaults(value: &Value) -> io::Result<HashMap<u32, FoodProperties>> {
    value
        .as_object()
        .ok_or_else(|| invalid("food defaults are not an object"))?
        .iter()
        .map(|(name, entry)| {
            let bad = || invalid(format!("invalid food defaults for {name}"));
            let id = items().id(name).ok_or_else(bad)?;
            let food = &entry["food"];
            let nutrition = food["nutrition"]
                .as_u64()
                .and_then(|value| i32::try_from(value).ok())
                .ok_or_else(bad)?;
            let saturation = food["saturation"].as_f64().ok_or_else(bad)? as f32;
            let can_always_eat = food
                .get("can_always_eat")
                .map_or(Some(false), Value::as_bool)
                .ok_or_else(bad)?;
            let consumable = entry["consumable"].as_object().ok_or_else(bad)?;
            let seconds = consumable
                .get("consume_seconds")
                .map_or(Some(1.6), Value::as_f64)
                .ok_or_else(bad)? as f32;
            let ticks = seconds * 20.0;
            let remainder = entry.get("use_remainder").map(food_remainder).transpose()?;
            if !saturation.is_finite()
                || saturation < 0.0
                || !(0.0..u32::MAX as f32).contains(&ticks)
            {
                return Err(bad());
            }
            Ok((
                id,
                FoodProperties {
                    nutrition,
                    saturation,
                    can_always_eat,
                    use_ticks: ticks as u32,
                    remainder,
                },
            ))
        })
        .collect()
}

fn food_remainder(value: &Value) -> io::Result<ItemStack> {
    let bad = || invalid("invalid default food remainder");
    let item = value["id"]
        .as_str()
        .and_then(|name| items().id(name))
        .filter(|&item| item != 0)
        .ok_or_else(bad)?;
    let max_count = i64::from(items().max_stack_size(item).ok_or_else(bad)?);
    let count = value.get("count").map_or(Some(1), Value::as_i64);
    let count = count
        .filter(|count| (1..=max_count).contains(count))
        .ok_or_else(bad)?;
    if value.get("components").is_some() {
        return Err(bad());
    }
    Ok(ItemStack {
        item,
        count: count as i32,
        ..ItemStack::empty()
    })
}

/// Returns None for an installation made without a local Java runtime.
pub(crate) fn load(server: &Path) -> io::Result<Option<Registry>> {
    let path = registry_path(server);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let data: Value = serde_json::from_slice(&bytes).map_err(invalid)?;
    if data["version"] != VERSION || data["server_sha1"] != SERVER_JAR_SHA1 {
        return Err(invalid(
            "block entity registry is for a different server jar",
        ));
    }
    Registry::entries(&data["entries"]).map(Some)
}

/// Runs Mojang's data generator, storing the registries and supported food defaults on this machine.
/// The caller must verify the jar before calling this function.
pub(crate) fn generate(server: &Path, jar: &Path) -> io::Result<()> {
    let path = registry_path(server);
    let parent = path.parent().expect("the registry has a parent");
    fs::create_dir_all(parent)?;
    let scratch = Scratch::new(parent)?;
    let jar = fs::canonicalize(jar)?;
    let output = Command::new("java")
        .args(["-DbundlerMainClass=net.minecraft.data.Main", "-jar"])
        .arg(jar)
        .arg("--reports")
        .current_dir(&scratch.0)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            io::Error::new(error.kind(), format!("could not start Java 25: {error}"))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!(
            "Minecraft's data generator failed: {stderr}"
        )));
    }
    let report = fs::read(scratch.0.join("generated/reports/registries.json"))?;
    let report: Value = serde_json::from_slice(&report).map_err(invalid)?;
    let registry = Registry::from_report(&report)?;
    let builtin = builtin_from_report(&report)?;
    let foods = food_reports(
        &scratch
            .0
            .join("generated/reports/minecraft/components/item"),
    )?;
    write(
        &scratch,
        &builtin_path(server),
        &json!({
            "version": VERSION,
            "server_sha1": SERVER_JAR_SHA1,
            "registries": builtin,
            "foods": foods,
        }),
    )?;
    write(
        &scratch,
        &path,
        &json!({
            "version": VERSION,
            "server_sha1": SERVER_JAR_SHA1,
            "entries": registry.0,
        }),
    )?;
    log::info!(
        "Generated Minecraft {VERSION} block entity and built-in registries in {}",
        parent.display()
    );
    Ok(())
}

/// Writes `data` to `path` through a file in `scratch`, so that `path` is never left half written.
fn write(scratch: &Scratch, path: &Path, data: &Value) -> io::Result<()> {
    let temporary = scratch.0.join("registry.json");
    let mut file = File::create(&temporary)?;
    file.write_all(&serde_json::to_vec(data).map_err(invalid)?)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, path)
}

/// A private workspace for Mojang's bundler and all other reports it produces.
struct Scratch(PathBuf);

impl Scratch {
    fn new(parent: &Path) -> io::Result<Self> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        loop {
            let path = parent.join(format!(
                ".registry-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn invalid(error: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_and_validates_report_ids() {
        // Invented IDs: no data from Mojang is included in the repository.
        let report = json!({"minecraft:block_entity_type": {"entries": {
            "minecraft:sign": {"protocol_id": 47},
            "minecraft:chest": {"protocol_id": 99}
        }}});
        let registry = Registry::from_report(&report).unwrap();
        assert_eq!(registry.id("minecraft:sign"), Some(47));
        assert_eq!(registry.id("minecraft:unknown"), None);
        let duplicate = json!({"minecraft:block_entity_type": {"entries": {
            "minecraft:sign": {"protocol_id": 47},
            "minecraft:chest": {"protocol_id": 47}
        }}});
        assert!(Registry::from_report(&duplicate).is_err());
        assert!(Registry::from_report(&json!({})).is_err());
        assert!(Registry::entries(&json!({"minecraft:sign": -1})).is_err());
        assert!(Registry::entries(&json!({"minecraft:chest": 1})).is_err());
    }

    #[test]
    fn orders_builtin_registries_by_protocol_id() {
        // Invented IDs: no data from Mojang is included in the repository.
        let report = json!({"minecraft:fluid": {"entries": {
            "minecraft:water": {"protocol_id": 1},
            "minecraft:empty": {"protocol_id": 0}
        }}});
        let builtin = builtin_from_report(&report).unwrap();
        assert_eq!(
            builtin["minecraft:fluid"],
            ["minecraft:empty", "minecraft:water"]
        );
        for entries in [
            json!({"minecraft:water": {"protocol_id": 1}, "minecraft:lava": {"protocol_id": 1}}),
            json!({"minecraft:water": {"protocol_id": 2}, "minecraft:lava": {"protocol_id": 0}}),
            json!({"minecraft:water": {"protocol_id": -1}}),
        ] {
            let report = json!({"minecraft:fluid": {"entries": entries}});
            assert!(builtin_from_report(&report).is_err());
        }
    }

    #[test]
    fn loaded_registry_must_match_the_pinned_version_and_jar() {
        let scratch = Scratch::new(&std::env::temp_dir()).unwrap();
        assert!(load(&scratch.0).unwrap().is_none());
        assert!(load_builtin(&scratch.0).unwrap().is_none());
        let builtin = builtin_path(&scratch.0);
        fs::create_dir_all(builtin.parent().unwrap()).unwrap();
        let mut data = json!({
            "version": VERSION,
            "server_sha1": SERVER_JAR_SHA1,
            "registries": {"minecraft:fluid": ["minecraft:empty"]}
        });
        fs::write(&builtin, data.to_string()).unwrap();
        let loaded = load_builtin(&scratch.0).unwrap().unwrap();
        assert_eq!(loaded.registries["minecraft:fluid"], ["minecraft:empty"]);
        assert!(loaded.foods.is_none());
        data["foods"] = json!({"minecraft:apple": {
            "food": {"nutrition": 2, "saturation": 0.5}, "consumable": {}
        }});
        fs::write(&builtin, data.to_string()).unwrap();
        let loaded = load_builtin(&scratch.0).unwrap().unwrap();
        assert!(!loaded.has_food_remainders());
        assert_eq!(loaded.foods.unwrap().len(), 1);
        data["foods"]["minecraft:mushroom_stew"] = json!({
            "food": {"nutrition": 2, "saturation": 0.5}, "consumable": {},
            "use_remainder": {"id": "minecraft:bowl"}
        });
        fs::write(&builtin, data.to_string()).unwrap();
        assert!(
            load_builtin(&scratch.0)
                .unwrap()
                .unwrap()
                .has_food_remainders()
        );
        data["server_sha1"] = json!("other");
        fs::write(&builtin, data.to_string()).unwrap();
        assert!(load_builtin(&scratch.0).is_err());
        let path = registry_path(&scratch.0);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut data = json!({
            "version": VERSION,
            "server_sha1": SERVER_JAR_SHA1,
            "entries": {"minecraft:sign": 47}
        });
        fs::write(&path, data.to_string()).unwrap();
        assert_eq!(
            load(&scratch.0).unwrap().unwrap().id("minecraft:sign"),
            Some(47)
        );
        data["version"] = json!("other");
        fs::write(&path, data.to_string()).unwrap();
        assert!(load(&scratch.0).is_err());
        data["version"] = json!(VERSION);
        data["server_sha1"] = json!("other");
        fs::write(&path, data.to_string()).unwrap();
        assert!(load(&scratch.0).is_err());
    }
}
