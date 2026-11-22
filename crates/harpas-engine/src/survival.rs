//! Player health, food, damage and respawning in the Overworld.

use crate::Game;
use crate::block::BlockPos;
use crate::inventory::ItemStack;
use crate::item_data::{FoodProperties, ItemCodec};
use crate::player::{DamageKind, GameMode, Output, PlayerState, Position};
use crate::world::{ChunkPos, ChunkState, World};
use harpas_core::nbt::{Compound, Nbt};

const MAX_HEALTH: f32 = 20.0;
const MAX_FOOD: i32 = 20;
const SAFE_FALL_DISTANCE: f64 = 3.0;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Vitals {
    pub(crate) health: f32,
    pub(crate) food: i32,
    pub(crate) saturation: f32,
    pub(crate) exhaustion: f32,
    pub(crate) food_timer: i32,
}

impl Default for Vitals {
    fn default() -> Self {
        Self {
            health: MAX_HEALTH,
            food: MAX_FOOD,
            saturation: 5.0,
            exhaustion: 0.0,
            food_timer: 0,
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

    fn exhaust(&mut self, amount: f32) {
        self.exhaustion = (self.exhaustion + amount).min(40.0);
    }

    fn heal(&mut self, amount: f32) {
        self.health = (self.health + amount).min(MAX_HEALTH);
    }

    fn eat(&mut self, food: &FoodProperties) {
        self.food = self.food.saturating_add(food.nutrition).min(MAX_FOOD);
        self.saturation = (self.saturation + food.saturation).min(self.food as f32);
    }

    /// Returns whether this tick should deal starvation damage.
    fn tick_food(&mut self, rules: SurvivalRules) -> bool {
        if self.exhaustion > 4.0 {
            self.exhaustion -= 4.0;
            if self.saturation > 0.0 {
                self.saturation = (self.saturation - 1.0).max(0.0);
            } else if rules.difficulty != Difficulty::Peaceful {
                self.food = (self.food - 1).max(0);
            }
        }

        let regenerating =
            rules.natural_regeneration && self.health < MAX_HEALTH && self.food >= 18;
        let starving = self.food == 0;
        if !regenerating && !starving {
            self.food_timer = 0;
            return false;
        }
        let saturated = regenerating && self.saturation > 0.0 && self.food == MAX_FOOD;
        self.food_timer = self.food_timer.saturating_add(1);
        let interval = if saturated { 10 } else { 80 };
        if self.food_timer < interval {
            return false;
        }
        self.food_timer = 0;
        if starving {
            return self.health > rules.difficulty.starvation_floor();
        }
        let exhaustion = if saturated {
            self.saturation.min(6.0)
        } else {
            6.0
        };
        self.heal(exhaustion / 6.0);
        self.exhaust(exhaustion);
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Difficulty {
    Peaceful,
    Easy,
    Normal,
    Hard,
}

impl Difficulty {
    fn starvation_floor(self) -> f32 {
        match self {
            Self::Peaceful | Self::Easy => 10.0,
            Self::Normal => 1.0,
            Self::Hard => 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SurvivalRules {
    fall_damage: bool,
    keep_inventory: bool,
    show_death_messages: bool,
    natural_regeneration: bool,
    difficulty: Difficulty,
}

impl Default for SurvivalRules {
    fn default() -> Self {
        Self::read(None, None)
    }
}

impl SurvivalRules {
    pub(crate) fn read(rules: Option<&Compound>, level: Option<&Compound>) -> Self {
        let boolean = |name, default| match rules.and_then(|rules| rules.get(name)) {
            Some(Nbt::Byte(value)) => *value != 0,
            _ => default,
        };
        let difficulty = match level.and_then(|level| level.get("difficulty_settings")) {
            Some(Nbt::Compound(settings)) => match settings.get("difficulty") {
                Some(Nbt::String(name)) if name == "peaceful" => Difficulty::Peaceful,
                Some(Nbt::String(name)) if name == "normal" => Difficulty::Normal,
                Some(Nbt::String(name)) if name == "hard" => Difficulty::Hard,
                _ => Difficulty::Easy,
            },
            _ => Difficulty::Easy,
        };
        Self {
            fall_damage: boolean("minecraft:fall_damage", true),
            keep_inventory: boolean("minecraft:keep_inventory", false),
            show_death_messages: boolean("minecraft:show_death_messages", true),
            natural_regeneration: boolean("minecraft:natural_health_regeneration", true),
            difficulty,
        }
    }
}

pub(crate) struct FoodUse {
    pub(crate) off_hand: bool,
    ticks_left: u32,
    stack: ItemStack,
    food: FoodProperties,
}

impl PlayerState {
    pub(crate) fn is_dead(&self) -> bool {
        self.vitals.health <= 0.0
    }

    fn uses_food(&self) -> bool {
        matches!(
            self.player.profile.game_mode,
            GameMode::Survival | GameMode::Adventure
        )
    }
}

impl Game {
    pub(crate) fn sprint_exhaustion(&mut self, index: usize, before: Position, after: Position) {
        let player = &mut self.players[index];
        if !player.sprinting
            || !player.uses_food()
            || self
                .world
                .as_ref()
                .is_none_or(|world| world.point_in_water([after.x, after.y, after.z]))
        {
            return;
        }
        if before.on_ground && !after.on_ground && after.y > before.y {
            player.vitals.exhaust(0.2);
        }
        if after.on_ground {
            let centimeters = (100.0 * (after.x - before.x).hypot(after.z - before.z)).round();
            player.vitals.exhaust(0.1 * centimeters as f32 * 0.01);
        }
    }

    pub(crate) fn use_item(&mut self, index: usize, off_hand: bool, sequence: i32) {
        let player = &mut self.players[index];
        if !player.acknowledge_block_changes(sequence)
            || player.awaiting_teleport.is_some()
            || !player.uses_food()
            || player.food_use.is_some()
        {
            return;
        }
        let slot = player.inventory.hand_slot(off_hand);
        let stack = &player.inventory.slots[slot];
        let codec = self
            .world
            .as_ref()
            .map_or_else(|| ItemCodec::fallback(), World::block_item_codec);
        let Some(food) = codec.food(stack) else {
            return;
        };
        if player.vitals.food >= MAX_FOOD && !food.can_always_eat {
            return;
        }
        player.food_use = Some(FoodUse {
            off_hand,
            ticks_left: food.use_ticks,
            stack: stack.clone(),
            food,
        });
        self.send_using_item(index, Some(off_hand));
    }

    pub(crate) fn stop_using_item(&mut self, index: usize) {
        if self.players[index].food_use.take().is_some() {
            self.send_using_item(index, None);
        }
    }

    fn send_using_item(&self, index: usize, off_hand: Option<bool>) {
        let entity_id = self.players[index].player.entity_id;
        for viewer in &self.players {
            if viewer.player.entity_id == entity_id || viewer.tracked.contains(&entity_id) {
                viewer.send(Output::UsingItem {
                    entity_id,
                    off_hand,
                });
            }
        }
    }

    fn tick_food_use(&mut self, index: usize) {
        let player = &mut self.players[index];
        let Some(using) = &mut player.food_use else {
            return;
        };
        let slot = player.inventory.hand_slot(using.off_hand);
        let held = &mut player.inventory.slots[slot];
        if held.is_empty()
            || held.item != using.stack.item
            || held.components != using.stack.components
        {
            self.stop_using_item(index);
            return;
        }
        using.ticks_left = using.ticks_left.saturating_sub(1);
        if using.ticks_left == 0 {
            let mut remainder = using.food.remainder.clone().unwrap_or_default();
            held.count -= 1;
            if held.is_empty() {
                *held = std::mem::take(&mut remainder);
            }
            player.vitals.eat(&using.food);
            player.inventory.add(&mut remainder, false);
            player.send(Output::FinishUsingItem(player.player.entity_id));
            self.stop_using_item(index);
            if !remainder.is_empty() {
                self.drop_item(index, remainder);
            }
        }
    }

    pub(crate) fn tick_survival(&mut self, rules: SurvivalRules) {
        for index in 0..self.players.len() {
            let player = &mut self.players[index];
            player.hurt_cooldown = player.hurt_cooldown.saturating_sub(1);
            if !player.data_ready || player.disconnecting || player.is_dead() || !player.uses_food()
            {
                continue;
            }
            let before = player.vitals;
            self.tick_food_use(index);
            let player = &mut self.players[index];
            // On Peaceful, natural regeneration restores food before the starvation check.
            if rules.difficulty == Difficulty::Peaceful && rules.natural_regeneration {
                player.peaceful_food_ticks += 1;
                if player.peaceful_food_ticks == 10 {
                    player.peaceful_food_ticks = 0;
                    player.vitals.food = (player.vitals.food + 1).min(MAX_FOOD);
                }
            }
            if player.vitals.tick_food(rules)
                && self.hurt_player(index, 1.0, DamageKind::Starvation, rules)
            {
                continue;
            }
            let player = &self.players[index];
            if player.vitals.health != before.health
                || player.vitals.food != before.food
                || (player.vitals.saturation == 0.0) != (before.saturation == 0.0)
            {
                player.send(player.vitals.update());
            }
        }
    }

    pub(crate) fn check_fall(&mut self, index: usize, before: Position, after: Position) {
        let Some(world) = &self.world else { return };
        let rules = world.survival_rules;
        let player = &mut self.players[index];
        let down = (before.y - after.y).max(0.0);
        if player.fall_distance == 0.0 && down == 0.0 {
            return;
        }
        if !rules.fall_damage
            || matches!(
                player.player.profile.game_mode,
                GameMode::Creative | GameMode::Spectator
            )
            || !matches!(
                world.chunk(ChunkPos::at(after.x, after.z)),
                Some(ChunkState::Loaded(_))
            )
            || world.point_in_water([after.x, after.y, after.z])
        {
            player.fall_distance = 0.0;
            return;
        }
        if after.on_ground && supports(world, after) {
            let distance = std::mem::take(&mut player.fall_distance) + down;
            let damage = (distance - SAFE_FALL_DISTANCE).ceil().max(0.0) as f32;
            if damage > 0.0 {
                self.hurt_player(index, damage, DamageKind::Fall, rules);
            }
        } else {
            player.fall_distance += down;
        }
    }

    fn hurt_player(
        &mut self,
        index: usize,
        damage: f32,
        kind: DamageKind,
        rules: SurvivalRules,
    ) -> bool {
        let player = &mut self.players[index];
        // Only the part above the previous hit applies during the first ten ticks of damage immunity.
        let taken = if player.hurt_cooldown > 10 {
            damage - player.last_damage
        } else {
            player.hurt_cooldown = 20;
            damage
        };
        if taken <= 0.0 {
            return false;
        }
        player.last_damage = damage;
        player.vitals.health = (player.vitals.health - taken).max(0.0);
        let entity_id = player.player.entity_id;
        for viewer in &self.players {
            if viewer.player.entity_id == entity_id || viewer.tracked.contains(&entity_id) {
                viewer.send(Output::Damage { entity_id, kind });
            }
        }
        if self.players[index].is_dead() {
            self.die(index, kind, rules);
        }
        let player = &self.players[index];
        player.send(player.vitals.update());
        true
    }

    fn die(&mut self, index: usize, kind: DamageKind, rules: SurvivalRules) {
        self.stop_using_item(index);
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
        let Position { x, y, z, .. } = player.player.position;
        player.death_location = Some(BlockPos {
            x: x.floor() as i32,
            y: y.floor() as i32,
            z: z.floor() as i32,
        });
        let entity_id = player.player.entity_id;
        let uuid = player.player.profile.uuid;
        let name = player.player.profile.name.clone();
        for viewer in &self.players {
            if rules.show_death_messages || viewer.player.entity_id == entity_id {
                viewer.send(Output::Death {
                    entity_id,
                    uuid,
                    name: name.clone(),
                    kind: rules.show_death_messages.then_some(kind),
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
        player.sprinting = false;
        player.fall_distance = 0.0;
        player.hurt_cooldown = 0;
        player.last_damage = 0.0;
        player.peaceful_food_ticks = 0;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhaustion_consumes_saturation_before_food_and_requires_more_than_four() {
        let rules = SurvivalRules::default();
        let mut vitals = Vitals {
            saturation: 1.0,
            exhaustion: 4.0,
            ..Vitals::default()
        };
        vitals.tick_food(rules);
        assert_eq!(
            (vitals.food, vitals.saturation, vitals.exhaustion),
            (20, 1.0, 4.0)
        );
        vitals.exhaust(0.25);
        vitals.tick_food(rules);
        assert_eq!(
            (vitals.food, vitals.saturation, vitals.exhaustion),
            (20, 0.0, 0.25)
        );
        vitals.exhaust(4.0);
        vitals.tick_food(rules);
        assert_eq!((vitals.food, vitals.exhaustion), (19, 0.25));
    }

    #[test]
    fn regeneration_uses_the_food_timer_and_honors_the_game_rule() {
        let rules = SurvivalRules::default();
        for (food, saturation, interval) in [(20, 6.0, 10), (18, 0.0, 80)] {
            let mut vitals = Vitals {
                health: 16.0,
                food,
                saturation,
                ..Vitals::default()
            };
            for _ in 1..interval {
                vitals.tick_food(rules);
                assert_eq!(vitals.health, 16.0);
            }
            vitals.tick_food(rules);
            assert_eq!(
                (vitals.health, vitals.exhaustion, vitals.food_timer),
                (17.0, 6.0, 0)
            );
        }
        let disabled_rules = Compound::from_iter([("minecraft:natural_health_regeneration", 0_i8)]);
        let mut vitals = Vitals {
            health: 16.0,
            food_timer: 9,
            ..Vitals::default()
        };
        vitals.tick_food(SurvivalRules::read(Some(&disabled_rules), None));
        assert_eq!(
            (vitals.health, vitals.exhaustion, vitals.food_timer),
            (16.0, 0.0, 0)
        );
    }

    #[test]
    fn starvation_uses_the_food_timer_and_resets_when_the_player_eats() {
        let disabled_rules = Compound::from_iter([("minecraft:natural_health_regeneration", 0_i8)]);
        let rules = SurvivalRules::read(Some(&disabled_rules), None);
        let mut vitals = Vitals {
            food: 0,
            saturation: 0.0,
            food_timer: 40,
            ..Vitals::default()
        };
        for _ in 0..39 {
            assert!(!vitals.tick_food(rules));
        }
        assert!(vitals.tick_food(rules));
        assert_eq!(vitals.food_timer, 0);
        for _ in 0..79 {
            assert!(!vitals.tick_food(rules));
        }
        assert!(vitals.tick_food(rules));
        vitals.food_timer = 79;
        vitals.eat(&FoodProperties {
            nutrition: 1,
            saturation: 0.0,
            can_always_eat: false,
            use_ticks: 1,
            remainder: None,
        });
        assert!(!vitals.tick_food(rules));
        assert_eq!(vitals.food_timer, 0);
    }
}
