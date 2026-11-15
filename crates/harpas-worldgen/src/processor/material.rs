//! Material substitutions observed through vanilla's runtime-added template processors.

use harpas_core::block::block_states;
use harpas_core::block_behavior::block_behavior;
use harpas_core::random::RandomSource;

use crate::{WorldgenData, WorldgenDataError};

#[derive(Debug, Clone)]
pub(super) struct BlockAge {
    mossiness: f32,
    stairs: Vec<bool>,
    slabs: Vec<bool>,
    walls: Vec<bool>,
}

impl BlockAge {
    pub(super) fn new(mossiness: f32, data: &WorldgenData) -> Result<Self, WorldgenDataError> {
        Ok(Self {
            mossiness,
            stairs: data.block_tag_states("minecraft:stairs")?,
            slabs: data.block_tag_states("minecraft:slabs")?,
            walls: data.block_tag_states("minecraft:walls")?,
        })
    }

    pub(super) fn process(&self, state: u32, random: &mut dyn RandomSource) -> u32 {
        let name = block_states().properties(state).unwrap().0;
        if matches!(
            name,
            "minecraft:stone" | "minecraft:stone_bricks" | "minecraft:chiseled_stone_bricks"
        ) {
            if random.next_float() >= 0.5 {
                return state;
            }
            // Both candidates draw their facing and half before the material and alternative.
            let stone = random_stairs("stone_brick_stairs", random);
            let mossy = random_stairs("mossy_stone_brick_stairs", random);
            let moss = random.next_float() < self.mossiness;
            match (moss, random.next_int_bounded(2)) {
                (true, 0) => id("mossy_stone_bricks"),
                (true, _) => mossy,
                (false, 0) => id("cracked_stone_bricks"),
                (false, _) => stone,
            }
        } else if self.stairs[state as usize] {
            if random.next_float() >= 0.5 {
                return state;
            }
            let moss = random.next_float() < self.mossiness;
            match (moss, random.next_int_bounded(2)) {
                (true, 0) => copy_all(state, "mossy_stone_brick_stairs"),
                (true, _) => id("mossy_stone_brick_slab"),
                (false, 0) => id("stone_slab"),
                (false, _) => id("stone_brick_slab"),
            }
        } else if self.slabs[state as usize] {
            if random.next_float() < self.mossiness {
                copy_all(state, "mossy_stone_brick_slab")
            } else {
                state
            }
        } else if self.walls[state as usize] {
            if random.next_float() < self.mossiness {
                copy_all(state, "mossy_stone_brick_wall")
            } else {
                state
            }
        } else if name == "minecraft:obsidian" && random.next_float() < 0.15 {
            id("crying_obsidian")
        } else {
            state
        }
    }
}

pub(super) fn blackstone(state: u32) -> u32 {
    let replacement = match block_states().properties(state).unwrap().0 {
        "minecraft:cobblestone" | "minecraft:mossy_cobblestone" => "blackstone",
        "minecraft:cobblestone_stairs" | "minecraft:mossy_cobblestone_stairs" => {
            "blackstone_stairs"
        }
        "minecraft:cobblestone_slab" | "minecraft:mossy_cobblestone_slab" => "blackstone_slab",
        "minecraft:cobblestone_wall" | "minecraft:mossy_cobblestone_wall" => "blackstone_wall",
        "minecraft:stone" => "polished_blackstone",
        "minecraft:stone_slab" | "minecraft:smooth_stone_slab" => "polished_blackstone_slab",
        "minecraft:stone_stairs" => "polished_blackstone_stairs",
        "minecraft:iron_bars" => "iron_chain",
        "minecraft:stone_bricks" | "minecraft:mossy_stone_bricks" => "polished_blackstone_bricks",
        "minecraft:stone_brick_stairs" | "minecraft:mossy_stone_brick_stairs" => {
            "polished_blackstone_brick_stairs"
        }
        "minecraft:stone_brick_slab" | "minecraft:mossy_stone_brick_slab" => {
            "polished_blackstone_brick_slab"
        }
        "minecraft:stone_brick_wall" | "minecraft:mossy_stone_brick_wall" => {
            "polished_blackstone_brick_wall"
        }
        "minecraft:cracked_stone_bricks" => "cracked_polished_blackstone_bricks",
        "minecraft:chiseled_stone_bricks" => "chiseled_polished_blackstone",
        _ => return state,
    };
    copy(state, replacement, &["facing", "half", "type"])
}

pub(super) fn full_shape(state: u32) -> bool {
    let (name, props) = block_states().properties(state).unwrap();
    match name {
        // Outline shapes that fill a block despite their smaller or absent collision shapes.
        "minecraft:soul_sand"
        | "minecraft:mud"
        | "minecraft:honey_block"
        | "minecraft:powder_snow"
        | "minecraft:cobweb"
        | "minecraft:end_gateway"
        | "minecraft:firefly_bush"
        | "minecraft:kelp_plant"
        | "minecraft:tall_grass"
        | "minecraft:large_fern"
        | "minecraft:sunflower"
        | "minecraft:lilac"
        | "minecraft:rose_bush"
        | "minecraft:peony"
        | "minecraft:pitcher_plant"
        | "minecraft:sculk_shrieker" => true,
        "minecraft:snow" => props.contains(&("layers", "8")),
        "minecraft:wheat" => props.contains(&("age", "7")),
        "minecraft:sweet_berry_bush" => props.contains(&("age", "3")),
        // Face-less attachment blocks use a full outline as their fallback selection shape.
        "minecraft:vine"
        | "minecraft:glow_lichen"
        | "minecraft:resin_clump"
        | "minecraft:sculk_vein" => !props
            .iter()
            .any(|(key, value)| *key != "waterlogged" && *value == "true"),
        "minecraft:pale_moss_carpet" => props
            .iter()
            .all(|(_, value)| matches!(*value, "false" | "none")),
        _ => block_behavior()
            .collision_shape(state)
            .iter()
            .any(|b| b.min == [0; 3] && b.max == [32; 3]),
    }
}

fn id(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}

fn copy(state: u32, replacement: &str, keys: &[&str]) -> u32 {
    let props = block_states().properties(state).unwrap().1;
    block_states()
        .state(
            &format!("minecraft:{replacement}"),
            props.into_iter().filter(|(key, _)| keys.contains(key)),
        )
        .unwrap()
        .0
}

fn copy_all(state: u32, replacement: &str) -> u32 {
    block_states()
        .state(
            &format!("minecraft:{replacement}"),
            block_states().properties(state).unwrap().1,
        )
        .unwrap()
        .0
}

fn random_stairs(name: &str, random: &mut dyn RandomSource) -> u32 {
    let facing = ["north", "east", "south", "west"][random.next_int_bounded(4) as usize];
    let half = if random.next_int_bounded(2) == 0 {
        "top"
    } else {
        "bottom"
    };
    block_states()
        .state(
            &format!("minecraft:{name}"),
            [("facing", facing), ("half", half)],
        )
        .unwrap()
        .0
}
