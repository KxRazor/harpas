//! The crafting table's transient menu. Its grid belongs to the menu; its inventory slots are a view of the
//! player's inventory. Clicks use the same rules as the inventory menu, with the table's slot layout.

use crate::Game;
use crate::block::BlockPos;
use crate::inventory::{Inventory, ItemStack, Remote};
use crate::player::{Output, PlayerState};

pub(crate) struct CraftingTable {
    pub(crate) id: i32,
    pub(crate) position: BlockPos,
    pub(crate) contents: Inventory,
}

impl PlayerState {
    pub(crate) fn container_id(&self) -> i32 {
        self.crafting_table.as_ref().map_or(0, |table| table.id)
    }

    /// Borrows the active menu with the current player inventory, then writes back the shared slots. World
    /// interactions and item pickups continue to use the player's inventory while the table is open.
    pub(crate) fn with_menu<R>(&mut self, f: impl FnOnce(&mut Inventory) -> R) -> R {
        let Some(table) = &mut self.crafting_table else {
            return f(&mut self.inventory);
        };
        let menu = &mut table.contents;
        menu.slots[10..46].clone_from_slice(&self.inventory.slots[9..45]);
        menu.slots[46] = self.inventory.slots[45].clone();
        menu.selected = self.inventory.selected;
        menu.carried = self.inventory.carried.clone();
        let result = f(menu);
        self.inventory.slots[9..45].clone_from_slice(&menu.slots[10..46]);
        self.inventory.slots[45] = menu.slots[46].clone();
        self.inventory.carried = menu.carried.clone();
        // Both menus see the same inventory slots. Keep the inactive menu's remote view in step with what
        // the active one sent or the client predicted; its own state ID remains independent.
        self.inventory.remote[9..45].clone_from_slice(&menu.remote[10..46]);
        self.inventory.remote_carried = menu.remote_carried.clone();
        result
    }
}

impl Game {
    pub(crate) fn open_crafting_table(&mut self, index: usize, position: BlockPos) {
        if self.players[index].crafting_table.is_some() {
            let id = self.players[index].container_id();
            self.close_container(index, id);
            self.players[index].send(Output::CloseContainer(id));
        }
        let state = &mut self.players[index];
        state.next_container_id = state.next_container_id % 100 + 1;
        let id = state.next_container_id;
        state.crafting_table = Some(CraftingTable {
            id,
            position,
            contents: Inventory {
                table: true,
                slots: vec![ItemStack::empty(); 47],
                remote: vec![Remote::Known(ItemStack::empty()); 47],
                ..Inventory::default()
            },
        });
        state.send(Output::OpenCraftingTable(id));
        self.broadcast_full_state(index);
    }

    /// `CraftingMenu.stillValid`: the table still exists and is within eight blocks of the player's feet.
    pub(crate) fn validate_crafting_table(&mut self, index: usize) {
        let Some(table) = &self.players[index].crafting_table else {
            return;
        };
        let p = self.players[index].player.position;
        let valid = harpas_core::block::block_states().name(self.block_at(table.position))
            == Some("minecraft:crafting_table")
            && [table.position.x, table.position.y, table.position.z]
                .into_iter()
                .zip([p.x, p.y, p.z])
                .map(|(coordinate, feet)| (f64::from(coordinate) + 0.5 - feet).powi(2))
                .sum::<f64>()
                <= 64.0;
        if !valid {
            let id = table.id;
            self.players[index].send(Output::CloseContainer(id));
            self.close_container(index, id);
        }
    }
}
