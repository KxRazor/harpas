//! Loot tables from the vanilla data pack, evaluated for broken blocks, chests and animal kills.
//!
//! A table rolls each pool whose conditions pass, as many times as its `rolls`, and each roll picks one of the
//! entries that expand, by weight; then the entry's, the pool's and the table's functions apply, and stacks over
//! their item's size are split. The tables' rolls draw from the world's random sequence named by the table
//! (`random_sequence`, `RandomSequences`), which the world seed seeds.
//!
//! Supported: `item`, `alternatives`, `group`, `sequence` and `empty` entries; the conditions
//! `survives_explosion`, `block_state_property`, `match_tool` (by item, the tool's data components not being
//! decoded yet), `random_chance`, `table_bonus`, `any_of`, `all_of` and `inverted`; the functions `set_count`,
//! `limit_count`, `apply_bonus` and `explosion_decay`; constant, uniform and binomial numbers. Chest loot
//! supports `enchant_randomly` with the vanilla loot enchantment tag, `set_potion` and `set_stew_effect`.
//! Block tool enchantments are read as none. Mob loot supports Looting with `enchanted_count_increase`.
//! Not done yet: `location_check` and
//! `entity_properties` conditions (read as false) and `dynamic` entries (which add nothing), and the functions
//! that copy data into the item (`copy_components`, `copy_state`), which drop the item without it.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek};
use std::path::Path;

use harpas_core::block::block_states;
use harpas_core::item::items;
use harpas_core::math::round_f32;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_core::random::RandomSource;
use log::warn;
use serde_json::Value;

use crate::inventory::ItemStack;
use crate::item_data::{ItemCodec, STEW_EFFECTS, invalid};

pub(crate) mod chest;

const MAX_WORK: usize = 16_384;
const MAX_STACKS: usize = 4_096;

fn check_structure(value: &Value, depth: usize, remaining: &mut usize) -> io::Result<()> {
    if depth > 64 || *remaining == 0 {
        return Err(invalid("loot table is too large or deeply nested"));
    }
    *remaining -= 1;
    match value {
        Value::Array(values) => values
            .iter()
            .try_for_each(|value| check_structure(value, depth + 1, remaining)),
        Value::Object(fields) => fields
            .values()
            .try_for_each(|value| check_structure(value, depth + 1, remaining)),
        _ => Ok(()),
    }
}

fn required<'a>(value: &'a Value, key: &str) -> io::Result<&'a Value> {
    value
        .get(key)
        .ok_or_else(|| invalid(format!("missing loot field {key}")))
}

fn string<'a>(value: &'a Value, key: &str) -> io::Result<&'a str> {
    required(value, key)?
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid(format!("loot field {key} must be a nonempty string")))
}

fn array(value: &Value) -> io::Result<&[Value]> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| invalid("expected a loot array"))
}

fn list<'a>(value: &'a Value, key: &str) -> io::Result<&'a [Value]> {
    value.get(key).map_or(Ok(&[]), array)
}

fn integer(value: &Value) -> io::Result<i32> {
    value
        .as_f64()
        .filter(|value| {
            value.fract() == 0.0 && (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(value)
        })
        .map(|value| value as i32)
        .ok_or_else(|| invalid("expected a 32-bit loot integer"))
}

fn probability(value: f32) -> io::Result<f32> {
    if !(0.0..=1.0).contains(&value) {
        return Err(invalid("loot probability must be between 0 and 1"));
    }
    Ok(value)
}

fn finite(value: &Value) -> io::Result<f32> {
    value
        .as_f64()
        .map(|value| value as f32)
        .filter(|value| value.is_finite())
        .ok_or_else(|| invalid("expected a finite loot number"))
}

fn kind(id: &str) -> &str {
    id.strip_prefix("minecraft:").unwrap_or(id)
}

fn item_id(name: &str) -> Option<u32> {
    if name.contains(':') {
        items().id(name)
    } else {
        items().id(&format!("minecraft:{name}"))
    }
}

/// A number provider (`NumberProvider`).
#[derive(Debug, Clone, PartialEq)]
enum Number {
    Constant(f32),
    Uniform(Box<Number>, Box<Number>),
    Binomial(Box<Number>, Box<Number>),
}

impl Number {
    fn parse(value: &Value) -> io::Result<Self> {
        if value.is_number() {
            return Ok(Self::Constant(finite(value)?));
        }
        let field = |name| Self::parse(required(value, name)?).map(Box::new);
        Ok(match kind(string(value, "type")?) {
            "constant" => Self::Constant(finite(required(value, "value")?)?),
            "uniform" => Self::Uniform(field("min")?, field("max")?),
            "binomial" => {
                let p = field("p")?;
                p.validate_probability()?;
                Self::Binomial(field("n")?, p)
            }
            other => return Err(invalid(format!("unsupported loot number provider {other}"))),
        })
    }

    fn validate_probability(&self) -> io::Result<()> {
        if let Self::Constant(value) = self {
            probability(*value)?;
        }
        Ok(())
    }

    /// `getFloat`.
    fn float(&self, context: &mut Context<'_>) -> io::Result<f32> {
        context.charge(1)?;
        Ok(match self {
            Self::Constant(n) => *n,
            Self::Uniform(min, max) => {
                let (min, max) = (min.float(context)?, max.float(context)?);
                // `Mth.nextFloat`.
                if min >= max {
                    min
                } else {
                    let width = max - min;
                    if !width.is_finite() {
                        return Err(invalid("loot number range is too wide"));
                    }
                    context.random.next_float() * width + min
                }
            }
            Self::Binomial(..) => self.int(context)? as f32,
        })
    }

    /// `getInt`.
    fn int(&self, context: &mut Context<'_>) -> io::Result<i32> {
        context.charge(1)?;
        Ok(match self {
            Self::Constant(n) => round_f32(*n),
            Self::Uniform(min, max) => {
                let (min, max) = (min.int(context)?, max.int(context)?);
                // `Mth.nextInt`.
                if min >= max {
                    min
                } else {
                    let width = i32::try_from(i64::from(max) - i64::from(min) + 1)
                        .map_err(|_| invalid("loot number range is too wide"))?;
                    context.random.next_int_bounded(width) + min
                }
            }
            Self::Binomial(n, p) => {
                let n = n.int(context)?.max(0);
                let p = probability(p.float(context)?)?;
                context.charge(n as usize)?;
                (0..n).filter(|_| context.random.next_float() < p).count() as i32
            }
        })
    }
}

/// A block state property's expected value: exact, or an integer range.
#[derive(Debug, Clone, PartialEq)]
enum PropertyValue {
    Exact(String),
    Range(Option<i64>, Option<i64>),
}

/// A loot condition (`LootItemCondition`).
#[derive(Debug, Clone, PartialEq)]
enum Condition {
    /// Always true without an explosion (`survives_explosion`).
    SurvivesExplosion,
    BlockState {
        block: String,
        properties: Vec<(String, PropertyValue)>,
    },
    /// The tool is one of these items; a predicate on the tool's components never matches yet.
    MatchTool {
        items: Option<HashSet<String>>,
        components: bool,
    },
    RandomChance(Number),
    /// The chance for no enchantment level, as enchantments are not decoded yet.
    TableBonus(f32),
    AnyOf(Vec<Condition>),
    AllOf(Vec<Condition>),
    Inverted(Box<Condition>),
    /// A condition not ported yet, read as false.
    Unsupported,
}

impl Condition {
    fn parse(value: &Value) -> io::Result<Self> {
        let name = kind(string(value, "condition")?);
        Ok(match name {
            "survives_explosion" => Self::SurvivesExplosion,
            "block_state_property" => {
                let mut properties = Vec::new();
                if let Some(entries) = value.get("properties") {
                    let entries = entries
                        .as_object()
                        .ok_or_else(|| invalid("invalid loot block properties"))?;
                    for (key, expected) in entries {
                        let expected = match expected {
                            Value::String(s) => PropertyValue::Exact(s.clone()),
                            Value::Bool(_) | Value::Number(_) => {
                                PropertyValue::Exact(expected.to_string())
                            }
                            Value::Object(range) => {
                                let bound = |key| {
                                    range
                                        .get(key)
                                        .map(|v: &Value| {
                                            v.as_i64()
                                                .or_else(|| v.as_str()?.parse().ok())
                                                .ok_or_else(|| {
                                                    invalid("invalid loot property bound")
                                                })
                                        })
                                        .transpose()
                                };
                                let (min, max) = (bound("min")?, bound("max")?);
                                if matches!((min, max), (Some(min), Some(max)) if min > max) {
                                    return Err(invalid("inverted loot property range"));
                                }
                                PropertyValue::Range(min, max)
                            }
                            _ => return Err(invalid("invalid loot property")),
                        };
                        properties.push((key.clone(), expected));
                    }
                }
                Self::BlockState {
                    block: string(value, "block")?.to_owned(),
                    properties,
                }
            }
            "match_tool" => {
                let Some(predicate) = value.get("predicate") else {
                    return Ok(Self::MatchTool {
                        items: None,
                        components: false,
                    });
                };
                let predicate = predicate
                    .as_object()
                    .ok_or_else(|| invalid("invalid loot tool predicate"))?;
                let items = predicate
                    .get("items")
                    .map(|items| {
                        if let Some(item) = items.as_str() {
                            return Ok(HashSet::from([item.to_owned()]));
                        }
                        array(items)?
                            .iter()
                            .map(|item| {
                                item.as_str()
                                    .map(str::to_owned)
                                    .ok_or_else(|| invalid("invalid loot tool item"))
                            })
                            .collect::<io::Result<HashSet<_>>>()
                    })
                    .transpose()?;
                let components =
                    predicate.contains_key("predicates") || predicate.contains_key("components");
                Self::MatchTool { items, components }
            }
            "random_chance" => {
                let chance = Number::parse(required(value, "chance")?)?;
                chance.validate_probability()?;
                Self::RandomChance(chance)
            }
            "table_bonus" => {
                let chances = array(required(value, "chances")?)?;
                let mut chances = chances
                    .iter()
                    .map(|chance| finite(chance).and_then(probability));
                let first = chances
                    .next()
                    .ok_or_else(|| invalid("empty loot probability table"))??;
                for chance in chances {
                    chance?;
                }
                Self::TableBonus(first)
            }
            "any_of" | "all_of" => {
                let terms = array(required(value, "terms")?)?
                    .iter()
                    .map(Self::parse)
                    .collect::<io::Result<_>>()?;
                if name == "any_of" {
                    Self::AnyOf(terms)
                } else {
                    Self::AllOf(terms)
                }
            }
            "inverted" => Self::Inverted(Box::new(Self::parse(required(value, "term")?)?)),
            _ => Self::Unsupported,
        })
    }

    fn test(&self, context: &mut Context<'_>) -> io::Result<bool> {
        context.charge(1)?;
        Ok(match self {
            Self::SurvivesExplosion => true,
            Self::BlockState { block, properties } => {
                let Some((name, values)) = context
                    .state
                    .and_then(|state| block_states().properties(state))
                else {
                    return Ok(false);
                };
                name == block
                    && properties.iter().all(|(key, expected)| {
                        let Some(&(_, value)) = values.iter().find(|(k, _)| k == key) else {
                            return false;
                        };
                        match expected {
                            PropertyValue::Exact(e) => e == value,
                            PropertyValue::Range(min, max) => value.parse::<i64>().is_ok_and(|n| {
                                min.is_none_or(|min| n >= min) && max.is_none_or(|max| n <= max)
                            }),
                        }
                    })
            }
            Self::MatchTool { items, components } => {
                if *components {
                    return Ok(false);
                }
                match items {
                    Some(items) => context
                        .tool
                        .name()
                        .is_some_and(|name| !context.tool.is_empty() && items.contains(name)),
                    None => true,
                }
            }
            Self::RandomChance(chance) => {
                let chance = probability(chance.float(context)?)?;
                context.random.next_float() < chance
            }
            Self::TableBonus(chance) => context.random.next_float() < *chance,
            Self::AnyOf(terms) => {
                for term in terms {
                    if term.test(context)? {
                        return Ok(true);
                    }
                }
                false
            }
            Self::AllOf(terms) => all(terms, context)?,
            Self::Inverted(term) => !term.test(context)?,
            Self::Unsupported => false,
        })
    }
}

fn all(conditions: &[Condition], context: &mut Context<'_>) -> io::Result<bool> {
    for condition in conditions {
        if !condition.test(context)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// A loot function (`LootItemFunction`), with its own conditions.
#[derive(Debug, Clone, PartialEq)]
enum Function {
    SetCount {
        count: Number,
        add: bool,
    },
    LimitCount {
        min: Option<Number>,
        max: Option<Number>,
    },
    ExactCount(i32),
    LootingBonus {
        count: Number,
        limit: i32,
    },
    /// `apply_bonus` with no enchantment level: the binomial formula still rolls its extra chances.
    BinomialBonus {
        extra: i32,
        probability: f32,
    },
    /// `apply_bonus` that changes nothing without a level, though `uniform_bonus_count` still draws.
    UniformBonus,
    /// An enchantment from the `on_random_loot` tag, filtered by the item.
    EnchantRandomly,
    SetPotion(String),
    SetStewEffect(Vec<StewEffect>),
    TreasureMap,
    SetTranslatedItemName(String),
    /// A function that changes nothing for a broken block without an explosion or the data it copies.
    None,
}

#[derive(Debug, Clone, PartialEq)]
struct StewEffect {
    name: String,
    duration: Number,
}

impl StewEffect {
    fn parse(value: &Value) -> io::Result<Self> {
        Ok(Self {
            name: string(value, "type")?.to_owned(),
            duration: Number::parse(required(value, "duration")?)?,
        })
    }

    fn roll(&self, context: &mut Context<'_>) -> io::Result<Nbt> {
        let duration = self.duration.int(context)?;
        let name = self.name.strip_prefix("minecraft:").unwrap_or(&self.name);
        // Vanilla's instant effects use the sampled duration directly. Other effects use ticks.
        let ticks = match name {
            "instant_health" | "instant_damage" | "saturation" => duration,
            _ => duration.saturating_mul(20),
        };
        Ok(Nbt::List(List::Compound(vec![Compound::from_iter([
            ("id", Nbt::from(self.name.as_str())),
            ("duration", Nbt::Int(ticks)),
        ])])))
    }
}

impl Function {
    fn parse(value: &Value) -> io::Result<(Self, Vec<Condition>)> {
        let name = kind(string(value, "function")?);
        let function = match name {
            "set_count" => Self::SetCount {
                count: Number::parse(required(value, "count")?)?,
                add: value
                    .get("add")
                    .map(|add| {
                        add.as_bool()
                            .ok_or_else(|| invalid("invalid loot count add flag"))
                    })
                    .transpose()?
                    .unwrap_or(false),
            },
            "limit_count" => {
                let limit = required(value, "limit")?;
                if limit.is_number() {
                    return Ok((Self::ExactCount(integer(limit)?), conditions(value)?));
                }
                if !limit.is_object() {
                    return Err(invalid("invalid loot count limit"));
                }
                let min = limit.get("min").map(Number::parse).transpose()?;
                let max = limit.get("max").map(Number::parse).transpose()?;
                if matches!((&min, &max), (Some(Number::Constant(min)), Some(Number::Constant(max))) if round_f32(*min) > round_f32(*max))
                {
                    return Err(invalid("inverted loot count range"));
                }
                Self::LimitCount { min, max }
            }
            "enchanted_count_increase" if value["enchantment"] == "minecraft:looting" => {
                let limit = value.get("limit").map(integer).transpose()?.unwrap_or(0);
                if limit < 0 {
                    return Err(invalid("negative looting limit"));
                }
                Self::LootingBonus {
                    count: Number::parse(required(value, "count")?)?,
                    limit,
                }
            }
            "apply_bonus" => match kind(string(value, "formula")?) {
                "binomial_with_bonus_count" => {
                    let parameters = required(value, "parameters")?;
                    let extra = integer(required(parameters, "extra")?)?;
                    if extra < 0 {
                        return Err(invalid("negative loot trial count"));
                    }
                    Self::BinomialBonus {
                        extra,
                        probability: probability(finite(required(parameters, "probability")?)?)?,
                    }
                }
                "uniform_bonus_count" => Self::UniformBonus,
                _ => Self::None,
            },
            "explosion_decay" | "copy_components" | "copy_state" => Self::None,
            "enchant_randomly" if value["options"].as_str() == Some(chest::ENCHANTMENT_TAG) => {
                Self::EnchantRandomly
            }
            "set_potion" => Self::SetPotion(string(value, "id")?.to_owned()),
            "set_stew_effect" => Self::SetStewEffect(
                array(required(value, "effects")?)?
                    .iter()
                    .map(StewEffect::parse)
                    .collect::<io::Result<_>>()?,
            ),
            "exploration_map"
                if value["decoration"] == "minecraft:red_x"
                    && value["zoom"].as_i64() == Some(1)
                    && value["destination"]
                        .as_str()
                        .unwrap_or("minecraft:on_treasure_maps")
                        == "minecraft:on_treasure_maps"
                    && value["search_radius"].as_i64().unwrap_or(50) == 50
                    && value["skip_existing_chunks"].as_bool() == Some(false) =>
            {
                Self::TreasureMap
            }
            "set_name" if value["target"] == "item_name" => value["name"]["translate"]
                .as_str()
                .map_or(Self::None, |name| {
                    Self::SetTranslatedItemName(name.to_owned())
                }),
            other => {
                warn!("Loot function {other} is not ported; it changes nothing");
                Self::None
            }
        };
        Ok((function, conditions(value)?))
    }

    fn apply(&self, stack: &mut ItemStack, context: &mut Context<'_>) -> io::Result<()> {
        context.charge(1)?;
        match self {
            Self::SetCount { count, add } => {
                let n = count.int(context)?;
                if *add {
                    add_count(stack, n)?;
                } else {
                    stack.count = n;
                }
            }
            Self::LimitCount { min, max } => {
                let min = min.as_ref().map(|min| min.int(context)).transpose()?;
                let max = max.as_ref().map(|max| max.int(context)).transpose()?;
                if matches!((min, max), (Some(min), Some(max)) if min > max) {
                    return Err(invalid("inverted loot count range"));
                }
                if let Some(min) = min {
                    stack.count = stack.count.max(min);
                }
                if let Some(max) = max {
                    stack.count = stack.count.min(max);
                }
            }
            Self::ExactCount(count) => stack.count = *count,
            Self::LootingBonus { count, limit } if context.looting_level > 0 => {
                let bonus = round_f32(context.looting_level as f32 * count.float(context)?);
                add_count(stack, bonus)?;
                if *limit > 0 {
                    stack.count = stack.count.min(*limit);
                }
            }
            Self::BinomialBonus { extra, probability } => {
                context.charge(*extra as usize)?;
                let bonus = (0..*extra)
                    .filter(|_| context.random.next_float() < *probability)
                    .count();
                add_count(stack, bonus as i32)?;
            }
            // `count + random.nextInt(bonusMultiplier * 0 + 1)`: always 0, but the draw is made.
            Self::UniformBonus => {
                add_count(stack, context.random.next_int_bounded(1))?;
            }
            Self::EnchantRandomly => {
                if let Some((enchantments, codec)) = context.chest {
                    context.charge(enchantments.len())?;
                    enchantments.enchant(stack, codec, context.random)?;
                }
            }
            Self::SetPotion(name) => {
                if let Some((_, codec)) = context.chest {
                    codec.set_potion(stack, name)?;
                }
            }
            Self::SetStewEffect(effects) => {
                if let Some((_, codec)) = context.chest
                    && stack.name() == Some("minecraft:suspicious_stew")
                    && !effects.is_empty()
                {
                    let effect =
                        &effects[context.random.next_int_bounded(effects.len() as i32) as usize];
                    codec.set_saved_component(stack, STEW_EFFECTS, &effect.roll(context)?)?;
                }
            }
            Self::TreasureMap => {
                if let Some((_, codec)) = context.chest
                    && stack.name() == Some("minecraft:map")
                {
                    context.treasure_requested = true;
                    if let Some(map) = context.treasure_map {
                        map.apply(stack, codec)?;
                    }
                }
            }
            Self::SetTranslatedItemName(translation) => {
                if let Some((_, codec)) = context.chest {
                    let name = Compound::from_iter([("translate", translation.as_str())]);
                    codec.set_saved_component(
                        stack,
                        "minecraft:item_name",
                        &Nbt::Compound(name),
                    )?;
                }
            }
            Self::LootingBonus { .. } | Self::None => {}
        }
        Ok(())
    }
}

fn add_count(stack: &mut ItemStack, count: i32) -> io::Result<()> {
    stack.count = stack
        .count
        .checked_add(count)
        .ok_or_else(|| invalid("loot count overflow"))?;
    Ok(())
}

fn conditions(value: &Value) -> io::Result<Vec<Condition>> {
    list(value, "conditions")?
        .iter()
        .map(Condition::parse)
        .collect()
}

fn functions(value: &Value) -> io::Result<Vec<(Function, Vec<Condition>)>> {
    list(value, "functions")?
        .iter()
        .map(Function::parse)
        .collect()
}

/// Applies functions in order to a stack, each when its conditions pass (`LootItemFunction.decorate`).
fn decorate(
    functions: &[(Function, Vec<Condition>)],
    stack: &mut ItemStack,
    context: &mut Context<'_>,
) -> io::Result<()> {
    for (function, conditions) in functions {
        if all(conditions, context)? {
            function.apply(stack, context)?;
        }
    }
    Ok(())
}

/// A pool entry (`LootPoolEntryContainer`).
#[derive(Debug, Clone, PartialEq)]
enum Entry {
    Item {
        item: u32,
        weight: i32,
        conditions: Vec<Condition>,
        functions: Vec<(Function, Vec<Condition>)>,
    },
    Empty {
        weight: i32,
        conditions: Vec<Condition>,
    },
    Alternatives(Vec<Entry>, Vec<Condition>),
    Group(Vec<Entry>, Vec<Condition>),
    Sequence(Vec<Entry>, Vec<Condition>),
    /// An entry not ported yet, which never expands.
    Unsupported,
}

impl Entry {
    fn parse(value: &Value) -> io::Result<Self> {
        let name = kind(string(value, "type")?);
        let weight = value.get("weight").map(integer).transpose()?.unwrap_or(1);
        if weight <= 0 {
            return Err(invalid("loot weight must be positive"));
        }
        let conditions = conditions(value)?;
        let functions = functions(value)?;
        let children = || {
            array(required(value, "children")?)?
                .iter()
                .map(Self::parse)
                .collect::<io::Result<Vec<_>>>()
        };
        Ok(match name {
            "item" => {
                let name = string(value, "name")?;
                let item =
                    item_id(name).ok_or_else(|| invalid(format!("unknown loot item {name}")))?;
                Self::Item {
                    item,
                    weight,
                    conditions,
                    functions,
                }
            }
            "empty" => Self::Empty { weight, conditions },
            "alternatives" => Self::Alternatives(children()?, conditions),
            "group" => Self::Group(children()?, conditions),
            "sequence" => Self::Sequence(children()?, conditions),
            "tag" | "dynamic" | "loot_table" => Self::Unsupported,
            other => return Err(invalid(format!("unknown loot entry type {other}"))),
        })
    }

    /// `expand`: the entries this one offers to the roll, if it runs.
    fn expand<'a>(
        &'a self,
        context: &mut Context<'_>,
        out: &mut Vec<&'a Entry>,
    ) -> io::Result<bool> {
        context.charge(1)?;
        Ok(match self {
            Self::Item { conditions, .. } | Self::Empty { conditions, .. } => {
                let offered = all(conditions, context)?;
                if offered {
                    out.push(self);
                }
                offered
            }
            Self::Alternatives(children, conditions) => {
                if !all(conditions, context)? {
                    return Ok(false);
                }
                for child in children {
                    if child.expand(context, out)? {
                        return Ok(true);
                    }
                }
                false
            }
            Self::Group(children, conditions) => {
                if !all(conditions, context)? {
                    return Ok(false);
                }
                for child in children {
                    child.expand(context, out)?;
                }
                true
            }
            Self::Sequence(children, conditions) => {
                if !all(conditions, context)? {
                    return Ok(false);
                }
                for child in children {
                    if !child.expand(context, out)? {
                        break;
                    }
                }
                true
            }
            Self::Unsupported => false,
        })
    }

    fn weight(&self) -> i32 {
        match self {
            Self::Item { weight, .. } | Self::Empty { weight, .. } => *weight,
            _ => 0,
        }
    }

    fn max_weight(&self) -> io::Result<i32> {
        match self {
            Self::Group(children, _) | Self::Sequence(children, _) => total_weight(children),
            Self::Alternatives(children, _) => children
                .iter()
                .try_fold(0, |max, entry| Ok(max.max(entry.max_weight()?))),
            _ => Ok(self.weight()),
        }
    }
}

fn total_weight(entries: &[Entry]) -> io::Result<i32> {
    entries.iter().try_fold(0i32, |total, entry| {
        total
            .checked_add(entry.max_weight()?)
            .ok_or_else(|| invalid("loot weight sum overflows"))
    })
}

#[derive(Debug, Clone, PartialEq)]
struct Pool {
    rolls: Number,
    bonus_rolls: Number,
    entries: Vec<Entry>,
    conditions: Vec<Condition>,
    functions: Vec<(Function, Vec<Condition>)>,
}

/// A loot table (`LootTable`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LootTable {
    pools: Vec<Pool>,
    functions: Vec<(Function, Vec<Condition>)>,
    /// The world random sequence the table draws from, if it names one.
    pub(crate) random_sequence: Option<String>,
}

/// What a table is evaluated with.
struct Context<'a> {
    state: Option<u32>,
    tool: &'a ItemStack,
    looting_level: i32,
    random: &'a mut dyn RandomSource,
    chest: Option<(&'a chest::LootEnchantments, &'a ItemCodec)>,
    treasure_map: Option<crate::maps::TreasureMap>,
    /// Whether an `exploration_map` function was applied to an empty map, which searches for its target.
    treasure_requested: bool,
    remaining_work: usize,
}

impl Context<'_> {
    fn charge(&mut self, amount: usize) -> io::Result<()> {
        self.remaining_work = self
            .remaining_work
            .checked_sub(amount)
            .ok_or_else(|| invalid("loot evaluation exceeds its work limit"))?;
        Ok(())
    }
}

impl LootTable {
    fn parse(value: &Value) -> io::Result<Self> {
        if !value.is_object() {
            return Err(invalid("loot table must be an object"));
        }
        let mut nodes_remaining = MAX_WORK;
        check_structure(value, 0, &mut nodes_remaining)?;
        let pools = list(value, "pools")?
            .iter()
            .map(|pool| {
                let entries = array(required(pool, "entries")?)?
                    .iter()
                    .map(Entry::parse)
                    .collect::<io::Result<Vec<_>>>()?;
                total_weight(&entries)?;
                Ok(Pool {
                    rolls: Number::parse(required(pool, "rolls")?)?,
                    bonus_rolls: pool
                        .get("bonus_rolls")
                        .map(Number::parse)
                        .transpose()?
                        .unwrap_or(Number::Constant(0.0)),
                    entries,
                    conditions: conditions(pool)?,
                    functions: functions(pool)?,
                })
            })
            .collect::<io::Result<_>>()?;
        let random_sequence = value
            .get("random_sequence")
            .map(|_| string(value, "random_sequence").map(str::to_owned))
            .transpose()?;
        Ok(Self {
            pools,
            functions: functions(value)?,
            random_sequence,
        })
    }

    /// The stacks a broken block of `state` drops, broken with `tool` and no explosion (`getRandomItems`).
    pub(crate) fn drops(
        &self,
        state: u32,
        tool: &ItemStack,
        random: &mut dyn RandomSource,
    ) -> io::Result<Vec<ItemStack>> {
        self.roll(&mut Context {
            state: Some(state),
            tool,
            looting_level: 0,
            random,
            chest: None,
            treasure_map: None,
            treasure_requested: false,
            remaining_work: MAX_WORK,
        })
    }

    /// The loot from killing a mob, with Looting from the attacker's main hand.
    pub(crate) fn mob_drops(
        &self,
        tool: &ItemStack,
        looting_level: i32,
        random: &mut dyn RandomSource,
    ) -> io::Result<Vec<ItemStack>> {
        self.roll(&mut Context {
            state: None,
            tool,
            looting_level,
            random,
            chest: None,
            treasure_map: None,
            treasure_requested: false,
            remaining_work: MAX_WORK,
        })
    }

    /// Whether any entry, pool or the table itself can turn an empty map into an exploration map.
    pub(crate) fn has_treasure_map(&self) -> bool {
        self.has_function(&Function::TreasureMap)
    }

    fn has_function(&self, searched: &Function) -> bool {
        fn in_functions(functions: &[(Function, Vec<Condition>)], searched: &Function) -> bool {
            functions.iter().any(|(function, _)| function == searched)
        }
        fn in_entry(entry: &Entry, searched: &Function) -> bool {
            match entry {
                Entry::Item { functions, .. } => in_functions(functions, searched),
                Entry::Alternatives(children, _)
                | Entry::Group(children, _)
                | Entry::Sequence(children, _) => {
                    children.iter().any(|entry| in_entry(entry, searched))
                }
                Entry::Empty { .. } | Entry::Unsupported => false,
            }
        }
        in_functions(&self.functions, searched)
            || self.pools.iter().any(|pool| {
                in_functions(&pool.functions, searched)
                    || pool.entries.iter().any(|entry| in_entry(entry, searched))
            })
    }

    fn roll(&self, context: &mut Context<'_>) -> io::Result<Vec<ItemStack>> {
        let mut stacks = Vec::new();
        for pool in &self.pools {
            context.charge(1)?;
            if !all(&pool.conditions, context)? {
                continue;
            }
            // Luck is 0, but the bonus rolls are still drawn.
            let rolls = pool.rolls.int(context)?.max(0);
            pool.bonus_rolls.float(context)?;
            context.charge(rolls as usize)?;
            for _ in 0..rolls {
                let mut offered = Vec::new();
                for entry in &pool.entries {
                    entry.expand(context, &mut offered)?;
                }
                let total: i32 = offered.iter().map(|e| e.weight()).sum();
                if total == 0 {
                    continue;
                }
                let chosen = if offered.len() == 1 {
                    offered[0]
                } else {
                    let mut k = context.random.next_int_bounded(total);
                    let Some(entry) = offered.iter().find(|e| {
                        k -= e.weight();
                        k < 0
                    }) else {
                        continue;
                    };
                    entry
                };
                let Entry::Item {
                    item, functions, ..
                } = chosen
                else {
                    continue;
                };
                let mut stack = ItemStack {
                    item: *item,
                    count: 1,
                    components: Default::default(),
                };
                decorate(functions, &mut stack, context)?;
                decorate(&pool.functions, &mut stack, context)?;
                decorate(&self.functions, &mut stack, context)?;
                stacks.push(stack);
            }
        }
        // `createStackSplitter`: stacks larger than their item holds come out as several.
        let mut split = Vec::new();
        for stack in stacks {
            if stack.is_empty() {
                continue;
            }
            let max = stack.max_stack_size().unwrap_or(64);
            let pieces = (stack.count as usize).div_ceil(max as usize);
            if pieces > MAX_STACKS - split.len() {
                return Err(invalid("loot emits too many stacks"));
            }
            let mut left = stack.count;
            while left > 0 {
                let count = left.min(max);
                split.push(ItemStack {
                    count,
                    ..stack.clone()
                });
                left -= count;
            }
        }
        Ok(split)
    }
}

/// The loot tables for blocks, supported chests and supported animals.
#[derive(Debug, Default)]
pub struct LootTables {
    blocks: HashMap<String, LootTable>,
    pub(crate) chests: Option<chest::ChestLootTables>,
    pub(crate) animals: HashMap<crate::player::AnimalSpecies, LootTable>,
}

impl LootTables {
    /// Reads loot tables from a data pack archive, such as the vanilla data the server provisions.
    pub fn from_zip_path(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::from_zip_reader(std::fs::File::open(path)?)
    }

    /// Reads loot tables from a data pack archive.
    pub fn from_zip_reader(reader: impl Read + Seek) -> io::Result<Self> {
        let mut archive = zip::ZipArchive::new(reader).map_err(io::Error::other)?;
        let mut tables = Self::default();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).map_err(io::Error::other)?;
            let name = entry.name().map_err(io::Error::other)?.to_string();
            let Some(block) = name
                .strip_prefix("data/minecraft/loot_table/blocks/")
                .and_then(|n| n.strip_suffix(".json"))
            else {
                continue;
            };
            let result = serde_json::from_reader(&mut entry)
                .map_err(io::Error::other)
                .and_then(|json| tables.insert(&format!("minecraft:{block}"), &json));
            if let Err(error) = result {
                warn!("Cannot load {block} loot: {error}");
            }
        }
        match chest::ChestLootTables::load(&mut archive) {
            Ok(chests) => tables.chests = chests,
            Err(error) => warn!("Chests cannot generate their loot: {error}"),
        }
        use crate::player::AnimalSpecies;
        for (species, name) in [(AnimalSpecies::Pig, "pig"), (AnimalSpecies::Cow, "cow")] {
            match archive.by_name(&format!("data/minecraft/loot_table/entities/{name}.json")) {
                Ok(entry) => match serde_json::from_reader(entry)
                    .map_err(io::Error::other)
                    .and_then(|json| LootTable::parse(&json))
                {
                    Ok(table) => {
                        tables.animals.insert(species, table);
                    }
                    Err(error) => warn!("Cannot read {name} loot: {error}"),
                },
                Err(zip::result::ZipError::FileNotFound) => {}
                Err(error) => warn!("Cannot read {name} loot: {error}"),
            }
        }
        Ok(tables)
    }

    /// Adds the table of a block from its JSON.
    pub fn insert(&mut self, block: &str, json: &Value) -> io::Result<()> {
        self.blocks
            .insert(block.to_owned(), LootTable::parse(json)?);
        Ok(())
    }

    /// The number of block tables.
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Whether there are no tables.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// The table of a block, by its name.
    pub(crate) fn block(&self, name: &str) -> Option<&LootTable> {
        self.blocks.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harpas_core::random::LegacyRandom;
    use serde_json::json;

    fn state(name: &str, properties: &[(&str, &str)]) -> u32 {
        block_states()
            .state(name, properties.iter().copied())
            .unwrap()
            .0
    }

    fn names(stacks: &[ItemStack]) -> Vec<(&'static str, i32)> {
        stacks
            .iter()
            .map(|s| (s.name().unwrap(), s.count))
            .collect()
    }

    #[test]
    fn stone_drops_cobblestone_without_silk_touch() {
        let table = LootTable::parse(
            &json!({"pools": [{"rolls": 1.0, "bonus_rolls": 0.0, "entries": [{
            "type": "minecraft:alternatives", "children": [
                {"type": "minecraft:item", "name": "minecraft:stone", "conditions": [{"condition": "minecraft:match_tool",
                    "predicate": {"predicates": {"minecraft:enchantments": [{"enchantments": "minecraft:silk_touch"}]}}}]},
                {"type": "minecraft:item", "name": "minecraft:cobblestone", "conditions": [{"condition": "minecraft:survives_explosion"}]}
            ]}]}]}),
        ).unwrap();
        let mut random = LegacyRandom::new(0);
        let drops = table
            .drops(
                state("minecraft:stone", &[]),
                &ItemStack::empty(),
                &mut random,
            )
            .unwrap();
        assert_eq!(names(&drops), [("minecraft:cobblestone", 1)]);
    }

    #[test]
    fn a_double_slab_drops_two_and_counts_follow_their_functions() {
        let slab = LootTable::parse(
            &json!({"pools": [{"rolls": 1.0, "entries": [{"type": "minecraft:item",
            "name": "minecraft:oak_slab", "functions": [{"function": "minecraft:set_count", "count": 2.0, "add": false,
            "conditions": [{"condition": "minecraft:block_state_property", "block": "minecraft:oak_slab",
            "properties": {"type": "double"}}]}, {"function": "minecraft:explosion_decay"}]}]}]}),
        ).unwrap();
        let mut random = LegacyRandom::new(0);
        let double = state("minecraft:oak_slab", &[("type", "double")]);
        let bottom = state("minecraft:oak_slab", &[("type", "bottom")]);
        assert_eq!(
            names(
                &slab
                    .drops(double, &ItemStack::empty(), &mut random)
                    .unwrap()
            ),
            [("minecraft:oak_slab", 2)]
        );
        assert_eq!(
            names(
                &slab
                    .drops(bottom, &ItemStack::empty(), &mut random)
                    .unwrap()
            ),
            [("minecraft:oak_slab", 1)]
        );
        let glowstone = LootTable::parse(
            &json!({"pools": [{"rolls": 1.0, "entries": [{"type": "minecraft:item",
            "name": "minecraft:glowstone_dust", "functions": [
                {"function": "minecraft:set_count", "count": {"type": "minecraft:uniform", "min": 2.0, "max": 4.0}},
                {"function": "minecraft:limit_count", "limit": {"min": 1.0, "max": 4.0}}]}]}]}),
        ).unwrap();
        for _ in 0..50 {
            let drops = glowstone
                .drops(
                    state("minecraft:glowstone", &[]),
                    &ItemStack::empty(),
                    &mut random,
                )
                .unwrap();
            assert!((2..=4).contains(&drops[0].count));
        }
    }

    #[test]
    fn looting_scales_and_rounds_one_draw_and_can_limit_the_total() {
        let table = LootTable::parse(&json!({
            "pools": [{"rolls": 1, "entries": [{
                "type": "minecraft:item",
                "name": "minecraft:feather",
                "functions": [{
                    "function": "minecraft:enchanted_count_increase",
                    "enchantment": "minecraft:looting",
                    "count": {"type": "minecraft:uniform", "min": 0, "max": 2},
                    "limit": 4
                }]
            }]}]
        }))
        .unwrap();
        for (level, expected_count) in [(0, 1), (1, 2), (2, 4), (3, 4)] {
            let mut random = LegacyRandom::new(0);
            let mut expected_random = random.clone();
            if level > 0 {
                expected_random.next_float();
            }
            let drops = table
                .mob_drops(&ItemStack::empty(), level, &mut random)
                .unwrap();
            assert_eq!(names(&drops), [("minecraft:feather", expected_count)]);
            assert_eq!(random.next_int(), expected_random.next_int());
        }
    }

    #[test]
    fn shears_match_by_item_and_unported_conditions_never_pass() {
        let table = LootTable::parse(
            &json!({"pools": [{"rolls": 1.0, "entries": [{"type": "minecraft:alternatives",
            "children": [
                {"type": "minecraft:item", "name": "minecraft:short_grass", "conditions": [{"condition": "minecraft:match_tool",
                    "predicate": {"items": "minecraft:shears"}}]},
                {"type": "minecraft:item", "name": "minecraft:wheat_seeds", "conditions": [{"condition": "minecraft:location_check"}]}
            ]}]}]}),
        ).unwrap();
        let mut random = LegacyRandom::new(0);
        let grass = state("minecraft:short_grass", &[]);
        let shears = ItemStack {
            item: items().id("minecraft:shears").unwrap(),
            count: 1,
            components: Default::default(),
        };
        assert_eq!(
            names(&table.drops(grass, &shears, &mut random).unwrap()),
            [("minecraft:short_grass", 1)]
        );
        assert!(
            table
                .drops(grass, &ItemStack::empty(), &mut random)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn count_limits_accept_scalar_and_dynamic_bounds() {
        let make = |limit| {
            LootTable::parse(&json!({"pools": [{"rolls": 1, "entries": [{
                "type": "minecraft:item", "name": "minecraft:stone",
                "functions": [{"function": "minecraft:set_count", "count": 10},
                    {"function": "minecraft:limit_count", "limit": limit}]
            }]}]}))
            .unwrap()
        };
        for (table, expected) in [
            (make(json!(3)), 3),
            (
                make(json!({"max": {"type": "minecraft:uniform", "min": 4, "max": 4}})),
                4,
            ),
        ] {
            let drops = table
                .drops(
                    state("minecraft:stone", &[]),
                    &ItemStack::empty(),
                    &mut LegacyRandom::new(0),
                )
                .unwrap();
            assert_eq!(names(&drops), [("minecraft:stone", expected)]);
        }
    }

    #[test]
    fn invalid_loot_structures_and_weights_are_rejected_on_load() {
        for table in [
            json!({"pools": false}),
            json!({"pools": [{"entries": []}]}),
            json!({"pools": [{"rolls": 1, "entries": [{"type": "empty", "weight": -1}]}]}),
            json!({"pools": [{"rolls": 1, "entries": [
                {"type": "empty", "weight": i32::MAX}, {"type": "empty", "weight": 1}]}]}),
            json!({"pools": [{"rolls": 1, "entries": [{"type": "item", "name": "minecraft:stone", "functions": [
                {"function": "limit_count", "limit": {"min": 3, "max": 2}}]}]}]}),
        ] {
            assert!(LootTable::parse(&table).is_err(), "{table}");
        }
    }

    #[test]
    fn rolls_trials_and_output_stacks_are_bounded() {
        for (rolls, count) in [
            (json!(i32::MAX), json!(1)),
            (json!({"type": "binomial", "n": i32::MAX, "p": 1}), json!(1)),
            (json!(1), json!(i32::MAX)),
        ] {
            let table = LootTable::parse(&json!({"pools": [{"rolls": rolls, "entries": [{
                "type": "item", "name": "minecraft:stone", "functions": [{"function": "set_count", "count": count}]
            }]}]})).unwrap();
            assert!(
                table
                    .drops(
                        state("minecraft:stone", &[]),
                        &ItemStack::empty(),
                        &mut LegacyRandom::new(0)
                    )
                    .is_err()
            );
        }
    }
}
