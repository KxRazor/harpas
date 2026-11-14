//! Configured features beyond ores: `simple_block`, the three selectors, `block_column`, `disk`, `tree` (in
//! [`crate::tree`]), `fallen_tree` (in [`crate::fallen_tree`]) and the huge mushrooms (in
//! [`crate::huge_mushroom`]), with the block state providers they use.
//!
//! A configured feature compiles from its JSON once, with every placed feature it refers to, and fails to
//! compile, as a whole, when any part of it is not ported yet: a feature type, a block state provider, a
//! block predicate or the survival rule of a block it places. A placed feature that does not compile is
//! skipped, which changes no other feature's seed.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use harpas_core::block::block_states;
use harpas_core::noise::NormalNoise;
use harpas_core::random::{LegacyRandom, RandomSource, WorldgenRandom};
use serde_json::Value;

use crate::bamboo::{CompiledBamboo, place_bamboo};
use crate::block_blob::place_block_blob;
use crate::coral::{CompiledCoral, place_coral};
use crate::data::{
    BlockDirection, ConfiguredFeatureDef, FeatureRefDef, IntProviderDef, PlacedFeatureDef,
    WorldgenData, WorldgenDataError, parse_block_predicate, parse_block_state_def,
    parse_int_provider, parse_noise_parameters, parse_placed_feature,
};
use crate::desert_well::place_desert_well;
use crate::fallen_tree::{CompiledFallenTree, place_fallen_tree};
use crate::feature::{CompiledOre, FeatureWorld, compile_ore, place_ore};
use crate::freeze_top_layer::{CompiledFreezeTopLayer, place_freeze_top_layer};
use crate::huge_mushroom::{CompiledHugeMushroom, place_huge_mushroom};
use crate::ice::{CompiledIce, place_ice};
use crate::liquid::{CompiledLiquid, place_liquid};
use crate::multiface::{CompiledMultifaceGrowth, place_multiface_growth, place_vines};
use crate::ocean_vegetation::{CompiledOceanPlant, place_ocean_plant};
use crate::placement::{
    BlockPredicate, CompiledModifier, Fluid, StateSet, compile_placement, compile_predicate, fluid,
    pick_weighted, place_positions, sample_int_provider,
};
use crate::pointed_dripstone::{CompiledPointedDripstone, place_pointed_dripstone};
use crate::root_system::{CompiledRootSystem, place_root_system};
use crate::surface::resolve_block_state;
use crate::survival::{SurvivalRule, compile_block_survival};
use crate::tree::{CompiledTree, TreeBlocks, place_tree};
use crate::underwater_magma::{CompiledUnderwaterMagma, place_underwater_magma};
use crate::vegetation_patch::{CompiledVegetationPatch, place_vegetation_patch};

/// A placed feature compiled with its configured feature.
#[derive(Debug)]
pub struct CompiledPlaced {
    /// The placement modifiers.
    pub modifiers: Vec<CompiledModifier>,
    /// The configured feature placed at each position.
    pub feature: Arc<CompiledFeature>,
}

/// A configured feature that Harpas places.
#[derive(Debug)]
pub enum CompiledFeature {
    /// `minecraft:ore`.
    Ore(CompiledOre),
    /// `minecraft:simple_block`.
    SimpleBlock {
        /// The state to place.
        provider: StateProvider,
        /// The survival rule of each block the provider can place.
        rules: Vec<(StateSet, SurvivalRule)>,
    },
    /// `minecraft:random_selector`: the first feature whose chance comes up, or the default.
    RandomSelector {
        /// The features with their chances, in order.
        features: Vec<(f32, Arc<CompiledPlaced>)>,
        /// The feature placed when no chance comes up.
        default: Arc<CompiledPlaced>,
    },
    /// `minecraft:simple_random_selector`: one feature chosen uniformly.
    SimpleRandomSelector(Vec<Arc<CompiledPlaced>>),
    /// `minecraft:random_boolean_selector`: one of two features.
    RandomBooleanSelector(Arc<CompiledPlaced>, Arc<CompiledPlaced>),
    /// `minecraft:block_column`.
    BlockColumn {
        /// Each layer's height and states, from the origin outward.
        layers: Vec<(IntProviderDef, StateProvider)>,
        /// The offset of one step.
        step: [i32; 3],
        /// The predicate the column's positions must meet.
        allowed: BlockPredicate,
        /// Whether a column cut short keeps its last layers rather than its first.
        prioritize_tip: bool,
    },
    /// `minecraft:tree`.
    Tree(Box<CompiledTree>),
    /// `minecraft:fallen_tree`.
    FallenTree(Box<CompiledFallenTree>),
    /// `minecraft:huge_brown_mushroom` and `minecraft:huge_red_mushroom`.
    HugeMushroom(Box<CompiledHugeMushroom>),
    /// `minecraft:vegetation_patch`.
    VegetationPatch(Box<CompiledVegetationPatch>),
    /// `minecraft:root_system`.
    RootSystem(Box<CompiledRootSystem>),
    /// `minecraft:seagrass`, `minecraft:kelp` and `minecraft:sea_pickle`.
    OceanPlant(Box<CompiledOceanPlant>),
    /// `minecraft:coral_tree`, `minecraft:coral_claw` and `minecraft:coral_mushroom`.
    Coral(Box<CompiledCoral>),
    /// `minecraft:bamboo`.
    Bamboo(Box<CompiledBamboo>),
    /// `minecraft:block_blob`.
    BlockBlob {
        /// The block of the blob.
        state: u32,
        /// The predicate tested on the block below its origin.
        can_place_on: BlockPredicate,
    },
    /// `minecraft:desert_well`.
    DesertWell,
    /// `minecraft:underwater_magma`.
    UnderwaterMagma(CompiledUnderwaterMagma),
    /// `minecraft:freeze_top_layer`.
    FreezeTopLayer(Box<CompiledFreezeTopLayer>),
    /// `minecraft:pointed_dripstone`.
    PointedDripstone(Box<CompiledPointedDripstone>),
    /// `minecraft:vines`.
    Vines,
    /// `minecraft:spike`, `minecraft:iceberg` and `minecraft:blue_ice`.
    Ice(Box<CompiledIce>),
    /// `minecraft:spring_feature` and `minecraft:lake`.
    Liquid(Box<CompiledLiquid>),
    /// `minecraft:multiface_growth`.
    MultifaceGrowth(Box<CompiledMultifaceGrowth>),
    /// `minecraft:disk`.
    Disk {
        /// The state to place.
        provider: StateProvider,
        /// The blocks the disk replaces.
        target: BlockPredicate,
        /// The disk's radius.
        radius: IntProviderDef,
        /// Blocks above and below the origin's height.
        half_height: i32,
    },
}

/// A block state provider (`BlockStateProvider`).
#[derive(Debug)]
pub enum StateProvider {
    /// `minecraft:simple_state_provider`.
    Simple(u32),
    /// `minecraft:weighted_state_provider`.
    Weighted(Vec<(u32, i32)>),
    /// `minecraft:randomized_int_state_provider`: another provider's state with one integer property drawn.
    RandomizedInt {
        /// The provider of the base state.
        source: Box<StateProvider>,
        /// The property set.
        property: String,
        /// The values drawn.
        values: IntProviderDef,
    },
    /// `minecraft:noise_provider`.
    Noise(NoiseStates),
    /// `minecraft:noise_threshold_provider`.
    NoiseThreshold {
        /// The noise.
        noise: NoiseSampler,
        /// Below this noise value, a low state is drawn.
        threshold: f32,
        /// The chance of drawing a high state above the threshold.
        high_chance: f32,
        /// The state when no high state is drawn.
        default: u32,
        /// States below the threshold.
        low: Vec<u32>,
        /// States above it.
        high: Vec<u32>,
    },
    /// `minecraft:dual_noise_provider`.
    DualNoise {
        /// The noise that picks among the chosen states.
        states: NoiseStates,
        /// The fewest and most states to choose.
        variety: (i32, i32),
        /// The noise that chooses the states.
        slow: NoiseSampler,
    },
    /// `minecraft:rule_based_state_provider`: the first rule whose predicate holds, or the fallback.
    RuleBased {
        /// The rules in order.
        rules: Vec<(BlockPredicate, StateProvider)>,
        /// The provider when no rule holds; none leaves the block as it is.
        fallback: Option<Box<StateProvider>>,
    },
}

/// A noise of a noise-based provider, seeded from the provider's own seed, and its scale.
#[derive(Debug)]
pub struct NoiseSampler {
    noise: NormalNoise,
    scale: f32,
    /// Whether coordinates are scaled in single precision, as `DualNoiseProvider`'s slow noise scales them.
    single: bool,
}

impl NoiseSampler {
    fn value(&self, [x, y, z]: [i32; 3]) -> f64 {
        let scaled = |value: i32| {
            if self.single {
                f64::from(value as f32 * self.scale)
            } else {
                f64::from(value) * f64::from(self.scale)
            }
        };
        self.noise.get_value(scaled(x), scaled(y), scaled(z))
    }
}

/// `NoiseProvider`: a list of states that a noise value indexes.
#[derive(Debug)]
pub struct NoiseStates {
    noise: NoiseSampler,
    states: Vec<u32>,
}

/// `NoiseProvider.getRandomState`: the state at the noise value's position in the list.
fn state_at(states: &[u32], noise: f64) -> u32 {
    let unit = (1.0 + noise) / 2.0;
    let unit = if unit < 0.0 { 0.0 } else { unit.min(0.9999) };
    states[(unit * states.len() as f64) as usize]
}

impl StateProvider {
    /// `getState`: a rule-based provider without a state leaves the block as it is.
    pub fn state<R: RandomSource>(
        &self,
        world: &impl FeatureWorld,
        random: &mut WorldgenRandom<R>,
        position: [i32; 3],
    ) -> u32 {
        self.optional_state(world, random, position)
            .unwrap_or_else(|| world.block_state(position[0], position[1], position[2]))
    }

    /// `getOptionalState`: None when a rule-based provider has no rule that holds and no fallback.
    pub fn optional_state<R: RandomSource>(
        &self,
        world: &impl FeatureWorld,
        random: &mut WorldgenRandom<R>,
        position: [i32; 3],
    ) -> Option<u32> {
        Some(match self {
            Self::Simple(state) => *state,
            Self::Weighted(entries) => *pick_weighted(entries, random)?,
            Self::RandomizedInt {
                source,
                property,
                values,
            } => {
                let state = source.state(world, random, position);
                let Some((name, properties)) = block_states().properties(state) else {
                    return Some(state);
                };
                if !properties.iter().any(|(key, _)| key == property) {
                    return Some(state);
                }
                let value = sample_int_provider(values, random)?.to_string();
                let properties = properties.iter().map(|&(key, current)| {
                    (
                        key,
                        if key == property {
                            value.as_str()
                        } else {
                            current
                        },
                    )
                });
                block_states().state(name, properties)?.0
            }
            Self::Noise(noise) => state_at(&noise.states, noise.noise.value(position)),
            Self::NoiseThreshold {
                noise,
                threshold,
                high_chance,
                default,
                low,
                high,
            } => {
                if noise.value(position) < f64::from(*threshold) {
                    low[random.next_int_bounded(low.len() as i32) as usize]
                } else if random.next_float() < *high_chance {
                    high[random.next_int_bounded(high.len() as i32) as usize]
                } else {
                    *default
                }
            }
            Self::DualNoise {
                states,
                variety,
                slow,
            } => {
                let slow_value = slow.value(position);
                let count = clamped_map(
                    slow_value,
                    -1.0,
                    1.0,
                    f64::from(variety.0),
                    f64::from(variety.1) + 1.0,
                ) as i32;
                let chosen: Vec<u32> = (0..count.max(0))
                    .map(|i| {
                        let at = [
                            position[0] + i * 54545,
                            position[1],
                            position[2] + i * 34234,
                        ];
                        state_at(&states.states, slow.value(at))
                    })
                    .collect();
                if chosen.is_empty() {
                    return None;
                }
                state_at(&chosen, states.noise.value(position))
            }
            Self::RuleBased { rules, fallback } => {
                for (predicate, then) in rules {
                    if predicate.test(world, position) {
                        return Some(then.state(world, random, position));
                    }
                }
                return fallback
                    .as_ref()
                    .map(|fallback| fallback.state(world, random, position));
            }
        })
    }

    /// Every block state the provider can return, as far as compilation can tell.
    fn candidates(&self, out: &mut Vec<u32>) {
        match self {
            Self::Simple(state) => out.push(*state),
            Self::Weighted(entries) => out.extend(entries.iter().map(|(state, _)| *state)),
            Self::RandomizedInt { source, .. } => source.candidates(out),
            Self::Noise(noise) => out.extend(&noise.states),
            Self::NoiseThreshold {
                default, low, high, ..
            } => {
                out.push(*default);
                out.extend(low);
                out.extend(high);
            }
            Self::DualNoise { states, .. } => out.extend(&states.states),
            Self::RuleBased { rules, fallback } => {
                for (_, then) in rules {
                    then.candidates(out);
                }
                if let Some(fallback) = fallback {
                    fallback.candidates(out);
                }
            }
        }
    }
}

/// `Mth.clampedMap`.
fn clamped_map(value: f64, from_min: f64, from_max: f64, to_min: f64, to_max: f64) -> f64 {
    let t = (value - from_min) / (from_max - from_min);
    if t < 0.0 {
        to_min
    } else if t > 1.0 {
        to_max
    } else {
        to_min + t * (to_max - to_min)
    }
}

/// Places a configured feature at a position; returns whether it placed anything (`Feature.place`). Nothing
/// happens, and no random value is drawn, unless the region may be written at the origin.
pub fn place_feature<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    feature: &CompiledFeature,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    if !world.can_write(origin[0], origin[2]) {
        return false;
    }
    match feature {
        CompiledFeature::Ore(ore) => place_ore(world, ore, random, origin),
        CompiledFeature::SimpleBlock { provider, rules } => {
            place_simple_block(world, provider, rules, random, origin)
        }
        CompiledFeature::RandomSelector { features, default } => {
            for (chance, feature) in features {
                if random.next_float() < *chance {
                    return place_placed(world, feature, random, origin);
                }
            }
            place_placed(world, default, random, origin)
        }
        CompiledFeature::SimpleRandomSelector(features) => {
            let index = random.next_int_bounded(features.len() as i32) as usize;
            place_placed(world, &features[index], random, origin)
        }
        CompiledFeature::RandomBooleanSelector(when_true, when_false) => {
            let chosen = if random.next_bool() {
                when_true
            } else {
                when_false
            };
            place_placed(world, chosen, random, origin)
        }
        CompiledFeature::BlockColumn {
            layers,
            step,
            allowed,
            prioritize_tip,
        } => place_block_column(
            world,
            layers,
            *step,
            allowed,
            *prioritize_tip,
            random,
            origin,
        ),
        CompiledFeature::Tree(tree) => place_tree(world, tree, random, origin),
        CompiledFeature::FallenTree(tree) => place_fallen_tree(world, tree, random, origin),
        CompiledFeature::HugeMushroom(mushroom) => {
            place_huge_mushroom(world, mushroom, random, origin)
        }
        CompiledFeature::VegetationPatch(patch) => {
            place_vegetation_patch(world, patch, random, origin)
        }
        CompiledFeature::RootSystem(roots) => place_root_system(world, roots, random, origin),
        CompiledFeature::OceanPlant(plant) => place_ocean_plant(world, plant, random, origin),
        CompiledFeature::Coral(coral) => place_coral(world, coral, random, origin),
        CompiledFeature::Bamboo(bamboo) => place_bamboo(world, bamboo, random, origin),
        CompiledFeature::BlockBlob {
            state,
            can_place_on,
        } => place_block_blob(world, *state, can_place_on, random, origin),
        CompiledFeature::DesertWell => place_desert_well(world, random, origin),
        CompiledFeature::UnderwaterMagma(magma) => {
            place_underwater_magma(world, magma, random, origin)
        }
        CompiledFeature::FreezeTopLayer(surface) => place_freeze_top_layer(world, surface, origin),
        CompiledFeature::PointedDripstone(dripstone) => {
            place_pointed_dripstone(world, dripstone, random, origin)
        }
        CompiledFeature::Vines => place_vines(world, origin),
        CompiledFeature::Ice(ice) => place_ice(world, ice, random, origin),
        CompiledFeature::Liquid(liquid) => place_liquid(world, liquid, random, origin),
        CompiledFeature::MultifaceGrowth(growth) => {
            place_multiface_growth(world, growth, random, origin)
        }
        CompiledFeature::Disk {
            provider,
            target,
            radius,
            half_height,
        } => place_disk(
            world,
            provider,
            target,
            radius,
            *half_height,
            random,
            origin,
        ),
    }
}

/// Places a placed feature without a biome check, as features place the placed features they refer to
/// (`PlacedFeature.place`).
pub fn place_placed<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    placed: &CompiledPlaced,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    let mut any = false;
    place_positions(
        world,
        &HashSet::new(),
        &placed.modifiers,
        random,
        origin,
        &mut |world, random, position| {
            any |= place_feature(world, &placed.feature, random, position);
        },
    );
    any
}

/// The blocks of `DoublePlantBlock`, which `simple_block` places as two halves.
fn is_double_plant(state: u32) -> bool {
    block_states().properties(state).is_some_and(|(name, _)| {
        matches!(
            name,
            "minecraft:tall_grass"
                | "minecraft:large_fern"
                | "minecraft:sunflower"
                | "minecraft:lilac"
                | "minecraft:rose_bush"
                | "minecraft:peony"
        )
    })
}

/// `SimpleBlockFeature.place`.
fn place_simple_block<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    provider: &StateProvider,
    rules: &[(StateSet, SurvivalRule)],
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    let Some(state) = provider.optional_state(world, random, origin) else {
        return false;
    };
    let Some((_, rule)) = rules.iter().find(|(states, _)| states.contains(state)) else {
        return false;
    };
    if !rule.test(world, origin, state) {
        return false;
    }
    let [x, y, z] = origin;
    if is_double_plant(state) {
        if !block_states().is_air(world.block_state(x, y + 1, z)) {
            return false;
        }
        for (half, at) in [("lower", origin), ("upper", [x, y + 1, z])] {
            let part = with_half(state, half, world.block_state(at[0], at[1], at[2]));
            world.set_block_state(at[0], at[1], at[2], part);
        }
    } else if crate::mossy_carpet::is_carpet(state) {
        // `MossyCarpetBlock.placeAt` with the region's random source; a world without one sets the carpet alone.
        match world.region_random() {
            Some(region) => {
                let mut region = region.clone();
                crate::mossy_carpet::place_at(world, origin, &mut region);
                if let Some(shared) = world.region_random() {
                    *shared = region;
                }
            }
            None => {
                world.set_block_state(x, y, z, state);
            }
        }
    } else {
        world.set_block_state(x, y, z, state);
    }
    true
}

/// `DoublePlantBlock.placeAt`: one half, waterlogged when the block it replaces is water.
fn with_half(state: u32, half: &str, replaced: u32) -> u32 {
    let Some((name, properties)) = block_states().properties(state) else {
        return state;
    };
    let water = if matches!(fluid(replaced), Fluid::Water | Fluid::FlowingWater) {
        "true"
    } else {
        "false"
    };
    let properties = properties.iter().map(|&(key, value)| match key {
        "half" => (key, half),
        "waterlogged" => (key, water),
        _ => (key, value),
    });
    block_states()
        .state(name, properties)
        .map_or(state, |(state, _)| state)
}

/// `BlockColumnFeature.place`: the layers' heights are drawn, then shortened where the column meets a position
/// it may not take. Positions are checked one step ahead of the ones placed.
fn place_block_column<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    layers: &[(IntProviderDef, StateProvider)],
    step: [i32; 3],
    allowed: &BlockPredicate,
    prioritize_tip: bool,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    let mut heights: Vec<i32> = layers
        .iter()
        .map(|(height, _)| sample_int_provider(height, random).unwrap_or(0))
        .collect();
    let total: i32 = heights.iter().sum();
    if total == 0 {
        return false;
    }
    let add = |a: [i32; 3]| [a[0] + step[0], a[1] + step[1], a[2] + step[2]];
    let mut check = add(origin);
    for placed in 0..total {
        if !allowed.test(world, check) {
            truncate(&mut heights, total, placed, prioritize_tip);
            break;
        }
        check = add(check);
    }
    let mut position = origin;
    for ((_, provider), &height) in layers.iter().zip(&heights) {
        for _ in 0..height {
            let state = provider.state(world, random, position);
            world.set_block_state(position[0], position[1], position[2], state);
            position = add(position);
        }
    }
    true
}

/// `BlockColumnFeature.truncate`: removes blocks from the first or the last layers until `new_height` remain.
fn truncate(heights: &mut [i32], total: i32, new_height: i32, prioritize_tip: bool) {
    let mut remove = total - new_height;
    let order: Box<dyn Iterator<Item = usize>> = if prioritize_tip {
        Box::new(0..heights.len())
    } else {
        Box::new((0..heights.len()).rev())
    };
    for index in order {
        if remove <= 0 {
            break;
        }
        let taken = heights[index].min(remove);
        remove -= taken;
        heights[index] -= taken;
    }
}

/// `DiskFeature.place`: columns within the radius, each replacing target blocks from the top down.
fn place_disk<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    provider: &StateProvider,
    target: &BlockPredicate,
    radius: &IntProviderDef,
    half_height: i32,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    let top = origin[1] + half_height;
    let bottom = origin[1] - half_height - 1;
    let radius = sample_int_provider(radius, random).unwrap_or(0);
    let mut placed = false;
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            if dx * dx + dz * dz > radius * radius {
                continue;
            }
            let (x, z) = (origin[0] + dx, origin[2] + dz);
            let mut above_placed = false;
            for y in (bottom + 1..=top).rev() {
                if !target.test(world, [x, y, z]) {
                    above_placed = false;
                    continue;
                }
                let Some(state) = provider.optional_state(world, random, [x, y, z]) else {
                    continue;
                };
                world.set_block_state(x, y, z, state);
                if !above_placed {
                    mark_above_for_postprocessing(world, [x, y, z]);
                }
                placed = true;
                above_placed = true;
            }
        }
    }
    placed
}

/// `Feature.markAboveForPostProcessing`: up to two blocks above, stopping at air.
pub(crate) fn mark_above_for_postprocessing(world: &mut impl FeatureWorld, [x, y, z]: [i32; 3]) {
    for dy in 1..=2 {
        if block_states().is_air(world.block_state(x, y + dy, z)) {
            return;
        }
        world.mark_postprocessing(x, y + dy, z);
    }
}

/// Compiles configured and placed features, sharing every placed feature they refer to.
pub struct FeatureCompiler<'a> {
    data: &'a WorldgenData,
    configured: HashMap<String, Option<Arc<CompiledFeature>>>,
    placed: HashMap<String, Option<Arc<CompiledPlaced>>>,
    active: HashSet<String>,
    tree_blocks: Option<Arc<TreeBlocks>>,
}

impl<'a> FeatureCompiler<'a> {
    /// A compiler for the features of a data pack.
    pub fn new(data: &'a WorldgenData) -> Self {
        Self {
            data,
            configured: HashMap::new(),
            placed: HashMap::new(),
            active: HashSet::new(),
            tree_blocks: None,
        }
    }

    /// The block sets trees read, compiled on first use.
    pub(crate) fn tree_blocks(&mut self) -> Result<Arc<TreeBlocks>, WorldgenDataError> {
        if let Some(blocks) = &self.tree_blocks {
            return Ok(blocks.clone());
        }
        let blocks = Arc::new(TreeBlocks::new(self.data)?);
        self.tree_blocks = Some(blocks.clone());
        Ok(blocks)
    }

    /// Compiles a placed feature of the data pack by name; None when part of it is not ported yet.
    pub fn placed(&mut self, name: &str) -> Result<Option<Arc<CompiledPlaced>>, WorldgenDataError> {
        if let Some(compiled) = self.placed.get(name) {
            return Ok(compiled.clone());
        }
        let definition = self
            .data
            .placed_features
            .get(name)
            .ok_or_else(|| invalid(name, format!("unknown placed feature {name}")))?;
        if !self.active.insert(format!("placed {name}")) {
            return Err(invalid(name, "placed feature refers to itself"));
        }
        let compiled = self.placed_definition(name, definition)?;
        self.active.remove(&format!("placed {name}"));
        self.placed.insert(name.to_string(), compiled.clone());
        Ok(compiled)
    }

    fn placed_definition(
        &mut self,
        name: &str,
        definition: &PlacedFeatureDef,
    ) -> Result<Option<Arc<CompiledPlaced>>, WorldgenDataError> {
        let Some(modifiers) = compile_placement(name, &definition.placement, self.data)? else {
            return Ok(None);
        };
        let feature = match &definition.feature {
            FeatureRefDef::Reference(id) => self.configured(id)?,
            FeatureRefDef::Inline(configured) => self.configured_definition(name, configured)?,
        };
        Ok(feature.map(|feature| Arc::new(CompiledPlaced { modifiers, feature })))
    }

    /// A placed feature referred to from a configured feature: a name, or an inline definition, which may
    /// not use the biome modifier, since features place it without a biome check.
    pub(crate) fn placed_ref(
        &mut self,
        name: &str,
        json: &Value,
    ) -> Result<Option<Arc<CompiledPlaced>>, WorldgenDataError> {
        if let Some(id) = json.as_str() {
            return self.placed(&resource_name(id));
        }
        let definition = parse_placed_feature(name, json)?;
        let compiled = self.placed_definition(name, &definition)?;
        let with_biome = compiled.as_ref().is_some_and(|placed| {
            placed
                .modifiers
                .iter()
                .any(|modifier| matches!(modifier, CompiledModifier::Biome))
        });
        Ok(if with_biome { None } else { compiled })
    }

    /// Compiles a configured feature of the data pack by name.
    pub fn configured(
        &mut self,
        name: &str,
    ) -> Result<Option<Arc<CompiledFeature>>, WorldgenDataError> {
        if let Some(compiled) = self.configured.get(name) {
            return Ok(compiled.clone());
        }
        let definition = self
            .data
            .configured_features
            .get(name)
            .ok_or_else(|| invalid(name, format!("unknown configured feature {name}")))?;
        if !self.active.insert(format!("configured {name}")) {
            return Err(invalid(name, "configured feature refers to itself"));
        }
        let compiled = self.configured_definition(name, definition)?;
        self.active.remove(&format!("configured {name}"));
        self.configured.insert(name.to_string(), compiled.clone());
        Ok(compiled)
    }

    fn configured_definition(
        &mut self,
        name: &str,
        definition: &ConfiguredFeatureDef,
    ) -> Result<Option<Arc<CompiledFeature>>, WorldgenDataError> {
        let config = match &definition.config {
            crate::data::FeatureConfigDef::Other(config) => config,
            crate::data::FeatureConfigDef::Ore(_) => {
                return Ok(compile_ore(name, definition, self.data)?
                    .map(|ore| Arc::new(CompiledFeature::Ore(ore))));
            }
        };
        let field = |key: &str| {
            config
                .get(key)
                .ok_or_else(|| invalid(name, format!("missing field `{key}`")))
        };
        let feature = match definition.feature_type.as_str() {
            "minecraft:simple_block" => {
                if config.get("schedule_tick").and_then(Value::as_bool) == Some(true) {
                    // Scheduled ticks are not saved in proto-chunks yet.
                    return Ok(None);
                }
                let Some(provider) = self.provider(name, field("to_place")?)? else {
                    return Ok(None);
                };
                let mut candidates = Vec::new();
                provider.candidates(&mut candidates);
                let mut rules = Vec::new();
                let mut blocks = HashSet::new();
                for state in candidates {
                    let Some((block, _)) = block_states().properties(state) else {
                        return Ok(None);
                    };
                    if !blocks.insert(block.to_string()) {
                        continue;
                    }
                    let Some(rule) = compile_block_survival(name, block, self.data)? else {
                        return Ok(None);
                    };
                    let states = crate::placement::states_where(|id| {
                        block_states()
                            .properties(id)
                            .is_some_and(|(owner, _)| owner == block)
                    });
                    rules.push((states, rule));
                }
                CompiledFeature::SimpleBlock { provider, rules }
            }
            "minecraft:random_selector" => {
                let entries = field("features")?
                    .as_array()
                    .ok_or_else(|| invalid(name, "`features` must be an array"))?;
                let mut features = Vec::with_capacity(entries.len());
                for entry in entries {
                    let chance = entry
                        .get("chance")
                        .and_then(Value::as_f64)
                        .ok_or_else(|| invalid(name, "a weighted feature needs a `chance`"))?
                        as f32;
                    let feature = entry
                        .get("feature")
                        .ok_or_else(|| invalid(name, "a weighted feature needs a `feature`"))?;
                    let Some(feature) = self.placed_ref(name, feature)? else {
                        return Ok(None);
                    };
                    features.push((chance, feature));
                }
                let Some(default) = self.placed_ref(name, field("default")?)? else {
                    return Ok(None);
                };
                CompiledFeature::RandomSelector { features, default }
            }
            "minecraft:simple_random_selector" => {
                let features = field("features")?;
                let entries: Vec<&Value> = match features {
                    Value::Array(entries) => entries.iter().collect(),
                    Value::String(id) if !id.starts_with('#') => vec![features],
                    _ => return Ok(None),
                };
                if entries.is_empty() {
                    return Err(invalid(name, "`features` must not be empty"));
                }
                let mut compiled = Vec::with_capacity(entries.len());
                for entry in entries {
                    let Some(feature) = self.placed_ref(name, entry)? else {
                        return Ok(None);
                    };
                    compiled.push(feature);
                }
                CompiledFeature::SimpleRandomSelector(compiled)
            }
            "minecraft:random_boolean_selector" => {
                let (Some(when_true), Some(when_false)) = (
                    self.placed_ref(name, field("feature_true")?)?,
                    self.placed_ref(name, field("feature_false")?)?,
                ) else {
                    return Ok(None);
                };
                CompiledFeature::RandomBooleanSelector(when_true, when_false)
            }
            "minecraft:block_column" => {
                let direction = field("direction")?
                    .as_str()
                    .and_then(BlockDirection::from_name)
                    .ok_or_else(|| invalid(name, "unknown column direction"))?;
                let Some(allowed) = self.predicate(name, field("allowed_placement")?)? else {
                    return Ok(None);
                };
                let mut layers = Vec::new();
                for layer in field("layers")?
                    .as_array()
                    .ok_or_else(|| invalid(name, "`layers` must be an array"))?
                {
                    let height = parse_int_provider(
                        name,
                        layer
                            .get("height")
                            .ok_or_else(|| invalid(name, "a layer needs a `height`"))?,
                    )?;
                    if matches!(height, IntProviderDef::Other(_)) {
                        return Ok(None);
                    }
                    let Some(provider) = self.provider(
                        name,
                        layer
                            .get("provider")
                            .ok_or_else(|| invalid(name, "a layer needs a `provider`"))?,
                    )?
                    else {
                        return Ok(None);
                    };
                    layers.push((height, provider));
                }
                CompiledFeature::BlockColumn {
                    layers,
                    step: direction.offset(),
                    allowed,
                    prioritize_tip: field("prioritize_tip")?.as_bool().unwrap_or(false),
                }
            }
            "minecraft:disk" => {
                let (Some(provider), Some(target)) = (
                    self.provider(name, field("state_provider")?)?,
                    self.predicate(name, field("target")?)?,
                ) else {
                    return Ok(None);
                };
                let radius = parse_int_provider(name, field("radius")?)?;
                if matches!(radius, IntProviderDef::Other(_)) {
                    return Ok(None);
                }
                CompiledFeature::Disk {
                    provider,
                    target,
                    radius,
                    half_height: field("half_height")?
                        .as_i64()
                        .and_then(|value| i32::try_from(value).ok())
                        .ok_or_else(|| invalid(name, "`half_height` must be an integer"))?,
                }
            }
            "minecraft:tree" => match self.tree(name, config)? {
                Some(tree) => CompiledFeature::Tree(Box::new(tree)),
                None => return Ok(None),
            },
            "minecraft:fallen_tree" => match self.fallen_tree(name, config)? {
                Some(tree) => CompiledFeature::FallenTree(Box::new(tree)),
                None => return Ok(None),
            },
            "minecraft:vegetation_patch" => match self.vegetation_patch(name, config)? {
                Some(patch) => CompiledFeature::VegetationPatch(Box::new(patch)),
                None => return Ok(None),
            },
            kind @ ("minecraft:seagrass" | "minecraft:kelp" | "minecraft:sea_pickle") => {
                match self.ocean_plant(name, kind, config)? {
                    Some(plant) => CompiledFeature::OceanPlant(Box::new(plant)),
                    None => return Ok(None),
                }
            }
            "minecraft:vines" => CompiledFeature::Vines,
            kind @ ("minecraft:coral_tree"
            | "minecraft:coral_claw"
            | "minecraft:coral_mushroom") => CompiledFeature::Coral(Box::new(self.coral(kind)?)),
            "minecraft:desert_well" => CompiledFeature::DesertWell,
            "minecraft:underwater_magma" => {
                CompiledFeature::UnderwaterMagma(self.underwater_magma(name, config)?)
            }
            "minecraft:freeze_top_layer" => {
                CompiledFeature::FreezeTopLayer(Box::new(self.freeze_top_layer(name)?))
            }
            "minecraft:pointed_dripstone" => {
                CompiledFeature::PointedDripstone(Box::new(self.pointed_dripstone(name, config)?))
            }
            "minecraft:bamboo" => CompiledFeature::Bamboo(Box::new(self.bamboo(name, config)?)),
            "minecraft:block_blob" => {
                let Some(can_place_on) = self.predicate(name, field("can_place_on")?)? else {
                    return Ok(None);
                };
                CompiledFeature::BlockBlob {
                    state: resolve_block_state(
                        &parse_block_state_def(name, field("state")?)?,
                        name,
                    )?,
                    can_place_on,
                }
            }
            kind @ ("minecraft:spike" | "minecraft:iceberg" | "minecraft:blue_ice") => {
                match self.ice(name, kind, config)? {
                    Some(ice) => CompiledFeature::Ice(Box::new(ice)),
                    None => return Ok(None),
                }
            }
            kind @ ("minecraft:spring_feature" | "minecraft:lake") => {
                match self.liquid(name, kind, config)? {
                    Some(liquid) => CompiledFeature::Liquid(Box::new(liquid)),
                    None => return Ok(None),
                }
            }
            "minecraft:multiface_growth" => match self.multiface_growth(name, config)? {
                Some(growth) => CompiledFeature::MultifaceGrowth(Box::new(growth)),
                None => return Ok(None),
            },
            "minecraft:root_system" => match self.root_system(name, config)? {
                Some(roots) => CompiledFeature::RootSystem(Box::new(roots)),
                None => return Ok(None),
            },
            kind @ ("minecraft:huge_brown_mushroom" | "minecraft:huge_red_mushroom") => {
                let red = kind == "minecraft:huge_red_mushroom";
                match self.huge_mushroom(name, red, config)? {
                    Some(mushroom) => CompiledFeature::HugeMushroom(Box::new(mushroom)),
                    None => return Ok(None),
                }
            }
            _ => return Ok(None),
        };
        Ok(Some(Arc::new(feature)))
    }

    /// The data pack the compiler reads.
    pub(crate) fn data(&self) -> &WorldgenData {
        self.data
    }

    pub(crate) fn predicate(
        &self,
        name: &str,
        json: &Value,
    ) -> Result<Option<BlockPredicate>, WorldgenDataError> {
        compile_predicate(name, &parse_block_predicate(name, json)?, self.data)
    }

    pub(crate) fn provider(
        &mut self,
        name: &str,
        json: &Value,
    ) -> Result<Option<StateProvider>, WorldgenDataError> {
        let state = |json: &Value| resolve_block_state(&parse_block_state_def(name, json)?, name);
        let states = |json: Option<&Value>| -> Result<Vec<u32>, WorldgenDataError> {
            json.and_then(Value::as_array)
                .ok_or_else(|| invalid(name, "a provider needs a list of states"))?
                .iter()
                .map(state)
                .collect()
        };
        let number = |key: &str| {
            json.get(key)
                .and_then(Value::as_f64)
                .ok_or_else(|| invalid(name, format!("a provider needs `{key}`")))
        };
        let noise = |noise_key: &str,
                     scale_key: &str,
                     single: bool|
         -> Result<NoiseSampler, WorldgenDataError> {
            let seed = json
                .get("seed")
                .and_then(Value::as_i64)
                .ok_or_else(|| invalid(name, "a noise provider needs a `seed`"))?;
            let parameters = parse_noise_parameters(
                name,
                json.get(noise_key)
                    .ok_or_else(|| invalid(name, format!("a provider needs `{noise_key}`")))?,
            )?;
            let mut random = WorldgenRandom::new(LegacyRandom::new(seed));
            Ok(NoiseSampler {
                noise: NormalNoise::from_parameters(&mut random, &parameters),
                scale: number(scale_key)? as f32,
                single,
            })
        };
        let kind = json
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid(name, "a block state provider needs a `type`"))?;
        Ok(Some(match kind {
            "minecraft:simple_state_provider" => StateProvider::Simple(state(
                json.get("state")
                    .ok_or_else(|| invalid(name, "a simple provider needs a `state`"))?,
            )?),
            "minecraft:weighted_state_provider" => {
                let entries = json
                    .get("entries")
                    .and_then(Value::as_array)
                    .filter(|entries| !entries.is_empty())
                    .ok_or_else(|| invalid(name, "a weighted provider needs `entries`"))?;
                let mut weighted = Vec::with_capacity(entries.len());
                for entry in entries {
                    let data = entry
                        .get("data")
                        .ok_or_else(|| invalid(name, "a weighted entry needs `data`"))?;
                    let weight = entry
                        .get("weight")
                        .and_then(Value::as_i64)
                        .and_then(|weight| i32::try_from(weight).ok())
                        .ok_or_else(|| invalid(name, "a weighted entry needs a `weight`"))?;
                    weighted.push((state(data)?, weight));
                }
                StateProvider::Weighted(weighted)
            }
            "minecraft:randomized_int_state_provider" => {
                let Some(source) = self.provider(
                    name,
                    json.get("source")
                        .ok_or_else(|| invalid(name, "a randomized provider needs a `source`"))?,
                )?
                else {
                    return Ok(None);
                };
                let values = parse_int_provider(
                    name,
                    json.get("values")
                        .ok_or_else(|| invalid(name, "a randomized provider needs `values`"))?,
                )?;
                if matches!(values, IntProviderDef::Other(_)) {
                    return Ok(None);
                }
                StateProvider::RandomizedInt {
                    source: Box::new(source),
                    property: json
                        .get("property")
                        .and_then(Value::as_str)
                        .ok_or_else(|| invalid(name, "a randomized provider needs a `property`"))?
                        .to_string(),
                    values,
                }
            }
            "minecraft:noise_provider" => StateProvider::Noise(NoiseStates {
                noise: noise("noise", "scale", false)?,
                states: states(json.get("states"))?,
            }),
            "minecraft:noise_threshold_provider" => StateProvider::NoiseThreshold {
                noise: noise("noise", "scale", false)?,
                threshold: number("threshold")? as f32,
                high_chance: number("high_chance")? as f32,
                default: state(
                    json.get("default_state")
                        .ok_or_else(|| invalid(name, "a provider needs `default_state`"))?,
                )?,
                low: states(json.get("low_states"))?,
                high: states(json.get("high_states"))?,
            },
            "minecraft:dual_noise_provider" => {
                let variety = json
                    .get("variety")
                    .and_then(Value::as_array)
                    .filter(|range| range.len() == 2)
                    .and_then(|range| Some((range[0].as_i64()?, range[1].as_i64()?)))
                    .ok_or_else(|| invalid(name, "`variety` must be two integers"))?;
                StateProvider::DualNoise {
                    states: NoiseStates {
                        noise: noise("noise", "scale", false)?,
                        states: states(json.get("states"))?,
                    },
                    variety: (variety.0 as i32, variety.1 as i32),
                    slow: noise("slow_noise", "slow_scale", true)?,
                }
            }
            "minecraft:rule_based_state_provider" => {
                let mut rules = Vec::new();
                for rule in json
                    .get("rules")
                    .and_then(Value::as_array)
                    .ok_or_else(|| invalid(name, "a rule-based provider needs `rules`"))?
                {
                    let (Some(predicate), Some(then)) = (
                        self.predicate(
                            name,
                            rule.get("if_true")
                                .ok_or_else(|| invalid(name, "a rule needs `if_true`"))?,
                        )?,
                        self.provider(
                            name,
                            rule.get("then")
                                .ok_or_else(|| invalid(name, "a rule needs `then`"))?,
                        )?,
                    ) else {
                        return Ok(None);
                    };
                    rules.push((predicate, then));
                }
                let fallback = match json.get("fallback") {
                    Some(fallback) => match self.provider(name, fallback)? {
                        Some(provider) => Some(Box::new(provider)),
                        None => return Ok(None),
                    },
                    None => None,
                };
                StateProvider::RuleBased { rules, fallback }
            }
            _ => return Ok(None),
        }))
    }
}

fn resource_name(name: &str) -> String {
    if name.contains(':') {
        name.to_string()
    } else {
        format!("minecraft:{name}")
    }
}

fn invalid(context: &str, message: impl Into<String>) -> WorldgenDataError {
    WorldgenDataError::InvalidDefinition {
        context: context.to_string(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::RegionFeatureWorld;
    use crate::generator::GeneratedChunk;
    use harpas_core::random::XoroshiroRandom;

    fn state(name: &str, properties: &[(&str, &str)]) -> u32 {
        block_states()
            .state(name, properties.iter().copied())
            .unwrap()
            .0
    }

    fn world(blocks: &[([i32; 3], u32)]) -> RegionFeatureWorld {
        let mut chunk = GeneratedChunk::new(0, 0, -64, 384, 1);
        for &([x, y, z], state) in blocks {
            chunk.set_block_state(x as usize, y, z as usize, state);
        }
        RegionFeatureWorld::new([chunk], 0).unwrap()
    }

    #[test]
    fn truncation_takes_from_the_base_or_the_tip() {
        let mut heights = [2, 3, 1];
        truncate(&mut heights, 6, 2, true);
        assert_eq!(heights, [0, 1, 1]);
        let mut heights = [2, 3, 1];
        truncate(&mut heights, 6, 2, false);
        assert_eq!(heights, [2, 0, 0]);
    }

    #[test]
    fn noise_values_index_the_state_list() {
        let states = [1, 2, 3, 4];
        assert_eq!(state_at(&states, -2.0), 1);
        assert_eq!(state_at(&states, 0.0), 3);
        assert_eq!(state_at(&states, 5.0), 4);
        assert_eq!(clamped_map(0.0, -1.0, 1.0, 1.0, 4.0), 2.5);
        assert_eq!(clamped_map(3.0, -1.0, 1.0, 1.0, 4.0), 4.0);
    }

    #[test]
    fn double_plants_place_both_halves_and_need_room() {
        let dirt = state("minecraft:dirt", &[]);
        let grass = state("minecraft:tall_grass", &[("half", "lower")]);
        let rules = vec![(
            crate::placement::states_where(|id| {
                block_states()
                    .properties(id)
                    .is_some_and(|(name, _)| name == "minecraft:tall_grass")
            }),
            SurvivalRule::Always,
        )];
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(1));
        let mut open = world(&[([4, 0, 4], dirt)]);
        let provider = StateProvider::Simple(grass);
        assert!(place_simple_block(
            &mut open,
            &provider,
            &rules,
            &mut random,
            [4, 1, 4]
        ));
        assert_eq!(open.block_state(4, 1, 4), grass);
        assert_eq!(
            open.block_state(4, 2, 4),
            state("minecraft:tall_grass", &[("half", "upper")])
        );
        let mut covered = world(&[([4, 0, 4], dirt), ([4, 2, 4], dirt)]);
        assert!(!place_simple_block(
            &mut covered,
            &provider,
            &rules,
            &mut random,
            [4, 1, 4]
        ));
    }

    #[test]
    fn disks_replace_targets_within_the_radius() {
        let dirt = state("minecraft:dirt", &[]);
        let clay = state("minecraft:clay", &[]);
        let floor: Vec<_> = (0..16)
            .flat_map(|x| (0..16).map(move |z| ([x, 0, z], dirt)))
            .collect();
        let mut world = world(&floor);
        let target = BlockPredicate::States {
            states: crate::placement::states_where(|id| id == dirt),
            offset: [0; 3],
        };
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(1));
        let placed = place_disk(
            &mut world,
            &StateProvider::Simple(clay),
            &target,
            &IntProviderDef::Constant(2),
            1,
            &mut random,
            [8, 0, 8],
        );
        assert!(placed);
        let replaced: Vec<_> = (0..16)
            .flat_map(|x| (0..16).map(move |z| (x, z)))
            .filter(|&(x, z)| world.block_state(x, 0, z) == clay)
            .collect();
        assert_eq!(replaced.len(), 13, "a radius-2 disk covers 13 columns");
        assert!(replaced.contains(&(8, 10)) && !replaced.contains(&(10, 10)));
    }
}
