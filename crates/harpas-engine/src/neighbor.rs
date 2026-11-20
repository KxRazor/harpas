//! Neighbor and shape updates of the blocks next to a changed block, run in vanilla's order (`Level.setBlock`,
//! `updateNeighborsAt`, `BlockStateBase.updateNeighbourShapes` and `CollectingNeighborUpdater`).
//!
//! When a block changes with [`UPDATE_NEIGHBORS`], each of its six neighbors is told (`neighborChanged`), in the
//! order west, east, down, up, north, south; only liquids react so far. Then, unless the shape is known, each
//! neighbor is asked for its new shape (`updateShape`) against the new block, in the order west, east, north,
//! south, down, up. Every update a change adds runs before the next update of the change that caused it, the
//! first added first, as the collecting updater pushes them. A neighbor whose shape becomes air breaks
//! (`Level.destroyBlock`): its particles and sound play, it leaves the fluid it held, and it updates its
//! neighbors. The shape rules of each block are those `harpas-worldgen` ports for features and structures
//! (`ShapeUpdates`); a liquid, or a block that holds water, also schedules its fluid's tick.
//!
//! Not done yet: the blocks other than liquids that react to their neighbors, redstone among them, the indirect
//! shape updates of redstone wire, `onPlace` for blocks other than liquids, `affectNeighborsAfterRemoval`, and
//! drops. Vanilla loads a chunk that an update reaches; here a block next to a chunk that is not loaded keeps its
//! shape, so that it is never broken for missing support the server has not read.

use harpas_core::block::block_states;
use harpas_worldgen::BlockGetter;
use harpas_worldgen::shape_update::{Direction, chest_shape};
use log::error;

use crate::block::{self, BlockPos};
use crate::chest::CHEST;
use crate::world::{ChunkState, World};

/// `Block.UPDATE_NEIGHBORS`: run neighbor updates around the change.
pub const UPDATE_NEIGHBORS: u32 = 1;
/// `Block.UPDATE_CLIENTS`: send the change to the players who have the chunk.
pub const UPDATE_CLIENTS: u32 = 2;
/// `Block.UPDATE_KNOWN_SHAPE`: do not update the shapes of the neighbors.
pub const UPDATE_KNOWN_SHAPE: u32 = 16;
/// `Block.UPDATE_SUPPRESS_DROPS`: a block broken by the change drops nothing.
pub const UPDATE_SUPPRESS_DROPS: u32 = 32;
/// `Block.UPDATE_ALL`.
pub const UPDATE_ALL: u32 = UPDATE_NEIGHBORS | UPDATE_CLIENTS;
/// How deep a change's shape updates may go (`Block.UPDATE_LIMIT`).
pub const UPDATE_LIMIT: u32 = 512;
/// The most updates one change may chain (`max-chained-neighbor-updates`, 1,000,000 by default).
const MAX_CHAINED_UPDATES: usize = 1_000_000;

/// The order in which a block updates the shapes of its neighbors (`BlockBehaviour.UPDATE_SHAPE_ORDER`).
const UPDATE_SHAPE_ORDER: [Direction; 6] = [
    Direction::West,
    Direction::East,
    Direction::North,
    Direction::South,
    Direction::Down,
    Direction::Up,
];

/// Vanilla's world event for a broken block, whose data is the block state (`LevelEvent.PARTICLES_DESTROY_BLOCK`).
pub(crate) const BLOCK_DESTROYED: i32 = 2001;

/// One queued shape update: the block at `pos` updates against its neighbor in `direction`, at
/// `neighbor_pos`, which is now `neighbor_state` (`CollectingNeighborUpdater.ShapeUpdate`).
#[derive(Debug, Clone, Copy)]
struct ShapeUpdate {
    direction: Direction,
    neighbor_state: u32,
    pos: BlockPos,
    neighbor_pos: BlockPos,
    flags: u32,
    recursion: u32,
}

/// The six neighbor updates around a changed block, one at a time (`CollectingNeighborUpdater.MultiNeighborUpdate`).
#[derive(Debug, Clone, Copy)]
struct NeighborUpdates {
    source: BlockPos,
    /// The next direction, in [`UPDATE_ORDER`].
    next: usize,
}

/// One queued update (`CollectingNeighborUpdater.NeighborUpdates`).
#[derive(Debug, Clone, Copy)]
enum Entry {
    Shape(ShapeUpdate),
    Neighbors(NeighborUpdates),
}

/// The order of the neighbor updates around a block (`NeighborUpdater.UPDATE_ORDER`).
const UPDATE_ORDER: [Direction; 6] = [
    Direction::West,
    Direction::East,
    Direction::Down,
    Direction::Up,
    Direction::North,
    Direction::South,
];

/// The queue of a running chain of updates (`CollectingNeighborUpdater`).
#[derive(Debug, Default)]
pub(crate) struct Updater {
    stack: Vec<Entry>,
    added_this_layer: Vec<Entry>,
    count: usize,
}

/// A world event for every player within 64 blocks of the block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LevelEvent {
    pub(crate) event: i32,
    pub(crate) pos: BlockPos,
    pub(crate) data: i32,
}

/// The blocks of a world as shape updates read them.
struct Blocks<'a>(&'a World);

impl BlockGetter for Blocks<'_> {
    fn block_state(&self, x: i32, y: i32, z: i32) -> u32 {
        self.0
            .block_state(BlockPos { x, y, z })
            .unwrap_or_else(block::void_air)
    }
}

fn step(pos: BlockPos, direction: Direction) -> BlockPos {
    let [x, y, z] = direction.step([pos.x, pos.y, pos.z]);
    BlockPos { x, y, z }
}

impl World {
    /// Changes a block as vanilla's `Level.setBlock`: the change is sent to the players who have the chunk with
    /// [`UPDATE_CLIENTS`], and unless `flags` has [`UPDATE_KNOWN_SHAPE`] the neighbors update their shapes,
    /// down to `recursion` levels. Returns whether the block changed.
    pub fn set_block(&mut self, pos: BlockPos, state: u32, flags: u32, recursion: u32) -> bool {
        let Some(old) = self.block_state(pos) else {
            return false;
        };
        if old == state {
            return false;
        }
        let was_chest = block_states().name(old) == Some(CHEST);
        let is_chest = block_states().name(state) == Some(CHEST);
        let contents = if was_chest && !is_chest {
            match self.chest_items(pos) {
                Ok(items) => Some(items),
                Err(error) => {
                    // Keep loot seeds and unsupported stacks in the world until their codecs are available.
                    log::debug!("Keeping chest at {pos:?}: {error}");
                    return false;
                }
            }
        } else {
            None
        };
        let Some(chunk) = self.chunk_mut(pos.chunk()) else {
            return false;
        };
        if block::set(chunk, pos, state).is_none() {
            return false;
        }
        if let Some(items) = contents
            && flags & UPDATE_SUPPRESS_DROPS == 0
        {
            for stack in items {
                self.pop_resource(pos, stack);
            }
        }
        if is_chest
            && !was_chest
            && let Err(error) = self.save_chest(pos, &[])
        {
            log::debug!("Cannot initialize chest at {pos:?}: {error}");
        }
        if block::changes_light(old, state) {
            self.relight_around(pos);
        }
        // `onPlace`, as the chunk sets the block.
        self.liquid_changed(pos, state);
        if flags & UPDATE_CLIENTS != 0 {
            self.block_changed(pos);
        }
        if flags & UPDATE_NEIGHBORS != 0 {
            // `updateNeighborsAt`.
            self.add_and_run(
                pos,
                Entry::Neighbors(NeighborUpdates {
                    source: pos,
                    next: 0,
                }),
            );
        }
        if flags & UPDATE_KNOWN_SHAPE == 0 && recursion > 0 {
            let flags = flags & !(UPDATE_NEIGHBORS | UPDATE_SUPPRESS_DROPS);
            for direction in UPDATE_SHAPE_ORDER {
                let target = step(pos, direction);
                self.add_and_run(
                    target,
                    Entry::Shape(ShapeUpdate {
                        direction: direction.opposite(),
                        neighbor_state: state,
                        pos: target,
                        neighbor_pos: pos,
                        flags,
                        recursion: recursion - 1,
                    }),
                );
            }
        }
        true
    }

    /// Breaks a block as vanilla's `Level.destroyBlock`: its particles and sound play, it drops its loot unless
    /// `drop` is false, and it leaves the fluid it held. Returns whether a block was broken.
    pub(crate) fn destroy_block(&mut self, pos: BlockPos, drop: bool, recursion: u32) -> bool {
        let Some(state) = self.block_state(pos) else {
            return false;
        };
        if block_states().is_air(state) {
            return false;
        }
        if block_states().name(state) == Some(CHEST) && self.chest_items(pos).is_err() {
            return false;
        }
        let fire = matches!(
            block_states().properties(state),
            Some(("minecraft:fire" | "minecraft:soul_fire", _))
        );
        if !fire {
            self.level_events.push(LevelEvent {
                event: BLOCK_DESTROYED,
                pos,
                data: state as i32,
            });
        }
        if drop {
            self.drop_resources(state, pos, &crate::inventory::ItemStack::empty());
        }
        self.set_block(pos, block::removed(state), UPDATE_ALL, recursion)
    }

    /// Takes the world events played since the last call, in order.
    pub(crate) fn take_level_events(&mut self) -> Vec<LevelEvent> {
        std::mem::take(&mut self.level_events)
    }

    /// `CollectingNeighborUpdater.addAndRun`.
    fn add_and_run(&mut self, pos: BlockPos, entry: Entry) {
        let running = self.updater.count > 0;
        let too_deep = self.updater.count >= MAX_CHAINED_UPDATES;
        self.updater.count += 1;
        if !too_deep {
            if running {
                self.updater.added_this_layer.push(entry);
            } else {
                self.updater.stack.push(entry);
            }
        } else if self.updater.count - 1 == MAX_CHAINED_UPDATES {
            let BlockPos { x, y, z } = pos;
            error!(
                "Too many chained neighbor updates. Skipping the rest. First skipped position: {x}, {y}, {z}"
            );
        }
        if !running {
            self.run_updates();
        }
    }

    /// `CollectingNeighborUpdater.runUpdates`: the entry on top runs one update at a time until it has added
    /// others, which run before its next one, the first added first.
    fn run_updates(&mut self) {
        while !self.updater.stack.is_empty() || !self.updater.added_this_layer.is_empty() {
            let added = std::mem::take(&mut self.updater.added_this_layer);
            self.updater.stack.extend(added.into_iter().rev());
            while self.updater.added_this_layer.is_empty() {
                let Some(top) = self.updater.stack.len().checked_sub(1) else {
                    break;
                };
                let more = match self.updater.stack[top] {
                    Entry::Shape(update) => {
                        self.execute_shape_update(update);
                        false
                    }
                    Entry::Neighbors(mut updates) => {
                        let target = step(updates.source, UPDATE_ORDER[updates.next]);
                        updates.next += 1;
                        self.updater.stack[top] = Entry::Neighbors(updates);
                        self.neighbor_changed(target);
                        updates.next < UPDATE_ORDER.len()
                    }
                };
                if !more {
                    // Entries added meanwhile wait in the layer, so the top is still this entry.
                    self.updater.stack.remove(top);
                    break;
                }
            }
        }
        self.updater.stack.clear();
        self.updater.added_this_layer.clear();
        self.updater.count = 0;
    }

    /// `NeighborUpdater.executeUpdate`: the block's `neighborChanged`, which only liquids react to so far.
    fn neighbor_changed(&mut self, pos: BlockPos) {
        if !self.neighborhood_loaded(pos) {
            return;
        }
        if let Some(state) = self.block_state(pos) {
            self.liquid_changed(pos, state);
        }
    }

    /// `NeighborUpdater.executeShapeUpdate` and `Block.updateOrDestroy`.
    fn execute_shape_update(&mut self, update: ShapeUpdate) {
        let Some(state) = self.block_state(update.pos) else {
            return;
        };
        let is_chest = block_states().name(state) == Some(CHEST);
        if !is_chest && !self.neighborhood_loaded(update.pos) {
            return;
        }
        self.fluid_shape_update(update.pos, state, update.neighbor_state);
        let new = if is_chest {
            // The chest rule only reads this neighbor, even at a chunk edge.
            chest_shape(state, update.direction, update.neighbor_state)
        } else {
            let Some(shapes) = self.shape_updates.clone() else {
                return;
            };
            let BlockPos { x, y, z } = update.pos;
            shapes.update(
                &Blocks(self),
                state,
                [x, y, z],
                update.direction,
                update.neighbor_state,
            )
        };
        debug_assert_eq!(step(update.pos, update.direction), update.neighbor_pos);
        if new == state {
            return;
        }
        if block_states().is_air(new) {
            self.destroy_block(
                update.pos,
                update.flags & UPDATE_SUPPRESS_DROPS == 0,
                update.recursion,
            );
        } else {
            self.set_block(
                update.pos,
                new,
                update.flags & !UPDATE_SUPPRESS_DROPS,
                update.recursion,
            );
        }
    }

    /// Whether the chunks a shape update of the block at `pos` reads, its own and those within a block of it,
    /// are loaded, or known to be missing from a world that does not generate them.
    fn neighborhood_loaded(&self, pos: BlockPos) -> bool {
        let chunks = |n: i32| [(n - 1) >> 4, (n + 1) >> 4];
        let [min_x, max_x] = chunks(pos.x);
        let [min_z, max_z] = chunks(pos.z);
        (min_x..=max_x).all(|x| {
            (min_z..=max_z).all(|z| {
                matches!(
                    self.chunk(crate::world::ChunkPos { x, z }),
                    Some(ChunkState::Loaded(_) | ChunkState::Missing)
                )
            })
        })
    }
}
