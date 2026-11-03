//! `SurfaceSystem` and `SurfaceRules`: biome surface replacement, terracotta bands, pillars, and icebergs
//!
//! Compiles the `surface_rule` JSON tree from `worldgen/noise_settings/*.json` (all 4 rule types and 11
//! condition types of Minecraft 26.1) and applies it column by column after base terrain generation
//! (`docs/world.md#world-generation`).

use harpas_core::block::{BlockStates, block_states};
use harpas_core::noise::{NormalNoise, PerlinSimplexNoise};
use harpas_core::random::{LegacyRandom, RandomSource};
use indexmap::IndexMap;
use serde_json::Value;

use crate::aquifer::map;
use crate::biome::{BiomeManager, MultiNoiseBiomeSource};
use crate::data::{BiomeDef, BlockStateDef, TemperatureModifier, WorldgenData, WorldgenDataError};
use crate::density::{CompiledDensityGraph, PositionalRandom};
use crate::noise_chunk::NoiseChunk;

/// Vertical coordinate anchor (`VerticalAnchor` in `SurfaceRules`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerticalAnchor {
    /// Absolute world Y coordinate (`{"absolute": y}`).
    Absolute(i32),
    /// Offset above `min_y` (`{"above_bottom": offset}` -> `min_y + offset`).
    AboveBottom(i32),
    /// Offset below `min_y + height - 1` (`{"below_top": offset}` -> `min_y + height - 1 - offset`).
    BelowTop(i32),
}

impl VerticalAnchor {
    /// Resolves this vertical anchor against `(min_y, height)`.
    #[inline]
    pub const fn resolve(self, min_y: i32, height: i32) -> i32 {
        match self {
            Self::Absolute(y) => y,
            Self::AboveBottom(offset) => min_y + offset,
            Self::BelowTop(offset) => min_y + height - 1 - offset,
        }
    }
}

/// Surface orientation for `minecraft:stone_depth` (`CaveSurface`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaveSurface {
    /// Floor surface (`"floor"`, measures `stone_depth_above`).
    Floor,
    /// Ceiling surface (`"ceiling"`, measures `stone_depth_below`).
    Ceiling,
}

/// Compiled condition in a surface rule (`SurfaceRules.ConditionSource`).
#[derive(Debug, Clone)]
pub enum SurfaceCondition {
    /// `minecraft:biome`: true when the current block's biome ID is in the list.
    Biome(Vec<u32>),
    /// `minecraft:noise_threshold`: true when `noise.get_value(bx, 0.0, bz)` is in `[min_threshold, max_threshold]`.
    NoiseThreshold {
        /// Seeded 3D `NormalNoise` evaluated at `(bx, 0.0, bz)`.
        noise: Box<NormalNoise>,
        /// Inclusive lower threshold.
        min_threshold: f64,
        /// Inclusive upper threshold.
        max_threshold: f64,
    },
    /// `minecraft:vertical_gradient`: probabilistic vertical transition between `true_at_and_below` and `false_at_and_above`.
    VerticalGradient {
        /// Positional random factory forked from `random_name`.
        random: PositionalRandom,
        /// Y coordinate at and below which the condition is always `true`.
        true_at_and_below: VerticalAnchor,
        /// Y coordinate at and above which the condition is always `false`.
        false_at_and_above: VerticalAnchor,
    },
    /// `minecraft:y_above`: true when `block_y + (add_stone_depth ? stone_depth_above : 0) >= anchor + surface_depth * surface_depth_multiplier`.
    YAbove {
        /// Reference vertical anchor.
        anchor: VerticalAnchor,
        /// Multiplier applied to `surface_depth`.
        surface_depth_multiplier: i32,
        /// Whether to add `stone_depth_above` to `block_y`.
        add_stone_depth: bool,
    },
    /// `minecraft:water`: true when not under fluid or `block_y + (add_stone_depth ? stone_depth_above : 0) >= water_height + offset + surface_depth * surface_depth_multiplier`.
    Water {
        /// Vertical offset relative to `water_height`.
        offset: i32,
        /// Multiplier applied to `surface_depth`.
        surface_depth_multiplier: i32,
        /// Whether to add `stone_depth_above` to `block_y`.
        add_stone_depth: bool,
    },
    /// `minecraft:temperature`: true when the biome at `(bx, by, bz)` is cold enough to snow (`getTemperature < 0.15`).
    Temperature,
    /// `minecraft:steep`: true on steep south- or west-facing slopes (`dh >= 4`).
    Steep,
    /// `minecraft:not`: logical negation of `invert`.
    Not(Box<SurfaceCondition>),
    /// `minecraft:hole`: true when `surface_depth <= 0`.
    Hole,
    /// `minecraft:above_preliminary_surface`: true when `block_y >= min_surface_level`.
    AbovePreliminarySurface,
    /// `minecraft:stone_depth`: true when `depth <= 1 + offset + (add_surface_depth ? surface_depth : 0) + secondary_depth`.
    StoneDepth {
        /// Depth offset (`0` for `ON_FLOOR` / `UNDER_FLOOR`).
        offset: i32,
        /// Whether to add `surface_depth`.
        add_surface_depth: bool,
        /// Range mapped from `surface_secondary` (`[-1, 1] -> [0, secondary_depth_range]`).
        secondary_depth_range: i32,
        /// Whether to check `stone_depth_above` (`Floor`) or `stone_depth_below` (`Ceiling`).
        surface_type: CaveSurface,
    },
}

/// Compiled surface rule (`SurfaceRules.RuleSource`).
#[derive(Debug, Clone)]
pub enum SurfaceRule {
    /// `minecraft:block`: places a fixed block state.
    Block(u32),
    /// `minecraft:sequence`: tries each rule in order and returns the first `Some(state)`.
    Sequence(Vec<SurfaceRule>),
    /// `minecraft:condition`: evaluates `then_run` if `if_true` holds.
    Condition {
        /// Guard condition.
        if_true: SurfaceCondition,
        /// Rule executed when `if_true` returns `true`.
        then_run: Box<SurfaceRule>,
    },
    /// `minecraft:bandlands`: places the terracotta band for `(bx, by, bz)`.
    Bandlands,
}

/// Evaluates height- and Simplex-noise-adjusted biome temperatures (`Biome.getTemperature`).
#[derive(Debug, Clone)]
pub struct BiomeTemperatureSampler {
    temperature_noise: PerlinSimplexNoise,
    frozen_temperature_noise: PerlinSimplexNoise,
    biome_info_noise: PerlinSimplexNoise,
}

impl Default for BiomeTemperatureSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl BiomeTemperatureSampler {
    /// Creates a `BiomeTemperatureSampler` with vanilla's fixed LegacyRandom seeds (`1234`, `3456`, `2345`).
    pub fn new() -> Self {
        let mut rng_temp = LegacyRandom::new(1234);
        let temperature_noise = PerlinSimplexNoise::new(&mut rng_temp, &[0]);

        let mut rng_frozen = LegacyRandom::new(3456);
        let frozen_temperature_noise = PerlinSimplexNoise::new(&mut rng_frozen, &[-2, -1, 0]);

        let mut rng_info = LegacyRandom::new(2345);
        let biome_info_noise = PerlinSimplexNoise::new(&mut rng_info, &[0]);

        Self {
            temperature_noise,
            frozen_temperature_noise,
            biome_info_noise,
        }
    }

    /// Computes `Biome.getHeightAdjustedTemperature` at `(bx, by, bz)` for `biome`.
    pub fn height_adjusted_temperature(
        &self,
        biome: &BiomeDef,
        bx: i32,
        by: i32,
        bz: i32,
        sea_level: i32,
    ) -> f32 {
        let base_temp = match biome.temperature_modifier {
            TemperatureModifier::None => biome.temperature,
            TemperatureModifier::Frozen => {
                let v1 = self.frozen_temperature_noise.get_value(
                    f64::from(bx) * 0.05,
                    f64::from(bz) * 0.05,
                    false,
                ) * 7.0;
                let v2 = self.biome_info_noise.get_value(
                    f64::from(bx) * 0.2,
                    f64::from(bz) * 0.2,
                    false,
                );
                if v1 + v2 < 0.3 {
                    let v3 = self.biome_info_noise.get_value(
                        f64::from(bx) * 0.09,
                        f64::from(bz) * 0.09,
                        false,
                    );
                    if v3 < 0.8 { 0.2_f32 } else { biome.temperature }
                } else {
                    biome.temperature
                }
            }
        };

        let shift_y = sea_level + 17;
        if by > shift_y {
            let noise = (self.temperature_noise.get_value(
                f64::from(bx as f32 / 8.0_f32),
                f64::from(bz as f32 / 8.0_f32),
                false,
            ) * 8.0) as f32;
            base_temp - (noise + (by as f32) - (shift_y as f32)) * 0.05_f32 / 40.0_f32
        } else {
            base_temp
        }
    }

    /// Returns `true` when `height_adjusted_temperature(biome, bx, by, bz, sea_level) < 0.15` (`Biome.coldEnoughToSnow`).
    #[inline]
    pub fn cold_enough_to_snow(
        &self,
        biome: &BiomeDef,
        bx: i32,
        by: i32,
        bz: i32,
        sea_level: i32,
    ) -> bool {
        self.height_adjusted_temperature(biome, bx, by, bz, sea_level) < 0.15_f32
    }
}

/// Abstraction over a chunk column used by [`SurfaceSystem::build_surface`].
pub trait SurfaceChunkAccess {
    /// Minimum Y coordinate of the chunk (`-64` in Overworld).
    fn min_y(&self) -> i32;
    /// Total height of the chunk in blocks (`384` in Overworld).
    fn height(&self) -> i32;
    /// Returns the block state ID at local column `(local_x, block_y, local_z)` (`local_x, local_z` in `0..16`).
    fn block_state(&self, local_x: usize, block_y: i32, local_z: usize) -> u32;
    /// Sets the block state ID at local column `(local_x, block_y, local_z)`.
    fn set_block_state(&mut self, local_x: usize, block_y: i32, local_z: usize, state: u32);
    /// Records that the fluid a surface rule placed at `(local_x, block_y, local_z)` needs postprocessing.
    fn mark_fluid_postprocessing(&mut self, local_x: usize, block_y: i32, local_z: usize);
    /// Returns the `WORLD_SURFACE_WG` height (one above the highest non-air block, or `min_y` if empty) at `(local_x, local_z)`.
    fn world_surface_wg(&self, local_x: usize, local_z: usize) -> i32;
    /// Returns the stored biome ID at quart coordinates `(quart_x, quart_y, quart_z)`.
    fn get_noise_biome(&self, quart_x: i32, quart_y: i32, quart_z: i32) -> u32;
}

/// Vanilla's `SurfaceSystem` for a dimension.
#[derive(Debug, Clone)]
pub struct SurfaceSystem {
    default_block: u32,
    sea_level: i32,
    air_state: u32,
    water_state: u32,
    lava_state: u32,
    packed_ice_state: u32,
    snow_block_state: u32,
    eroded_badlands_id: Option<u32>,
    frozen_ocean_id: Option<u32>,
    deep_frozen_ocean_id: Option<u32>,
    clay_bands: [u32; 192],
    clay_bands_offset_noise: NormalNoise,
    surface_noise: NormalNoise,
    surface_secondary_noise: NormalNoise,
    badlands_pillar_noise: NormalNoise,
    badlands_pillar_roof_noise: NormalNoise,
    badlands_surface_noise: NormalNoise,
    iceberg_pillar_noise: NormalNoise,
    iceberg_pillar_roof_noise: NormalNoise,
    iceberg_surface_noise: NormalNoise,
    noise_random: PositionalRandom,
    temperature_sampler: BiomeTemperatureSampler,
}

impl SurfaceSystem {
    /// Creates a `SurfaceSystem` from a compiled density graph, noise settings, and worldgen data.
    pub fn new(
        graph: &CompiledDensityGraph,
        default_block: u32,
        sea_level: i32,
        data: &WorldgenData,
    ) -> Result<Self, WorldgenDataError> {
        let states = block_states();
        let air_state = states.default_state("minecraft:air").unwrap_or(0);
        let water_state = states.default_state("minecraft:water").unwrap_or(0);
        let lava_state = states.default_state("minecraft:lava").unwrap_or(0);
        let packed_ice_state = states.default_state("minecraft:packed_ice").unwrap_or(0);
        let snow_block_state = states.default_state("minecraft:snow_block").unwrap_or(0);

        let root_pos = graph.positional_random();
        let noise_random = root_pos.clone();
        let mut clay_rng = root_pos.fork_from_hash_of("minecraft:clay_bands");
        let clay_bands = generate_bands(&mut *clay_rng, states);

        let noise_or_zero = |name: &str| -> Result<NormalNoise, WorldgenDataError> {
            if data.noises.contains_key(name) {
                graph.instantiate_noise(name, &data.noises)
            } else {
                let mut rng = root_pos.fork_from_hash_of(name);
                Ok(NormalNoise::new(&mut *rng, 0, &[0.0]))
            }
        };

        Ok(Self {
            default_block,
            sea_level,
            air_state,
            water_state,
            lava_state,
            packed_ice_state,
            snow_block_state,
            eroded_badlands_id: data.biome_id("minecraft:eroded_badlands"),
            frozen_ocean_id: data.biome_id("minecraft:frozen_ocean"),
            deep_frozen_ocean_id: data.biome_id("minecraft:deep_frozen_ocean"),
            clay_bands,
            clay_bands_offset_noise: noise_or_zero("minecraft:clay_bands_offset")?,
            surface_noise: noise_or_zero("minecraft:surface")?,
            surface_secondary_noise: noise_or_zero("minecraft:surface_secondary")?,
            badlands_pillar_noise: noise_or_zero("minecraft:badlands_pillar")?,
            badlands_pillar_roof_noise: noise_or_zero("minecraft:badlands_pillar_roof")?,
            badlands_surface_noise: noise_or_zero("minecraft:badlands_surface")?,
            iceberg_pillar_noise: noise_or_zero("minecraft:iceberg_pillar")?,
            iceberg_pillar_roof_noise: noise_or_zero("minecraft:iceberg_pillar_roof")?,
            iceberg_surface_noise: noise_or_zero("minecraft:iceberg_surface")?,
            noise_random,
            temperature_sampler: BiomeTemperatureSampler::new(),
        })
    }

    /// Returns the 192-layer badlands terracotta band array (`SurfaceSystem.clayBands`).
    pub const fn clay_bands(&self) -> &[u32; 192] {
        &self.clay_bands
    }

    /// Returns the terracotta block state ID at `(bx, by, bz)` (`SurfaceSystem.getBand`).
    #[inline]
    pub fn get_band(&self, bx: i32, by: i32, bz: i32) -> u32 {
        let offset = java_round(
            self.clay_bands_offset_noise
                .get_value(f64::from(bx), 0.0, f64::from(bz))
                * 4.0,
        );
        let len = self.clay_bands.len() as i32;
        let idx = (by + offset + len).rem_euclid(len) as usize;
        self.clay_bands[idx]
    }

    /// Computes `SurfaceSystem.getSurfaceDepth(blockX, blockZ)`.
    #[inline]
    pub fn get_surface_depth(&self, bx: i32, bz: i32) -> i32 {
        let noise_val = self
            .surface_noise
            .get_value(f64::from(bx), 0.0, f64::from(bz));
        let jitter = match &self.noise_random {
            PositionalRandom::Xoroshiro(f) => f.at(bx, 0, bz).next_double(),
            PositionalRandom::Legacy(f) => f.at(bx, 0, bz).next_double(),
        };
        (noise_val * 2.75 + 3.0 + jitter * 0.25) as i32
    }

    /// Evaluates `SurfaceSystem.getSurfaceSecondary(blockX, blockZ)`.
    #[inline]
    pub fn get_surface_secondary(&self, bx: i32, bz: i32) -> f64 {
        self.surface_secondary_noise
            .get_value(f64::from(bx), 0.0, f64::from(bz))
    }

    /// Evaluates the surface rule for dirt exposed by a carver at `position`.
    #[allow(clippy::too_many_arguments)]
    pub fn top_material<C: SurfaceChunkAccess>(
        &self,
        chunk: &C,
        biome_manager: &BiomeManager,
        biome_source: Option<&MultiNoiseBiomeSource>,
        noise_chunk: &mut NoiseChunk<'_>,
        biomes: &IndexMap<String, BiomeDef>,
        rule: &SurfaceRule,
        position: [i32; 3],
        above_has_fluid: bool,
    ) -> Option<u32> {
        let [bx, by, bz] = position;
        let mut context = SurfaceContext {
            system: self,
            chunk,
            biome_manager,
            biome_source,
            noise_chunk,
            biomes,
            min_y: chunk.min_y(),
            height: chunk.height(),
            bx,
            by,
            bz,
            local_x: (bx & 15) as usize,
            local_z: (bz & 15) as usize,
            surface_depth: self.get_surface_depth(bx, bz),
            stone_depth_above: 1,
            stone_depth_below: 1,
            water_height: if above_has_fluid { by + 1 } else { i32::MIN },
            cached_surface_secondary: None,
            cached_min_surface_level: None,
            cached_steep: None,
            cached_biome_id: None,
        };
        rule.try_apply(&mut context)
    }

    /// Applies eroded badlands pillars, frozen ocean icebergs, and `rule` across all 16×16 columns of `chunk`
    /// (`SurfaceSystem.buildSurface`).
    #[allow(clippy::too_many_arguments)]
    pub fn build_surface<C: SurfaceChunkAccess>(
        &self,
        chunk: &mut C,
        chunk_x: i32,
        chunk_z: i32,
        biome_manager: &BiomeManager,
        biome_source: Option<&MultiNoiseBiomeSource>,
        noise_chunk: &mut NoiseChunk<'_>,
        biomes: &IndexMap<String, BiomeDef>,
        rule: &SurfaceRule,
    ) {
        let first_block_x = chunk_x * 16;
        let first_block_z = chunk_z * 16;
        let min_y = chunk.min_y();
        let height = chunk.height();
        let max_y_exclusive = min_y + height;

        for local_x in 0..16_usize {
            for local_z in 0..16_usize {
                let bx = first_block_x + (local_x as i32);
                let bz = first_block_z + (local_z as i32);
                let mut top_y = chunk.world_surface_wg(local_x, local_z) - 1;

                let surface_biome_id = Self::lookup_biome_at(
                    chunk,
                    biome_manager,
                    biome_source,
                    noise_chunk,
                    bx,
                    (top_y + 1).max(min_y),
                    bz,
                );

                if Some(surface_biome_id) == self.eroded_badlands_id {
                    self.eroded_badlands_extension(chunk, local_x, local_z, bx, bz, top_y, min_y);
                    top_y = chunk.world_surface_wg(local_x, local_z) - 1;
                }

                let surface_depth = self.get_surface_depth(bx, bz);
                let mut stone_depth_above = 0_i32;
                let mut water_height = i32::MIN;
                let mut next_ceiling_check_y = i32::MAX;

                for by in (min_y..=top_y.min(max_y_exclusive - 1)).rev() {
                    let state = chunk.block_state(local_x, by, local_z);
                    if self.is_air_state(state) {
                        stone_depth_above = 0;
                        water_height = i32::MIN;
                        continue;
                    }
                    if self.is_fluid_state(state) {
                        if water_height == i32::MIN {
                            water_height = by + 1;
                        }
                        continue;
                    }

                    if next_ceiling_check_y >= by {
                        next_ceiling_check_y = i32::MIN / 2;
                        for search_y in (min_y - 1..by).rev() {
                            if search_y < min_y {
                                break;
                            }
                            let below = chunk.block_state(local_x, search_y, local_z);
                            if !self.is_stone_state(below) {
                                next_ceiling_check_y = search_y;
                                break;
                            }
                        }
                    }

                    stone_depth_above += 1;
                    let stone_depth_below = by - next_ceiling_check_y;

                    if state == self.default_block {
                        let mut ctx = SurfaceContext {
                            system: self,
                            chunk,
                            biome_manager,
                            biome_source,
                            noise_chunk,
                            biomes,
                            min_y,
                            height,
                            bx,
                            by,
                            bz,
                            local_x,
                            local_z,
                            surface_depth,
                            stone_depth_above,
                            stone_depth_below,
                            water_height,
                            cached_surface_secondary: None,
                            cached_min_surface_level: None,
                            cached_steep: None,
                            cached_biome_id: None,
                        };
                        if let Some(new_state) = rule.try_apply(&mut ctx) {
                            chunk.set_block_state(local_x, by, local_z, new_state);
                            if self.is_fluid_state(new_state) {
                                chunk.mark_fluid_postprocessing(local_x, by, local_z);
                            }
                        }
                    }
                }

                if Some(surface_biome_id) == self.frozen_ocean_id
                    || Some(surface_biome_id) == self.deep_frozen_ocean_id
                {
                    let biome_temp = biomes
                        .get_index(surface_biome_id as usize)
                        .map(|(_, b)| {
                            self.temperature_sampler.height_adjusted_temperature(
                                b,
                                bx,
                                self.sea_level,
                                bz,
                                self.sea_level,
                            )
                        })
                        .unwrap_or(0.0);
                    let min_surface_level = min_surface_level(noise_chunk, bx, bz, surface_depth);
                    self.frozen_ocean_extension(
                        chunk,
                        local_x,
                        local_z,
                        bx,
                        bz,
                        biome_temp,
                        min_surface_level,
                        top_y + 1,
                    );
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn eroded_badlands_extension<C: SurfaceChunkAccess>(
        &self,
        chunk: &mut C,
        local_x: usize,
        local_z: usize,
        bx: i32,
        bz: i32,
        surface_y: i32,
        min_y: i32,
    ) {
        let pillar =
            self.badlands_pillar_noise
                .get_value(f64::from(bx) * 0.2, 0.0, f64::from(bz) * 0.2)
                * 15.0;
        let surface = (self
            .badlands_surface_noise
            .get_value(f64::from(bx), 0.0, f64::from(bz))
            * 8.25)
            .abs();
        let threshold = pillar.min(surface);
        if threshold <= 0.0 {
            return;
        }
        let roof = (self.badlands_pillar_roof_noise.get_value(
            f64::from(bx) * 0.75,
            0.0,
            f64::from(bz) * 0.75,
        ) * 1.5)
            .abs();
        let top_y =
            (64.0 + (threshold * threshold * 2.5).min((roof * 50.0).ceil() + 24.0)).floor() as i32;
        if surface_y > top_y {
            return;
        }
        let max_valid_y = (min_y + chunk.height() - 1).min(top_y);
        for y in (min_y..=max_valid_y).rev() {
            let state = chunk.block_state(local_x, y, local_z);
            if state == self.default_block {
                break;
            }
            if state == self.water_state {
                return;
            }
        }
        for y in (min_y..=max_valid_y).rev() {
            let state = chunk.block_state(local_x, y, local_z);
            if !self.is_air_state(state) {
                break;
            }
            chunk.set_block_state(local_x, y, local_z, self.default_block);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn frozen_ocean_extension<C: SurfaceChunkAccess>(
        &self,
        chunk: &mut C,
        local_x: usize,
        local_z: usize,
        bx: i32,
        bz: i32,
        biome_temp: f32,
        min_surface_level: i32,
        start_height: i32,
    ) {
        let pillar =
            self.iceberg_pillar_noise
                .get_value(f64::from(bx) * 1.28, 0.0, f64::from(bz) * 1.28)
                * 15.0;
        let surface = (self
            .iceberg_surface_noise
            .get_value(f64::from(bx), 0.0, f64::from(bz))
            * 8.25)
            .abs();
        let threshold = pillar.min(surface);
        if threshold <= 1.8 {
            return;
        }
        let roof = (self.iceberg_pillar_roof_noise.get_value(
            f64::from(bx) * 1.17,
            0.0,
            f64::from(bz) * 1.17,
        ) * 1.5)
            .abs();
        let mut top = threshold * threshold * 1.2;
        let roof_cap = (roof * 40.0).ceil() + 14.0;
        if top > roof_cap {
            top = roof_cap;
        }
        if biome_temp > 0.1_f32 {
            top -= 2.0;
        }
        let (bottom_y, top_height) = if top > 2.0 {
            let b = f64::from(self.sea_level) - top - 7.0;
            (b as i32, top + f64::from(self.sea_level))
        } else {
            (0, 0.0)
        };
        let max_y = start_height.max(top_height as i32 + 1);

        match &self.noise_random {
            PositionalRandom::Xoroshiro(f) => {
                let mut rng = f.at(bx, 0, bz);
                let max_snow_depth = 2 + rng.next_int_bounded(4);
                let snow_start = self.sea_level + 18 + rng.next_int_bounded(10);
                let mut snow_depth = 0;
                self.apply_iceberg_column(
                    chunk,
                    &mut rng,
                    local_x,
                    local_z,
                    min_surface_level,
                    max_y,
                    bottom_y,
                    top_height,
                    max_snow_depth,
                    snow_start,
                    &mut snow_depth,
                );
            }
            PositionalRandom::Legacy(f) => {
                let mut rng = f.at(bx, 0, bz);
                let max_snow_depth = 2 + rng.next_int_bounded(4);
                let snow_start = self.sea_level + 18 + rng.next_int_bounded(10);
                let mut snow_depth = 0;
                self.apply_iceberg_column(
                    chunk,
                    &mut rng,
                    local_x,
                    local_z,
                    min_surface_level,
                    max_y,
                    bottom_y,
                    top_height,
                    max_snow_depth,
                    snow_start,
                    &mut snow_depth,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_iceberg_column<C: SurfaceChunkAccess>(
        &self,
        chunk: &mut C,
        rng: &mut impl RandomSource,
        local_x: usize,
        local_z: usize,
        min_surface_level: i32,
        max_y: i32,
        bottom_y: i32,
        top_height: f64,
        max_snow_depth: i32,
        snow_start: i32,
        snow_depth: &mut i32,
    ) {
        for y in (min_surface_level..=max_y).rev() {
            let state = chunk.block_state(local_x, y, local_z);
            let is_air_above =
                self.is_air_state(state) && y < (top_height as i32) && rng.next_double() > 0.01;
            let is_water_below = state == self.water_state
                && y > bottom_y
                && y < self.sea_level
                && bottom_y != 0
                && rng.next_double() > 0.15;
            if is_air_above || is_water_below {
                if *snow_depth <= max_snow_depth && y > snow_start {
                    chunk.set_block_state(local_x, y, local_z, self.snow_block_state);
                    *snow_depth += 1;
                } else {
                    chunk.set_block_state(local_x, y, local_z, self.packed_ice_state);
                }
            }
        }
    }

    #[inline]
    fn is_air_state(&self, state: u32) -> bool {
        state == self.air_state || block_states().is_air(state)
    }

    #[inline]
    fn is_fluid_state(&self, state: u32) -> bool {
        state == self.water_state || state == self.lava_state
    }

    #[inline]
    fn is_stone_state(&self, state: u32) -> bool {
        !self.is_air_state(state) && !self.is_fluid_state(state)
    }

    #[allow(clippy::too_many_arguments)]
    fn lookup_biome_at<C: SurfaceChunkAccess>(
        chunk: &C,
        biome_manager: &BiomeManager,
        biome_source: Option<&MultiNoiseBiomeSource>,
        noise_chunk: &mut NoiseChunk<'_>,
        bx: i32,
        by: i32,
        bz: i32,
    ) -> u32 {
        let chunk_q_min_x = noise_chunk.chunk_x() << 2;
        let chunk_q_min_z = noise_chunk.chunk_z() << 2;
        let min_q_y = chunk.min_y() >> 2;
        let max_q_y = (chunk.min_y() + chunk.height() - 1) >> 2;

        biome_manager.get_biome(bx, by, bz, |qx, qy, qz| {
            let cy = qy.clamp(min_q_y, max_q_y);
            if qx >= chunk_q_min_x
                && qx < chunk_q_min_x + 4
                && qz >= chunk_q_min_z
                && qz < chunk_q_min_z + 4
            {
                chunk.get_noise_biome(qx, cy, qz)
            } else if let Some(source) = biome_source {
                source.neighbor_biome(noise_chunk, qx, cy, qz, min_q_y, max_q_y)
            } else {
                chunk.get_noise_biome(
                    qx.clamp(chunk_q_min_x, chunk_q_min_x + 3),
                    cy,
                    qz.clamp(chunk_q_min_z, chunk_q_min_z + 3),
                )
            }
        })
    }
}

/// The lowest Y that surface rules and icebergs reach in a column (`SurfaceRules.Context.getMinSurfaceLevel`).
fn min_surface_level(
    noise_chunk: &mut NoiseChunk<'_>,
    bx: i32,
    bz: i32,
    surface_depth: i32,
) -> i32 {
    let cell_x = bx >> 4;
    let cell_z = bz >> 4;
    let d00 = f64::from(noise_chunk.preliminary_surface_level(cell_x << 4, cell_z << 4));
    let d10 = f64::from(noise_chunk.preliminary_surface_level((cell_x + 1) << 4, cell_z << 4));
    let d01 = f64::from(noise_chunk.preliminary_surface_level(cell_x << 4, (cell_z + 1) << 4));
    let d11 =
        f64::from(noise_chunk.preliminary_surface_level((cell_x + 1) << 4, (cell_z + 1) << 4));
    let fx = f64::from(bx & 15) / 16.0;
    let fz = f64::from(bz & 15) / 16.0;
    let top0 = d00 + fx * (d10 - d00);
    let top1 = d01 + fx * (d11 - d01);
    (top0 + fz * (top1 - top0)).floor() as i32 + surface_depth - 8
}

struct SurfaceContext<'a, 'b, C: SurfaceChunkAccess> {
    system: &'a SurfaceSystem,
    chunk: &'a C,
    biome_manager: &'a BiomeManager,
    biome_source: Option<&'a MultiNoiseBiomeSource>,
    noise_chunk: &'a mut NoiseChunk<'b>,
    biomes: &'a IndexMap<String, BiomeDef>,
    min_y: i32,
    height: i32,
    bx: i32,
    by: i32,
    bz: i32,
    local_x: usize,
    local_z: usize,
    surface_depth: i32,
    stone_depth_above: i32,
    stone_depth_below: i32,
    water_height: i32,
    cached_surface_secondary: Option<f64>,
    cached_min_surface_level: Option<i32>,
    cached_steep: Option<bool>,
    cached_biome_id: Option<u32>,
}

impl<C: SurfaceChunkAccess> SurfaceContext<'_, '_, C> {
    fn surface_secondary(&mut self) -> f64 {
        if let Some(v) = self.cached_surface_secondary {
            return v;
        }
        let v = self.system.get_surface_secondary(self.bx, self.bz);
        self.cached_surface_secondary = Some(v);
        v
    }

    fn min_surface_level(&mut self) -> i32 {
        if let Some(v) = self.cached_min_surface_level {
            return v;
        }
        let level = min_surface_level(self.noise_chunk, self.bx, self.bz, self.surface_depth);
        self.cached_min_surface_level = Some(level);
        level
    }

    fn is_steep(&mut self) -> bool {
        if let Some(v) = self.cached_steep {
            return v;
        }
        let z_north = self.local_z.saturating_sub(1);
        let z_south = (self.local_z + 1).min(15);
        let h_north = self.chunk.world_surface_wg(self.local_x, z_north);
        let h_south = self.chunk.world_surface_wg(self.local_x, z_south);
        if h_south >= h_north + 4 {
            self.cached_steep = Some(true);
            return true;
        }
        let x_west = self.local_x.saturating_sub(1);
        let x_east = (self.local_x + 1).min(15);
        let h_west = self.chunk.world_surface_wg(x_west, self.local_z);
        let h_east = self.chunk.world_surface_wg(x_east, self.local_z);
        let steep = h_west >= h_east + 4;
        self.cached_steep = Some(steep);
        steep
    }

    fn biome_id(&mut self) -> u32 {
        if let Some(id) = self.cached_biome_id {
            return id;
        }
        let id = SurfaceSystem::lookup_biome_at(
            self.chunk,
            self.biome_manager,
            self.biome_source,
            self.noise_chunk,
            self.bx,
            self.by,
            self.bz,
        );
        self.cached_biome_id = Some(id);
        id
    }

    fn cold_enough_to_snow(&mut self) -> bool {
        let id = self.biome_id();
        let Some((_, biome)) = self.biomes.get_index(id as usize) else {
            return false;
        };
        self.system.temperature_sampler.cold_enough_to_snow(
            biome,
            self.bx,
            self.by,
            self.bz,
            self.system.sea_level,
        )
    }
}

impl SurfaceCondition {
    fn test<C: SurfaceChunkAccess>(&self, ctx: &mut SurfaceContext<'_, '_, C>) -> bool {
        match self {
            Self::Biome(allowed) => {
                let id = ctx.biome_id();
                allowed.contains(&id)
            }
            Self::NoiseThreshold {
                noise,
                min_threshold,
                max_threshold,
            } => {
                let val = noise.get_value(f64::from(ctx.bx), 0.0, f64::from(ctx.bz));
                val >= *min_threshold && val <= *max_threshold
            }
            Self::VerticalGradient {
                random,
                true_at_and_below,
                false_at_and_above,
            } => {
                let true_y = true_at_and_below.resolve(ctx.min_y, ctx.height);
                let false_y = false_at_and_above.resolve(ctx.min_y, ctx.height);
                if ctx.by <= true_y {
                    return true;
                }
                if ctx.by >= false_y {
                    return false;
                }
                let prob = map(
                    f64::from(ctx.by),
                    f64::from(true_y),
                    f64::from(false_y),
                    1.0,
                    0.0,
                );
                let roll = match random {
                    PositionalRandom::Xoroshiro(f) => f.at(ctx.bx, ctx.by, ctx.bz).next_float(),
                    PositionalRandom::Legacy(f) => f.at(ctx.bx, ctx.by, ctx.bz).next_float(),
                };
                f64::from(roll) < prob
            }
            Self::YAbove {
                anchor,
                surface_depth_multiplier,
                add_stone_depth,
            } => {
                let depth = if *add_stone_depth {
                    ctx.stone_depth_above
                } else {
                    0
                };
                ctx.by + depth
                    >= anchor.resolve(ctx.min_y, ctx.height)
                        + ctx.surface_depth * (*surface_depth_multiplier)
            }
            Self::Water {
                offset,
                surface_depth_multiplier,
                add_stone_depth,
            } => {
                if ctx.water_height == i32::MIN {
                    return true;
                }
                let depth = if *add_stone_depth {
                    ctx.stone_depth_above
                } else {
                    0
                };
                ctx.by + depth
                    >= ctx.water_height
                        + (*offset)
                        + ctx.surface_depth * (*surface_depth_multiplier)
            }
            Self::Temperature => ctx.cold_enough_to_snow(),
            Self::Steep => ctx.is_steep(),
            Self::Not(inner) => !inner.test(ctx),
            Self::Hole => ctx.surface_depth <= 0,
            Self::AbovePreliminarySurface => ctx.by >= ctx.min_surface_level(),
            Self::StoneDepth {
                offset,
                add_surface_depth,
                secondary_depth_range,
                surface_type,
            } => {
                let depth = match surface_type {
                    CaveSurface::Ceiling => ctx.stone_depth_below,
                    CaveSurface::Floor => ctx.stone_depth_above,
                };
                let surf = if *add_surface_depth {
                    ctx.surface_depth
                } else {
                    0
                };
                let sec = if *secondary_depth_range == 0 {
                    0
                } else {
                    map(
                        ctx.surface_secondary(),
                        -1.0,
                        1.0,
                        0.0,
                        f64::from(*secondary_depth_range),
                    ) as i32
                };
                depth <= 1 + (*offset) + surf + sec
            }
        }
    }
}

impl SurfaceRule {
    fn try_apply<C: SurfaceChunkAccess>(&self, ctx: &mut SurfaceContext<'_, '_, C>) -> Option<u32> {
        match self {
            Self::Block(state) => Some(*state),
            Self::Sequence(rules) => {
                for rule in rules {
                    if let Some(state) = rule.try_apply(ctx) {
                        return Some(state);
                    }
                }
                None
            }
            Self::Condition { if_true, then_run } => {
                if if_true.test(ctx) {
                    then_run.try_apply(ctx)
                } else {
                    None
                }
            }
            Self::Bandlands => Some(ctx.system.get_band(ctx.bx, ctx.by, ctx.bz)),
        }
    }

    /// Compiles a `SurfaceRule` from a `serde_json::Value` (`NoiseSettingsDef::surface_rule`).
    pub fn compile(
        json: &Value,
        graph: &CompiledDensityGraph,
        data: &WorldgenData,
    ) -> Result<Self, WorldgenDataError> {
        compile_rule("surface_rule", json, graph, data)
    }
}

/// Resolves a [`BlockStateDef`] to its `u32` block state ID in `harpas_core::block::block_states()`.
pub fn resolve_block_state(def: &BlockStateDef, ctx: &str) -> Result<u32, WorldgenDataError> {
    let states = block_states();
    let props = def.properties.iter().map(|(k, v)| (k.as_str(), v.as_str()));
    let (id, _ignored) =
        states
            .state(&def.name, props)
            .ok_or_else(|| WorldgenDataError::InvalidDefinition {
                context: ctx.to_string(),
                message: format!("unknown block state `{}`", def.name),
            })?;
    Ok(id)
}

fn compile_rule(
    ctx: &str,
    json: &Value,
    graph: &CompiledDensityGraph,
    data: &WorldgenData,
) -> Result<SurfaceRule, WorldgenDataError> {
    let obj = json
        .as_object()
        .ok_or_else(|| invalid(ctx, "surface rule must be a JSON object"))?;
    let ty = obj
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(ctx, "surface rule missing `type`"))?;

    match ty {
        "minecraft:block" => {
            let state_val = obj
                .get("result_state")
                .ok_or_else(|| invalid(ctx, "`minecraft:block` missing `result_state`"))?;
            let state_def = parse_block_state_json(ctx, state_val)?;
            let state_id = resolve_block_state(&state_def, ctx)?;
            Ok(SurfaceRule::Block(state_id))
        }
        "minecraft:sequence" => {
            let seq_arr = obj
                .get("sequence")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid(ctx, "`minecraft:sequence` missing `sequence` array"))?;
            let mut rules = Vec::with_capacity(seq_arr.len());
            for item in seq_arr {
                rules.push(compile_rule(ctx, item, graph, data)?);
            }
            Ok(SurfaceRule::Sequence(rules))
        }
        "minecraft:condition" => {
            let cond_val = obj
                .get("if_true")
                .ok_or_else(|| invalid(ctx, "`minecraft:condition` missing `if_true`"))?;
            let then_val = obj
                .get("then_run")
                .ok_or_else(|| invalid(ctx, "`minecraft:condition` missing `then_run`"))?;
            let if_true = compile_condition(ctx, cond_val, graph, data)?;
            let then_run = Box::new(compile_rule(ctx, then_val, graph, data)?);
            Ok(SurfaceRule::Condition { if_true, then_run })
        }
        "minecraft:bandlands" => Ok(SurfaceRule::Bandlands),
        other => Err(invalid(
            ctx,
            format!("unsupported surface rule type `{other}`"),
        )),
    }
}

fn compile_condition(
    ctx: &str,
    json: &Value,
    graph: &CompiledDensityGraph,
    data: &WorldgenData,
) -> Result<SurfaceCondition, WorldgenDataError> {
    let obj = json
        .as_object()
        .ok_or_else(|| invalid(ctx, "surface condition must be a JSON object"))?;
    let ty = obj
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(ctx, "surface condition missing `type`"))?;

    match ty {
        "minecraft:biome" => {
            let arr = obj
                .get("biome_is")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid(ctx, "`minecraft:biome` missing `biome_is` array"))?;
            let mut ids = Vec::with_capacity(arr.len());
            for item in arr {
                let name = item
                    .as_str()
                    .ok_or_else(|| invalid(ctx, "`biome_is` entry must be a string"))?;
                if let Some(id) = data.biome_id(name) {
                    ids.push(id);
                }
            }
            Ok(SurfaceCondition::Biome(ids))
        }
        "minecraft:noise_threshold" => {
            let noise_name = obj
                .get("noise")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid(ctx, "`minecraft:noise_threshold` missing `noise`"))?;
            let min_threshold = obj
                .get("min_threshold")
                .and_then(Value::as_f64)
                .ok_or_else(|| {
                    invalid(ctx, "`minecraft:noise_threshold` missing `min_threshold`")
                })?;
            let max_threshold = obj
                .get("max_threshold")
                .and_then(Value::as_f64)
                .ok_or_else(|| {
                    invalid(ctx, "`minecraft:noise_threshold` missing `max_threshold`")
                })?;
            let noise = Box::new(graph.instantiate_noise(noise_name, &data.noises)?);
            Ok(SurfaceCondition::NoiseThreshold {
                noise,
                min_threshold,
                max_threshold,
            })
        }
        "minecraft:vertical_gradient" => {
            let random_name = obj
                .get("random_name")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    invalid(ctx, "`minecraft:vertical_gradient` missing `random_name`")
                })?;
            let true_at = parse_vertical_anchor(
                ctx,
                obj.get("true_at_and_below")
                    .ok_or_else(|| invalid(ctx, "missing `true_at_and_below`"))?,
            )?;
            let false_at = parse_vertical_anchor(
                ctx,
                obj.get("false_at_and_above")
                    .ok_or_else(|| invalid(ctx, "missing `false_at_and_above`"))?,
            )?;
            let random = graph
                .positional_random()
                .fork_positional_from_hash_of(random_name);
            Ok(SurfaceCondition::VerticalGradient {
                random,
                true_at_and_below: true_at,
                false_at_and_above: false_at,
            })
        }
        "minecraft:y_above" => {
            let anchor = parse_vertical_anchor(
                ctx,
                obj.get("anchor")
                    .ok_or_else(|| invalid(ctx, "`minecraft:y_above` missing `anchor`"))?,
            )?;
            let surface_depth_multiplier = obj
                .get("surface_depth_multiplier")
                .and_then(Value::as_i64)
                .ok_or_else(|| invalid(ctx, "missing `surface_depth_multiplier`"))?
                as i32;
            let add_stone_depth = obj
                .get("add_stone_depth")
                .and_then(Value::as_bool)
                .ok_or_else(|| invalid(ctx, "missing `add_stone_depth`"))?;
            Ok(SurfaceCondition::YAbove {
                anchor,
                surface_depth_multiplier,
                add_stone_depth,
            })
        }
        "minecraft:water" => {
            let offset = obj
                .get("offset")
                .and_then(Value::as_i64)
                .ok_or_else(|| invalid(ctx, "`minecraft:water` missing `offset`"))?
                as i32;
            let surface_depth_multiplier = obj
                .get("surface_depth_multiplier")
                .and_then(Value::as_i64)
                .ok_or_else(|| invalid(ctx, "missing `surface_depth_multiplier`"))?
                as i32;
            let add_stone_depth = obj
                .get("add_stone_depth")
                .and_then(Value::as_bool)
                .ok_or_else(|| invalid(ctx, "missing `add_stone_depth`"))?;
            Ok(SurfaceCondition::Water {
                offset,
                surface_depth_multiplier,
                add_stone_depth,
            })
        }
        "minecraft:temperature" => Ok(SurfaceCondition::Temperature),
        "minecraft:steep" => Ok(SurfaceCondition::Steep),
        "minecraft:not" => {
            let inv = obj
                .get("invert")
                .ok_or_else(|| invalid(ctx, "`minecraft:not` missing `invert`"))?;
            Ok(SurfaceCondition::Not(Box::new(compile_condition(
                ctx, inv, graph, data,
            )?)))
        }
        "minecraft:hole" => Ok(SurfaceCondition::Hole),
        "minecraft:above_preliminary_surface" => Ok(SurfaceCondition::AbovePreliminarySurface),
        "minecraft:stone_depth" => {
            let offset = obj
                .get("offset")
                .and_then(Value::as_i64)
                .ok_or_else(|| invalid(ctx, "`minecraft:stone_depth` missing `offset`"))?
                as i32;
            let add_surface_depth = obj
                .get("add_surface_depth")
                .and_then(Value::as_bool)
                .ok_or_else(|| invalid(ctx, "missing `add_surface_depth`"))?;
            let secondary_depth_range = obj
                .get("secondary_depth_range")
                .and_then(Value::as_i64)
                .ok_or_else(|| invalid(ctx, "missing `secondary_depth_range`"))?
                as i32;
            let surface_type = match obj.get("surface_type").and_then(Value::as_str) {
                Some("floor") => CaveSurface::Floor,
                Some("ceiling") => CaveSurface::Ceiling,
                other => {
                    return Err(invalid(ctx, format!("invalid `surface_type`: {other:?}")));
                }
            };
            Ok(SurfaceCondition::StoneDepth {
                offset,
                add_surface_depth,
                secondary_depth_range,
                surface_type,
            })
        }
        other => Err(invalid(
            ctx,
            format!("unsupported surface condition type `{other}`"),
        )),
    }
}

pub(crate) fn parse_vertical_anchor(
    ctx: &str,
    json: &Value,
) -> Result<VerticalAnchor, WorldgenDataError> {
    let obj = json
        .as_object()
        .ok_or_else(|| invalid(ctx, "vertical anchor must be a JSON object"))?;
    if let Some(v) = obj.get("absolute").and_then(Value::as_i64) {
        return Ok(VerticalAnchor::Absolute(v as i32));
    }
    if let Some(v) = obj.get("above_bottom").and_then(Value::as_i64) {
        return Ok(VerticalAnchor::AboveBottom(v as i32));
    }
    if let Some(v) = obj.get("below_top").and_then(Value::as_i64) {
        return Ok(VerticalAnchor::BelowTop(v as i32));
    }
    Err(invalid(
        ctx,
        "vertical anchor must contain `absolute`, `above_bottom`, or `below_top`",
    ))
}

fn parse_block_state_json(ctx: &str, json: &Value) -> Result<BlockStateDef, WorldgenDataError> {
    let obj = json
        .as_object()
        .ok_or_else(|| invalid(ctx, "block state must be an object"))?;
    let name = obj
        .get("Name")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(ctx, "block state missing `Name`"))?
        .to_string();
    let mut properties = std::collections::BTreeMap::new();
    if let Some(props_obj) = obj.get("Properties").and_then(Value::as_object) {
        for (k, v) in props_obj {
            if let Some(s) = v.as_str() {
                properties.insert(k.clone(), s.to_string());
            }
        }
    }
    Ok(BlockStateDef { name, properties })
}

fn invalid(ctx: &str, message: impl Into<String>) -> WorldgenDataError {
    WorldgenDataError::InvalidDefinition {
        context: ctx.to_string(),
        message: message.into(),
    }
}

/// Java's `Math.round(double)`, converted to a 32-bit band offset.
#[inline]
pub fn java_round(val: f64) -> i32 {
    harpas_core::math::round_f64(val) as i32
}

/// Generates the 192-element terracotta band array (`SurfaceSystem.generateBands`).
pub fn generate_bands(rng: &mut dyn RandomSource, states: &BlockStates) -> [u32; 192] {
    let terracotta = states
        .default_state("minecraft:terracotta")
        .expect("terracotta");
    let orange = states
        .default_state("minecraft:orange_terracotta")
        .expect("orange_terracotta");
    let yellow = states
        .default_state("minecraft:yellow_terracotta")
        .expect("yellow_terracotta");
    let brown = states
        .default_state("minecraft:brown_terracotta")
        .expect("brown_terracotta");
    let red = states
        .default_state("minecraft:red_terracotta")
        .expect("red_terracotta");
    let white = states
        .default_state("minecraft:white_terracotta")
        .expect("white_terracotta");
    let light_gray = states
        .default_state("minecraft:light_gray_terracotta")
        .expect("light_gray_terracotta");

    let mut bands = [terracotta; 192];
    let mut i = 0_usize;
    while i < bands.len() {
        i += (rng.next_int_bounded(5) as usize) + 1;
        if i < bands.len() {
            bands[i] = orange;
        }
        i += 1;
    }

    make_bands(rng, &mut bands, 1, yellow);
    make_bands(rng, &mut bands, 2, brown);
    make_bands(rng, &mut bands, 1, red);

    let white_count = rng.next_int_between_inclusive(9, 15);
    let mut count = 0;
    let mut pos = 0_usize;
    while count < white_count && pos < bands.len() {
        bands[pos] = white;
        if pos > 1 && rng.next_bool() {
            bands[pos - 1] = light_gray;
        }
        if pos + 1 < bands.len() && rng.next_bool() {
            bands[pos + 1] = light_gray;
        }
        count += 1;
        pos += (rng.next_int_bounded(16) as usize) + 4;
    }

    bands
}

fn make_bands(rng: &mut dyn RandomSource, bands: &mut [u32; 192], min_width: i32, state: u32) {
    let band_count = rng.next_int_between_inclusive(6, 15);
    for _ in 0..band_count {
        let width = min_width + rng.next_int_bounded(3);
        let start = rng.next_int_bounded(bands.len() as i32);
        let mut p = 0;
        while start + p < (bands.len() as i32) && p < width {
            bands[(start + p) as usize] = state;
            p += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::java_round;

    #[test]
    fn band_offset_uses_java_rounding_just_below_a_half() {
        assert_eq!(java_round(f64::from_bits(0x3fdf_ffff_ffff_ffff)), 0);
        assert_eq!(java_round(0.5), 1);
        assert_eq!(java_round(-0.5), 0);
    }
}
