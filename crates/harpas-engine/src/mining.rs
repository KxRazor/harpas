//! Mining speed and harvest rules for the first two pickaxes.

use std::io;

use harpas_core::block::block_states;
use harpas_worldgen::{WorldgenData, WorldgenDataError};

use crate::inventory::ItemStack;
use crate::item_data::ItemCodec;
use crate::world::World;

/// Mining rules resolved from the installed block tags.
#[derive(Debug)]
pub struct MiningRules {
    pickaxe_blocks: Vec<bool>,
    incorrect_wooden: Vec<bool>,
    incorrect_stone: Vec<bool>,
}

/// How the selected item breaks a block before player modifiers are applied.
#[derive(Debug, Clone, Copy)]
pub struct Mining {
    /// Base mining speed.
    pub speed: f32,
    /// Whether the player can collect this block's drops.
    pub can_harvest: bool,
    durability: Option<i32>,
}

impl Mining {
    pub(crate) fn by_hand(state: u32) -> Self {
        Self {
            speed: 1.0,
            can_harvest: !block_states().requires_correct_tool(state),
            durability: None,
        }
    }

    /// Applies the wear from a completed break to the corresponding tool. Returns whether it broke.
    pub fn wear(self, codec: &ItemCodec, tool: &mut ItemStack) -> io::Result<bool> {
        self.durability
            .map_or(Ok(false), |maximum| codec.wear_tool(tool, maximum))
    }
}

impl MiningRules {
    /// Resolves the pickaxe and tool-tier tags, including their nested block tags.
    pub fn new(data: &WorldgenData) -> Result<Self, WorldgenDataError> {
        Ok(Self {
            pickaxe_blocks: data.block_tag_states("minecraft:mineable/pickaxe")?,
            incorrect_wooden: data.block_tag_states("minecraft:incorrect_for_wooden_tool")?,
            incorrect_stone: data.block_tag_states("minecraft:incorrect_for_stone_tool")?,
        })
    }

    /// Determines speed and harvest eligibility for a held item with its default mining rules.
    pub fn for_block(&self, state: u32, tool: &ItemStack) -> Mining {
        let mut mining = Mining::by_hand(state);
        if tool.is_empty() {
            return mining;
        }
        let (speed, durability, incorrect) = match tool.name() {
            Some("minecraft:wooden_pickaxe") => (2.0, 59, &self.incorrect_wooden),
            Some("minecraft:stone_pickaxe") => (4.0, 131, &self.incorrect_stone),
            _ => return mining,
        };
        if block_states().hardness(state) > 0.0 {
            mining.durability = Some(durability);
        }
        let state = state as usize;
        if self.pickaxe_blocks.get(state) == Some(&true) {
            mining.speed = speed;
            mining.can_harvest |= !incorrect[state];
        }
        mining
    }
}

impl World {
    pub(crate) fn mining(&self, state: u32, tool: &ItemStack) -> Mining {
        self.mining
            .as_deref()
            .filter(|_| {
                !self
                    .block_item_codec()
                    .component_removed(tool, "minecraft:tool")
            })
            .map_or_else(
                || Mining::by_hand(state),
                |rules| rules.for_block(state, tool),
            )
    }
}
