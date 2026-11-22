//! Player health, landing damage and the Overworld respawn after a fall.

use crate::Game;
use crate::block::BlockPos;
use crate::player::{GameMode, Output, PlayerState, Position};
use crate::world::{ChunkState, World};
use harpas_core::nbt::{Compound, Nbt};

const MAX_HEALTH: f32 = 20.0;
const SAFE_FALL_DISTANCE: f64 = 3.0;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Vitals {
    pub(crate) health: f32,
    pub(crate) food: i32,
    pub(crate) saturation: f32,
}

impl Default for Vitals {
    fn default() -> Self {
        Self {
            health: MAX_HEALTH,
            food: 20,
            saturation: 5.0,
        }
    }
}

impl Vitals {
    pub(crate) fn update(self) -> Output {
        Output::Health {
            health: self.health,
            food: self.food,
            saturation: self.saturation,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DeathRules {
    pub(crate) fall_damage: bool,
    pub(crate) keep_inventory: bool,
    pub(crate) show_death_messages: bool,
}

impl Default for DeathRules {
    fn default() -> Self {
        Self::read(&Compound::new())
    }
}

impl DeathRules {
    pub(crate) fn read(rules: &Compound) -> Self {
        let boolean = |name, default| match rules.get(name) {
            Some(Nbt::Byte(value)) => *value != 0,
            _ => default,
        };
        Self {
            fall_damage: boolean("minecraft:fall_damage", true),
            keep_inventory: boolean("minecraft:keep_inventory", false),
            show_death_messages: boolean("minecraft:show_death_messages", true),
        }
    }
}

impl PlayerState {
    pub(crate) fn is_dead(&self) -> bool {
        self.vitals.health <= 0.0
    }
}

impl Game {
    pub(crate) fn check_fall(&mut self, index: usize, before: Position, after: Position) {
        let Some(world) = &self.world else { return };
        let rules = world.death_rules;
        let player = &mut self.players[index];
        let down = (before.y - after.y).max(0.0);
        if player.fall_distance == 0.0 && down == 0.0 {
            return;
        }
        let at = BlockPos {
            x: after.x.floor() as i32,
            y: after.y.floor() as i32,
            z: after.z.floor() as i32,
        };
        if !rules.fall_damage
            || matches!(
                player.player.profile.game_mode,
                GameMode::Creative | GameMode::Spectator
            )
            || !matches!(world.chunk(at.chunk()), Some(ChunkState::Loaded(_)))
            || world.point_in_water([after.x, after.y, after.z])
        {
            player.fall_distance = 0.0;
            return;
        }
        if after.on_ground && supports(world, after) {
            let distance = std::mem::take(&mut player.fall_distance) + down;
            let damage = (distance - SAFE_FALL_DISTANCE).ceil().max(0.0) as f32;
            if damage > 0.0 {
                self.hurt_by_falling(index, damage, at, rules);
            }
        } else {
            player.fall_distance += down;
        }
    }

    fn hurt_by_falling(&mut self, index: usize, damage: f32, at: BlockPos, rules: DeathRules) {
        let player = &mut self.players[index];
        // Only the part above the previous hit applies during the first ten ticks of damage immunity.
        let taken = if player.fall_hurt_cooldown > 10 {
            damage - player.last_fall_damage
        } else {
            player.fall_hurt_cooldown = 20;
            damage
        };
        if taken <= 0.0 {
            return;
        }
        player.last_fall_damage = damage;
        player.vitals.health = (player.vitals.health - taken).max(0.0);
        let entity_id = player.player.entity_id;
        for viewer in &self.players {
            if viewer.player.entity_id == entity_id || viewer.tracked.contains(&entity_id) {
                viewer.send(Output::FallDamage(entity_id));
            }
        }
        if self.players[index].is_dead() {
            self.die_from_fall(index, at, rules);
        }
        let player = &self.players[index];
        player.send(player.vitals.update());
    }

    fn die_from_fall(&mut self, index: usize, at: BlockPos, rules: DeathRules) {
        self.stop_digging(index);
        if self.players[index].block_menu.is_some() {
            self.close_block_menu(index);
        }
        let player = &mut self.players[index];
        let transient = player.inventory.take_transient_items();
        let mut drops = transient;
        if !rules.keep_inventory {
            drops.extend(std::mem::take(&mut player.inventory).slots);
        }
        player.pending_map_chest = None;
        player.death_location = Some(at);
        let entity_id = player.player.entity_id;
        let uuid = player.player.profile.uuid;
        let name = player.player.profile.name.clone();
        for viewer in &self.players {
            if rules.show_death_messages || viewer.player.entity_id == entity_id {
                viewer.send(Output::FallDeath {
                    entity_id,
                    uuid,
                    name: name.clone(),
                    show_message: rules.show_death_messages,
                });
            }
            if viewer.tracked.contains(&entity_id) {
                viewer.send(Output::PlayerDied(entity_id));
            }
        }
        for stack in drops {
            self.drop_item(index, stack);
        }
        self.save_player_inventory(index);
    }

    pub(crate) fn respawn(&mut self, index: usize) {
        let position = self
            .world
            .as_ref()
            .map_or_else(Position::default, World::spawn_position);
        let player = &mut self.players[index];
        let entity_id = player.player.entity_id;
        player.player.position = position;
        player.tick_start = position;
        player.client_floating = false;
        player.floating_ticks = 0;
        player.secondary_use = false;
        player.fall_hurt_cooldown = 0;
        player.last_fall_damage = 0.0;
        player.vitals = Vitals::default();
        player.digging = Default::default();
        player.sent_chunks.clear();
        let teleport_id = player.begin_teleport();
        player.send(Output::Respawn {
            teleport_id,
            position,
            mode: player.player.profile.game_mode,
            previous: player.previous_game_mode,
            death_location: player.death_location,
        });
        player.send(player.vitals.update());
        let contents = crate::place::initial_contents(&mut player.inventory);
        player.send(contents);
        player.send(Output::SelectedHotbarSlot(player.inventory.selected as i32));
        for viewer in &mut self.players {
            if viewer.tracked.remove(&entity_id) {
                viewer.send(Output::RemoveEntity(entity_id));
            }
        }
        self.save_player_inventory(index);
    }
}

fn supports(world: &World, position: Position) -> bool {
    let Position { x, y, z, .. } = position;
    world.collides([[x - 0.3, y - 1.0e-5, z - 0.3], [x + 0.3, y, z + 0.3]])
}
