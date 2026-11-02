//! The block state registry of Java 26.1, from `data/blocks-26.1.txt`
//!
//! Blocks are defined by the game's code, not by data packs, so their states and global IDs are fixed for a
//! Minecraft version. Chunk sections store these IDs; Anvil files and commands name a state by its block and
//! property values, which this registry turns into IDs and back.

use std::collections::HashMap;
use std::sync::OnceLock;

/// A block and its states.
#[derive(Debug)]
struct Block {
    /// The name with its namespace, such as `minecraft:oak_stairs`.
    name: String,
    /// The ID of the first state.
    first: u32,
    /// The ID of the default state.
    default: u32,
    /// Properties in vanilla's order, each with its values in order.
    properties: Vec<(&'static str, Vec<&'static str>)>,
}

impl Block {
    fn states(&self) -> u32 {
        self.properties
            .iter()
            .map(|(_, values)| values.len() as u32)
            .product()
    }
}

/// The blocks of one Minecraft version and their states.
#[derive(Debug)]
pub struct BlockStates {
    blocks: Vec<Block>,
    by_name: HashMap<String, usize>,
    /// For each state ID, the index of its block.
    owners: Vec<u16>,
}

/// The block states of Java 26.1.
pub fn block_states() -> &'static BlockStates {
    static STATES: OnceLock<BlockStates> = OnceLock::new();
    STATES.get_or_init(|| BlockStates::parse(include_str!("../data/blocks-26.1.txt")))
}

impl BlockStates {
    fn parse(text: &'static str) -> Self {
        let mut blocks = Vec::new();
        let mut owners = Vec::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let mut fields = line.split(' ');
            let name = format!("minecraft:{}", fields.next().expect("a block name"));
            let mut default = 0;
            let mut properties = Vec::new();
            for field in fields {
                let (key, value) = field.split_once('=').expect("a key and a value");
                if key == "default" {
                    default = value.parse().expect("a default state offset");
                } else {
                    properties.push((key, value.split(',').collect()));
                }
            }
            let first = owners.len() as u32;
            let block = Block {
                name,
                first,
                default: first + default,
                properties,
            };
            owners.extend(std::iter::repeat_n(
                blocks.len() as u16,
                block.states() as usize,
            ));
            blocks.push(block);
        }
        let by_name = blocks
            .iter()
            .enumerate()
            .map(|(index, block)| (block.name.clone(), index))
            .collect();
        Self {
            blocks,
            by_name,
            owners,
        }
    }

    /// Number of block states.
    pub fn len(&self) -> u32 {
        self.owners.len() as u32
    }

    /// Whether there are no block states; never true for a real version.
    pub fn is_empty(&self) -> bool {
        self.owners.is_empty()
    }

    /// Bits per global ID in paletted containers: the base-2 logarithm of the number of states, rounded up.
    pub fn global_bits(&self) -> u8 {
        (u32::BITS - (self.len() - 1).leading_zeros()) as u8
    }

    /// The default state of the block `name`, such as `minecraft:stone`.
    pub fn default_state(&self, name: &str) -> Option<u32> {
        self.by_name
            .get(name)
            .map(|&index| self.blocks[index].default)
    }

    /// The state of the block `name` with the given property values. Properties not given, unknown or with a
    /// value the block does not have keep the default state's value, as vanilla reads them; those that do not
    /// apply are returned.
    pub fn state<'a>(
        &self,
        name: &str,
        properties: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Option<(u32, Vec<&'a str>)> {
        let block = &self.blocks[*self.by_name.get(name)?];
        let mut offset = block.default - block.first;
        let mut ignored = Vec::new();
        for (key, value) in properties {
            let mut stride = 1;
            let mut applied = false;
            for (property, values) in block.properties.iter().rev() {
                let count = values.len() as u32;
                if *property == key {
                    if let Some(position) = values.iter().position(|v| *v == value) {
                        let current = offset / stride % count;
                        offset = offset - current * stride + position as u32 * stride;
                        applied = true;
                    }
                    break;
                }
                stride *= count;
            }
            if !applied {
                ignored.push(key);
            }
        }
        Some((block.first + offset, ignored))
    }

    /// The block name and property values of state `id`, properties in vanilla's order.
    pub fn properties(&self, id: u32) -> Option<(&str, Vec<(&str, &str)>)> {
        let block = &self.blocks[*self.owners.get(id as usize)? as usize];
        let mut offset = id - block.first;
        let mut properties = Vec::with_capacity(block.properties.len());
        for (property, values) in block.properties.iter().rev() {
            let count = values.len() as u32;
            properties.push((*property, values[(offset % count) as usize]));
            offset /= count;
        }
        properties.reverse();
        Some((&block.name, properties))
    }

    /// Whether state `id` is air: `air`, `cave_air` or `void_air`.
    pub fn is_air(&self, id: u32) -> bool {
        self.properties(id).is_some_and(|(name, _)| {
            matches!(
                name,
                "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_has_every_26_1_state() {
        let states = block_states();
        assert_eq!(states.len(), 29_873);
        assert_eq!(states.global_bits(), 15);
        assert_eq!(states.default_state("minecraft:air"), Some(0));
        assert_eq!(states.default_state("minecraft:stone"), Some(1));
        assert_eq!(states.default_state("stone"), None);
    }

    #[test]
    fn states_are_numbered_with_the_last_property_fastest() {
        let states = block_states();
        // minecraft-data: oak_stairs takes 3907 to 3986, default 3918 (north, bottom, straight, not waterlogged).
        assert_eq!(states.default_state("minecraft:oak_stairs"), Some(3918));
        let stairs = |properties: &[(&'static str, &'static str)]| {
            states
                .state("minecraft:oak_stairs", properties.iter().copied())
                .unwrap()
        };
        assert_eq!(stairs(&[]), (3918, vec![]));
        assert_eq!(stairs(&[("waterlogged", "true")]).0, 3917);
        assert_eq!(
            stairs(&[
                ("facing", "east"),
                ("half", "top"),
                ("shape", "outer_right"),
                ("waterlogged", "false"),
            ])
            .0,
            3907 + 3 * 20 + 4 * 2 + 1
        );
        assert_eq!(
            states.properties(3917),
            Some((
                "minecraft:oak_stairs",
                vec![
                    ("facing", "north"),
                    ("half", "bottom"),
                    ("shape", "straight"),
                    ("waterlogged", "true"),
                ]
            ))
        );
        assert_eq!(states.properties(29_873), None);
    }

    #[test]
    fn invalid_properties_keep_the_default_value() {
        let states = block_states();
        // note_block: 581 to 1930, default 582 (harp, note 0, not powered).
        let (id, ignored) = states
            .state(
                "minecraft:note_block",
                [("note", "24"), ("powered", "maybe"), ("color", "red")],
            )
            .unwrap();
        assert_eq!(id, 582 + 24 * 2);
        assert_eq!(ignored, ["powered", "color"]);
        assert_eq!(states.state("minecraft:not_a_block", []), None);
    }

    #[test]
    fn every_state_round_trips_through_its_properties() {
        let states = block_states();
        for id in 0..states.len() {
            let (name, properties) = states.properties(id).unwrap();
            assert_eq!(states.state(name, properties), Some((id, vec![])), "{name}");
        }
        let air: Vec<u32> = (0..states.len()).filter(|&id| states.is_air(id)).collect();
        assert_eq!(air.len(), 3);
        assert!(!states.is_air(1));
    }
}
