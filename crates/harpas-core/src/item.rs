//! The item registry of Java 26.1, from `data/items-26.1.txt`
//!
//! Items are defined by the game's code, so their numeric IDs, which the protocol sends, are fixed for a Minecraft
//! version. Each item has the stack size of its default `max_stack_size` component.

use std::collections::HashMap;
use std::sync::OnceLock;

/// The items of one Minecraft version, in registry order.
#[derive(Debug)]
pub struct Items {
    /// Each item's name, with its namespace, and its default maximum stack size.
    items: Vec<(String, u8)>,
    by_name: HashMap<String, u32>,
}

/// The items of Java 26.1.
pub fn items() -> &'static Items {
    static ITEMS: OnceLock<Items> = OnceLock::new();
    ITEMS.get_or_init(|| Items::parse(include_str!("../data/items-26.1.txt")))
}

impl Items {
    fn parse(text: &str) -> Self {
        let items: Vec<(String, u8)> = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let mut fields = line.split(' ');
                let name = format!("minecraft:{}", fields.next().expect("an item name"));
                let stack = fields
                    .next()
                    .map_or(64, |n| n.parse().expect("a stack size"));
                (name, stack)
            })
            .collect();
        let by_name = items
            .iter()
            .enumerate()
            .map(|(id, (name, _))| (name.clone(), id as u32))
            .collect();
        Self { items, by_name }
    }

    /// The number of items.
    pub fn len(&self) -> u32 {
        self.items.len() as u32
    }

    /// Whether there are no items; never true for a real version.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The name of item `id`, such as `minecraft:stone`.
    pub fn name(&self, id: u32) -> Option<&str> {
        self.items.get(id as usize).map(|(name, _)| name.as_str())
    }

    /// The ID of the item named `name`.
    pub fn id(&self, name: &str) -> Option<u32> {
        self.by_name.get(name).copied()
    }

    /// The default maximum stack size of item `id`: 1, 16 or 64.
    pub fn max_stack_size(&self, id: u32) -> Option<u8> {
        self.items.get(id as usize).map(|(_, stack)| *stack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_have_vanillas_ids_and_stack_sizes() {
        let items = items();
        assert_eq!(items.len(), 1506);
        assert_eq!(items.name(0), Some("minecraft:air"));
        assert_eq!(items.id("minecraft:stone"), Some(1));
        let stack = |name| items.max_stack_size(items.id(name).unwrap());
        assert_eq!(stack("minecraft:stone"), Some(64));
        assert_eq!(stack("minecraft:snowball"), Some(16));
        assert_eq!(stack("minecraft:diamond_sword"), Some(1));
    }
}
