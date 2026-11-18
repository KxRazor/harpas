//! Menus opened at a block. Each viewer has its own synchronization state and a view of the player's inventory.

use harpas_core::block::block_states;

use crate::Game;
use crate::block::BlockPos;
use crate::chest;
use crate::inventory::{CHEST_SLOTS, Inventory, ItemStack, MenuKind, OFF_HAND, Remote};
use crate::player::{Output, PlayerState};

pub(crate) struct BlockMenu {
    pub(crate) id: i32,
    pub(crate) position: BlockPos,
    pub(crate) contents: Inventory,
}

impl PlayerState {
    pub(crate) fn container_id(&self) -> i32 {
        self.block_menu.as_ref().map_or(0, |menu| menu.id)
    }

    pub(crate) fn chest_position(&self) -> Option<BlockPos> {
        let menu = self.block_menu.as_ref()?;
        (menu.contents.kind == MenuKind::Chest).then_some(menu.position)
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
    fn open_block_menu(&mut self, index: usize, position: BlockPos, contents: Inventory) -> i32 {
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
            contents,
        });
        id
    }

    pub(crate) fn open_crafting_table(&mut self, index: usize, position: BlockPos) {
        let id = self.open_block_menu(index, position, block_inventory(MenuKind::CraftingTable));
        self.players[index].send(Output::OpenCraftingTable(id));
        self.broadcast_full_state(index);
    }

    pub(crate) fn open_chest(&mut self, index: usize, position: BlockPos) {
        if !chest::is_single(self.block_at(position))
            || chest::blocks_opening(self.block_at(BlockPos {
                y: position.y + 1,
                ..position
            }))
        {
            return;
        }
        let Some(world) = &self.world else { return };
        let Ok(tag) = world.chest_tag(position) else {
            return;
        };
        if tag.is_some_and(|tag| tag.get("lock").is_some()) {
            return;
        }
        let Ok(items) = chest::read_items(tag, world.chest_codec()) else {
            return;
        };
        let title = tag.and_then(|tag| tag.get("CustomName")).cloned();
        let mut menu = block_inventory(MenuKind::Chest);
        menu.slots[..CHEST_SLOTS].clone_from_slice(&items);
        let id = self.open_block_menu(index, position, menu);
        self.players[index].send(Output::OpenChest {
            container_id: id,
            title,
        });
        self.broadcast_full_state(index);
    }

    /// A block menu remains valid while its block exists and is within eight blocks of the player's feet.
    pub(crate) fn validate_block_menu(&mut self, index: usize) {
        let Some(menu) = &self.players[index].block_menu else {
            return;
        };
        let p = self.players[index].player.position;
        let state = self.block_at(menu.position);
        let same_block = match menu.contents.kind {
            MenuKind::CraftingTable => {
                block_states().name(state) == Some("minecraft:crafting_table")
            }
            MenuKind::Chest => chest::is_single(state),
            MenuKind::Player => unreachable!("a player's inventory is not a block menu"),
        };
        let valid = same_block
            && [menu.position.x, menu.position.y, menu.position.z]
                .into_iter()
                .zip([p.x, p.y, p.z])
                .map(|(coordinate, feet)| (f64::from(coordinate) + 0.5 - feet).powi(2))
                .sum::<f64>()
                <= 64.0;
        if !valid {
            self.close_block_menu(index);
        }
    }

    pub(crate) fn close_block_menu(&mut self, index: usize) {
        let id = self.players[index].container_id();
        self.players[index].send(Output::CloseContainer(id));
        self.close_container(index, id);
    }

    pub(crate) fn synchronize_chest(&mut self, position: BlockPos, items: &[ItemStack]) {
        for player in &mut self.players {
            if player.chest_position() == Some(position) {
                let menu = player.block_menu.as_mut().expect("chest viewer has a menu");
                menu.contents.slots[..CHEST_SLOTS].clone_from_slice(items);
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
