//! Item stacks in vanilla's saved NBT. The owner's generated registry supplies component IDs, with the pinned
//! Java 26.1 IDs used by mining and eating available when the registry is absent. A saved item is never loaded with its data
//! stripped, and a new Creative stack must be serializable before it is accepted.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, OnceLock};

use harpas_core::item::items;
use harpas_core::nbt::{Compound, List, Nbt};

use crate::inventory::{ArmorSlot, ComponentPatch, ItemStack};
use crate::player::EffectKind;

const POTION_CONTENTS: &str = "minecraft:potion_contents";
pub(crate) const STEW_EFFECTS: &str = "minecraft:suspicious_stew_effects";
const DEFAULT_STEW_DURATION: i32 = 160;
const DAMAGE: &str = "minecraft:damage";
const MAX_DAMAGE: &str = "minecraft:max_damage";
const UNBREAKABLE: &str = "minecraft:unbreakable";
const FOOD: &str = "minecraft:food";
const CONSUMABLE: &str = "minecraft:consumable";
const USE_REMAINDER: &str = "minecraft:use_remainder";
const BLOCKS_ATTACKS: &str = "minecraft:blocks_attacks";

/// The default `food` component of a supported item.
#[derive(Debug, Clone)]
pub struct FoodProperties {
    /// Food points restored by one item.
    pub nutrition: i32,
    /// Saturation points added before clamping to the new food level.
    pub saturation: f32,
    /// Whether this food can be eaten with a full food bar.
    pub can_always_eat: bool,
}

/// The default components of a supported item that can be eaten or drunk.
#[derive(Debug, Clone)]
pub struct Consumable {
    /// Nutrition and the hunger requirement for foods.
    pub food: Option<FoodProperties>,
    /// Number of ticks use must be held.
    pub use_ticks: u32,
    /// The default stack left after consumption, such as a bowl, bucket or bottle.
    pub remainder: Option<ItemStack>,
    /// Action taken when consumption finishes.
    pub effect: Option<ConsumeEffect>,
}

/// An effect applied or removed when a supported item is consumed.
#[derive(Debug, Clone)]
pub enum ConsumeEffect {
    /// Apply the whole group of effects after a successful roll.
    Apply {
        /// Effects applied in order.
        effects: Vec<FoodEffect>,
        /// Probability of applying the group, from zero to one.
        probability: f32,
    },
    /// Apply the effect stored in a drinkable potion.
    ApplyPotion(FoodEffect),
    /// Remove the specified active status effects.
    Remove(Vec<EffectKind>),
    /// Remove all active status effects.
    ClearAll,
}

/// A supported effect configured on a vanilla consumable.
#[derive(Debug, Clone, Copy)]
pub struct FoodEffect {
    /// The effect to apply.
    pub kind: EffectKind,
    /// Duration in ticks.
    pub duration: i32,
    /// Level of the effect, starting at zero.
    pub amplifier: u8,
}

/// The default main-hand modifiers and durability of an ordinary vanilla sword.
#[derive(Debug, Clone, Copy)]
pub struct Sword {
    /// Additive modifier to the player's attack damage.
    pub attack_damage_modifier: f64,
    /// Additive modifier to the player's attack speed.
    pub attack_speed_modifier: f64,
    /// Maximum durability of the item before stack overrides.
    pub durability: i32,
}

/// The default protection and durability of an ordinary piece of player armor.
#[derive(Debug, Clone, Copy)]
pub struct Armor {
    /// Equipment slot in which the piece provides protection.
    pub slot: ArmorSlot,
    /// Additive armor and armor toughness modifiers, in that order.
    pub modifiers: [f64; 2],
    /// Maximum durability of the item before stack overrides.
    pub durability: i32,
}

/// Converts the supported item component values between their network bytes and vanilla's saved NBT.
#[derive(Debug)]
pub struct ItemCodec {
    names: Vec<String>,
    ids: HashMap<String, i32>,
    enchantments: Vec<String>,
    potions: Vec<String>,
    mob_effects: Vec<String>,
    effect_ids: [Option<i32>; EffectKind::ALL.len()],
    consumables: HashMap<u32, Consumable>,
    swords: HashMap<u32, Sword>,
    armor: HashMap<u32, Armor>,
}

impl Default for ItemCodec {
    fn default() -> Self {
        Self::new(&[])
    }
}

impl ItemCodec {
    /// Component names in the numeric order of the locally generated `minecraft:data_component_type` registry.
    /// An empty registry uses the Java 26.1 IDs needed for basic mining tools and food component removals.
    pub fn new(names: &[String]) -> Self {
        let names = if names.is_empty() {
            let mut names = vec![String::new(); 29];
            for (index, name) in [
                (1, "minecraft:max_stack_size"),
                (2, MAX_DAMAGE),
                (3, DAMAGE),
                (4, UNBREAKABLE),
                (23, FOOD),
                (24, CONSUMABLE),
                (28, "minecraft:tool"),
            ] {
                names[index] = name.to_owned();
            }
            names
        } else {
            names.to_vec()
        };
        Self {
            ids: names
                .iter()
                .enumerate()
                .filter(|(_, name)| !name.is_empty())
                .map(|(id, name)| (name.clone(), id as i32))
                .collect(),
            names,
            enchantments: Vec::new(),
            potions: Vec::new(),
            mob_effects: Vec::new(),
            effect_ids: Default::default(),
            swords: HashMap::new(),
            armor: HashMap::new(),
            consumables: HashMap::from([(
                items().id("minecraft:apple").expect("registered apple"),
                Consumable {
                    food: Some(FoodProperties {
                        nutrition: 4,
                        saturation: 2.4,
                        can_always_eat: false,
                    }),
                    use_ticks: 32,
                    remainder: None,
                    effect: None,
                },
            )]),
        }
    }

    pub(crate) fn fallback() -> &'static Self {
        static DEFAULT: OnceLock<ItemCodec> = OnceLock::new();
        DEFAULT.get_or_init(Self::default)
    }

    /// Loads the supported consumable defaults from the owner's server jar, keyed by item ID.
    pub fn with_consumables(mut self, consumables: HashMap<u32, Consumable>) -> Self {
        self.consumables = consumables;
        self
    }

    /// Loads the sword defaults from the owner's server jar, keyed by item ID.
    pub fn with_swords(mut self, swords: HashMap<u32, Sword>) -> Self {
        self.swords = swords;
        self
    }

    pub(crate) fn sword(&self, stack: &ItemStack) -> Option<Sword> {
        if stack.is_empty() {
            return None;
        }
        let mut sword = *self.swords.get(&stack.item)?;
        if self.component_removed(stack, "minecraft:attribute_modifiers") {
            sword.attack_damage_modifier = 0.0;
            sword.attack_speed_modifier = 0.0;
        }
        Some(sword)
    }

    /// Loads the player armor defaults from the owner's server jar, keyed by item ID.
    pub fn with_armor(mut self, armor: HashMap<u32, Armor>) -> Self {
        self.armor = armor;
        self
    }

    pub(crate) fn armor(&self, stack: &ItemStack, slot: ArmorSlot) -> Option<Armor> {
        let armor = *self.armor.get(&stack.item)?;
        (!stack.is_empty() && armor.slot == slot).then_some(armor)
    }

    pub(crate) fn enchantment_level(&self, stack: &ItemStack, enchantment: &str) -> i32 {
        if stack.is_empty() {
            return 0;
        }
        let Some(Nbt::Compound(levels)) = self.saved_component(stack, "minecraft:enchantments")
        else {
            return 0;
        };
        match levels.get(enchantment) {
            Some(Nbt::Int(level)) => *level,
            _ => 0,
        }
    }

    pub(crate) fn consumable(&self, stack: &ItemStack) -> Option<Consumable> {
        if stack.is_empty() || self.component_removed(stack, CONSUMABLE) {
            return None;
        }
        let mut consumable = self.consumables.get(&stack.item)?.clone();
        if consumable.food.is_some() && self.component_removed(stack, FOOD) {
            return None;
        }
        if self.component_removed(stack, USE_REMAINDER) {
            consumable.remainder = None;
        }
        if stack.name() == Some("minecraft:potion") {
            let Nbt::Compound(contents) = self.saved_component(stack, POTION_CONTENTS)? else {
                return None;
            };
            let Some(Nbt::String(name)) = contents.get("potion") else {
                return None;
            };
            let (duration, amplifier) = match name.as_str() {
                "minecraft:regeneration" => (900, 0),
                "minecraft:long_regeneration" => (1800, 0),
                "minecraft:strong_regeneration" => (450, 1),
                _ => return None,
            };
            consumable.effect = Some(ConsumeEffect::ApplyPotion(FoodEffect {
                kind: EffectKind::Regeneration,
                duration,
                amplifier,
            }));
        }
        Some(consumable)
    }

    pub(crate) fn is_shield(&self, stack: &ItemStack) -> bool {
        !stack.is_empty()
            && stack.name() == Some("minecraft:shield")
            && !self.component_removed(stack, BLOCKS_ATTACKS)
    }

    pub(crate) fn stew_effects(&self, stack: &ItemStack) -> Vec<FoodEffect> {
        let Some(Nbt::List(List::Compound(entries))) = self.saved_component(stack, STEW_EFFECTS)
        else {
            return Vec::new();
        };
        entries
            .into_iter()
            .filter_map(|entry| {
                let Some(Nbt::String(name)) = entry.get("id") else {
                    return None;
                };
                let kind = EffectKind::ALL.into_iter().find(|kind| kind.id() == name)?;
                let duration = match entry.get("duration") {
                    Some(Nbt::Int(duration)) if *duration == -1 || *duration > 0 => *duration,
                    None => DEFAULT_STEW_DURATION,
                    _ => return None,
                };
                Some(FoodEffect {
                    kind,
                    duration,
                    amplifier: 0,
                })
            })
            .collect()
    }

    /// Adds the enchantment registry in the same numeric order sent to clients during Configuration.
    pub fn with_enchantments(mut self, names: &[&str]) -> Self {
        self.enchantments = names.iter().map(|name| (*name).to_owned()).collect();
        self
    }

    /// Adds the locally generated built-in potion registry in numeric order.
    pub fn with_potions(mut self, names: &[String]) -> Self {
        self.potions = names.to_vec();
        self
    }

    /// Adds the locally generated built-in mob effect registry in numeric order.
    pub fn with_mob_effects(mut self, names: &[String]) -> Self {
        self.mob_effects = names.to_vec();
        self.effect_ids = EffectKind::ALL.map(|kind| {
            names
                .iter()
                .position(|name| name == kind.id())
                .map(|id| id as i32)
        });
        self
    }

    pub(crate) fn effect_ids(&self) -> [Option<i32>; EffectKind::ALL.len()] {
        self.effect_ids
    }

    pub(crate) fn set_potion(&self, stack: &mut ItemStack, name: &str) -> io::Result<()> {
        let value = Nbt::Compound(Compound::from_iter([("potion", name)]));
        self.set_saved_component(stack, POTION_CONTENTS, &value)
    }

    pub(crate) fn set_saved_component(
        &self,
        stack: &mut ItemStack,
        name: &str,
        value: &Nbt,
    ) -> io::Result<()> {
        let &id = self
            .ids
            .get(name)
            .ok_or_else(|| invalid(format!("missing item component ID for {name}")))?;
        Self::set_component(stack, id, encode_component(name, value, self)?);
        self.resolve_stack_limit(stack)
    }

    pub(crate) fn component_removed(&self, stack: &ItemStack, name: &str) -> bool {
        self.ids
            .get(name)
            .is_some_and(|id| stack.components.removed.contains(id))
    }

    /// Applies one durability point to the held tool. Returns whether it broke.
    pub(crate) fn wear_tool(&self, stack: &mut ItemStack, default_max: i32) -> io::Result<bool> {
        self.wear_item(stack, default_max, 1)
    }

    /// Applies the requested durability loss. Returns whether an item broke.
    pub(crate) fn wear_item(
        &self,
        stack: &mut ItemStack,
        default_max: i32,
        amount: i32,
    ) -> io::Result<bool> {
        let Some(&damage_id) = self.ids.get(DAMAGE) else {
            return Ok(false);
        };
        let has_unbreakable = self.component_bytes(stack, UNBREAKABLE).is_some();
        if stack.is_empty()
            || has_unbreakable
            || self.component_removed(stack, DAMAGE)
            || self.component_removed(stack, MAX_DAMAGE)
        {
            return Ok(false);
        }
        let number = |name, default| {
            self.component_bytes(stack, name)
                .map_or(Ok(default), |mut bytes| read_varint(&mut bytes))
        };
        let maximum = number(MAX_DAMAGE, default_max)?;
        let next_damage = number(DAMAGE, 0)?.saturating_add(amount).min(maximum);
        let broken = next_damage >= maximum;
        if broken && stack.count == 1 {
            *stack = ItemStack::empty();
            return Ok(true);
        }
        let mut bytes = Vec::new();
        write_varint(&mut bytes, next_damage);
        let mut updated = stack.clone();
        Self::set_component(&mut updated, damage_id, bytes);
        self.resolve_stack_limit(&mut updated)?;
        updated.count -= i32::from(broken);
        *stack = updated;
        Ok(broken)
    }

    pub(crate) fn saved_component(&self, stack: &ItemStack, name: &str) -> Option<Nbt> {
        decode_component(name, self.component_bytes(stack, name)?, self).ok()
    }

    fn component_bytes<'a>(&self, stack: &'a ItemStack, name: &str) -> Option<&'a [u8]> {
        let id = self.ids.get(name)?;
        stack
            .components
            .added
            .iter()
            .find(|(kind, _)| kind == id)
            .map(|(_, bytes)| bytes.as_slice())
    }

    fn set_component(stack: &mut ItemStack, id: i32, bytes: Vec<u8>) {
        let patch = Arc::make_mut(&mut stack.components);
        match patch.added.binary_search_by_key(&id, |(id, _)| *id) {
            Ok(index) => patch.added[index].1 = bytes,
            Err(index) => patch.added.insert(index, (id, bytes)),
        }
        patch.removed.retain(|&removed| removed != id);
    }

    /// Reads a saved item, retaining fields outside the known item schema with the stack as it moves.
    pub fn read(&self, tag: &Compound) -> io::Result<ItemStack> {
        let Some(Nbt::String(name)) = tag.get("id") else {
            return Err(invalid("item has no string id"));
        };
        let item = items()
            .id(name)
            .or_else(|| items().id(&format!("minecraft:{name}")))
            .ok_or_else(|| invalid(format!("unknown item {name}")))?;
        let count = match tag.get("count") {
            None => 1,
            Some(Nbt::Int(count)) if (1..=99).contains(count) => *count,
            _ => return Err(invalid("item count must be an integer from 1 to 99")),
        };
        if item == 0 {
            return Ok(ItemStack::empty());
        }
        let mut patch = ComponentPatch::default();
        match tag.get("components") {
            None => {}
            Some(Nbt::Compound(components)) => {
                for (name, value) in components.iter() {
                    let (removed, name) = name
                        .strip_prefix('!')
                        .map_or((false, name), |name| (true, name));
                    let full = if name.contains(':') {
                        name.to_owned()
                    } else {
                        format!("minecraft:{name}")
                    };
                    let id = *self.ids.get(&full).ok_or_else(|| {
                        invalid(format!(
                            "unknown item component {name}; generate the built-in registries"
                        ))
                    })?;
                    if removed {
                        if !matches!(value, Nbt::Compound(c) if c.is_empty()) {
                            return Err(invalid("removed component must be an empty compound"));
                        }
                        patch.removed.push(id);
                    } else {
                        patch
                            .added
                            .push((id, encode_component(&full, value, self)?));
                    }
                }
            }
            _ => return Err(invalid("item components must be a compound")),
        }
        patch.added.sort_by_key(|(id, _)| *id);
        patch.removed.sort_unstable();
        if patch.added.iter().any(|(id, _)| patch.removed.contains(id)) {
            return Err(invalid("component is both added and removed"));
        }
        let mut extra = tag.clone();
        for key in ["id", "count", "components", "Slot"] {
            extra.remove(key);
        }
        if !extra.is_empty() {
            Nbt::Compound(extra).write_named("", &mut patch.preserved);
        }
        let mut stack = ItemStack {
            item,
            count,
            components: Arc::new(patch),
        };
        // Prove that the loaded network values can be saved before exposing an editable inventory.
        self.write(&stack)?;
        self.resolve_stack_limit(&mut stack)?;
        Ok(stack)
    }

    /// Writes an item in the vanilla item schema; runtime menu slots are assigned by the inventory serializer.
    pub fn write(&self, stack: &ItemStack) -> io::Result<Compound> {
        if stack.is_empty() {
            return Err(invalid("cannot save an empty item entry"));
        }
        if !(1..=99).contains(&stack.count) {
            return Err(invalid("item count outside 1..=99"));
        }
        let name = stack.name().ok_or_else(|| invalid("unknown item ID"))?;
        let mut tag = if stack.components.preserved.is_empty() {
            Compound::new()
        } else {
            let mut bytes = stack.components.preserved.as_slice();
            // These extra fields are file data, never sent to clients. Keep the Anvil file reader's
            // memory allowance instead of imposing the smaller network-NBT quota on unrelated fields.
            match Nbt::read_named_within(&mut bytes, 1024 * 1024 * 1024).map_err(invalid)? {
                (_, Nbt::Compound(tag)) if bytes.is_empty() => tag,
                _ => return Err(invalid("invalid preserved item NBT")),
            }
        };
        tag.insert("id", name);
        tag.insert("count", stack.count);
        let mut components = Compound::new();
        for (id, bytes) in &stack.components.added {
            let name = self.name(*id)?;
            if components.get(name).is_some() {
                return Err(invalid("duplicate component"));
            }
            components.insert(name, decode_component(name, bytes, self)?);
        }
        for id in &stack.components.removed {
            let name = self.name(*id)?;
            if components.get(name).is_some() {
                return Err(invalid("component is both added and removed"));
            }
            if components
                .insert(format!("!{name}"), Compound::new())
                .is_some()
            {
                return Err(invalid("duplicate removed component"));
            }
        }
        if !components.is_empty() {
            tag.insert("components", components);
        }
        Ok(tag)
    }

    /// Resolves the stack size of components that have passed codec validation.
    pub(crate) fn resolve_stack_limit(&self, stack: &mut ItemStack) -> io::Result<()> {
        let patch = &stack.components;
        let mut limit = if patch.added.is_empty() {
            None
        } else {
            items().max_stack_size(stack.item).map(i32::from)
        };
        if let Some(id) = self.ids.get("minecraft:max_stack_size") {
            if let Some((_, bytes)) = patch.added.iter().find(|(kind, _)| kind == id) {
                limit = Some(read_varint(&mut bytes.as_slice())?);
            } else if patch.removed.contains(id) {
                limit = Some(1);
            }
        }
        if patch.max_stack_size != limit {
            Arc::make_mut(&mut stack.components).max_stack_size = limit;
        }
        Ok(())
    }

    fn name(&self, id: i32) -> io::Result<&str> {
        usize::try_from(id)
            .ok()
            .and_then(|id| self.names.get(id))
            .filter(|name| !name.is_empty())
            .map(String::as_str)
            .ok_or_else(|| {
                invalid(format!(
                    "unknown component ID {id}; generate the built-in registries"
                ))
            })
    }
}

#[derive(Clone, Copy)]
enum Kind {
    VarInt,
    Int,
    Float,
    Bool,
    Unit,
    Nbt,
    MapDecorations,
    String,
    Lore,
    Rarity,
    Enchantable,
    Enchantments,
    PotionContents,
    StewEffects,
}

fn kind(name: &str) -> io::Result<Kind> {
    Ok(match name.strip_prefix("minecraft:").unwrap_or(name) {
        "max_stack_size"
        | "max_damage"
        | "damage"
        | "repair_cost"
        | "map_id"
        | "ominous_bottle_amplifier" => Kind::VarInt,
        "dyed_color" | "map_color" => Kind::Int,
        "potion_duration_scale" | "minimum_attack_charge" => Kind::Float,
        "enchantment_glint_override" => Kind::Bool,
        "unbreakable" | "glider" => Kind::Unit,
        "custom_data"
        | "custom_name"
        | "item_name"
        | "bucket_entity_data"
        | "intangible_projectile" => Kind::Nbt,
        "map_decorations" => Kind::MapDecorations,
        "item_model" | "tooltip_style" | "note_block_sound" => Kind::String,
        "lore" => Kind::Lore,
        "rarity" => Kind::Rarity,
        "enchantable" => Kind::Enchantable,
        "enchantments" | "stored_enchantments" => Kind::Enchantments,
        "potion_contents" => Kind::PotionContents,
        "suspicious_stew_effects" => Kind::StewEffects,
        _ => return Err(invalid(format!("unsupported saved item component {name}"))),
    })
}

fn integer(value: &Nbt) -> io::Result<i32> {
    match value {
        Nbt::Int(value) => Ok(*value),
        _ => Err(invalid("expected integer component")),
    }
}

fn encode_component(name: &str, value: &Nbt, codec: &ItemCodec) -> io::Result<Vec<u8>> {
    validate_component(name, value)?;
    let mut bytes = Vec::new();
    match kind(name)? {
        Kind::VarInt => write_varint(&mut bytes, integer(value)?),
        Kind::Int => bytes.extend_from_slice(&integer(value)?.to_be_bytes()),
        Kind::Float => match value {
            Nbt::Float(value) if value.is_finite() => bytes.extend_from_slice(&value.to_be_bytes()),
            _ => return Err(invalid("expected finite float component")),
        },
        Kind::Bool => match value {
            Nbt::Byte(value @ 0..=1) => bytes.push(*value as u8),
            _ => return Err(invalid("expected boolean component")),
        },
        Kind::Unit => {
            if !matches!(value, Nbt::Compound(c) if c.is_empty()) {
                return Err(invalid("expected empty component"));
            }
        }
        Kind::Nbt => value.write_network(&mut bytes),
        Kind::MapDecorations => {
            let Nbt::Compound(entries) = value else {
                unreachable!("validated map decorations")
            };
            // NBT compound key order is immaterial. Give equivalent markers the same stack bytes.
            let mut entries: Vec<_> = entries.iter().collect();
            entries.sort_unstable_by_key(|(id, _)| *id);
            let ordered = entries
                .into_iter()
                .map(|(id, marker)| {
                    let Nbt::Compound(marker) = marker else {
                        unreachable!("validated map marker")
                    };
                    let mut fields: Vec<_> = marker.iter().collect();
                    fields.sort_unstable_by_key(|(key, _)| *key);
                    let sorted = fields
                        .into_iter()
                        .map(|(key, value)| (key, value.clone()))
                        .collect();
                    (id, Nbt::Compound(sorted))
                })
                .collect();
            Nbt::Compound(ordered).write_network(&mut bytes);
        }
        Kind::String => match value {
            Nbt::String(value) => write_string(&mut bytes, value),
            _ => return Err(invalid("expected string component")),
        },
        Kind::Lore => match value {
            Nbt::List(List::Empty) => write_varint(&mut bytes, 0),
            Nbt::List(List::String(lines)) => {
                write_varint(&mut bytes, lines.len() as i32);
                for line in lines {
                    Nbt::from(line.as_str()).write_network(&mut bytes);
                }
            }
            Nbt::List(List::Compound(lines)) => {
                write_varint(&mut bytes, lines.len() as i32);
                for line in lines {
                    if line.len() == 1
                        && let Some(value) = line.get("")
                    {
                        value.write_network(&mut bytes);
                    } else {
                        Nbt::Compound(line.clone()).write_network(&mut bytes);
                    }
                }
            }
            _ => return Err(invalid("expected text component list")),
        },
        Kind::Rarity => match value {
            Nbt::String(value) => write_varint(
                &mut bytes,
                ["common", "uncommon", "rare", "epic"]
                    .iter()
                    .position(|&v| v == value)
                    .ok_or_else(|| invalid("unknown rarity"))? as i32,
            ),
            _ => return Err(invalid("expected rarity name")),
        },
        Kind::Enchantments => {
            let Nbt::Compound(levels) = value else {
                return Err(invalid("expected enchantment map"));
            };
            write_varint(&mut bytes, levels.len() as i32);
            for (name, level) in levels.iter() {
                let full = if name.contains(':') {
                    name.to_owned()
                } else {
                    format!("minecraft:{name}")
                };
                let id = codec
                    .enchantments
                    .iter()
                    .position(|name| *name == full)
                    .ok_or_else(|| invalid(format!("unknown enchantment {name}")))?;
                let level = integer(level)?;
                if !(0..=255).contains(&level) {
                    return Err(invalid("enchantment level outside 0..=255"));
                }
                write_varint(&mut bytes, id as i32);
                write_varint(&mut bytes, level);
            }
        }
        Kind::Enchantable => match value {
            Nbt::Compound(value) => write_varint(
                &mut bytes,
                integer(
                    value
                        .get("value")
                        .ok_or_else(|| invalid("missing enchantable value"))?,
                )?,
            ),
            _ => return Err(invalid("expected enchantable compound")),
        },
        Kind::PotionContents => encode_potion_contents(&mut bytes, value, &codec.potions)?,
        Kind::StewEffects => encode_stew_effects(&mut bytes, value, &codec.mob_effects)?,
    }
    Ok(bytes)
}

fn decode_component(name: &str, mut bytes: &[u8], codec: &ItemCodec) -> io::Result<Nbt> {
    let value = match kind(name)? {
        Kind::VarInt => Nbt::Int(read_varint(&mut bytes)?),
        Kind::Int => Nbt::Int(i32::from_be_bytes(take(&mut bytes)?)),
        Kind::Float => {
            let value = f32::from_be_bytes(take(&mut bytes)?);
            if !value.is_finite() {
                return Err(invalid("non-finite component"));
            }
            Nbt::Float(value)
        }
        Kind::Bool => Nbt::Byte(i8::from(read_bool(&mut bytes)?)),
        Kind::Unit => Nbt::Compound(Compound::new()),
        Kind::Nbt | Kind::MapDecorations => Nbt::read_network(&mut bytes).map_err(invalid)?,
        Kind::String => Nbt::String(read_string(&mut bytes)?),
        Kind::Lore => {
            let size = usize::try_from(read_varint(&mut bytes)?).map_err(invalid)?;
            if size > 256 {
                return Err(invalid("too many lore lines"));
            }
            let lines = (0..size)
                .map(|_| Nbt::read_network(&mut bytes).map_err(invalid))
                .collect::<io::Result<Vec<_>>>()?;
            if lines.iter().all(|line| matches!(line, Nbt::String(_))) {
                let value = Nbt::List(if lines.is_empty() {
                    List::Empty
                } else {
                    List::String(
                        lines
                            .into_iter()
                            .map(|line| match line {
                                Nbt::String(line) => line,
                                _ => unreachable!(),
                            })
                            .collect(),
                    )
                });
                if !bytes.is_empty() {
                    return Err(invalid("trailing lore bytes"));
                }
                return Ok(value);
            }
            // NbtOps wraps primitive values under an empty key when a list contains mixed tag kinds.
            // Unwrap these again on the network; they are not text objects with a "text" field.
            Nbt::List(List::Compound(
                lines
                    .into_iter()
                    .map(|line| match line {
                        Nbt::Compound(value) => Ok(value),
                        Nbt::String(value) => {
                            Ok(Compound::from_iter([("".to_owned(), Nbt::String(value))]))
                        }
                        _ => Err(invalid("invalid lore text")),
                    })
                    .collect::<io::Result<_>>()?,
            ))
        }
        Kind::Rarity => Nbt::from(
            *["common", "uncommon", "rare", "epic"]
                .get(read_varint(&mut bytes)? as usize)
                .ok_or_else(|| invalid("unknown rarity"))?,
        ),
        Kind::Enchantments => {
            let size = usize::try_from(read_varint(&mut bytes)?).map_err(invalid)?;
            if size > bytes.len() / 2 {
                return Err(invalid("truncated enchantment map"));
            }
            let mut levels = Compound::new();
            for _ in 0..size {
                let id = usize::try_from(read_varint(&mut bytes)?).map_err(invalid)?;
                let name = codec
                    .enchantments
                    .get(id)
                    .ok_or_else(|| invalid("unknown enchantment ID"))?;
                let level = read_varint(&mut bytes)?;
                if !(0..=255).contains(&level) || levels.insert(name, level).is_some() {
                    return Err(invalid("invalid or duplicate enchantment"));
                }
            }
            Nbt::Compound(levels)
        }
        Kind::Enchantable => Nbt::Compound(Compound::from_iter([(
            "value".to_owned(),
            Nbt::Int(read_varint(&mut bytes)?),
        )])),
        Kind::PotionContents => decode_potion_contents(&mut bytes, &codec.potions)?,
        Kind::StewEffects => decode_stew_effects(&mut bytes, &codec.mob_effects)?,
    };
    if !bytes.is_empty() {
        return Err(invalid(format!("trailing bytes in component {name}")));
    }
    validate_component(name, &value)?;
    Ok(value)
}

fn encode_potion_contents(bytes: &mut Vec<u8>, value: &Nbt, potions: &[String]) -> io::Result<()> {
    let Nbt::Compound(contents) = value else {
        return Err(invalid("expected potion contents compound"));
    };
    let (mut potion, mut color, mut name) = (None, None, None);
    for (field, value) in contents.iter() {
        match (field, value) {
            ("potion", Nbt::String(value)) => {
                let id = potions
                    .iter()
                    .position(|name| {
                        name == value || name.strip_prefix("minecraft:") == Some(value)
                    })
                    .ok_or_else(|| invalid(format!("unknown potion {value}")))?;
                potion = Some(id as i32);
            }
            ("custom_color", Nbt::Int(value)) => color = Some(*value),
            ("custom_name", Nbt::String(value)) => name = Some(value),
            ("custom_effects", Nbt::List(effects)) if effects.is_empty() => {}
            ("custom_effects", _) => return Err(invalid("custom potion effects are unsupported")),
            _ => return Err(invalid(format!("invalid potion contents field {field}"))),
        }
    }
    bytes.push(u8::from(potion.is_some()));
    if let Some(id) = potion {
        write_varint(bytes, id);
    }
    bytes.push(u8::from(color.is_some()));
    if let Some(color) = color {
        bytes.extend_from_slice(&color.to_be_bytes());
    }
    write_varint(bytes, 0); // Custom effect count.
    bytes.push(u8::from(name.is_some()));
    if let Some(name) = name {
        write_string(bytes, name);
    }
    Ok(())
}

fn decode_potion_contents(bytes: &mut &[u8], potions: &[String]) -> io::Result<Nbt> {
    let mut contents = Compound::new();
    if read_bool(bytes)? {
        let id = usize::try_from(read_varint(bytes)?).map_err(invalid)?;
        let potion = potions
            .get(id)
            .ok_or_else(|| invalid("unknown potion ID"))?;
        contents.insert("potion", potion.as_str());
    }
    if read_bool(bytes)? {
        contents.insert("custom_color", i32::from_be_bytes(take(bytes)?));
    }
    if read_varint(bytes)? != 0 {
        return Err(invalid("custom potion effects are unsupported"));
    }
    if read_bool(bytes)? {
        contents.insert("custom_name", read_string(bytes)?);
    }
    Ok(Nbt::Compound(contents))
}

fn encode_stew_effects(bytes: &mut Vec<u8>, value: &Nbt, effects: &[String]) -> io::Result<()> {
    let entries = match value {
        Nbt::List(List::Empty) => &[][..],
        Nbt::List(List::Compound(entries)) => entries.as_slice(),
        _ => return Err(invalid("expected a list of stew effects")),
    };
    write_varint(bytes, entries.len() as i32);
    for entry in entries {
        let Some(Nbt::String(name)) = entry.get("id") else {
            return Err(invalid("stew effect has no string id"));
        };
        let id = effects
            .iter()
            .position(|effect| effect == name || effect.strip_prefix("minecraft:") == Some(name))
            .ok_or_else(|| invalid(format!("unknown mob effect {name}")))?;
        let duration = entry.get("duration");
        if entry.len() != 1 + usize::from(duration.is_some()) {
            return Err(invalid("unknown stew effect field"));
        }
        write_varint(bytes, id as i32);
        write_varint(bytes, duration.map_or(Ok(DEFAULT_STEW_DURATION), integer)?);
    }
    Ok(())
}

fn decode_stew_effects(bytes: &mut &[u8], effects: &[String]) -> io::Result<Nbt> {
    let count = usize::try_from(read_varint(bytes)?).map_err(invalid)?;
    if count == 0 {
        return Ok(Nbt::List(List::Empty));
    }
    if count > bytes.len() / 2 {
        return Err(invalid("truncated stew effects"));
    }
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let id = usize::try_from(read_varint(bytes)?).map_err(invalid)?;
        let effect = effects
            .get(id)
            .ok_or_else(|| invalid("unknown mob effect ID"))?;
        let mut entry = Compound::from_iter([("id", effect.as_str())]);
        let duration = read_varint(bytes)?;
        if duration != DEFAULT_STEW_DURATION {
            entry.insert("duration", duration);
        }
        entries.push(entry);
    }
    Ok(Nbt::List(List::Compound(entries)))
}

fn validate_component(name: &str, value: &Nbt) -> io::Result<()> {
    let short = name.strip_prefix("minecraft:").unwrap_or(name);
    let valid = match (short, value) {
        ("max_stack_size", Nbt::Int(value)) => (1..=99).contains(value),
        ("max_damage", Nbt::Int(value)) => *value > 0,
        ("damage" | "repair_cost" | "map_id", Nbt::Int(value)) => *value >= 0,
        ("ominous_bottle_amplifier", Nbt::Int(value)) => (0..=4).contains(value),
        ("potion_duration_scale", Nbt::Float(value)) => value.is_finite() && *value >= 0.0,
        ("minimum_attack_charge", Nbt::Float(value)) => {
            value.is_finite() && (0.0..=1.0).contains(value)
        }
        ("intangible_projectile", value) => matches!(value, Nbt::Compound(c) if c.is_empty()),
        ("custom_data" | "bucket_entity_data", value) => {
            matches!(value, Nbt::Compound(_))
        }
        ("map_decorations", value) => matches!(value, Nbt::Compound(decorations)
            if decorations.iter().all(|(_, marker)| matches!(marker, Nbt::Compound(entry)
                if matches!(entry.get("type"), Some(Nbt::String(_)))
                    && matches!(entry.get("x"), Some(Nbt::Double(x)) if x.is_finite())
                    && matches!(entry.get("z"), Some(Nbt::Double(z)) if z.is_finite())
                    && matches!(entry.get("rotation"), Some(Nbt::Float(r)) if r.is_finite())))),
        ("custom_name" | "item_name", value) => {
            matches!(value, Nbt::String(_) | Nbt::Compound(_) | Nbt::List(_))
        }
        ("enchantable", Nbt::Compound(value)) => {
            matches!(value.get("value"), Some(Nbt::Int(value)) if *value > 0)
        }
        ("item_model" | "tooltip_style" | "note_block_sound", Nbt::String(value)) => {
            let (namespace, path) = value.split_once(':').unwrap_or(("minecraft", value));
            !namespace.is_empty()
                && !path.is_empty()
                && value.len() <= 32767
                && namespace
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
                && path
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-/".contains(&b))
        }
        _ => true,
    };
    if valid {
        Ok(())
    } else {
        Err(invalid(format!("invalid value for {name}")))
    }
}

fn write_varint(bytes: &mut Vec<u8>, value: i32) {
    let mut value = value as u32;
    loop {
        let byte = (value & 127) as u8;
        value >>= 7;
        bytes.push(byte | if value == 0 { 0 } else { 128 });
        if value == 0 {
            break;
        }
    }
}

fn write_string(bytes: &mut Vec<u8>, value: &str) {
    write_varint(bytes, value.len() as i32);
    bytes.extend_from_slice(value.as_bytes());
}

fn read_string(bytes: &mut &[u8]) -> io::Result<String> {
    let size = usize::try_from(read_varint(bytes)?).map_err(invalid)?;
    let (value, rest) = bytes
        .split_at_checked(size)
        .ok_or_else(|| invalid("truncated string"))?;
    let value = std::str::from_utf8(value).map_err(invalid)?.to_owned();
    *bytes = rest;
    Ok(value)
}

fn read_bool(bytes: &mut &[u8]) -> io::Result<bool> {
    match take(bytes)? {
        [0] => Ok(false),
        [1] => Ok(true),
        _ => Err(invalid("invalid boolean component")),
    }
}

fn read_varint(bytes: &mut &[u8]) -> io::Result<i32> {
    let mut value = 0u32;
    for index in 0..5 {
        let [byte] = take(bytes)?;
        if index == 4 && byte & 0xf0 != 0 {
            return Err(invalid("component VarInt overflow"));
        }
        value |= u32::from(byte & 127) << (index * 7);
        if byte & 128 == 0 {
            return Ok(value as i32);
        }
    }
    Err(invalid("component VarInt overflow"))
}

fn take<const N: usize>(bytes: &mut &[u8]) -> io::Result<[u8; N]> {
    let result = bytes
        .get(..N)
        .ok_or_else(|| invalid("truncated component"))?
        .try_into()
        .expect("checked length");
    *bytes = &bytes[N..];
    Ok(result)
}

pub(crate) fn invalid(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mining_wear_respects_saved_durability_overrides() {
        let codec = ItemCodec::default();
        let tool = |components| {
            codec
                .read(&Compound::from_iter([
                    ("id", Nbt::from("minecraft:wooden_pickaxe")),
                    ("components", Nbt::Compound(components)),
                ]))
                .unwrap()
        };
        let mut short_lived = tool(Compound::from_iter([("minecraft:max_damage", 2)]));
        assert!(!codec.wear_tool(&mut short_lived, 59).unwrap());
        assert_eq!(
            short_lived,
            tool(Compound::from_iter([
                ("minecraft:max_damage", 2),
                ("minecraft:damage", 1)
            ]))
        );
        assert!(codec.wear_tool(&mut short_lived, 59).unwrap());
        assert!(short_lived.is_empty());

        for component in [
            "minecraft:unbreakable",
            "!minecraft:damage",
            "!minecraft:max_damage",
        ] {
            let original = tool(Compound::from_iter([(component, Compound::new())]));
            let mut protected = original.clone();
            assert!(!codec.wear_tool(&mut protected, 59).unwrap());
            assert_eq!(protected, original);
        }
    }

    #[test]
    fn components_and_removals_survive_moves_count_changes_and_save_load() {
        let codec = ItemCodec::new(&[
            "minecraft:damage".to_owned(),
            "minecraft:custom_data".to_owned(),
            "minecraft:unbreakable".to_owned(),
        ]);
        let mut original = Compound::from_iter([
            ("id".to_owned(), Nbt::from("minecraft:diamond_pickaxe")),
            ("count".to_owned(), Nbt::Int(1)),
            ("future".to_owned(), Nbt::Long(123)),
            (
                "components".to_owned(),
                Nbt::Compound(Compound::from_iter([
                    ("minecraft:damage".to_owned(), Nbt::Int(73)),
                    (
                        "minecraft:custom_data".to_owned(),
                        Nbt::Compound(Compound::from_iter([(
                            "key".to_owned(),
                            Nbt::from("value"),
                        )])),
                    ),
                    (
                        "!minecraft:unbreakable".to_owned(),
                        Nbt::Compound(Compound::new()),
                    ),
                ])),
            ),
        ]);
        let mut stack = codec.read(&original).unwrap();
        assert_eq!(stack.components.added[0], (0, vec![73]));
        stack.count = 2;
        original.insert("count", 2);
        assert_eq!(codec.write(&stack).unwrap(), original);
    }

    #[test]
    fn unsupported_or_malformed_components_are_errors_in_both_directions() {
        let codec = ItemCodec::new(&[
            "minecraft:damage".to_owned(),
            "minecraft:unknown_future_component".to_owned(),
        ]);
        let mut tag = Compound::from_iter([("id".to_owned(), Nbt::from("minecraft:stone"))]);
        tag.insert(
            "components",
            Compound::from_iter([("minecraft:unknown_future_component".to_owned(), Nbt::Int(1))]),
        );
        assert!(codec.read(&tag).is_err());
        let mut stack = ItemStack {
            item: 1,
            count: 1,
            components: Arc::new(ComponentPatch {
                added: vec![(1, vec![1])],
                ..Default::default()
            }),
        };
        assert!(codec.write(&stack).is_err());
        Arc::make_mut(&mut stack.components).added = vec![(0, vec![1, 2])];
        assert!(
            codec.write(&stack).is_err(),
            "a component's trailing bytes must not be dropped"
        );
    }

    #[test]
    fn potion_contents_preserve_the_stack_limit_and_reject_custom_effects() {
        let codec = ItemCodec::new(&[
            POTION_CONTENTS.to_owned(),
            "minecraft:max_stack_size".to_owned(),
        ])
        .with_potions(&["minecraft:test_potion".to_owned()]);
        let tag = Compound::from_iter([("id", "minecraft:potion")]);
        let mut potion = codec.read(&tag).unwrap();
        codec.set_potion(&mut potion, "test_potion").unwrap();
        assert_eq!(potion.max_stack_size(), Some(1));

        let mut inventory = crate::inventory::Inventory::default();
        for _ in 0..2 {
            assert!(inventory.add(&mut potion.clone(), false));
        }
        assert_eq!(
            inventory.slots[crate::inventory::HOTBAR..][..2],
            [potion.clone(), potion.clone()]
        );
        let mut saved = codec.write(&potion).unwrap();
        assert_eq!(codec.read(&saved).unwrap(), potion);

        let mut components = Compound::from_iter([("minecraft:max_stack_size", Nbt::Int(16))]);
        let mut contents = Compound::from_iter([("potion", "minecraft:test_potion")]);
        components.insert(POTION_CONTENTS, contents.clone());
        saved.insert("components", components.clone());
        assert_eq!(codec.read(&saved).unwrap().max_stack_size(), Some(16));

        contents.insert("custom_effects", List::Compound(vec![Compound::new()]));
        components.insert(POTION_CONTENTS, contents);
        saved.insert("components", components);
        assert!(codec.read(&saved).is_err());
        Arc::make_mut(&mut potion.components).added[0].1 = vec![0, 0, 1, 0];
        assert!(codec.write(&potion).is_err());
    }

    #[test]
    fn only_supported_potion_contents_can_be_drunk() {
        let codec = ItemCodec::new(&[POTION_CONTENTS.to_owned(), USE_REMAINDER.to_owned()])
            .with_potions(&[
                "minecraft:regeneration".to_owned(),
                "minecraft:water_breathing".to_owned(),
            ])
            .with_consumables(HashMap::from([(
                items().id("minecraft:potion").unwrap(),
                Consumable {
                    food: None,
                    use_ticks: 32,
                    remainder: Some(ItemStack {
                        item: items().id("minecraft:glass_bottle").unwrap(),
                        count: 1,
                        ..ItemStack::empty()
                    }),
                    effect: None,
                },
            )]));
        let mut stack = codec
            .read(&Compound::from_iter([("id", "minecraft:potion")]))
            .unwrap();
        assert!(codec.consumable(&stack).is_none());
        codec.set_potion(&mut stack, "water_breathing").unwrap();
        assert!(codec.consumable(&stack).is_none());
        codec.set_potion(&mut stack, "regeneration").unwrap();
        assert!(codec.consumable(&stack).unwrap().remainder.is_some());
        Arc::make_mut(&mut stack.components).removed.push(1);
        let drink = codec.consumable(&stack).unwrap();
        assert!(drink.remainder.is_none());
        assert!(matches!(drink.effect, Some(ConsumeEffect::ApplyPotion(_))));
    }

    #[test]
    fn saved_stew_can_contain_multiple_effects_and_default_duration() {
        let codec = ItemCodec::new(&[STEW_EFFECTS.to_owned()]).with_mob_effects(&[
            "minecraft:regeneration".to_owned(),
            "minecraft:glowing".to_owned(),
            "minecraft:poison".to_owned(),
        ]);
        let effects = List::Compound(vec![
            Compound::from_iter([("id", "minecraft:regeneration")]),
            Compound::from_iter([("id", "minecraft:glowing")]),
            Compound::from_iter([
                ("id", Nbt::from("minecraft:poison")),
                ("duration", Nbt::Int(5)),
            ]),
        ]);
        let mut tag = Compound::from_iter([
            ("id", Nbt::from("minecraft:suspicious_stew")),
            ("count", Nbt::Int(1)),
        ]);
        tag.insert("components", Compound::from_iter([(STEW_EFFECTS, effects)]));
        let stack = codec.read(&tag).unwrap();
        assert_eq!(
            stack.components.added,
            [(0, vec![3, 0, 160, 1, 1, 160, 1, 2, 5])]
        );
        assert_eq!(codec.write(&stack).unwrap(), tag);
        assert_eq!(
            codec
                .stew_effects(&stack)
                .into_iter()
                .map(|effect| (effect.kind, effect.duration, effect.amplifier))
                .collect::<Vec<_>>(),
            [
                (EffectKind::Regeneration, 160, 0),
                (EffectKind::Poison, 5, 0)
            ]
        );
        tag.insert(
            "components",
            Compound::from_iter([(format!("!{STEW_EFFECTS}"), Compound::new())]),
        );
        assert!(codec.stew_effects(&codec.read(&tag).unwrap()).is_empty());
    }
}

#[cfg(test)]
mod preservation_tests {
    use super::*;

    #[test]
    fn unknown_file_fields_do_not_inherit_the_network_nbt_quota() {
        let tag = Compound::from_iter([
            ("id", Nbt::from("minecraft:stone")),
            ("count", Nbt::Int(1)),
            (
                "future_file_field",
                Nbt::ByteArray(vec![7; harpas_core::nbt::NETWORK_QUOTA + 1]),
            ),
        ]);
        let codec = ItemCodec::default();
        let item = codec.read(&tag).unwrap();
        assert_eq!(codec.write(&item).unwrap(), tag);
        assert!(
            item.components.added.is_empty(),
            "file-only fields are not sent to clients"
        );
    }
}
