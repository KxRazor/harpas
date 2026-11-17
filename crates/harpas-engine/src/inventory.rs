//! A player's inventory, as vanilla's `Inventory` and `InventoryMenu` hold it, and its changes sent to the client
//! (`AbstractContainerMenu.broadcastChanges`).
//!
//! The inventory has the 46 slots of the inventory menu: the crafting result and grid (0 to 4), the armor (5 to
//! 8), the main inventory (9 to 35), the hotbar (36 to 44) and the off hand (45). Slots the client changes in
//! Creative are taken as they are; a slot the server changes is sent at the player's next tick, each change
//! counted in the menu's state ID.
//!
//! Not done yet: reading and saving the inventory with the player's data, which Harpas does not save yet, so an
//! inventory starts empty; the other containers; and clicks in Survival.

use std::sync::Arc;

use harpas_core::item::items;

/// The number of slots of the inventory menu.
pub const SLOTS: usize = 46;
/// The first hotbar slot of the inventory menu.
pub const HOTBAR: usize = 36;
/// The off hand's slot of the inventory menu.
pub const OFF_HAND: usize = 45;
/// The most items a stack can hold with its components (`ItemStack.MAX_STACK_SIZE`).
const MAX_STACK_SIZE: i32 = 99;

/// The changes to an item's default data components, in the Java network encoding of their values, which are not
/// decoded yet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComponentPatch {
    /// Added or replaced components: each type's numeric ID and its encoded value.
    pub added: Vec<(i32, Vec<u8>)>,
    /// Removed components, by type ID.
    pub removed: Vec<i32>,
}

/// A stack of items, or none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemStack {
    /// The item's numeric ID in `minecraft:item`.
    pub item: u32,
    /// How many; zero is no stack.
    pub count: i32,
    /// The changes to the item's default components, shared by the copies of the stack.
    pub components: Arc<ComponentPatch>,
}

impl ItemStack {
    /// No stack (`ItemStack.EMPTY`).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Whether this is no stack, or air (`ItemStack.isEmpty`).
    pub fn is_empty(&self) -> bool {
        self.count <= 0 || self.item == 0
    }

    /// The item's name, such as `minecraft:stone`.
    pub fn name(&self) -> Option<&'static str> {
        items().name(self.item)
    }

    /// The most items this stack can hold: its item's default, unless its components may change it, up to 99.
    pub(crate) fn max_stack_size(&self) -> Option<i32> {
        let default = i32::from(items().max_stack_size(self.item)?);
        Some(if self.components.added.is_empty() {
            default
        } else {
            MAX_STACK_SIZE
        })
    }

    /// Whether a client may send the stack (`ItemStack.validateStrict`): a known item, at most as many as its
    /// stack holds.
    pub(crate) fn is_valid(&self) -> bool {
        self.is_empty() || self.max_stack_size().is_some_and(|max| self.count <= max)
    }
}

/// The slots of a player's inventory menu, and what the client was last told of them.
#[derive(Debug)]
pub(crate) struct Inventory {
    pub(crate) slots: Vec<ItemStack>,
    /// What the client holds in each slot, as far as the server knows (`remoteSlots`).
    remote: Vec<ItemStack>,
    /// The hotbar slot in the main hand, 0 to 8 (`Inventory.selected`).
    pub(crate) selected: usize,
    /// The menu's count of changes sent (`stateId`), from 0 to 32,767.
    state_id: i32,
}

impl Default for Inventory {
    fn default() -> Self {
        Self {
            slots: vec![ItemStack::empty(); SLOTS],
            remote: vec![ItemStack::empty(); SLOTS],
            selected: 0,
            state_id: 0,
        }
    }
}

impl Inventory {
    /// `incrementStateId`.
    pub(crate) fn next_state_id(&mut self) -> i32 {
        self.state_id = (self.state_id + 1) & 32_767;
        self.state_id
    }

    /// The slot of a hand: the selected hotbar slot, or the off hand.
    pub(crate) fn hand_slot(&self, off_hand: bool) -> usize {
        if off_hand {
            OFF_HAND
        } else {
            HOTBAR + self.selected
        }
    }

    /// Puts a stack the client chose in a slot, which the client already shows (`setByPlayer` and
    /// `setRemoteSlot`).
    pub(crate) fn set_by_client(&mut self, slot: usize, stack: ItemStack) {
        self.remote[slot] = stack.clone();
        self.slots[slot] = stack;
    }

    /// The slots whose stacks the client has not been told of (`broadcastChanges`), each marked as told.
    pub(crate) fn changes(&mut self) -> Vec<(usize, ItemStack)> {
        let mut changed = Vec::new();
        for slot in 0..SLOTS {
            if self.slots[slot] != self.remote[slot] {
                self.remote[slot] = self.slots[slot].clone();
                changed.push((slot, self.slots[slot].clone()));
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(name: &str, count: i32) -> ItemStack {
        ItemStack {
            item: items().id(name).unwrap(),
            count,
            components: Arc::default(),
        }
    }

    #[test]
    fn stacks_hold_at_most_their_items_stack_size() {
        assert!(stack("minecraft:stone", 64).is_valid());
        assert!(!stack("minecraft:stone", 65).is_valid());
        assert!(!stack("minecraft:diamond_sword", 2).is_valid());
        assert!(ItemStack::empty().is_valid());
        let unknown = ItemStack {
            item: 100_000,
            count: 1,
            components: Arc::default(),
        };
        assert!(!unknown.is_valid());
    }

    #[test]
    fn only_slots_the_client_was_not_told_of_are_sent() {
        let mut inventory = Inventory::default();
        inventory.set_by_client(36, stack("minecraft:stone", 3));
        assert!(inventory.changes().is_empty());
        inventory.slots[36].count = 2;
        assert_eq!(inventory.changes(), [(36, stack("minecraft:stone", 2))]);
        assert!(inventory.changes().is_empty());
        assert_eq!(inventory.next_state_id(), 1);
    }
}
