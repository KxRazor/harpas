//! Player health, food, damage and respawning in the Overworld.

use crate::Game;
use crate::block::BlockPos;
use crate::entity::Kind;
use crate::inventory::{ArmorSlot, ItemStack};
use crate::item_data::{Consumable, ConsumeEffect, FoodProperties, ItemCodec, MeleeWeapon};
use crate::player::{DamageKind, Effect, EffectKind, GameMode, Output, PlayerState, Position};
use crate::world::{ChunkPos, ChunkState, World};
use harpas_core::math::{cos_f32, sin_f32, wrap_degrees_f32};
use harpas_core::nbt::{Compound, Nbt};
use harpas_core::random::RandomSource;

const MAX_HEALTH: f32 = 20.0;
const MAX_FOOD: i32 = 20;
const SAFE_FALL_DISTANCE: f64 = 3.0;
const LAVA_FIRE_TICKS: i16 = 15 * 20;
const FIRE_ASPECT_TICKS_PER_LEVEL: i16 = 4 * 20;
const SURVIVAL_ENTITY_REACH: f64 = 3.0;
/// Java allows three extra blocks when validating a received attack on a mob.
const ANIMAL_ATTACK_REACH: f64 = SURVIVAL_ENTITY_REACH + 3.0;
const BASE_ATTACK_SPEED: f64 = 4.0;
const BASE_ATTACK_DAMAGE: f64 = 1.0;
const SHIELD_RAISE_TICKS: u32 = 5;
const SHIELD_DURABILITY: i32 = 336;
const SHIELD_WEAR_THRESHOLD: f32 = 3.0;
const SWEEP_MAX_HORIZONTAL_MOVEMENT: f64 = 0.25;
const SWEEP_BASE_DAMAGE: f32 = 1.0;
const HIT_KNOCKBACK: f64 = 0.4;
// A standing player already carries one tick's downward gravity before a hit.
const GROUNDED_HIT_LIFT: f64 = HIT_KNOCKBACK - 0.08 * 0.98 / 2.0;

fn hit_push(random: &mut impl RandomSource, mut dx: f64, mut dz: f64) -> [f64; 2] {
    while dx * dx + dz * dz < 1.0e-5 {
        dx = (random.next_double() - 0.5) * 0.02;
        dz = (random.next_double() - 0.5) * 0.02;
    }
    let scale = HIT_KNOCKBACK / dx.hypot(dz);
    [dx * scale, dz * scale]
}

fn apply_knockback(velocity: &mut [f64; 3], on_ground: bool, [x, z]: [f64; 2], strength: f64) {
    velocity[0] = velocity[0] / 2.0 + x;
    velocity[2] = velocity[2] / 2.0 + z;
    if on_ground {
        velocity[1] = (velocity[1] / 2.0 + strength).min(HIT_KNOCKBACK);
    }
}

fn attack_charge(source: &PlayerState) -> f32 {
    let speed = BASE_ATTACK_SPEED + source.attack_speed_modifier.unwrap_or_default();
    ((f32::from(source.attack_charge_ticks) + 0.5) / (20.0 / speed as f32)).min(1.0)
}

fn within_reach(eye: [f64; 3], [min, max]: [[f64; 3]; 2], reach: f64) -> bool {
    (0..3)
        .map(|axis| (eye[axis] - eye[axis].clamp(min[axis], max[axis])).powi(2))
        .sum::<f64>()
        < reach.powi(2)
}

pub(crate) type ActiveEffects = [Option<ActiveEffect>; EffectKind::ALL.len()];

#[derive(Debug, Clone)]
pub(crate) struct ActiveEffect {
    visible: Effect,
    hidden: Option<Box<Self>>,
}

impl ActiveEffect {
    pub(crate) fn absorption_limit(&self) -> f32 {
        4.0 * (f32::from(self.visible.amplifier) + 1.0)
    }

    pub(crate) fn read(tag: &Compound) -> Option<Self> {
        let duration = match tag.get("duration") {
            Some(Nbt::Int(duration)) if *duration == -1 || *duration > 0 => *duration,
            _ => return None,
        };
        let amplifier = match tag.get("amplifier") {
            Some(Nbt::Byte(amplifier)) => *amplifier as u8,
            Some(Nbt::Int(amplifier)) => u8::try_from(*amplifier).ok()?,
            _ => 0,
        };
        let flag = |name, default| match tag.get(name) {
            Some(Nbt::Byte(value)) => *value != 0,
            _ => default,
        };
        let show_particles = flag("show_particles", true);
        Some(Self {
            visible: Effect {
                duration,
                amplifier,
                ambient: flag("ambient", false),
                show_particles,
                show_icon: flag("show_icon", show_particles),
            },
            hidden: match tag.get("hidden_effect") {
                Some(Nbt::Compound(hidden)) => Self::read(hidden).map(Box::new),
                _ => None,
            },
        })
    }

    pub(crate) fn write(&self) -> Compound {
        let effect = self.visible;
        let mut tag = Compound::from_iter([
            ("duration", Nbt::Int(effect.duration)),
            ("amplifier", Nbt::Int(i32::from(effect.amplifier))),
            ("ambient", Nbt::Byte(i8::from(effect.ambient))),
            ("show_particles", Nbt::Byte(i8::from(effect.show_particles))),
            ("show_icon", Nbt::Byte(i8::from(effect.show_icon))),
        ]);
        if let Some(hidden) = &self.hidden {
            tag.insert("hidden_effect", hidden.write());
        }
        tag
    }

    fn from_food(duration: i32, amplifier: u8) -> Self {
        Self {
            visible: Effect {
                duration,
                amplifier,
                ambient: false,
                show_particles: true,
                show_icon: true,
            },
            hidden: None,
        }
    }

    fn refresh_from_food(&mut self, duration: i32, amplifier: u8) -> bool {
        let previous = self.visible;
        let food = Self::from_food(duration, amplifier);
        let lasts_longer = previous.duration != -1 && duration > previous.duration;
        if amplifier > previous.amplifier {
            // Keep a weaker effect if it will outlast the stronger one.
            if previous.duration == -1 || previous.duration > duration {
                self.hidden = Some(Box::new(Self {
                    visible: previous,
                    hidden: self.hidden.take(),
                }));
            }
            self.visible = food.visible;
        } else if lasts_longer {
            if amplifier == previous.amplifier {
                self.visible.duration = duration;
            } else if let Some(hidden) = &mut self.hidden {
                hidden.refresh_from_food(duration, amplifier);
            } else {
                self.hidden = Some(Box::new(food));
            }
        }
        self.visible.ambient = false;
        self.visible.show_particles = true;
        self.visible.show_icon = true;
        self.visible != previous
    }

    fn count_down(&mut self) {
        if self.visible.duration > 0 {
            self.visible.duration -= 1;
        }
        if let Some(hidden) = &mut self.hidden {
            hidden.count_down();
        }
    }

    fn tick(slot: &mut Option<Self>) -> bool {
        let Some(effect) = slot else { return false };
        effect.count_down();
        if effect.visible.duration != 0 {
            return false;
        }
        let mut next = effect.hidden.take();
        while let Some(hidden) = next.as_mut().filter(|hidden| hidden.visible.duration == 0) {
            next = hidden.hidden.take();
        }
        *slot = next.map(|effect| *effect);
        true
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Vitals {
    pub(crate) health: f32,
    pub(crate) absorption: f32,
    pub(crate) fire_ticks: i16,
    pub(crate) food: i32,
    pub(crate) saturation: f32,
    pub(crate) exhaustion: f32,
    pub(crate) food_timer: i32,
}

impl Default for Vitals {
    fn default() -> Self {
        Self {
            health: MAX_HEALTH,
            absorption: 0.0,
            fire_ticks: 0,
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
    fire_damage: bool,
    keep_inventory: bool,
    show_death_messages: bool,
    natural_regeneration: bool,
    mob_drops: bool,
    pvp: bool,
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
            fire_damage: boolean("minecraft:fire_damage", true),
            keep_inventory: boolean("minecraft:keep_inventory", false),
            show_death_messages: boolean("minecraft:show_death_messages", true),
            natural_regeneration: boolean("minecraft:natural_health_regeneration", true),
            mob_drops: boolean("minecraft:mob_drops", true),
            pvp: boolean("minecraft:pvp", true),
            difficulty,
        }
    }
}

pub(crate) struct ItemUse {
    pub(crate) off_hand: bool,
    ticks_left: u32,
    stack: ItemStack,
    kind: ItemUseKind,
}

enum ItemUseKind {
    Consume(Consumable),
    Shield,
}

impl ItemUse {
    fn matches(&self, held: &ItemStack) -> bool {
        !held.is_empty() && held.item == self.stack.item && held.components == self.stack.components
    }
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

    pub(crate) fn max_absorption(&self) -> f32 {
        self.effects[EffectKind::Absorption as usize]
            .as_ref()
            .map_or(0.0, ActiveEffect::absorption_limit)
    }

    pub(crate) fn is_burning(&self) -> bool {
        self.vitals.fire_ticks > 0 && self.player.profile.game_mode != GameMode::Spectator
    }

    pub(crate) fn send_max_absorption(&self) {
        self.send(Output::MaxAbsorption {
            entity_id: self.player.entity_id,
            amount: self.max_absorption(),
        });
    }

    pub(crate) fn send_absorption(&self) {
        self.send(Output::Absorption {
            entity_id: self.player.entity_id,
            amount: self.vitals.absorption,
        });
    }

    fn set_absorption(&mut self, amount: f32) {
        if self.vitals.absorption != amount {
            self.vitals.absorption = amount;
            self.send_absorption();
        }
    }

    pub(crate) fn send_effect(&self, kind: EffectKind, effect_id: Option<i32>, blend: bool) {
        if let Some(effect_id) = effect_id {
            self.send(Output::EntityEffect {
                entity_id: self.player.entity_id,
                effect_id,
                effect: self.effects[kind as usize]
                    .as_ref()
                    .map(|effect| effect.visible),
                blend,
            });
        }
    }

    fn remove_effect(&mut self, kind: EffectKind, effect_id: Option<i32>) {
        if self.effects[kind as usize].take().is_some() {
            self.send_effect(kind, effect_id, false);
            if kind == EffectKind::Absorption {
                self.send_max_absorption();
                self.set_absorption(0.0);
            }
        }
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
            || player.item_use.is_some()
        {
            return;
        }
        let slot = player.inventory.hand_slot(off_hand);
        let stack = &player.inventory.slots[slot];
        let codec = self
            .world
            .as_ref()
            .map_or_else(|| ItemCodec::fallback(), World::block_item_codec);
        let (kind, ticks_left) = if codec.is_shield(stack) {
            if player.shield_cooldown > 0 {
                return;
            }
            (ItemUseKind::Shield, SHIELD_RAISE_TICKS)
        } else if let Some(consumable) = codec.consumable(stack) {
            if player.vitals.food >= MAX_FOOD
                && consumable
                    .food
                    .as_ref()
                    .is_some_and(|food| !food.can_always_eat)
            {
                return;
            }
            let ticks = consumable.use_ticks;
            (ItemUseKind::Consume(consumable), ticks)
        } else {
            return;
        };
        player.item_use = Some(ItemUse {
            off_hand,
            ticks_left,
            stack: stack.clone(),
            kind,
        });
        self.send_using_item(index, Some(off_hand));
    }

    pub(crate) fn stop_using_item(&mut self, index: usize) {
        if self.players[index].item_use.take().is_some() {
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

    fn tick_item_use(&mut self, index: usize, effect_ids: [Option<i32>; EffectKind::ALL.len()]) {
        let player = &mut self.players[index];
        let Some(using) = &mut player.item_use else {
            return;
        };
        let slot = player.inventory.hand_slot(using.off_hand);
        let held = &mut player.inventory.slots[slot];
        if !using.matches(held) {
            self.stop_using_item(index);
            return;
        }
        using.ticks_left = using.ticks_left.saturating_sub(1);
        if using.ticks_left == 0
            && let ItemUseKind::Consume(consumable) = &mut using.kind
        {
            let codec = self
                .world
                .as_ref()
                .map_or_else(|| ItemCodec::fallback(), World::block_item_codec);
            let stew_effects = codec.stew_effects(held);
            let potion_effect = if matches!(consumable.effect, Some(ConsumeEffect::ApplyPotion(_)))
            {
                None
            } else {
                codec.potion_effect(held)
            };
            let mut remainder = consumable.remainder.take().unwrap_or_default();
            held.count -= 1;
            if held.is_empty() {
                *held = std::mem::take(&mut remainder);
            }
            if let Some(food) = &consumable.food {
                player.vitals.eat(food);
            }
            let default_effects = match consumable.effect.take() {
                Some(ConsumeEffect::Apply {
                    effects,
                    probability,
                }) if player.random.next_float() < probability => effects,
                Some(ConsumeEffect::ApplyPotion(effect)) => vec![effect],
                Some(ConsumeEffect::Remove(kinds)) => {
                    for kind in kinds {
                        player.remove_effect(kind, effect_ids[kind as usize]);
                    }
                    Vec::new()
                }
                Some(ConsumeEffect::ClearAll) => {
                    for (kind, effect_id) in EffectKind::ALL.into_iter().zip(effect_ids) {
                        player.remove_effect(kind, effect_id);
                    }
                    Vec::new()
                }
                _ => Vec::new(),
            };
            for effect in stew_effects
                .into_iter()
                .chain(default_effects)
                .chain(potion_effect)
            {
                let slot = &mut player.effects[effect.kind as usize];
                let new = slot.is_none();
                let changed = if let Some(active) = slot {
                    active.refresh_from_food(effect.duration, effect.amplifier)
                } else {
                    *slot = Some(ActiveEffect::from_food(effect.duration, effect.amplifier));
                    true
                };
                if changed {
                    player.send_effect(effect.kind, effect_ids[effect.kind as usize], new);
                    if effect.kind == EffectKind::Absorption {
                        player.send_max_absorption();
                        player.set_absorption(player.max_absorption());
                    }
                }
            }
            player.inventory.add(&mut remainder, false);
            player.send(Output::FinishUsingItem(player.player.entity_id));
            self.stop_using_item(index);
            if !remainder.is_empty() {
                self.drop_item(index, remainder);
            }
        }
    }

    fn tick_effects(
        &mut self,
        index: usize,
        effect_ids: [Option<i32>; EffectKind::ALL.len()],
        rules: SurvivalRules,
    ) {
        for (kind, effect_id) in EffectKind::ALL.into_iter().zip(effect_ids) {
            let player = &mut self.players[index];
            let Some(active) = &player.effects[kind as usize] else {
                continue;
            };
            let effect = active.visible;
            let tick = if effect.duration == -1 {
                player.ticks_alive
            } else {
                effect.duration
            };
            let interval = |base: i32| {
                base.checked_shr(u32::from(effect.amplifier))
                    .unwrap_or(0)
                    .max(1)
            };
            match kind {
                EffectKind::Hunger if player.uses_food() => {
                    player
                        .vitals
                        .exhaust(0.005 * (f32::from(effect.amplifier) + 1.0));
                }
                EffectKind::Poison
                    if player.uses_food()
                        && player.vitals.health > 1.0
                        && tick % interval(25) == 0 =>
                {
                    self.hurt_player(index, 1.0, DamageKind::Magic, rules);
                }
                EffectKind::Regeneration if tick % interval(50) == 0 => player.vitals.heal(1.0),
                _ => {}
            }
            let player = &mut self.players[index];
            let slot = &mut player.effects[kind as usize];
            let changed = if kind == EffectKind::Absorption && player.vitals.absorption == 0.0 {
                // Spending the last absorption heart ends the effect on the next effect tick.
                *slot = None;
                true
            } else {
                ActiveEffect::tick(slot)
            };
            if changed {
                player.send_effect(kind, effect_id, false);
                if kind == EffectKind::Absorption {
                    player.send_max_absorption();
                    player.set_absorption(player.vitals.absorption.min(player.max_absorption()));
                }
            }
        }
    }

    pub(crate) fn tick_survival(&mut self, rules: SurvivalRules) {
        let effect_ids = self
            .world
            .as_ref()
            .map_or_else(Default::default, World::effect_ids);
        for index in 0..self.players.len() {
            let player = &mut self.players[index];
            player.hurt_cooldown = player.hurt_cooldown.saturating_sub(1);
            if !player.data_ready || player.disconnecting || player.is_dead() {
                continue;
            }
            let before = player.vitals;
            player.ticks_alive = player.ticks_alive.wrapping_add(1);
            if player.shield_cooldown > 0 {
                player.shield_cooldown -= 1;
                if player.shield_cooldown == 0 {
                    player.send(Output::ShieldCooldown(0));
                }
            }
            let main_hand = &player.inventory.slots[player.inventory.hand_slot(false)];
            if player.last_main_hand_item == main_hand.item {
                player.attack_charge_ticks = player.attack_charge_ticks.saturating_add(1);
            } else {
                player.last_main_hand_item = main_hand.item;
                player.attack_charge_ticks = 0;
            }
            let codec = self
                .world
                .as_ref()
                .map_or_else(|| ItemCodec::fallback(), World::block_item_codec);
            let weapon = codec.melee_weapon(main_hand);
            player.attack_damage_modifier =
                weapon.map_or(0.0, |weapon| weapon.attack_damage_modifier);
            let sweeping_edge = codec
                .enchantment_level(main_hand, "minecraft:sweeping_edge")
                .max(0) as f32;
            player.sweeping_damage_ratio = sweeping_edge / (sweeping_edge + 1.0);
            let modifier = weapon
                .map(|weapon| weapon.attack_speed_modifier)
                .filter(|&modifier| modifier != 0.0);
            if player.attack_speed_modifier != modifier {
                player.attack_speed_modifier = modifier;
                player.send(Output::AttackSpeed {
                    entity_id: player.player.entity_id,
                    base_speed: BASE_ATTACK_SPEED,
                    modifier,
                });
            }
            let modifiers = ArmorSlot::ALL.map(|slot| {
                let stack = &player.inventory.slots[slot.menu_slot()];
                codec
                    .armor(stack, slot)
                    .filter(|_| !codec.component_removed(stack, "minecraft:attribute_modifiers"))
                    .map(|armor| armor.modifiers)
            });
            if player.armor_modifiers != modifiers {
                player.armor_modifiers = modifiers;
                player.send(Output::ArmorAttributes {
                    entity_id: player.player.entity_id,
                    modifiers,
                });
            }
            self.tick_fire(index, rules);
            if self.players[index].is_dead() {
                self.players[index].send(self.players[index].vitals.update());
                continue;
            }
            self.tick_effects(index, effect_ids, rules);
            if self.players[index].uses_food() {
                self.tick_item_use(index, effect_ids);
                let player = &mut self.players[index];
                // On Peaceful, natural regeneration restores food before the starvation check.
                if rules.difficulty == Difficulty::Peaceful && rules.natural_regeneration {
                    player.peaceful_food_ticks += 1;
                    if player.peaceful_food_ticks == 10 {
                        player.peaceful_food_ticks = 0;
                        player.vitals.food = (player.vitals.food + 1).min(MAX_FOOD);
                    }
                }
                if player.vitals.tick_food(rules) {
                    self.hurt_player(index, 1.0, DamageKind::Starvation, rules);
                }
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

    fn tick_fire(&mut self, index: usize, rules: SurvivalRules) {
        let Some(world) = &self.world else { return };
        let player = &mut self.players[index];
        let was_burning = player.vitals.fire_ticks > 0;
        let Position { x, y, z, .. } = player.player.position;
        let bounds = [[x - 0.3, y, z - 0.3], [x + 0.3, y + 1.8, z + 0.3]];
        let hit = if player.player.profile.game_mode == GameMode::Spectator {
            player.vitals.fire_ticks = 0;
            None
        } else if world.entity_fluid_height(bounds, false) > 0.0 {
            player.fall_distance *= 0.5;
            player.vitals.fire_ticks = player.vitals.fire_ticks.max(LAVA_FIRE_TICKS);
            Some((4.0, DamageKind::Lava))
        } else if was_burning && world.entity_fluid_height(bounds, true) > 0.0 {
            player.vitals.fire_ticks = 0;
            None
        } else if was_burning {
            let pulse = player.vitals.fire_ticks % 20 == 0;
            player.vitals.fire_ticks -= 1;
            pulse.then_some((1.0, DamageKind::OnFire))
        } else {
            None
        };
        if player.is_burning() != was_burning {
            self.send_burning(index);
        }
        if let Some((damage, kind)) = hit {
            self.hurt_player(index, damage, kind, rules);
        }
    }

    fn send_burning(&self, index: usize) {
        let player = &self.players[index];
        let entity_id = player.player.entity_id;
        let burning = player.is_burning();
        for viewer in &self.players {
            if viewer.player.entity_id == entity_id || viewer.tracked.contains(&entity_id) {
                viewer.send(Output::OnFire { entity_id, burning });
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
        if matches!(
            player.player.profile.game_mode,
            GameMode::Creative | GameMode::Spectator
        ) || !matches!(
            world.chunk(ChunkPos::at(after.x, after.z)),
            Some(ChunkState::Loaded(_))
        ) || world.point_in_water([after.x, after.y, after.z])
        {
            player.fall_distance = 0.0;
            return;
        }
        if after.on_ground && supports(world, after) {
            let distance = std::mem::take(&mut player.fall_distance) + down;
            let damage = (distance - SAFE_FALL_DISTANCE).ceil().max(0.0) as f32;
            if rules.fall_damage && damage > 0.0 {
                let health = player.vitals.health;
                self.hurt_player(index, damage, DamageKind::Fall, rules);
                let player = &self.players[index];
                if health != player.vitals.health {
                    player.send(player.vitals.update());
                }
            }
        } else {
            player.fall_distance += down;
        }
    }

    fn can_crit(&self, source: &PlayerState) -> bool {
        let position = source.player.position;
        source.fall_distance > 0.0
            && !position.on_ground
            && !source.sprinting
            && source.effects[EffectKind::Blindness as usize].is_none()
            && !self.world.as_ref().is_some_and(|world| {
                let Position { x, y, z, .. } = position;
                let bounds = [[x - 0.3, y, z - 0.3], [x + 0.3, y + 1.8, z + 0.3]];
                world.on_climbable(position) || world.entity_fluid_height(bounds, true) > 0.0
            })
    }

    pub(crate) fn attack_entity(&mut self, attacker: usize, target_id: i32) {
        let Some(defender) = self.active(target_id) else {
            self.attack_animal(attacker, target_id);
            return;
        };
        let rules = self
            .world
            .as_ref()
            .map_or_else(Default::default, |world| world.survival_rules);
        let source = &self.players[attacker];
        let target = &self.players[defender];
        let codec = self
            .world
            .as_ref()
            .map_or_else(|| ItemCodec::fallback(), World::block_item_codec);
        let slot = source.inventory.hand_slot(false);
        let main_hand = &source.inventory.slots[slot];
        let weapon = codec.melee_weapon(main_hand);
        if !rules.pvp
            || attacker == defender
            || source.player.profile.game_mode != GameMode::Survival
            || !target.uses_food()
            || source.awaiting_teleport.is_some()
            || target.awaiting_teleport.is_some()
            || (!main_hand.is_empty() && weapon.is_none())
        {
            return;
        }
        let Position { x, y, z, .. } = target.player.position;
        // Measure reach to the nearest point on the standing player's hitbox.
        let bounds = [[x - 0.3, y, z - 0.3], [x + 0.3, y + 1.8, z + 0.3]];
        if !within_reach(source.eye_position(), bounds, SURVIVAL_ENTITY_REACH) {
            return;
        }

        let source_id = source.player.entity_id;
        let dx = x - source.player.position.x;
        let dz = z - source.player.position.z;
        // Equipment attributes change on the player tick. Wear applies to the item held at the hit.
        let base_damage = BASE_ATTACK_DAMAGE + source.attack_damage_modifier;
        let charge = attack_charge(source);
        let charged = charge > 0.9;
        let sprint_hit = charged && source.sprinting;
        let charged_damage = base_damage as f32 * (0.2 + charge * charge * 0.8);
        let position = source.player.position;
        let critical = charged && self.can_crit(source);
        let sweeping = charged
            && !sprint_hit
            && position.on_ground
            && weapon.is_some()
            && main_hand
                .name()
                .is_some_and(|name| name.ends_with("_sword"))
            && source.horizontal_movement_squared < SWEEP_MAX_HORIZONTAL_MOVEMENT.powi(2);
        let sharpness_level = codec.enchantment_level(main_hand, "minecraft:sharpness");
        let sharpness_bonus = if sharpness_level > 0 {
            1.0 + (sharpness_level - 1) as f32 * 0.5
        } else {
            0.0
        };
        let critical_multiplier = if critical { 1.5 } else { 1.0 };
        // Sharpness scales linearly with charge on the primary hit.
        let damage = charged_damage * critical_multiplier + sharpness_bonus * charge;
        let sweeping_damage_ratio = source.sweeping_damage_ratio;
        let knockback_level = codec.enchantment_level(main_hand, "minecraft:knockback");
        let extra_knockback = 0.5 * f64::from(knockback_level + i32::from(sprint_hit));
        let fire_aspect = codec.enchantment_level(main_hand, "minecraft:fire_aspect") as i16;
        self.players[attacker].attack_charge_ticks = 0;
        let shield_disable_ticks = weapon.map_or(0, |weapon| weapon.shield_disable_ticks);
        if self.block_melee_attack(defender, attacker, damage, shield_disable_ticks) {
            return;
        }
        let fresh_hit = self.players[defender].hurt_cooldown <= 10;
        let health = self.players[defender].vitals.health;
        if !self.hurt_player(defender, damage, DamageKind::PlayerAttack(source_id), rules) {
            return;
        }
        if critical {
            for viewer in &self.players {
                if viewer.player.entity_id == source_id || viewer.tracked.contains(&source_id) {
                    viewer.send(Output::CriticalHit(target_id));
                }
            }
        }
        self.players[attacker].vitals.exhaust(0.1);
        if extra_knockback > 0.0 {
            self.players[attacker].sprinting = false;
        }
        let target = &mut self.players[defender];
        let on_ground = target.player.position.on_ground;
        if fresh_hit {
            let [dx, dz] = hit_push(&mut target.random, dx, dz);
            let vertical = if on_ground { GROUNDED_HIT_LIFT } else { 0.0 };
            let mut velocity = [dx, vertical, dz];
            if extra_knockback > 0.0 {
                // A second knockback halves the horizontal motion before pushing along
                // the attacker's facing and caps the lift on the ground.
                let yaw = wrap_degrees_f32(position.yaw).to_radians();
                let (x, z) = (-f64::from(sin_f32(yaw)), f64::from(cos_f32(yaw)));
                let push = extra_knockback / x.hypot(z);
                velocity[0] = velocity[0] * 0.5 + x * push;
                velocity[2] = velocity[2] * 0.5 + z * push;
                if on_ground {
                    velocity[1] = HIT_KNOCKBACK;
                }
            }
            target.send(Output::EntityVelocity {
                entity_id: target_id,
                velocity,
            });
        }
        if health != target.vitals.health {
            target.send(target.vitals.update());
        }
        if fire_aspect > 0 {
            let was_burning = target.is_burning();
            target.vitals.fire_ticks = target
                .vitals
                .fire_ticks
                .max(FIRE_ASPECT_TICKS_PER_LEVEL * fire_aspect);
            if !was_burning {
                self.send_burning(defender);
            }
        }
        if sweeping {
            let sweep_damage = SWEEP_BASE_DAMAGE + sweeping_damage_ratio * charged_damage;
            // Secondary targets take the full Sharpness bonus.
            self.sweep_attack(attacker, defender, sweep_damage + sharpness_bonus, rules);
        }
        self.wear_melee_weapon(attacker, slot, weapon);
    }

    fn wear_melee_weapon(&mut self, attacker: usize, slot: usize, weapon: Option<MeleeWeapon>) {
        if let Some(weapon) = weapon.filter(|weapon| weapon.damage_per_attack > 0) {
            let codec = self
                .world
                .as_ref()
                .map_or_else(|| ItemCodec::fallback(), World::block_item_codec);
            let source_id = self.players[attacker].player.entity_id;
            let stack = &mut self.players[attacker].inventory.slots[slot];
            match codec.wear_item(stack, weapon.durability, weapon.damage_per_attack) {
                Ok(true) => Self::send_item_broken(
                    &self.players,
                    source_id,
                    Output::MainHandItemBroken(source_id),
                ),
                Ok(false) => {}
                Err(error) => log::warn!("Cannot apply melee weapon wear to item {slot}: {error}"),
            }
        }
    }

    fn attack_animal(&mut self, attacker: usize, entity_id: i32) {
        let source = &self.players[attacker];
        let hand_slot = source.inventory.hand_slot(false);
        let main_hand = &source.inventory.slots[hand_slot];
        if source.player.profile.game_mode != GameMode::Survival
            || source.awaiting_teleport.is_some()
        {
            return;
        }
        let charge = attack_charge(source);
        let charged = charge > 0.9;
        let critical = charged && self.can_crit(source);
        let sprint_hit = charged && source.sprinting;
        let position = source.player.position;
        let source_id = source.player.entity_id;
        let base_damage = BASE_ATTACK_DAMAGE + source.attack_damage_modifier;
        let Some(world) = &mut self.world else { return };
        let sword = world
            .block_item_codec()
            .melee_weapon(main_hand)
            .filter(|_| {
                main_hand
                    .name()
                    .is_some_and(|name| name.ends_with("_sword"))
            });
        if !main_hand.is_empty() && sword.is_none() {
            return;
        }
        let entities = &mut world.entities;
        let Some(handle) = entities.animal_target(entity_id) else {
            return;
        };
        let slot = entities.slot(handle);
        if !within_reach(
            source.eye_position(),
            entities.animal_box(slot),
            ANIMAL_ATTACK_REACH,
        ) {
            return;
        }
        let Kind::Animal(animal) = entities.kind[slot].as_mut().expect("live animal") else {
            unreachable!()
        };
        self.players[attacker].attack_charge_ticks = 0;
        let mut damage = base_damage as f32 * (0.2 + charge * charge * 0.8);
        if critical {
            damage *= 1.5;
        }
        let fresh_hit = animal.hurt_cooldown <= 10;
        let taken = if fresh_hit {
            damage
        } else {
            damage - animal.last_damage
        };
        if taken <= 0.0 || crate::entity_data::nbt_bool(&animal.nbt, "Invulnerable") {
            return;
        }
        animal.last_damage = damage;
        if fresh_hit {
            animal.hurt_cooldown = 20;
        }
        animal.health = (animal.health - taken).max(0.0);
        let health = animal.health;
        let species = animal.species;

        let animal_position @ [x, _, z] = entities.position[slot];
        let velocity = &mut entities.velocity[slot];
        let on_ground = entities.on_ground[slot];
        if fresh_hit {
            let [dx, dz] = hit_push(&mut world.random, x - position.x, z - position.z);
            apply_knockback(velocity, on_ground, [dx, dz], HIT_KNOCKBACK);
        }
        if sprint_hit {
            let yaw = wrap_degrees_f32(position.yaw).to_radians();
            let [x, z] = [-sin_f32(yaw), cos_f32(yaw)].map(f64::from);
            let scale = 0.5 / x.hypot(z);
            apply_knockback(velocity, on_ground, [x * scale, z * scale], 0.5);
            self.players[attacker].sprinting = false;
        }
        let pushed = fresh_hit || sprint_hit;
        let velocity = *velocity;
        if pushed {
            entities.server[slot].last_velocity = velocity;
            entities.impulse[slot] = true;
        }
        entities.mark_changed(handle);
        self.players[attacker].vitals.exhaust(0.1);
        for viewer in entities.seen_by(handle) {
            let Some(player) = self.players.iter().find(|p| p.player.entity_id == viewer) else {
                continue;
            };
            if fresh_hit {
                player.send(Output::Damage {
                    entity_id,
                    kind: DamageKind::PlayerAttack(source_id),
                });
            }
            if health == 0.0 {
                player.send(Output::AnimalDied(entity_id));
            } else {
                player.send(Output::AnimalHealth { entity_id, health });
            }
            if pushed {
                player.send(Output::EntityVelocity {
                    entity_id,
                    velocity,
                });
            }
            if critical {
                player.send(Output::CriticalHit(entity_id));
            }
        }
        if health == 0.0 && world.survival_rules.mob_drops {
            let tool = &self.players[attacker].inventory.slots[hand_slot];
            world.drop_animal_loot(species, animal_position, tool);
        }
        self.wear_melee_weapon(attacker, hand_slot, sword);
    }

    fn sweep_attack(
        &mut self,
        attacker: usize,
        defender: usize,
        damage: f32,
        rules: SurvivalRules,
    ) {
        let source = self.players[attacker].player.position;
        let primary = self.players[defender].player.position;
        let source_id = self.players[attacker].player.entity_id;
        let player_width = f64::from(0.6_f32);
        let player_height = f64::from(1.8_f32);
        let yaw = wrap_degrees_f32(source.yaw).to_radians();
        let direction = [-sin_f32(yaw), cos_f32(yaw)];
        let [x, z] = direction.map(f64::from);
        let push = HIT_KNOCKBACK / x.hypot(z);
        for index in 0..self.players.len() {
            let target = &self.players[index];
            let position = target.player.position;
            let dx = position.x - source.x;
            let dy = position.y - source.y;
            let dz = position.z - source.z;
            // The primary player's box is inflated by one block horizontally and a quarter vertically.
            if index == attacker
                || index == defender
                || !target.data_ready
                || target.disconnecting
                || target.is_dead()
                || !target.uses_food()
                || target.awaiting_teleport.is_some()
                || (position.x - primary.x).abs() >= player_width + 1.0
                || (position.y - primary.y).abs() >= player_height + 0.25
                || (position.z - primary.z).abs() >= player_width + 1.0
                || dx * dx + dy * dy + dz * dz >= SURVIVAL_ENTITY_REACH.powi(2)
            {
                continue;
            }
            let fresh_hit = target.hurt_cooldown <= 10;
            let health = target.vitals.health;
            let entity_id = target.player.entity_id;
            if self.block_melee_attack(index, attacker, damage, 0)
                || !self.hurt_player(index, damage, DamageKind::PlayerAttack(source_id), rules)
            {
                continue;
            }
            let target = &mut self.players[index];
            let [dx, dz] = if fresh_hit {
                hit_push(&mut target.random, dx, dz)
            } else {
                [0.0; 2]
            };
            let vertical = match (position.on_ground, fresh_hit) {
                (true, true) => HIT_KNOCKBACK,
                (true, false) => GROUNDED_HIT_LIFT,
                (false, _) => 0.0,
            };
            // The sweep adds a second push along the attacker's facing, halving the first.
            let velocity = [dx * 0.5 + x * push, vertical, dz * 0.5 + z * push];
            target.send(Output::EntityVelocity {
                entity_id,
                velocity,
            });
            if health != target.vitals.health {
                target.send(target.vitals.update());
            }
        }

        let seed = self
            .world
            .as_mut()
            .map_or(0, |world| world.random.next_long());
        let particle = [source.x + x, source.y + player_height * 0.5, source.z + z];
        for viewer in &self.players {
            let position = viewer.player.position;
            if (position.x - source.x).powi(2)
                + (position.y - source.y).powi(2)
                + (position.z - source.z).powi(2)
                < 16.0_f64.powi(2)
            {
                viewer.send(Output::SweepAttackSound {
                    position: source,
                    seed,
                });
            }
            if (position.x.floor() + 0.5 - particle[0]).powi(2)
                + (position.y.floor() + 0.5 - particle[1]).powi(2)
                + (position.z.floor() + 0.5 - particle[2]).powi(2)
                < 32.0_f64.powi(2)
            {
                viewer.send(Output::SweepAttackParticle {
                    position: particle,
                    direction,
                });
            }
        }
    }

    fn block_melee_attack(
        &mut self,
        defender: usize,
        attacker: usize,
        damage: f32,
        shield_disable_ticks: i32,
    ) -> bool {
        let source = self.players[attacker].player.position;
        let target = &mut self.players[defender];
        let Some(using) = &mut target.item_use else {
            return false;
        };
        let position = target.player.position;
        let dx = source.x - position.x;
        let dz = source.z - position.z;
        let yaw = wrap_degrees_f32(position.yaw).to_radians();
        let towards_attacker = -f64::from(sin_f32(yaw)) * dx + f64::from(cos_f32(yaw)) * dz;
        let off_hand = using.off_hand;
        let slot = target.inventory.hand_slot(off_hand);
        let held = &mut target.inventory.slots[slot];
        if !matches!(using.kind, ItemUseKind::Shield)
            || using.ticks_left != 0
            || !using.matches(held)
            || towards_attacker < 0.0
        {
            return false;
        }

        let entity_id = target.player.entity_id;
        if damage >= SHIELD_WEAR_THRESHOLD {
            let codec = self
                .world
                .as_ref()
                .map_or_else(|| ItemCodec::fallback(), World::block_item_codec);
            match codec.wear_item(held, SHIELD_DURABILITY, 1 + damage.floor() as i32) {
                Ok(true) => {
                    let broken = if off_hand {
                        Output::OffHandItemBroken(entity_id)
                    } else {
                        Output::MainHandItemBroken(entity_id)
                    };
                    Self::send_item_broken(&self.players, entity_id, broken);
                    self.stop_using_item(defender);
                }
                Ok(false) => using.stack = held.clone(),
                Err(error) => log::warn!("Cannot apply shield wear to item {slot}: {error}"),
            }
        }
        if shield_disable_ticks > 0 && self.players[defender].item_use.is_some() {
            let target = &mut self.players[defender];
            target.shield_cooldown = shield_disable_ticks;
            target.send(Output::ShieldCooldown(shield_disable_ticks));
            self.stop_using_item(defender);
            self.play_shield_sound(defender, true);
        }
        self.play_shield_sound(defender, false);
        true
    }

    fn play_shield_sound(&mut self, defender: usize, disabled: bool) {
        let target = &mut self.players[defender];
        let position = target.player.position;
        let pitch = 0.8 + target.random.next_float() * 0.4;
        let seed = target.random.next_long();
        for nearby in &self.players {
            let listener = nearby.player.position;
            if (listener.x - position.x).powi(2)
                + (listener.y - position.y).powi(2)
                + (listener.z - position.z).powi(2)
                < 16.0_f64.powi(2)
            {
                nearby.send(Output::ShieldSound {
                    position,
                    disabled,
                    pitch,
                    seed,
                });
            }
        }
    }

    fn hurt_armor(&mut self, index: usize, damage: f32) -> f32 {
        let codec = self
            .world
            .as_ref()
            .map_or_else(|| ItemCodec::fallback(), World::block_item_codec);
        let entity_id = self.players[index].player.entity_id;
        let wear = ((damage / 4.0) as i32).max(1);
        for slot in ArmorSlot::ALL {
            let stack = &mut self.players[index].inventory.slots[slot.menu_slot()];
            if let Some(armor) = codec.armor(stack, slot)
                && !codec.component_removed(stack, "minecraft:equippable")
            {
                match codec.wear_item(stack, armor.durability, wear) {
                    Ok(true) => Self::send_item_broken(
                        &self.players,
                        entity_id,
                        Output::ArmorBroken { entity_id, slot },
                    ),
                    Ok(false) => {}
                    Err(error) => log::warn!("Cannot apply wear to armor in {slot:?}: {error}"),
                }
            }
        }
        // A broken piece still protects against the hit that broke it. Attributes update on the player tick.
        let [armor, toughness] = self.players[index]
            .armor_modifiers
            .into_iter()
            .flatten()
            .fold([0.0, 0.0], |[armor, toughness], [points, extra]| {
                [armor + points, toughness + extra]
            });
        let armor = armor as f32;
        let toughness = toughness as f32;
        let effective = (armor - damage / (2.0 + toughness / 4.0))
            .max(armor * 0.2)
            .min(20.0);
        damage * (1.0 - effective / 25.0)
    }

    fn hurt_player(
        &mut self,
        index: usize,
        damage: f32,
        kind: DamageKind,
        rules: SurvivalRules,
    ) -> bool {
        let player = &mut self.players[index];
        if matches!(kind, DamageKind::Lava | DamageKind::OnFire)
            && (!rules.fire_damage
                || !player.uses_food()
                || player.effects[EffectKind::FireResistance as usize].is_some())
        {
            return false;
        }
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
        let player_attack = matches!(kind, DamageKind::PlayerAttack(_));
        let taken = if player_attack {
            self.hurt_armor(index, taken)
        } else {
            taken
        };
        let player = &self.players[index];
        let taken = match &player.effects[EffectKind::Resistance as usize] {
            Some(effect) if kind != DamageKind::Starvation => {
                let remaining = (25 - 5 * (i32::from(effect.visible.amplifier) + 1)).max(0);
                taken * remaining as f32 / 25.0
            }
            _ => taken,
        };
        let taken = if player_attack {
            let codec = self
                .world
                .as_ref()
                .map_or_else(|| ItemCodec::fallback(), World::block_item_codec);
            // Enchantments use the equipment left after armor wear, before absorption is spent.
            let protection = ArmorSlot::ALL
                .into_iter()
                .map(|slot| {
                    let stack = &player.inventory.slots[slot.menu_slot()];
                    codec.enchantment_level(stack, "minecraft:protection")
                })
                .sum::<i32>()
                .min(20);
            taken * (1.0 - protection as f32 / 25.0)
        } else {
            taken
        };
        let player = &mut self.players[index];
        let absorbed = player.vitals.absorption.min(taken);
        let health_damage = taken - absorbed;
        if health_damage > 0.0 && player_attack {
            player.vitals.exhaust(0.1);
        }
        player.vitals.health = (player.vitals.health - health_damage).max(0.0);
        let entity_id = player.player.entity_id;
        for viewer in &self.players {
            if viewer.player.entity_id == entity_id || viewer.tracked.contains(&entity_id) {
                viewer.send(Output::Damage { entity_id, kind });
            }
        }
        let player = &mut self.players[index];
        player.set_absorption(player.vitals.absorption - absorbed);
        if self.players[index].is_dead() {
            self.die(index, kind, rules);
        }
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
            player.experience = Default::default();
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
        let attacker = match kind {
            DamageKind::PlayerAttack(id) => self.players.iter().find_map(|state| {
                let profile = &state.player.profile;
                (state.player.entity_id == id).then(|| (profile.uuid, profile.name.clone()))
            }),
            _ => None,
        };
        for viewer in &self.players {
            if rules.show_death_messages || viewer.player.entity_id == entity_id {
                viewer.send(Output::Death {
                    entity_id,
                    uuid,
                    name: name.clone(),
                    kind: rules.show_death_messages.then_some(kind),
                    attacker: attacker.clone(),
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
        player.shield_cooldown = 0;
        player.attack_charge_ticks = 0;
        player.last_main_hand_item = 0;
        player.attack_damage_modifier = 0.0;
        player.attack_speed_modifier = None;
        player.sweeping_damage_ratio = 0.0;
        player.armor_modifiers = [None; 4];
        player.last_damage = 0.0;
        player.peaceful_food_ticks = 0;
        player.effects = Default::default();
        player.ticks_alive = 0;
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
        player.send(player.experience.update());
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
    fn stronger_food_effect_preserves_weaker_effects_that_outlast_it() {
        let mut effect = ActiveEffect::from_food(12, 0);
        assert!(effect.refresh_from_food(2, 2));
        assert_eq!((effect.visible.amplifier, effect.visible.duration), (2, 2));
        assert!(!effect.refresh_from_food(6, 1));
        let mut active = Some(effect);
        for (ticks, expected) in [(2, (1, 4)), (4, (0, 6))] {
            for _ in 1..ticks {
                assert!(!ActiveEffect::tick(&mut active));
            }
            assert!(ActiveEffect::tick(&mut active));
            let visible = active.as_ref().unwrap().visible;
            assert_eq!((visible.amplifier, visible.duration), expected);
        }
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
        });
        assert!(!vitals.tick_food(rules));
        assert_eq!(vitals.food_timer, 0);
    }
}
