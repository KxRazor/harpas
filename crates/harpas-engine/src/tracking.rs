//! Sending entities other than players to the players who see them (`ChunkMap.TrackedEntity` and
//! `ServerEntity`), and players dropping items (`ServerPlayer.drop`).
//!
//! An item entity is seen by the players within 96 blocks of it horizontally, or their view distance if smaller,
//! who have its chunk. A player who starts to see it is sent it with the position and velocity last sent to the
//! others and its stack; one who stops is told to remove it. Every 20 ticks, or when its movement changed
//! outside its physics or its stack changed, its viewers are sent its new position: a move relative to the
//! position last sent while it stays within 8 blocks of it and on the same ground, and a full position
//! otherwise, its velocity when it changed, and its stack when it changed.

use std::collections::HashSet;

use harpas_core::math::{cos_f32, sin_f32};
use harpas_core::random::RandomSource;

use crate::entity::{ItemEntity, Kind, RemovedEntity};
use crate::inventory::ItemStack;
use crate::world::ChunkPos;
use crate::{Game, player};

/// How far players see item entities: 6 chunks (`EntityType.ITEM`'s `clientTrackingRange`).
const ITEM_RANGE: f64 = 96.0;
/// How far players see pigs (`EntityType.PIG`'s `clientTrackingRange`).
const PIG_RANGE: f64 = 160.0;
/// How often an item entity's viewers are sent its position (`updateInterval`).
const ITEM_UPDATE_INTERVAL: i32 = 20;
/// How often a pig's viewers are sent its position (`updateInterval`).
const PIG_UPDATE_INTERVAL: i32 = 3;

/// What the players who see an entity were last sent of it (`ServerEntity`).
#[derive(Debug, Clone, Default)]
pub(crate) struct ServerEntity {
    tick_count: i32,
    /// The position last sent, which moves are relative to (`positionCodec`'s base).
    base: [f64; 3],
    last_velocity: [f64; 3],
    was_on_ground: bool,
    teleport_delay: i32,
    /// Whether the stack changed since it was last sent (`EntityData.isDirty`).
    pub(crate) dirty: bool,
    /// The players who see it, by entity ID (`seenBy`).
    seen_by: HashSet<i32>,
}

impl ServerEntity {
    /// The players who see the entity, by entity ID.
    pub(crate) fn seen_by(&self) -> impl Iterator<Item = i32> + '_ {
        self.seen_by.iter().copied()
    }

    pub(crate) fn new(position: [f64; 3], velocity: [f64; 3], on_ground: bool) -> Self {
        Self {
            base: position,
            last_velocity: velocity,
            was_on_ground: on_ground,
            ..Self::default()
        }
    }
}

/// A coordinate in 1/4096 of a block (`VecDeltaCodec.encode`).
fn encode(n: f64) -> i64 {
    (n * 4096.0 + 0.5).floor() as i64
}

impl Game {
    /// A player drops a stack, as vanilla's `Player.drop` does when the stack is not thrown at random: from 0.3
    /// below its eyes, thrown where it looks, picked up after 40 ticks. Returns whether it was dropped.
    pub(crate) fn drop_item(&mut self, index: usize, stack: ItemStack) -> bool {
        if stack.is_empty() {
            return false;
        }
        let Some(world) = &mut self.world else {
            return false;
        };
        let Some(id) = crate::next_entity_id() else {
            return false;
        };
        let p = self.players[index].player.position;
        let radians = std::f32::consts::PI / 180.0;
        let (g, h) = (sin_f32(p.pitch * radians), cos_f32(p.pitch * radians));
        let (i, j) = (sin_f32(p.yaw * radians), cos_f32(p.yaw * radians));
        let random = &mut world.random;
        let k = random.next_float() * (std::f32::consts::PI * 2.0);
        let l = 0.02_f32 * random.next_float();
        let y = -g * 0.3 + 0.1 + (random.next_float() - random.next_float()) * 0.1;
        let velocity = [
            f64::from(-i * h * 0.3) + f64::from(k).cos() * f64::from(l),
            f64::from(y),
            f64::from(j * h * 0.3) + f64::from(k).sin() * f64::from(l),
        ];
        let position = [p.x, p.y + 1.62 - f64::from(0.3_f32), p.z];
        let mut item = ItemEntity::new(stack);
        item.pickup_delay = 40;
        world.add_item(id, position, velocity, item);
        true
    }

    /// Drops one item of the selected hotbar slot, or all of it (`ServerPlayer.drop`). The client already took
    /// it out of the slot.
    pub(crate) fn drop_selected(&mut self, index: usize, all: bool) {
        if self.players[index].player.profile.game_mode == player::GameMode::Spectator {
            return;
        }
        let inventory = &mut self.players[index].inventory;
        let slot = inventory.hand_slot(false);
        let held = &mut inventory.slots[slot];
        if held.is_empty() {
            return;
        }
        let count = if all { held.count } else { 1 };
        let dropped = ItemStack {
            count,
            ..held.clone()
        };
        held.count -= count;
        if held.count <= 0 {
            *held = ItemStack::empty();
        }
        let left = held.clone();
        inventory.set_by_client(slot, left);
        self.drop_item(index, dropped);
    }

    /// Each player picks up the items it touches (`Player.aiStep`'s `touch` and `ItemEntity.playerTouch`): those
    /// whose box meets the player's grown by a block sideways and half a block up and down, once their pickup
    /// delay is over, as much as the inventory takes. The players who see the item are shown it go.
    pub(crate) fn pick_up_items(&mut self) {
        let Some(world) = &mut self.world else {
            return;
        };
        for index in 0..self.players.len() {
            let player = &self.players[index];
            if player.disconnecting
                || !player.data_ready
                || player.is_dead()
                || player.player.profile.game_mode == player::GameMode::Spectator
            {
                continue;
            }
            let p = player.player.position;
            let reach = [
                [p.x - 1.3, p.y - 0.5, p.z - 1.3],
                [p.x + 1.3, p.y + 2.3, p.z + 1.3],
            ];
            let collector = player.player.entity_id;
            let creative = player.player.profile.game_mode == player::GameMode::Creative;
            let entities = &mut world.entities;
            for handle in entities.order.clone() {
                if !entities.contains(handle) {
                    continue;
                }
                let i = entities.slot(handle);
                let [x, y, z] = entities.position[i];
                let item_box = [[x - 0.125, y, z - 0.125], [x + 0.125, y + 0.25, z + 0.125]];
                if !(0..3).all(|a| item_box[0][a] < reach[1][a] && reach[0][a] < item_box[1][a]) {
                    continue;
                }
                let Some(item) = entities.item(handle) else {
                    continue;
                };
                if item.pickup_delay != 0 {
                    continue;
                }
                let mut stack = item.stack.clone();
                let count = stack.count;
                if !self.players[index].inventory.add(&mut stack, creative) {
                    continue;
                }
                // `take`, to everyone who sees the item.
                let id = entities.network_id[i];
                for viewer in entities.seen_by(handle) {
                    if let Some(player) = self.players.iter().find(|p| p.player.entity_id == viewer)
                    {
                        player.send(player::Output::PickupItem {
                            collected: id,
                            collector,
                            count,
                        });
                    }
                }
                if stack.is_empty() {
                    entities.remove(handle);
                } else {
                    entities.set_stack(handle, stack);
                }
            }
        }
    }

    /// Updates which players see each entity and sends them its changes (`ChunkMap.tick`).
    pub(crate) fn track_entities(&mut self, removed: &[RemovedEntity]) {
        let Some(world) = &mut self.world else {
            return;
        };
        let entities = &mut world.entities;
        // Entities that left: their viewers remove them.
        for removal in removed {
            let id = removal.network_id;
            for &viewer in &removal.viewers {
                if let Some(player) = self.players.iter().find(|p| p.player.entity_id == viewer) {
                    player.send(player::Output::RemoveEntity(id));
                }
            }
        }
        for handle in entities.order.clone() {
            let i = entities.slot(handle);
            let id = entities.network_id[i];
            let position = entities.position[i];
            let chunk = ChunkPos::at(position[0], position[2]);
            let kind = entities.kind[i].as_ref().expect("live entity");
            let tracking_range = match kind {
                Kind::Item(_) => ITEM_RANGE,
                Kind::Pig(_) => PIG_RANGE,
            };
            // `updatePlayers`.
            for player in &self.players {
                let viewer = player.player.entity_id;
                let range = tracking_range.min(f64::from(player.view_distance) * 16.0);
                let p = &player.player.position;
                let (dx, dz) = (p.x - position[0], p.z - position[2]);
                let visible = !player.disconnecting
                    && dx * dx + dz * dz <= range * range
                    && player.sent_chunks.contains(&chunk);
                let server = &mut entities.server[i];
                if visible && server.seen_by.insert(viewer) {
                    player.send(match kind {
                        Kind::Item(item) => player::Output::SpawnItem {
                            entity_id: id,
                            uuid: entities.uuid[i],
                            position: server.base,
                            velocity: server.last_velocity,
                            stack: item.stack.clone(),
                        },
                        Kind::Pig(pig) => player::Output::SpawnPig {
                            entity_id: id,
                            uuid: entities.uuid[i],
                            position: server.base,
                            velocity: server.last_velocity,
                            rotation: pig.rotation,
                            health: pig.health,
                            variant: pig.variant,
                            no_ai: pig.no_ai(),
                        },
                    });
                } else if !visible && server.seen_by.remove(&viewer) {
                    player.send(player::Output::RemoveEntity(id));
                }
            }
            let (update_interval, rotation) = match kind {
                Kind::Item(_) => (ITEM_UPDATE_INTERVAL, [0.0; 2]),
                Kind::Pig(pig) if !pig.no_ai() => (PIG_UPDATE_INTERVAL, pig.rotation),
                Kind::Pig(_) => continue,
            };
            // `sendChanges`.
            let on_ground = entities.on_ground[i];
            let velocity = entities.velocity[i];
            let impulse = std::mem::take(&mut entities.impulse[i]);
            let server = &mut entities.server[i];
            let mut outputs = Vec::new();
            if server.tick_count % update_interval == 0 || impulse || server.dirty {
                server.teleport_delay += 1;
                let delta: [i64; 3] =
                    std::array::from_fn(|a| encode(position[a]) - encode(server.base[a]));
                let moved: f64 = (0..3).map(|a| (position[a] - server.base[a]).powi(2)).sum();
                let force = moved >= 7.629_394_5e-6 || server.tick_count % 60 == 0;
                let mut send_position = false;
                if server.tick_count > 0 {
                    let overflow = delta.iter().any(|d| *d < -32768 || *d > 32767);
                    if overflow || server.teleport_delay > 400 || server.was_on_ground != on_ground
                    {
                        server.was_on_ground = on_ground;
                        server.teleport_delay = 0;
                        send_position = true;
                        outputs.push(player::Output::EntitySync {
                            entity_id: id,
                            position,
                            velocity,
                            rotation,
                            on_ground,
                        });
                    } else if force {
                        send_position = true;
                        outputs.push(player::Output::EntityMove {
                            entity_id: id,
                            delta: delta.map(|d| d as i16),
                            on_ground,
                        });
                    }
                }
                if server.tick_count > 0 {
                    let change: f64 = (0..3)
                        .map(|a| (velocity[a] - server.last_velocity[a]).powi(2))
                        .sum();
                    let still = velocity.iter().all(|v| *v == 0.0);
                    if change > 1.0e-7 || (change > 0.0 && still) {
                        server.last_velocity = velocity;
                        // Sent before the position, as vanilla broadcasts it first.
                        outputs.insert(
                            0,
                            player::Output::EntityVelocity {
                                entity_id: id,
                                velocity,
                            },
                        );
                    }
                }
                if std::mem::take(&mut server.dirty)
                    && let Kind::Item(item) = kind
                {
                    outputs.push(player::Output::ItemChanged {
                        entity_id: id,
                        stack: item.stack.clone(),
                    });
                }
                if send_position {
                    server.base = position;
                }
            }
            server.tick_count += 1;
            for output in outputs {
                for viewer in &server.seen_by {
                    if let Some(player) =
                        self.players.iter().find(|p| p.player.entity_id == *viewer)
                    {
                        player.send(output.clone());
                    }
                }
            }
        }
    }
}
