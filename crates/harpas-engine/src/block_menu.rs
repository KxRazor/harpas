//! Menus opened at a block. Each viewer has its own synchronization state and a view of the player's inventory.

use harpas_core::block::block_states;

use crate::Game;
use crate::block::BlockPos;
use crate::chest::Chest;
use crate::inventory::{Inventory, ItemStack, MenuKind, OFF_HAND, Remote};
use crate::player::{Output, PlayerState};

pub(crate) struct BlockMenu {
    pub(crate) id: i32,
    pub(crate) position: BlockPos,
    pub(crate) chest: Option<Chest>,
    pub(crate) contents: Inventory,
}

impl PlayerState {
    pub(crate) fn container_id(&self) -> i32 {
        self.block_menu.as_ref().map_or(0, |menu| menu.id)
    }

    /// Borrows the active menu with the current player inventory, then writes back the shared slots.
    pub(crate) fn with_menu<R>(&mut self, f: impl FnOnce(&mut Inventory) -> R) -> R {
        let Some(block) = &mut self.block_menu else {
            return f(&mut self.inventory);
        };
        let menu = &mut block.contents;
        let slots = menu.kind.player_slots();
        let player_slots = MenuKind::Player.player_slots();
        menu.slots[slots.clone()].clone_from_slice(&self.inventory.slots[player_slots.clone()]);
        menu.slots[slots.end] = self.inventory.slots[OFF_HAND].clone();
        menu.selected = self.inventory.selected;
        menu.carried = self.inventory.carried.clone();
        let result = f(menu);
        self.inventory.slots[player_slots.clone()].clone_from_slice(&menu.slots[slots.clone()]);
        self.inventory.slots[OFF_HAND] = menu.slots[slots.end].clone();
        self.inventory.carried = menu.carried.clone();
        // The inactive inventory menu and the block menu have independent state IDs, but share these slots.
        self.inventory.remote[player_slots].clone_from_slice(&menu.remote[slots]);
        self.inventory.remote_carried = menu.remote_carried.clone();
        result
    }
}

impl Game {
    fn open_block_menu(
        &mut self,
        index: usize,
        position: BlockPos,
        contents: Inventory,
        chest: Option<Chest>,
    ) -> i32 {
        let active = self.players[index].container_id();
        if active != 0 {
            self.close_container(index, active);
            self.players[index].send(Output::CloseContainer(active));
        }
        let state = &mut self.players[index];
        state.next_container_id = state.next_container_id % 100 + 1;
        let id = state.next_container_id;
        state.block_menu = Some(BlockMenu {
            id,
            position,
            chest,
            contents,
        });
        id
    }

    pub(crate) fn open_crafting_table(&mut self, index: usize, position: BlockPos) {
        let id = self.open_block_menu(
            index,
            position,
            block_inventory(MenuKind::CraftingTable),
            None,
        );
        self.players[index].send(Output::OpenCraftingTable(id));
        self.broadcast_full_state(index);
    }

    pub(crate) fn open_chest(&mut self, index: usize, position: BlockPos) {
        let Some(world) = &mut self.world else { return };
        let Some(chest) = Chest::at(world, position) else {
            return;
        };
        if chest.is_blocked(world) {
            return;
        }
        let spectator =
            self.players[index].player.profile.game_mode == crate::player::GameMode::Spectator;
        let Ok((items, title)) = world.open_chest_menu(chest, spectator) else {
            return;
        };
        let double = chest.is_double();
        let mut menu = block_inventory(MenuKind::Chest { double });
        menu.slots[..items.len()].clone_from_slice(&items);
        let id = self.open_block_menu(index, position, menu, Some(chest));
        self.players[index].send(Output::OpenChest {
            container_id: id,
            double,
            title,
        });
        self.broadcast_full_state(index);
    }

    /// A block menu remains valid while its blocks still form that container and are within eight blocks.
    pub(crate) fn validate_block_menu(&mut self, index: usize) {
        let Some(menu) = &self.players[index].block_menu else {
            return;
        };
        let p = self.players[index].player.position;
        let in_range = |pos: BlockPos| {
            [pos.x, pos.y, pos.z]
                .into_iter()
                .zip([p.x, p.y, p.z])
                .map(|(coordinate, feet)| (f64::from(coordinate) + 0.5 - feet).powi(2))
                .sum::<f64>()
                <= 64.0
        };
        let valid = if let Some(chest) = menu.chest {
            self.world
                .as_ref()
                .and_then(|world| Chest::at(world, menu.position))
                == Some(chest)
                && chest.positions().all(in_range)
        } else {
            block_states().name(self.block_at(menu.position)) == Some("minecraft:crafting_table")
                && in_range(menu.position)
        };
        if !valid {
            self.close_block_menu(index);
        }
    }

    pub(crate) fn close_block_menu(&mut self, index: usize) {
        let id = self.players[index].container_id();
        self.players[index].send(Output::CloseContainer(id));
        self.close_container(index, id);
    }

    pub(crate) fn synchronize_chest(&mut self, chest: Chest, items: &[ItemStack]) {
        for player in &mut self.players {
            if let Some(menu) = &mut player.block_menu
                && menu.chest == Some(chest)
            {
                menu.contents.slots[..items.len()].clone_from_slice(items);
            }
        }
    }
}

fn block_inventory(kind: MenuKind) -> Inventory {
    let slots = kind.visible_slots() + 1;
    Inventory {
        kind,
        slots: vec![ItemStack::empty(); slots],
        remote: vec![Remote::Known(ItemStack::empty()); slots],
        ..Inventory::default()
    }
}
