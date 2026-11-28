//! Clicks in the player's inventory menu, as vanilla's `AbstractContainerMenu.doClick` and `InventoryMenu` handle
//! them, including the crafting grids and chest menus.
//!
//! Every click type is ported: picking up and putting down stacks or one item (`PICKUP`), moving a stack to the
//! other part of the inventory (`QUICK_MOVE`, `InventoryMenu.quickMoveStack`), swapping with a hotbar slot or the
//! off hand (`SWAP`), taking a full stack in Creative (`CLONE`), throwing (`THROW`), dragging a stack over slots
//! (`QUICK_CRAFT`) and gathering a stack (`PICKUP_ALL`). The armor slots take only armor of their kind, one at a
//! time, and the result slot only gives. The result of the crafting grid is crafted again whenever the grid
//! changes, and taking it uses one item of each slot of the grid, leaving the remainders of buckets and bottles.
//!
//! Bundles and the limit on how fast a player throws items remain to be implemented.

use std::sync::Arc;

use crate::furnace::{FUEL, INPUT, RESULT as SMELTED};
use crate::inventory::{Inventory, ItemStack, MenuKind, OFF_HAND};
use crate::item_data::ItemCodec;
use crate::recipe::{Recipes, remainder};

/// The result slot of the inventory menu.
const RESULT: usize = 0;
/// The armor slots, from the head down.
const ARMOR: std::ops::Range<usize> = 5..9;
/// A click outside the window.
const OUTSIDE: i16 = -999;

/// A drag in progress (`quickcraftStatus`, `quickcraftType`, `quickcraftSlots`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct QuickCraft {
    status: i32,
    kind: i32,
    slots: Vec<usize>,
}

/// What a click needs from the world and whether the player is in Creative.
pub(crate) struct Context<'a> {
    pub(crate) recipes: Option<&'a Recipes>,
    pub(crate) codec: &'a ItemCodec,
    pub(crate) creative: bool,
}

/// The armor slot an item goes in (`getEquipmentSlotForItem`'s armor), by the kind of item its name says.
fn armor_slot(stack: &ItemStack) -> Option<usize> {
    let name = stack.name()?;
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    if name.ends_with("_helmet")
        || name.ends_with("_skull")
        || name.ends_with("_head")
        || name == "carved_pumpkin"
    {
        Some(5)
    } else if name.ends_with("_chestplate") || name == "elytra" {
        Some(6)
    } else if name.ends_with("_leggings") {
        Some(7)
    } else if name.ends_with("_boots") {
        Some(8)
    } else {
        None
    }
}

fn same_item_same_components(a: &ItemStack, b: &ItemStack) -> bool {
    a.item == b.item && a.components == b.components
}

fn max_stack(stack: &ItemStack) -> i32 {
    stack.max_stack_size().unwrap_or(64)
}

fn with_count(stack: &ItemStack, count: i32) -> ItemStack {
    if count <= 0 {
        return ItemStack::empty();
    }
    ItemStack {
        count,
        ..stack.clone()
    }
}

impl Inventory {
    fn is_crafting_result(&self, slot: usize) -> bool {
        matches!(self.kind, MenuKind::Player | MenuKind::CraftingTable) && slot == RESULT
    }

    fn may_pickup(&self, slot: usize, context: &Context<'_>) -> bool {
        context.creative
            || self.kind != MenuKind::Player
            || !ARMOR.contains(&slot)
            || context
                .codec
                .enchantment_level(&self.slots[slot], "minecraft:binding_curse")
                == 0
    }

    /// `Slot.mayPlace`.
    fn may_place(&self, slot: usize, stack: &ItemStack, context: &Context<'_>) -> bool {
        match slot {
            SMELTED if self.kind == MenuKind::Furnace => false,
            FUEL if self.kind == MenuKind::Furnace => {
                context.recipes.is_some_and(|r| r.burn_duration(stack) > 0)
                    || (stack.name() == Some("minecraft:bucket")
                        && self.slots[FUEL].name() != Some("minecraft:bucket"))
            }
            s if self.is_crafting_result(s) => false,
            s if self.kind == MenuKind::Player && ARMOR.contains(&s) => {
                armor_slot(stack) == Some(s)
            }
            _ => true,
        }
    }

    /// `Slot.getMaxStackSize(stack)`: one in an armor slot, otherwise as many as the stack holds.
    fn slot_max(&self, slot: usize, stack: &ItemStack) -> i32 {
        let limit = if (self.kind == MenuKind::Player && ARMOR.contains(&slot))
            || (self.kind == MenuKind::Furnace
                && slot == FUEL
                && stack.name() == Some("minecraft:bucket"))
        {
            1
        } else {
            99
        };
        limit.min(max_stack(stack))
    }

    /// `Slot.setByPlayer`: an empty stack clears the slot.
    fn set(&mut self, slot: usize, stack: ItemStack) {
        self.slots[slot] = if stack.is_empty() {
            ItemStack::empty()
        } else {
            stack
        };
    }

    /// `Slot.safeInsert`: puts up to `count` of `stack` in the slot; returns what is left.
    fn safe_insert(
        &mut self,
        slot: usize,
        mut stack: ItemStack,
        count: i32,
        context: &Context<'_>,
    ) -> ItemStack {
        if stack.is_empty() || !self.may_place(slot, &stack, context) {
            return stack;
        }
        let held = self.slots[slot].clone();
        let n = count
            .min(stack.count)
            .min(self.slot_max(slot, &stack) - held.count);
        if n <= 0 {
            return stack;
        }
        if held.is_empty() {
            self.set(slot, with_count(&stack, n));
        } else if same_item_same_components(&held, &stack) {
            self.slots[slot].count += n;
        } else {
            return stack;
        }
        stack.count -= n;
        if stack.count <= 0 {
            ItemStack::empty()
        } else {
            stack
        }
    }

    /// `Slot.tryRemove`: up to `count` of the slot's stack, no more than `decrement`. The result slot gives its
    /// whole stack or nothing, and taking it crafts.
    fn try_remove(
        &mut self,
        slot: usize,
        count: i32,
        decrement: i32,
        context: &Context<'_>,
        drops: &mut Vec<ItemStack>,
    ) -> Option<ItemStack> {
        let held = self.slots[slot].clone();
        if held.is_empty() || !self.may_pickup(slot, context) {
            return None;
        }
        if self.is_crafting_result(slot) {
            if decrement < held.count {
                return None;
            }
            self.slots[RESULT] = ItemStack::empty();
            self.take_result(context, drops);
            return Some(held);
        }
        let n = count.min(decrement).min(held.count);
        if n <= 0 {
            return None;
        }
        let taken = with_count(&held, n);
        self.set(slot, with_count(&held, held.count - n));
        if self.grid().contains(&slot) {
            self.update_result(context);
        }
        Some(taken)
    }

    /// `Slot.safeTake`.
    fn safe_take(
        &mut self,
        slot: usize,
        count: i32,
        decrement: i32,
        context: &Context<'_>,
        drops: &mut Vec<ItemStack>,
    ) -> ItemStack {
        self.try_remove(slot, count, decrement, context, drops)
            .unwrap_or_default()
    }

    /// Crafts the result of the grid again (`slotChangedCraftingGrid`).
    pub(crate) fn update_result(&mut self, context: &Context<'_>) {
        if self.is_crafting_result(RESULT) {
            let grid: Vec<&ItemStack> = self.grid().map(|slot| &self.slots[slot]).collect();
            self.slots[RESULT] = context.recipes.map_or_else(ItemStack::empty, |recipes| {
                recipes.craft(
                    if self.kind == MenuKind::CraftingTable {
                        3
                    } else {
                        2
                    },
                    &grid,
                )
            });
        }
    }

    /// `ResultSlot.onTake`: one item of each slot of the grid goes, and an item's remainder takes its place, or
    /// goes to the inventory, or is dropped.
    fn take_result(&mut self, context: &Context<'_>, drops: &mut Vec<ItemStack>) {
        for slot in self.grid() {
            let used = self.slots[slot].clone();
            if used.is_empty() {
                continue;
            }
            self.set(slot, with_count(&used, used.count - 1));
            let Some(left) = remainder(used.item) else {
                continue;
            };
            let mut left = ItemStack {
                item: left,
                count: 1,
                components: Arc::default(),
            };
            let now = self.slots[slot].clone();
            if now.is_empty() {
                self.set(slot, left);
            } else if same_item_same_components(&now, &left) {
                self.slots[slot].count += 1;
            } else if !self.add(&mut left, context.creative) {
                drops.push(left);
            }
        }
        self.update_result(context);
    }

    /// `AbstractContainerMenu.moveItemStackTo`: into stacks of the same item in the range first, then the first
    /// empty slot that takes it. Returns whether anything moved.
    fn move_stack_to(
        &mut self,
        stack: &mut ItemStack,
        start: usize,
        end: usize,
        reverse: bool,
        context: &Context<'_>,
    ) -> bool {
        let order: Vec<usize> = if reverse {
            (start..end).rev().collect()
        } else {
            (start..end).collect()
        };
        let mut moved = false;
        if max_stack(stack) > 1 {
            for &slot in &order {
                if stack.is_empty() {
                    break;
                }
                let held = &self.slots[slot];
                if held.is_empty() || !same_item_same_components(stack, held) {
                    continue;
                }
                let total = held.count + stack.count;
                let max = self.slot_max(slot, held);
                if total <= max {
                    stack.count = 0;
                    self.slots[slot].count = total;
                    moved = true;
                } else if held.count < max {
                    stack.count -= max - held.count;
                    self.slots[slot].count = max;
                    moved = true;
                }
            }
        }
        if !stack.is_empty() {
            for &slot in &order {
                if self.slots[slot].is_empty() && self.may_place(slot, stack, context) {
                    let n = stack.count.min(self.slot_max(slot, stack));
                    self.set(slot, with_count(stack, n));
                    stack.count -= n;
                    moved = true;
                    break;
                }
            }
        }
        moved
    }

    /// `InventoryMenu.quickMoveStack`: the stack moved, or none.
    fn quick_move_stack(
        &mut self,
        index: usize,
        context: &Context<'_>,
        drops: &mut Vec<ItemStack>,
    ) -> ItemStack {
        let mut stack = self.slots[index].clone();
        if stack.is_empty() || !self.may_pickup(index, context) {
            return ItemStack::empty();
        }
        let original = stack.clone();
        let armor = armor_slot(&original).filter(|&slot| self.slots[slot].is_empty());
        let off_hand = self.kind == MenuKind::Player
            && original.name() == Some("minecraft:shield")
            && self.slots[OFF_HAND].is_empty();
        let kind = self.kind;
        let player = kind.player_slots();
        let mut move_to =
            |start, end, reverse| self.move_stack_to(&mut stack, start, end, reverse, context);
        let moved = match kind {
            MenuKind::Chest { .. } => {
                if index < player.start {
                    move_to(player.start, player.end, true)
                } else {
                    move_to(0, player.start, false)
                }
            }
            MenuKind::Furnace => match index {
                SMELTED => move_to(player.start, player.end, true),
                INPUT | FUEL => move_to(player.start, player.end, false),
                _ if context
                    .recipes
                    .is_some_and(|r| r.smelting(&original).is_some()) =>
                {
                    move_to(INPUT, INPUT + 1, false)
                }
                _ if context
                    .recipes
                    .is_some_and(|r| r.burn_duration(&original) > 0) =>
                {
                    move_to(FUEL, FUEL + 1, false)
                }
                _ if index < player.end - 9 => move_to(player.end - 9, player.end, false),
                _ => move_to(player.start, player.end - 9, false),
            },
            MenuKind::CraftingTable => match index {
                RESULT => move_to(10, 46, true),
                1..=9 => move_to(10, 46, false),
                10..=36 => move_to(37, 46, false),
                _ => move_to(10, 37, false),
            },
            MenuKind::Player => match index {
                RESULT => move_to(9, 45, true),
                1..=8 => move_to(9, 45, false),
                _ if armor.is_some() => {
                    let a = armor.expect("an armor slot");
                    move_to(a, a + 1, false)
                }
                _ if off_hand => move_to(OFF_HAND, OFF_HAND + 1, false),
                9..=35 => move_to(36, 45, false),
                36..=44 => move_to(9, 36, false),
                _ => move_to(9, 45, false),
            },
        };
        if !moved {
            return ItemStack::empty();
        }
        // What is left stays in the slot.
        self.slots[index] = if stack.is_empty() {
            ItemStack::empty()
        } else {
            stack.clone()
        };
        if self.grid().contains(&index) {
            self.update_result(context);
        }
        if stack.count == original.count {
            return ItemStack::empty();
        }
        if self.is_crafting_result(index) {
            // `onTake`, then what did not fit is dropped.
            self.slots[RESULT] = ItemStack::empty();
            self.take_result(context, drops);
            if !stack.is_empty() {
                drops.push(stack);
            }
        }
        original
    }

    fn reset_quick_craft(&mut self) {
        self.quickcraft = QuickCraft::default();
    }

    /// `canItemQuickReplace`.
    fn can_quick_replace(&self, slot: usize, stack: &ItemStack, overflow: bool) -> bool {
        let held = &self.slots[slot];
        if held.is_empty() {
            return true;
        }
        same_item_same_components(stack, held)
            && held.count + if overflow { 0 } else { stack.count } <= max_stack(stack)
    }

    /// `AbstractContainerMenu.doClick` for the inventory menu. Returns the stacks the player drops.
    pub(crate) fn click(
        &mut self,
        slot: i16,
        button: i8,
        mode: i32,
        context: &Context<'_>,
    ) -> Vec<ItemStack> {
        let mut drops = Vec::new();
        let slots = self.kind.visible_slots();
        let index = usize::try_from(slot).ok().filter(|&s| s < slots);
        let button = i32::from(button);
        if mode == 5 {
            self.quick_craft(index, button, context, &mut drops);
            return drops;
        }
        if self.quickcraft.status != 0 {
            self.reset_quick_craft();
            return drops;
        }
        match mode {
            // PICKUP and QUICK_MOVE.
            0 | 1 if button == 0 || button == 1 => {
                let primary = button == 0;
                if slot == OUTSIDE {
                    if !self.carried.is_empty() {
                        if primary {
                            drops.push(std::mem::take(&mut self.carried));
                        } else {
                            drops.push(with_count(&self.carried, 1));
                            self.carried = with_count(&self.carried, self.carried.count - 1);
                        }
                    }
                    return drops;
                }
                let Some(index) = index else { return drops };
                if mode == 1 {
                    let mut moved = self.quick_move_stack(index, context, &mut drops);
                    while !moved.is_empty()
                        && self.slots[index].item == moved.item
                        && !self.slots[index].is_empty()
                    {
                        moved = self.quick_move_stack(index, context, &mut drops);
                    }
                    return drops;
                }
                self.pickup(index, primary, context, &mut drops);
            }
            // SWAP with a hotbar slot or the off hand.
            2 if (0..9).contains(&button) || button == 40 => {
                let Some(index) = index else { return drops };
                let other = self.storage_slot(button as usize);
                if index == other || !self.may_pickup(index, context) {
                    return drops;
                }
                let theirs = self.slots[other].clone();
                let ours = self.slots[index].clone();
                if theirs.is_empty() && ours.is_empty() {
                    return drops;
                }
                if theirs.is_empty() {
                    if self.is_crafting_result(index) {
                        self.slots[RESULT] = ItemStack::empty();
                        self.set(other, ours);
                        self.take_result(context, &mut drops);
                    } else {
                        self.set(other, ours);
                        self.set(index, ItemStack::empty());
                    }
                } else if ours.is_empty() {
                    if self.may_place(index, &theirs, context) {
                        let max = self.slot_max(index, &theirs);
                        if theirs.count > max {
                            self.set(index, with_count(&theirs, max));
                            self.set(other, with_count(&theirs, theirs.count - max));
                        } else {
                            self.set(other, ItemStack::empty());
                            self.set(index, theirs);
                        }
                    }
                } else if self.may_place(index, &theirs, context) {
                    let max = self.slot_max(index, &theirs);
                    if theirs.count > max {
                        self.set(index, with_count(&theirs, max));
                        self.set(other, with_count(&theirs, theirs.count - max));
                        let mut back = ours;
                        self.add(&mut back, context.creative);
                        if !back.is_empty() {
                            drops.push(back);
                        }
                    } else {
                        self.set(other, ours);
                        self.set(index, theirs);
                    }
                }
                if self.grid().contains(&index) {
                    self.update_result(context);
                }
            }
            // CLONE, in Creative.
            3 if context.creative && self.carried.is_empty() => {
                if let Some(index) = index.filter(|&i| !self.slots[i].is_empty()) {
                    let held = &self.slots[index];
                    self.carried = with_count(held, max_stack(held));
                }
            }
            // THROW.
            4 if self.carried.is_empty() => {
                let Some(index) = index else { return drops };
                let count = if button == 0 {
                    1
                } else {
                    self.slots[index].count
                };
                let thrown = self.safe_take(index, count, i32::MAX, context, &mut drops);
                let item = thrown.item;
                drops.push(thrown);
                if button == 1 {
                    while !self.slots[index].is_empty() && self.slots[index].item == item {
                        let thrown = self.safe_take(index, count, i32::MAX, context, &mut drops);
                        if thrown.is_empty() {
                            break;
                        }
                        drops.push(thrown);
                    }
                }
            }
            // PICKUP_ALL.
            6 => {
                let Some(index) = index else { return drops };
                if self.carried.is_empty() || !self.slots[index].is_empty() {
                    return drops;
                }
                let (start, step): (i64, i64) = if button == 0 {
                    (0, 1)
                } else {
                    (slots as i64 - 1, -1)
                };
                for pass in 0..2 {
                    let mut i = start;
                    while (0..slots as i64).contains(&i)
                        && self.carried.count < max_stack(&self.carried)
                    {
                        let s = i as usize;
                        let held = self.slots[s].clone();
                        if !self.is_crafting_result(s)
                            && !held.is_empty()
                            && self.can_quick_replace(s, &self.carried.clone(), true)
                            && (pass != 0 || held.count != max_stack(&held))
                        {
                            let room = max_stack(&self.carried) - self.carried.count;
                            let taken = self.safe_take(s, held.count, room, context, &mut drops);
                            self.carried.count += taken.count;
                        }
                        i += step;
                    }
                }
            }
            _ => {}
        }
        drops
    }

    /// The `PICKUP` click on a slot.
    fn pickup(
        &mut self,
        index: usize,
        primary: bool,
        context: &Context<'_>,
        drops: &mut Vec<ItemStack>,
    ) {
        let held = self.slots[index].clone();
        let carried = self.carried.clone();
        if held.is_empty() {
            if !carried.is_empty() {
                let n = if primary { carried.count } else { 1 };
                self.carried = self.safe_insert(index, carried, n, context);
            }
        } else if carried.is_empty() {
            let n = if primary {
                held.count
            } else {
                (held.count + 1) / 2
            };
            if let Some(taken) = self.try_remove(index, n, i32::MAX, context, drops) {
                self.carried = taken;
            }
        } else if self.may_place(index, &carried, context) {
            if same_item_same_components(&held, &carried) {
                let n = if primary { carried.count } else { 1 };
                self.carried = self.safe_insert(index, carried, n, context);
            } else if self.may_pickup(index, context)
                && carried.count <= self.slot_max(index, &carried)
            {
                self.carried = held;
                self.set(index, carried);
            }
        } else if same_item_same_components(&held, &carried) {
            let room = max_stack(&carried) - carried.count;
            if let Some(taken) = self.try_remove(index, held.count, room, context, drops) {
                self.carried.count += taken.count;
            }
        }
        if self.grid().contains(&index) {
            self.update_result(context);
        }
    }

    /// The `QUICK_CRAFT` click: a drag starts, crosses slots, and ends by sharing the carried stack among them.
    fn quick_craft(
        &mut self,
        index: Option<usize>,
        button: i32,
        context: &Context<'_>,
        drops: &mut Vec<ItemStack>,
    ) {
        let previous = self.quickcraft.status;
        self.quickcraft.status = button & 3;
        let broken =
            (previous != 1 || self.quickcraft.status != 2) && previous != self.quickcraft.status;
        if broken || self.carried.is_empty() {
            self.reset_quick_craft();
        } else if self.quickcraft.status == 0 {
            self.quickcraft.kind = (button >> 2) & 3;
            let valid = self.quickcraft.kind == 0
                || self.quickcraft.kind == 1
                || (self.quickcraft.kind == 2 && context.creative);
            if valid {
                self.quickcraft.status = 1;
                self.quickcraft.slots.clear();
            } else {
                self.reset_quick_craft();
            }
        } else if self.quickcraft.status == 1 {
            let Some(slot) = index else { return };
            let carried = self.carried.clone();
            if !self.is_crafting_result(slot)
                && self.can_quick_replace(slot, &carried, true)
                && self.may_place(slot, &carried, context)
                && (self.quickcraft.kind == 2
                    || carried.count as usize > self.quickcraft.slots.len())
                && !self.quickcraft.slots.contains(&slot)
            {
                self.quickcraft.slots.push(slot);
            }
        } else if self.quickcraft.status == 2 {
            let slots = self.quickcraft.slots.clone();
            let kind = self.quickcraft.kind;
            if slots.len() == 1 {
                self.reset_quick_craft();
                drops.extend(self.click(slots[0] as i16, kind as i8, 0, context));
                return;
            }
            if !slots.is_empty() {
                let source = self.carried.clone();
                let mut remaining = source.count;
                for &slot in &slots {
                    let carried = self.carried.clone();
                    if self.can_quick_replace(slot, &carried, true)
                        && self.may_place(slot, &carried, context)
                        && (kind == 2 || carried.count as usize >= slots.len())
                    {
                        let held = self.slots[slot].count;
                        let cap = max_stack(&source).min(self.slot_max(slot, &source));
                        let share = match kind {
                            0 => source.count / slots.len() as i32,
                            1 => 1,
                            _ => max_stack(&source),
                        };
                        let placed = (share + held).min(cap);
                        if placed < held {
                            continue;
                        }
                        remaining -= placed - held;
                        self.set(slot, with_count(&source, placed));
                    }
                }
                self.carried = with_count(&source, remaining);
                if slots.iter().any(|s| self.grid().contains(s)) {
                    self.update_result(context);
                }
            }
            self.reset_quick_craft();
        } else {
            self.reset_quick_craft();
        }
    }

    /// Closing the inventory (`InventoryMenu.removed`): the cursor's stack and the crafting grid go back into the
    /// inventory, and what does not fit is dropped. Returns the stacks dropped.
    pub(crate) fn close(&mut self, context: &Context<'_>) -> Vec<ItemStack> {
        let mut drops = Vec::new();
        for mut stack in self.take_transient_items() {
            // `placeItemBackInInventory`.
            self.add(&mut stack, false);
            if !stack.is_empty() {
                drops.push(stack);
            }
        }
        self.update_result(context);
        drops
    }

    /// The cursor and input grid on removal. A disconnected player drops these instead of returning them to
    /// its inventory (`clearContainer`); the result is a preview and is never an additional item.
    pub(crate) fn take_transient_items(&mut self) -> Vec<ItemStack> {
        let mut back = vec![std::mem::take(&mut self.carried)];
        for slot in self.grid() {
            back.push(std::mem::take(&mut self.slots[slot]));
        }
        if self.is_crafting_result(RESULT) {
            self.slots[RESULT] = ItemStack::empty();
        }
        self.reset_quick_craft();
        back.retain(|stack| !stack.is_empty());
        back
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use harpas_core::item::items;
    use serde_json::json;

    use super::*;
    use crate::inventory::HOTBAR;

    fn stack(name: &str, count: i32) -> ItemStack {
        ItemStack {
            item: items().id(name).unwrap(),
            count,
            components: Arc::default(),
        }
    }

    fn recipes() -> Recipes {
        let tags = HashMap::from([(
            "minecraft:oak_logs".to_owned(),
            vec!["minecraft:oak_log".to_owned()],
        )]);
        let recipes = [
            json!({"type": "minecraft:crafting_shapeless", "ingredients": ["#minecraft:oak_logs"],
            "result": {"id": "minecraft:oak_planks", "count": 4}}),
        ];
        Recipes::from_json(&tags, recipes.iter())
    }

    fn held(inventory: &Inventory, slot: usize) -> (Option<&'static str>, i32) {
        (inventory.slots[slot].name(), inventory.slots[slot].count)
    }

    #[test]
    fn clicks_pick_up_split_put_down_and_swap() {
        let context = Context {
            recipes: None,
            codec: ItemCodec::fallback(),
            creative: false,
        };
        let mut inventory = Inventory::default();
        inventory.slots[9] = stack("minecraft:stone", 10);
        // A right click takes half, rounded up; a left click puts all down, a right click one.
        inventory.click(9, 1, 0, &context);
        assert_eq!((inventory.carried.count, inventory.slots[9].count), (5, 5));
        inventory.click(10, 1, 0, &context);
        assert_eq!((inventory.carried.count, inventory.slots[10].count), (4, 1));
        inventory.click(10, 0, 0, &context);
        assert_eq!(
            (inventory.carried.is_empty(), inventory.slots[10].count),
            (true, 5)
        );
        // Shift-click moves from the main inventory to the hotbar, number keys swap with a hotbar slot.
        inventory.click(10, 0, 1, &context);
        assert_eq!(held(&inventory, HOTBAR), (Some("minecraft:stone"), 5));
        inventory.click(9, 2, 2, &context);
        assert_eq!(held(&inventory, HOTBAR + 2), (Some("minecraft:stone"), 5));
        assert!(inventory.slots[9].is_empty());
        // Armor slots take only armor; a click outside drops what the cursor carries.
        inventory.click((HOTBAR + 2) as i16, 0, 0, &context);
        inventory.click(5, 0, 0, &context);
        assert!(inventory.slots[5].is_empty());
        let drops = inventory.click(-999, 0, 0, &context);
        assert_eq!(drops.len(), 1);
        assert!(inventory.carried.is_empty());
    }

    #[test]
    fn binding_curse_blocks_all_ways_to_take_armor_and_shields_shift_to_offhand() {
        use harpas_core::nbt::{Compound, Nbt};

        let codec = ItemCodec::new(&["minecraft:enchantments".into()])
            .with_enchantments(&["minecraft:binding_curse"]);
        let mut context = Context {
            recipes: None,
            codec: &codec,
            creative: false,
        };
        let mut cursed = stack("minecraft:iron_helmet", 1);
        codec
            .set_saved_component(
                &mut cursed,
                "minecraft:enchantments",
                &Nbt::Compound(Compound::from_iter([("minecraft:binding_curse", 1)])),
            )
            .unwrap();
        let mut menu = Inventory::default();
        menu.slots[5] = cursed.clone();
        for mode in [0, 1, 2, 4] {
            let drops = menu.click(5, 0, mode, &context);
            assert!(drops.iter().all(ItemStack::is_empty));
            assert_eq!(menu.slots[5], cursed);
        }
        menu.carried = stack("minecraft:diamond_helmet", 1);
        menu.click(5, 0, 0, &context);
        assert_eq!(menu.slots[5], cursed);
        menu.carried = ItemStack::empty();
        context.creative = true;
        menu.click(5, 40, 2, &context);
        assert_eq!(menu.slots[OFF_HAND], cursed);
        menu.slots[OFF_HAND] = ItemStack::empty();
        let shield = stack("minecraft:shield", 1);
        menu.slots[9] = shield.clone();
        menu.click(9, 0, 1, &context);
        assert_eq!(menu.slots[OFF_HAND], shield);
    }

    #[test]
    fn swapping_fuel_drops_the_displaced_remainder_after_a_partial_insert() {
        let context = Context {
            recipes: None,
            codec: ItemCodec::fallback(),
            creative: false,
        };
        let mut inventory = Inventory {
            kind: MenuKind::Furnace,
            ..Inventory::default()
        };
        for index in inventory.kind.player_slots() {
            inventory.slots[index] = stack("minecraft:stone", 64);
        }
        let hotbar = inventory.storage_slot(0);
        let coal = inventory.storage_slot(1);
        inventory.slots[hotbar] = stack("minecraft:bucket", 4);
        inventory.slots[coal] = stack("minecraft:coal", 62);
        inventory.slots[FUEL] = stack("minecraft:coal", 5);
        let drops = inventory.click(FUEL as i16, 0, 2, &context);
        assert_eq!(held(&inventory, FUEL), (Some("minecraft:bucket"), 1));
        assert_eq!(held(&inventory, hotbar), (Some("minecraft:bucket"), 3));
        assert_eq!(held(&inventory, coal), (Some("minecraft:coal"), 64));
        assert_eq!(drops, [stack("minecraft:coal", 3)]);
    }

    #[test]
    fn dragging_shares_the_stack_among_the_slots() {
        let context = Context {
            recipes: None,
            codec: ItemCodec::fallback(),
            creative: false,
        };
        let mut inventory = Inventory {
            carried: stack("minecraft:dirt", 10),
            ..Inventory::default()
        };
        // Start a left drag, cross three slots, end it.
        inventory.click(-999, 0, 5, &context);
        for slot in [9, 10, 11] {
            inventory.click(slot, 1, 5, &context);
        }
        inventory.click(-999, 2, 5, &context);
        assert_eq!(
            [9, 10, 11].map(|slot| inventory.slots[slot].count),
            [3, 3, 3]
        );
        assert_eq!(inventory.carried.count, 1);
    }

    #[test]
    fn the_grid_crafts_and_taking_the_result_uses_the_ingredients() {
        let recipes = recipes();
        let context = Context {
            recipes: Some(&recipes),
            codec: ItemCodec::fallback(),
            creative: false,
        };
        let mut inventory = Inventory {
            carried: stack("minecraft:oak_log", 3),
            ..Inventory::default()
        };
        inventory.click(1, 0, 0, &context);
        assert_eq!(held(&inventory, 0), (Some("minecraft:oak_planks"), 4));
        // Taking the result once uses one log.
        inventory.click(0, 0, 0, &context);
        assert_eq!(held(&inventory, 1), (Some("minecraft:oak_log"), 2));
        assert_eq!(
            (inventory.carried.name(), inventory.carried.count),
            (Some("minecraft:oak_planks"), 4)
        );
        inventory.click(36, 0, 0, &context);
        // Shift-clicking the result crafts as many times as it can, into the inventory.
        inventory.click(0, 0, 1, &context);
        assert!(inventory.slots[1].is_empty());
        assert!(inventory.slots[0].is_empty());
        let planks: i32 = (9..45)
            .filter(|&s| inventory.slots[s].name() == Some("minecraft:oak_planks"))
            .map(|s| inventory.slots[s].count)
            .sum();
        assert_eq!(planks, 12);
        // Closing the inventory puts the grid back.
        inventory.slots[2] = stack("minecraft:oak_log", 1);
        inventory.update_result(&context);
        assert!(inventory.close(&context).is_empty());
        assert!(inventory.slots[2].is_empty() && inventory.slots[0].is_empty());
        assert!((9..45).any(|s| inventory.slots[s].name() == Some("minecraft:oak_log")));
    }

    fn table() -> Inventory {
        Inventory {
            kind: MenuKind::CraftingTable,
            slots: vec![ItemStack::empty(); 47],
            remote: vec![crate::inventory::Remote::Known(ItemStack::empty()); 47],
            ..Inventory::default()
        }
    }

    #[test]
    fn table_recipes_use_all_nine_slots_and_shift_craft_into_the_hotbar() {
        let recipes = [json!({"type": "minecraft:crafting_shaped",
            "pattern": ["PPP", " S ", " S "],
            "key": {"P": "minecraft:oak_planks", "S": "minecraft:stick"},
            "result": {"id": "minecraft:wooden_pickaxe"}})];
        let recipes = Recipes::from_json(&HashMap::new(), recipes.iter());
        let context = Context {
            recipes: Some(&recipes),
            codec: ItemCodec::fallback(),
            creative: false,
        };
        let mut menu = table();
        for slot in [1, 2, 3] {
            menu.slots[slot] = stack("minecraft:oak_planks", 2);
        }
        for slot in [5, 8] {
            menu.slots[slot] = stack("minecraft:stick", 2);
        }
        menu.update_result(&context);
        assert_eq!(held(&menu, 0), (Some("minecraft:wooden_pickaxe"), 1));
        assert!(menu.click(0, 0, 1, &context).is_empty());
        assert!(menu.slots[..10].iter().all(ItemStack::is_empty));
        assert_eq!(held(&menu, 45), (Some("minecraft:wooden_pickaxe"), 1));
        assert_eq!(held(&menu, 44), (Some("minecraft:wooden_pickaxe"), 1));
        // The table has no armor slots: shift-clicking a helmet moves it to the main inventory.
        menu.slots[37] = stack("minecraft:iron_helmet", 1);
        menu.click(37, 0, 1, &context);
        assert_eq!(held(&menu, 10), (Some("minecraft:iron_helmet"), 1));
        assert!(menu.slots[37].is_empty());
    }

    #[test]
    fn table_clicks_map_hotbar_and_offhand_and_do_not_expose_the_hidden_slot() {
        let context = Context {
            recipes: None,
            codec: ItemCodec::fallback(),
            creative: false,
        };
        let mut menu = table();
        menu.slots[37] = stack("minecraft:stone", 10);
        menu.click(9, 0, 2, &context);
        assert_eq!(held(&menu, 9), (Some("minecraft:stone"), 10));
        assert!(menu.slots[37].is_empty());
        menu.slots[46] = stack("minecraft:dirt", 4);
        menu.click(9, 40, 2, &context);
        assert_eq!(held(&menu, 46), (Some("minecraft:stone"), 10));
        assert_eq!(held(&menu, 9), (Some("minecraft:dirt"), 4));
        menu.click(46, 0, 0, &context);
        assert!(menu.carried.is_empty());
        menu.click(9, 0, 0, &context);
        menu.click(0, 0, 0, &context);
        assert!(menu.slots[0].is_empty());
        assert_eq!(menu.carried.count, 4);
        // Main inventory and hotbar use the table's boundaries.
        menu.carried = ItemStack::empty();
        menu.slots[36] = stack("minecraft:oak_log", 2);
        menu.click(36, 0, 1, &context);
        assert_eq!(held(&menu, 37), (Some("minecraft:oak_log"), 2));
    }

    #[test]
    fn table_crafting_returns_buckets_and_close_drops_only_what_does_not_fit() {
        let recipes = [json!({"type": "minecraft:crafting_shaped",
            "pattern": ["MMM", "SES", "WWW"],
            "key": {"M": "minecraft:milk_bucket", "S": "minecraft:sugar",
                    "E": "minecraft:egg", "W": "minecraft:wheat"},
            "result": {"id": "minecraft:cake"}})];
        let recipes = Recipes::from_json(&HashMap::new(), recipes.iter());
        let context = Context {
            recipes: Some(&recipes),
            codec: ItemCodec::fallback(),
            creative: false,
        };
        let mut menu = table();
        for slot in [1, 2, 3] {
            menu.slots[slot] = stack("minecraft:milk_bucket", 1);
        }
        for slot in [4, 6] {
            menu.slots[slot] = stack("minecraft:sugar", 1);
        }
        menu.slots[5] = stack("minecraft:egg", 1);
        for slot in [7, 8, 9] {
            menu.slots[slot] = stack("minecraft:wheat", 1);
        }
        menu.update_result(&context);
        assert!(menu.click(0, 0, 0, &context).is_empty());
        assert_eq!(menu.carried.name(), Some("minecraft:cake"));
        for slot in [1, 2, 3] {
            assert_eq!(held(&menu, slot), (Some("minecraft:bucket"), 1));
        }
        // A full inventory drops the cursor and the remaining ingredients, never the displayed result.
        for slot in &mut menu.slots[10..46] {
            *slot = stack("minecraft:stone", 64);
        }
        let drops = menu.close(&context);
        assert_eq!(
            drops
                .iter()
                .map(|s| (s.name(), s.count))
                .collect::<Vec<_>>(),
            [
                (Some("minecraft:cake"), 1),
                (Some("minecraft:bucket"), 1),
                (Some("minecraft:bucket"), 1),
                (Some("minecraft:bucket"), 1)
            ]
        );
        assert!(menu.slots[..10].iter().all(ItemStack::is_empty));
        assert!(menu.carried.is_empty());
        assert!(menu.close(&context).is_empty());
    }
}
