//! 7D `Climate` R-tree, `OverworldBiomeBuilder`, `MultiNoiseBiomeSource`, and `BiomeManager` 3D Voronoi zoom
//!
//! Implements vanilla's multi-noise biome lookup (`Climate.RTree` with `CHILDREN_PER_NODE = 6` and
//! `OverworldBiomeBuilder`'s 7,593 parameter boxes), section `4×4×4` biome container filling, and
//! `BiomeManager`'s SHA-256 seed obfuscation and 3D Voronoi zoom (`docs/world.md#24-biomes-and-surface-rules`).

use std::collections::HashSet;

use harpas_core::chunk::Section;
use harpas_core::random::RandomSource;
use sha2::{Digest, Sha256};

use crate::data::{MultiNoiseParameterListDef, WorldgenData, WorldgenDataError};
use crate::noise_chunk::NoiseChunk;

const QUANTIZATION_FACTOR: f32 = 10_000.0;
const CHILDREN_PER_NODE: usize = 6;

/// Converts a floating-point climate coordinate to its quantized integer form (`(coord * 10000.0F) as i64`).
#[inline]
pub fn quantize_coord(coord: f32) -> i64 {
    (coord * QUANTIZATION_FACTOR) as i64
}

/// Converts a quantized integer climate coordinate back to `f32` (`coord as f32 / 10000.0F`).
#[inline]
pub fn unquantize_coord(coord: i64) -> f32 {
    (coord as f32) / QUANTIZATION_FACTOR
}

/// A quantized 1D interval `[min, max]` in climate parameter space (`Climate.Parameter`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Parameter {
    /// Inclusive lower bound (quantized by `10,000`).
    pub min: i64,
    /// Inclusive upper bound (quantized by `10,000`).
    pub max: i64,
}

impl Parameter {
    /// Creates a single-point interval `[v, v]` from a float coordinate.
    #[inline]
    pub fn point(value: f32) -> Self {
        let q = quantize_coord(value);
        Self { min: q, max: q }
    }

    /// Creates a closed interval `[min, max]` from float coordinates.
    #[inline]
    pub fn span(min: f32, max: f32) -> Self {
        let q_min = quantize_coord(min);
        let q_max = quantize_coord(max);
        assert!(
            q_min <= q_max,
            "Parameter::span min ({q_min}) > max ({q_max})"
        );
        Self {
            min: q_min,
            max: q_max,
        }
    }

    /// Creates the smallest interval enclosing both `a` and `b`.
    #[inline]
    pub const fn span_params(a: Self, b: Self) -> Self {
        let min = if a.min < b.min { a.min } else { b.min };
        let max = if a.max > b.max { a.max } else { b.max };
        Self { min, max }
    }

    /// Distance from `value` to this interval (`0` if `min <= value <= max`).
    #[inline]
    pub const fn distance(self, value: i64) -> i64 {
        let above = value - self.max;
        let below = self.min - value;
        if above > 0 {
            above
        } else if below > 0 {
            below
        } else {
            0
        }
    }

    /// Distance between two intervals (`0` if they overlap).
    #[inline]
    pub const fn distance_param(self, other: Self) -> i64 {
        let above = other.min - self.max;
        let below = self.min - other.max;
        if above > 0 {
            above
        } else if below > 0 {
            below
        } else {
            0
        }
    }
}

/// A 7D hyper-rectangle in climate parameter space (`Climate.ParameterPoint`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParameterPoint {
    /// Temperature range.
    pub temperature: Parameter,
    /// Humidity / vegetation range.
    pub humidity: Parameter,
    /// Continentalness range.
    pub continentalness: Parameter,
    /// Erosion range.
    pub erosion: Parameter,
    /// Depth range.
    pub depth: Parameter,
    /// Weirdness / ridges range.
    pub weirdness: Parameter,
    /// Quantized offset penalty.
    pub offset: i64,
}

impl ParameterPoint {
    /// Returns the 7-element [`Parameter`] array representing this hyper-rectangle.
    #[inline]
    pub const fn parameter_space(&self) -> [Parameter; 7] {
        [
            self.temperature,
            self.humidity,
            self.continentalness,
            self.erosion,
            self.depth,
            self.weirdness,
            Parameter {
                min: self.offset,
                max: self.offset,
            },
        ]
    }
}

/// A sampled 6D climate point (`Climate.TargetPoint`), extended with `offset = 0` during 7D R-tree queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TargetPoint {
    /// Quantized temperature.
    pub temperature: i64,
    /// Quantized humidity / vegetation.
    pub humidity: i64,
    /// Quantized continentalness.
    pub continentalness: i64,
    /// Quantized erosion.
    pub erosion: i64,
    /// Quantized depth.
    pub depth: i64,
    /// Quantized weirdness / ridges.
    pub weirdness: i64,
}

impl TargetPoint {
    /// Creates a `TargetPoint` by quantizing 6 `f32` coordinates.
    pub fn from_floats(
        temperature: f32,
        humidity: f32,
        continentalness: f32,
        erosion: f32,
        depth: f32,
        weirdness: f32,
    ) -> Self {
        Self {
            temperature: quantize_coord(temperature),
            humidity: quantize_coord(humidity),
            continentalness: quantize_coord(continentalness),
            erosion: quantize_coord(erosion),
            depth: quantize_coord(depth),
            weirdness: quantize_coord(weirdness),
        }
    }

    /// Returns the 7-element target array `[temperature, humidity, continentalness, erosion, depth, weirdness, 0]`.
    #[inline]
    pub const fn to_parameter_array(self) -> [i64; 7] {
        [
            self.temperature,
            self.humidity,
            self.continentalness,
            self.erosion,
            self.depth,
            self.weirdness,
            0,
        ]
    }
}

#[inline]
fn distance_squared(space: &[Parameter; 7], target: &[i64; 7]) -> i64 {
    let mut sum = 0i64;
    let mut i = 0;
    while i < 7 {
        let d = space[i].distance(target[i]);
        sum = sum.wrapping_add(d.wrapping_mul(d));
        i += 1;
    }
    sum
}

#[inline]
fn union_space(children: &[RTreeNode]) -> [Parameter; 7] {
    assert!(!children.is_empty());
    let mut result = *children[0].parameter_space();
    for child in &children[1..] {
        let s = child.parameter_space();
        for d in 0..7 {
            result[d] = Parameter::span_params(result[d], s[d]);
        }
    }
    result
}

#[derive(Debug, Clone)]
enum RTreeNode {
    Leaf {
        parameter_space: [Parameter; 7],
        leaf_index: usize,
    },
    SubTree {
        parameter_space: [Parameter; 7],
        children: Box<[RTreeNode]>,
    },
}

impl RTreeNode {
    #[inline]
    const fn parameter_space(&self) -> &[Parameter; 7] {
        match self {
            Self::Leaf {
                parameter_space, ..
            }
            | Self::SubTree {
                parameter_space, ..
            } => parameter_space,
        }
    }

    fn search(&self, target: &[i64; 7], best_leaf: &mut Option<usize>, best_dist: &mut i64) {
        match self {
            Self::Leaf {
                parameter_space,
                leaf_index,
            } => {
                let dist = distance_squared(parameter_space, target);
                if dist < *best_dist {
                    *best_dist = dist;
                    *best_leaf = Some(*leaf_index);
                }
            }
            Self::SubTree { children, .. } => {
                for child in children {
                    if *best_dist <= 0 {
                        return;
                    }
                    let bound = distance_squared(child.parameter_space(), target);
                    if *best_dist > bound {
                        child.search(target, best_leaf, best_dist);
                    }
                }
            }
        }
    }

    /// Whether a leaf at distance `dist` satisfies `other`.
    fn has_leaf_at(&self, target: &[i64; 7], dist: i64, other: &impl Fn(usize) -> bool) -> bool {
        match self {
            Self::Leaf {
                parameter_space,
                leaf_index,
            } => distance_squared(parameter_space, target) == dist && other(*leaf_index),
            Self::SubTree { children, .. } => children.iter().any(|child| {
                distance_squared(child.parameter_space(), target) <= dist
                    && child.has_leaf_at(target, dist, other)
            }),
        }
    }
}

#[derive(Debug, Clone)]
struct LeafEntry<T> {
    parameter_space: [Parameter; 7],
    value: T,
}

/// Vanilla's 7D bounding-interval hierarchy (`Climate.RTree<T>`).
#[derive(Debug, Clone)]
pub struct RTree<T> {
    root: RTreeNode,
    leaves: Box<[LeafEntry<T>]>,
}

impl<T: Clone> RTree<T> {
    /// Builds a 7D R-tree from `(ParameterPoint, T)` pairs using vanilla's `Climate.RTree.create` algorithm.
    ///
    /// # Panics
    ///
    /// Panics if `points` is empty.
    pub fn new(points: Vec<(ParameterPoint, T)>) -> Self {
        assert!(
            !points.is_empty(),
            "Need at least one value to build a tree"
        );
        let mut leaves = Vec::with_capacity(points.len());
        let mut nodes = Vec::with_capacity(points.len());
        for (idx, (point, value)) in points.into_iter().enumerate() {
            let parameter_space = point.parameter_space();
            leaves.push(LeafEntry {
                parameter_space,
                value,
            });
            nodes.push(RTreeNode::Leaf {
                parameter_space,
                leaf_index: idx,
            });
        }
        let root = build_node(nodes);
        Self {
            root,
            leaves: leaves.into_boxed_slice(),
        }
    }

    /// Searches the R-tree for the leaf closest to `target`, using and updating `last_result` just as vanilla's
    /// thread-local `lastResult` does.
    pub fn search(&self, target: TargetPoint, last_result: &mut Option<usize>) -> &T {
        let target_arr = target.to_parameter_array();
        let mut best_leaf = *last_result;
        let mut best_dist = match best_leaf {
            Some(idx) => distance_squared(&self.leaves[idx].parameter_space, &target_arr),
            None => i64::MAX,
        };
        self.root
            .search(&target_arr, &mut best_leaf, &mut best_dist);
        let chosen = best_leaf.expect("tree is non-empty");
        *last_result = Some(chosen);
        &self.leaves[chosen].value
    }
}

impl<T> RTree<T> {
    /// The values of all leaves, in the order they were given.
    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.leaves.iter().map(|leaf| &leaf.value)
    }
}

impl<T: Clone + PartialEq> RTree<T> {
    /// The value closest to `target`, or `None` if equally close leaves disagree, when vanilla's answer depends on
    /// its last result.
    pub fn unambiguous_search(&self, target: TargetPoint) -> Option<&T> {
        let mut last_result = None;
        let value = self.search(target, &mut last_result);
        let target = target.to_parameter_array();
        let chosen = last_result.expect("search sets its result");
        let dist = distance_squared(&self.leaves[chosen].parameter_space, &target);
        let tied = self
            .root
            .has_leaf_at(&target, dist, &|leaf| self.leaves[leaf].value != *value);
        (!tied).then_some(value)
    }
}

fn build_node(mut children: Vec<RTreeNode>) -> RTreeNode {
    assert!(!children.is_empty());
    if children.len() == 1 {
        return children.into_iter().next().expect("len == 1");
    }
    if children.len() <= CHILDREN_PER_NODE {
        children.sort_by_key(|node| {
            let space = node.parameter_space();
            let mut total = 0i64;
            for p in space {
                total += ((p.min + p.max) / 2).abs();
            }
            total
        });
        let parameter_space = union_space(&children);
        return RTreeNode::SubTree {
            parameter_space,
            children: children.into_boxed_slice(),
        };
    }

    let mut best_cost = i64::MAX;
    let mut best_dim = 0usize;
    let mut best_buckets: Vec<Vec<RTreeNode>> = Vec::new();

    for dim in 0..7 {
        sort_nodes(&mut children, dim, false);
        let buckets = bucketize(&children);
        let mut total_cost = 0i64;
        for bucket in &buckets {
            let space = union_space(bucket);
            total_cost = total_cost.saturating_add(space_cost(&space));
        }
        if best_cost > total_cost {
            best_cost = total_cost;
            best_dim = dim;
            best_buckets = buckets;
        }
    }

    let mut best_buckets: Vec<_> = best_buckets
        .into_iter()
        .map(|bucket| (union_space(&bucket), bucket))
        .collect();
    best_buckets.sort_by(|(a, _), (b, _)| compare_spaces(a, b, best_dim, true));
    let built_children: Vec<RTreeNode> = best_buckets
        .into_iter()
        .map(|(_, bucket)| build_node(bucket))
        .collect();
    let parameter_space = union_space(&built_children);
    RTreeNode::SubTree {
        parameter_space,
        children: built_children.into_boxed_slice(),
    }
}

fn sort_nodes(children: &mut [RTreeNode], start_dim: usize, abs: bool) {
    children
        .sort_by(|a, b| compare_spaces(a.parameter_space(), b.parameter_space(), start_dim, abs));
}

fn compare_spaces(
    a: &[Parameter; 7],
    b: &[Parameter; 7],
    start_dim: usize,
    abs: bool,
) -> std::cmp::Ordering {
    for offset in 0..7 {
        let dim = (start_dim + offset) % 7;
        let mid_a = (a[dim].min + a[dim].max) / 2;
        let mid_b = (b[dim].min + b[dim].max) / 2;
        let ka = if abs { mid_a.abs() } else { mid_a };
        let kb = if abs { mid_b.abs() } else { mid_b };
        let cmp = ka.cmp(&kb);
        if cmp != std::cmp::Ordering::Equal {
            return cmp;
        }
    }
    std::cmp::Ordering::Equal
}

fn bucketize(nodes: &[RTreeNode]) -> Vec<Vec<RTreeNode>> {
    let total = nodes.len() as f64;
    let exp = ((total - 0.01).ln() / (CHILDREN_PER_NODE as f64).ln()).floor();
    let bucket_size = (CHILDREN_PER_NODE as f64).powf(exp) as usize;
    let bucket_size = bucket_size.max(1);
    nodes.chunks(bucket_size).map(|c| c.to_vec()).collect()
}

#[inline]
fn space_cost(space: &[Parameter; 7]) -> i64 {
    let mut sum = 0i64;
    for p in space {
        sum += (p.max - p.min).abs();
    }
    sum
}

/// Vanilla's `MultiNoiseBiomeSource` backed by a 7D [`RTree`] of numeric biome IDs (`u32`).
#[derive(Debug, Clone)]
pub struct MultiNoiseBiomeSource {
    tree: RTree<u32>,
}

impl MultiNoiseBiomeSource {
    /// Builds a `MultiNoiseBiomeSource` from an explicit list of `(ParameterPoint, biome_id)` entries.
    pub fn from_entries(entries: Vec<(ParameterPoint, u32)>) -> Self {
        Self {
            tree: RTree::new(entries),
        }
    }

    /// Builds the Overworld `MultiNoiseBiomeSource` using vanilla's `OverworldBiomeBuilder` and `biome_id_lookup`.
    pub fn overworld<F>(mut biome_id_lookup: F) -> Result<Self, WorldgenDataError>
    where
        F: FnMut(&str) -> Option<u32>,
    {
        let raw = overworld_biome_parameters();
        let mut entries = Vec::with_capacity(raw.len());
        for (point, name) in raw {
            let id = biome_id_lookup(name).ok_or_else(|| WorldgenDataError::InvalidDefinition {
                context: "minecraft:overworld".to_string(),
                message: format!("missing biome `{name}` in biome registry"),
            })?;
            entries.push((point, id));
        }
        Ok(Self::from_entries(entries))
    }

    /// Builds a `MultiNoiseBiomeSource` from a [`MultiNoiseParameterListDef`] and [`WorldgenData`].
    pub fn from_parameter_list(
        def: &MultiNoiseParameterListDef,
        data: &WorldgenData,
    ) -> Result<Self, WorldgenDataError> {
        match def {
            MultiNoiseParameterListDef::Preset(preset) => match preset.as_str() {
                "minecraft:overworld" => Self::overworld(|name| data.biome_id(name)),
                "minecraft:nether" => {
                    let raw = nether_biome_parameters();
                    let mut entries = Vec::with_capacity(raw.len());
                    for (point, name) in raw {
                        let id = data.biome_id(name).ok_or_else(|| {
                            WorldgenDataError::InvalidDefinition {
                                context: "minecraft:nether".to_string(),
                                message: format!("missing biome `{name}` in biome registry"),
                            }
                        })?;
                        entries.push((point, id));
                    }
                    Ok(Self::from_entries(entries))
                }
                other => {
                    if let Some(nested) = data.parameter_lists.get(other)
                        && !matches!(nested, MultiNoiseParameterListDef::Preset(p) if p == other)
                    {
                        return Self::from_parameter_list(nested, data);
                    }
                    Err(WorldgenDataError::InvalidDefinition {
                        context: other.to_string(),
                        message: format!("unknown multi_noise biome preset `{other}`"),
                    })
                }
            },
            MultiNoiseParameterListDef::Biomes(list) => {
                let mut entries = Vec::with_capacity(list.len());
                for item in list {
                    let id = data.biome_id(&item.biome).ok_or_else(|| {
                        WorldgenDataError::InvalidDefinition {
                            context: item.biome.clone(),
                            message: format!("unknown biome `{}`", item.biome),
                        }
                    })?;
                    entries.push((item.parameters, id));
                }
                Ok(Self::from_entries(entries))
            }
        }
    }

    /// The biome IDs this source can produce (`BiomeSource.possibleBiomes`).
    pub fn possible_biomes(&self) -> HashSet<u32> {
        self.tree.values().copied().collect()
    }

    /// Chooses a random position among those within `radius` blocks of `(block_x, block_z)` at the quart height of
    /// `block_y` whose biome is `allowed`, returning its block X and Z (`BiomeSource.findBiomeHorizontal`).
    pub fn find_biome_horizontal(
        &self,
        noise_chunk: &mut NoiseChunk<'_>,
        [block_x, block_y, block_z]: [i32; 3],
        radius: i32,
        allowed: &HashSet<u32>,
        random: &mut impl RandomSource,
    ) -> Option<(i32, i32)> {
        let (quart_x, quart_y, quart_z) = (block_x >> 2, block_y >> 2, block_z >> 2);
        let quart_radius = radius >> 2;
        let mut last_result = None;
        let mut result = None;
        let mut found = 0;
        for dz in -quart_radius..=quart_radius {
            for dx in -quart_radius..=quart_radius {
                let (x, z) = (quart_x + dx, quart_z + dz);
                let target = noise_chunk.sample_climate(x, quart_y, z);
                if allowed.contains(&self.get_noise_biome(target, &mut last_result)) {
                    if result.is_none() || random.next_int_bounded(found + 1) == 0 {
                        result = Some((x << 2, z << 2));
                    }
                    found += 1;
                }
            }
        }
        result
    }

    /// Queries the biome ID for a sampled [`TargetPoint`].
    #[inline]
    pub fn get_noise_biome(&self, target: TargetPoint, last_result: &mut Option<usize>) -> u32 {
        *self.tree.search(target, last_result)
    }

    /// Fills the `4×4×4` biome container of `section` at `(chunk_x, section_y, chunk_z)` in vanilla's
    /// `x -> y -> z` loop order (`LevelChunkSection.fillBiomesFromNoise`).
    pub fn fill_section_biomes(
        &self,
        section: &mut Section,
        chunk_x: i32,
        section_y: i32,
        chunk_z: i32,
        noise_chunk: &mut NoiseChunk<'_>,
        last_result: &mut Option<usize>,
    ) {
        let quart_min_x = chunk_x << 2;
        let quart_min_y = section_y << 2;
        let quart_min_z = chunk_z << 2;
        for x in 0..4usize {
            for y in 0..4usize {
                for z in 0..4usize {
                    let target = noise_chunk.sample_climate(
                        quart_min_x + (x as i32),
                        quart_min_y + (y as i32),
                        quart_min_z + (z as i32),
                    );
                    let biome_id = self.get_noise_biome(target, last_result);
                    section.set_biome(x, y, z, biome_id);
                }
            }
        }
    }

    /// The biome at quart coordinates in a chunk other than the one `noise_chunk` generates, as vanilla reads it
    /// from that chunk. Ties between parameter points depend on what was searched before, so a tie takes the result
    /// of filling that whole chunk in section order, starting without a last result. Filled chunks are kept in
    /// `noise_chunk`.
    pub fn neighbor_biome(
        &self,
        noise_chunk: &mut NoiseChunk<'_>,
        quart_x: i32,
        quart_y: i32,
        quart_z: i32,
        min_quart_y: i32,
        max_quart_y: i32,
    ) -> u32 {
        let chunk = (quart_x >> 2, quart_z >> 2);
        let index = ((quart_y - min_quart_y) as usize) * 16
            + ((quart_z & 3) as usize) * 4
            + (quart_x & 3) as usize;
        if let Some(biomes) = noise_chunk.neighbor_biomes(chunk) {
            return biomes[index];
        }
        let target = noise_chunk.sample_climate(quart_x, quart_y, quart_z);
        if let Some(&biome) = self.tree.unambiguous_search(target) {
            return biome;
        }
        let mut biomes = vec![0; (max_quart_y - min_quart_y + 1) as usize * 16];
        let mut last_result = None;
        for section_y in (min_quart_y..=max_quart_y).step_by(4) {
            for x in 0..4 {
                for y in 0..4 {
                    for z in 0..4 {
                        let target = noise_chunk.sample_climate(
                            (chunk.0 << 2) + x,
                            section_y + y,
                            (chunk.1 << 2) + z,
                        );
                        biomes[((section_y + y - min_quart_y) * 16 + z * 4 + x) as usize] =
                            self.get_noise_biome(target, &mut last_result);
                    }
                }
            }
        }
        let biome = biomes[index];
        noise_chunk.keep_neighbor_biomes(chunk, biomes);
        biome
    }

    /// Fills the `4×4×4` biome containers for all `sections` of a chunk starting at `min_section_y`
    /// (`-4` for `min_y = -64`).
    pub fn fill_chunk_biomes(
        &self,
        sections: &mut [Section],
        chunk_x: i32,
        min_section_y: i32,
        chunk_z: i32,
        noise_chunk: &mut NoiseChunk<'_>,
    ) {
        let mut last_result = None;
        for (idx, section) in sections.iter_mut().enumerate() {
            let section_y = min_section_y + (idx as i32);
            self.fill_section_biomes(
                section,
                chunk_x,
                section_y,
                chunk_z,
                noise_chunk,
                &mut last_result,
            );
        }
    }
}

/// Vanilla's `BiomeManager` 3D Voronoi zoom from `4×4×4` quart biome samples to block coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BiomeManager {
    biome_zoom_seed: i64,
}

impl BiomeManager {
    /// Creates a `BiomeManager` from an already-obfuscated `biome_zoom_seed`.
    pub const fn from_zoom_seed(biome_zoom_seed: i64) -> Self {
        Self { biome_zoom_seed }
    }

    /// Creates a `BiomeManager` by obfuscating `world_seed` with SHA-256 (`BiomeManager.obfuscateSeed`).
    pub fn from_world_seed(world_seed: i64) -> Self {
        Self {
            biome_zoom_seed: Self::obfuscate_seed(world_seed),
        }
    }

    /// Obfuscates a 64-bit world seed using the first 8 little-endian bytes of `SHA-256(world_seed.to_le_bytes())`
    /// (`BiomeManager.obfuscateSeed` / Guava `Hashing.sha256().hashLong(seed).asLong()`).
    pub fn obfuscate_seed(world_seed: i64) -> i64 {
        let digest = Sha256::digest(world_seed.to_le_bytes());
        i64::from_le_bytes(digest[..8].try_into().expect("8-byte slice"))
    }

    /// Returns the obfuscated zoom seed.
    pub const fn biome_zoom_seed(&self) -> i64 {
        self.biome_zoom_seed
    }

    /// Resolves the zoomed biome at block coordinates `(block_x, block_y, block_z)` by querying `quart_lookup`
    /// at the winning corner of the surrounding `4×4×4` cell (`BiomeManager.getBiome`).
    pub fn get_biome<F>(&self, block_x: i32, block_y: i32, block_z: i32, mut quart_lookup: F) -> u32
    where
        F: FnMut(i32, i32, i32) -> u32,
    {
        let ox = block_x - 2;
        let oy = block_y - 2;
        let oz = block_z - 2;
        let parent_x = ox >> 2;
        let parent_y = oy >> 2;
        let parent_z = oz >> 2;
        let frac_x = f64::from(ox & 3) / 4.0;
        let frac_y = f64::from(oy & 3) / 4.0;
        let frac_z = f64::from(oz & 3) / 4.0;

        let mut best_index = 0usize;
        let mut best_distance = f64::INFINITY;
        for i in 0..8usize {
            let bx = (i & 4) == 0;
            let by = (i & 2) == 0;
            let bz = (i & 1) == 0;
            let qx = if bx { parent_x } else { parent_x + 1 };
            let qy = if by { parent_y } else { parent_y + 1 };
            let qz = if bz { parent_z } else { parent_z + 1 };
            let rx = if bx { frac_x } else { frac_x - 1.0 };
            let ry = if by { frac_y } else { frac_y - 1.0 };
            let rz = if bz { frac_z } else { frac_z - 1.0 };
            let dist = get_fiddled_distance(self.biome_zoom_seed, qx, qy, qz, rx, ry, rz);
            if best_distance > dist {
                best_index = i;
                best_distance = dist;
            }
        }

        let final_qx = if (best_index & 4) == 0 {
            parent_x
        } else {
            parent_x + 1
        };
        let final_qy = if (best_index & 2) == 0 {
            parent_y
        } else {
            parent_y + 1
        };
        let final_qz = if (best_index & 1) == 0 {
            parent_z
        } else {
            parent_z + 1
        };
        quart_lookup(final_qx, final_qy, final_qz)
    }
}

#[inline]
fn lcg_next(seed: i64, salt: i64) -> i64 {
    let mult = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    seed.wrapping_mul(mult).wrapping_add(salt)
}

#[inline]
fn get_fiddle(seed: i64) -> f64 {
    let floor_mod = ((seed >> 24) & 1023) as i32;
    let d = f64::from(floor_mod) / 1024.0;
    (d - 0.5) * 0.9
}

#[inline]
fn get_fiddled_distance(
    biome_zoom_seed: i64,
    x: i32,
    y: i32,
    z: i32,
    frac_x: f64,
    frac_y: f64,
    frac_z: f64,
) -> f64 {
    let mut s = lcg_next(biome_zoom_seed, i64::from(x));
    s = lcg_next(s, i64::from(y));
    s = lcg_next(s, i64::from(z));
    s = lcg_next(s, i64::from(x));
    s = lcg_next(s, i64::from(y));
    s = lcg_next(s, i64::from(z));
    let fx = get_fiddle(s);
    s = lcg_next(s, biome_zoom_seed);
    let fy = get_fiddle(s);
    s = lcg_next(s, biome_zoom_seed);
    let fz = get_fiddle(s);
    let dx = frac_x + fx;
    let dy = frac_y + fy;
    let dz = frac_z + fz;
    dz * dz + dy * dy + dx * dx
}

/// Generates the 5 `(ParameterPoint, biome_name)` entries of the `"minecraft:nether"` preset.
pub fn nether_biome_parameters() -> Vec<(ParameterPoint, &'static str)> {
    let pt = |t: f32, h: f32, c: f32, e: f32, d: f32, w: f32, offset: f32| ParameterPoint {
        temperature: Parameter::point(t),
        humidity: Parameter::point(h),
        continentalness: Parameter::point(c),
        erosion: Parameter::point(e),
        depth: Parameter::point(d),
        weirdness: Parameter::point(w),
        offset: quantize_coord(offset),
    };
    vec![
        (
            pt(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0),
            "minecraft:nether_wastes",
        ),
        (
            pt(0.0, -0.5, 0.0, 0.0, 0.0, 0.0, 0.0),
            "minecraft:soul_sand_valley",
        ),
        (
            pt(0.4, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0),
            "minecraft:crimson_forest",
        ),
        (
            pt(0.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.375),
            "minecraft:warped_forest",
        ),
        (
            pt(-0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.175),
            "minecraft:basalt_deltas",
        ),
    ]
}

/// Generates all 7,593 `(ParameterPoint, biome_name)` entries of vanilla 26.1's `OverworldBiomeBuilder`.
pub fn overworld_biome_parameters() -> Vec<(ParameterPoint, &'static str)> {
    let mut out = Vec::with_capacity(7593);
    OverworldBiomeBuilder::new().add_biomes(&mut out);
    out
}

struct OverworldBiomeBuilder {
    full_range: Parameter,
    temperatures: [Parameter; 5],
    humidities: [Parameter; 5],
    erosions: [Parameter; 7],
    frozen_range: Parameter,
    unfrozen_range: Parameter,
    mushroom_fields_continentalness: Parameter,
    deep_ocean_continentalness: Parameter,
    ocean_continentalness: Parameter,
    coast_continentalness: Parameter,
    inland_continentalness: Parameter,
    near_inland_continentalness: Parameter,
    mid_inland_continentalness: Parameter,
    far_inland_continentalness: Parameter,
}

const OCEANS: [[&str; 5]; 2] = [
    [
        "minecraft:deep_frozen_ocean",
        "minecraft:deep_cold_ocean",
        "minecraft:deep_ocean",
        "minecraft:deep_lukewarm_ocean",
        "minecraft:warm_ocean",
    ],
    [
        "minecraft:frozen_ocean",
        "minecraft:cold_ocean",
        "minecraft:ocean",
        "minecraft:lukewarm_ocean",
        "minecraft:warm_ocean",
    ],
];

const MIDDLE_BIOMES: [[&str; 5]; 5] = [
    [
        "minecraft:snowy_plains",
        "minecraft:snowy_plains",
        "minecraft:snowy_plains",
        "minecraft:snowy_taiga",
        "minecraft:taiga",
    ],
    [
        "minecraft:plains",
        "minecraft:plains",
        "minecraft:forest",
        "minecraft:taiga",
        "minecraft:old_growth_spruce_taiga",
    ],
    [
        "minecraft:flower_forest",
        "minecraft:plains",
        "minecraft:forest",
        "minecraft:birch_forest",
        "minecraft:dark_forest",
    ],
    [
        "minecraft:savanna",
        "minecraft:savanna",
        "minecraft:forest",
        "minecraft:jungle",
        "minecraft:jungle",
    ],
    [
        "minecraft:desert",
        "minecraft:desert",
        "minecraft:desert",
        "minecraft:desert",
        "minecraft:desert",
    ],
];

const MIDDLE_BIOMES_VARIANT: [[Option<&str>; 5]; 5] = [
    [
        Some("minecraft:ice_spikes"),
        None,
        Some("minecraft:snowy_taiga"),
        None,
        None,
    ],
    [
        None,
        None,
        None,
        None,
        Some("minecraft:old_growth_pine_taiga"),
    ],
    [
        Some("minecraft:sunflower_plains"),
        None,
        None,
        Some("minecraft:old_growth_birch_forest"),
        None,
    ],
    [
        None,
        None,
        Some("minecraft:plains"),
        Some("minecraft:sparse_jungle"),
        Some("minecraft:bamboo_jungle"),
    ],
    [None, None, None, None, None],
];

const PLATEAU_BIOMES: [[&str; 5]; 5] = [
    [
        "minecraft:snowy_plains",
        "minecraft:snowy_plains",
        "minecraft:snowy_plains",
        "minecraft:snowy_taiga",
        "minecraft:snowy_taiga",
    ],
    [
        "minecraft:meadow",
        "minecraft:meadow",
        "minecraft:forest",
        "minecraft:taiga",
        "minecraft:old_growth_spruce_taiga",
    ],
    [
        "minecraft:meadow",
        "minecraft:meadow",
        "minecraft:meadow",
        "minecraft:meadow",
        "minecraft:pale_garden",
    ],
    [
        "minecraft:savanna_plateau",
        "minecraft:savanna_plateau",
        "minecraft:forest",
        "minecraft:forest",
        "minecraft:jungle",
    ],
    [
        "minecraft:badlands",
        "minecraft:badlands",
        "minecraft:badlands",
        "minecraft:wooded_badlands",
        "minecraft:wooded_badlands",
    ],
];

const PLATEAU_BIOMES_VARIANT: [[Option<&str>; 5]; 5] = [
    [Some("minecraft:ice_spikes"), None, None, None, None],
    [
        Some("minecraft:cherry_grove"),
        None,
        Some("minecraft:meadow"),
        Some("minecraft:meadow"),
        Some("minecraft:old_growth_pine_taiga"),
    ],
    [
        Some("minecraft:cherry_grove"),
        Some("minecraft:cherry_grove"),
        Some("minecraft:forest"),
        Some("minecraft:birch_forest"),
        Some("minecraft:pale_garden"),
    ],
    [None, None, None, None, None],
    [
        Some("minecraft:eroded_badlands"),
        Some("minecraft:eroded_badlands"),
        None,
        None,
        None,
    ],
];

const SHATTERED_BIOMES: [[Option<&str>; 5]; 5] = [
    [
        Some("minecraft:windswept_gravelly_hills"),
        Some("minecraft:windswept_gravelly_hills"),
        Some("minecraft:windswept_hills"),
        Some("minecraft:windswept_forest"),
        Some("minecraft:windswept_forest"),
    ],
    [
        Some("minecraft:windswept_gravelly_hills"),
        Some("minecraft:windswept_gravelly_hills"),
        Some("minecraft:windswept_hills"),
        Some("minecraft:windswept_forest"),
        Some("minecraft:windswept_forest"),
    ],
    [
        Some("minecraft:windswept_hills"),
        Some("minecraft:windswept_hills"),
        Some("minecraft:windswept_hills"),
        Some("minecraft:windswept_forest"),
        Some("minecraft:windswept_forest"),
    ],
    [None, None, None, None, None],
    [None, None, None, None, None],
];

impl OverworldBiomeBuilder {
    fn new() -> Self {
        let full_range = Parameter::span(-1.0, 1.0);
        let temperatures = [
            Parameter::span(-1.0, -0.45),
            Parameter::span(-0.45, -0.15),
            Parameter::span(-0.15, 0.2),
            Parameter::span(0.2, 0.55),
            Parameter::span(0.55, 1.0),
        ];
        let humidities = [
            Parameter::span(-1.0, -0.35),
            Parameter::span(-0.35, -0.1),
            Parameter::span(-0.1, 0.1),
            Parameter::span(0.1, 0.3),
            Parameter::span(0.3, 1.0),
        ];
        let erosions = [
            Parameter::span(-1.0, -0.78),
            Parameter::span(-0.78, -0.375),
            Parameter::span(-0.375, -0.2225),
            Parameter::span(-0.2225, 0.05),
            Parameter::span(0.05, 0.45),
            Parameter::span(0.45, 0.55),
            Parameter::span(0.55, 1.0),
        ];
        let frozen_range = temperatures[0];
        let unfrozen_range = Parameter::span_params(temperatures[1], temperatures[4]);
        Self {
            full_range,
            temperatures,
            humidities,
            erosions,
            frozen_range,
            unfrozen_range,
            mushroom_fields_continentalness: Parameter::span(-1.2, -1.05),
            deep_ocean_continentalness: Parameter::span(-1.05, -0.455),
            ocean_continentalness: Parameter::span(-0.455, -0.19),
            coast_continentalness: Parameter::span(-0.19, -0.11),
            inland_continentalness: Parameter::span(-0.11, 0.55),
            near_inland_continentalness: Parameter::span(-0.11, 0.03),
            mid_inland_continentalness: Parameter::span(0.03, 0.3),
            far_inland_continentalness: Parameter::span(0.3, 1.0),
        }
    }

    fn add_biomes(&self, out: &mut Vec<(ParameterPoint, &'static str)>) {
        self.add_off_coast_biomes(out);
        self.add_inland_biomes(out);
        self.add_underground_biomes(out);
    }

    fn add_off_coast_biomes(&self, out: &mut Vec<(ParameterPoint, &'static str)>) {
        self.add_surface_biome(
            out,
            self.full_range,
            self.full_range,
            self.mushroom_fields_continentalness,
            self.full_range,
            self.full_range,
            0.0,
            "minecraft:mushroom_fields",
        );
        for (i, &t) in self.temperatures.iter().enumerate() {
            self.add_surface_biome(
                out,
                t,
                self.full_range,
                self.deep_ocean_continentalness,
                self.full_range,
                self.full_range,
                0.0,
                OCEANS[0][i],
            );
            self.add_surface_biome(
                out,
                t,
                self.full_range,
                self.ocean_continentalness,
                self.full_range,
                self.full_range,
                0.0,
                OCEANS[1][i],
            );
        }
    }

    fn add_inland_biomes(&self, out: &mut Vec<(ParameterPoint, &'static str)>) {
        self.add_mid_slice(out, Parameter::span(-1.0, -0.93333334));
        self.add_high_slice(out, Parameter::span(-0.93333334, -0.7666667));
        self.add_peaks(out, Parameter::span(-0.7666667, -0.56666666));
        self.add_high_slice(out, Parameter::span(-0.56666666, -0.4));
        self.add_mid_slice(out, Parameter::span(-0.4, -0.26666668));
        self.add_low_slice(out, Parameter::span(-0.26666668, -0.05));
        self.add_valleys(out, Parameter::span(-0.05, 0.05));
        self.add_low_slice(out, Parameter::span(0.05, 0.26666668));
        self.add_mid_slice(out, Parameter::span(0.26666668, 0.4));
        self.add_high_slice(out, Parameter::span(0.4, 0.56666666));
        self.add_peaks(out, Parameter::span(0.56666666, 0.7666667));
        self.add_high_slice(out, Parameter::span(0.7666667, 0.93333334));
        self.add_mid_slice(out, Parameter::span(0.93333334, 1.0));
    }

    fn add_peaks(&self, out: &mut Vec<(ParameterPoint, &'static str)>, weirdness: Parameter) {
        for i in 0..self.temperatures.len() {
            let t = self.temperatures[i];
            for j in 0..self.humidities.len() {
                let h = self.humidities[j];
                let middle = self.pick_middle_biome(i, j, weirdness);
                let middle_or_badlands = self.pick_middle_biome_or_badlands_if_hot(i, j, weirdness);
                let middle_or_badlands_or_slope =
                    self.pick_middle_biome_or_badlands_if_hot_or_slope_if_cold(i, j, weirdness);
                let plateau = self.pick_plateau_biome(i, j, weirdness);
                let shattered = self.pick_shattered_biome(i, j, weirdness);
                let maybe_w_savanna =
                    self.maybe_pick_windswept_savanna_biome(i, j, weirdness, shattered);
                let peak = self.pick_peak_biome(i, j, weirdness);

                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.coast_continentalness,
                        self.far_inland_continentalness,
                    ),
                    self.erosions[0],
                    weirdness,
                    0.0,
                    peak,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.coast_continentalness,
                        self.near_inland_continentalness,
                    ),
                    self.erosions[1],
                    weirdness,
                    0.0,
                    middle_or_badlands_or_slope,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.mid_inland_continentalness,
                        self.far_inland_continentalness,
                    ),
                    self.erosions[1],
                    weirdness,
                    0.0,
                    peak,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.coast_continentalness,
                        self.near_inland_continentalness,
                    ),
                    Parameter::span_params(self.erosions[2], self.erosions[3]),
                    weirdness,
                    0.0,
                    middle,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.mid_inland_continentalness,
                        self.far_inland_continentalness,
                    ),
                    self.erosions[2],
                    weirdness,
                    0.0,
                    plateau,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    self.mid_inland_continentalness,
                    self.erosions[3],
                    weirdness,
                    0.0,
                    middle_or_badlands,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    self.far_inland_continentalness,
                    self.erosions[3],
                    weirdness,
                    0.0,
                    plateau,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.coast_continentalness,
                        self.far_inland_continentalness,
                    ),
                    self.erosions[4],
                    weirdness,
                    0.0,
                    middle,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.coast_continentalness,
                        self.near_inland_continentalness,
                    ),
                    self.erosions[5],
                    weirdness,
                    0.0,
                    maybe_w_savanna,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.mid_inland_continentalness,
                        self.far_inland_continentalness,
                    ),
                    self.erosions[5],
                    weirdness,
                    0.0,
                    shattered,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.coast_continentalness,
                        self.far_inland_continentalness,
                    ),
                    self.erosions[6],
                    weirdness,
                    0.0,
                    middle,
                );
            }
        }
    }

    fn add_high_slice(&self, out: &mut Vec<(ParameterPoint, &'static str)>, weirdness: Parameter) {
        for i in 0..self.temperatures.len() {
            let t = self.temperatures[i];
            for j in 0..self.humidities.len() {
                let h = self.humidities[j];
                let middle = self.pick_middle_biome(i, j, weirdness);
                let middle_or_badlands = self.pick_middle_biome_or_badlands_if_hot(i, j, weirdness);
                let middle_or_badlands_or_slope =
                    self.pick_middle_biome_or_badlands_if_hot_or_slope_if_cold(i, j, weirdness);
                let plateau = self.pick_plateau_biome(i, j, weirdness);
                let shattered = self.pick_shattered_biome(i, j, weirdness);
                let middle_or_w_savanna =
                    self.maybe_pick_windswept_savanna_biome(i, j, weirdness, middle);
                let slope = self.pick_slope_biome(i, j, weirdness);
                let peak = self.pick_peak_biome(i, j, weirdness);

                self.add_surface_biome(
                    out,
                    t,
                    h,
                    self.coast_continentalness,
                    Parameter::span_params(self.erosions[0], self.erosions[1]),
                    weirdness,
                    0.0,
                    middle,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    self.near_inland_continentalness,
                    self.erosions[0],
                    weirdness,
                    0.0,
                    slope,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.mid_inland_continentalness,
                        self.far_inland_continentalness,
                    ),
                    self.erosions[0],
                    weirdness,
                    0.0,
                    peak,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    self.near_inland_continentalness,
                    self.erosions[1],
                    weirdness,
                    0.0,
                    middle_or_badlands_or_slope,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.mid_inland_continentalness,
                        self.far_inland_continentalness,
                    ),
                    self.erosions[1],
                    weirdness,
                    0.0,
                    slope,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.coast_continentalness,
                        self.near_inland_continentalness,
                    ),
                    Parameter::span_params(self.erosions[2], self.erosions[3]),
                    weirdness,
                    0.0,
                    middle,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.mid_inland_continentalness,
                        self.far_inland_continentalness,
                    ),
                    self.erosions[2],
                    weirdness,
                    0.0,
                    plateau,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    self.mid_inland_continentalness,
                    self.erosions[3],
                    weirdness,
                    0.0,
                    middle_or_badlands,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    self.far_inland_continentalness,
                    self.erosions[3],
                    weirdness,
                    0.0,
                    plateau,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.coast_continentalness,
                        self.far_inland_continentalness,
                    ),
                    self.erosions[4],
                    weirdness,
                    0.0,
                    middle,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.coast_continentalness,
                        self.near_inland_continentalness,
                    ),
                    self.erosions[5],
                    weirdness,
                    0.0,
                    middle_or_w_savanna,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.mid_inland_continentalness,
                        self.far_inland_continentalness,
                    ),
                    self.erosions[5],
                    weirdness,
                    0.0,
                    shattered,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.coast_continentalness,
                        self.far_inland_continentalness,
                    ),
                    self.erosions[6],
                    weirdness,
                    0.0,
                    middle,
                );
            }
        }
    }

    fn add_mid_slice(&self, out: &mut Vec<(ParameterPoint, &'static str)>, weirdness: Parameter) {
        self.add_surface_biome(
            out,
            self.full_range,
            self.full_range,
            self.coast_continentalness,
            Parameter::span_params(self.erosions[0], self.erosions[2]),
            weirdness,
            0.0,
            "minecraft:stony_shore",
        );
        self.add_surface_biome(
            out,
            Parameter::span_params(self.temperatures[1], self.temperatures[2]),
            self.full_range,
            Parameter::span_params(
                self.near_inland_continentalness,
                self.far_inland_continentalness,
            ),
            self.erosions[6],
            weirdness,
            0.0,
            "minecraft:swamp",
        );
        self.add_surface_biome(
            out,
            Parameter::span_params(self.temperatures[3], self.temperatures[4]),
            self.full_range,
            Parameter::span_params(
                self.near_inland_continentalness,
                self.far_inland_continentalness,
            ),
            self.erosions[6],
            weirdness,
            0.0,
            "minecraft:mangrove_swamp",
        );

        for i in 0..self.temperatures.len() {
            let t = self.temperatures[i];
            for j in 0..self.humidities.len() {
                let h = self.humidities[j];
                let middle = self.pick_middle_biome(i, j, weirdness);
                let middle_or_badlands = self.pick_middle_biome_or_badlands_if_hot(i, j, weirdness);
                let middle_or_badlands_or_slope =
                    self.pick_middle_biome_or_badlands_if_hot_or_slope_if_cold(i, j, weirdness);
                let shattered = self.pick_shattered_biome(i, j, weirdness);
                let plateau = self.pick_plateau_biome(i, j, weirdness);
                let beach = self.pick_beach_biome(i, j);
                let middle_or_w_savanna =
                    self.maybe_pick_windswept_savanna_biome(i, j, weirdness, middle);
                let shattered_coast = self.pick_shattered_coast_biome(i, j, weirdness);
                let slope = self.pick_slope_biome(i, j, weirdness);

                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.near_inland_continentalness,
                        self.far_inland_continentalness,
                    ),
                    self.erosions[0],
                    weirdness,
                    0.0,
                    slope,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.near_inland_continentalness,
                        self.mid_inland_continentalness,
                    ),
                    self.erosions[1],
                    weirdness,
                    0.0,
                    middle_or_badlands_or_slope,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    self.far_inland_continentalness,
                    self.erosions[1],
                    weirdness,
                    0.0,
                    if i == 0 { slope } else { plateau },
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    self.near_inland_continentalness,
                    self.erosions[2],
                    weirdness,
                    0.0,
                    middle,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    self.mid_inland_continentalness,
                    self.erosions[2],
                    weirdness,
                    0.0,
                    middle_or_badlands,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    self.far_inland_continentalness,
                    self.erosions[2],
                    weirdness,
                    0.0,
                    plateau,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.coast_continentalness,
                        self.near_inland_continentalness,
                    ),
                    self.erosions[3],
                    weirdness,
                    0.0,
                    middle,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.mid_inland_continentalness,
                        self.far_inland_continentalness,
                    ),
                    self.erosions[3],
                    weirdness,
                    0.0,
                    middle_or_badlands,
                );
                if weirdness.max < 0 {
                    self.add_surface_biome(
                        out,
                        t,
                        h,
                        self.coast_continentalness,
                        self.erosions[4],
                        weirdness,
                        0.0,
                        beach,
                    );
                    self.add_surface_biome(
                        out,
                        t,
                        h,
                        Parameter::span_params(
                            self.near_inland_continentalness,
                            self.far_inland_continentalness,
                        ),
                        self.erosions[4],
                        weirdness,
                        0.0,
                        middle,
                    );
                } else {
                    self.add_surface_biome(
                        out,
                        t,
                        h,
                        Parameter::span_params(
                            self.coast_continentalness,
                            self.far_inland_continentalness,
                        ),
                        self.erosions[4],
                        weirdness,
                        0.0,
                        middle,
                    );
                }
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    self.coast_continentalness,
                    self.erosions[5],
                    weirdness,
                    0.0,
                    shattered_coast,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    self.near_inland_continentalness,
                    self.erosions[5],
                    weirdness,
                    0.0,
                    middle_or_w_savanna,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.mid_inland_continentalness,
                        self.far_inland_continentalness,
                    ),
                    self.erosions[5],
                    weirdness,
                    0.0,
                    shattered,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    self.coast_continentalness,
                    self.erosions[6],
                    weirdness,
                    0.0,
                    if weirdness.max < 0 { beach } else { middle },
                );
                if i == 0 {
                    self.add_surface_biome(
                        out,
                        t,
                        h,
                        Parameter::span_params(
                            self.near_inland_continentalness,
                            self.far_inland_continentalness,
                        ),
                        self.erosions[6],
                        weirdness,
                        0.0,
                        middle,
                    );
                }
            }
        }
    }

    fn add_low_slice(&self, out: &mut Vec<(ParameterPoint, &'static str)>, weirdness: Parameter) {
        self.add_surface_biome(
            out,
            self.full_range,
            self.full_range,
            self.coast_continentalness,
            Parameter::span_params(self.erosions[0], self.erosions[2]),
            weirdness,
            0.0,
            "minecraft:stony_shore",
        );
        self.add_surface_biome(
            out,
            Parameter::span_params(self.temperatures[1], self.temperatures[2]),
            self.full_range,
            Parameter::span_params(
                self.near_inland_continentalness,
                self.far_inland_continentalness,
            ),
            self.erosions[6],
            weirdness,
            0.0,
            "minecraft:swamp",
        );
        self.add_surface_biome(
            out,
            Parameter::span_params(self.temperatures[3], self.temperatures[4]),
            self.full_range,
            Parameter::span_params(
                self.near_inland_continentalness,
                self.far_inland_continentalness,
            ),
            self.erosions[6],
            weirdness,
            0.0,
            "minecraft:mangrove_swamp",
        );

        for i in 0..self.temperatures.len() {
            let t = self.temperatures[i];
            for j in 0..self.humidities.len() {
                let h = self.humidities[j];
                let middle = self.pick_middle_biome(i, j, weirdness);
                let middle_or_badlands = self.pick_middle_biome_or_badlands_if_hot(i, j, weirdness);
                let middle_or_badlands_or_slope =
                    self.pick_middle_biome_or_badlands_if_hot_or_slope_if_cold(i, j, weirdness);
                let beach = self.pick_beach_biome(i, j);
                let middle_or_w_savanna =
                    self.maybe_pick_windswept_savanna_biome(i, j, weirdness, middle);
                let shattered_coast = self.pick_shattered_coast_biome(i, j, weirdness);

                self.add_surface_biome(
                    out,
                    t,
                    h,
                    self.near_inland_continentalness,
                    Parameter::span_params(self.erosions[0], self.erosions[1]),
                    weirdness,
                    0.0,
                    middle_or_badlands,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.mid_inland_continentalness,
                        self.far_inland_continentalness,
                    ),
                    Parameter::span_params(self.erosions[0], self.erosions[1]),
                    weirdness,
                    0.0,
                    middle_or_badlands_or_slope,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    self.near_inland_continentalness,
                    Parameter::span_params(self.erosions[2], self.erosions[3]),
                    weirdness,
                    0.0,
                    middle,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.mid_inland_continentalness,
                        self.far_inland_continentalness,
                    ),
                    Parameter::span_params(self.erosions[2], self.erosions[3]),
                    weirdness,
                    0.0,
                    middle_or_badlands,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    self.coast_continentalness,
                    Parameter::span_params(self.erosions[3], self.erosions[4]),
                    weirdness,
                    0.0,
                    beach,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.near_inland_continentalness,
                        self.far_inland_continentalness,
                    ),
                    self.erosions[4],
                    weirdness,
                    0.0,
                    middle,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    self.coast_continentalness,
                    self.erosions[5],
                    weirdness,
                    0.0,
                    shattered_coast,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    self.near_inland_continentalness,
                    self.erosions[5],
                    weirdness,
                    0.0,
                    middle_or_w_savanna,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.mid_inland_continentalness,
                        self.far_inland_continentalness,
                    ),
                    self.erosions[5],
                    weirdness,
                    0.0,
                    middle,
                );
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    self.coast_continentalness,
                    self.erosions[6],
                    weirdness,
                    0.0,
                    beach,
                );
                if i == 0 {
                    self.add_surface_biome(
                        out,
                        t,
                        h,
                        Parameter::span_params(
                            self.near_inland_continentalness,
                            self.far_inland_continentalness,
                        ),
                        self.erosions[6],
                        weirdness,
                        0.0,
                        middle,
                    );
                }
            }
        }
    }

    fn add_valleys(&self, out: &mut Vec<(ParameterPoint, &'static str)>, weirdness: Parameter) {
        self.add_surface_biome(
            out,
            self.frozen_range,
            self.full_range,
            self.coast_continentalness,
            Parameter::span_params(self.erosions[0], self.erosions[1]),
            weirdness,
            0.0,
            if weirdness.max < 0 {
                "minecraft:stony_shore"
            } else {
                "minecraft:frozen_river"
            },
        );
        self.add_surface_biome(
            out,
            self.unfrozen_range,
            self.full_range,
            self.coast_continentalness,
            Parameter::span_params(self.erosions[0], self.erosions[1]),
            weirdness,
            0.0,
            if weirdness.max < 0 {
                "minecraft:stony_shore"
            } else {
                "minecraft:river"
            },
        );
        self.add_surface_biome(
            out,
            self.frozen_range,
            self.full_range,
            self.near_inland_continentalness,
            Parameter::span_params(self.erosions[0], self.erosions[1]),
            weirdness,
            0.0,
            "minecraft:frozen_river",
        );
        self.add_surface_biome(
            out,
            self.unfrozen_range,
            self.full_range,
            self.near_inland_continentalness,
            Parameter::span_params(self.erosions[0], self.erosions[1]),
            weirdness,
            0.0,
            "minecraft:river",
        );
        self.add_surface_biome(
            out,
            self.frozen_range,
            self.full_range,
            Parameter::span_params(self.coast_continentalness, self.far_inland_continentalness),
            Parameter::span_params(self.erosions[2], self.erosions[5]),
            weirdness,
            0.0,
            "minecraft:frozen_river",
        );
        self.add_surface_biome(
            out,
            self.unfrozen_range,
            self.full_range,
            Parameter::span_params(self.coast_continentalness, self.far_inland_continentalness),
            Parameter::span_params(self.erosions[2], self.erosions[5]),
            weirdness,
            0.0,
            "minecraft:river",
        );
        self.add_surface_biome(
            out,
            self.frozen_range,
            self.full_range,
            self.coast_continentalness,
            self.erosions[6],
            weirdness,
            0.0,
            "minecraft:frozen_river",
        );
        self.add_surface_biome(
            out,
            self.unfrozen_range,
            self.full_range,
            self.coast_continentalness,
            self.erosions[6],
            weirdness,
            0.0,
            "minecraft:river",
        );
        self.add_surface_biome(
            out,
            Parameter::span_params(self.temperatures[1], self.temperatures[2]),
            self.full_range,
            Parameter::span_params(self.inland_continentalness, self.far_inland_continentalness),
            self.erosions[6],
            weirdness,
            0.0,
            "minecraft:swamp",
        );
        self.add_surface_biome(
            out,
            Parameter::span_params(self.temperatures[3], self.temperatures[4]),
            self.full_range,
            Parameter::span_params(self.inland_continentalness, self.far_inland_continentalness),
            self.erosions[6],
            weirdness,
            0.0,
            "minecraft:mangrove_swamp",
        );
        self.add_surface_biome(
            out,
            self.frozen_range,
            self.full_range,
            Parameter::span_params(self.inland_continentalness, self.far_inland_continentalness),
            self.erosions[6],
            weirdness,
            0.0,
            "minecraft:frozen_river",
        );

        for i in 0..self.temperatures.len() {
            let t = self.temperatures[i];
            for j in 0..self.humidities.len() {
                let h = self.humidities[j];
                let middle_or_badlands = self.pick_middle_biome_or_badlands_if_hot(i, j, weirdness);
                self.add_surface_biome(
                    out,
                    t,
                    h,
                    Parameter::span_params(
                        self.mid_inland_continentalness,
                        self.far_inland_continentalness,
                    ),
                    Parameter::span_params(self.erosions[0], self.erosions[1]),
                    weirdness,
                    0.0,
                    middle_or_badlands,
                );
            }
        }
    }

    fn add_underground_biomes(&self, out: &mut Vec<(ParameterPoint, &'static str)>) {
        self.add_underground_biome(
            out,
            self.full_range,
            self.full_range,
            Parameter::span(0.8, 1.0),
            self.full_range,
            self.full_range,
            0.0,
            "minecraft:dripstone_caves",
        );
        self.add_underground_biome(
            out,
            self.full_range,
            Parameter::span(0.7, 1.0),
            self.full_range,
            self.full_range,
            self.full_range,
            0.0,
            "minecraft:lush_caves",
        );
        self.add_bottom_biome(
            out,
            self.full_range,
            self.full_range,
            self.full_range,
            Parameter::span_params(self.erosions[0], self.erosions[1]),
            self.full_range,
            0.0,
            "minecraft:deep_dark",
        );
    }

    fn pick_middle_biome(&self, t: usize, h: usize, weirdness: Parameter) -> &'static str {
        if weirdness.max < 0 {
            MIDDLE_BIOMES[t][h]
        } else {
            MIDDLE_BIOMES_VARIANT[t][h].unwrap_or(MIDDLE_BIOMES[t][h])
        }
    }

    fn pick_middle_biome_or_badlands_if_hot(
        &self,
        t: usize,
        h: usize,
        weirdness: Parameter,
    ) -> &'static str {
        if t == 4 {
            self.pick_badlands_biome(h, weirdness)
        } else {
            self.pick_middle_biome(t, h, weirdness)
        }
    }

    fn pick_middle_biome_or_badlands_if_hot_or_slope_if_cold(
        &self,
        t: usize,
        h: usize,
        weirdness: Parameter,
    ) -> &'static str {
        if t == 0 {
            self.pick_slope_biome(t, h, weirdness)
        } else {
            self.pick_middle_biome_or_badlands_if_hot(t, h, weirdness)
        }
    }

    fn maybe_pick_windswept_savanna_biome(
        &self,
        t: usize,
        h: usize,
        weirdness: Parameter,
        default_biome: &'static str,
    ) -> &'static str {
        if t > 1 && h < 4 && weirdness.max >= 0 {
            "minecraft:windswept_savanna"
        } else {
            default_biome
        }
    }

    fn pick_shattered_coast_biome(&self, t: usize, h: usize, weirdness: Parameter) -> &'static str {
        let base = if weirdness.max >= 0 {
            self.pick_middle_biome(t, h, weirdness)
        } else {
            self.pick_beach_biome(t, h)
        };
        self.maybe_pick_windswept_savanna_biome(t, h, weirdness, base)
    }

    fn pick_beach_biome(&self, t: usize, _h: usize) -> &'static str {
        if t == 0 {
            "minecraft:snowy_beach"
        } else if t == 4 {
            "minecraft:desert"
        } else {
            "minecraft:beach"
        }
    }

    fn pick_badlands_biome(&self, h: usize, weirdness: Parameter) -> &'static str {
        if h < 2 {
            if weirdness.max < 0 {
                "minecraft:badlands"
            } else {
                "minecraft:eroded_badlands"
            }
        } else if h < 3 {
            "minecraft:badlands"
        } else {
            "minecraft:wooded_badlands"
        }
    }

    fn pick_plateau_biome(&self, t: usize, h: usize, weirdness: Parameter) -> &'static str {
        if weirdness.max >= 0 {
            PLATEAU_BIOMES_VARIANT[t][h].unwrap_or(PLATEAU_BIOMES[t][h])
        } else {
            PLATEAU_BIOMES[t][h]
        }
    }

    fn pick_peak_biome(&self, t: usize, h: usize, weirdness: Parameter) -> &'static str {
        if t <= 2 {
            if weirdness.max < 0 {
                "minecraft:jagged_peaks"
            } else {
                "minecraft:frozen_peaks"
            }
        } else if t == 3 {
            "minecraft:stony_peaks"
        } else {
            self.pick_badlands_biome(h, weirdness)
        }
    }

    fn pick_slope_biome(&self, t: usize, h: usize, weirdness: Parameter) -> &'static str {
        if t >= 3 {
            self.pick_plateau_biome(t, h, weirdness)
        } else if h <= 1 {
            "minecraft:snowy_slopes"
        } else {
            "minecraft:grove"
        }
    }

    fn pick_shattered_biome(&self, t: usize, h: usize, weirdness: Parameter) -> &'static str {
        SHATTERED_BIOMES[t][h].unwrap_or_else(|| self.pick_middle_biome(t, h, weirdness))
    }

    #[allow(clippy::too_many_arguments)]
    fn add_surface_biome(
        &self,
        out: &mut Vec<(ParameterPoint, &'static str)>,
        temperature: Parameter,
        humidity: Parameter,
        continentalness: Parameter,
        erosion: Parameter,
        weirdness: Parameter,
        offset: f32,
        biome: &'static str,
    ) {
        let q_offset = quantize_coord(offset);
        out.push((
            ParameterPoint {
                temperature,
                humidity,
                continentalness,
                erosion,
                depth: Parameter::point(0.0),
                weirdness,
                offset: q_offset,
            },
            biome,
        ));
        out.push((
            ParameterPoint {
                temperature,
                humidity,
                continentalness,
                erosion,
                depth: Parameter::point(1.0),
                weirdness,
                offset: q_offset,
            },
            biome,
        ));
    }

    #[allow(clippy::too_many_arguments)]
    fn add_underground_biome(
        &self,
        out: &mut Vec<(ParameterPoint, &'static str)>,
        temperature: Parameter,
        humidity: Parameter,
        continentalness: Parameter,
        erosion: Parameter,
        weirdness: Parameter,
        offset: f32,
        biome: &'static str,
    ) {
        out.push((
            ParameterPoint {
                temperature,
                humidity,
                continentalness,
                erosion,
                depth: Parameter::span(0.2, 0.9),
                weirdness,
                offset: quantize_coord(offset),
            },
            biome,
        ));
    }

    #[allow(clippy::too_many_arguments)]
    fn add_bottom_biome(
        &self,
        out: &mut Vec<(ParameterPoint, &'static str)>,
        temperature: Parameter,
        humidity: Parameter,
        continentalness: Parameter,
        erosion: Parameter,
        weirdness: Parameter,
        offset: f32,
        biome: &'static str,
    ) {
        out.push((
            ParameterPoint {
                temperature,
                humidity,
                continentalness,
                erosion,
                depth: Parameter::point(1.1),
                weirdness,
                offset: quantize_coord(offset),
            },
            biome,
        ));
    }
}
