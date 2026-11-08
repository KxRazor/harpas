//! Vanilla's FEATURES stage for the feature families ported so far (see [`crate::configured`]).
//!
//! Every chunk decorates itself once, reading and writing the 3x3 chunks around it. Its features are the
//! ones listed by any biome in those chunks, run in the world's global order for each generation step, and
//! each is seeded from the chunk's decoration seed, its global index and the step. The placement modifiers
//! then choose positions and the configured feature writes blocks through FeatureWorld.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use harpas_core::block::block_states;
use harpas_core::math::sin_f32;
use harpas_core::nbt::Compound;
use harpas_core::random::{RandomSource, WorldgenRandom, XoroshiroRandom};

use crate::biome::{BiomeManager, overworld_biome_parameters};
use crate::configured::{CompiledPlaced, FeatureCompiler, place_feature};
use crate::data::{
    ConfiguredFeatureDef, FeatureConfigDef, FeatureRefDef, OreFeatureDef, PlacementModifierDef,
    RuleTestDef, WorldgenData, WorldgenDataError,
};
use crate::generator::{GeneratedChunk, HeightmapType};
use crate::placement::{CompiledModifier, compile_placement, place_positions};
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
    /// Biomes stored in the chunks within one chunk of a target, which select its features.
    fn region_biomes(&self, chunk_x: i32, chunk_z: i32) -> HashSet<u32>;
    /// Whether the column holding a block may be written (`WorldGenRegion.ensureCanWrite`); a feature whose
    /// origin may not be written does not run.
    fn can_write(&self, x: i32, z: i32) -> bool;
    /// Marks a block for postprocessing once its chunk is complete (`markPosForPostprocessing`).
    fn mark_postprocessing(&mut self, x: i32, y: i32, z: i32);
    /// Sets the saved data of the block entity at a block that was just placed, as a feature fills it in after
    /// `getBlockEntity`; returns whether the position is inside the writable region.
    fn set_block_entity(&mut self, x: i32, y: i32, z: i32, entity: Compound) -> bool;
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

    fn region_biomes(&self, _chunk_x: i32, _chunk_z: i32) -> HashSet<u32> {
        HashSet::from([self.biome])
    }

    fn can_write(&self, x: i32, z: i32) -> bool {
        self.local(x, self.chunk.min_y(), z).is_some()
    }

    fn mark_postprocessing(&mut self, x: i32, y: i32, z: i32) {
        if let Some((lx, y, lz)) = self.local(x, y, z) {
            self.chunk.mark_fluid_postprocessing(lx as u8, y, lz as u8);
        }
    }

    fn set_block_entity(&mut self, x: i32, y: i32, z: i32, entity: Compound) -> bool {
        let Some((lx, y, lz)) = self.local(x, y, z) else {
            return false;
        };
        self.chunk.set_block_entity(lx as u8, y, lz as u8, entity);
        true
    }
}

/// Where a RegionFeatureWorld takes its biomes from.
enum RegionBiomes {
    /// One biome everywhere.
    Fixed(u32),
    /// The biomes stored in the chunks, zoomed to block positions.
    Stored(BiomeManager),
}

/// The 3x3 chunks a FEATURES pass reads and writes.
///
/// Reads and writes outside the region are rejected.
pub struct RegionFeatureWorld {
    chunks: HashMap<(i32, i32), GeneratedChunk>,
    biomes: RegionBiomes,
    min_y: i32,
    height: i32,
}

impl RegionFeatureWorld {
    /// Builds a region of chunks that share the same vertical bounds and one biome. At least one chunk is
    /// required.
    pub fn new(chunks: impl IntoIterator<Item = GeneratedChunk>, biome: u32) -> Option<Self> {
        Self::build(chunks, RegionBiomes::Fixed(biome))
    }

    /// Builds a region that reads the biomes stored in its chunks, as a world of that seed does.
    pub fn with_stored_biomes(
        chunks: impl IntoIterator<Item = GeneratedChunk>,
        world_seed: i64,
    ) -> Option<Self> {
        Self::build(
            chunks,
            RegionBiomes::Stored(BiomeManager::from_world_seed(world_seed)),
        )
    }

    fn build(
        chunks: impl IntoIterator<Item = GeneratedChunk>,
        biomes: RegionBiomes,
    ) -> Option<Self> {
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
            biomes,
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

    /// Returns every chunk of the region, in no particular order.
    pub fn into_chunks(self) -> impl Iterator<Item = GeneratedChunk> {
        self.chunks.into_values()
    }

    /// The biome stored at quart coordinates, with Y clamped to the world as chunks do.
    fn stored_biome(&self, quart_x: i32, quart_y: i32, quart_z: i32) -> u32 {
        let Some(chunk) = self.chunks.get(&(quart_x >> 2, quart_z >> 2)) else {
            return 0;
        };
        let min_quart = self.min_y >> 2;
        let quart = (quart_y - min_quart).clamp(0, (self.height >> 2) - 1) as usize;
        chunk.sections()[quart >> 2].biome(
            (quart_x & 3) as usize,
            quart & 3,
            (quart_z & 3) as usize,
        )
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

    fn biome(&self, x: i32, y: i32, z: i32) -> u32 {
        match &self.biomes {
            RegionBiomes::Fixed(biome) => *biome,
            RegionBiomes::Stored(manager) => {
                manager.get_biome(x, y, z, |qx, qy, qz| self.stored_biome(qx, qy, qz))
            }
        }
    }

    fn heightmap(&self, kind: HeightmapType, x: i32, z: i32) -> i32 {
        let key = (x.div_euclid(16), z.div_euclid(16));
        self.chunks.get(&key).map_or(self.min_y, |chunk| {
            chunk
                .heightmaps()
                .get(kind, x.rem_euclid(16) as usize, z.rem_euclid(16) as usize)
        })
    }

    fn region_biomes(&self, chunk_x: i32, chunk_z: i32) -> HashSet<u32> {
        let RegionBiomes::Fixed(biome) = self.biomes else {
            let mut biomes = HashSet::new();
            for dx in -1..=1 {
                for dz in -1..=1 {
                    let Some(chunk) = self.chunks.get(&(chunk_x + dx, chunk_z + dz)) else {
                        continue;
                    };
                    for section in chunk.sections() {
                        for y in 0..4 {
                            for z in 0..4 {
                                for x in 0..4 {
                                    biomes.insert(section.biome(x, y, z));
                                }
                            }
                        }
                    }
                }
            }
            return biomes;
        };
        HashSet::from([biome])
    }

    fn can_write(&self, x: i32, z: i32) -> bool {
        self.chunks
            .contains_key(&(x.div_euclid(16), z.div_euclid(16)))
    }

    fn mark_postprocessing(&mut self, x: i32, y: i32, z: i32) {
        if let Some((key, lx, y, lz)) = self.local(x, y, z)
            && let Some(chunk) = self.chunks.get_mut(&key)
        {
            chunk.mark_fluid_postprocessing(lx as u8, y, lz as u8);
        }
    }

    fn set_block_entity(&mut self, x: i32, y: i32, z: i32, entity: Compound) -> bool {
        let Some((key, lx, y, lz)) = self.local(x, y, z) else {
            return false;
        };
        let Some(chunk) = self.chunks.get_mut(&key) else {
            return false;
        };
        chunk.set_block_entity(lx as u8, y, lz as u8, entity);
        true
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
    /// steps[step] lists the world's placed features in the order vanilla runs them.
    steps: Vec<Vec<usize>>,
    /// biome_indices[biome][step] lists positions in steps[step]; empty for biomes the world cannot have.
    biome_indices: Vec<Vec<Vec<usize>>>,
    /// For every placed feature, the biome IDs that list it (the biome modifier).
    allowed_biomes: Vec<HashSet<u32>>,
    /// Placement modifiers of every placed feature.
    placements: Vec<Vec<PlacementModifierDef>>,
    /// Compiled placement modifiers; None when one of them is not ported yet.
    modifiers: Vec<Option<Vec<CompiledModifier>>>,
    /// Compiled placed features; None when part of one is not ported yet.
    compiled: Vec<Option<Arc<CompiledPlaced>>>,
    /// Compiled ore configs; None for feature types or rule tests not ported yet.
    ores: Vec<Option<CompiledOre>>,
}

impl FeatureIndex {
    /// Indexes the features of a world with the normal Overworld's biome source.
    pub fn overworld(data: &WorldgenData) -> Result<Self, WorldgenDataError> {
        let mut biomes = Vec::new();
        let mut seen = HashSet::new();
        for (_, name) in overworld_biome_parameters() {
            if seen.insert(name) {
                let id = data
                    .biomes
                    .get_index_of(name)
                    .ok_or_else(|| invalid("features", format!("unknown biome {name}")))?;
                biomes.push(id as u32);
            }
        }
        Self::new(data, &biomes)
    }

    /// Resolves every biome's feature lists, orders the features of `possible_biomes`, the IDs a world's
    /// biome source can produce in its own order, and compiles the ore features.
    pub fn new(data: &WorldgenData, possible_biomes: &[u32]) -> Result<Self, WorldgenDataError> {
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
        let mut modifiers = Vec::with_capacity(placed_count);
        let mut ores = Vec::with_capacity(placed_count);
        let mut compiled = Vec::with_capacity(placed_count);
        let mut compiler = FeatureCompiler::new(data);
        for (name, placed) in &data.placed_features {
            compiled.push(compiler.placed(name)?);
            placements.push(placed.placement.clone());
            modifiers.push(compile_placement(name, &placed.placement, data)?);
            let configured = match &placed.feature {
                FeatureRefDef::Reference(id) => data
                    .configured_features
                    .get(id)
                    .ok_or_else(|| invalid(name, format!("unknown configured feature {id}")))?,
                FeatureRefDef::Inline(configured) => configured.as_ref(),
            };
            ores.push(compile_ore(name, configured, data)?);
        }

        let steps = sort_features(&biome_features, possible_biomes)?;
        let positions: Vec<HashMap<usize, usize>> = steps
            .iter()
            .map(|step| step.iter().enumerate().map(|(i, &f)| (f, i)).collect())
            .collect();
        let mut biome_indices = vec![Vec::new(); biome_features.len()];
        for &biome in possible_biomes {
            let lists = biome_features
                .get(biome as usize)
                .ok_or_else(|| invalid("features", format!("unknown biome ID {biome}")))?;
            biome_indices[biome as usize] = lists
                .iter()
                .enumerate()
                .map(|(step, list)| list.iter().map(|f| positions[step][f]).collect())
                .collect();
        }

        Ok(Self {
            biome_features,
            steps,
            biome_indices,
            allowed_biomes,
            placements,
            modifiers,
            compiled,
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

    /// Compiled placement modifiers of a placed feature, if Harpas applies all of them.
    pub fn modifiers(&self, placed: usize) -> Option<&[CompiledModifier]> {
        self.modifiers.get(placed)?.as_deref()
    }

    /// A placed feature with its configured feature, if Harpas places all of it.
    pub fn compiled(&self, placed: usize) -> Option<&Arc<CompiledPlaced>> {
        self.compiled.get(placed)?.as_ref()
    }

    /// Biome feature indices of a biome and generation step.
    pub fn features(&self, biome: u32, step: usize) -> Option<&[usize]> {
        self.biome_features
            .get(biome as usize)?
            .get(step)
            .map(Vec::as_slice)
    }

    /// Placed features of a generation step in the order they run; a feature's position is its seed index.
    pub fn step_order(&self, step: usize) -> Option<&[usize]> {
        self.steps.get(step).map(Vec::as_slice)
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
    /// Chance of discarding a block that touches air.
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

/// Merges the biomes' ordered lists into one order for each generation step.
///
/// Every list adds "runs before" edges, also across steps, and features with no order between them keep a
/// fixed one: a depth-first topological sort that visits nodes by step and then by the first time a feature
/// was seen. Vanilla's lists agree with each other, so a cycle is an error.
fn sort_features(
    biome_features: &[Vec<Vec<usize>>],
    possible_biomes: &[u32],
) -> Result<Vec<Vec<usize>>, WorldgenDataError> {
    /// A feature in a step, ordered by step and then by first appearance.
    type Node = (usize, usize);

    fn visit(
        node: Node,
        graph: &BTreeMap<Node, BTreeSet<Node>>,
        visited: &mut HashSet<Node>,
        visiting: &mut HashSet<Node>,
        sorted: &mut Vec<Node>,
    ) -> Result<(), WorldgenDataError> {
        if visited.contains(&node) {
            return Ok(());
        }
        if !visiting.insert(node) {
            return Err(invalid("features", "feature order has a cycle"));
        }
        for &next in graph.get(&node).into_iter().flatten() {
            visit(next, graph, visited, visiting, sorted)?;
        }
        visiting.remove(&node);
        visited.insert(node);
        sorted.push(node);
        Ok(())
    }

    let mut first_seen = HashMap::new();
    let mut features = Vec::new();
    let mut graph: BTreeMap<Node, BTreeSet<Node>> = BTreeMap::new();
    let mut step_count = 0;
    for &biome in possible_biomes {
        let lists = biome_features
            .get(biome as usize)
            .ok_or_else(|| invalid("features", format!("unknown biome ID {biome}")))?;
        step_count = step_count.max(lists.len());
        let mut previous: Option<Node> = None;
        for (step, list) in lists.iter().enumerate() {
            for &feature in list {
                let seen = *first_seen.entry(feature).or_insert_with(|| {
                    features.push(feature);
                    features.len() - 1
                });
                let node = (step, seen);
                graph.entry(node).or_default();
                if let Some(previous) = previous {
                    graph.entry(previous).or_default().insert(node);
                }
                previous = Some(node);
            }
        }
    }

    let (mut visited, mut visiting, mut sorted) = (HashSet::new(), HashSet::new(), Vec::new());
    for &node in graph.keys() {
        visit(node, &graph, &mut visited, &mut visiting, &mut sorted)?;
    }
    let mut steps = vec![Vec::new(); step_count];
    for &(step, seen) in sorted.iter().rev() {
        steps[step].push(features[seen]);
    }
    Ok(steps)
}

pub(crate) fn compile_ore(
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
    // A sphere inside another is dropped; this changes the order blocks are visited in, not which ones.
    for first in 0..spheres.len() {
        for second in first + 1..spheres.len() {
            let (a, b) = (spheres[first], spheres[second]);
            if a.3 <= 0.0 || b.3 <= 0.0 {
                continue;
            }
            let (dx, dy, dz, dr) = (a.0 - b.0, a.1 - b.1, a.2 - b.2, a.3 - b.3);
            if dr * dr > dx * dx + dy * dy + dz * dz {
                if dr > 0.0 {
                    spheres[second].3 = -1.0;
                } else {
                    spheres[first].3 = -1.0;
                }
            }
        }
    }
    // The visited set is indexed from the box's lower corner, which also bounds each sphere; the upper side
    // is not bounded, so blocks past it share indices with others, as in vanilla.
    let mut seen = vec![false; (width * height * width).max(0) as usize];
    let mut placed = 0;
    let max_y = world.min_y() + world.height();
    for (center_x, center_y, center_z, radius) in spheres {
        if radius < 0.0 {
            continue;
        }
        let from_x = ((center_x - radius).floor() as i32).max(min[0]);
        let from_y = ((center_y - radius).floor() as i32).max(min[1]);
        let from_z = ((center_z - radius).floor() as i32).max(min[2]);
        let to_x = ((center_x + radius).floor() as i32).max(from_x);
        let to_y = ((center_y + radius).floor() as i32).max(from_y);
        let to_z = ((center_z + radius).floor() as i32).max(from_z);
        for block_x in from_x..=to_x {
            for block_y in from_y..=to_y {
                for block_z in from_z..=to_z {
                    let local_x = block_x - min[0];
                    let local_y = block_y - min[1];
                    let local_z = block_z - min[2];
                    if !(world.min_y()..max_y).contains(&block_y) {
                        continue;
                    }
                    let dx = (f64::from(block_x) + 0.5 - center_x) / radius;
                    let dy = (f64::from(block_y) + 0.5 - center_y) / radius;
                    let dz = (f64::from(block_z) + 0.5 - center_z) / radius;
                    if dx * dx + dy * dy + dz * dz >= 1.0 {
                        continue;
                    }
                    let seen_index =
                        (local_x + local_y * width + local_z * width * height) as usize;
                    if seen_index >= seen.len() {
                        seen.resize(seen_index + 1, false);
                    }
                    let seen = &mut seen[seen_index];
                    if *seen {
                        continue;
                    }
                    *seen = true;
                    let state = world.block_state(block_x, block_y, block_z);
                    for target in &config.targets {
                        if !target.rule.matches(state, random) {
                            continue;
                        }
                        let discard = config.discard_chance_on_air_exposure;
                        let check_air =
                            discard >= 1.0 || (discard > 0.0 && random.next_float() < discard);
                        if check_air && touches_air(world, block_x, block_y, block_z) {
                            continue;
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

/// Whether any of a block's six neighbors is air, of any kind.
fn touches_air(world: &impl FeatureWorld, x: i32, y: i32, z: i32) -> bool {
    const SIDES: [[i32; 3]; 6] = [
        [0, -1, 0],
        [0, 1, 0],
        [0, 0, -1],
        [0, 0, 1],
        [-1, 0, 0],
        [1, 0, 0],
    ];
    SIDES.iter().any(|side| {
        let state = world.block_state(x + side[0], y + side[1], z + side[2]);
        block_states().properties(state).is_some_and(|(name, _)| {
            matches!(
                name,
                "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
            )
        })
    })
}

/// Decorates one chunk: vanilla's `applyBiomeDecoration`, without the structures of each step.
///
/// The chunk's features write up to one chunk away, so a chunk's final blocks also depend on the passes of
/// its eight neighbors. The world must hold the 3x3 chunks around the target.
pub fn apply_biome_decoration(
    world: &mut impl FeatureWorld,
    index: &FeatureIndex,
    world_seed: i64,
    chunk_x: i32,
    chunk_z: i32,
) -> Result<(), WorldgenDataError> {
    let mut random = WorldgenRandom::new(XoroshiroRandom::new(0));
    let origin = [chunk_x * 16, world.min_y(), chunk_z * 16];
    let decoration = random.set_decoration_seed(world_seed, origin[0], origin[2]);
    let biomes = world.region_biomes(chunk_x, chunk_z);
    for (step, order) in index.steps.iter().enumerate() {
        let mut positions = BTreeSet::<usize>::new();
        for &biome in &biomes {
            let lists = index
                .biome_indices
                .get(biome as usize)
                .ok_or_else(|| invalid("features", format!("unknown biome ID {biome}")))?;
            positions.extend(lists.get(step).into_iter().flatten());
        }
        for position in positions {
            let placed = order[position];
            let Some(compiled) = index.compiled(placed) else {
                continue;
            };
            random.set_feature_seed(decoration, position as i32, step as i32);
            place_positions(
                world,
                &index.allowed_biomes[placed],
                &compiled.modifiers,
                &mut random,
                origin,
                &mut |world, random, position| {
                    place_feature(world, &compiled.feature, random, position);
                },
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{
        BiomeDef, BlockStateDef, HeightProviderDef, IntProviderDef, OreFeatureDef, OreTargetDef,
        PlacedFeatureDef, TemperatureModifier,
    };
    use crate::placement::sample_height_provider;
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
        let index = FeatureIndex::new(&data, &[0]).expect("compile feature index");
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
    fn a_chunk_decorates_once_from_its_own_seed() {
        let data = synthetic_ore_data();
        let index = FeatureIndex::new(&data, &[0]).expect("compile feature index");
        let iron = block_states().default_state("minecraft:iron_ore").unwrap();
        let ores = |target: (i32, i32)| {
            let mut chunks = Vec::new();
            for x in -1..=1 {
                for z in -1..=1 {
                    chunks.push(flat_stone_chunk_at(target.0 + x, target.1 + z));
                }
            }
            let mut world = RegionFeatureWorld::new(chunks, 0).expect("region");
            apply_biome_decoration(&mut world, &index, 42, target.0, target.1)
                .expect("apply features");
            let mut found = BTreeSet::new();
            for x in -16..32 {
                for z in -16..32 {
                    for y in 24..=40 {
                        let (x, z) = (x + target.0 * 16, z + target.1 * 16);
                        if world.block_state(x, y, z) == iron {
                            found.insert((x, y, z));
                        }
                    }
                }
            }
            found
        };
        let here = ores((0, 0));
        assert!(!here.is_empty(), "the ore feature placed nothing");
        assert_eq!(here, ores((0, 0)));
        assert_ne!(here, ores((1, 0)));
    }

    #[test]
    fn every_position_of_a_placed_feature_is_tried() {
        let mut data = synthetic_ore_data();
        let placement = &mut data.placed_features[0].placement;
        placement[0] = PlacementModifierDef::Count(IntProviderDef::Constant(8));
        let index = FeatureIndex::new(&data, &[0]).expect("compile feature index");
        let mut chunk = flat_stone_chunk();
        let mut world = ChunkFeatureWorld::new(&mut chunk, 0);
        apply_biome_decoration(&mut world, &index, 42, 0, 0).expect("apply features");
        let iron = block_states().default_state("minecraft:iron_ore").unwrap();
        let mut count = 0;
        for x in 0..16 {
            for z in 0..16 {
                for y in 24..=40 {
                    count += usize::from(world.block_state(x, y, z) == iron);
                }
            }
        }
        assert!(count > 15, "only {count} blocks from eight veins");
    }

    #[test]
    fn features_keep_the_order_every_biome_agrees_on() {
        let biomes = vec![vec![vec![1, 3]], vec![vec![2, 3]], vec![vec![1, 2]]];
        let steps = sort_features(&biomes, &[0, 1, 2]).expect("order features");
        assert_eq!(steps, vec![vec![1, 2, 3]]);
        let cycle = vec![vec![vec![1, 2]], vec![vec![2, 1]]];
        assert!(sort_features(&cycle, &[0, 1]).is_err());
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
        assert!(FeatureIndex::new(&data, &[0]).is_err());
    }
}
