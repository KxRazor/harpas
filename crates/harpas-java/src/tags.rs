//! The tags a Java 26.1 server sends in the Configuration state's Update Tags packet, resolved from the tag files
//! of a data pack as vanilla's `TagLoader` resolves them (`docs/java-connection.md#tags`).

use std::collections::{HashMap, HashSet};
use std::fmt;

use harpas_protocol_java::v26_1::configuration;
use serde_json::Value;

use crate::registries;

/// The tags of every registry a client is sent tags for, with their entries as numeric IDs.
#[derive(Debug, Clone, PartialEq)]
pub struct Tags {
    registries: Vec<configuration::RegistryTags>,
}

// Names and numeric IDs only, so equality is total.
impl Eq for Tags {}

/// A tag file that could not be used. Vanilla logs these and leaves the tag out, as `resolve` does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagError {
    /// Identifier of the registry, such as `minecraft:block`.
    pub registry: String,
    /// Identifier of the tag, such as `minecraft:logs`.
    pub tag: String,
    /// What is wrong with it.
    pub reason: String,
}

impl fmt::Display for TagError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} tag #{}: {}", self.registry, self.tag, self.reason)
    }
}

/// One tag file, parsed.
struct TagFile {
    /// `(id, required)` of each value, in the file's order; tag references keep their `#`.
    values: Vec<(String, bool)>,
}

impl Tags {
    /// The tags the client needs before it accepts the registries, all empty: what a server without vanilla's
    /// data sends, from `data/tags-26.1.txt`.
    pub fn required_empty() -> Self {
        let mut tags = Self {
            registries: Vec::new(),
        };
        tags.add_required();
        tags
    }

    /// Resolves the tags of a data pack.
    ///
    /// `files` gives each file of the pack by its path, such as `data/minecraft/tags/fluid/water.json`, and its
    /// contents; files that are not tags, and the packs nested under `data/minecraft/datapacks/`, are skipped.
    /// `builtin` gives the entries of each built-in registry, such as `minecraft:block`, in the order of their
    /// numeric IDs. The synchronized registries come from `data/registries-26.1.txt`. Tags of a registry that is
    /// neither, such as `minecraft:worldgen/structure`, are not sent, as vanilla does not send them.
    ///
    /// Like vanilla, a tag whose required entry or tag does not exist is left out and reported, and so are the
    /// tags that require it. The entries of a tag follow its file, a referenced tag's entries in its place, each
    /// entry where it first appears. Registries and tags are sorted by identifier: vanilla's order of them changes
    /// from one run to the next, and only the order inside a tag is the same every time. Any tag the client
    /// requires that the pack lacks is added empty.
    pub fn resolve<'a>(
        files: impl IntoIterator<Item = (&'a str, &'a [u8])>,
        builtin: &HashMap<String, Vec<String>>,
    ) -> (Self, Vec<TagError>) {
        let mut ids: HashMap<&str, HashMap<&str, i32>> = HashMap::new();
        for (registry, entries) in builtin {
            ids.insert(registry, numbered(entries.iter().map(String::as_str)));
        }
        for registry in registries::registries() {
            ids.insert(registry.id, numbered(registry.entries.iter().copied()));
        }

        let mut errors = Vec::new();
        let mut files_by_registry: HashMap<&str, HashMap<String, TagFile>> = HashMap::new();
        for (path, contents) in files {
            let Some((namespace, rest)) = tag_path(path) else {
                continue;
            };
            // The longest registry whose folder holds the file: `worldgen/biome/x` is a biome tag.
            let Some(registry) = ids
                .keys()
                .copied()
                .filter(|registry| {
                    let folder = registry.strip_prefix("minecraft:").unwrap_or(registry);
                    rest.strip_prefix(folder)
                        .is_some_and(|tail| tail.starts_with('/'))
                })
                .max_by_key(|registry| registry.len())
            else {
                continue;
            };
            let folder = registry.strip_prefix("minecraft:").unwrap_or(registry);
            let tag = format!("{namespace}:{}", &rest[folder.len() + 1..]);
            match parse(contents) {
                Ok(file) => {
                    files_by_registry
                        .entry(registry)
                        .or_default()
                        .insert(tag, file);
                }
                Err(reason) => errors.push(TagError {
                    registry: registry.to_owned(),
                    tag,
                    reason,
                }),
            }
        }

        let mut registries = Vec::new();
        for (registry, files) in files_by_registry {
            let mut resolver = Resolver {
                registry,
                ids: &ids[registry],
                files: &files,
                resolved: HashMap::new(),
                active: HashSet::new(),
                errors: &mut errors,
            };
            let mut names: Vec<&String> = files.keys().collect();
            names.sort();
            let tags = names
                .into_iter()
                .filter_map(|name| {
                    resolver.resolve(name).map(|entries| configuration::Tag {
                        name: name.clone(),
                        entries,
                    })
                })
                .collect();
            registries.push(configuration::RegistryTags {
                registry: registry.to_owned(),
                tags,
            });
        }
        registries.sort_by(|a, b| a.registry.cmp(&b.registry));
        errors.sort_by(|a, b| (&a.registry, &a.tag).cmp(&(&b.registry, &b.tag)));
        let mut tags = Self { registries };
        tags.add_required();
        (tags, errors)
    }

    /// The registries and their tags, as the Update Tags packet carries them.
    pub fn registries(&self) -> &[configuration::RegistryTags] {
        &self.registries
    }

    /// Adds, empty, the tags the client requires that are missing.
    fn add_required(&mut self) {
        for required in registries::required_tags() {
            let index = match self
                .registries
                .iter()
                .position(|r| r.registry == required.id)
            {
                Some(index) => index,
                None => {
                    self.registries.push(configuration::RegistryTags {
                        registry: required.id.to_owned(),
                        tags: Vec::new(),
                    });
                    self.registries.len() - 1
                }
            };
            let registry = &mut self.registries[index];
            for name in &required.entries {
                if !registry.tags.iter().any(|tag| tag.name == *name) {
                    registry.tags.push(configuration::Tag {
                        name: (*name).to_owned(),
                        entries: Vec::new(),
                    });
                }
            }
            registry.tags.sort_by(|a, b| a.name.cmp(&b.name));
        }
        self.registries.sort_by(|a, b| a.registry.cmp(&b.registry));
    }
}

impl Default for Tags {
    fn default() -> Self {
        Self::required_empty()
    }
}

fn numbered<'a>(entries: impl Iterator<Item = &'a str>) -> HashMap<&'a str, i32> {
    entries
        .enumerate()
        .map(|(id, entry)| (entry, i32::try_from(id).expect("registries are small")))
        .collect()
}

/// The namespace of a tag file and its path below `tags/`, without `.json`.
fn tag_path(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("data/")?;
    let (namespace, rest) = rest.split_once('/')?;
    let rest = rest.strip_prefix("tags/")?.strip_suffix(".json")?;
    Some((namespace, rest))
}

/// An identifier with its namespace, `minecraft` when it has none.
fn qualified(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

fn parse(contents: &[u8]) -> Result<TagFile, String> {
    let json: Value =
        serde_json::from_slice(contents).map_err(|error| format!("invalid JSON: {error}"))?;
    let values = json
        .get("values")
        .and_then(Value::as_array)
        .ok_or("no `values` list")?;
    let values = values
        .iter()
        .map(|value| match value {
            Value::String(id) => Ok((id.clone(), true)),
            Value::Object(entry) => {
                let id = entry
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or("an entry has no `id`")?;
                let required = match entry.get("required") {
                    None => true,
                    Some(required) => required.as_bool().ok_or("`required` is not a boolean")?,
                };
                Ok((id.to_owned(), required))
            }
            _ => Err("an entry is neither an identifier nor an object".to_owned()),
        })
        .collect::<Result<_, String>>()?;
    Ok(TagFile { values })
}

/// Resolves the tags of one registry, each once.
struct Resolver<'a> {
    registry: &'a str,
    ids: &'a HashMap<&'a str, i32>,
    files: &'a HashMap<String, TagFile>,
    /// The entries of each tag resolved so far, or `None` for a tag left out.
    resolved: HashMap<String, Option<Vec<i32>>>,
    /// The tags being resolved, to find cycles.
    active: HashSet<String>,
    errors: &'a mut Vec<TagError>,
}

impl Resolver<'_> {
    fn resolve(&mut self, name: &str) -> Option<Vec<i32>> {
        if let Some(entries) = self.resolved.get(name) {
            return entries.clone();
        }
        let file = self.files.get(name)?;
        if !self.active.insert(name.to_owned()) {
            self.fail(name, "it refers to itself".to_owned());
            return None;
        }
        let mut entries = Vec::new();
        let mut seen = HashSet::new();
        let mut missing = None;
        for (id, required) in &file.values {
            let found = match id.strip_prefix('#') {
                Some(tag) => {
                    let tag = qualified(tag);
                    self.resolve(&tag).ok_or_else(|| format!("tag #{tag}"))
                }
                None => {
                    let id = qualified(id);
                    self.ids
                        .get(id.as_str())
                        .map(|&entry| vec![entry])
                        .ok_or(id)
                }
            };
            match found {
                Ok(found) => entries.extend(found.into_iter().filter(|entry| seen.insert(*entry))),
                Err(what) if *required => {
                    missing = Some(what);
                    break;
                }
                Err(_) => {}
            }
        }
        self.active.remove(name);
        let entries = match missing {
            Some(what) => {
                self.fail(name, format!("it requires {what}, which does not exist"));
                None
            }
            None => Some(entries),
        };
        self.resolved.insert(name.to_owned(), entries.clone());
        entries
    }

    fn fail(&mut self, tag: &str, reason: String) {
        self.errors.push(TagError {
            registry: self.registry.to_owned(),
            tag: tag.to_owned(),
            reason,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin() -> HashMap<String, Vec<String>> {
        // Invented registries: no data from Mojang is included in the repository.
        HashMap::from([(
            "minecraft:fluid".to_owned(),
            ["empty", "flowing_water", "water", "flowing_lava", "lava"]
                .map(|name| format!("minecraft:{name}"))
                .to_vec(),
        )])
    }

    fn tag<'a>(tags: &'a Tags, registry: &str, name: &str) -> Option<&'a [i32]> {
        tags.registries()
            .iter()
            .find(|r| r.registry == registry)?
            .tags
            .iter()
            .find(|t| t.name == name)
            .map(|t| t.entries.as_slice())
    }

    #[test]
    fn resolves_nested_tags_in_file_order() {
        let files: [(&str, &[u8]); 5] = [
            (
                "data/minecraft/tags/fluid/water.json",
                br#"{"values":["minecraft:water","flowing_water"]}"#,
            ),
            (
                "data/minecraft/tags/fluid/wet.json",
                br##"{"values":["minecraft:lava","#minecraft:water","minecraft:water",{"id":"minecraft:steam","required":false}]}"##,
            ),
            (
                "data/minecraft/tags/worldgen/biome/is_cold.json",
                br#"{"values":["minecraft:snowy_plains"]}"#,
            ),
            (
                "data/minecraft/tags/worldgen/structure/ignored.json",
                br#"{"values":[]}"#,
            ),
            (
                "data/minecraft/datapacks/other/data/minecraft/tags/fluid/water.json",
                br#"{"values":[]}"#,
            ),
        ];
        let (tags, errors) = Tags::resolve(files, &builtin());
        assert_eq!(errors, []);
        assert_eq!(
            tag(&tags, "minecraft:fluid", "minecraft:water"),
            Some(&[2, 1][..])
        );
        assert_eq!(
            tag(&tags, "minecraft:fluid", "minecraft:wet"),
            Some(&[4, 2, 1][..])
        );
        let snowy = registries::entry_id("minecraft:worldgen/biome", "minecraft:snowy_plains");
        assert_eq!(
            tag(&tags, "minecraft:worldgen/biome", "minecraft:is_cold"),
            snowy.map(|id| vec![id]).as_deref()
        );
        assert!(
            !tags
                .registries()
                .iter()
                .any(|r| r.registry.contains("structure"))
        );
        // The tags the client requires are there, empty when the pack lacks them.
        assert_eq!(
            tag(&tags, "minecraft:item", "minecraft:enchantable/armor"),
            Some(&[][..])
        );
    }

    #[test]
    fn leaves_out_tags_with_missing_requirements() {
        let files: [(&str, &[u8]); 4] = [
            (
                "data/minecraft/tags/fluid/broken.json",
                br#"{"values":["minecraft:steam"]}"#,
            ),
            (
                "data/minecraft/tags/fluid/depends.json",
                br##"{"values":["minecraft:water","#minecraft:broken"]}"##,
            ),
            (
                "data/minecraft/tags/fluid/optional.json",
                br##"{"values":["minecraft:water",{"id":"#minecraft:broken","required":false}]}"##,
            ),
            (
                "data/minecraft/tags/fluid/cycle.json",
                br##"{"values":["#cycle"]}"##,
            ),
        ];
        let (tags, errors) = Tags::resolve(files, &builtin());
        assert_eq!(tag(&tags, "minecraft:fluid", "minecraft:broken"), None);
        assert_eq!(tag(&tags, "minecraft:fluid", "minecraft:depends"), None);
        assert_eq!(
            tag(&tags, "minecraft:fluid", "minecraft:optional"),
            Some(&[2][..])
        );
        assert_eq!(tag(&tags, "minecraft:fluid", "minecraft:cycle"), None);
        let failed: Vec<_> = errors.iter().map(|e| e.tag.as_str()).collect();
        assert_eq!(
            failed,
            [
                "minecraft:broken",
                "minecraft:cycle",
                "minecraft:cycle",
                "minecraft:depends"
            ]
        );
    }

    #[test]
    fn without_builtin_registries_only_synchronized_ones_are_resolved() {
        let files: [(&str, &[u8]); 1] = [(
            "data/minecraft/tags/fluid/water.json",
            br#"{"values":["minecraft:water"]}"#,
        )];
        let (tags, errors) = Tags::resolve(files, &HashMap::new());
        assert_eq!(errors, []);
        assert!(
            !tags
                .registries()
                .iter()
                .any(|r| r.registry == "minecraft:fluid")
        );
        assert_eq!(tags, Tags::required_empty());
    }
}
