//! The entities of a world other than players, stored as [docs/engine-core.md](../../../docs/engine-core.md#5-storage-shaped-like-vanilla-not-a-general-purpose-ecs)
//! describes: generational slots that never move, the hot data in arrays indexed by slot, and one ordered list,
//! in the order entities were added, that drives the entity tick.
//!
//! Item entities are the first type (`ItemEntity`), ported one to one: gravity, collisions with blocks
//! (`Entity.move` and `collide`), the friction and speed of the block below, the bounce of slime blocks,
//! floating in water and lava, burning in lava, being pushed out of a block they are stuck in, merging with the
//! stacks around them, the pickup delay and despawning after 6,000 ticks.
//!
//! Not done yet: players in the same list, saving entities, the push of flowing water, the blocks that slow
//! entities inside them (cobwebs, berry bushes, powder snow), beds' bounce, fire-resistant items, and picking
//! items up.

use harpas_core::block::block_states;
use harpas_core::block_behavior::block_behavior;
use harpas_core::random::RandomSource;

use crate::block::{self, BlockPos};
use crate::fluid::FluidState;
use crate::inventory::ItemStack;
use crate::world::{ChunkPos, ChunkState, World};

/// An entity's handle: its slot and the slot's generation, which changes when the slot is reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntityHandle {
    index: u32,
    generation: u32,
}

/// The width and height of an item entity (`EntityType.ITEM`).
const ITEM_SIZE: f64 = 0.25;
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
    health: f32,
}

/// The type-specific part of an entity.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Kind {
    Item(ItemEntity),
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
        self.kind[i] = None;
        self.generations[i] = self.generations[i].wrapping_add(1);
        self.free.push(handle.index);
        self.order.retain(|h| *h != handle);
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
        }
    }

    fn item_mut(&mut self, handle: EntityHandle) -> Option<&mut ItemEntity> {
        match self.kind.get_mut(handle.index as usize)?.as_mut()? {
            Kind::Item(item) => Some(item),
        }
    }

    /// An item entity's box (`getBoundingBox`).
    fn item_box(&self, i: usize) -> Aabb {
        let [x, y, z] = self.position[i];
        let half = ITEM_SIZE / 2.0;
        [[x - half, y, z - half], [x + half, y + ITEM_SIZE, z + half]]
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

    /// `Entity.move` for an item entity.
    fn move_entity(&mut self, i: usize, movement: [f64; 3]) {
        let entity = self.entities.item_box(i);
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
                let entity = self.entities.item_box(i);
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
                v[1] = -v[1] * 0.8;
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

    /// The highest fluid of one kind the entity's box reaches into above its bottom (`updateFluidHeightAndDoFluidPushing`,
    /// without the push of flowing fluid).
    fn entity_fluid_height(&self, i: usize, water: bool) -> f64 {
        let b = self.entities.item_box(i);
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
    fn collides(&self, area: Aabb) -> bool {
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
        let in_lava = self.entity_fluid_height(i, false) > 0.0;
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
        let water = self.entity_fluid_height(i, true);
        let lava = self.entity_fluid_height(i, false);
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
                self.move_entity(i, v);
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
        let left = from_item.stack.count - moved;
        if left <= 0 {
            self.entities.remove(from);
        } else {
            self.entities.item_mut(from).expect("an item").stack.count = left;
            let from_slot = self.entities.slot(from);
            self.entities.server[from_slot].dirty = true;
        }
    }

    /// Runs the tick of every entity whose chunk a player sees, in order, and removes the items that are gone
    /// (`entityTickList.forEach(tickNonPassenger)`). Returns the entities removed.
    pub(crate) fn tick_entities(&mut self) -> Vec<EntityHandle> {
        let mut removed = Vec::new();
        for handle in self.entities.order.clone() {
            if !self.entities.contains(handle) {
                removed.push(handle);
                continue;
            }
            let [x, _, z] = self.entities.position[self.entities.slot(handle)];
            let chunk = ChunkPos {
                x: floor(x) >> 4,
                z: floor(z) >> 4,
            };
            let ticking = self.wanted_set.contains(&chunk)
                && matches!(self.chunk(chunk), Some(ChunkState::Loaded(_)));
            if ticking && !self.tick_item(handle) {
                self.entities.remove(handle);
                removed.push(handle);
            }
        }
        removed
    }

    /// Adds an item entity with a random UUID (`Mth.createInsecureUUID`).
    pub(crate) fn add_item(
        &mut self,
        network_id: i32,
        position: [f64; 3],
        velocity: [f64; 3],
        item: ItemEntity,
    ) -> EntityHandle {
        let high = (self.random.next_long() & -61441) | 16384;
        let low = (self.random.next_long() & 0x3FFF_FFFF_FFFF_FFFF) | i64::MIN;
        let uuid = (u128::from(high as u64) << 64) | u128::from(low as u64);
        self.entities
            .add(network_id, uuid, position, velocity, Kind::Item(item))
    }
}
