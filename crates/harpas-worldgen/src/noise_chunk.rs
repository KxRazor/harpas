//! `NoiseChunk`: per-chunk density function caching and `4×8×4` trilinear cell interpolation
//!
//! Evaluates a [`CompiledDensityGraph`] for a 16×16 chunk column using vanilla's 5 cache and interpolation
//! wrappers (`FlatCache`, `Cache2D`, `CacheOnce`, `CacheAllInCell`, and `NoiseInterpolator`) in vanilla's
//! exact Y -> X -> Z interpolation order (`docs/world.md#22-the-noise-chunk-pipeline`).

use std::collections::HashMap;

use crate::biome::{TargetPoint, quantize_coord};
use crate::data::NoiseSamplingSettings;
use crate::density::{
    BinaryOp, CompiledDensityGraph, CompiledNoiseRouter, DensityOp, NodeId, eval_end_islands,
    y_clamped_gradient,
};

#[derive(Debug, Clone)]
struct FlatCacheState {
    values: Vec<f64>,
    initialized: bool,
}

#[derive(Debug, Clone, Copy)]
struct Cache2DState {
    last_pos_x: i32,
    last_pos_z: i32,
    last_value: f64,
}

impl Default for Cache2DState {
    fn default() -> Self {
        Self {
            last_pos_x: i32::MIN,
            last_pos_z: i32::MIN,
            last_value: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct CacheOnceState {
    last_counter: u64,
    last_value: f64,
}

#[derive(Debug, Clone)]
struct CacheAllInCellState {
    values: Vec<f64>,
}

/// Per-interpolator corner slices and current trilinearly interpolated values (`NoiseChunk.NoiseInterpolator`).
#[derive(Debug, Clone)]
struct InterpolatorState {
    /// `[cell_count_xz + 1][cell_count_y + 1]` flattened as `iz * (cell_count_y + 1) + iy` for `cell_x`.
    slice0: Vec<f64>,
    /// `[cell_count_xz + 1][cell_count_y + 1]` flattened as `iz * (cell_count_y + 1) + iy` for `cell_x + 1`.
    slice1: Vec<f64>,
    y_stride: usize,
    v000: f64,
    v001: f64,
    v100: f64,
    v101: f64,
    v010: f64,
    v011: f64,
    v110: f64,
    v111: f64,
    noise00: f64,
    noise10: f64,
    noise01: f64,
    noise11: f64,
    noise0: f64,
    noise1: f64,
    value: f64,
}

impl InterpolatorState {
    fn new(cell_count_xz: usize, cell_count_y: usize) -> Self {
        let y_stride = cell_count_y + 1;
        let len = (cell_count_xz + 1) * y_stride;
        Self {
            slice0: vec![0.0; len],
            slice1: vec![0.0; len],
            y_stride,
            v000: 0.0,
            v001: 0.0,
            v100: 0.0,
            v101: 0.0,
            v010: 0.0,
            v011: 0.0,
            v110: 0.0,
            v111: 0.0,
            noise00: 0.0,
            noise10: 0.0,
            noise01: 0.0,
            noise11: 0.0,
            noise0: 0.0,
            noise1: 0.0,
            value: 0.0,
        }
    }

    #[inline]
    fn select_cell_yz(&mut self, dy: usize, dz: usize) {
        let idx0 = dz * self.y_stride + dy;
        let idx1 = (dz + 1) * self.y_stride + dy;
        self.v000 = self.slice0[idx0];
        self.v001 = self.slice0[idx1];
        self.v100 = self.slice1[idx0];
        self.v101 = self.slice1[idx1];
        self.v010 = self.slice0[idx0 + 1];
        self.v011 = self.slice0[idx1 + 1];
        self.v110 = self.slice1[idx0 + 1];
        self.v111 = self.slice1[idx1 + 1];
    }

    /// Interpolates along Y first (`NoiseInterpolator.updateForY`).
    #[inline]
    fn update_for_y(&mut self, delta_y: f64) {
        self.noise00 = lerp_f64(delta_y, self.v000, self.v010);
        self.noise10 = lerp_f64(delta_y, self.v100, self.v110);
        self.noise01 = lerp_f64(delta_y, self.v001, self.v011);
        self.noise11 = lerp_f64(delta_y, self.v101, self.v111);
    }

    /// Interpolates along X second (`NoiseInterpolator.updateForX`).
    #[inline]
    fn update_for_x(&mut self, delta_x: f64) {
        self.noise0 = lerp_f64(delta_x, self.noise00, self.noise10);
        self.noise1 = lerp_f64(delta_x, self.noise01, self.noise11);
    }

    /// Interpolates along Z third (`NoiseInterpolator.updateForZ`).
    #[inline]
    fn update_for_z(&mut self, delta_z: f64) {
        self.value = lerp_f64(delta_z, self.noise0, self.noise1);
    }

    #[inline]
    fn swap_slices(&mut self) {
        std::mem::swap(&mut self.slice0, &mut self.slice1);
    }
}

/// Vanilla's `Mth.lerp(double delta, double start, double end)`.
#[inline]
fn lerp_f64(delta: f64, start: f64, end: f64) -> f64 {
    start + delta * (end - start)
}

/// Per-chunk stateful density evaluator (`NoiseChunk`).
#[derive(Debug, Clone)]
pub struct NoiseChunk<'a> {
    graph: &'a CompiledDensityGraph,
    router: &'a CompiledNoiseRouter,
    settings: NoiseSamplingSettings,
    chunk_x: i32,
    chunk_z: i32,
    cell_width: i32,
    cell_height: i32,
    cell_count_xz: usize,
    cell_count_y: usize,
    first_cell_x: i32,
    first_cell_y: i32,
    first_cell_z: i32,
    first_noise_x: i32,
    first_noise_z: i32,
    noise_size_xz: usize,
    interpolating: bool,
    filling_cell: bool,
    cell_start_block_x: i32,
    cell_start_block_y: i32,
    cell_start_block_z: i32,
    in_cell_x: usize,
    in_cell_y: usize,
    in_cell_z: usize,
    interpolation_counter: u64,
    array_interpolation_counter: u64,
    flat_caches: Vec<FlatCacheState>,
    cache_2d: Vec<Cache2DState>,
    cache_once: Vec<CacheOnceState>,
    cache_all_in_cell: Vec<CacheAllInCellState>,
    interpolators: Vec<InterpolatorState>,
    preliminary_surface_level_cache: HashMap<i64, i32>,
    neighbor_biomes: HashMap<(i32, i32), Vec<u32>>,
}

impl<'a> NoiseChunk<'a> {
    /// Creates a `NoiseChunk` for chunk coordinates `(chunk_x, chunk_z)` and eagerly populates its `5×5`
    /// quart-resolution `FlatCache` grids at `y = 0` in bottom-up topological order.
    pub fn new(
        graph: &'a CompiledDensityGraph,
        router: &'a CompiledNoiseRouter,
        settings: NoiseSamplingSettings,
        chunk_x: i32,
        chunk_z: i32,
    ) -> Self {
        let cell_width = settings.cell_width();
        let cell_height = settings.cell_height();
        let cell_count_xz = settings.cell_count_xz();
        let cell_count_y = settings.cell_count_y();
        let first_block_x = chunk_x * 16;
        let first_block_z = chunk_z * 16;
        let first_cell_x = first_block_x.div_euclid(cell_width);
        let first_cell_y = settings.min_y.div_euclid(cell_height);
        let first_cell_z = first_block_z.div_euclid(cell_width);
        let first_noise_x = first_block_x >> 2;
        let first_noise_z = first_block_z >> 2;
        let noise_size_xz = ((cell_count_xz as i32 * cell_width) >> 2) as usize;
        let flat_grid_width = noise_size_xz + 1;

        let flat_caches = (0..graph.flat_cache_args().len())
            .map(|_| FlatCacheState {
                values: vec![0.0; flat_grid_width * flat_grid_width],
                initialized: false,
            })
            .collect();
        let cache_2d = vec![Cache2DState::default(); graph.cache_2d_args().len()];
        let cache_once = vec![CacheOnceState::default(); graph.cache_once_args().len()];
        let cell_volume = (cell_width as usize) * (cell_width as usize) * (cell_height as usize);
        let cache_all_in_cell = (0..graph.cache_all_in_cell_args().len())
            .map(|_| CacheAllInCellState {
                values: vec![0.0; cell_volume],
            })
            .collect();
        let interpolators = (0..graph.interpolator_args().len())
            .map(|_| InterpolatorState::new(cell_count_xz, cell_count_y))
            .collect();

        let mut chunk = Self {
            graph,
            router,
            settings,
            chunk_x,
            chunk_z,
            cell_width,
            cell_height,
            cell_count_xz,
            cell_count_y,
            first_cell_x,
            first_cell_y,
            first_cell_z,
            first_noise_x,
            first_noise_z,
            noise_size_xz,
            interpolating: false,
            filling_cell: false,
            cell_start_block_x: first_block_x,
            cell_start_block_y: settings.min_y,
            cell_start_block_z: first_block_z,
            in_cell_x: 0,
            in_cell_y: 0,
            in_cell_z: 0,
            interpolation_counter: 0,
            array_interpolation_counter: 0,
            flat_caches,
            cache_2d,
            cache_once,
            cache_all_in_cell,
            interpolators,
            preliminary_surface_level_cache: HashMap::new(),
            neighbor_biomes: HashMap::new(),
        };

        chunk.populate_flat_caches();
        chunk
    }

    fn populate_flat_caches(&mut self) {
        let flat_count = self.graph.flat_cache_args().len();
        let grid_w = self.noise_size_xz + 1;
        for slot_idx in 0..flat_count {
            let arg_node = self.graph.flat_cache_args()[slot_idx];
            for qz in 0..=self.noise_size_xz {
                let quart_z = self.first_noise_z + (qz as i32);
                let block_z = quart_z << 2;
                for qx in 0..=self.noise_size_xz {
                    let quart_x = self.first_noise_x + (qx as i32);
                    let block_x = quart_x << 2;
                    let val = self.eval_point(arg_node, block_x, 0, block_z);
                    self.flat_caches[slot_idx].values[qx * grid_w + qz] = val;
                }
            }
            self.flat_caches[slot_idx].initialized = true;
        }
    }

    /// Underlying compiled density graph.
    pub const fn graph(&self) -> &'a CompiledDensityGraph {
        self.graph
    }

    /// Underlying compiled noise router.
    pub const fn router(&self) -> &'a CompiledNoiseRouter {
        self.router
    }

    /// Noise sampling grid settings (`min_y`, `height`, `size_horizontal`, `size_vertical`).
    pub const fn settings(&self) -> NoiseSamplingSettings {
        self.settings
    }

    /// Chunk X coordinate.
    pub const fn chunk_x(&self) -> i32 {
        self.chunk_x
    }

    /// Chunk Z coordinate.
    pub const fn chunk_z(&self) -> i32 {
        self.chunk_z
    }

    /// Width of one cell in blocks (`4` in Overworld).
    pub const fn cell_width(&self) -> i32 {
        self.cell_width
    }

    /// Height of one cell in blocks (`8` in Overworld).
    pub const fn cell_height(&self) -> i32 {
        self.cell_height
    }

    /// Number of cells along X and Z in a chunk (`4` in Overworld).
    pub const fn cell_count_xz(&self) -> usize {
        self.cell_count_xz
    }

    /// Number of cells along Y in a chunk (`48` in Overworld).
    pub const fn cell_count_y(&self) -> usize {
        self.cell_count_y
    }

    /// Minimum cell X index of this chunk (`first_block_x / cell_width`).
    pub const fn first_cell_x(&self) -> i32 {
        self.first_cell_x
    }

    /// Minimum cell Y index of this chunk (`min_y / cell_height`).
    pub const fn first_cell_y(&self) -> i32 {
        self.first_cell_y
    }

    /// Minimum cell Z index of this chunk (`first_block_z / cell_width`).
    pub const fn first_cell_z(&self) -> i32 {
        self.first_cell_z
    }

    /// Current block X coordinate (`cell_start_block_x + in_cell_x`).
    #[inline]
    pub const fn block_x(&self) -> i32 {
        self.cell_start_block_x + (self.in_cell_x as i32)
    }

    /// Current block Y coordinate (`cell_start_block_y + in_cell_y`).
    #[inline]
    pub const fn block_y(&self) -> i32 {
        self.cell_start_block_y + (self.in_cell_y as i32)
    }

    /// Current block Z coordinate (`cell_start_block_z + in_cell_z`).
    #[inline]
    pub const fn block_z(&self) -> i32 {
        self.cell_start_block_z + (self.in_cell_z as i32)
    }

    /// Begins cell interpolation and fills `slice0` at `first_cell_x` (`NoiseChunk.initializeForFirstCellX`).
    pub fn initialize_for_first_cell_x(&mut self) {
        assert!(!self.interpolating, "already interpolating");
        self.interpolating = true;
        self.interpolation_counter = 0;
        self.fill_slice(true, self.first_cell_x);
    }

    /// Advances to cell X offset `dx` (`0..cell_count_xz`) and fills `slice1` at `first_cell_x + dx + 1`
    /// (`NoiseChunk.advanceCellX`).
    pub fn advance_cell_x(&mut self, dx: usize) {
        self.fill_slice(false, self.first_cell_x + (dx as i32) + 1);
        self.cell_start_block_x = (self.first_cell_x + (dx as i32)) * self.cell_width;
    }

    fn fill_slice(&mut self, is_slice_0: bool, cell_x: i32) {
        self.cell_start_block_x = cell_x * self.cell_width;
        self.in_cell_x = 0;
        let interp_count = self.graph.interpolator_args().len();
        let y_stride = self.cell_count_y + 1;

        for iz in 0..=self.cell_count_xz {
            let cell_z = self.first_cell_z + (iz as i32);
            self.cell_start_block_z = cell_z * self.cell_width;
            self.in_cell_z = 0;
            self.array_interpolation_counter = self.array_interpolation_counter.wrapping_add(1);

            for iy in 0..=self.cell_count_y {
                let cell_y = self.first_cell_y + (iy as i32);
                self.cell_start_block_y = cell_y * self.cell_height;
                self.in_cell_y = 0;
                self.interpolation_counter = self.interpolation_counter.wrapping_add(1);

                // While evaluating interpolator corners, `interpolating` is temporarily suspended so inner
                // functions evaluate exact corner values while sharing `CacheOnce` via `interpolation_counter`.
                let prev_interpolating = self.interpolating;
                self.interpolating = false;
                self.filling_cell = true;
                let flat_idx = iz * y_stride + iy;
                for slot in 0..interp_count {
                    let arg_node = self.graph.interpolator_args()[slot];
                    let val = self.eval_node(arg_node);
                    if is_slice_0 {
                        self.interpolators[slot].slice0[flat_idx] = val;
                    } else {
                        self.interpolators[slot].slice1[flat_idx] = val;
                    }
                }
                self.filling_cell = false;
                self.interpolating = prev_interpolating;
            }
        }
    }

    /// Selects the cell at `(dy, dz)` (`dy` in `0..cell_count_y`, `dz` in `0..cell_count_xz`), loads the 8
    /// corner values for every `NoiseInterpolator`, and populates any `CacheAllInCell` nodes (`NoiseChunk.selectCellYZ`).
    pub fn select_cell_yz(&mut self, dy: usize, dz: usize) {
        for interp in &mut self.interpolators {
            interp.select_cell_yz(dy, dz);
        }
        self.filling_cell = true;
        self.cell_start_block_y = (self.first_cell_y + (dy as i32)) * self.cell_height;
        self.cell_start_block_z = (self.first_cell_z + (dz as i32)) * self.cell_width;
        self.array_interpolation_counter = self.array_interpolation_counter.wrapping_add(1);

        let all_in_cell_count = self.graph.cache_all_in_cell_args().len();
        if all_in_cell_count > 0 {
            let cw = self.cell_width as usize;
            let ch = self.cell_height as usize;
            for ix in 0..cw {
                self.in_cell_x = ix;
                for iz in 0..cw {
                    self.in_cell_z = iz;
                    for iy in 0..ch {
                        self.in_cell_y = iy;
                        self.interpolation_counter = self.interpolation_counter.wrapping_add(1);
                        let prev_interp = self.interpolating;
                        self.interpolating = false;
                        let idx = (ix * cw + iz) * ch + iy;
                        for slot in 0..all_in_cell_count {
                            let arg = self.graph.cache_all_in_cell_args()[slot];
                            let v = self.eval_node(arg);
                            self.cache_all_in_cell[slot].values[idx] = v;
                        }
                        self.interpolating = prev_interp;
                    }
                }
            }
        }
        self.filling_cell = false;
    }

    /// Updates all `NoiseInterpolator`s for `in_cell_y` (`0..cell_height`) and `delta_y = in_cell_y / cell_height`
    /// (`NoiseChunk.updateForY`).
    #[inline]
    pub fn update_for_y(&mut self, in_cell_y: usize, delta_y: f64) {
        self.in_cell_y = in_cell_y;
        for interp in &mut self.interpolators {
            interp.update_for_y(delta_y);
        }
    }

    /// Updates all `NoiseInterpolator`s for `in_cell_x` (`0..cell_width`) and `delta_x = in_cell_x / cell_width`
    /// (`NoiseChunk.updateForX`).
    #[inline]
    pub fn update_for_x(&mut self, in_cell_x: usize, delta_x: f64) {
        self.in_cell_x = in_cell_x;
        for interp in &mut self.interpolators {
            interp.update_for_x(delta_x);
        }
    }

    /// Updates all `NoiseInterpolator`s for `in_cell_z` (`0..cell_width`) and `delta_z = in_cell_z / cell_width`,
    /// and increments `interpolation_counter` (`NoiseChunk.updateForZ`).
    #[inline]
    pub fn update_for_z(&mut self, in_cell_z: usize, delta_z: f64) {
        self.in_cell_z = in_cell_z;
        self.interpolation_counter = self.interpolation_counter.wrapping_add(1);
        for interp in &mut self.interpolators {
            interp.update_for_z(delta_z);
        }
    }

    /// Swaps `slice0` and `slice1` on every `NoiseInterpolator` at the end of a cell X column (`NoiseChunk.swapSlices`).
    #[inline]
    pub fn swap_slices(&mut self) {
        for interp in &mut self.interpolators {
            interp.swap_slices();
        }
    }

    /// Ends cell interpolation (`NoiseChunk.stopInterpolation`).
    #[inline]
    pub fn stop_interpolation(&mut self) {
        assert!(self.interpolating, "not currently interpolating");
        self.interpolating = false;
    }

    /// Evaluates `node` using `self` as the active `NoiseChunk` context (`ctx == NoiseChunk.this`).
    #[inline]
    pub fn eval_node(&mut self, node: NodeId) -> f64 {
        let bx = self.block_x();
        let by = self.block_y();
        let bz = self.block_z();
        self.eval_internal(node, bx, by, bz, true)
    }

    /// Evaluates `node` at `(block_x, block_y, block_z)` as a `SinglePointContext` (`ctx != NoiseChunk.this`),
    /// using `FlatCache` and `Cache2D` while bypassing cell-bound `Interpolated`, `CacheOnce`, and `CacheAllInCell` state.
    #[inline]
    pub fn eval_point(&mut self, node: NodeId, block_x: i32, block_y: i32, block_z: i32) -> f64 {
        self.eval_internal(node, block_x, block_y, block_z, false)
    }

    /// The biomes kept for another chunk by [`Self::keep_neighbor_biomes`].
    pub(crate) fn neighbor_biomes(&self, chunk: (i32, i32)) -> Option<&[u32]> {
        self.neighbor_biomes.get(&chunk).map(Vec::as_slice)
    }

    /// Keeps the biomes filled for another chunk while this chunk generates.
    pub(crate) fn keep_neighbor_biomes(&mut self, chunk: (i32, i32), biomes: Vec<u32>) {
        self.neighbor_biomes.insert(chunk, biomes);
    }

    /// Computes and caches the preliminary surface level (`NoiseChunk.preliminarySurfaceLevel(blockX, blockZ)`)
    /// at quart-aligned coordinates `(block_x >> 2, block_z >> 2)`.
    pub fn preliminary_surface_level(&mut self, block_x: i32, block_z: i32) -> i32 {
        let quart_x = block_x >> 2;
        let quart_z = block_z >> 2;
        let key = ((quart_x as i64) & 0xffff_ffff) | (((quart_z as i64) & 0xffff_ffff) << 32);
        if let Some(&cached) = self.preliminary_surface_level_cache.get(&key) {
            return cached;
        }
        let aligned_x = quart_x << 2;
        let aligned_z = quart_z << 2;
        let level = self
            .eval_point(
                self.router.preliminary_surface_level,
                aligned_x,
                0,
                aligned_z,
            )
            .floor() as i32;
        self.preliminary_surface_level_cache.insert(key, level);
        level
    }

    /// Samples the 6 climate channels of the router at quart coordinates `(quart_x, quart_y, quart_z)`, reading
    /// 2D climate channels from `FlatCache` (`NoiseChunk.cachedClimateSampler`).
    pub fn sample_climate(&mut self, quart_x: i32, quart_y: i32, quart_z: i32) -> TargetPoint {
        let bx = quart_x << 2;
        let by = quart_y << 2;
        let bz = quart_z << 2;
        let temperature =
            quantize_coord(self.eval_point(self.router.temperature, bx, by, bz) as f32);
        let humidity = quantize_coord(self.eval_point(self.router.vegetation, bx, by, bz) as f32);
        let continentalness =
            quantize_coord(self.eval_point(self.router.continents, bx, by, bz) as f32);
        let erosion = quantize_coord(self.eval_point(self.router.erosion, bx, by, bz) as f32);
        let depth = quantize_coord(self.eval_point(self.router.depth, bx, by, bz) as f32);
        let weirdness = quantize_coord(self.eval_point(self.router.ridges, bx, by, bz) as f32);
        TargetPoint {
            temperature,
            humidity,
            continentalness,
            erosion,
            depth,
            weirdness,
        }
    }

    fn eval_internal(
        &mut self,
        node: NodeId,
        bx: i32,
        by: i32,
        bz: i32,
        is_chunk_ctx: bool,
    ) -> f64 {
        let compiled = &self.graph.nodes()[node.0 as usize];
        match &compiled.op {
            DensityOp::Constant(bits) => f64::from_bits(*bits),
            DensityOp::Unary { op, argument } => {
                let op = *op;
                let arg = *argument;
                let v = self.eval_internal(arg, bx, by, bz, is_chunk_ctx);
                op.apply(v)
            }
            DensityOp::Binary {
                op,
                argument1,
                argument2,
            } => {
                let op = *op;
                let a1 = *argument1;
                let a2 = *argument2;
                let v1 = self.eval_internal(a1, bx, by, bz, is_chunk_ctx);
                match op {
                    BinaryOp::Add => v1 + self.eval_internal(a2, bx, by, bz, is_chunk_ctx),
                    BinaryOp::Mul => {
                        if v1 == 0.0 {
                            0.0
                        } else {
                            v1 * self.eval_internal(a2, bx, by, bz, is_chunk_ctx)
                        }
                    }
                    BinaryOp::Min => {
                        let min2 = self.graph.nodes()[a2.0 as usize].min_value;
                        if v1 < min2 {
                            v1
                        } else {
                            v1.min(self.eval_internal(a2, bx, by, bz, is_chunk_ctx))
                        }
                    }
                    BinaryOp::Max => {
                        let max2 = self.graph.nodes()[a2.0 as usize].max_value;
                        if v1 > max2 {
                            v1
                        } else {
                            v1.max(self.eval_internal(a2, bx, by, bz, is_chunk_ctx))
                        }
                    }
                }
            }
            DensityOp::FlatCache { slot, argument } => {
                let slot_idx = slot.0 as usize;
                let arg = *argument;
                let qx = bx >> 2;
                let qz = bz >> 2;
                let ix = qx - self.first_noise_x;
                let iz = qz - self.first_noise_z;
                let grid_w = (self.noise_size_xz + 1) as i32;
                if ix >= 0
                    && iz >= 0
                    && ix < grid_w
                    && iz < grid_w
                    && self.flat_caches[slot_idx].initialized
                {
                    return self.flat_caches[slot_idx].values
                        [(ix as usize) * (grid_w as usize) + (iz as usize)];
                }
                self.eval_internal(arg, bx, by, bz, is_chunk_ctx)
            }
            DensityOp::Cache2D { slot, argument } => {
                let slot_idx = slot.0 as usize;
                let arg = *argument;
                let cached = self.cache_2d[slot_idx];
                if cached.last_pos_x == bx && cached.last_pos_z == bz {
                    return cached.last_value;
                }
                let val = self.eval_internal(arg, bx, by, bz, is_chunk_ctx);
                self.cache_2d[slot_idx] = Cache2DState {
                    last_pos_x: bx,
                    last_pos_z: bz,
                    last_value: val,
                };
                val
            }
            DensityOp::CacheOnce { slot, argument } => {
                let slot_idx = slot.0 as usize;
                let arg = *argument;
                if !is_chunk_ctx {
                    return self.eval_internal(arg, bx, by, bz, false);
                }
                let counter = self.interpolation_counter;
                if self.cache_once[slot_idx].last_counter == counter && counter != 0 {
                    return self.cache_once[slot_idx].last_value;
                }
                let val = self.eval_internal(arg, bx, by, bz, true);
                self.cache_once[slot_idx] = CacheOnceState {
                    last_counter: counter,
                    last_value: val,
                };
                val
            }
            DensityOp::CacheAllInCell { slot, argument } => {
                let slot_idx = slot.0 as usize;
                let arg = *argument;
                if !is_chunk_ctx || !self.interpolating {
                    return self.eval_internal(arg, bx, by, bz, is_chunk_ctx);
                }
                let cw = self.cell_width as usize;
                let ch = self.cell_height as usize;
                let idx = (self.in_cell_x * cw + self.in_cell_z) * ch + self.in_cell_y;
                self.cache_all_in_cell[slot_idx].values[idx]
            }
            DensityOp::Interpolated { slot, argument } => {
                let slot_idx = slot.0 as usize;
                let arg = *argument;
                if !is_chunk_ctx || !self.interpolating {
                    return self.eval_internal(arg, bx, by, bz, is_chunk_ctx);
                }
                self.interpolators[slot_idx].value
            }
            DensityOp::Clamp {
                input,
                min_bits,
                max_bits,
            } => {
                let in_id = *input;
                let min_v = f64::from_bits(*min_bits);
                let max_v = f64::from_bits(*max_bits);
                self.eval_internal(in_id, bx, by, bz, is_chunk_ctx)
                    .clamp(min_v, max_v)
            }
            DensityOp::YClampedGradient {
                from_y,
                to_y,
                from_value_bits,
                to_value_bits,
            } => y_clamped_gradient(
                by,
                *from_y,
                *to_y,
                f64::from_bits(*from_value_bits),
                f64::from_bits(*to_value_bits),
            ),
            DensityOp::RangeChoice {
                input,
                min_inclusive_bits,
                max_exclusive_bits,
                when_in_range,
                when_out_of_range,
            } => {
                let in_id = *input;
                let min_inc = f64::from_bits(*min_inclusive_bits);
                let max_exc = f64::from_bits(*max_exclusive_bits);
                let yes = *when_in_range;
                let no = *when_out_of_range;
                let v = self.eval_internal(in_id, bx, by, bz, is_chunk_ctx);
                if v >= min_inc && v < max_exc {
                    self.eval_internal(yes, bx, by, bz, is_chunk_ctx)
                } else {
                    self.eval_internal(no, bx, by, bz, is_chunk_ctx)
                }
            }
            DensityOp::Noise {
                noise,
                xz_scale_bits,
                y_scale_bits,
            } => {
                let noise_id = *noise;
                let xz_scale = f64::from_bits(*xz_scale_bits);
                let y_scale = f64::from_bits(*y_scale_bits);
                self.graph.noise(noise_id).get_value(
                    f64::from(bx) * xz_scale,
                    f64::from(by) * y_scale,
                    f64::from(bz) * xz_scale,
                )
            }
            DensityOp::ShiftedNoise {
                noise,
                xz_scale_bits,
                y_scale_bits,
                shift_x,
                shift_y,
                shift_z,
            } => {
                let noise_id = *noise;
                let xz_scale = f64::from_bits(*xz_scale_bits);
                let y_scale = f64::from_bits(*y_scale_bits);
                let sx = *shift_x;
                let sy = *shift_y;
                let sz = *shift_z;
                let dx =
                    f64::from(bx) * xz_scale + self.eval_internal(sx, bx, by, bz, is_chunk_ctx);
                let dy = f64::from(by) * y_scale + self.eval_internal(sy, bx, by, bz, is_chunk_ctx);
                let dz =
                    f64::from(bz) * xz_scale + self.eval_internal(sz, bx, by, bz, is_chunk_ctx);
                self.graph.noise(noise_id).get_value(dx, dy, dz)
            }
            DensityOp::ShiftA { noise } => {
                let noise_id = *noise;
                self.graph.noise(noise_id).get_value(
                    f64::from(bx) * 0.25,
                    0.0,
                    f64::from(bz) * 0.25,
                ) * 4.0
            }
            DensityOp::ShiftB { noise } => {
                let noise_id = *noise;
                self.graph.noise(noise_id).get_value(
                    f64::from(bz) * 0.25,
                    f64::from(bx) * 0.25,
                    0.0,
                ) * 4.0
            }
            DensityOp::Shift { noise } => {
                let noise_id = *noise;
                self.graph.noise(noise_id).get_value(
                    f64::from(bx) * 0.25,
                    f64::from(by) * 0.25,
                    f64::from(bz) * 0.25,
                ) * 4.0
            }
            DensityOp::WeirdScaledSampler {
                input,
                noise,
                mapper,
            } => {
                let in_id = *input;
                let noise_id = *noise;
                let mapper = *mapper;
                let v = self.eval_internal(in_id, bx, by, bz, is_chunk_ctx);
                let rarity = mapper.map(v);
                rarity
                    * self
                        .graph
                        .noise(noise_id)
                        .get_value(
                            f64::from(bx) / rarity,
                            f64::from(by) / rarity,
                            f64::from(bz) / rarity,
                        )
                        .abs()
            }
            DensityOp::OldBlendedNoise(id) => {
                let id = *id;
                self.graph
                    .blended_noise(id)
                    .sample(f64::from(bx), f64::from(by), f64::from(bz))
            }
            DensityOp::EndIslands => {
                let simplex = self
                    .graph
                    .end_islands_noise()
                    .expect("end_islands_noise initialized");
                eval_end_islands(simplex, bx, bz)
            }
            DensityOp::FindTopSurface {
                density,
                upper_bound,
                lower_bound,
                cell_height,
            } => {
                let density_id = *density;
                let upper_id = *upper_bound;
                let lower_bound = *lower_bound;
                let cell_height = *cell_height;
                let upper = self.eval_internal(upper_id, bx, by, bz, is_chunk_ctx);
                let top_y =
                    ((upper / f64::from(cell_height)).floor() as i32).saturating_mul(cell_height);
                if top_y <= lower_bound {
                    return f64::from(lower_bound);
                }
                let mut y = top_y;
                while y >= lower_bound {
                    if self.eval_internal(density_id, bx, y, bz, false) > 0.0 {
                        return f64::from(y);
                    }
                    y -= cell_height;
                }
                f64::from(lower_bound)
            }
            DensityOp::Spline(spline) => {
                let spline = spline.clone();
                let val_f32 = spline
                    .apply_with(&mut |coord| self.eval_internal(coord, bx, by, bz, is_chunk_ctx));
                f64::from(val_f32)
            }
        }
    }
}
