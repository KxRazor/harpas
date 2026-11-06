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
    /// The block's default light emission.
    emission: u8,
    /// The block's default light dampening.
    filter: u8,
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
    /// For each state ID, its light emission, 0 to 15.
    emission: Vec<u8>,
    /// For each state ID, its light dampening, 0 to 15.
    dampening: Vec<u8>,
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
            let mut emission = 0;
            let mut filter = 0;
            let mut properties = Vec::new();
            for field in fields {
                let (key, value) = field.split_once('=').expect("a key and a value");
                match key {
                    "default" => default = value.parse().expect("a default state offset"),
                    "emit" => emission = value.parse().expect("a light emission"),
                    "filter" => filter = value.parse().expect("a light dampening"),
                    _ => properties.push((key, value.split(',').collect())),
                }
            }
            let first = owners.len() as u32;
            let block = Block {
                name,
                first,
                default: first + default,
                properties,
                emission,
                filter,
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
        let mut states = Self {
            blocks,
            by_name,
            owners,
            emission: Vec::new(),
            dampening: Vec::new(),
        };
        let light: Vec<_> = (0..states.owners.len())
            .map(|id| states.state_light(id as u32))
            .collect();
        states.emission = light.iter().map(|&(emission, _)| emission).collect();
        states.dampening = light.iter().map(|&(_, dampening)| dampening).collect();
        states
    }

    /// Number of block states.
    pub fn len(&self) -> u32 {
        self.owners.len() as u32
    }

    /// Whether there are no block states; never true for a real version.
    pub fn is_empty(&self) -> bool {
        self.owners.is_empty()
    }

    /// The light this block state emits, from 0 to 15.
    pub fn light_emission(&self, id: u32) -> u8 {
        self.emission.get(id as usize).copied().unwrap_or(0)
    }

    /// The light dampening of this block state, from 0 to 15.
    pub fn light_dampening(&self, id: u32) -> u8 {
        self.dampening.get(id as usize).copied().unwrap_or(15)
    }

    /// Applies the state-dependent light rules on top of the block's defaults.
    fn state_light(&self, id: u32) -> (u8, u8) {
        let Some((name, properties)) = self.properties(id) else {
            return (0, 15);
        };
        let block = &self.blocks[self.owners[id as usize] as usize];
        let name = name.strip_prefix("minecraft:").unwrap_or(name);
        let value = |key: &str| {
            properties
                .iter()
                .find(|(property, _)| *property == key)
                .map(|(_, value)| *value)
        };
        let is = |key: &str, expected: &str| value(key) == Some(expected);
        let number = |key: &str| value(key).and_then(|value| value.parse::<u8>().ok());
        let mut emission = block.emission;
        let mut dampening = block.filter;

        match name {
            "light" => emission = number("level").unwrap_or(0),
            "respawn_anchor" => {
                emission = [0, 3, 7, 11, 15][usize::from(number("charges").unwrap_or(0).min(4))];
            }
            "campfire" | "soul_campfire" => {
                emission = if is("lit", "true") {
                    if name == "campfire" { 15 } else { 10 }
                } else {
                    0
                };
            }
            "furnace" | "blast_furnace" | "smoker" => {
                emission = if is("lit", "true") { 13 } else { 0 };
            }
            "redstone_lamp" => emission = if is("lit", "true") { 15 } else { 0 },
            "redstone_ore" | "deepslate_redstone_ore" => {
                emission = if is("lit", "true") { 9 } else { 0 };
            }
            "redstone_torch" | "redstone_wall_torch" => {
                emission = if is("lit", "true") { 7 } else { 0 };
            }
            "cave_vines" | "cave_vines_plant" => {
                emission = if is("berries", "true") { 14 } else { 0 };
            }
            "glow_lichen" => {
                emission = if ["down", "east", "north", "south", "up", "west"]
                    .iter()
                    .any(|key| is(key, "true"))
                {
                    7
                } else {
                    0
                };
            }
            "sea_pickle" => {
                emission = if is("waterlogged", "true") {
                    3 + 3 * number("pickles").unwrap_or(1)
                } else {
                    0
                };
            }
            "trial_spawner" => {
                emission = match value("trial_spawner_state") {
                    Some("waiting_for_players") => 4,
                    Some("active" | "waiting_for_reward_ejection" | "ejecting_reward") => 8,
                    _ => 0,
                };
            }
            "vault" => {
                emission = match value("vault_state") {
                    Some("active" | "ejecting" | "unlocking") => 12,
                    _ => 6,
                };
            }
            _ if name == "candle" || name.ends_with("_candle") => {
                emission = if is("lit", "true") {
                    3 * number("candles").unwrap_or(1)
                } else {
                    0
                };
            }
            _ if name == "candle_cake" || name.ends_with("_candle_cake") => {
                emission = if is("lit", "true") { 3 } else { 0 };
            }
            _ => {}
        }

        let bulb = name.strip_prefix("waxed_").unwrap_or(name);
        if matches!(
            bulb,
            "copper_bulb"
                | "exposed_copper_bulb"
                | "weathered_copper_bulb"
                | "oxidized_copper_bulb"
        ) {
            emission = if is("lit", "true") {
                match bulb {
                    "copper_bulb" => 15,
                    "exposed_copper_bulb" => 12,
                    "weathered_copper_bulb" => 8,
                    _ => 4,
                }
            } else {
                0
            };
        }

        let leaf_like = name.ends_with("_leaves") || name == "mangrove_roots";
        let waterlogged = is("waterlogged", "true");
        let grate = name == "copper_grate" || name.ends_with("_copper_grate");
        if name == "snow" {
            dampening = if is("layers", "8") { 15 } else { 0 };
        } else if name == "piston" || name == "sticky_piston" {
            dampening = if is("extended", "false") { 15 } else { 0 };
        } else if name.ends_with("_slab") && is("type", "double") {
            dampening = 15;
        } else {
            if name == "resin_clump" || name == "sculk_vein" {
                let any_face = ["down", "east", "north", "south", "up", "west"]
                    .iter()
                    .any(|key| is(key, "true"));
                dampening = if any_face { 0 } else { 1 };
            } else if name == "glow_lichen" {
                dampening = 0;
            } else if name == "sculk_shrieker" {
                dampening = 1;
            } else if grate
                || (dampening == 1
                    && !leaf_like
                    && properties.iter().any(|(key, _)| *key == "waterlogged"))
            {
                dampening = 0;
            }
            if waterlogged && !grate {
                dampening = dampening.max(1);
            }
        }

        (emission, dampening)
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
    fn light_values_cover_defaults_and_state_overrides() {
        let states = block_states();
        let id = |name: &str, properties: &[(&str, &str)]| {
            states.state(name, properties.iter().copied()).unwrap().0
        };
        assert_eq!(states.light_dampening(id("minecraft:stone", &[])), 15);
        assert_eq!(states.light_dampening(id("minecraft:glass", &[])), 0);
        assert_eq!(
            states.light_dampening(id("minecraft:water", &[("level", "0")])),
            1
        );
        assert_eq!(
            states.light_dampening(id("minecraft:oak_leaves", &[("waterlogged", "true")])),
            1
        );
        assert_eq!(
            states.light_dampening(id("minecraft:oak_stairs", &[("waterlogged", "true")])),
            1
        );
        assert_eq!(
            states.light_dampening(id("minecraft:oak_stairs", &[("waterlogged", "false")])),
            0
        );
        assert_eq!(
            states.light_dampening(id("minecraft:oak_slab", &[("type", "double")])),
            15
        );
        assert_eq!(
            states.light_dampening(id("minecraft:snow", &[("layers", "8")])),
            15
        );
        assert_eq!(
            states.light_dampening(id("minecraft:snow", &[("layers", "1")])),
            0
        );
        assert_eq!(
            states.light_dampening(id("minecraft:piston", &[("extended", "false")])),
            15
        );
        assert_eq!(
            states.light_dampening(id("minecraft:piston", &[("extended", "true")])),
            0
        );

        assert_eq!(states.light_emission(id("minecraft:torch", &[])), 14);
        assert_eq!(
            states.light_emission(id("minecraft:campfire", &[("lit", "true")])),
            15
        );
        assert_eq!(
            states.light_emission(id("minecraft:campfire", &[("lit", "false")])),
            0
        );
        assert_eq!(
            states.light_emission(id("minecraft:light", &[("level", "7")])),
            7
        );
        assert_eq!(
            states.light_emission(id("minecraft:respawn_anchor", &[("charges", "4")])),
            15
        );
        assert_eq!(
            states.light_emission(id("minecraft:candle", &[("candles", "3"), ("lit", "true")])),
            9
        );
        assert_eq!(
            states.light_emission(id(
                "minecraft:sea_pickle",
                &[("pickles", "4"), ("waterlogged", "true")]
            )),
            15
        );
        assert_eq!(
            states.light_emission(id("minecraft:copper_bulb", &[("lit", "true")])),
            15
        );
        assert_eq!(
            states.light_emission(id("minecraft:glow_lichen", &[("up", "true")])),
            7
        );
    }

    #[test]
    fn every_state_has_a_valid_light_value() {
        let states = block_states();
        for id in 0..states.len() {
            assert!(states.light_emission(id) <= 15);
            assert!(matches!(states.light_dampening(id), 0 | 1 | 15));
        }
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
