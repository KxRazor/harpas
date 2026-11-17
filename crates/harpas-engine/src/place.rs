//! Using an item on a block, as vanilla's `handleUseItemOn`, `ServerPlayerGameMode.useItemOn` and
//! `BlockItem.place` do it, and the Creative inventory.
//!
//! A block item places its block next to the clicked face, or in the clicked block when that block can be
//! replaced. The block takes the state vanilla gives it for that placement (`getStateForPlacement`), must survive
//! where it is placed and leave the players' boxes clear, and is set with neighbor and shape updates. Whatever
//! happens, the clicked block and the one beside the clicked face are sent back, which corrects the client's
//! prediction, and the click is acknowledged.
//!
//! The placements ported so far are those of blocks without properties, of pillars (their axis follows the
//! clicked face), slabs (a second slab of the same kind makes a double slab), stairs, blocks that face the
//! player (furnaces, carved pumpkins, glazed terracotta, looms and stonecutters), and fences, walls, panes and
//! bars, which connect to their neighbors. Other block items place nothing yet. Not done yet either: clicking a
//! block that reacts to being used, such as a door or a chest, which here places nothing, sneaking, the sound
//! of a placed block, and the message at the top of the world.

use harpas_core::block::block_states;
use harpas_core::block_behavior::{Face, block_behavior};
use harpas_core::item::items;
use harpas_worldgen::BlockGetter;
use harpas_worldgen::shape_update::Direction;

use crate::block::{self, BlockPos};
use crate::fluid::{FluidState, FluidType};
use crate::inventory::{self, ItemStack};
use crate::{Game, neighbor, player};

/// What a click with an item did (`InteractionResult`): only a placement consumes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placed {
    Yes,
    No,
}

fn direction(face: Face) -> Direction {
    match face {
        Face::Down => Direction::Down,
        Face::Up => Direction::Up,
        Face::North => Direction::North,
        Face::South => Direction::South,
        Face::West => Direction::West,
        Face::East => Direction::East,
    }
}

fn step(pos: BlockPos, direction: Direction) -> BlockPos {
    let [x, y, z] = direction.step([pos.x, pos.y, pos.z]);
    BlockPos { x, y, z }
}

/// The horizontal direction a player with `yaw` faces (`Direction.fromYRot`).
fn horizontal_direction(yaw: f32) -> Direction {
    let index = (f64::from(yaw) / 90.0 + 0.5).floor() as i32 & 3;
    [
        Direction::South,
        Direction::West,
        Direction::North,
        Direction::East,
    ][index as usize]
}

/// The state with some properties set to other values.
fn with(state: u32, changes: &[(&str, &str)]) -> u32 {
    let states = block_states();
    let Some((name, properties)) = states.properties(state) else {
        return state;
    };
    let properties = properties.into_iter().map(|(key, value)| {
        changes
            .iter()
            .find(|(k, _)| *k == key)
            .map_or((key, value), |&(k, v)| (k, v))
    });
    states
        .state(name, properties)
        .map_or(state, |(state, _)| state)
}

fn property(state: u32, key: &str) -> Option<&'static str> {
    block_states()
        .properties(state)?
        .1
        .into_iter()
        .find(|(k, _)| *k == key)
        .map(|(_, value)| value)
}

/// The blocks that react to being used, which a click opens, toggles or changes instead of placing against:
/// those with a block entity, and the others vanilla gives a use.
fn reacts_to_use(state: u32) -> bool {
    if block_behavior().has_block_entity(state) {
        return true;
    }
    let Some(name) = block_states().name(state) else {
        return false;
    };
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    (name.ends_with("_door") && name != "iron_door")
        || (name.ends_with("_trapdoor") && name != "iron_trapdoor")
        || name.ends_with("_fence_gate")
        || name.ends_with("_button")
        || name.ends_with("anvil")
        || name.ends_with("cake")
        || matches!(
            name,
            "lever"
                | "repeater"
                | "comparator"
                | "note_block"
                | "crafting_table"
                | "grindstone"
                | "stonecutter"
                | "loom"
                | "cartography_table"
                | "smithing_table"
                | "respawn_anchor"
                | "composter"
                | "cauldron"
                | "water_cauldron"
                | "lava_cauldron"
                | "powder_snow_cauldron"
                | "redstone_wire"
                | "dragon_egg"
                | "pumpkin"
                | "sweet_berry_bush"
                | "cave_vines"
                | "cave_vines_plant"
                | "flower_pot"
                | "tnt"
        )
        || name.starts_with("potted_")
}

/// The block an item places, when it is a block item whose placement is ported: the item and the block have the
/// same name, except the torches, which place a wall torch on a wall.
fn placed_block(stack: &ItemStack) -> Option<u32> {
    let name = stack.name()?;
    if matches!(
        name,
        "minecraft:torch"
            | "minecraft:soul_torch"
            | "minecraft:copper_torch"
            | "minecraft:redstone_torch"
    ) {
        return None;
    }
    block_states().default_state(name)
}

/// Where and against what a block is placed (`BlockPlaceContext`).
struct Context {
    face: Direction,
    /// Where the click hit, in the world.
    location: [f64; 3],
    /// Where the block is placed (`getClickedPos`).
    pos: BlockPos,
    /// The player's horizontal direction.
    facing: Direction,
}

impl Context {
    /// Whether the click was in the upper half of the block the block is placed in.
    fn upper_half(&self) -> bool {
        self.location[1] - f64::from(self.pos.y) > 0.5
    }
}

/// `BlockState.canBeReplaced(BlockPlaceContext)`: a replaceable block, unless the stack is its own item; a slab
/// by a slab of its kind on its open half.
fn can_be_replaced(
    state: u32,
    item: u32,
    face: Direction,
    location: [f64; 3],
    at: BlockPos,
    clicked: bool,
) -> bool {
    let states = block_states();
    let name = states.name(state).unwrap_or_default();
    let same_item = items().id(name) == Some(item);
    if name.ends_with("_slab") && property(state, "type").is_some() {
        let kind = property(state, "type");
        if kind == Some("double") || !same_item {
            return false;
        }
        if clicked {
            let upper = location[1] - f64::from(at.y) > 0.5;
            let horizontal = !matches!(face, Direction::Up | Direction::Down);
            return if kind == Some("bottom") {
                face == Direction::Up || (upper && horizontal)
            } else {
                face == Direction::Down || (!upper && horizontal)
            };
        }
        return true;
    }
    block_behavior().can_be_replaced(state) && !same_item
}

impl Game {
    /// A click with an item on a block (`handleUseItemOn`).
    pub(crate) fn use_item_on(
        &mut self,
        index: usize,
        off_hand: bool,
        pos: BlockPos,
        face: Face,
        cursor: [f32; 3],
        sequence: i32,
    ) {
        let state = &self.players[index];
        let range = state.block_interaction_range() + 1.0;
        if pos.distance_squared(state.eye_position()) < range * range {
            let inside = cursor
                .iter()
                .all(|c| (f64::from(*c) - 0.5).abs() < 1.000_000_1);
            if inside {
                if pos.y <= block::MAX_Y && self.players[index].awaiting_teleport.is_none() {
                    self.use_item_on_block(index, off_hand, pos, face, cursor);
                }
                self.send_block(index, pos);
                self.send_block(index, step(pos, direction(face)));
            } else {
                log::warn!(
                    "Rejecting UseItemOnPacket from {}: Location too far away from hit block {pos:?}.",
                    self.players[index].player.profile.name
                );
            }
        }
        let state = &mut self.players[index];
        if sequence < 0 {
            state.disconnecting = true;
            state.send(player::Output::Disconnect("Network Protocol Error"));
            return;
        }
        state.acknowledge_block_changes = Some(
            state
                .acknowledge_block_changes
                .map_or(sequence, |acknowledged| acknowledged.max(sequence)),
        );
    }

    /// `ServerPlayerGameMode.useItemOn`.
    fn use_item_on_block(
        &mut self,
        index: usize,
        off_hand: bool,
        hit: BlockPos,
        face: Face,
        cursor: [f32; 3],
    ) -> Placed {
        let mode = self.players[index].player.profile.game_mode;
        if mode == player::GameMode::Spectator {
            return Placed::No;
        }
        let clicked = self.block_at(hit);
        if reacts_to_use(clicked) {
            return Placed::No;
        }
        let slot = self.players[index].inventory.hand_slot(off_hand);
        let stack = self.players[index].inventory.slots[slot].clone();
        // Adventure may only place where a component allows it.
        if stack.is_empty() || mode == player::GameMode::Adventure {
            return Placed::No;
        }
        let placed = self.place(index, &stack, hit, face, cursor);
        if placed == Placed::Yes && mode != player::GameMode::Creative {
            let held = &mut self.players[index].inventory.slots[slot];
            held.count -= 1;
            if held.count <= 0 {
                *held = ItemStack::empty();
            }
        }
        placed
    }

    /// `BlockItem.place`.
    fn place(
        &mut self,
        index: usize,
        stack: &ItemStack,
        hit: BlockPos,
        face: Face,
        cursor: [f32; 3],
    ) -> Placed {
        let Some(block) = placed_block(stack) else {
            return Placed::No;
        };
        if !stack.components.added.is_empty() {
            // Components such as `block_state` change the placed block; they are not decoded yet.
            return Placed::No;
        }
        let face = direction(face);
        let location = [
            f64::from(hit.x) + f64::from(cursor[0]),
            f64::from(hit.y) + f64::from(cursor[1]),
            f64::from(hit.z) + f64::from(cursor[2]),
        ];
        let replace_clicked =
            can_be_replaced(self.block_at(hit), stack.item, face, location, hit, true);
        let pos = if replace_clicked {
            hit
        } else {
            step(hit, face)
        };
        let context = Context {
            face,
            location,
            pos,
            facing: horizontal_direction(self.players[index].player.position.yaw),
        };
        let can_place = replace_clicked
            || can_be_replaced(self.block_at(pos), stack.item, face, location, pos, false);
        if !can_place {
            return Placed::No;
        }
        let Some(state) = self.placement_state(block, &context) else {
            return Placed::No;
        };
        let Some(state) = self.settle(state, pos) else {
            // It cannot survive there.
            return Placed::No;
        };
        if !self.unobstructed(state, pos) {
            return Placed::No;
        }
        let Some(world) = &mut self.world else {
            return Placed::No;
        };
        if !world.set_block(pos, state, neighbor::UPDATE_ALL, neighbor::UPDATE_LIMIT) {
            return Placed::No;
        }
        Placed::Yes
    }

    /// `Block.getStateForPlacement` for the blocks ported so far; none for the others.
    fn placement_state(&self, block: u32, context: &Context) -> Option<u32> {
        let states = block_states();
        let name = states.name(block)?;
        let short = name.strip_prefix("minecraft:").unwrap_or(name);
        let (_, properties) = states.properties(block)?;
        let keys: Vec<&str> = properties.iter().map(|(key, _)| *key).collect();
        let water = || {
            if FluidState::of(self.block_at(context.pos)).kind() == Some(FluidType::Water) {
                "true"
            } else {
                "false"
            }
        };
        let half = || {
            let bottom = context.face != Direction::Down
                && (context.face == Direction::Up || !context.upper_half());
            if bottom { "bottom" } else { "top" }
        };
        if keys.is_empty() {
            return Some(block);
        }
        if keys == ["axis"] {
            let axis = match context.face {
                Direction::West | Direction::East => "x",
                Direction::Down | Direction::Up => "y",
                Direction::North | Direction::South => "z",
            };
            return Some(with(block, &[("axis", axis)]));
        }
        if short.ends_with("_slab") && keys == ["type", "waterlogged"] {
            let existing = self.block_at(context.pos);
            if states.name(existing) == Some(name) {
                return Some(with(
                    existing,
                    &[("type", "double"), ("waterlogged", "false")],
                ));
            }
            return Some(with(block, &[("type", half()), ("waterlogged", water())]));
        }
        if short.ends_with("_stairs") && keys == ["facing", "half", "shape", "waterlogged"] {
            // The shape follows from the stairs around it once placed, as `settle` asks of every block.
            return Some(with(
                block,
                &[
                    ("facing", context.facing.name()),
                    ("half", half()),
                    ("waterlogged", water()),
                ],
            ));
        }
        let faces_player = matches!(
            short,
            "furnace"
                | "blast_furnace"
                | "smoker"
                | "carved_pumpkin"
                | "jack_o_lantern"
                | "loom"
                | "stonecutter"
        ) || short.ends_with("_glazed_terracotta");
        if faces_player && keys.first() == Some(&"facing") {
            return Some(with(block, &[("facing", context.facing.opposite().name())]));
        }
        let connects = (short.ends_with("_fence")
            || short.ends_with("_wall")
            || short.ends_with("_pane")
            || short == "iron_bars")
            && keys.iter().all(|key| {
                matches!(
                    *key,
                    "north" | "east" | "south" | "west" | "up" | "waterlogged"
                )
            });
        if connects {
            return Some(with(block, &[("waterlogged", water())]));
        }
        None
    }

    /// The state once it has taken its shape against each neighbor, as placement computes it for connecting
    /// blocks and stairs, or none when it cannot survive there (`canSurvive`, with the rules ported for shape
    /// updates).
    fn settle(&self, mut state: u32, pos: BlockPos) -> Option<u32> {
        let world = self.world.as_ref()?;
        let Some(shapes) = world.shape_updates.clone() else {
            return Some(state);
        };
        struct Blocks<'a>(&'a crate::world::World);
        impl BlockGetter for Blocks<'_> {
            fn block_state(&self, x: i32, y: i32, z: i32) -> u32 {
                self.0
                    .block_state(BlockPos { x, y, z })
                    .unwrap_or_else(block::void_air)
            }
        }
        for direction in Direction::ALL {
            let neighbor = self.block_at(step(pos, direction));
            state = shapes.update(
                &Blocks(world),
                state,
                [pos.x, pos.y, pos.z],
                direction,
                neighbor,
            );
            if block_states().is_air(state) {
                return None;
            }
        }
        Some(state)
    }

    /// Whether no player but spectators stands in the block's collision shape at `pos` (`isUnobstructed`).
    fn unobstructed(&self, state: u32, pos: BlockPos) -> bool {
        let boxes = block_behavior().collision_shape(state);
        self.players.iter().all(|player| {
            if player.disconnecting
                || player.player.profile.game_mode == player::GameMode::Spectator
            {
                return true;
            }
            let p = &player.player.position;
            let entity = [
                [p.x - 0.3, p.y, p.z - 0.3],
                [p.x + 0.3, p.y + 1.8, p.z + 0.3],
            ];
            !boxes.iter().any(|b| {
                (0..3).all(|axis| {
                    let origin = [pos.x, pos.y, pos.z][axis];
                    let min = f64::from(origin) + f64::from(b.min[axis]) / 32.0;
                    let max = f64::from(origin) + f64::from(b.max[axis]) / 32.0;
                    min < entity[1][axis] && entity[0][axis] < max
                })
            })
        })
    }

    /// A stack the client puts in a slot in Creative (`handleSetCreativeModeSlot`). A stack the client may not
    /// send disconnects it, as a stack vanilla cannot read; a negative slot drops the stack, which needs item
    /// entities, not ported yet.
    pub(crate) fn set_creative_slot(&mut self, index: usize, slot: i16, stack: ItemStack) {
        let state = &mut self.players[index];
        if !stack.is_valid() {
            state.disconnecting = true;
            state.send(player::Output::Disconnect("Network Protocol Error"));
            return;
        }
        if state.player.profile.game_mode != player::GameMode::Creative {
            return;
        }
        if (1..=45).contains(&slot) {
            let stack = if stack.is_empty() {
                ItemStack::empty()
            } else {
                stack
            };
            state.inventory.set_by_client(slot as usize, stack);
        }
    }

    /// Sends each player the slots of its inventory that changed (`broadcastChanges`, in the player's tick).
    pub(crate) fn send_inventory_changes(&mut self) {
        for state in &mut self.players {
            if state.disconnecting {
                continue;
            }
            for (slot, stack) in state.inventory.changes() {
                let state_id = state.inventory.next_state_id();
                state.send(player::Output::SetSlot {
                    state_id,
                    slot: slot as i16,
                    stack,
                });
            }
        }
    }
}

/// The inventory's contents for a player who joins (`initMenu`, `sendAllDataToRemote`).
pub(crate) fn initial_contents(inventory: &mut inventory::Inventory) -> player::Output {
    let state_id = inventory.next_state_id();
    player::Output::InventoryContent {
        state_id,
        slots: inventory.slots.clone(),
        carried: ItemStack::empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaw_gives_the_horizontal_direction() {
        assert_eq!(horizontal_direction(0.0), Direction::South);
        assert_eq!(horizontal_direction(90.0), Direction::West);
        assert_eq!(horizontal_direction(180.0), Direction::North);
        assert_eq!(horizontal_direction(-90.0), Direction::East);
        assert_eq!(horizontal_direction(44.0), Direction::South);
        assert_eq!(horizontal_direction(46.0), Direction::West);
    }
}
