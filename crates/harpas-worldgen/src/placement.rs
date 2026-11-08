//! Vanilla's placement modifiers, block predicates and value providers.
//!
//! A placed feature's modifiers turn one origin, the chunk's lowest corner, into the positions where its
//! configured feature runs. Each modifier maps a position to zero or more positions, and vanilla chains them
//! as lazy streams: every position goes through the remaining modifiers, and the feature places it, before the
//! next position comes out. Features share the placement's random source, so this depth-first order decides
//! every random value; [`place_positions`] keeps it.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use harpas_core::block::block_states;
use harpas_core::block_behavior::block_behavior;
use harpas_core::noise::PerlinSimplexNoise;
use harpas_core::random::{LegacyRandom, RandomSource, WorldgenRandom};

use crate::data::{
    BlockPredicateDef, HeightProviderDef, IntProviderDef, PlacementModifierDef, WorldgenData,
    WorldgenDataError,
};
use crate::feature::FeatureWorld;
use crate::generator::HeightmapType;
use crate::survival::{SurvivalRule, compile_survival};

/// A placement modifier, compiled against the block registry.
#[derive(Debug, Clone)]
pub enum CompiledModifier {
    /// `minecraft:count`.
    Count(IntProviderDef),
    /// `minecraft:rarity_filter`, with a chance of one in this value.
    RarityFilter(i32),
    /// `minecraft:in_square`.
    InSquare,
    /// `minecraft:height_range`.
    HeightRange(HeightProviderDef),
    /// `minecraft:biome`.
    Biome,
    /// `minecraft:heightmap`.
    Heightmap(HeightmapType),
    /// `minecraft:count_on_every_layer`.
    CountOnEveryLayer(IntProviderDef),
    /// `minecraft:environment_scan`.
    EnvironmentScan {
        /// Offset of one step.
        step: [i32; 3],
        /// The predicate that ends the search with a position.
        target: BlockPredicate,
        /// The predicate every position on the way must meet.
        allowed: BlockPredicate,
        /// Largest number of steps.
        max_steps: i32,
    },
    /// `minecraft:noise_based_count`.
    NoiseBasedCount {
        /// Scale from noise to count.
        noise_to_count_ratio: i32,
        /// Divisor of block coordinates before sampling.
        noise_factor: f64,
        /// Added to the noise before scaling.
        noise_offset: f64,
    },
    /// `minecraft:noise_threshold_count`.
    NoiseThresholdCount {
        /// The noise value that separates the two counts.
        noise_level: f64,
        /// Count below the noise level.
        below_noise: i32,
        /// Count at or above the noise level.
        above_noise: i32,
    },
    /// `minecraft:random_offset`.
    RandomOffset {
        /// Horizontal offset, sampled for X and then Z.
        xz_spread: IntProviderDef,
        /// Vertical offset.
        y_spread: IntProviderDef,
    },
    /// `minecraft:surface_relative_threshold_filter`.
    SurfaceRelativeThresholdFilter {
        /// The heightmap the range is relative to.
        heightmap: HeightmapType,
        /// Lowest offset from the heightmap.
        min_inclusive: i32,
        /// Highest offset from the heightmap.
        max_inclusive: i32,
    },
    /// `minecraft:surface_water_depth_filter`, with the deepest water allowed.
    SurfaceWaterDepthFilter(i32),
    /// `minecraft:block_predicate_filter`.
    BlockPredicateFilter(BlockPredicate),
    /// `minecraft:fixed_placement`.
    FixedPlacement(Vec<[i32; 3]>),
}

/// A block predicate, compiled against the block registry (`BlockPredicate`). `matching_blocks`,
/// `matching_block_tag`, `solid`, `replaceable` and `has_sturdy_face` read only the block state, so they all
/// compile to a set of states.
#[derive(Debug, Clone)]
pub enum BlockPredicate {
    /// The block state at an offset is in a set.
    States {
        /// Membership by block state ID.
        states: StateSet,
        /// Offset from the tested position.
        offset: [i32; 3],
    },
    /// The fluid at an offset is in a set: `matching_fluids`.
    Fluids {
        /// One bit for every [`Fluid`].
        fluids: u8,
        /// Offset from the tested position.
        offset: [i32; 3],
    },
    /// A block state could stay at the offset position: `would_survive`.
    Survives {
        /// The block's survival rule.
        rule: Box<SurvivalRule>,
        /// The block state tested.
        state: u32,
        /// Offset from the tested position.
        offset: [i32; 3],
    },
    /// The offset position is inside the world's height: `inside_world_bounds`.
    InsideWorldBounds {
        /// Offset from the tested position.
        offset: [i32; 3],
    },
    /// Every predicate holds, tested in order: `all_of`.
    AllOf(Vec<BlockPredicate>),
    /// Any predicate holds, tested in order: `any_of`.
    AnyOf(Vec<BlockPredicate>),
    /// The predicate does not hold: `not`.
    Not(Box<BlockPredicate>),
    /// Always holds: `true`.
    True,
}

impl BlockPredicate {
    /// Tests the predicate at a position.
    pub fn test(&self, world: &impl FeatureWorld, position: [i32; 3]) -> bool {
        match self {
            Self::States { states, offset } => {
                let [x, y, z] = add(position, *offset);
                states.contains(world.block_state(x, y, z))
            }
            Self::Fluids { fluids, offset } => {
                let [x, y, z] = add(position, *offset);
                fluids & (1 << fluid(world.block_state(x, y, z)) as u8) != 0
            }
            Self::Survives {
                rule,
                state,
                offset,
            } => rule.test(world, add(position, *offset), *state),
            Self::InsideWorldBounds { offset } => {
                let y = position[1] + offset[1];
                (world.min_y()..world.min_y() + world.height()).contains(&y)
            }
            Self::AllOf(predicates) => predicates.iter().all(|p| p.test(world, position)),
            Self::AnyOf(predicates) => predicates.iter().any(|p| p.test(world, position)),
            Self::Not(predicate) => !predicate.test(world, position),
            Self::True => true,
        }
    }
}

/// A set of block states, as one bit per block state ID.
#[derive(Debug, Clone)]
pub struct StateSet(Arc<[u64]>);

impl StateSet {
    /// The states whose membership is true, indexed by block state ID.
    pub fn from_membership(membership: &[bool]) -> Self {
        let mut words = vec![0_u64; membership.len().div_ceil(64)];
        for (state, _) in membership.iter().enumerate().filter(|(_, member)| **member) {
            words[state / 64] |= 1 << (state % 64);
        }
        Self(words.into())
    }

    /// Whether a block state is in the set.
    pub fn contains(&self, state: u32) -> bool {
        let state = state as usize;
        self.0
            .get(state / 64)
            .is_some_and(|word| word & (1 << (state % 64)) != 0)
    }
}

/// The fluid a block state holds (`FluidState`), by fluid type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Fluid {
    /// No fluid (`minecraft:empty`).
    Empty,
    /// A water source (`minecraft:water`).
    Water,
    /// Flowing or falling water (`minecraft:flowing_water`).
    FlowingWater,
    /// A lava source (`minecraft:lava`).
    Lava,
    /// Flowing or falling lava (`minecraft:flowing_lava`).
    FlowingLava,
}

/// The fluid a block state holds.
///
/// Liquid blocks hold a source at level 0 and a flowing fluid otherwise; bubble columns, kelp and seagrass
/// always hold a water source, as does every block with `waterlogged=true`.
pub fn fluid(state: u32) -> Fluid {
    static TABLE: OnceLock<Vec<Fluid>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let states = block_states();
        (0..states.len())
            .map(|id| {
                let (name, properties) = states.properties(id).expect("known block state");
                let level_zero = properties.contains(&("level", "0"));
                match name {
                    "minecraft:water" if level_zero => Fluid::Water,
                    "minecraft:water" => Fluid::FlowingWater,
                    "minecraft:lava" if level_zero => Fluid::Lava,
                    "minecraft:lava" => Fluid::FlowingLava,
                    "minecraft:bubble_column"
                    | "minecraft:kelp"
                    | "minecraft:kelp_plant"
                    | "minecraft:seagrass"
                    | "minecraft:tall_seagrass" => Fluid::Water,
                    _ if properties.contains(&("waterlogged", "true")) => Fluid::Water,
                    _ => Fluid::Empty,
                }
            })
            .collect()
    });
    table.get(state as usize).copied().unwrap_or(Fluid::Empty)
}

/// Compiles a placed feature's modifiers; None when one of them is not ported yet.
pub fn compile_placement(
    name: &str,
    modifiers: &[PlacementModifierDef],
    data: &WorldgenData,
) -> Result<Option<Vec<CompiledModifier>>, WorldgenDataError> {
    let mut compiled = Vec::with_capacity(modifiers.len());
    for modifier in modifiers {
        let ints_ported = |providers: &[&IntProviderDef]| providers.iter().all(|p| int_ported(p));
        compiled.push(match modifier {
            PlacementModifierDef::Count(count) if ints_ported(&[count]) => {
                CompiledModifier::Count(count.clone())
            }
            PlacementModifierDef::RarityFilter(chance) => CompiledModifier::RarityFilter(*chance),
            PlacementModifierDef::InSquare => CompiledModifier::InSquare,
            PlacementModifierDef::HeightRange(height) if height_ported(height) => {
                CompiledModifier::HeightRange(height.clone())
            }
            PlacementModifierDef::Biome => CompiledModifier::Biome,
            PlacementModifierDef::Heightmap(kind) => CompiledModifier::Heightmap(*kind),
            PlacementModifierDef::CountOnEveryLayer(count) if ints_ported(&[count]) => {
                CompiledModifier::CountOnEveryLayer(count.clone())
            }
            PlacementModifierDef::EnvironmentScan {
                direction,
                target,
                allowed,
                max_steps,
            } => {
                let (Some(target), Some(allowed)) = (
                    compile_predicate(name, target, data)?,
                    compile_predicate(name, allowed, data)?,
                ) else {
                    return Ok(None);
                };
                CompiledModifier::EnvironmentScan {
                    step: direction.offset(),
                    target,
                    allowed,
                    max_steps: *max_steps,
                }
            }
            PlacementModifierDef::NoiseBasedCount {
                noise_to_count_ratio,
                noise_factor,
                noise_offset,
            } => CompiledModifier::NoiseBasedCount {
                noise_to_count_ratio: *noise_to_count_ratio,
                noise_factor: *noise_factor,
                noise_offset: *noise_offset,
            },
            PlacementModifierDef::NoiseThresholdCount {
                noise_level,
                below_noise,
                above_noise,
            } => CompiledModifier::NoiseThresholdCount {
                noise_level: *noise_level,
                below_noise: *below_noise,
                above_noise: *above_noise,
            },
            PlacementModifierDef::RandomOffset {
                xz_spread,
                y_spread,
            } if ints_ported(&[xz_spread, y_spread]) => CompiledModifier::RandomOffset {
                xz_spread: xz_spread.clone(),
                y_spread: y_spread.clone(),
            },
            PlacementModifierDef::SurfaceRelativeThresholdFilter {
                heightmap,
                min_inclusive,
                max_inclusive,
            } => CompiledModifier::SurfaceRelativeThresholdFilter {
                heightmap: *heightmap,
                min_inclusive: *min_inclusive,
                max_inclusive: *max_inclusive,
            },
            PlacementModifierDef::SurfaceWaterDepthFilter(depth) => {
                CompiledModifier::SurfaceWaterDepthFilter(*depth)
            }
            PlacementModifierDef::BlockPredicateFilter(predicate) => {
                match compile_predicate(name, predicate, data)? {
                    Some(predicate) => CompiledModifier::BlockPredicateFilter(predicate),
                    None => return Ok(None),
                }
            }
            PlacementModifierDef::FixedPlacement(positions) => {
                CompiledModifier::FixedPlacement(positions.clone())
            }
            _ => return Ok(None),
        });
    }
    Ok(Some(compiled))
}

/// Compiles a block predicate; None when it uses an unknown predicate type or `would_survive` with a block whose
/// survival rule is not ported yet.
pub fn compile_predicate(
    name: &str,
    predicate: &BlockPredicateDef,
    data: &WorldgenData,
) -> Result<Option<BlockPredicate>, WorldgenDataError> {
    let all = |predicates: &[BlockPredicateDef]| -> Result<Option<Vec<_>>, WorldgenDataError> {
        let mut compiled = Vec::with_capacity(predicates.len());
        for predicate in predicates {
            match compile_predicate(name, predicate, data)? {
                Some(predicate) => compiled.push(predicate),
                None => return Ok(None),
            }
        }
        Ok(Some(compiled))
    };
    Ok(Some(match predicate {
        BlockPredicateDef::MatchingBlocks { blocks, offset } => {
            let states = block_states();
            let mut membership = vec![false; states.len() as usize];
            for entry in blocks {
                if let Some(tag) = entry.strip_prefix('#') {
                    for (member, in_tag) in membership.iter_mut().zip(data.block_tag_states(tag)?) {
                        *member |= in_tag;
                    }
                    continue;
                }
                if states.default_state(entry).is_none() {
                    return Err(WorldgenDataError::InvalidDefinition {
                        context: name.to_string(),
                        message: format!("unknown block {entry}"),
                    });
                }
                for (id, member) in membership.iter_mut().enumerate() {
                    *member |= states
                        .properties(id as u32)
                        .is_some_and(|(block, _)| block == entry);
                }
            }
            BlockPredicate::States {
                states: StateSet::from_membership(&membership),
                offset: *offset,
            }
        }
        BlockPredicateDef::MatchingBlockTag { tag, offset } => BlockPredicate::States {
            states: StateSet::from_membership(&data.block_tag_states(tag)?),
            offset: *offset,
        },
        BlockPredicateDef::MatchingFluids { fluids, offset } => BlockPredicate::Fluids {
            fluids: fluid_mask(name, fluids, data)?,
            offset: *offset,
        },
        BlockPredicateDef::Solid { offset } => BlockPredicate::States {
            states: states_where(|state| block_behavior().is_solid(state)),
            offset: *offset,
        },
        BlockPredicateDef::Replaceable { offset } => BlockPredicate::States {
            states: states_where(|state| block_behavior().can_be_replaced(state)),
            offset: *offset,
        },
        BlockPredicateDef::HasSturdyFace { direction, offset } => {
            let face = direction.face();
            BlockPredicate::States {
                states: states_where(|state| block_behavior().is_face_sturdy(state, face)),
                offset: *offset,
            }
        }
        BlockPredicateDef::WouldSurvive { state, offset } => {
            match compile_survival(name, state, data)? {
                Some((rule, state)) => BlockPredicate::Survives {
                    rule: Box::new(rule),
                    state,
                    offset: *offset,
                },
                None => return Ok(None),
            }
        }
        BlockPredicateDef::InsideWorldBounds { offset } => {
            BlockPredicate::InsideWorldBounds { offset: *offset }
        }
        BlockPredicateDef::AllOf(predicates) => match all(predicates)? {
            Some(predicates) => BlockPredicate::AllOf(predicates),
            None => return Ok(None),
        },
        BlockPredicateDef::AnyOf(predicates) => match all(predicates)? {
            Some(predicates) => BlockPredicate::AnyOf(predicates),
            None => return Ok(None),
        },
        BlockPredicateDef::Not(predicate) => match compile_predicate(name, predicate, data)? {
            Some(predicate) => BlockPredicate::Not(Box::new(predicate)),
            None => return Ok(None),
        },
        BlockPredicateDef::True => BlockPredicate::True,
        BlockPredicateDef::Other { .. } => return Ok(None),
    }))
}

/// The states for which `test` holds.
pub fn states_where(test: impl Fn(u32) -> bool) -> StateSet {
    let membership: Vec<bool> = (0..block_states().len()).map(test).collect();
    StateSet::from_membership(&membership)
}

/// One bit per [`Fluid`] for a holder set of fluid identifiers or one `#`-prefixed fluid tag.
pub fn fluid_mask(
    name: &str,
    entries: &[String],
    data: &WorldgenData,
) -> Result<u8, WorldgenDataError> {
    let mut mask = 0;
    for entry in entries {
        let fluids = match entry.strip_prefix('#') {
            Some(tag) => data.fluid_tag_members(tag)?,
            None => HashSet::from([entry.clone()]),
        };
        for fluid in fluids {
            mask |= 1
                << match fluid.as_str() {
                    "minecraft:empty" => Fluid::Empty,
                    "minecraft:water" => Fluid::Water,
                    "minecraft:flowing_water" => Fluid::FlowingWater,
                    "minecraft:lava" => Fluid::Lava,
                    "minecraft:flowing_lava" => Fluid::FlowingLava,
                    other => {
                        return Err(WorldgenDataError::InvalidDefinition {
                            context: name.to_string(),
                            message: format!("unknown fluid {other}"),
                        });
                    }
                } as u8;
        }
    }
    Ok(mask)
}

fn int_ported(provider: &IntProviderDef) -> bool {
    match provider {
        IntProviderDef::Clamped { source, .. } => int_ported(source),
        IntProviderDef::WeightedList(entries) => entries.iter().all(|(entry, _)| int_ported(entry)),
        IntProviderDef::Other(_) => false,
        _ => true,
    }
}

fn height_ported(provider: &HeightProviderDef) -> bool {
    match provider {
        HeightProviderDef::WeightedList(entries) => {
            entries.iter().all(|(entry, _)| height_ported(entry))
        }
        HeightProviderDef::Other(_) => false,
        _ => true,
    }
}

/// Runs a placed feature's modifiers from `origin` and calls `place` at every position that comes out, in
/// vanilla's depth-first order. `allowed` holds the biomes that list the placed feature.
pub fn place_positions<W, R, F>(
    world: &mut W,
    allowed: &HashSet<u32>,
    modifiers: &[CompiledModifier],
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
    place: &mut F,
) where
    W: FeatureWorld,
    R: RandomSource,
    F: FnMut(&mut W, &mut WorldgenRandom<R>, [i32; 3]),
{
    let Some((modifier, rest)) = modifiers.split_first() else {
        place(world, random, origin);
        return;
    };
    let mut position = origin;
    match modifier {
        CompiledModifier::Count(count) => {
            let count = sample_int_provider(count, random).unwrap_or(0);
            return repeat(world, allowed, rest, random, origin, place, count);
        }
        CompiledModifier::NoiseBasedCount {
            noise_to_count_ratio,
            noise_factor,
            noise_offset,
        } => {
            let noise = biome_info_noise().get_value(
                f64::from(origin[0]) / noise_factor,
                f64::from(origin[2]) / noise_factor,
                false,
            );
            let count = ((noise + noise_offset) * f64::from(*noise_to_count_ratio)).ceil() as i32;
            return repeat(world, allowed, rest, random, origin, place, count);
        }
        CompiledModifier::NoiseThresholdCount {
            noise_level,
            below_noise,
            above_noise,
        } => {
            let noise = biome_info_noise().get_value(
                f64::from(origin[0]) / 200.0,
                f64::from(origin[2]) / 200.0,
                false,
            );
            let count = if noise < *noise_level {
                *below_noise
            } else {
                *above_noise
            };
            return repeat(world, allowed, rest, random, origin, place, count);
        }
        CompiledModifier::CountOnEveryLayer(count) => {
            // Vanilla collects every layer's positions before any of them is placed.
            let positions = count_on_every_layer(world, count, random, origin);
            for position in positions {
                place_positions(world, allowed, rest, random, position, place);
            }
            return;
        }
        CompiledModifier::FixedPlacement(positions) => {
            let chunk = (origin[0] >> 4, origin[2] >> 4);
            let positions = positions
                .iter()
                .filter(|position| (position[0] >> 4, position[2] >> 4) == chunk);
            for &position in positions {
                place_positions(world, allowed, rest, random, position, place);
            }
            return;
        }
        CompiledModifier::RarityFilter(chance) => {
            if random.next_float() >= 1.0 / *chance as f32 {
                return;
            }
        }
        CompiledModifier::InSquare => {
            position[0] += random.next_int_bounded(16);
            position[2] += random.next_int_bounded(16);
        }
        CompiledModifier::HeightRange(provider) => {
            let Some(y) = sample_height_provider(provider, world.min_y(), world.height(), random)
            else {
                return;
            };
            position[1] = y;
        }
        CompiledModifier::Biome => {
            if !allowed.contains(&world.biome(position[0], position[1], position[2])) {
                return;
            }
        }
        CompiledModifier::Heightmap(kind) => {
            let y = world.heightmap(*kind, position[0], position[2]);
            if y <= world.min_y() {
                return;
            }
            position[1] = y;
        }
        CompiledModifier::EnvironmentScan {
            step,
            target,
            allowed: search,
            max_steps,
        } => match environment_scan(world, *step, target, search, *max_steps, position) {
            Some(found) => position = found,
            None => return,
        },
        CompiledModifier::RandomOffset {
            xz_spread,
            y_spread,
        } => {
            position[0] += sample_int_provider(xz_spread, random).unwrap_or(0);
            position[1] += sample_int_provider(y_spread, random).unwrap_or(0);
            position[2] += sample_int_provider(xz_spread, random).unwrap_or(0);
        }
        CompiledModifier::SurfaceRelativeThresholdFilter {
            heightmap,
            min_inclusive,
            max_inclusive,
        } => {
            let surface = i64::from(world.heightmap(*heightmap, position[0], position[2]));
            let y = i64::from(position[1]);
            if surface + i64::from(*min_inclusive) > y || y > surface + i64::from(*max_inclusive) {
                return;
            }
        }
        CompiledModifier::SurfaceWaterDepthFilter(max_depth) => {
            let floor = world.heightmap(HeightmapType::OceanFloor, position[0], position[2]);
            let surface = world.heightmap(HeightmapType::WorldSurface, position[0], position[2]);
            if surface - floor > *max_depth {
                return;
            }
        }
        CompiledModifier::BlockPredicateFilter(predicate) => {
            if !predicate.test(world, position) {
                return;
            }
        }
    }
    place_positions(world, allowed, rest, random, position, place);
}

/// `RepeatingPlacement`: the same position `count` times, each through the remaining modifiers.
fn repeat<W, R, F>(
    world: &mut W,
    allowed: &HashSet<u32>,
    rest: &[CompiledModifier],
    random: &mut WorldgenRandom<R>,
    position: [i32; 3],
    place: &mut F,
    count: i32,
) where
    W: FeatureWorld,
    R: RandomSource,
    F: FnMut(&mut W, &mut WorldgenRandom<R>, [i32; 3]),
{
    for _ in 0..count {
        place_positions(world, allowed, rest, random, position, place);
    }
}

/// `CountOnEveryLayerPlacement`: for each layer, from the top down, picks `count` columns and finds the
/// ground of that layer in each, until a layer finds none.
fn count_on_every_layer<R: RandomSource>(
    world: &impl FeatureWorld,
    count: &IntProviderDef,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> Vec<[i32; 3]> {
    let mut positions = Vec::new();
    let mut layer = 0;
    loop {
        let mut found = false;
        // Vanilla samples the count again before every run.
        let mut run = 0;
        while run < sample_int_provider(count, random).unwrap_or(0) {
            run += 1;
            let x = random.next_int_bounded(16) + origin[0];
            let z = random.next_int_bounded(16) + origin[2];
            let top = world.heightmap(HeightmapType::MotionBlocking, x, z);
            if let Some(y) = ground_of_layer(world, x, top, z, layer) {
                positions.push([x, y, z]);
                found = true;
            }
        }
        layer += 1;
        if !found {
            return positions;
        }
    }
}

/// The Y above the `layer`-th solid block below an empty one, counting down from `top`; bedrock is never a
/// layer.
fn ground_of_layer(world: &impl FeatureWorld, x: i32, top: i32, z: i32, layer: i32) -> Option<i32> {
    let mut seen = 0;
    let mut above = layer_flags(world.block_state(x, top, z));
    let mut y = top;
    while y > world.min_y() {
        let below = layer_flags(world.block_state(x, y - 1, z));
        if below & EMPTY == 0 && above & EMPTY != 0 && below & BEDROCK == 0 {
            if seen == layer {
                return Some(y);
            }
            seen += 1;
        }
        above = below;
        y -= 1;
    }
    None
}

fn environment_scan(
    world: &impl FeatureWorld,
    step: [i32; 3],
    target: &BlockPredicate,
    allowed: &BlockPredicate,
    max_steps: i32,
    mut position: [i32; 3],
) -> Option<[i32; 3]> {
    if !allowed.test(world, position) {
        return None;
    }
    for _ in 0..max_steps {
        if target.test(world, position) {
            return Some(position);
        }
        position = add(position, step);
        if !(world.min_y()..world.min_y() + world.height()).contains(&position[1]) {
            return None;
        }
        if !allowed.test(world, position) {
            break;
        }
    }
    target.test(world, position).then_some(position)
}

const EMPTY: u8 = 1;
const BEDROCK: u8 = 2;

/// Per block state: air of any kind, water or lava (`CountOnEveryLayerPlacement.isEmpty`), and bedrock.
fn layer_flags(state: u32) -> u8 {
    static TABLE: OnceLock<Vec<u8>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let states = block_states();
        (0..states.len())
            .map(|id| match states.properties(id).map(|(name, _)| name) {
                Some(
                    "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
                    | "minecraft:water" | "minecraft:lava",
                ) => EMPTY,
                Some("minecraft:bedrock") => BEDROCK,
                _ => 0,
            })
            .collect()
    });
    table.get(state as usize).copied().unwrap_or(0)
}

fn add(a: [i32; 3], b: [i32; 3]) -> [i32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

/// `Biome.BIOME_INFO_NOISE`, which the noise count modifiers sample.
fn biome_info_noise() -> &'static PerlinSimplexNoise {
    static NOISE: OnceLock<PerlinSimplexNoise> = OnceLock::new();
    NOISE.get_or_init(|| PerlinSimplexNoise::new(&mut LegacyRandom::new(2345), &[0]))
}

/// `Mth.clamp` for floats: NaN passes through the upper bound, as `Math.min` lets it.
fn clamp_f32(value: f32, min: f32, max: f32) -> f32 {
    if value < min {
        min
    } else if value.is_nan() {
        value
    } else {
        value.min(max)
    }
}

/// `Mth.nextInt`: `min` when the range is empty.
fn next_int_between<R: RandomSource>(random: &mut WorldgenRandom<R>, min: i32, max: i32) -> i32 {
    if min >= max {
        min
    } else {
        random.next_int_bounded(max - min + 1) + min
    }
}

/// `Mth.randomBetweenInclusive`.
fn between_inclusive<R: RandomSource>(random: &mut WorldgenRandom<R>, min: i32, max: i32) -> i32 {
    random.next_int_bounded(max.wrapping_sub(min).wrapping_add(1)) + min
}

/// `WeightedList.getRandomOrThrow`: one draw below the total weight, then the entry whose share holds it.
pub(crate) fn pick_weighted<'a, T, R: RandomSource>(
    entries: &'a [(T, i32)],
    random: &mut WorldgenRandom<R>,
) -> Option<&'a T> {
    let total: i32 = entries.iter().map(|(_, weight)| weight).sum();
    if total <= 0 {
        return None;
    }
    let mut draw = random.next_int_bounded(total);
    for (entry, weight) in entries {
        draw -= weight;
        if draw < 0 {
            return Some(entry);
        }
    }
    None
}

/// Samples an IntProvider; None for provider types not ported yet.
pub fn sample_int_provider<R: RandomSource>(
    provider: &IntProviderDef,
    random: &mut WorldgenRandom<R>,
) -> Option<i32> {
    match provider {
        IntProviderDef::Constant(value) => Some(*value),
        IntProviderDef::Uniform {
            min_inclusive,
            max_inclusive,
        } => {
            if min_inclusive > max_inclusive {
                return None;
            }
            Some(between_inclusive(random, *min_inclusive, *max_inclusive))
        }
        IntProviderDef::BiasedToBottom {
            min_inclusive,
            max_inclusive,
        } => {
            let bound = random.next_int_bounded(max_inclusive - min_inclusive + 1) + 1;
            Some(min_inclusive + random.next_int_bounded(bound))
        }
        IntProviderDef::Clamped {
            source,
            min_inclusive,
            max_inclusive,
        } => Some(
            sample_int_provider(source, random)?
                .max(*min_inclusive)
                .min(*max_inclusive),
        ),
        IntProviderDef::ClampedNormal {
            mean,
            deviation,
            min_inclusive,
            max_inclusive,
        } => {
            let value = mean + random.next_gaussian() as f32 * deviation;
            Some(clamp_f32(value, *min_inclusive as f32, *max_inclusive as f32) as i32)
        }
        IntProviderDef::Trapezoid { min, max, plateau } => {
            if *plateau == 0 && *max == -*min {
                let a = random.next_int_bounded(max + 1);
                return Some(a - random.next_int_bounded(max + 1));
            }
            let range = max - min;
            if *plateau == range {
                return Some(between_inclusive(random, *min, *max));
            }
            let half = (range - plateau) / 2;
            let a = between_inclusive(random, 0, range - half);
            Some(min + a + between_inclusive(random, 0, half))
        }
        IntProviderDef::WeightedList(entries) => {
            let entry = pick_weighted(entries, random)?;
            sample_int_provider(entry, random)
        }
        IntProviderDef::Other(_) => None,
    }
}

/// Samples a HeightProvider; None for provider types not ported yet.
pub fn sample_height_provider<R: RandomSource>(
    provider: &HeightProviderDef,
    min_y: i32,
    height: i32,
    random: &mut WorldgenRandom<R>,
) -> Option<i32> {
    match provider {
        HeightProviderDef::Constant(anchor) => Some(anchor.resolve(min_y, height)),
        HeightProviderDef::Uniform {
            min_inclusive,
            max_inclusive,
        } => {
            let min = min_inclusive.resolve(min_y, height);
            let max = max_inclusive.resolve(min_y, height);
            if max < min {
                return Some(min);
            }
            Some(between_inclusive(random, min, max))
        }
        HeightProviderDef::Trapezoid {
            min_inclusive,
            max_inclusive,
            plateau,
        } => {
            let min = min_inclusive.resolve(min_y, height);
            let max = max_inclusive.resolve(min_y, height);
            if max < min {
                return Some(min);
            }
            let range = max - min;
            if range == 0 {
                return Some(min);
            }
            let plateau = (*plateau).clamp(0, range);
            let half = (range - plateau) / 2;
            let a = random.next_int_bounded(range - half + 1);
            let b = random.next_int_bounded(half + 1);
            Some(min + a + b)
        }
        HeightProviderDef::BiasedToBottom {
            min_inclusive,
            max_inclusive,
            inner,
        } => {
            let min = min_inclusive.resolve(min_y, height);
            let max = max_inclusive.resolve(min_y, height);
            if max - min - inner < 0 {
                return Some(min);
            }
            let bound = random.next_int_bounded(max - min - inner + 1);
            Some(random.next_int_bounded(bound + inner) + min)
        }
        HeightProviderDef::VeryBiasedToBottom {
            min_inclusive,
            max_inclusive,
            inner,
        } => {
            let min = min_inclusive.resolve(min_y, height);
            let max = max_inclusive.resolve(min_y, height);
            if max - min - inner < 0 {
                return Some(min);
            }
            let first = next_int_between(random, min + inner, max);
            let second = next_int_between(random, min, first - 1);
            Some(next_int_between(random, min, second - 1 + inner))
        }
        HeightProviderDef::WeightedList(entries) => {
            let entry = pick_weighted(entries, random)?;
            sample_height_provider(entry, min_y, height, random)
        }
        HeightProviderDef::Other(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::RegionFeatureWorld;
    use crate::generator::GeneratedChunk;
    use harpas_core::random::XoroshiroRandom;

    fn state(name: &str) -> u32 {
        block_states().default_state(name).unwrap()
    }

    /// A chunk at the origin with stone up to Y 9, air above, and the given columns' blocks overridden.
    fn world(blocks: &[([i32; 3], &str)]) -> RegionFeatureWorld {
        let mut chunk = GeneratedChunk::new(0, 0, -64, 384, 1);
        for y in -64..10 {
            for z in 0..16 {
                for x in 0..16 {
                    chunk.set_block_state(x, y, z, state("minecraft:stone"));
                }
            }
        }
        for &([x, y, z], name) in blocks {
            chunk.set_block_state(x as usize, y, z as usize, state(name));
        }
        RegionFeatureWorld::new([chunk], 0).unwrap()
    }

    fn positions(world: &mut RegionFeatureWorld, modifiers: &[CompiledModifier]) -> Vec<[i32; 3]> {
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(7));
        let mut out = Vec::new();
        place_positions(
            world,
            &HashSet::from([0]),
            modifiers,
            &mut random,
            [0, -64, 0],
            &mut |_, _, position| out.push(position),
        );
        out
    }

    #[test]
    fn fluids_follow_level_and_waterlogging() {
        let water = |level: &str| {
            block_states()
                .state("minecraft:water", [("level", level)])
                .unwrap()
                .0
        };
        assert_eq!(fluid(water("0")), Fluid::Water);
        assert_eq!(fluid(water("3")), Fluid::FlowingWater);
        assert_eq!(fluid(water("8")), Fluid::FlowingWater);
        assert_eq!(fluid(state("minecraft:lava")), Fluid::Lava);
        assert_eq!(fluid(state("minecraft:kelp")), Fluid::Water);
        assert_eq!(fluid(state("minecraft:stone")), Fluid::Empty);
        let logged = block_states()
            .state("minecraft:oak_stairs", [("waterlogged", "true")])
            .unwrap()
            .0;
        assert_eq!(fluid(logged), Fluid::Water);
    }

    #[test]
    fn heightmap_moves_to_the_surface_and_filters_by_water_depth() {
        let mut flat = world(&[]);
        let found = positions(
            &mut flat,
            &[
                CompiledModifier::InSquare,
                CompiledModifier::Heightmap(HeightmapType::WorldSurfaceWg),
                CompiledModifier::SurfaceWaterDepthFilter(0),
            ],
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0][1], 10);

        let water: Vec<_> = (0..16)
            .flat_map(|x| (0..16).map(move |z| ([x, 10, z], "minecraft:water")))
            .collect();
        let mut flooded = world(&water);
        let found = positions(
            &mut flooded,
            &[
                CompiledModifier::InSquare,
                CompiledModifier::SurfaceWaterDepthFilter(0),
            ],
        );
        assert!(found.is_empty(), "one block of water is deeper than zero");
    }

    #[test]
    fn count_on_every_layer_finds_each_floor() {
        // A cave between Y 0 and Y 4 gives every column a second layer below the surface at Y 10.
        let cave: Vec<_> = (0..16)
            .flat_map(|x| {
                (0..16).flat_map(move |z| (0..4).map(move |y| ([x, y, z], "minecraft:air")))
            })
            .collect();
        let mut caves = world(&cave);
        let found = positions(
            &mut caves,
            &[CompiledModifier::CountOnEveryLayer(
                IntProviderDef::Constant(2),
            )],
        );
        let heights: Vec<_> = found.iter().map(|position| position[1]).collect();
        assert_eq!(heights, [10, 10, 0, 0]);
    }

    #[test]
    fn environment_scan_stops_at_the_target() {
        let mut flat = world(&[]);
        let air = BlockPredicate::States {
            states: StateSet::from_membership(
                &(0..block_states().len())
                    .map(|id| id == state("minecraft:air"))
                    .collect::<Vec<_>>(),
            ),
            offset: [0; 3],
        };
        let scan = |max_steps| CompiledModifier::EnvironmentScan {
            step: [0, -1, 0],
            target: BlockPredicate::Not(Box::new(air.clone())),
            allowed: air.clone(),
            max_steps,
        };
        let start = CompiledModifier::HeightRange(HeightProviderDef::Constant(
            crate::surface::VerticalAnchor::Absolute(20),
        ));
        assert_eq!(
            positions(&mut flat, &[start.clone(), scan(32)]),
            [[0, 9, 0]]
        );
        assert!(positions(&mut flat, &[start, scan(5)]).is_empty());
    }

    #[test]
    fn trapezoid_and_weighted_providers_stay_in_range() {
        let mut random = WorldgenRandom::new(XoroshiroRandom::new(3));
        let symmetric = IntProviderDef::Trapezoid {
            min: -3,
            max: 3,
            plateau: 0,
        };
        let weighted = IntProviderDef::WeightedList(vec![
            (IntProviderDef::Constant(30), 9),
            (IntProviderDef::Constant(31), 1),
        ]);
        for _ in 0..1000 {
            let value = sample_int_provider(&symmetric, &mut random).unwrap();
            assert!((-3..=3).contains(&value));
            let value = sample_int_provider(&weighted, &mut random).unwrap();
            assert!(value == 30 || value == 31);
        }
    }
}
