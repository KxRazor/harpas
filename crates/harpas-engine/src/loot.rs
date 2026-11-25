//! Loot tables from the vanilla data pack, evaluated for broken blocks, chests and pig kills.
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
//! Tool enchantments are read as none.
//! Not done yet: `location_check` and
//! `entity_properties` conditions (read as false) and `dynamic` entries (which add nothing), and the functions
//! that copy data into the item (`copy_components`, `copy_state`), which drop the item without it.

use std::collections::HashMap;
use std::io::{self, Read, Seek};
use std::path::Path;

use harpas_core::block::block_states;
use harpas_core::item::items;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_core::random::RandomSource;
use log::warn;
use serde_json::Value;

use crate::inventory::ItemStack;
use crate::item_data::{ItemCodec, STEW_EFFECTS};

pub(crate) mod chest;

/// A number provider (`NumberProvider`).
#[derive(Debug, Clone, PartialEq)]
enum Number {
    Constant(f32),
    Uniform(Box<Number>, Box<Number>),
    Binomial(Box<Number>, Box<Number>),
}

impl Number {
    fn parse(value: &Value) -> Option<Self> {
        if let Some(n) = value.as_f64() {
            return Some(Self::Constant(n as f32));
        }
        let kind = value.get("type")?.as_str()?;
        let field = |name| value.get(name).and_then(Self::parse).map(Box::new);
        match kind {
            "minecraft:constant" => Some(Self::Constant(value.get("value")?.as_f64()? as f32)),
            "minecraft:uniform" => Some(Self::Uniform(field("min")?, field("max")?)),
            "minecraft:binomial" => Some(Self::Binomial(field("n")?, field("p")?)),
            _ => None,
        }
    }

    /// `getFloat`.
    fn float(&self, random: &mut dyn RandomSource) -> f32 {
        match self {
            Self::Constant(n) => *n,
            Self::Uniform(min, max) => {
                let (min, max) = (min.float(random), max.float(random));
                // `Mth.nextFloat`.
                if min >= max {
                    min
                } else {
                    random.next_float() * (max - min) + min
                }
            }
            Self::Binomial(..) => self.int(random) as f32,
        }
    }

    /// `getInt`.
    fn int(&self, random: &mut dyn RandomSource) -> i32 {
        match self {
            Self::Constant(n) => (n + 0.5).floor() as i32,
            Self::Uniform(min, max) => {
                let (min, max) = (min.int(random), max.int(random));
                // `Mth.nextInt`.
                if min >= max {
                    min
                } else {
                    random.next_int_bounded(max - min + 1) + min
                }
            }
            Self::Binomial(n, p) => {
                let (n, p) = (n.int(random), p.float(random));
                (0..n).filter(|_| random.next_float() < p).count() as i32
            }
        }
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
        items: Option<Vec<String>>,
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
    fn parse(value: &Value) -> Self {
        let Some(kind) = value.get("condition").and_then(Value::as_str) else {
            return Self::Unsupported;
        };
        let list = |name| {
            value
                .get(name)
                .and_then(Value::as_array)
                .map(|terms| terms.iter().map(Self::parse).collect())
                .unwrap_or_default()
        };
        match kind {
            "minecraft:survives_explosion" => Self::SurvivesExplosion,
            "minecraft:block_state_property" => {
                let properties = value
                    .get("properties")
                    .and_then(Value::as_object)
                    .map(|properties| {
                        properties
                            .iter()
                            .map(|(key, expected)| {
                                let expected = match expected {
                                    Value::String(s) => PropertyValue::Exact(s.clone()),
                                    Value::Object(range) => PropertyValue::Range(
                                        range.get("min").and_then(Value::as_i64),
                                        range.get("max").and_then(Value::as_i64),
                                    ),
                                    other => PropertyValue::Exact(other.to_string()),
                                };
                                (key.clone(), expected)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Self::BlockState {
                    block: value
                        .get("block")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    properties,
                }
            }
            "minecraft:match_tool" => {
                let predicate = value.get("predicate");
                let items = predicate
                    .and_then(|p| p.get("items"))
                    .map(|items| match items {
                        Value::String(s) => vec![s.clone()],
                        Value::Array(a) => a
                            .iter()
                            .filter_map(|s| s.as_str().map(str::to_owned))
                            .collect(),
                        _ => Vec::new(),
                    });
                let components = predicate.is_some_and(|p| {
                    p.get("predicates").is_some() || p.get("components").is_some()
                });
                Self::MatchTool { items, components }
            }
            "minecraft:random_chance" => value
                .get("chance")
                .and_then(Number::parse)
                .map_or(Self::Unsupported, Self::RandomChance),
            "minecraft:table_bonus" => value
                .get("chances")
                .and_then(Value::as_array)
                .and_then(|c| c.first())
                .and_then(Value::as_f64)
                .map_or(Self::Unsupported, |chance| Self::TableBonus(chance as f32)),
            "minecraft:any_of" => Self::AnyOf(list("terms")),
            "minecraft:all_of" => Self::AllOf(list("terms")),
            "minecraft:inverted" => value.get("term").map_or(Self::Unsupported, |term| {
                Self::Inverted(Box::new(Self::parse(term)))
            }),
            _ => Self::Unsupported,
        }
    }

    fn test(&self, context: &mut Context<'_>) -> bool {
        match self {
            Self::SurvivesExplosion => true,
            Self::BlockState { block, properties } => {
                let Some((name, values)) = context
                    .state
                    .and_then(|state| block_states().properties(state))
                else {
                    return false;
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
                    return false;
                }
                match items {
                    Some(items) => context.tool.name().is_some_and(|name| {
                        !context.tool.is_empty() && items.iter().any(|i| i == name)
                    }),
                    None => true,
                }
            }
            Self::RandomChance(chance) => {
                let chance = chance.float(context.random);
                context.random.next_float() < chance
            }
            Self::TableBonus(chance) => context.random.next_float() < *chance,
            Self::AnyOf(terms) => terms.iter().any(|t| t.test(context)),
            Self::AllOf(terms) => terms.iter().all(|t| t.test(context)),
            Self::Inverted(term) => !term.test(context),
            Self::Unsupported => false,
        }
    }
}

fn all(conditions: &[Condition], context: &mut Context<'_>) -> bool {
    conditions.iter().all(|c| c.test(context))
}

/// A loot function (`LootItemFunction`), with its own conditions.
#[derive(Debug, Clone, PartialEq)]
enum Function {
    SetCount {
        count: Number,
        add: bool,
    },
    LimitCount {
        min: Option<i32>,
        max: Option<i32>,
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
    fn parse(value: &Value) -> Option<Self> {
        Some(Self {
            name: value["type"].as_str()?.to_owned(),
            duration: Number::parse(&value["duration"])?,
        })
    }

    fn roll(&self, random: &mut dyn RandomSource) -> Nbt {
        let duration = self.duration.int(random);
        let name = self.name.strip_prefix("minecraft:").unwrap_or(&self.name);
        // Vanilla's instant effects use the sampled duration directly. Other effects use ticks.
        let ticks = match name {
            "instant_health" | "instant_damage" | "saturation" => duration,
            _ => duration.saturating_mul(20),
        };
        Nbt::List(List::Compound(vec![Compound::from_iter([
            ("id", Nbt::from(self.name.as_str())),
            ("duration", Nbt::Int(ticks)),
        ])]))
    }
}

impl Function {
    fn parse(value: &Value) -> (Self, Vec<Condition>) {
        let conditions = conditions(value);
        let kind = value
            .get("function")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let function = match kind {
            "minecraft:set_count" => {
                value
                    .get("count")
                    .and_then(Number::parse)
                    .map_or(Self::None, |count| Self::SetCount {
                        count,
                        add: value.get("add").and_then(Value::as_bool).unwrap_or(false),
                    })
            }
            "minecraft:limit_count" => {
                let limit = value.get("limit");
                let bound = |name| {
                    limit
                        .and_then(|l| l.get(name))
                        .and_then(Value::as_f64)
                        .map(|n| n.floor() as i32)
                };
                Self::LimitCount {
                    min: bound("min"),
                    max: bound("max"),
                }
            }
            "minecraft:apply_bonus" => {
                let parameters = value.get("parameters");
                match value.get("formula").and_then(Value::as_str) {
                    Some("minecraft:binomial_with_bonus_count") => Self::BinomialBonus {
                        extra: parameters
                            .and_then(|p| p.get("extra"))
                            .and_then(Value::as_i64)
                            .unwrap_or(0) as i32,
                        probability: parameters
                            .and_then(|p| p.get("probability"))
                            .and_then(Value::as_f64)
                            .unwrap_or(0.0) as f32,
                    },
                    Some("minecraft:uniform_bonus_count") => Self::UniformBonus,
                    _ => Self::None,
                }
            }
            "minecraft:explosion_decay" | "minecraft:copy_components" | "minecraft:copy_state" => {
                Self::None
            }
            "minecraft:enchant_randomly"
                if value["options"].as_str() == Some(chest::ENCHANTMENT_TAG) =>
            {
                Self::EnchantRandomly
            }
            "minecraft:set_potion" => value["id"]
                .as_str()
                .map_or(Self::None, |name| Self::SetPotion(name.to_owned())),
            "minecraft:set_stew_effect" => value["effects"]
                .as_array()
                .and_then(|entries| entries.iter().map(StewEffect::parse).collect())
                .map_or(Self::None, Self::SetStewEffect),
            "minecraft:exploration_map"
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
            "minecraft:set_name" if value["target"] == "item_name" => value["name"]["translate"]
                .as_str()
                .map_or(Self::None, |name| {
                    Self::SetTranslatedItemName(name.to_owned())
                }),
            other => {
                warn!("Loot function {other} is not ported; it changes nothing");
                Self::None
            }
        };
        (function, conditions)
    }

    fn apply(&self, stack: &mut ItemStack, context: &mut Context<'_>) -> io::Result<()> {
        let random = &mut *context.random;
        match self {
            Self::SetCount { count, add } => {
                let n = count.int(random);
                stack.count = if *add { stack.count + n } else { n };
            }
            Self::LimitCount { min, max } => {
                if let Some(min) = min {
                    stack.count = stack.count.max(*min);
                }
                if let Some(max) = max {
                    stack.count = stack.count.min(*max);
                }
            }
            Self::BinomialBonus { extra, probability } => {
                for _ in 0..*extra {
                    if random.next_float() < *probability {
                        stack.count += 1;
                    }
                }
            }
            // `count + random.nextInt(bonusMultiplier * 0 + 1)`: always 0, but the draw is made.
            Self::UniformBonus => {
                stack.count += random.next_int_bounded(1);
            }
            Self::EnchantRandomly => {
                if let Some((enchantments, codec)) = context.chest {
                    enchantments.enchant(stack, codec, random)?;
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
                    let effect = &effects[random.next_int_bounded(effects.len() as i32) as usize];
                    codec.set_saved_component(stack, STEW_EFFECTS, &effect.roll(random))?;
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
            Self::None => {}
        }
        Ok(())
    }
}

fn conditions(value: &Value) -> Vec<Condition> {
    value
        .get("conditions")
        .and_then(Value::as_array)
        .map(|c| c.iter().map(Condition::parse).collect())
        .unwrap_or_default()
}

fn functions(value: &Value) -> Vec<(Function, Vec<Condition>)> {
    value
        .get("functions")
        .and_then(Value::as_array)
        .map(|f| f.iter().map(Function::parse).collect())
        .unwrap_or_default()
}

/// Applies functions in order to a stack, each when its conditions pass (`LootItemFunction.decorate`).
fn decorate(
    functions: &[(Function, Vec<Condition>)],
    stack: &mut ItemStack,
    context: &mut Context<'_>,
) -> io::Result<()> {
    for (function, conditions) in functions {
        if all(conditions, context) {
            function.apply(stack, context)?;
        }
    }
    Ok(())
}

/// A pool entry (`LootPoolEntryContainer`).
#[derive(Debug, Clone, PartialEq)]
enum Entry {
    Item {
        item: Option<u32>,
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
    fn parse(value: &Value) -> Self {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let weight = value.get("weight").and_then(Value::as_i64).unwrap_or(1) as i32;
        let children = || {
            value
                .get("children")
                .and_then(Value::as_array)
                .map(|c| c.iter().map(Self::parse).collect())
                .unwrap_or_default()
        };
        match kind {
            "minecraft:item" => Self::Item {
                item: value
                    .get("name")
                    .and_then(Value::as_str)
                    .and_then(|name| items().id(name)),
                weight,
                conditions: conditions(value),
                functions: functions(value),
            },
            "minecraft:empty" => Self::Empty {
                weight,
                conditions: conditions(value),
            },
            "minecraft:alternatives" => Self::Alternatives(children(), conditions(value)),
            "minecraft:group" => Self::Group(children(), conditions(value)),
            "minecraft:sequence" => Self::Sequence(children(), conditions(value)),
            _ => Self::Unsupported,
        }
    }

    /// `expand`: the entries this one offers to the roll, if it runs.
    fn expand<'a>(&'a self, context: &mut Context<'_>, out: &mut Vec<&'a Entry>) -> bool {
        match self {
            Self::Item { conditions, .. } | Self::Empty { conditions, .. } => {
                if all(conditions, context) {
                    out.push(self);
                    true
                } else {
                    false
                }
            }
            Self::Alternatives(children, conditions) => {
                all(conditions, context) && children.iter().any(|child| child.expand(context, out))
            }
            Self::Group(children, conditions) => {
                if !all(conditions, context) {
                    return false;
                }
                for child in children {
                    child.expand(context, out);
                }
                true
            }
            Self::Sequence(children, conditions) => {
                if !all(conditions, context) {
                    return false;
                }
                for child in children {
                    if !child.expand(context, out) {
                        break;
                    }
                }
                true
            }
            Self::Unsupported => false,
        }
    }

    fn weight(&self) -> i32 {
        match self {
            Self::Item { weight, .. } | Self::Empty { weight, .. } => (*weight).max(0),
            _ => 0,
        }
    }
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
    random: &'a mut dyn RandomSource,
    chest: Option<(&'a chest::LootEnchantments, &'a ItemCodec)>,
    treasure_map: Option<crate::maps::TreasureMap>,
    /// Whether an `exploration_map` function was applied to an empty map, which searches for its target.
    treasure_requested: bool,
}

impl LootTable {
    fn parse(value: &Value) -> Self {
        let pools = value
            .get("pools")
            .and_then(Value::as_array)
            .map(|pools| {
                pools
                    .iter()
                    .map(|pool| Pool {
                        rolls: pool
                            .get("rolls")
                            .and_then(Number::parse)
                            .unwrap_or(Number::Constant(1.0)),
                        bonus_rolls: pool
                            .get("bonus_rolls")
                            .and_then(Number::parse)
                            .unwrap_or(Number::Constant(0.0)),
                        entries: pool
                            .get("entries")
                            .and_then(Value::as_array)
                            .map(|e| e.iter().map(Entry::parse).collect())
                            .unwrap_or_default(),
                        conditions: conditions(pool),
                        functions: functions(pool),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            pools,
            functions: functions(value),
            random_sequence: value
                .get("random_sequence")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }
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
            random,
            chest: None,
            treasure_map: None,
            treasure_requested: false,
        })
    }

    /// The loot from killing a mob without fire or Looting bonuses.
    pub(crate) fn mob_drops(
        &self,
        tool: &ItemStack,
        random: &mut dyn RandomSource,
    ) -> io::Result<Vec<ItemStack>> {
        self.roll(&mut Context {
            state: None,
            tool,
            random,
            chest: None,
            treasure_map: None,
            treasure_requested: false,
        })
    }

    /// Whether any entry, pool or the table itself can turn an empty map into an exploration map.
    pub(crate) fn has_treasure_map(&self) -> bool {
        fn in_functions(functions: &[(Function, Vec<Condition>)]) -> bool {
            functions
                .iter()
                .any(|(function, _)| *function == Function::TreasureMap)
        }
        fn in_entry(entry: &Entry) -> bool {
            match entry {
                Entry::Item { functions, .. } => in_functions(functions),
                Entry::Alternatives(children, _)
                | Entry::Group(children, _)
                | Entry::Sequence(children, _) => children.iter().any(in_entry),
                Entry::Empty { .. } | Entry::Unsupported => false,
            }
        }
        in_functions(&self.functions)
            || self
                .pools
                .iter()
                .any(|pool| in_functions(&pool.functions) || pool.entries.iter().any(in_entry))
    }

    fn roll(&self, context: &mut Context<'_>) -> io::Result<Vec<ItemStack>> {
        let mut stacks = Vec::new();
        for pool in &self.pools {
            if !all(&pool.conditions, context) {
                continue;
            }
            // Luck is 0, but the bonus rolls are still drawn.
            let rolls = pool.rolls.int(context.random)
                + (pool.bonus_rolls.float(context.random) * 0.0).floor() as i32;
            for _ in 0..rolls {
                let mut offered = Vec::new();
                for entry in &pool.entries {
                    entry.expand(context, &mut offered);
                }
                let offered: Vec<_> = offered.into_iter().filter(|e| e.weight() > 0).collect();
                let total: i32 = offered.iter().map(|e| e.weight()).sum();
                if total == 0 || offered.is_empty() {
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
                    item: Some(item),
                    functions,
                    ..
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

/// The loot tables for blocks, supported chests and pigs.
#[derive(Debug, Default)]
pub struct LootTables {
    blocks: HashMap<String, LootTable>,
    pub(crate) chests: Option<chest::ChestLootTables>,
    pub(crate) pig: Option<LootTable>,
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
            let mut text = String::new();
            entry.read_to_string(&mut text)?;
            let json: Value = serde_json::from_str(&text).map_err(io::Error::other)?;
            tables.insert(&format!("minecraft:{block}"), &json);
        }
        match chest::ChestLootTables::load(&mut archive) {
            Ok(chests) => tables.chests = chests,
            Err(error) => warn!("Chests cannot generate their loot: {error}"),
        }
        match archive.by_name("data/minecraft/loot_table/entities/pig.json") {
            Ok(entry) => match serde_json::from_reader(entry) {
                Ok(json) => tables.pig = Some(LootTable::parse(&json)),
                Err(error) => warn!("Pigs cannot generate their loot: {error}"),
            },
            Err(zip::result::ZipError::FileNotFound) => {}
            Err(error) => warn!("Pigs cannot generate their loot: {error}"),
        }
        Ok(tables)
    }

    /// Adds the table of a block from its JSON.
    pub fn insert(&mut self, block: &str, json: &Value) {
        self.blocks.insert(block.to_owned(), LootTable::parse(json));
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
        );
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
        );
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
        );
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
    fn shears_match_by_item_and_unported_conditions_never_pass() {
        let table = LootTable::parse(
            &json!({"pools": [{"rolls": 1.0, "entries": [{"type": "minecraft:alternatives",
            "children": [
                {"type": "minecraft:item", "name": "minecraft:short_grass", "conditions": [{"condition": "minecraft:match_tool",
                    "predicate": {"items": "minecraft:shears"}}]},
                {"type": "minecraft:item", "name": "minecraft:wheat_seeds", "conditions": [{"condition": "minecraft:location_check"}]}
            ]}]}]}),
        );
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
}
