//! Block entity type IDs generated locally from the owner's verified Minecraft server jar.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

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

/// Runs Mojang's data generator, storing only the block entity registry on this machine.
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
    let registry = Registry::from_report(&serde_json::from_slice(&report).map_err(invalid)?)?;
    let data = json!({
        "version": VERSION,
        "server_sha1": SERVER_JAR_SHA1,
        "entries": registry.0,
    });
    let temporary = scratch.0.join("block-entity-types.json");
    let mut file = File::create(&temporary)?;
    file.write_all(&serde_json::to_vec(&data).map_err(invalid)?)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, &path)?;
    log::info!(
        "Generated Minecraft {VERSION} block entity registry in {}",
        path.display()
    );
    Ok(())
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
    fn loaded_registry_must_match_the_pinned_version_and_jar() {
        let scratch = Scratch::new(&std::env::temp_dir()).unwrap();
        assert!(load(&scratch.0).unwrap().is_none());
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
