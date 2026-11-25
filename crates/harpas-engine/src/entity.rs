//! The entities of a world other than players, stored as [docs/engine-core.md](../../../docs/engine-core.md#5-storage-shaped-like-vanilla-not-a-general-purpose-ecs)
//! describes: generational slots that never move, the hot data in arrays indexed by slot, and one ordered list,
//! in the order entities were added, that drives the entity tick.
//!
//! Item entities are the first type (`ItemEntity`), ported one to one: gravity, collisions with blocks
//! (`Entity.move` and `collide`), the friction and speed of the block below, the bounce of slime blocks,
//! floating in water and lava, burning in lava, being pushed out of a block they are stuck in, merging with the
//! stacks around them, the pickup delay and despawning after 6,000 ticks.
//!
//! Item entities are saved by the world with their current chunk. Players in the same list, the push of
//! flowing water, blocks that slow entities inside them, beds' bounce and fire-resistant items follow later.

use std::collections::HashSet;
use std::sync::Arc;

use harpas_core::block::block_states;
use harpas_core::block_behavior::block_behavior;
use harpas_core::nbt::{Compound, Nbt};
use harpas_core::random::RandomSource;

use crate::block::{self, BlockPos};
use crate::fluid::FluidState;
use crate::inventory::ItemStack;
use crate::player::PigVariant;
use crate::world::{ChunkPos, ChunkState, World};

/// An entity's handle: its slot and the slot's generation, which changes when the slot is reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntityHandle {
    index: u32,
    generation: u32,
}

/// The width and height of an item entity (`EntityType.ITEM`).
const ITEM_SIZE: f64 = 0.25;
/// The width and height of an adult pig.
const PIG_SIZE: f64 = 0.9_f32 as f64;
/// The length of a living entity's death animation before removal.
const DEATH_TICKS: u8 = 20;
/// How many ticks an item lasts on the ground (`ItemEntity.LIFETIME`).
const LIFETIME: i32 = 6000;
/// A pickup delay that never ends (`INFINITE_PICKUP_DELAY`).
const INFINITE_PICKUP_DELAY: i32 = 32767;
/// An age that never ends (`INFINITE_LIFETIME`).
const INFINITE_LIFETIME: i32 = -32768;

/// What an item entity holds besides the hot data (`ItemEntity`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ItemEntity {
    pub(crate) stack: ItemStack,
    pub(crate) age: i32,
    pub(crate) pickup_delay: i32,
    pub(crate) health: f32,
    /// Unrelated saved entity fields, kept with the item when it crosses chunks.
    pub(crate) rest: Arc<Compound>,
}

/// The saved state of a living adult pig.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Pig {
    /// Yaw and pitch, in degrees.
    pub(crate) rotation: [f32; 2],
    pub(crate) health: f32,
    pub(crate) hurt_cooldown: u8,
    pub(crate) last_damage: f32,
    death_time: u8,
    pub(crate) fall_distance: f64,
    pub(crate) variant: PigVariant,
    pub(crate) nbt: Arc<Compound>,
}

impl Pig {
    pub(crate) fn new(
        rotation: [f32; 2],
        health: f32,
        fall_distance: f64,
        variant: PigVariant,
        nbt: Arc<Compound>,
    ) -> Self {
        Self {
            rotation,
            health,
            hurt_cooldown: 0,
            last_damage: 0.0,
            death_time: 0,
            fall_distance,
            variant,
            nbt,
        }
    }

    pub(crate) fn no_ai(&self) -> bool {
        self.nbt.get("NoAI") == Some(&Nbt::Byte(1))
    }
}

/// The type-specific part of an entity.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Kind {
    Item(ItemEntity),
    Pig(Pig),
}

/// A removal keeps its network identity and viewers even after its storage slot is reused.
#[derive(Debug)]
pub(crate) struct RemovedEntity {
    pub(crate) network_id: i32,
    pub(crate) viewers: Vec<i32>,
}

/// The entities of a world.
#[derive(Debug, Default)]
pub(crate) struct Entities {
    generations: Vec<u32>,
    free: Vec<u32>,
    /// The handles in tick order (`EntityTickList`), the order they were added in.
    pub(crate) order: Vec<EntityHandle>,
    // Hot data, by slot.
    pub(crate) network_id: Vec<i32>,
    pub(crate) uuid: Vec<u128>,
    pub(crate) position: Vec<[f64; 3]>,
    /// The position at the start of the entity's last tick (`xo`, `yo`, `zo`).
    pub(crate) old_position: Vec<[f64; 3]>,
    pub(crate) velocity: Vec<[f64; 3]>,
    pub(crate) on_ground: Vec<bool>,
    /// Whether the entity's movement changed outside its own physics, so that viewers are sent it (`hasImpulse`).
    pub(crate) impulse: Vec<bool>,
    pub(crate) tick_count: Vec<i32>,
    /// The block the entity stands on, when it stands on one (`mainSupportingBlockPos`).
    supporting: Vec<Option<BlockPos>>,
    on_ground_no_blocks: Vec<bool>,
    pub(crate) kind: Vec<Option<Kind>>,
    /// What the players who see the entity were last sent of it.
    pub(crate) server: Vec<crate::tracking::ServerEntity>,
    /// Entities removed outside the entity tick, whose viewers are still to be told.
    pub(crate) removed: Vec<RemovedEntity>,
    pub(crate) changed_chunks: HashSet<ChunkPos>,
}

/// A box in world coordinates: its lower and upper corners.
type Aabb = [[f64; 3]; 2];

impl Entities {
    /// Adds an entity at the end of the tick order.
    pub(crate) fn add(
        &mut self,
        network_id: i32,
        uuid: u128,
        position: [f64; 3],
        velocity: [f64; 3],
        kind: Kind,
    ) -> EntityHandle {
        let index = match self.free.pop() {
            Some(index) => index,
            None => {
                self.generations.push(0);
                self.network_id.push(0);
                self.uuid.push(0);
                self.position.push([0.0; 3]);
                self.old_position.push([0.0; 3]);
                self.velocity.push([0.0; 3]);
                self.on_ground.push(false);
                self.impulse.push(false);
                self.tick_count.push(0);
                self.supporting.push(None);
                self.on_ground_no_blocks.push(false);
                self.kind.push(None);
                self.server.push(crate::tracking::ServerEntity::default());
                (self.generations.len() - 1) as u32
            }
        };
        let i = index as usize;
        self.network_id[i] = network_id;
        self.uuid[i] = uuid;
        self.position[i] = position;
        self.old_position[i] = position;
        self.velocity[i] = velocity;
        self.on_ground[i] = false;
        self.impulse[i] = false;
        self.tick_count[i] = 0;
        self.supporting[i] = None;
        self.on_ground_no_blocks[i] = false;
        self.kind[i] = Some(kind);
        self.server[i] = crate::tracking::ServerEntity::new(position, velocity, false);
        let handle = EntityHandle {
            index,
            generation: self.generations[i],
        };
        self.order.push(handle);
        handle
    }

    /// Removes an entity (`discard`); its slot is reused with another generation.
    pub(crate) fn remove(&mut self, handle: EntityHandle) {
        if !self.contains(handle) {
            return;
        }
        let i = handle.index as usize;
        self.mark_changed(handle);
        self.removed.push(RemovedEntity {
            network_id: self.network_id[i],
            viewers: self.server[i].seen_by().collect(),
        });
        self.kind[i] = None;
        self.generations[i] = self.generations[i].wrapping_add(1);
        self.free.push(handle.index);
        self.order.retain(|h| *h != handle);
    }

    pub(crate) fn mark_changed(&mut self, handle: EntityHandle) {
        let [x, _, z] = self.position[self.slot(handle)];
        self.changed_chunks.insert(ChunkPos::at(x, z));
    }

    /// The players who see an entity, by entity ID.
    pub(crate) fn seen_by(&self, handle: EntityHandle) -> Vec<i32> {
        self.server[handle.index as usize].seen_by().collect()
    }

    /// Replaces an item entity's stack, which its viewers are sent (`setItem`).
    pub(crate) fn set_stack(&mut self, handle: EntityHandle, stack: ItemStack) {
        if let Some(item) = self.item_mut(handle) {
            item.stack = stack;
            self.server[handle.index as usize].dirty = true;
            self.mark_changed(handle);
        }
    }

    pub(crate) fn contains(&self, handle: EntityHandle) -> bool {
        let i = handle.index as usize;
        self.generations.get(i) == Some(&handle.generation) && self.kind[i].is_some()
    }

    pub(crate) fn slot(&self, handle: EntityHandle) -> usize {
        handle.index as usize
    }

    pub(crate) fn item(&self, handle: EntityHandle) -> Option<&ItemEntity> {
        match self.kind.get(handle.index as usize)?.as_ref()? {
            Kind::Item(item) => Some(item),
            Kind::Pig(_) => None,
        }
    }

    fn item_mut(&mut self, handle: EntityHandle) -> Option<&mut ItemEntity> {
        match self.kind.get_mut(handle.index as usize)?.as_mut()? {
            Kind::Item(item) => Some(item),
            Kind::Pig(_) => None,
        }
    }

    fn item_box(&self, i: usize) -> Aabb {
        self.bounds(i, ITEM_SIZE)
    }

    pub(crate) fn pig_target(&self, entity_id: i32) -> Option<EntityHandle> {
        self.order.iter().copied().find(|&handle| {
            let slot = self.slot(handle);
            self.network_id[slot] == entity_id
                && matches!(&self.kind[slot], Some(Kind::Pig(pig)) if pig.health > 0.0)
        })
    }

    pub(crate) fn pig_box(&self, i: usize) -> Aabb {
        self.bounds(i, PIG_SIZE)
    }

    /// The box of an entity whose width and height are equal (`getBoundingBox`).
    fn bounds(&self, i: usize, size: f64) -> Aabb {
        let [x, y, z] = self.position[i];
        let half = size / 2.0;
        [[x - half, y, z - half], [x + half, y + size, z + half]]
    }
}

impl ItemEntity {
    /// A new item entity's state (`new ItemEntity`): a pickup delay of none and a full health.
    pub(crate) fn new(stack: ItemStack) -> Self {
        Self {
            stack,
            age: 0,
            pickup_delay: 0,
            health: 5.0,
            rest: Arc::default(),
        }
    }

    /// `isMergable`.
    fn mergeable(&self) -> bool {
        self.pickup_delay != INFINITE_PICKUP_DELAY
            && self.age != INFINITE_LIFETIME
            && self.age < LIFETIME
            && self
                .stack
                .max_stack_size()
                .is_some_and(|max| self.stack.count < max)
    }
}

fn floor(n: f64) -> i32 {
    n.floor() as i32
}

/// The friction of a block (`Block.getFriction`): ice and its kinds and slime blocks are slippery.
fn friction(state: u32) -> f32 {
    match block_states().name(state) {
        Some("minecraft:ice" | "minecraft:packed_ice" | "minecraft:frosted_ice") => 0.98,
        Some("minecraft:blue_ice") => 0.989,
        Some("minecraft:slime_block") => 0.8,
        _ => 0.6,
    }
}

/// The speed factor of a block (`Block.getSpeedFactor`): soul sand and honey slow what moves on them.
fn speed_factor(state: u32) -> f32 {
    match block_states().name(state) {
        Some("minecraft:soul_sand" | "minecraft:honey_block") => 0.4,
        _ => 1.0,
    }
}

impl World {
    fn state(&self, pos: BlockPos) -> u32 {
        self.block_state(pos).unwrap_or_else(block::void_air)
    }

    /// The collision boxes of the blocks a box reaches, in world coordinates (`collectColliders`). Blocks of
    /// chunks not loaded have none.
    fn block_boxes(&self, area: Aabb) -> Vec<Aabb> {
        let mut boxes = Vec::new();
        let behavior = block_behavior();
        for x in floor(area[0][0] - 1.0e-7) - 1..=floor(area[1][0] + 1.0e-7) + 1 {
            for z in floor(area[0][2] - 1.0e-7) - 1..=floor(area[1][2] + 1.0e-7) + 1 {
                for y in floor(area[0][1] - 1.0e-7) - 1..=floor(area[1][1] + 1.0e-7) + 1 {
                    let Some(state) = self.block_state(BlockPos { x, y, z }) else {
                        continue;
                    };
                    for b in behavior.collision_shape(state) {
                        let at =
                            |axis: usize, n: i8| f64::from([x, y, z][axis]) + f64::from(n) / 32.0;
                        let shape = [
                            [at(0, b.min[0]), at(1, b.min[1]), at(2, b.min[2])],
                            [at(0, b.max[0]), at(1, b.max[1]), at(2, b.max[2])],
                        ];
                        let touches =
                            (0..3).all(|a| shape[0][a] < area[1][a] && area[0][a] < shape[1][a]);
                        if touches {
                            boxes.push(shape);
                        }
                    }
                }
            }
        }
        boxes
    }

    /// `Entity.collide` for an entity that cannot step up: the movement along Y first, then the larger of X
    /// and Z, each cut where a block's box stops it (`Shapes.collide`).
    fn collide(&self, entity: Aabb, movement: [f64; 3]) -> [f64; 3] {
        if movement == [0.0; 3] {
            return movement;
        }
        let swept = [
            std::array::from_fn(|a| entity[0][a] + movement[a].min(0.0)),
            std::array::from_fn(|a| entity[1][a] + movement[a].max(0.0)),
        ];
        let boxes = self.block_boxes(swept);
        if boxes.is_empty() {
            return movement;
        }
        let order = if movement[0].abs() < movement[2].abs() {
            [1, 2, 0]
        } else {
            [1, 0, 2]
        };
        let mut moved = [0.0; 3];
        for axis in order {
            let mut d = movement[axis];
            if d == 0.0 {
                continue;
            }
            let current: Aabb = [
                std::array::from_fn(|a| entity[0][a] + moved[a]),
                std::array::from_fn(|a| entity[1][a] + moved[a]),
            ];
            for b in &boxes {
                let overlaps = (0..3)
                    .filter(|&a| a != axis)
                    .all(|a| current[1][a] - 1.0e-7 > b[0][a] && current[0][a] + 1.0e-7 < b[1][a]);
                if !overlaps {
                    continue;
                }
                if d > 0.0 && b[0][axis] >= current[1][axis] - 1.0e-7 {
                    d = d.min(b[0][axis] - current[1][axis]);
                } else if d < 0.0 && b[1][axis] <= current[0][axis] + 1.0e-7 {
                    d = d.max(b[1][axis] - current[0][axis]);
                }
                if d.abs() < 1.0e-7 {
                    d = 0.0;
                    break;
                }
            }
            moved[axis] = d;
        }
        moved
    }

    /// The block below an entity that touches it, nearest its position (`Level.findSupportingBlock`).
    fn supporting_block(&self, entity: Aabb, position: [f64; 3]) -> Option<BlockPos> {
        let behavior = block_behavior();
        let mut best: Option<(f64, BlockPos)> = None;
        for x in floor(entity[0][0]) - 1..=floor(entity[1][0]) + 1 {
            for z in floor(entity[0][2]) - 1..=floor(entity[1][2]) + 1 {
                for y in floor(entity[0][1]) - 1..=floor(entity[1][1]) + 1 {
                    let pos = BlockPos { x, y, z };
                    let Some(state) = self.block_state(pos) else {
                        continue;
                    };
                    let touches = behavior.collision_shape(state).iter().any(|b| {
                        (0..3).all(|a| {
                            let origin = f64::from([x, y, z][a]);
                            origin + f64::from(b.min[a]) / 32.0 < entity[1][a]
                                && entity[0][a] < origin + f64::from(b.max[a]) / 32.0
                        })
                    });
                    if !touches {
                        continue;
                    }
                    let center = [f64::from(x) + 0.5, f64::from(y) + 0.5, f64::from(z) + 0.5];
                    let distance: f64 = (0..3).map(|a| (center[a] - position[a]).powi(2)).sum();
                    let closer = match best {
                        None => true,
                        Some((d, p)) => distance < d || (distance == d && pos < p),
                    };
                    if closer {
                        best = Some((distance, pos));
                    }
                }
            }
        }
        best.map(|(_, pos)| pos)
    }

    /// The block whose friction and speed affect an entity (`getBlockPosBelowThatAffectsMyMovement`).
    fn block_below(&self, i: usize) -> BlockPos {
        let entities = &self.entities;
        let [x, y, z] = entities.position[i];
        let offset = 0.500_001_f32;
        match entities.supporting[i] {
            Some(pos) => {
                let name = block_states().name(self.state(pos)).unwrap_or_default();
                let fence = name.ends_with("_fence")
                    || name.ends_with("_wall")
                    || name.ends_with("_fence_gate");
                if fence {
                    pos
                } else {
                    BlockPos {
                        y: floor(y - f64::from(offset)),
                        ..pos
                    }
                }
            }
            None => BlockPos {
                x: floor(x),
                y: floor(y - f64::from(offset)),
                z: floor(z),
            },
        }
    }

    /// The block collision and support changes of `Entity.move`.
    fn move_entity(&mut self, i: usize, movement: [f64; 3], size: f64) {
        let entity = self.entities.bounds(i, size);
        let collided = self.collide(entity, movement);
        let length: f64 = collided.iter().map(|d| d * d).sum();
        let wanted: f64 = movement.iter().map(|d| d * d).sum();
        if length > 1.0e-7 || wanted - length < 1.0e-7 {
            for (axis, d) in collided.iter().enumerate() {
                self.entities.position[i][axis] += d;
            }
        }
        let equal = |a: f64, b: f64| (b - a).abs() < 1.0e-5;
        let x_collision = !equal(movement[0], collided[0]);
        let z_collision = !equal(movement[2], collided[2]);
        let vertical = movement[1] != collided[1];
        let on_ground = vertical && movement[1] < 0.0;
        if movement[1].abs() > 0.0 {
            self.entities.on_ground[i] = on_ground;
            // `checkSupportingBlock`.
            if on_ground {
                let entity = self.entities.bounds(i, size);
                let below = [
                    [entity[0][0], entity[0][1] - 1.0e-6, entity[0][2]],
                    [entity[1][0], entity[0][1], entity[1][2]],
                ];
                let position = self.entities.position[i];
                let mut found = self.supporting_block(below, position);
                if found.is_none() && !self.entities.on_ground_no_blocks[i] {
                    let back = [
                        [
                            below[0][0] - collided[0],
                            below[0][1],
                            below[0][2] - collided[2],
                        ],
                        [
                            below[1][0] - collided[0],
                            below[1][1],
                            below[1][2] - collided[2],
                        ],
                    ];
                    found = self.supporting_block(back, position);
                }
                self.entities.supporting[i] = found;
                self.entities.on_ground_no_blocks[i] = found.is_none();
            } else {
                self.entities.on_ground_no_blocks[i] = false;
                self.entities.supporting[i] = None;
            }
        }
        if x_collision || z_collision {
            let v = &mut self.entities.velocity[i];
            if x_collision {
                v[0] = 0.0;
            }
            if z_collision {
                v[2] = 0.0;
            }
        }
        if vertical {
            // `updateEntityMovementAfterFallOn`: a slime block bounces what falls on it, every other block stops it.
            let [x, y, z] = self.entities.position[i];
            let on = match self.entities.supporting[i] {
                Some(pos) => pos,
                None => BlockPos {
                    x: floor(x),
                    y: floor(y - 0.2),
                    z: floor(z),
                },
            };
            let slime = block_states().name(self.state(on)) == Some("minecraft:slime_block");
            let v = &mut self.entities.velocity[i];
            if slime && v[1] < 0.0 {
                let bounce = if matches!(self.entities.kind[i], Some(Kind::Item(_))) {
                    0.8
                } else {
                    1.0
                };
                v[1] *= -bounce;
            } else {
                v[1] = 0.0;
            }
        }
        // `getBlockSpeedFactor`.
        let [x, y, z] = self.entities.position[i];
        let inside = self.state(BlockPos {
            x: floor(x),
            y: floor(y),
            z: floor(z),
        });
        let mut factor = speed_factor(inside);
        let liquid = matches!(
            block_states().name(inside),
            Some("minecraft:water" | "minecraft:bubble_column")
        );
        if !liquid && factor == 1.0 {
            factor = speed_factor(self.state(self.block_below(i)));
        }
        let v = &mut self.entities.velocity[i];
        v[0] *= f64::from(factor);
        v[2] *= f64::from(factor);
    }

    /// The highest fluid of one kind the given box reaches into above its bottom (`updateFluidHeightAndDoFluidPushing`,
    /// without the push of flowing fluid).
    pub(crate) fn entity_fluid_height(&self, b: Aabb, water: bool) -> f64 {
        let b = [
            [b[0][0] + 0.001, b[0][1] + 0.001, b[0][2] + 0.001],
            [b[1][0] - 0.001, b[1][1] - 0.001, b[1][2] - 0.001],
        ];
        let mut height: f64 = 0.0;
        for x in floor(b[0][0])..(b[1][0].ceil() as i32) {
            for y in floor(b[0][1])..(b[1][1].ceil() as i32) {
                for z in floor(b[0][2])..(b[1][2].ceil() as i32) {
                    let pos = BlockPos { x, y, z };
                    let state = self.state(pos);
                    let fluid = FluidState::of(state);
                    let Some(kind) = fluid.kind() else { continue };
                    if kind.is_water() != water {
                        continue;
                    }
                    let above = FluidState::of(self.state(BlockPos { y: y + 1, ..pos }));
                    let own = if above.kind().is_some_and(|k| k.is_water() == water) {
                        1.0
                    } else {
                        fluid.amount() as f32 / 9.0
                    };
                    let top = f64::from(y) + f64::from(own);
                    if top >= b[0][1] {
                        height = height.max(top - b[0][1]);
                    }
                }
            }
        }
        height
    }

    /// Whether any collision box of the blocks overlaps the box (`Level.noCollision`, negated).
    pub(crate) fn collides(&self, area: Aabb) -> bool {
        !self.block_boxes(area).is_empty()
    }

    /// `Entity.moveTowardsClosestSpace`: an entity stuck in a block is pushed towards the nearest side that is not
    /// a full block.
    fn move_towards_closest_space(&mut self, i: usize, at: [f64; 3]) {
        let pos = BlockPos {
            x: floor(at[0]),
            y: floor(at[1]),
            z: floor(at[2]),
        };
        let fraction = [
            at[0] - f64::from(pos.x),
            at[1] - f64::from(pos.y),
            at[2] - f64::from(pos.z),
        ];
        // North, south, west, east, up: axis, step.
        let sides = [(2, -1), (2, 1), (0, -1), (0, 1), (1, 1)];
        let mut best = (1, 1);
        let mut nearest = f64::MAX;
        for (axis, step) in sides {
            let mut side = [pos.x, pos.y, pos.z];
            side[axis] += step;
            let shape = block_behavior().collision_shape(self.state(BlockPos {
                x: side[0],
                y: side[1],
                z: side[2],
            }));
            let full = matches!(shape, [b] if b.min == [0, 0, 0] && b.max == [32, 32, 32]);
            if !full {
                let distance = if step > 0 {
                    1.0 - fraction[axis]
                } else {
                    fraction[axis]
                };
                if distance < nearest {
                    nearest = distance;
                    best = (axis, step);
                }
            }
        }
        let speed = self.random.next_float() * 0.2 + 0.1;
        let v = &mut self.entities.velocity[i];
        for component in v.iter_mut() {
            *component *= 0.75;
        }
        v[best.0] = f64::from(best.1 as f32 * speed);
    }

    /// `ItemEntity.tick`; returns whether the item is still there.
    fn tick_item(&mut self, handle: EntityHandle) -> bool {
        let i = self.entities.slot(handle);
        let Some(item) = self.entities.item(handle) else {
            return false;
        };
        if item.stack.is_empty() {
            return false;
        }
        // `Entity.baseTick`: lava burns the item, 4 health a tick.
        let in_lava = self.entity_fluid_height(self.entities.item_box(i), false) > 0.0;
        if in_lava {
            let item = self.entities.item_mut(handle).expect("an item");
            item.health -= 4.0;
            if item.health <= 0.0 {
                return false;
            }
        }
        self.entities.tick_count[i] += 1;
        let item = self.entities.item_mut(handle).expect("an item");
        if item.pickup_delay > 0 && item.pickup_delay != INFINITE_PICKUP_DELAY {
            item.pickup_delay -= 1;
        }
        self.entities.old_position[i] = self.entities.position[i];
        let before = self.entities.velocity[i];
        let bounds = self.entities.item_box(i);
        let water = self.entity_fluid_height(bounds, true);
        let lava = self.entity_fluid_height(bounds, false);
        let v = &mut self.entities.velocity[i];
        if water > 0.1 {
            // `setUnderwaterMovement`.
            *v = [
                v[0] * 0.99,
                v[1] + if v[1] < 0.06 { 5.0e-4 } else { 0.0 },
                v[2] * 0.99,
            ];
        } else if lava > 0.1 {
            // `setUnderLavaMovement`.
            *v = [
                v[0] * 0.95,
                v[1] + if v[1] < 0.06 { 5.0e-4 } else { 0.0 },
                v[2] * 0.95,
            ];
        } else {
            v[1] -= 0.04;
        }
        // An item stuck in a block moves through blocks while it is pushed out (`noPhysics`).
        let b = self.entities.item_box(i);
        let deflated = [
            std::array::from_fn(|a| b[0][a] + 1.0e-7),
            std::array::from_fn(|a| b[1][a] - 1.0e-7),
        ];
        let no_physics = self.collides(deflated);
        if no_physics {
            let [x, _, z] = self.entities.position[i];
            self.move_towards_closest_space(i, [x, (b[0][1] + b[1][1]) / 2.0, z]);
        }
        let v = self.entities.velocity[i];
        let id = self.entities.network_id[i];
        let ticks = self.entities.tick_count[i];
        if !self.entities.on_ground[i]
            || v[0] * v[0] + v[2] * v[2] > 1.0e-5
            || (ticks + id) % 4 == 0
        {
            if no_physics {
                for (axis, d) in v.iter().enumerate() {
                    self.entities.position[i][axis] += d;
                }
            } else {
                self.move_entity(i, v, ITEM_SIZE);
            }
            let mut f = 0.98_f32;
            if self.entities.on_ground[i] {
                f = friction(self.state(self.block_below(i))) * 0.98;
            }
            let v = &mut self.entities.velocity[i];
            v[0] *= f64::from(f);
            v[1] *= 0.98;
            v[2] *= f64::from(f);
            if self.entities.on_ground[i] && v[1] < 0.0 {
                v[1] *= -0.5;
            }
        }
        let old = self.entities.old_position[i];
        let now = self.entities.position[i];
        let moved = (0..3).any(|a| floor(old[a]) != floor(now[a]));
        let rate = if moved { 2 } else { 40 };
        if self.entities.tick_count[i] % rate == 0 {
            self.merge_with_neighbours(handle);
            if !self.entities.contains(handle) {
                return true;
            }
        }
        let item = self.entities.item_mut(handle).expect("an item");
        if item.age != INFINITE_LIFETIME {
            item.age += 1;
        }
        let age = item.age;
        let after = self.entities.velocity[i];
        let change: f64 = (0..3).map(|a| (after[a] - before[a]).powi(2)).sum();
        if change > 0.01 {
            self.entities.impulse[i] = true;
        }
        age < LIFETIME
    }

    /// The dry movement of a living adult pig without movement input (`travelInAir`).
    fn tick_pig(&mut self, i: usize) -> bool {
        let Some(Kind::Pig(pig)) = &self.entities.kind[i] else {
            return false;
        };
        if pig.no_ai() {
            return false;
        }
        let gravity = if pig.nbt.get("NoGravity") == Some(&Nbt::Byte(1)) {
            0.0
        } else {
            0.08
        };
        let before = self.entities.position[i];
        let velocity = self.entities.velocity[i];
        let on_ground = self.entities.on_ground[i];
        let drag = if on_ground {
            0.91_f32 * friction(self.state(self.block_below(i)))
        } else {
            0.91_f32
        };
        let [mut x, mut y, mut z] = velocity;
        if x * x + z * z < 9.0e-6 {
            x = 0.0;
            z = 0.0;
        }
        if y.abs() < 0.003 {
            y = 0.0;
        }
        self.entities.old_position[i] = before;
        self.entities.tick_count[i] += 1;
        self.entities.velocity[i] = [x, y, z];
        self.move_entity(i, [x, y, z], PIG_SIZE);
        let v = &mut self.entities.velocity[i];
        *v = [
            v[0] * f64::from(drag),
            (v[1] - gravity) * f64::from(0.98_f32),
            v[2] * f64::from(drag),
        ];
        let after = self.entities.position[i];
        let grounded = self.entities.on_ground[i];
        let Some(Kind::Pig(pig)) = &mut self.entities.kind[i] else {
            unreachable!()
        };
        let fall_distance = pig.fall_distance;
        if grounded {
            pig.fall_distance = 0.0;
        } else if after[1] < before[1] {
            pig.fall_distance += f64::from((before[1] - after[1]) as f32);
        }
        before != after
            || velocity != *v
            || on_ground != grounded
            || fall_distance != pig.fall_distance
    }

    /// `ItemEntity.mergeWithNeighbours`: the stacks of the same item within half a block merge into the larger
    /// one.
    fn merge_with_neighbours(&mut self, handle: EntityHandle) {
        if !self
            .entities
            .item(handle)
            .is_some_and(ItemEntity::mergeable)
        {
            return;
        }
        let i = self.entities.slot(handle);
        let b = self.entities.item_box(i);
        let area = [
            [b[0][0] - 0.5, b[0][1], b[0][2] - 0.5],
            [b[1][0] + 0.5, b[1][1], b[1][2] + 0.5],
        ];
        let others: Vec<EntityHandle> = self
            .entities
            .order
            .iter()
            .copied()
            .filter(|&other| {
                other != handle && {
                    let o = self.entities.item_box(self.entities.slot(other));
                    (0..3).all(|a| o[0][a] < area[1][a] && area[0][a] < o[1][a])
                }
            })
            .collect();
        for other in others {
            if !self.entities.item(other).is_some_and(ItemEntity::mergeable) {
                continue;
            }
            self.try_to_merge(handle, other);
            if !self.entities.contains(handle) {
                break;
            }
        }
    }

    /// `ItemEntity.tryToMerge`.
    fn try_to_merge(&mut self, this: EntityHandle, other: EntityHandle) {
        let (Some(a), Some(b)) = (self.entities.item(this), self.entities.item(other)) else {
            return;
        };
        let (a, b) = (a.stack.clone(), b.stack.clone());
        let Some(max) = b.max_stack_size() else {
            return;
        };
        if a.count + b.count > max || a.item != b.item || a.components != b.components {
            return;
        }
        let (into, from) = if b.count < a.count {
            (this, other)
        } else {
            (other, this)
        };
        let from_item = self.entities.item(from).expect("an item").clone();
        let into_item = self.entities.item_mut(into).expect("an item");
        let room = into_item.stack.max_stack_size().unwrap_or(64).min(64) - into_item.stack.count;
        let moved = room.min(from_item.stack.count);
        into_item.stack.count += moved;
        into_item.pickup_delay = into_item.pickup_delay.max(from_item.pickup_delay);
        into_item.age = into_item.age.min(from_item.age);
        let into_slot = self.entities.slot(into);
        self.entities.server[into_slot].dirty = true;
        self.entities.mark_changed(into);
        let left = from_item.stack.count - moved;
        if left <= 0 {
            self.entities.remove(from);
        } else {
            self.entities.item_mut(from).expect("an item").stack.count = left;
            let from_slot = self.entities.slot(from);
            self.entities.server[from_slot].dirty = true;
            self.entities.mark_changed(from);
        }
    }

    /// Runs the tick of every entity whose chunk a player sees, in order, and removes expired entities
    /// (`entityTickList.forEach(tickNonPassenger)`). Returns the entities removed.
    pub(crate) fn tick_entities(&mut self) -> Vec<RemovedEntity> {
        for handle in self.entities.order.clone() {
            if !self.entities.contains(handle) {
                continue;
            }
            let i = self.entities.slot(handle);
            let [x, _, z] = self.entities.position[i];
            let chunk = ChunkPos::at(x, z);
            let ticking = self.wanted_set.contains(&chunk)
                && matches!(self.chunk(chunk), Some(ChunkState::Loaded(_)));
            if !ticking {
                continue;
            }
            let changed = match self.entities.kind[i].as_mut().expect("live entity") {
                Kind::Item(_) => {
                    if !self.tick_item(handle) {
                        self.entities.remove(handle);
                    }
                    true
                }
                Kind::Pig(pig) => {
                    pig.hurt_cooldown = pig.hurt_cooldown.saturating_sub(1);
                    if pig.health <= 0.0 {
                        pig.death_time += 1;
                        if pig.death_time >= DEATH_TICKS {
                            self.entities.remove(handle);
                            continue;
                        }
                    }
                    self.tick_pig(i)
                }
            };
            if changed {
                self.entities.changed_chunks.insert(chunk);
                if self.entities.contains(handle) {
                    let [x, _, z] = self.entities.position[i];
                    self.entities.changed_chunks.insert(ChunkPos::at(x, z));
                }
            }
        }
        std::mem::take(&mut self.entities.removed)
    }

    /// Drops what a broken block of `state` at `pos` leaves, broken with `tool` (`Block.dropResources`): its loot
    /// table's stacks, each popped out of the block. A block without a table drops nothing.
    pub(crate) fn drop_resources(&mut self, state: u32, pos: BlockPos, tool: &ItemStack) {
        let Some(loot) = self.loot.clone() else {
            return;
        };
        let Some(table) = block_states().name(state).and_then(|name| loot.block(name)) else {
            return;
        };
        let stacks = table.drops(
            state,
            tool,
            self.drop_random(table.random_sequence.as_deref()),
        );
        let Ok(stacks) = stacks else { return };
        for stack in stacks {
            self.pop_resource(pos, stack);
        }
    }

    /// Drops the pig table's stacks at the pig's position on a lethal melee hit.
    pub(crate) fn drop_pig_loot(&mut self, position: [f64; 3], tool: &ItemStack) {
        let Some(loot) = self.loot.clone() else {
            return;
        };
        let Some(table) = &loot.pig else {
            return;
        };
        match table.mob_drops(tool, self.drop_random(table.random_sequence.as_deref())) {
            Ok(stacks) => {
                for stack in stacks {
                    if let Some(id) = crate::next_entity_id() {
                        self.add_resource(id, position, stack);
                    }
                }
            }
            Err(error) => log::warn!("Cannot roll pig loot: {error}"),
        }
    }

    fn drop_random(&mut self, name: Option<&str>) -> &mut dyn RandomSource {
        let Some(name) = name else {
            return &mut self.random;
        };
        self.random_sequences
            .entry(name.to_owned())
            .or_insert_with(|| harpas_core::random::RandomSequence::new(self.seed, Some(name)))
            .random()
    }

    /// `Block.popResource`: an item entity at a random point around the block's center, thrown a little up, picked
    /// up after 10 ticks.
    pub(crate) fn pop_resource(&mut self, pos: BlockPos, stack: ItemStack) {
        if stack.is_empty() {
            return;
        }
        let Some(id) = crate::next_entity_id() else {
            return;
        };
        let random = &mut self.random;
        let mut offset = || random.next_double() * 0.5 - 0.25;
        let half = ITEM_SIZE / 2.0;
        let position = [
            f64::from(pos.x) + 0.5 + offset(),
            f64::from(pos.y) + 0.5 + offset() - half,
            f64::from(pos.z) + 0.5 + offset(),
        ];
        self.add_resource(id, position, stack);
    }

    fn add_resource(&mut self, id: i32, position: [f64; 3], stack: ItemStack) {
        let velocity = [
            self.random.next_double() * 0.2 - 0.1,
            0.2,
            self.random.next_double() * 0.2 - 0.1,
        ];
        let mut item = ItemEntity::new(stack);
        item.pickup_delay = 10;
        self.add_item(id, position, velocity, item);
    }

    /// Adds an item entity with a random UUID (`Mth.createInsecureUUID`).
    pub(crate) fn add_item(
        &mut self,
        network_id: i32,
        position: [f64; 3],
        velocity: [f64; 3],
        item: ItemEntity,
    ) -> EntityHandle {
        let high = (self.item_uuids.next_long() & -61441) | 16384;
        let low = (self.item_uuids.next_long() & 0x3FFF_FFFF_FFFF_FFFF) | i64::MIN;
        let uuid = (u128::from(high as u64) << 64) | u128::from(low as u64);
        let handle = self
            .entities
            .add(network_id, uuid, position, velocity, Kind::Item(item));
        self.entities.mark_changed(handle);
        handle
    }
}
