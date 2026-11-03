//! `NoiseBasedAquifer`: 3D localized water and lava aquifers with pressure-based stone barriers
//!
//! Evaluates local fluid levels, Deep Dark aquifer suppression, randomized spread levels, deep lava pockets,
//! and barrier density between the 3 nearest aquifer centers on a `16×12×16` grid. A fourth center is
//! considered when deciding whether adjacent fluids need postprocessing
//! (`docs/world.md#world-generation`).

use harpas_core::random::RandomSource;

use crate::density::PositionalRandom;
use crate::noise_chunk::NoiseChunk;

const X_RANGE: i32 = 10;
const Y_RANGE: i32 = 9;
const Z_RANGE: i32 = 10;
const X_SPACING: i32 = 16;
const Y_SPACING: i32 = 12;
const Z_SPACING: i32 = 16;

/// Way below any valid build height (`DimensionType.WAY_BELOW_MIN_Y = DimensionType.MIN_Y << 4 = -2032 << 4`).
pub const WAY_BELOW_MIN_Y: i32 = -2032 << 4;

/// `similarity(10^2, 12^2) = 1.0 - (144 - 100) / 25.0 = -0.76`.
const FLOWING_UPDATE_SIMILARITY: f64 = 1.0 - 44.0 / 25.0;

/// 13 chunk offsets sampled around an aquifer center to find `minPreliminarySurface` and surface water,
/// with `(0, 0)` first so origin early-returns match vanilla (`SURFACE_SAMPLING_OFFSETS_IN_CHUNKS`).
const SURFACE_SAMPLING_OFFSETS_IN_CHUNKS: [(i32, i32); 13] = [
    (0, 0),
    (-2, -1),
    (-1, -1),
    (0, -1),
    (1, -1),
    (-3, 0),
    (-2, 0),
    (-1, 0),
    (1, 0),
    (-2, 1),
    (-1, 1),
    (0, 1),
    (1, 1),
];

/// Fluid surface level and fluid block state (`Aquifer.FluidStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FluidStatus {
    /// Y coordinate of the fluid surface (`block_y < fluid_level` is filled with `fluid_type`).
    pub fluid_level: i32,
    /// Block state ID of the fluid (`minecraft:water` or `minecraft:lava`).
    pub fluid_type: u32,
}

impl FluidStatus {
    /// Creates a `FluidStatus` with `fluid_level` and `fluid_type`.
    pub const fn new(fluid_level: i32, fluid_type: u32) -> Self {
        Self {
            fluid_level,
            fluid_type,
        }
    }

    /// Returns `self.fluid_type` when `block_y < self.fluid_level`, or `air_state` otherwise.
    #[inline]
    pub const fn at(self, block_y: i32, air_state: u32) -> u32 {
        if block_y < self.fluid_level {
            self.fluid_type
        } else {
            air_state
        }
    }
}

/// Global fallback fluid picker (`Aquifer.FluidPicker` created by `NoiseBasedChunkGenerator.createFluidPicker`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlobalFluidPicker {
    sea_level: i32,
    default_fluid_state: u32,
    lava_level: i32,
    lava_state: u32,
}

impl GlobalFluidPicker {
    /// Creates the standard Overworld/NoiseGenerator global fluid picker: lava below `y = -54` (or `sea_level`
    /// if lower), and `default_fluid_state` up to `sea_level` elsewhere.
    pub const fn new(sea_level: i32, default_fluid_state: u32, lava_state: u32) -> Self {
        Self {
            sea_level,
            default_fluid_state,
            lava_level: -54,
            lava_state,
        }
    }

    /// Returns the global `FluidStatus` at `(block_x, block_y, block_z)`.
    #[inline]
    pub fn compute_fluid(self, _block_x: i32, block_y: i32, _block_z: i32) -> FluidStatus {
        let min_level = self.lava_level.min(self.sea_level);
        if block_y < min_level {
            FluidStatus::new(self.lava_level, self.lava_state)
        } else {
            FluidStatus::new(self.sea_level, self.default_fluid_state)
        }
    }
}

/// Aquifer evaluator for a chunk (`Aquifer`), supporting both disabled (global fluid only) and 3D
/// noise-based aquifers (`NoiseBasedAquifer`).
#[derive(Debug, Clone)]
pub enum Aquifer {
    /// Disabled aquifer (`Aquifer.createDisabled`), using only the global fluid picker.
    Disabled {
        /// Global fluid picker.
        global_picker: GlobalFluidPicker,
        /// Block state ID for air (`minecraft:air`).
        air_state: u32,
    },
    /// 3D noise-based aquifer (`NoiseBasedAquifer`).
    NoiseBased(Box<NoiseBasedAquifer>),
}

impl Aquifer {
    /// Creates a disabled aquifer using `global_picker`.
    pub const fn disabled(global_picker: GlobalFluidPicker, air_state: u32) -> Self {
        Self::Disabled {
            global_picker,
            air_state,
        }
    }

    /// Creates a 3D `NoiseBasedAquifer` for chunk `(chunk_x, chunk_z)`.
    #[allow(clippy::too_many_arguments)]
    pub fn noise_based(
        chunk_x: i32,
        chunk_z: i32,
        min_y: i32,
        height: i32,
        positional_random: PositionalRandom,
        global_picker: GlobalFluidPicker,
        air_state: u32,
        water_state: u32,
        lava_state: u32,
    ) -> Self {
        Self::NoiseBased(Box::new(NoiseBasedAquifer::new(
            chunk_x,
            chunk_z,
            min_y,
            height,
            positional_random,
            global_picker,
            air_state,
            water_state,
            lava_state,
        )))
    }

    /// Evaluates the block state at `(block_x, block_y, block_z)` given `substance` (`final_density`).
    ///
    /// Returns `None` when the block is solid (to be filled by `OreVeinifier` or `default_block`), or
    /// `Some(state_id)` when filled by air or fluid.
    pub fn compute_substance(
        &mut self,
        noise_chunk: &mut NoiseChunk<'_>,
        block_x: i32,
        block_y: i32,
        block_z: i32,
        substance: f64,
    ) -> Option<u32> {
        match self {
            Self::Disabled {
                global_picker,
                air_state,
            } => {
                if substance > 0.0 {
                    None
                } else {
                    Some(
                        global_picker
                            .compute_fluid(block_x, block_y, block_z)
                            .at(block_y, *air_state),
                    )
                }
            }
            Self::NoiseBased(aquifer) => {
                aquifer.compute_substance(noise_chunk, block_x, block_y, block_z, substance)
            }
        }
    }

    /// Whether the last call to [`Self::compute_substance`] requested a fluid tick update.
    pub fn should_schedule_fluid_update(&self) -> bool {
        match self {
            Self::Disabled { .. } => false,
            Self::NoiseBased(aquifer) => aquifer.should_schedule_fluid_update(),
        }
    }
}

/// Vanilla's `NoiseBasedAquifer` for a single chunk.
#[derive(Debug, Clone)]
pub struct NoiseBasedAquifer {
    positional_random: PositionalRandom,
    global_picker: GlobalFluidPicker,
    air_state: u32,
    water_state: u32,
    lava_state: u32,
    should_schedule_fluid_update: bool,
    min_grid_x: i32,
    min_grid_y: i32,
    min_grid_z: i32,
    grid_size_x: usize,
    grid_size_z: usize,
    aquifer_cache: Vec<Option<FluidStatus>>,
    aquifer_location_cache: Vec<Option<(i32, i32, i32)>>,
}

impl NoiseBasedAquifer {
    /// Creates a `NoiseBasedAquifer` covering `(chunk_x, chunk_z)` over `[min_y, min_y + height)`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        chunk_x: i32,
        chunk_z: i32,
        min_y: i32,
        height: i32,
        positional_random: PositionalRandom,
        global_picker: GlobalFluidPicker,
        air_state: u32,
        water_state: u32,
        lava_state: u32,
    ) -> Self {
        let min_block_x = chunk_x * 16;
        let max_block_x = min_block_x + 15;
        let min_block_z = chunk_z * 16;
        let max_block_z = min_block_z + 15;

        // Include every cell that can be sampled from any position in this chunk.
        let min_grid_x = grid_x(min_block_x - 5) - 1;
        let max_grid_x = grid_x(max_block_x - 5) + 1;
        let grid_size_x = (max_grid_x - min_grid_x + 1) as usize;

        let min_grid_y = grid_y(min_y + 1) - 1;
        let max_grid_y = grid_y(min_y + height + 1) + 1;
        let grid_size_y = (max_grid_y - min_grid_y + 1) as usize;

        let min_grid_z = grid_z(min_block_z - 5) - 1;
        let max_grid_z = grid_z(max_block_z - 5) + 1;
        let grid_size_z = (max_grid_z - min_grid_z + 1) as usize;

        let total_cells = grid_size_x * grid_size_y * grid_size_z;
        Self {
            positional_random,
            global_picker,
            air_state,
            water_state,
            lava_state,
            should_schedule_fluid_update: false,
            min_grid_x,
            min_grid_y,
            min_grid_z,
            grid_size_x,
            grid_size_z,
            aquifer_cache: vec![None; total_cells],
            aquifer_location_cache: vec![None; total_cells],
        }
    }

    /// Whether the most recent `compute_substance` evaluation requested a fluid tick update.
    #[inline]
    pub const fn should_schedule_fluid_update(&self) -> bool {
        self.should_schedule_fluid_update
    }

    #[inline]
    fn get_index(&self, gx: i32, gy: i32, gz: i32) -> usize {
        let x = (gx - self.min_grid_x) as usize;
        let y = (gy - self.min_grid_y) as usize;
        let z = (gz - self.min_grid_z) as usize;
        (y * self.grid_size_z + z) * self.grid_size_x + x
    }

    #[inline]
    fn aquifer_location(&mut self, gx: i32, gy: i32, gz: i32, idx: usize) -> (i32, i32, i32) {
        if let Some(pos) = self.aquifer_location_cache[idx] {
            return pos;
        }
        let pos = match &self.positional_random {
            PositionalRandom::Xoroshiro(f) => {
                let mut rng = f.at(gx, gy, gz);
                (
                    gx * X_SPACING + rng.next_int_bounded(X_RANGE),
                    gy * Y_SPACING + rng.next_int_bounded(Y_RANGE),
                    gz * Z_SPACING + rng.next_int_bounded(Z_RANGE),
                )
            }
            PositionalRandom::Legacy(f) => {
                let mut rng = f.at(gx, gy, gz);
                (
                    gx * X_SPACING + rng.next_int_bounded(X_RANGE),
                    gy * Y_SPACING + rng.next_int_bounded(Y_RANGE),
                    gz * Z_SPACING + rng.next_int_bounded(Z_RANGE),
                )
            }
        };
        self.aquifer_location_cache[idx] = Some(pos);
        pos
    }

    /// Evaluates the aquifer substance at `(block_x, block_y, block_z)` given `substance` (`final_density`).
    pub fn compute_substance(
        &mut self,
        noise_chunk: &mut NoiseChunk<'_>,
        block_x: i32,
        block_y: i32,
        block_z: i32,
        substance: f64,
    ) -> Option<u32> {
        if substance > 0.0 {
            self.should_schedule_fluid_update = false;
            return None;
        }

        let global_fluid = self.global_picker.compute_fluid(block_x, block_y, block_z);
        if global_fluid.at(block_y, self.air_state) == self.lava_state {
            self.should_schedule_fluid_update = false;
            return Some(self.lava_state);
        }

        let anchor_x = grid_x(block_x - 5);
        let anchor_y = grid_y(block_y + 1);
        let anchor_z = grid_z(block_z - 5);

        let mut dist1 = i32::MAX;
        let mut dist2 = i32::MAX;
        let mut dist3 = i32::MAX;
        let mut dist4 = i32::MAX;
        let mut pos1 = 0_usize;
        let mut pos2 = 0_usize;
        let mut pos3 = 0_usize;
        let mut pos4 = 0_usize;

        for dx in 0..=1 {
            for dy in -1..=1 {
                for dz in 0..=1 {
                    let gx = anchor_x + dx;
                    let gy = anchor_y + dy;
                    let gz = anchor_z + dz;
                    let idx = self.get_index(gx, gy, gz);
                    let (cx, cy, cz) = self.aquifer_location(gx, gy, gz, idx);
                    let ddx = cx - block_x;
                    let ddy = cy - block_y;
                    let ddz = cz - block_z;
                    let dist = ddx * ddx + ddy * ddy + ddz * ddz;
                    if dist1 >= dist {
                        pos4 = pos3;
                        pos3 = pos2;
                        pos2 = pos1;
                        pos1 = idx;
                        dist4 = dist3;
                        dist3 = dist2;
                        dist2 = dist1;
                        dist1 = dist;
                    } else if dist2 >= dist {
                        pos4 = pos3;
                        pos3 = pos2;
                        pos2 = idx;
                        dist4 = dist3;
                        dist3 = dist2;
                        dist2 = dist;
                    } else if dist3 >= dist {
                        pos4 = pos3;
                        pos3 = idx;
                        dist4 = dist3;
                        dist3 = dist;
                    } else if dist4 >= dist {
                        pos4 = idx;
                        dist4 = dist;
                    }
                }
            }
        }

        let status1 = self.get_aquifer_status(noise_chunk, pos1);
        let sim12 = similarity(dist1, dist2);
        let fluid_state = status1.at(block_y, self.air_state);

        if sim12 <= 0.0 {
            if sim12 >= FLOWING_UPDATE_SIMILARITY {
                let status2 = self.get_aquifer_status(noise_chunk, pos2);
                self.should_schedule_fluid_update = status1 != status2;
            } else {
                self.should_schedule_fluid_update = false;
            }
            return Some(fluid_state);
        }

        if fluid_state == self.water_state
            && self
                .global_picker
                .compute_fluid(block_x, block_y - 1, block_z)
                .at(block_y - 1, self.air_state)
                == self.lava_state
        {
            self.should_schedule_fluid_update = true;
            return Some(fluid_state);
        }

        let mut barrier_noise_value: Option<f64> = None;
        let status2 = self.get_aquifer_status(noise_chunk, pos2);
        let pressure12 = sim12
            * self.calculate_pressure(
                noise_chunk,
                block_x,
                block_y,
                block_z,
                &mut barrier_noise_value,
                status1,
                status2,
            );
        if substance + pressure12 > 0.0 {
            self.should_schedule_fluid_update = false;
            return None;
        }

        let status3 = self.get_aquifer_status(noise_chunk, pos3);
        let sim13 = similarity(dist1, dist3);
        if sim13 > 0.0 {
            let pressure13 = sim12
                * sim13
                * self.calculate_pressure(
                    noise_chunk,
                    block_x,
                    block_y,
                    block_z,
                    &mut barrier_noise_value,
                    status1,
                    status3,
                );
            if substance + pressure13 > 0.0 {
                self.should_schedule_fluid_update = false;
                return None;
            }
        }

        let sim23 = similarity(dist2, dist3);
        if sim23 > 0.0 {
            let pressure23 = sim12
                * sim23
                * self.calculate_pressure(
                    noise_chunk,
                    block_x,
                    block_y,
                    block_z,
                    &mut barrier_noise_value,
                    status2,
                    status3,
                );
            if substance + pressure23 > 0.0 {
                self.should_schedule_fluid_update = false;
                return None;
            }
        }

        // Nearby centers with different fluid statuses can form a boundary even when the two closest agree.
        self.should_schedule_fluid_update = status1 != status2
            || (similarity(dist2, dist3) >= FLOWING_UPDATE_SIMILARITY && status1 != status3)
            || (similarity(dist1, dist4) >= FLOWING_UPDATE_SIMILARITY
                && status1 != self.get_aquifer_status(noise_chunk, pos4));
        Some(fluid_state)
    }

    fn get_aquifer_status(&mut self, noise_chunk: &mut NoiseChunk<'_>, idx: usize) -> FluidStatus {
        if let Some(status) = self.aquifer_cache[idx] {
            return status;
        }
        let (cx, cy, cz) = self.aquifer_location_cache[idx].expect("aquifer location cached");
        let status = self.compute_fluid(noise_chunk, cx, cy, cz);
        self.aquifer_cache[idx] = Some(status);
        status
    }

    fn compute_fluid(
        &self,
        noise_chunk: &mut NoiseChunk<'_>,
        x: i32,
        y: i32,
        z: i32,
    ) -> FluidStatus {
        let global_fluid = self.global_picker.compute_fluid(x, y, z);
        let mut min_prelim = i32::MAX;
        let top_y = y + 12;
        let bottom_y = y - 12;
        let mut center_under_water = false;

        for (offset_x, offset_z) in SURFACE_SAMPLING_OFFSETS_IN_CHUNKS {
            let sx = x + (offset_x << 4);
            let sz = z + (offset_z << 4);
            let prelim = noise_chunk.preliminary_surface_level(sx, sz);
            let surface_y = prelim + 8;
            let is_origin = offset_x == 0 && offset_z == 0;
            if is_origin && bottom_y > surface_y {
                return global_fluid;
            }
            let top_above_surface = top_y > surface_y;
            if top_above_surface || is_origin {
                let surface_fluid = self.global_picker.compute_fluid(sx, surface_y, sz);
                if surface_fluid.at(surface_y, self.air_state) != self.air_state {
                    if is_origin {
                        center_under_water = true;
                    }
                    if top_above_surface {
                        return surface_fluid;
                    }
                }
            }
            min_prelim = min_prelim.min(prelim);
        }

        let fluid_surface_level = self.compute_surface_level(
            noise_chunk,
            x,
            y,
            z,
            global_fluid,
            min_prelim,
            center_under_water,
        );
        let fluid_type =
            self.compute_fluid_type(noise_chunk, x, y, z, global_fluid, fluid_surface_level);
        FluidStatus::new(fluid_surface_level, fluid_type)
    }

    #[allow(clippy::too_many_arguments)]
    fn compute_surface_level(
        &self,
        noise_chunk: &mut NoiseChunk<'_>,
        x: i32,
        y: i32,
        z: i32,
        global_fluid: FluidStatus,
        min_prelim: i32,
        center_under_water: bool,
    ) -> i32 {
        let router = noise_chunk.router();
        let erosion_node = router.erosion;
        let depth_node = router.depth;
        let floodedness_node = router.fluid_level_floodedness_noise;

        let is_deep_dark = noise_chunk.eval_point(erosion_node, x, y, z) < f64::from(-0.225_f32)
            && noise_chunk.eval_point(depth_node, x, y, z) > f64::from(0.9_f32);

        let (partially_flooded_threshold, fully_flooded_threshold) = if is_deep_dark {
            (-1.0, -1.0)
        } else {
            let dist_below_surface = min_prelim + 8 - y;
            let factor = if center_under_water {
                clamped_map(f64::from(dist_below_surface), 0.0, 64.0, 1.0, 0.0)
            } else {
                0.0
            };
            let floodedness = noise_chunk
                .eval_point(floodedness_node, x, y, z)
                .clamp(-1.0, 1.0);
            let fully_flooded = map(factor, 1.0, 0.0, -0.3, 0.8);
            let partially_flooded = map(factor, 1.0, 0.0, -0.8, 0.4);
            (floodedness - partially_flooded, floodedness - fully_flooded)
        };

        if fully_flooded_threshold > 0.0 {
            global_fluid.fluid_level
        } else if partially_flooded_threshold > 0.0 {
            self.compute_randomized_fluid_surface_level(noise_chunk, x, y, z, min_prelim)
        } else {
            WAY_BELOW_MIN_Y
        }
    }

    fn compute_randomized_fluid_surface_level(
        &self,
        noise_chunk: &mut NoiseChunk<'_>,
        x: i32,
        y: i32,
        z: i32,
        min_prelim: i32,
    ) -> i32 {
        let cell_x = x.div_euclid(16);
        let cell_y = y.div_euclid(40);
        let cell_z = z.div_euclid(16);
        let center_y = cell_y * 40 + 20;
        let spread_node = noise_chunk.router().fluid_level_spread_noise;
        let spread = noise_chunk.eval_point(spread_node, cell_x, cell_y, cell_z) * 10.0;
        let offset = quantize(spread, 3);
        let target_level = center_y + offset;
        min_prelim.min(target_level)
    }

    fn compute_fluid_type(
        &self,
        noise_chunk: &mut NoiseChunk<'_>,
        x: i32,
        y: i32,
        z: i32,
        global_fluid: FluidStatus,
        fluid_surface_level: i32,
    ) -> u32 {
        let mut fluid_type = global_fluid.fluid_type;
        if fluid_surface_level <= -10
            && fluid_surface_level != WAY_BELOW_MIN_Y
            && global_fluid.fluid_type != self.lava_state
        {
            let cell_x = x.div_euclid(64);
            let cell_y = y.div_euclid(40);
            let cell_z = z.div_euclid(64);
            let lava_node = noise_chunk.router().lava_noise;
            let lava_noise = noise_chunk.eval_point(lava_node, cell_x, cell_y, cell_z);
            if lava_noise.abs() > 0.3 {
                fluid_type = self.lava_state;
            }
        }
        fluid_type
    }

    #[allow(clippy::too_many_arguments)]
    fn calculate_pressure(
        &self,
        noise_chunk: &mut NoiseChunk<'_>,
        bx: i32,
        by: i32,
        bz: i32,
        barrier_noise_value: &mut Option<f64>,
        status_a: FluidStatus,
        status_b: FluidStatus,
    ) -> f64 {
        let state_a = status_a.at(by, self.air_state);
        let state_b = status_b.at(by, self.air_state);
        if (state_a == self.lava_state && state_b == self.water_state)
            || (state_a == self.water_state && state_b == self.lava_state)
        {
            return 2.0;
        }
        let level_diff = (status_a.fluid_level - status_b.fluid_level).abs();
        if level_diff == 0 {
            return 0.0;
        }
        let avg_level = 0.5 * f64::from(status_a.fluid_level + status_b.fluid_level);
        let dist_from_avg = f64::from(by) + 0.5 - avg_level;
        let half_diff = f64::from(level_diff) / 2.0;
        let q = half_diff - dist_from_avg.abs();
        let base_pressure = if dist_from_avg > 0.0 {
            let p = 0.0 + q;
            if p > 0.0 { p / 1.5 } else { p / 2.5 }
        } else {
            let p = 3.0 + q;
            if p > 0.0 { p / 3.0 } else { p / 10.0 }
        };

        let noise = if !(-2.0..=2.0).contains(&base_pressure) {
            0.0
        } else if let Some(val) = *barrier_noise_value {
            val
        } else {
            let barrier_node = noise_chunk.router().barrier_noise;
            let val = noise_chunk.eval_point(barrier_node, bx, by, bz);
            *barrier_noise_value = Some(val);
            val
        };
        2.0 * (noise + base_pressure)
    }
}

#[inline]
const fn grid_x(x: i32) -> i32 {
    x.div_euclid(X_SPACING)
}

#[inline]
const fn grid_y(y: i32) -> i32 {
    y.div_euclid(Y_SPACING)
}

#[inline]
const fn grid_z(z: i32) -> i32 {
    z.div_euclid(Z_SPACING)
}

/// Vanilla's `NoiseBasedAquifer.similarity(int distSq1, int distSq2)`: `1.0 - |d2 - d1| / 25.0`.
#[inline]
pub fn similarity(dist_sq_1: i32, dist_sq_2: i32) -> f64 {
    1.0 - f64::from((dist_sq_2 - dist_sq_1).abs()) / 25.0
}

/// Vanilla's `Mth.quantize(double value, int factor)`: `floor(value / factor) * factor`.
#[inline]
pub fn quantize(value: f64, factor: i32) -> i32 {
    (value / f64::from(factor)).floor() as i32 * factor
}

/// Vanilla's `Mth.map(double value, double fromMin, double fromMax, double toMin, double toMax)`.
#[inline]
pub fn map(value: f64, from_min: f64, from_max: f64, to_min: f64, to_max: f64) -> f64 {
    let delta = (value - from_min) / (from_max - from_min);
    to_min + delta * (to_max - to_min)
}

/// Vanilla's `Mth.clampedMap(double value, double fromMin, double fromMax, double toMin, double toMax)`.
#[inline]
pub fn clamped_map(value: f64, from_min: f64, from_max: f64, to_min: f64, to_max: f64) -> f64 {
    let delta = (value - from_min) / (from_max - from_min);
    if delta < 0.0 {
        to_min
    } else if delta > 1.0 {
        to_max
    } else {
        to_min + delta * (to_max - to_min)
    }
}
