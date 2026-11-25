//! Direct lever power and the four-tick turn-off delay of a redstone lamp.

use harpas_core::block::block_states;
use harpas_worldgen::shape_update::Direction;

use crate::block::BlockPos;
use crate::neighbor::{UPDATE_ALL, UPDATE_CLIENTS, UPDATE_LIMIT};
use crate::place::{property, with};
use crate::world::World;

pub(crate) const LEVER: &str = "minecraft:lever";
pub(crate) const LAMP: &str = "minecraft:redstone_lamp";

pub(crate) fn lamp_id(id: &str) -> Option<&'static str> {
    matches!(id, LAMP | "redstone_lamp").then_some(LAMP)
}

impl World {
    pub(crate) fn toggle_lever(&mut self, pos: BlockPos, state: u32) {
        let powered = property(state, "powered") != Some("true");
        self.set_block(
            pos,
            with(
                state,
                &[("powered", if powered { "true" } else { "false" })],
            ),
            UPDATE_ALL,
            UPDATE_LIMIT,
        );
    }

    pub(crate) fn lever_signal(&self, pos: BlockPos) -> bool {
        Direction::ALL.iter().any(|dir| {
            let [x, y, z] = dir.step([pos.x, pos.y, pos.z]);
            self.block_state(BlockPos { x, y, z }).is_some_and(|state| {
                block_states().name(state) == Some(LEVER)
                    && property(state, "powered") == Some("true")
            })
        })
    }

    pub(crate) fn update_lamp(&mut self, pos: BlockPos, state: u32) {
        let powered = self.lever_signal(pos);
        if powered == (property(state, "lit") == Some("true")) {
            return;
        }
        if powered {
            self.set_block(
                pos,
                with(state, &[("lit", "true")]),
                UPDATE_CLIENTS,
                UPDATE_LIMIT,
            );
        } else if self.block_ticks.schedule(LAMP, pos, self.game_time, 4) {
            self.mark_changed(pos.chunk());
        }
    }

    pub(crate) fn tick_lamp(&mut self, pos: BlockPos, state: u32) {
        if property(state, "lit") == Some("true") && !self.lever_signal(pos) {
            self.set_block(
                pos,
                with(state, &[("lit", "false")]),
                UPDATE_CLIENTS,
                UPDATE_LIMIT,
            );
        }
    }
}
