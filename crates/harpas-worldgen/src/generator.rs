//! `NoiseBasedChunkGenerator`: orchestrates `NoiseChunk`, `MultiNoiseBiomeSource`, `Aquifer`, `OreVeinifier`,
//! `SurfaceSystem`, and chunk heightmaps (`OCEAN_FLOOR_WG`, `WORLD_SURFACE_WG`, `MOTION_BLOCKING`, etc.)
//!
//! Generates chunk columns through the `biomes`, `noise`, and `surface` worldgen stages
//! (`docs/world.md#world-generation`).

use std::sync::OnceLock;

use harpas_core::block::block_states;
use harpas_core::chunk::{PalettedContainer, Section, Strategy};

use crate::aquifer::{Aquifer, GlobalFluidPicker};
use crate::biome::{BiomeManager, MultiNoiseBiomeSource};
use crate::data::{
    MultiNoiseParameterListDef, NoiseSamplingSettings, WorldgenData, WorldgenDataError,
};
use crate::density::{CompiledDensityGraph, CompiledNoiseRouter};
use crate::noise_chunk::NoiseChunk;
use crate::ore_vein::OreVeinifier;
use crate::surface::{SurfaceChunkAccess, SurfaceRule, SurfaceSystem, resolve_block_state};

/// Vanilla heightmap types tracked during chunk generation and sent in chunk packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HeightmapType {
    /// Highest non-air block during world generation (`WORLD_SURFACE_WG`).
    WorldSurfaceWg,
    /// Highest non-air block (`WORLD_SURFACE`).
    WorldSurface,
    /// Highest motion-blocking solid block during world generation (`OCEAN_FLOOR_WG`).
    OceanFloorWg,
    /// Highest motion-blocking solid block (`OCEAN_FLOOR`).
    OceanFloor,
    /// Highest motion-blocking or fluid-containing block (`MOTION_BLOCKING`).
    MotionBlocking,
    /// Highest motion-blocking or fluid-containing block excluding leaves (`MOTION_BLOCKING_NO_LEAVES`).
    MotionBlockingNoLeaves,
}

impl HeightmapType {
    /// All 6 heightmap types in canonical NBT order.
    pub const ALL: [Self; 6] = [
        Self::WorldSurfaceWg,
        Self::WorldSurface,
        Self::OceanFloorWg,
        Self::OceanFloor,
        Self::MotionBlocking,
        Self::MotionBlockingNoLeaves,
    ];

    /// NBT compound key name (`"WORLD_SURFACE_WG"`, `"MOTION_BLOCKING"`, etc.).
    pub const fn nbt_key(self) -> &'static str {
        match self {
            Self::WorldSurfaceWg => "WORLD_SURFACE_WG",
            Self::WorldSurface => "WORLD_SURFACE",
            Self::OceanFloorWg => "OCEAN_FLOOR_WG",
            Self::OceanFloor => "OCEAN_FLOOR",
            Self::MotionBlocking => "MOTION_BLOCKING",
            Self::MotionBlockingNoLeaves => "MOTION_BLOCKING_NO_LEAVES",
        }
    }

    /// Returns whether `state` satisfies this heightmap's predicate for the blocks used during terrain and
    /// surface generation. Classification of other blocks is provisional until gameplay heightmaps are added.
    #[inline]
    pub fn matches(self, state: u32) -> bool {
        let flags = block_heightmap_flags()[state as usize];
        match self {
            Self::WorldSurfaceWg | Self::WorldSurface => (flags & FLAG_NOT_AIR) != 0,
            Self::OceanFloorWg | Self::OceanFloor => (flags & FLAG_BLOCKS_MOTION) != 0,
            Self::MotionBlocking => (flags & (FLAG_BLOCKS_MOTION | FLAG_HAS_FLUID)) != 0,
            Self::MotionBlockingNoLeaves => {
                (flags & (FLAG_BLOCKS_MOTION | FLAG_HAS_FLUID)) != 0
                    && (flags & FLAG_IS_LEAVES) == 0
            }
        }
    }
}

const FLAG_NOT_AIR: u8 = 1 << 0;
const FLAG_HAS_FLUID: u8 = 1 << 1;
const FLAG_BLOCKS_MOTION: u8 = 1 << 2;
const FLAG_IS_LEAVES: u8 = 1 << 3;

fn block_heightmap_flags() -> &'static [u8] {
    static FLAGS: OnceLock<Vec<u8>> = OnceLock::new();
    FLAGS.get_or_init(|| {
        let states = block_states();
        (0..states.len())
            .map(|id| {
                let (name, props) = states.properties(id).expect("known block state");
                let is_air = matches!(
                    name,
                    "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
                );
                if is_air {
                    return 0;
                }
                let has_fluid = matches!(
                    name,
                    "minecraft:water"
                        | "minecraft:lava"
                        | "minecraft:bubble_column"
                        | "minecraft:kelp"
                        | "minecraft:kelp_plant"
                        | "minecraft:seagrass"
                        | "minecraft:tall_seagrass"
                ) || props.contains(&("waterlogged", "true"));
                let is_leaves = name.ends_with("_leaves");
                let is_non_blocking = matches!(
                    name,
                    "minecraft:water"
                        | "minecraft:lava"
                        | "minecraft:bubble_column"
                        | "minecraft:fire"
                        | "minecraft:soul_fire"
                        | "minecraft:cobweb"
                        | "minecraft:short_grass"
                        | "minecraft:tall_grass"
                        | "minecraft:fern"
                        | "minecraft:large_fern"
                        | "minecraft:dead_bush"
                        | "minecraft:seagrass"
                        | "minecraft:tall_seagrass"
                        | "minecraft:kelp"
                        | "minecraft:kelp_plant"
                        | "minecraft:vine"
                        | "minecraft:glow_lichen"
                        | "minecraft:sugar_cane"
                        | "minecraft:brown_mushroom"
                        | "minecraft:red_mushroom"
                        | "minecraft:snow"
                        | "minecraft:powder_snow"
                );
                let mut f = FLAG_NOT_AIR;
                if has_fluid {
                    f |= FLAG_HAS_FLUID;
                }
                if !is_non_blocking {
                    f |= FLAG_BLOCKS_MOTION;
                }
                if is_leaves {
                    f |= FLAG_IS_LEAVES;
                }
                f
            })
            .collect()
    })
}

/// Per-chunk 16×16 heightmaps storing `first_available_y` (`highest_matching_y + 1`, or `min_y` when empty).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heightmaps {
    min_y: i32,
    height: i32,
    world_surface_wg: [i32; 256],
    world_surface: [i32; 256],
    ocean_floor_wg: [i32; 256],
    ocean_floor: [i32; 256],
    motion_blocking: [i32; 256],
    motion_blocking_no_leaves: [i32; 256],
}

impl Heightmaps {
    /// Creates an empty set of heightmaps initialized to `min_y`.
    pub const fn new(min_y: i32, height: i32) -> Self {
        Self {
            min_y,
            height,
            world_surface_wg: [min_y; 256],
            world_surface: [min_y; 256],
            ocean_floor_wg: [min_y; 256],
            ocean_floor: [min_y; 256],
            motion_blocking: [min_y; 256],
            motion_blocking_no_leaves: [min_y; 256],
        }
    }

    /// Returns `first_available_y` (`highest_matching_y + 1`, or `min_y` if empty) at `(local_x, local_z)`.
    #[inline]
    pub const fn get(&self, kind: HeightmapType, local_x: usize, local_z: usize) -> i32 {
        let idx = (local_z << 4) | local_x;
        match kind {
            HeightmapType::WorldSurfaceWg => self.world_surface_wg[idx],
            HeightmapType::WorldSurface => self.world_surface[idx],
            HeightmapType::OceanFloorWg => self.ocean_floor_wg[idx],
            HeightmapType::OceanFloor => self.ocean_floor[idx],
            HeightmapType::MotionBlocking => self.motion_blocking[idx],
            HeightmapType::MotionBlockingNoLeaves => self.motion_blocking_no_leaves[idx],
        }
    }

    /// Fast upward-or-top-down heightmap update when filling a chunk from top to bottom or raising a pillar.
    #[inline]
    pub fn record_filled(&mut self, local_x: usize, block_y: i32, local_z: usize, state: u32) {
        let flags = block_heightmap_flags()[state as usize];
        if flags == 0 {
            return;
        }
        let idx = (local_z << 4) | local_x;
        let top = block_y + 1;
        if (flags & FLAG_NOT_AIR) != 0 {
            if top > self.world_surface_wg[idx] {
                self.world_surface_wg[idx] = top;
            }
            if top > self.world_surface[idx] {
                self.world_surface[idx] = top;
            }
        }
        if (flags & FLAG_BLOCKS_MOTION) != 0 {
            if top > self.ocean_floor_wg[idx] {
                self.ocean_floor_wg[idx] = top;
            }
            if top > self.ocean_floor[idx] {
                self.ocean_floor[idx] = top;
            }
        }
        if (flags & (FLAG_BLOCKS_MOTION | FLAG_HAS_FLUID)) != 0 {
            if top > self.motion_blocking[idx] {
                self.motion_blocking[idx] = top;
            }
            if (flags & FLAG_IS_LEAVES) == 0 && top > self.motion_blocking_no_leaves[idx] {
                self.motion_blocking_no_leaves[idx] = top;
            }
        }
    }

    /// Packs `kind` into vanilla's `SimpleBitStorage` `LongArray` format (`37` `i64`s for `height = 384`).
    pub fn pack(&self, kind: HeightmapType) -> Vec<i64> {
        let values = match kind {
            HeightmapType::WorldSurfaceWg => &self.world_surface_wg,
            HeightmapType::WorldSurface => &self.world_surface,
            HeightmapType::OceanFloorWg => &self.ocean_floor_wg,
            HeightmapType::OceanFloor => &self.ocean_floor,
            HeightmapType::MotionBlocking => &self.motion_blocking,
            HeightmapType::MotionBlockingNoLeaves => &self.motion_blocking_no_leaves,
        };
        let range = (self.height + 1) as usize;
        let bits = if range <= 1 {
            1
        } else {
            (usize::BITS - (range - 1).leading_zeros()) as usize
        };
        let per_long = 64 / bits;
        let num_longs = 256_usize.div_ceil(per_long);
        let mask = (1_u64 << bits) - 1;
        let mut out = vec![0_i64; num_longs];
        for (i, &y) in values.iter().enumerate() {
            let offset = (y - self.min_y).clamp(0, self.height) as u64;
            let long_idx = i / per_long;
            let bit_shift = (i % per_long) * bits;
            let current = out[long_idx] as u64;
            out[long_idx] =
                ((current & !(mask << bit_shift)) | ((offset & mask) << bit_shift)) as i64;
        }
        out
    }
}

/// A generated chunk column holding its `Section`s and `Heightmaps`.
#[derive(Debug, Clone)]
pub struct GeneratedChunk {
    chunk_x: i32,
    chunk_z: i32,
    min_y: i32,
    height: i32,
    min_section_y: i32,
    sections: Vec<Section>,
    heightmaps: Heightmaps,
    fluid_postprocessing: Vec<(u8, i32, u8)>,
}

impl GeneratedChunk {
    /// Creates an air-filled chunk at `(chunk_x, chunk_z)` with `(min_y, height)` and `biome_count` registry size.
    pub fn new(chunk_x: i32, chunk_z: i32, min_y: i32, height: i32, biome_count: usize) -> Self {
        let min_section_y = min_y >> 4;
        let max_section_y = (min_y + height - 1) >> 4;
        let section_count = (max_section_y - min_section_y + 1) as usize;

        let states = block_states();
        let block_strategy = Strategy::block_states(states.global_bits());
        let biome_bits = (usize::BITS - biome_count.saturating_sub(1).leading_zeros()).max(1) as u8;
        let biome_strategy = Strategy::biomes(biome_bits);

        let sections = (0..section_count)
            .map(|_| {
                Section::new(
                    PalettedContainer::filled(block_strategy, 0),
                    PalettedContainer::filled(biome_strategy, 0),
                )
            })
            .collect();

        Self {
            chunk_x,
            chunk_z,
            min_y,
            height,
            min_section_y,
            sections,
            heightmaps: Heightmaps::new(min_y, height),
            fluid_postprocessing: Vec::new(),
        }
    }

    /// Chunk X coordinate.
    pub const fn chunk_x(&self) -> i32 {
        self.chunk_x
    }

    /// Chunk Z coordinate.
    pub const fn chunk_z(&self) -> i32 {
        self.chunk_z
    }

    /// Minimum Y coordinate (`-64` in Overworld).
    pub const fn min_y(&self) -> i32 {
        self.min_y
    }

    /// Total generation height in blocks (`384` in Overworld).
    pub const fn height(&self) -> i32 {
        self.height
    }

    /// Minimum section Y index (`-4` in Overworld).
    pub const fn min_section_y(&self) -> i32 {
        self.min_section_y
    }

    /// Immutable slice of the chunk's vertical `Section`s from `min_section_y` upward.
    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    /// Mutable slice of the chunk's vertical `Section`s from `min_section_y` upward.
    pub fn sections_mut(&mut self) -> &mut [Section] {
        &mut self.sections
    }

    /// Current heightmaps of this chunk.
    pub const fn heightmaps(&self) -> &Heightmaps {
        &self.heightmaps
    }

    /// Local X, absolute Y, and local Z of fluid blocks that require postprocessing after generation.
    pub fn fluid_postprocessing(&self) -> &[(u8, i32, u8)] {
        &self.fluid_postprocessing
    }

    /// Recomputes all 6 heightmaps by scanning each `(local_x, local_z)` column from top to bottom.
    pub fn recompute_heightmaps(&mut self) {
        let mut h = Heightmaps::new(self.min_y, self.height);
        let top_y = self.min_y + self.height - 1;
        for local_z in 0..16_usize {
            for local_x in 0..16_usize {
                for by in (self.min_y..=top_y).rev() {
                    let state = self.block_state(local_x, by, local_z);
                    if state != 0 {
                        h.record_filled(local_x, by, local_z, state);
                    }
                }
            }
        }
        self.heightmaps = h;
    }

    /// Returns the block state ID at `(local_x, block_y, local_z)`.
    #[inline]
    pub fn block_state(&self, local_x: usize, block_y: i32, local_z: usize) -> u32 {
        if block_y < self.min_y || block_y >= self.min_y + self.height {
            return 0;
        }
        let sec_idx = ((block_y >> 4) - self.min_section_y) as usize;
        let local_y = (block_y & 15) as usize;
        self.sections[sec_idx].block_state(local_x, local_y, local_z)
    }

    /// Sets the block state ID at `(local_x, block_y, local_z)` and updates heightmaps.
    #[inline]
    pub fn set_block_state(&mut self, local_x: usize, block_y: i32, local_z: usize, state: u32) {
        if block_y < self.min_y || block_y >= self.min_y + self.height {
            return;
        }
        let sec_idx = ((block_y >> 4) - self.min_section_y) as usize;
        let local_y = (block_y & 15) as usize;
        let prev = self.sections[sec_idx].set_block_state(local_x, local_y, local_z, state);
        if prev == state {
            return;
        }
        let top = block_y + 1;
        let removed_from_heightmap = HeightmapType::ALL.into_iter().any(|kind| {
            self.heightmaps.get(kind, local_x, local_z) == top
                && kind.matches(prev)
                && !kind.matches(state)
        });
        if removed_from_heightmap {
            self.recompute_column_heightmaps(local_x, local_z);
        } else {
            self.heightmaps
                .record_filled(local_x, block_y, local_z, state);
        }
    }

    fn recompute_column_heightmaps(&mut self, local_x: usize, local_z: usize) {
        let idx = (local_z << 4) | local_x;
        self.heightmaps.world_surface_wg[idx] = self.min_y;
        self.heightmaps.world_surface[idx] = self.min_y;
        self.heightmaps.ocean_floor_wg[idx] = self.min_y;
        self.heightmaps.ocean_floor[idx] = self.min_y;
        self.heightmaps.motion_blocking[idx] = self.min_y;
        self.heightmaps.motion_blocking_no_leaves[idx] = self.min_y;

        let top_y = self.min_y + self.height - 1;
        for by in (self.min_y..=top_y).rev() {
            let state = self.block_state(local_x, by, local_z);
            if state != 0 {
                self.heightmaps.record_filled(local_x, by, local_z, state);
            }
        }
    }
}

impl SurfaceChunkAccess for GeneratedChunk {
    #[inline]
    fn min_y(&self) -> i32 {
        self.min_y
    }

    #[inline]
    fn height(&self) -> i32 {
        self.height
    }

    #[inline]
    fn block_state(&self, local_x: usize, block_y: i32, local_z: usize) -> u32 {
        GeneratedChunk::block_state(self, local_x, block_y, local_z)
    }

    #[inline]
    fn set_block_state(&mut self, local_x: usize, block_y: i32, local_z: usize, state: u32) {
        GeneratedChunk::set_block_state(self, local_x, block_y, local_z, state);
    }

    #[inline]
    fn world_surface_wg(&self, local_x: usize, local_z: usize) -> i32 {
        self.heightmaps
            .get(HeightmapType::WorldSurfaceWg, local_x, local_z)
    }

    #[inline]
    fn get_noise_biome(&self, quart_x: i32, quart_y: i32, quart_z: i32) -> u32 {
        let min_q_y = self.min_y >> 2;
        let max_q_y = (self.min_y + self.height - 1) >> 2;
        let cy = quart_y.clamp(min_q_y, max_q_y);
        let sec_idx = ((cy >> 2) - self.min_section_y) as usize;
        let lx = (quart_x & 3) as usize;
        let ly = (cy & 3) as usize;
        let lz = (quart_z & 3) as usize;
        self.sections[sec_idx].biome(lx, ly, lz)
    }
}

/// Vanilla's `NoiseBasedChunkGenerator` compiled for a world seed and noise settings definition.
#[derive(Debug, Clone)]
pub struct NoiseBasedChunkGenerator {
    graph: CompiledDensityGraph,
    router: CompiledNoiseRouter,
    sampling: NoiseSamplingSettings,
    sea_level: i32,
    default_block: u32,
    default_fluid: u32,
    air_state: u32,
    water_state: u32,
    lava_state: u32,
    aquifers_enabled: bool,
    ore_veinifier: Option<OreVeinifier>,
    biome_source: MultiNoiseBiomeSource,
    biome_manager: BiomeManager,
    surface_system: SurfaceSystem,
    surface_rule: SurfaceRule,
}

impl NoiseBasedChunkGenerator {
    /// Compiles a `NoiseBasedChunkGenerator` for `settings_name` (such as `"minecraft:overworld"`) and
    /// `parameter_list` (such as `MultiNoiseParameterListDef::Preset("minecraft:overworld")`) with `world_seed`.
    pub fn new(
        world_seed: i64,
        settings_name: &str,
        parameter_list: &MultiNoiseParameterListDef,
        data: &WorldgenData,
    ) -> Result<Self, WorldgenDataError> {
        let settings = data.noise_settings.get(settings_name).ok_or_else(|| {
            WorldgenDataError::InvalidDefinition {
                context: settings_name.to_string(),
                message: format!("unknown noise_settings `{settings_name}`"),
            }
        })?;

        let (graph, router) = CompiledDensityGraph::compile_router(
            world_seed,
            settings.legacy_random_source,
            &settings.noise_router,
            &data.noises,
            &data.density_functions,
        )?;

        let default_block = resolve_block_state(&settings.default_block, settings_name)?;
        let default_fluid = resolve_block_state(&settings.default_fluid, settings_name)?;
        let states = block_states();
        let air_state = states.default_state("minecraft:air").unwrap_or(0);
        let water_state = states.default_state("minecraft:water").unwrap_or(0);
        let lava_state = states.default_state("minecraft:lava").unwrap_or(0);

        let ore_veinifier = settings
            .ore_veins_enabled
            .then(|| OreVeinifier::new(graph.ore_veins_random()));

        let biome_source = MultiNoiseBiomeSource::from_parameter_list(parameter_list, data)?;
        let biome_manager = BiomeManager::from_world_seed(world_seed);
        let surface_system = SurfaceSystem::new(&graph, default_block, settings.sea_level, data)?;
        let surface_rule = SurfaceRule::compile(&settings.surface_rule, &graph, data)?;

        Ok(Self {
            graph,
            router,
            sampling: settings.noise,
            sea_level: settings.sea_level,
            default_block,
            default_fluid,
            air_state,
            water_state,
            lava_state,
            aquifers_enabled: settings.aquifers_enabled,
            ore_veinifier,
            biome_source,
            biome_manager,
            surface_system,
            surface_rule,
        })
    }

    /// Convenience constructor for the standard Overworld (`"minecraft:overworld"` settings and preset).
    pub fn overworld(world_seed: i64, data: &WorldgenData) -> Result<Self, WorldgenDataError> {
        let preset = data
            .parameter_lists
            .get("minecraft:overworld")
            .cloned()
            .unwrap_or_else(|| MultiNoiseParameterListDef::Preset("minecraft:overworld".into()));
        Self::new(world_seed, "minecraft:overworld", &preset, data)
    }

    /// Underlying compiled density graph.
    pub const fn graph(&self) -> &CompiledDensityGraph {
        &self.graph
    }

    /// Underlying compiled noise router.
    pub const fn router(&self) -> &CompiledNoiseRouter {
        &self.router
    }

    /// Multi-noise biome source.
    pub const fn biome_source(&self) -> &MultiNoiseBiomeSource {
        &self.biome_source
    }

    /// 3D Voronoi biome manager.
    pub const fn biome_manager(&self) -> &BiomeManager {
        &self.biome_manager
    }

    /// Surface system.
    pub const fn surface_system(&self) -> &SurfaceSystem {
        &self.surface_system
    }

    /// Compiled surface rule.
    pub const fn surface_rule(&self) -> &SurfaceRule {
        &self.surface_rule
    }

    /// Creates a `NoiseChunk` for `(chunk_x, chunk_z)`.
    pub fn create_noise_chunk(&self, chunk_x: i32, chunk_z: i32) -> NoiseChunk<'_> {
        NoiseChunk::new(&self.graph, &self.router, self.sampling, chunk_x, chunk_z)
    }

    /// Creates the `Aquifer` for `(chunk_x, chunk_z)`.
    pub fn create_aquifer(&self, chunk_x: i32, chunk_z: i32) -> Aquifer {
        let global_picker =
            GlobalFluidPicker::new(self.sea_level, self.default_fluid, self.lava_state);
        if self.aquifers_enabled {
            Aquifer::noise_based(
                chunk_x,
                chunk_z,
                self.sampling.min_y,
                self.sampling.height,
                self.graph.aquifer_random(),
                global_picker,
                self.air_state,
                self.water_state,
                self.lava_state,
            )
        } else {
            Aquifer::disabled(global_picker, self.air_state)
        }
    }

    /// Populates all section biomes in `chunk` (`NoiseBasedChunkGenerator.createBiomes` / `doCreateBiomes`).
    pub fn populate_biomes(&self, chunk: &mut GeneratedChunk, noise_chunk: &mut NoiseChunk<'_>) {
        let chunk_x = chunk.chunk_x();
        let chunk_z = chunk.chunk_z();
        let min_sec_y = chunk.min_section_y();
        let mut last_node: Option<usize> = None;
        for (idx, section) in chunk.sections_mut().iter_mut().enumerate() {
            let sec_y = min_sec_y + (idx as i32);
            self.biome_source.fill_section_biomes(
                section,
                chunk_x,
                sec_y,
                chunk_z,
                noise_chunk,
                &mut last_node,
            );
        }
    }

    /// Fills base terrain blocks, aquifers, and ore veins in `chunk` (`NoiseBasedChunkGenerator.fillFromNoise` / `doFill`).
    pub fn fill_from_noise(&self, chunk: &mut GeneratedChunk, noise_chunk: &mut NoiseChunk<'_>) {
        let mut aquifer = self.create_aquifer(chunk.chunk_x(), chunk.chunk_z());
        let cell_width = self.sampling.cell_width() as usize;
        let cell_height = self.sampling.cell_height() as usize;
        let cell_count_xz = self.sampling.cell_count_xz();
        let cell_count_y = self.sampling.cell_count_y();
        let first_block_x = chunk.chunk_x() * 16;
        let first_block_z = chunk.chunk_z() * 16;
        let first_cell_y = self.sampling.min_y.div_euclid(cell_height as i32);
        let final_density_node = self.router.final_density;

        noise_chunk.initialize_for_first_cell_x();
        for cell_x in 0..cell_count_xz {
            noise_chunk.advance_cell_x(cell_x);
            for cell_z in 0..cell_count_xz {
                for cell_y in (0..cell_count_y).rev() {
                    noise_chunk.select_cell_yz(cell_y, cell_z);
                    for in_cell_y in (0..cell_height).rev() {
                        let block_y = (first_cell_y + (cell_y as i32)) * (cell_height as i32)
                            + (in_cell_y as i32);
                        let delta_y = (in_cell_y as f64) / (cell_height as f64);
                        noise_chunk.update_for_y(in_cell_y, delta_y);

                        for in_cell_x in 0..cell_width {
                            let block_x =
                                first_block_x + ((cell_x * cell_width + in_cell_x) as i32);
                            let local_x = (block_x & 15) as usize;
                            let delta_x = (in_cell_x as f64) / (cell_width as f64);
                            noise_chunk.update_for_x(in_cell_x, delta_x);

                            for in_cell_z in 0..cell_width {
                                let block_z =
                                    first_block_z + ((cell_z * cell_width + in_cell_z) as i32);
                                let local_z = (block_z & 15) as usize;
                                let delta_z = (in_cell_z as f64) / (cell_width as f64);
                                noise_chunk.update_for_z(in_cell_z, delta_z);

                                let final_density = noise_chunk.eval_node(final_density_node);
                                let state = match aquifer.compute_substance(
                                    noise_chunk,
                                    block_x,
                                    block_y,
                                    block_z,
                                    final_density,
                                ) {
                                    Some(fluid_or_air) => fluid_or_air,
                                    None => {
                                        if let Some(ore_veinifier) = &self.ore_veinifier
                                            && let Some(vein_state) = ore_veinifier.compute(
                                                noise_chunk,
                                                block_x,
                                                block_y,
                                                block_z,
                                            )
                                        {
                                            vein_state
                                        } else {
                                            self.default_block
                                        }
                                    }
                                };

                                if state != self.air_state {
                                    chunk.set_block_state(local_x, block_y, local_z, state);
                                    if aquifer.should_schedule_fluid_update()
                                        && (block_heightmap_flags()[state as usize]
                                            & FLAG_HAS_FLUID)
                                            != 0
                                    {
                                        chunk.fluid_postprocessing.push((
                                            local_x as u8,
                                            block_y,
                                            local_z as u8,
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            noise_chunk.swap_slices();
        }
        noise_chunk.stop_interpolation();
    }

    /// Applies surface rules, eroded badlands pillars, and frozen ocean icebergs to `chunk`
    /// (`NoiseBasedChunkGenerator.buildSurface`).
    pub fn build_surface(
        &self,
        chunk: &mut GeneratedChunk,
        noise_chunk: &mut NoiseChunk<'_>,
        data: &WorldgenData,
    ) {
        self.surface_system.build_surface(
            chunk,
            chunk.chunk_x(),
            chunk.chunk_z(),
            &self.biome_manager,
            Some(&self.biome_source),
            noise_chunk,
            &data.biomes,
            &self.surface_rule,
        );
    }

    /// Generates a complete chunk up to the `surface` stage (`populate_biomes` + `fill_from_noise` + `build_surface`).
    pub fn generate_chunk_surface(
        &self,
        chunk_x: i32,
        chunk_z: i32,
        data: &WorldgenData,
    ) -> GeneratedChunk {
        let mut chunk = GeneratedChunk::new(
            chunk_x,
            chunk_z,
            self.sampling.min_y,
            self.sampling.height,
            data.biomes.len().max(1),
        );
        let mut noise_chunk = self.create_noise_chunk(chunk_x, chunk_z);
        self.populate_biomes(&mut chunk, &mut noise_chunk);
        self.fill_from_noise(&mut chunk, &mut noise_chunk);
        self.build_surface(&mut chunk, &mut noise_chunk, data);
        chunk
    }
}

#[cfg(test)]
mod tests {
    use super::{GeneratedChunk, HeightmapType};
    use harpas_core::block::block_states;

    #[test]
    fn replacing_the_top_block_updates_each_affected_heightmap() {
        let states = block_states();
        let stone = states.default_state("minecraft:stone").unwrap();
        let water = states.default_state("minecraft:water").unwrap();
        let leaves = states.default_state("minecraft:oak_leaves").unwrap();
        let cave_air = states.default_state("minecraft:cave_air").unwrap();
        let mut chunk = GeneratedChunk::new(0, 0, -64, 384, 1);

        chunk.set_block_state(3, 5, 7, stone);
        chunk.set_block_state(3, 10, 7, stone);
        chunk.set_block_state(3, 10, 7, water);
        assert_eq!(
            chunk.heightmaps().get(HeightmapType::WorldSurfaceWg, 3, 7),
            11
        );
        assert_eq!(chunk.heightmaps().get(HeightmapType::OceanFloorWg, 3, 7), 6);
        assert_eq!(
            chunk.heightmaps().get(HeightmapType::MotionBlocking, 3, 7),
            11
        );

        chunk.set_block_state(3, 10, 7, leaves);
        assert_eq!(chunk.heightmaps().get(HeightmapType::OceanFloor, 3, 7), 11);
        assert_eq!(
            chunk
                .heightmaps()
                .get(HeightmapType::MotionBlockingNoLeaves, 3, 7),
            6
        );

        chunk.set_block_state(3, 10, 7, cave_air);
        for kind in HeightmapType::ALL {
            assert_eq!(chunk.heightmaps().get(kind, 3, 7), 6, "{kind:?}");
        }
    }

    #[test]
    fn snow_and_powder_snow_do_not_block_heightmaps() {
        let states = block_states();
        for state in [
            states.default_state("minecraft:snow").unwrap(),
            states.state("minecraft:snow", [("layers", "8")]).unwrap().0,
            states.default_state("minecraft:powder_snow").unwrap(),
        ] {
            assert!(HeightmapType::WorldSurfaceWg.matches(state));
            assert!(!HeightmapType::OceanFloorWg.matches(state));
            assert!(!HeightmapType::MotionBlocking.matches(state));
        }
    }
}
