//! Menus opened at a block. Each viewer has its own synchronization state and a view of the player's inventory.

use harpas_core::block::block_states;

use crate::Game;
use crate::block::BlockPos;
use crate::chest::Chest;
use crate::furnace::{FURNACE, FURNACE_SLOTS, Furnace};
use crate::inventory::{Inventory, ItemStack, MenuKind, OFF_HAND, Remote};
use crate::player::{Output, PlayerState};

pub(crate) struct BlockMenu {
    pub(crate) id: i32,
    pub(crate) position: BlockPos,
    pub(crate) backing: Backing,
    pub(crate) contents: Inventory,
}

#[derive(Clone, Copy)]
pub(crate) enum Backing {
    CraftingTable,
    Chest(Chest),
    Furnace([i16; 4]),
}

impl BlockMenu {
    pub(crate) fn uses(&self, pos: BlockPos) -> bool {
        match self.backing {
            Backing::CraftingTable => false,
            Backing::Chest(chest) => chest.positions().any(|half| half == pos),
            Backing::Furnace(_) => self.position == pos,
        }
    }
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
        backing: Backing,
    ) -> i32 {
        let active = self.players[index].container_id();
        if active != 0 {
            self.close_container(index, active);
            self.players[index].send(Output::CloseContainer(active));
        }
        let state = &mut self.players[index];
        state.pending_map_chest = None;
        state.next_container_id = state.next_container_id % 100 + 1;
        let id = state.next_container_id;
        state.block_menu = Some(BlockMenu {
            id,
            position,
            backing,
            contents,
        });
        id
    }

    pub(crate) fn open_crafting_table(&mut self, index: usize, position: BlockPos) {
        self.close_removed_crafting_tables();
        let id = self.open_block_menu(
            index,
            position,
            block_inventory(MenuKind::CraftingTable),
            Backing::CraftingTable,
        );
        self.players[index].send(Output::OpenCraftingTable(id));
        self.broadcast_full_state(index);
    }

    pub(crate) fn close_removed_crafting_tables(&mut self) {
        let Some(world) = &mut self.world else { return };
        if world.broken_crafting_tables.is_empty() {
            return;
        }
        let broken = std::mem::take(&mut world.broken_crafting_tables);
        for index in 0..self.players.len() {
            if self.players[index].block_menu.as_ref().is_some_and(|menu| {
                matches!(menu.backing, Backing::CraftingTable) && broken.contains(&menu.position)
            }) {
                self.close_block_menu(index);
            }
        }
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
        let (items, title) = match world.open_chest_menu(chest, spectator) {
            Ok(menu) => menu,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                self.players[index].pending_map_chest =
                    Some(crate::maps::PendingChestAction::Open(
                        position,
                        world.maps.watch(chest.positions()),
                    ));
                return;
            }
            Err(error) => {
                log::debug!("Cannot open chest at {position:?}: {error}");
                return;
            }
        };
        let double = chest.is_double();
        let mut menu = block_inventory(MenuKind::Chest { double });
        menu.slots[..items.len()].clone_from_slice(&items);
        let id = self.open_block_menu(index, position, menu, Backing::Chest(chest));
        self.players[index].send(Output::OpenChest {
            container_id: id,
            double,
            title,
        });
        self.broadcast_full_state(index);
    }

    pub(crate) fn open_furnace(&mut self, index: usize, position: BlockPos) {
        let Some(world) = &self.world else { return };
        let tag = match world.container_tag(position, FURNACE) {
            Ok(tag) => tag,
            Err(error) => {
                log::warn!("Cannot load furnace at {position:?}: {error}");
                return;
            }
        };
        if tag.is_some_and(|tag| tag.get("lock").is_some()) {
            return;
        }
        let title = tag.and_then(|tag| tag.get("CustomName").cloned());
        let furnace = match world.furnace(position) {
            Ok(furnace) => furnace,
            Err(error) => {
                log::warn!("Cannot load furnace at {position:?}: {error}");
                return;
            }
        };
        let mut menu = block_inventory(MenuKind::Furnace);
        menu.slots[..FURNACE_SLOTS].clone_from_slice(&furnace.slots);
        let id = self.open_block_menu(
            index,
            position,
            menu,
            Backing::Furnace(furnace.properties()),
        );
        self.players[index].send(Output::OpenFurnace {
            container_id: id,
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
        let at = |kind| {
            block_states().name(self.block_at(menu.position)) == Some(kind)
                && in_range(menu.position)
        };
        let valid = match menu.backing {
            Backing::Chest(chest) => {
                self.world
                    .as_ref()
                    .and_then(|world| Chest::at(world, menu.position))
                    == Some(chest)
                    && chest.positions().all(in_range)
            }
            Backing::CraftingTable => at("minecraft:crafting_table"),
            Backing::Furnace(_) => at(FURNACE),
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
                && matches!(menu.backing, Backing::Chest(open) if open == chest)
            {
                menu.contents.slots[..items.len()].clone_from_slice(items);
            }
        }
    }

    pub(crate) fn synchronize_furnace(&mut self, position: BlockPos, furnace: &Furnace) {
        let properties = furnace.properties();
        for player in &mut self.players {
            if let Some(menu) = &mut player.block_menu
                && menu.position == position
                && let Backing::Furnace(sent) = &mut menu.backing
            {
                menu.contents.slots[..FURNACE_SLOTS].clone_from_slice(&furnace.slots);
                let previous = std::mem::replace(sent, properties);
                let id = menu.id;
                for (property, (value, old)) in properties.into_iter().zip(previous).enumerate() {
                    if value != old {
                        player.send(Output::ContainerProperty {
                            container_id: id,
                            property: property as i16,
                            value,
                        });
                    }
                }
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
