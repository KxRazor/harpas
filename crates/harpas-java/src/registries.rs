//! The registries a 26.1 server synchronizes in the Configuration state, from `data/registries-26.1.txt`,
//! and the tags their data refers to, from `data/tags-26.1.txt`

use std::sync::OnceLock;

/// A synchronized registry and its entries, in the order vanilla sends them.
#[derive(Debug)]
pub(crate) struct Registry {
    /// Identifier of the registry, such as `minecraft:dimension_type`.
    pub(crate) id: &'static str,
    /// Identifiers of the entries; an entry's numeric ID is its position.
    pub(crate) entries: Vec<&'static str>,
}

/// The synchronized registries of Java 26.1.
pub(crate) fn registries() -> &'static [Registry] {
    static REGISTRIES: OnceLock<Vec<Registry>> = OnceLock::new();
    REGISTRIES.get_or_init(|| parse(include_str!("../data/registries-26.1.txt")))
}

/// The tags the client needs before it accepts the registries, by registry. They are sent empty for now.
pub(crate) fn required_tags() -> &'static [Registry] {
    static TAGS: OnceLock<Vec<Registry>> = OnceLock::new();
    TAGS.get_or_init(|| parse(include_str!("../data/tags-26.1.txt")))
}

/// Numeric ID of `entry` in `registry`.
pub(crate) fn entry_id(registry: &str, entry: &str) -> Option<i32> {
    let registry = registries().iter().find(|r| r.id == registry)?;
    let index = registry.entries.iter().position(|e| *e == entry)?;
    i32::try_from(index).ok()
}

fn parse(text: &'static str) -> Vec<Registry> {
    let mut registries: Vec<Registry> = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        match line.strip_prefix("  ") {
            Some(entry) => registries
                .last_mut()
                .expect("the file starts with a registry")
                .entries
                .push(entry),
            None => registries.push(Registry {
                id: line,
                entries: Vec::new(),
            }),
        }
    }
    registries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_has_every_synchronized_registry() {
        let registries = registries();
        assert_eq!(registries.len(), 28);
        assert_eq!(
            registries.iter().map(|r| r.entries.len()).sum::<usize>(),
            382
        );
        assert_eq!(
            entry_id("minecraft:dimension_type", "minecraft:overworld"),
            Some(0)
        );
        assert!(entry_id("minecraft:worldgen/biome", "minecraft:plains").is_some());
        assert_eq!(
            entry_id("minecraft:worldgen/biome", "minecraft:nowhere"),
            None
        );
        let tags = required_tags();
        assert!(tags.iter().any(
            |r| r.id == "minecraft:item" && r.entries.contains(&"minecraft:enchantable/armor")
        ));
    }
}
