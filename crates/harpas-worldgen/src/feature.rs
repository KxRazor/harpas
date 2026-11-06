//! Vanilla's FEATURES stage for the feature families ported so far.
//!
//! A chunk's placed features are chosen by the biome at each source chunk's center. Every feature is seeded
//! from the source chunk's decoration seed, its index in the step list and the generation step; the placement
//! modifiers then choose positions and the configured feature writes blocks through FeatureWorld.
//!
//! The source window is the 3x3 chunks a feature can write into around its source chunk. The complete
//! 17x17 vanilla loop and the remaining feature families follow.

use std::collections::{HashMap, HashSet};

use harpas_core::block::block_states;
use harpas_core::math::sin_f32;
use harpas_core::random::{RandomSource, WorldgenRandom, XoroshiroRandom};

use crate::data::{
    ConfiguredFeatureDef, FeatureConfigDef, FeatureRefDef, HeightProviderDef, IntProviderDef,
    OreFeatureDef, PlacementModifierDef, RuleTestDef, WorldgenData, WorldgenDataError,
};
use crate::generator::{GeneratedChunk, HeightmapType};
use crate::surface::resolve_block_state;

/// Number of vanilla generation steps (GenerationStep.Decoration).
pub const STEP_COUNT: usize = 11;

/// The world a FEATURES pass reads and writes.
pub trait FeatureWorld {
    /// Lowest block Y.
    fn min_y(&self) -> i32;
    /// Number of block Y values.
    fn height(&self) -> i32;
    /// Block state at world coordinates.
    fn block_state(&self, x: i32, y: i32, z: i32) -> u32;
    /// Sets a block state; returns whether the position is inside the writable region.
    fn set_block_state(&mut self, x: i32, y: i32, z: i32, state: u32) -> bool;
    /// Biome ID at a block position.
    fn biome(&self, x: i32, y: i32, z: i32) -> u32;
    /// First available Y for a heightmap at a column.
    fn heightmap(&self, kind: HeightmapType, x: i32, z: i32) -> i32;
    /// Biome at the center of a source chunk, which selects its feature lists.
    fn source_biome(&self, chunk_x: i32, chunk_z: i32) -> u32;
}

/// A single-chunk FeatureWorld, used by tests and by callers that pass one source chunk at a time.
pub struct ChunkFeatureWorld<'a> {
    chunk: &'a mut GeneratedChunk,
    biome: u32,
}

impl<'a> ChunkFeatureWorld<'a> {
    /// Wraps chunk as a world whose blocks all belong to biome.
    pub fn new(chunk: &'a mut GeneratedChunk, biome: u32) -> Self {
        Self { chunk, biome }
    }

    fn local(&self, x: i32, y: i32, z: i32) -> Option<(usize, i32, usize)> {
        let (x, z) = (x - self.chunk.chunk_x() * 16, z - self.chunk.chunk_z() * 16);
        if (0..16).contains(&x)
            && (0..16).contains(&z)
            && (self.chunk.min_y()..self.chunk.min_y() + self.chunk.height()).contains(&y)
        {
            Some((x as usize, y, z as usize))
        } else {
            None
        }
    }
}

impl FeatureWorld for ChunkFeatureWorld<'_> {
    fn min_y(&self) -> i32 {
        self.chunk.min_y()
    }

    fn height(&self) -> i32 {
        self.chunk.height()
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> u32 {
        self.local(x, y, z)
            .map_or_else(air_state, |(lx, y, lz)| self.chunk.block_state(lx, y, lz))
    }

    fn set_block_state(&mut self, x: i32, y: i32, z: i32, state: u32) -> bool {
        let Some((lx, y, lz)) = self.local(x, y, z) else {
            return false;
        };
        self.chunk.set_block_state(lx, y, lz, state);
        true
    }

    fn biome(&self, _x: i32, _y: i32, _z: i32) -> u32 {
        self.biome
    }

    fn heightmap(&self, kind: HeightmapType, x: i32, z: i32) -> i32 {
        self.local(x, 0, z)
            .map_or(self.chunk.min_y(), |(lx, _, lz)| {
                self.chunk.heightmaps().get(kind, lx, lz)
            })
    }

    fn source_biome(&self, _chunk_x: i32, _chunk_z: i32) -> u32 {
        self.biome
    }
}

/// A 3x3 chunk region for a FEATURES pass: every source chunk's features write into it.
///
/// Reads and writes outside the region are rejected. The biome is constant in this test-friendly region;
/// the engine supplies the zoomed biome through its own FeatureWorld implementation.
pub struct RegionFeatureWorld {
    chunks: HashMap<(i32, i32), GeneratedChunk>,
    biome: u32,
    min_y: i32,
    height: i32,
}

impl RegionFeatureWorld {
    /// Builds a region from chunks that share the same vertical bounds. At least one chunk is required.
    pub fn new(chunks: impl IntoIterator<Item = GeneratedChunk>, biome: u32) -> Option<Self> {
        let mut chunks = chunks.into_iter();
        let first = chunks.next()?;
        let (min_y, height) = (first.min_y(), first.height());
        let mut map = HashMap::new();
        map.insert((first.chunk_x(), first.chunk_z()), first);
        for chunk in chunks {
            if chunk.min_y() != min_y || chunk.height() != height {
                return None;
            }
            map.insert((chunk.chunk_x(), chunk.chunk_z()), chunk);
        }
        Some(Self {
            chunks: map,
            biome,
            min_y,
            height,
        })
    }

    /// A chunk in the region.
    pub fn chunk(&self, chunk_x: i32, chunk_z: i32) -> Option<&GeneratedChunk> {
        self.chunks.get(&(chunk_x, chunk_z))
    }

    /// Removes and returns a chunk from the region.
    pub fn into_chunk(mut self, chunk_x: i32, chunk_z: i32) -> Option<GeneratedChunk> {
        self.chunks.remove(&(chunk_x, chunk_z))
    }

    fn local(&self, x: i32, y: i32, z: i32) -> Option<((i32, i32), usize, i32, usize)> {
        if !(self.min_y..self.min_y + self.height).contains(&y) {
            return None;
        }
        let key = (x.div_euclid(16), z.div_euclid(16));
        self.chunks.contains_key(&key).then_some((
            key,
            x.rem_euclid(16) as usize,
            y,
            z.rem_euclid(16) as usize,
        ))
    }
}

impl FeatureWorld for RegionFeatureWorld {
    fn min_y(&self) -> i32 {
        self.min_y
    }

    fn height(&self) -> i32 {
        self.height
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> u32 {
        let Some((key, lx, y, lz)) = self.local(x, y, z) else {
            return air_state();
        };
        self.chunks
            .get(&key)
            .map_or_else(air_state, |chunk| chunk.block_state(lx, y, lz))
    }

    fn set_block_state(&mut self, x: i32, y: i32, z: i32, state: u32) -> bool {
        let Some((key, lx, y, lz)) = self.local(x, y, z) else {
            return false;
        };
        let Some(chunk) = self.chunks.get_mut(&key) else {
            return false;
        };
        chunk.set_block_state(lx, y, lz, state);
        true
    }

    fn biome(&self, _x: i32, _y: i32, _z: i32) -> u32 {
        self.biome
    }

    fn heightmap(&self, kind: HeightmapType, x: i32, z: i32) -> i32 {
        let key = (x.div_euclid(16), z.div_euclid(16));
        self.chunks.get(&key).map_or(self.min_y, |chunk| {
            chunk
                .heightmaps()
                .get(kind, x.rem_euclid(16) as usize, z.rem_euclid(16) as usize)
        })
    }

    fn source_biome(&self, _chunk_x: i32, _chunk_z: i32) -> u32 {
        self.biome
    }
}

fn air_state() -> u32 {
    block_states().default_state("minecraft:air").unwrap_or(0)
}

/// The placed features of every biome, resolved to positions in WorldgenData::placed_features.
#[derive(Debug, Clone)]
pub struct FeatureIndex {
    /// biome_features[biome][step] lists placed feature indices in data-pack order.
    biome_features: Vec<Vec<Vec<usize>>>,
    /// For every placed feature, the biome IDs that list it (the biome modifier).
    allowed_biomes: Vec<HashSet<u32>>,
    /// Placement modifiers of every placed feature.
    placements: Vec<Vec<PlacementModifierDef>>,
    /// Compiled ore configs; None for feature types or rule tests not ported yet.
    ores: Vec<Option<CompiledOre>>,
}

impl FeatureIndex {
    /// Resolves every biome's feature lists and compiles the ore features.
    pub fn new(data: &WorldgenData) -> Result<Self, WorldgenDataError> {
        let placed_count = data.placed_features.len();
        let mut biome_features = Vec::with_capacity(data.biomes.len());
        let mut allowed_biomes = vec![HashSet::new(); placed_count];
        for (biome_id, (biome_name, biome)) in data.biomes.iter().enumerate() {
            if biome.features.len() > STEP_COUNT {
                return Err(invalid(
                    biome_name,
                    format!(
                        "biome has {} generation steps, at most {STEP_COUNT}",
                        biome.features.len()
                    ),
                ));
            }
            let mut steps = Vec::with_capacity(biome.features.len());
            for features in &biome.features {
                let mut list = Vec::with_capacity(features.len());
                for id in features {
                    let index = data.placed_features.get_index_of(id).ok_or_else(|| {
                        invalid(biome_name, format!("unknown placed feature {id}"))
                    })?;
                    allowed_biomes[index].insert(biome_id as u32);
                    list.push(index);
                }
                steps.push(list);
            }
            biome_features.push(steps);
        }

        let mut placements = Vec::with_capacity(placed_count);
        let mut ores = Vec::with_capacity(placed_count);
        for (name, placed) in &data.placed_features {
            placements.push(placed.placement.clone());
            let configured = match &placed.feature {
                FeatureRefDef::Reference(id) => data
                    .configured_features
                    .get(id)
                    .ok_or_else(|| invalid(name, format!("unknown configured feature {id}")))?,
                FeatureRefDef::Inline(configured) => configured.as_ref(),
            };
            ores.push(compile_ore(name, configured, data)?);
        }

        Ok(Self {
            biome_features,
            allowed_biomes,
            placements,
            ores,
        })
    }

    /// Compiled ore config of a placed feature, if that feature is an ore Harpas places.
    pub fn ore(&self, placed: usize) -> Option<&CompiledOre> {
        self.ores.get(placed).and_then(Option::as_ref)
    }

    /// Placement modifiers of a placed feature.
    pub fn placement(&self, placed: usize) -> Option<&[PlacementModifierDef]> {
        self.placements.get(placed).map(Vec::as_slice)
    }

    /// Biome feature indices of a biome and generation step.
    pub fn features(&self, biome: u32, step: usize) -> Option<&[usize]> {
        self.biome_features
            .get(biome as usize)?
            .get(step)
            .map(Vec::as_slice)
    }

    /// Biome IDs that list a placed feature.
    pub fn allowed_biomes(&self, placed: usize) -> Option<&HashSet<u32>> {
        self.allowed_biomes.get(placed)
    }

    /// Number of biomes and placed features in the index.
    pub fn sizes(&self) -> (usize, usize) {
        (self.biome_features.len(), self.placements.len())
    }
}

/// A compiled minecraft:ore configuration.
#[derive(Debug, Clone)]
pub struct CompiledOre {
    /// Number of spheres along the vein.
    pub size: i32,
    /// Chance of discarding a block whose top neighbour is air.
    pub discard_chance_on_air_exposure: f32,
    /// Block states the vein may replace, in data-pack order.
    pub targets: Vec<OreTarget>,
}

/// One compiled ore target.
#[derive(Debug, Clone)]
pub struct OreTarget {
    /// Compiled rule test.
    pub rule: RuleTestRule,
    /// Block state placed where the rule matches.
    pub state: u32,
}

/// A compiled RuleTest.
#[derive(Debug, Clone)]
pub enum RuleTestRule {
    /// Block tag membership indexed by block state ID.
    Tag(Vec<bool>),
    /// One block by name.
    Block(String),
    /// One exact block state.
    BlockState(u32),
    /// One block with a probability, consuming a float only when the block matches.
    RandomBlock {
        /// Block name.
        block: String,
        /// Probability in [0, 1].
        probability: f32,
    },
    /// Every block.
    AlwaysTrue,
}

impl RuleTestRule {
    fn matches<R: RandomSource>(&self, state: u32, random: &mut WorldgenRandom<R>) -> bool {
        match self {
            Self::Tag(membership) => membership.get(state as usize).copied().unwrap_or(false),
            Self::Block(block) => block_states()
                .properties(state)
                .is_some_and(|(name, _)| name == block.as_str()),
            Self::BlockState(id) => state == *id,
            Self::RandomBlock { block, probability } => {
                block_states()
                    .properties(state)
                    .is_some_and(|(name, _)| name == block.as_str())
                    && random.next_float() < *probability
            }
            Self::AlwaysTrue => true,
        }
    }
}

fn compile_ore(
    name: &str,
    configured: &ConfiguredFeatureDef,
    data: &WorldgenData,
) -> Result<Option<CompiledOre>, WorldgenDataError> {
    // Scattered ore (ancient debris) has its own algorithm and follows with the Nether.
    if configured.feature_type != "minecraft:ore" {
        return Ok(None);
    }
    let FeatureConfigDef::Ore(ore) = &configured.config else {
        return Ok(None);
    };
    compile_ore_config(name, ore, data)
}

fn compile_ore_config(
    name: &str,
    ore: &OreFeatureDef,
    data: &WorldgenData,
) -> Result<Option<CompiledOre>, WorldgenDataError> {
    let mut targets = Vec::with_capacity(ore.targets.len());
    for target in &ore.targets {
        let rule = match &target.target {
            RuleTestDef::TagMatch(tag) => RuleTestRule::Tag(data.block_tag_states(tag)?),
            RuleTestDef::BlockMatch(block) => RuleTestRule::Block(resource_name(block)),
            RuleTestDef::BlockStateMatch(state) => {
                RuleTestRule::BlockState(resolve_block_state(state, name)?)
            }
            RuleTestDef::RandomBlockMatch { block, probability } => RuleTestRule::RandomBlock {
                block: resource_name(block),
                probability: *probability,
            },
            RuleTestDef::AlwaysTrue => RuleTestRule::AlwaysTrue,
            RuleTestDef::Other { .. } => return Ok(None),
        };
        targets.push(OreTarget {
            rule,
            state: resolve_block_state(&target.state, name)?,
        });
    }
    Ok(Some(CompiledOre {
        size: ore.size,
        discard_chance_on_air_exposure: ore.discard_chance_on_air_exposure,
        targets,
    }))
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
            Some(
                random.next_int_bounded(max_inclusive.wrapping_sub(*min_inclusive).wrapping_add(1))
                    + *min_inclusive,
            )
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
            Some(random.next_int_bounded(max.wrapping_sub(min).wrapping_add(1)) + min)
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
        HeightProviderDef::Other(_) => None,
    }
}

/// Places one ore feature; returns whether any block was placed.
///
/// This is vanilla's OreFeature.place and doPlace: a line of spheres whose bounds are sampled first, then one
/// pass over each sphere's box with vanilla's unit-sphere test and block rules.
pub fn place_ore<R: RandomSource>(
    world: &mut impl FeatureWorld,
    config: &CompiledOre,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    let size = config.size;
    if size <= 0 {
        return false;
    }
    let angle = random.next_float() * std::f32::consts::PI;
    let radius = size as f32 / 8.0;
    let cap = (((size as f32 / 16.0 * 2.0 + 1.0) / 2.0).ceil()) as i32;
    let sin = f64::from(angle).sin();
    let cos = f64::from(angle).cos();
    let x1 = f64::from(origin[0]) + sin * f64::from(radius);
    let x2 = f64::from(origin[0]) - sin * f64::from(radius);
    let z1 = f64::from(origin[2]) + cos * f64::from(radius);
    let z2 = f64::from(origin[2]) - cos * f64::from(radius);
    let y1 = origin[1] + random.next_int_bounded(3) - 2;
    let y2 = origin[1] + random.next_int_bounded(3) - 2;
    let ceil_radius = radius.ceil() as i32;
    let min_x = origin[0] - ceil_radius - cap;
    let min_y = origin[1] - 2 - cap;
    let min_z = origin[2] - ceil_radius - cap;
    let width = 2 * (ceil_radius + cap);
    let height = 2 * (2 + cap);
    for x in min_x..=min_x + width {
        for z in min_z..=min_z + width {
            if min_y > world.heightmap(HeightmapType::OceanFloorWg, x, z) {
                continue;
            }
            return do_place(
                world,
                config,
                random,
                [x1, x2],
                [z1, z2],
                [y1, y2],
                [min_x, min_y, min_z],
                width,
                height,
            );
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn do_place<R: RandomSource>(
    world: &mut impl FeatureWorld,
    config: &CompiledOre,
    random: &mut WorldgenRandom<R>,
    x: [f64; 2],
    z: [f64; 2],
    y: [i32; 2],
    min: [i32; 3],
    width: i32,
    height: i32,
) -> bool {
    let size = config.size;
    let mut spheres = Vec::with_capacity(size as usize);
    for index in 0..size {
        let t = index as f32 / size as f32;
        let center_x = x[0] + f64::from(t) * (x[1] - x[0]);
        let center_z = z[0] + f64::from(t) * (z[1] - z[0]);
        let center_y = f64::from(y[0]) + f64::from(t) * f64::from(y[1] - y[0]);
        let spread = random.next_double() * f64::from(size) / 16.0;
        let radius = ((f64::from(sin_f32(std::f32::consts::PI * t)) + 1.0) * spread + 1.0) / 2.0;
        spheres.push((center_x, center_y, center_z, radius));
    }
    let target_width = width.max(1);
    let target_height = height.max(1);
    let mut seen = vec![false; (target_width * target_height * target_width) as usize];
    let air = air_state();
    let mut placed = 0;
    for (center_x, center_y, center_z, radius) in spheres {
        let from_x = (center_x - radius).floor() as i32;
        let to_x = (center_x + radius).floor() as i32;
        let from_y = (center_y - radius).floor() as i32;
        let to_y = (center_y + radius).floor() as i32;
        let from_z = (center_z - radius).floor() as i32;
        let to_z = (center_z + radius).floor() as i32;
        for block_x in from_x..=to_x {
            for block_y in from_y..=to_y {
                for block_z in from_z..=to_z {
                    let local_x = block_x - min[0];
                    let local_y = block_y - min[1];
                    let local_z = block_z - min[2];
                    if !(0..target_width).contains(&local_x)
                        || !(0..target_height).contains(&local_y)
                        || !(0..target_width).contains(&local_z)
                    {
                        continue;
                    }
                    let dx = (f64::from(block_x) + 0.5 - center_x) / radius;
                    let dy = (f64::from(block_y) + 0.5 - center_y) / radius;
                    let dz = (f64::from(block_z) + 0.5 - center_z) / radius;
                    if dx * dx + dy * dy + dz * dz >= 1.0 {
                        continue;
                    }
                    let seen_index = (local_y * target_width + local_z) * target_width + local_x;
                    let seen = &mut seen[seen_index as usize];
                    if *seen {
                        continue;
                    }
                    *seen = true;
                    let state = world.block_state(block_x, block_y, block_z);
                    for target in &config.targets {
                        if !target.rule.matches(state, random) {
                            continue;
                        }
                        if config.discard_chance_on_air_exposure > 0.0
                            && random.next_float() < config.discard_chance_on_air_exposure
                            && world.block_state(block_x, block_y + 1, block_z) == air
                        {
                            break;
                        }
                        if world.set_block_state(block_x, block_y, block_z, target.state) {
                            placed += 1;
                        }
                        break;
                    }
                }
            }
        }
    }
    placed > 0
}

/// Applies the FEATURES stage to the target chunk from its 3x3 source window.
///
/// A source chunk's features can write up to one chunk away, so the target receives writes from itself
/// and its eight neighbours. The full 17x17 vanilla loop and the indirect effects of farther source
/// chunks follow with the complete stage.
pub fn apply_biome_decoration(
    world: &mut impl FeatureWorld,
    index: &FeatureIndex,
    world_seed: i64,
    chunk_x: i32,
    chunk_z: i32,
) -> Result<(), WorldgenDataError> {
    let mut random = WorldgenRandom::new(XoroshiroRandom::new(0));
    for dx in -1..=1 {
        for dz in -1..=1 {
            let source_x = chunk_x + dx;
            let source_z = chunk_z + dz;
            let biome = world.source_biome(source_x, source_z);
            let steps = index
                .biome_features
                .get(biome as usize)
                .ok_or_else(|| invalid("features", format!("unknown biome ID {biome}")))?;
            let decoration = random.set_decoration_seed(world_seed, source_x * 16, source_z * 16);
            let origin = [source_x * 16, world.min_y(), source_z * 16];
            for (step, features) in steps.iter().enumerate() {
                for (position, &placed) in features.iter().enumerate() {
                    let Some(ore) = index.ores.get(placed).and_then(Option::as_ref) else {
                        continue;
                    };
                    random.set_feature_seed(decoration, position as i32, step as i32);
                    place_placed_feature(world, index, placed, ore, &mut random, origin);
                }
            }
        }
    }
    Ok(())
}

fn place_placed_feature(
    world: &mut impl FeatureWorld,
    index: &FeatureIndex,
    placed: usize,
    ore: &CompiledOre,
    random: &mut WorldgenRandom<XoroshiroRandom>,
    origin: [i32; 3],
) -> bool {
    let modifiers = &index.placements[placed];
    let allowed = &index.allowed_biomes[placed];
    let mut start = 0;
    let count = match modifiers.first() {
        Some(PlacementModifierDef::Count(provider)) => {
            start = 1;
            match sample_int_provider(provider, random) {
                Some(count) if count > 0 => count,
                _ => return false,
            }
        }
        _ => 1,
    };
    'positions: for _ in 0..count {
        let mut position = origin;
        for modifier in &modifiers[start..] {
            match modifier {
                PlacementModifierDef::Count(_) => return false,
                PlacementModifierDef::InSquare => {
                    position[0] += random.next_int_bounded(16);
                    position[2] += random.next_int_bounded(16);
                }
                PlacementModifierDef::HeightRange(provider) => {
                    let Some(y) =
                        sample_height_provider(provider, world.min_y(), world.height(), random)
                    else {
                        return false;
                    };
                    position[1] = y;
                }
                PlacementModifierDef::Biome => {
                    if !allowed.contains(&world.biome(position[0], position[1], position[2])) {
                        continue 'positions;
                    }
                }
                PlacementModifierDef::Other { .. } => return false,
            }
        }
        if place_ore(world, ore, random, position) {
            return true;
        }
    }
    false
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{
        BiomeDef, BlockStateDef, OreFeatureDef, OreTargetDef, PlacedFeatureDef, TemperatureModifier,
    };
    use crate::surface::VerticalAnchor;
    use harpas_core::random::LegacyRandom;

    fn flat_stone_chunk_at(chunk_x: i32, chunk_z: i32) -> GeneratedChunk {
        let mut chunk = GeneratedChunk::new(chunk_x, chunk_z, -64, 384, 1);
        let stone = block_states().default_state("minecraft:stone").unwrap();
        for y in -64..64 {
            for z in 0..16 {
                for x in 0..16 {
                    chunk.set_block_state(x, y, z, stone);
                }
            }
        }
        chunk
    }

    fn flat_stone_chunk() -> GeneratedChunk {
        flat_stone_chunk_at(0, 0)
    }

    fn synthetic_ore_data() -> WorldgenData {
        let ore = ConfiguredFeatureDef {
            feature_type: "minecraft:ore".to_string(),
            config: FeatureConfigDef::Ore(OreFeatureDef {
                size: 9,
                discard_chance_on_air_exposure: 0.0,
                targets: vec![OreTargetDef {
                    target: RuleTestDef::BlockMatch("minecraft:stone".to_string()),
                    state: BlockStateDef::simple("minecraft:iron_ore"),
                }],
            }),
        };
        let placed = PlacedFeatureDef {
            feature: FeatureRefDef::Reference("example:ore".to_string()),
            placement: vec![
                PlacementModifierDef::Count(IntProviderDef::Constant(1)),
                PlacementModifierDef::InSquare,
                PlacementModifierDef::HeightRange(HeightProviderDef::Constant(
                    VerticalAnchor::Absolute(32),
                )),
                PlacementModifierDef::Biome,
            ],
        };
        let mut features = vec![Vec::new(); STEP_COUNT];
        features[6] = vec!["example:ore_placed".to_string()];
        let mut data = WorldgenData::default();
        data.configured_features
            .insert("example:ore".to_string(), ore);
        data.placed_features
            .insert("example:ore_placed".to_string(), placed);
        data.biomes.insert(
            "example:test".to_string(),
            BiomeDef {
                has_precipitation: true,
                temperature: 0.8,
                temperature_modifier: TemperatureModifier::None,
                downfall: 0.4,
                carvers: Vec::new(),
                features,
            },
        );
        data
    }

    #[test]
    fn height_providers_match_vanilla() {
        let mut random = WorldgenRandom::new(LegacyRandom::new(42));
        let constant = HeightProviderDef::Constant(VerticalAnchor::Absolute(5));
        for _ in 0..24 {
            assert_eq!(
                sample_height_provider(&constant, -64, 384, &mut random),
                Some(5)
            );
        }
        let uniform = HeightProviderDef::Uniform {
            min_inclusive: VerticalAnchor::Absolute(0),
            max_inclusive: VerticalAnchor::Absolute(10),
        };
        let expected_uniform = [
            7, 4, 8, 5, 3, 8, 1, 2, 10, 6, 0, 7, 2, 0, 2, 0, 4, 9, 0, 9, 10, 4, 3, 9,
        ];
        for value in expected_uniform {
            assert_eq!(
                sample_height_provider(&uniform, -64, 384, &mut random),
                Some(value)
            );
        }
        let triangle = HeightProviderDef::Trapezoid {
            min_inclusive: VerticalAnchor::Absolute(-24),
            max_inclusive: VerticalAnchor::Absolute(56),
            plateau: 0,
        };
        let expected_triangle = [
            -17, -17, 19, 37, 29, 3, -3, -9, 23, 37, 32, 15, 13, 20, 21, 6, -1, 31, -8, 39, 29, 9,
            18, 43,
        ];
        for value in expected_triangle {
            assert_eq!(
                sample_height_provider(&triangle, -64, 384, &mut random),
                Some(value)
            );
        }
        let plateau = HeightProviderDef::Trapezoid {
            min_inclusive: VerticalAnchor::Absolute(-10),
            max_inclusive: VerticalAnchor::Absolute(10),
            plateau: 3,
        };
        let expected_plateau = [
            -3, -5, 0, -1, -8, 2, -3, -5, -1, 0, -3, -3, 8, -3, -6, -2, -2, 0, 0, -6, -3, 4, -2, 7,
        ];
        for value in expected_plateau {
            assert_eq!(
                sample_height_provider(&plateau, -64, 384, &mut random),
                Some(value)
            );
        }
        let inverted = HeightProviderDef::Trapezoid {
            min_inclusive: VerticalAnchor::Absolute(10),
            max_inclusive: VerticalAnchor::Absolute(-10),
            plateau: 0,
        };
        let mut after = random.clone();
        assert_eq!(
            sample_height_provider(&inverted, -64, 384, &mut random),
            Some(10)
        );
        assert_eq!(random.next_int(), after.next_int());
    }

    #[test]
    fn ore_feature_matches_the_legacy_probe() {
        let mut chunk = flat_stone_chunk();
        let iron = block_states().default_state("minecraft:iron_ore").unwrap();
        let mut world = ChunkFeatureWorld::new(&mut chunk, 0);
        let config = CompiledOre {
            size: 9,
            discard_chance_on_air_exposure: 0.0,
            targets: vec![OreTarget {
                rule: RuleTestRule::Block("minecraft:stone".to_string()),
                state: iron,
            }],
        };
        let mut random = WorldgenRandom::new(LegacyRandom::new(42));
        assert!(place_ore(&mut world, &config, &mut random, [8, 32, 8]));
        let mut found = Vec::new();
        for y in -64..64 {
            for z in 0..16 {
                for x in 0..16 {
                    if world.block_state(x, y, z) == iron {
                        found.push((x, y, z));
                    }
                }
            }
        }
        found.sort_unstable();
        assert_eq!(found, vec![(7, 29, 8), (7, 30, 8), (8, 29, 7), (8, 30, 7)]);
    }

    #[test]
    fn scheduler_places_biome_ores() {
        let ore = ConfiguredFeatureDef {
            feature_type: "minecraft:ore".to_string(),
            config: FeatureConfigDef::Ore(OreFeatureDef {
                size: 9,
                discard_chance_on_air_exposure: 0.0,
                targets: vec![OreTargetDef {
                    target: RuleTestDef::BlockMatch("minecraft:stone".to_string()),
                    state: BlockStateDef::simple("minecraft:iron_ore"),
                }],
            }),
        };
        let placed = PlacedFeatureDef {
            feature: FeatureRefDef::Reference("example:ore".to_string()),
            placement: vec![
                PlacementModifierDef::Count(IntProviderDef::Constant(1)),
                PlacementModifierDef::InSquare,
                PlacementModifierDef::HeightRange(HeightProviderDef::Constant(
                    VerticalAnchor::Absolute(32),
                )),
                PlacementModifierDef::Biome,
            ],
        };
        let mut features = vec![Vec::new(); STEP_COUNT];
        features[6] = vec!["example:ore_placed".to_string()];
        let mut data = WorldgenData::default();
        data.configured_features
            .insert("example:ore".to_string(), ore);
        data.placed_features
            .insert("example:ore_placed".to_string(), placed);
        data.biomes.insert(
            "example:test".to_string(),
            BiomeDef {
                has_precipitation: true,
                temperature: 0.8,
                temperature_modifier: TemperatureModifier::None,
                downfall: 0.4,
                carvers: Vec::new(),
                features,
            },
        );
        let index = FeatureIndex::new(&data).expect("compile feature index");
        let mut chunk = flat_stone_chunk();
        let mut world = ChunkFeatureWorld::new(&mut chunk, 0);
        apply_biome_decoration(&mut world, &index, 42, 0, 0).expect("apply features");
        let iron = block_states().default_state("minecraft:iron_ore").unwrap();
        let mut count = 0;
        for x in 0..16 {
            for z in 0..16 {
                for y in 28..=34 {
                    if world.block_state(x, y, z) == iron {
                        count += 1;
                    }
                }
            }
        }
        assert!(count > 0, "the ore feature placed nothing");
    }

    #[test]
    fn region_world_routes_reads_writes_and_heightmaps() {
        let chunks = vec![flat_stone_chunk_at(0, 0), flat_stone_chunk_at(1, 0)];
        let mut world = RegionFeatureWorld::new(chunks, 0).expect("region");
        let iron = block_states().default_state("minecraft:iron_ore").unwrap();
        assert!(world.set_block_state(20, 30, 5, iron));
        assert_eq!(world.block_state(20, 30, 5), iron);
        assert!(!world.set_block_state(40, 30, 5, iron));
        assert_eq!(world.heightmap(HeightmapType::OceanFloorWg, 20, 5), 64);
        assert!(world.chunk(0, 0).is_some());
        assert!(world.into_chunk(1, 0).is_some());
    }

    #[test]
    fn scheduler_runs_the_three_by_three_source_window() {
        let data = synthetic_ore_data();
        let index = FeatureIndex::new(&data).expect("compile feature index");
        let mut chunks = Vec::new();
        for x in -1..=1 {
            for z in -1..=1 {
                chunks.push(flat_stone_chunk_at(x, z));
            }
        }
        let mut world = RegionFeatureWorld::new(chunks, 0).expect("region");
        apply_biome_decoration(&mut world, &index, 42, 0, 0).expect("apply features");
        let iron = block_states().default_state("minecraft:iron_ore").unwrap();
        for x in -1..=1 {
            for z in -1..=1 {
                let chunk = world.chunk(x, z).expect("source chunk");
                let mut count = 0;
                for lx in 0..16 {
                    for lz in 0..16 {
                        for y in 28..=34 {
                            if chunk.block_state(lx, y, lz) == iron {
                                count += 1;
                            }
                        }
                    }
                }
                assert!(count > 0, "source chunk ({x},{z}) placed no ore");
            }
        }
    }

    #[test]
    fn feature_index_rejects_unknown_biome_features() {
        let mut data = WorldgenData::default();
        data.biomes.insert(
            "example:test".to_string(),
            BiomeDef {
                has_precipitation: true,
                temperature: 0.8,
                temperature_modifier: TemperatureModifier::None,
                downfall: 0.4,
                carvers: Vec::new(),
                features: vec![vec!["example:missing".to_string()]],
            },
        );
        assert!(FeatureIndex::new(&data).is_err());
    }
}
