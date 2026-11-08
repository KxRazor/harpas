//! `NoiseBasedChunkGenerator`: orchestrates `NoiseChunk`, `MultiNoiseBiomeSource`, `Aquifer`, `OreVeinifier`,
//! `SurfaceSystem`, and chunk heightmaps (`OCEAN_FLOOR_WG`, `WORLD_SURFACE_WG`, `MOTION_BLOCKING`, etc.)
//!
//! Generates chunk columns through the `biomes`, `noise`, and `surface` worldgen stages
//! (`docs/world.md#world-generation`).

use std::collections::HashMap;
use std::sync::OnceLock;

use harpas_core::block::block_states;
use harpas_core::block_behavior::block_behavior;
use harpas_core::chunk::{PalettedContainer, Section, Strategy};
use harpas_core::nbt::Compound;
use harpas_core::random::{LegacyRandom, WorldgenRandom};

use crate::aquifer::{Aquifer, GlobalFluidPicker};
use crate::beardifier::Beardifier;
use crate::biome::{BiomeManager, MultiNoiseBiomeSource};
use crate::carver::{
    CarverConfig, CarverShape, CarvingEnvironment, CarvingMask, CavePart, canyon_should_skip,
    carve_ellipsoid, cave_should_skip, plan_canyon_path, plan_cave_parts,
};
use crate::data::{
    MultiNoiseParameterListDef, NoiseSamplingSettings, WorldgenData, WorldgenDataError,
};
use crate::density::{CompiledDensityGraph, CompiledNoiseRouter};
use crate::noise_chunk::NoiseChunk;
use crate::ore_vein::OreVeinifier;
use crate::placement::{Fluid, fluid};
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

    /// The 4 heightmaps that a chunk keeps once carvers have run (`ChunkStatus.FINAL_HEIGHTMAPS`).
    pub const FINAL: [Self; 4] = [
        Self::WorldSurface,
        Self::OceanFloor,
        Self::MotionBlocking,
        Self::MotionBlockingNoLeaves,
    ];

    /// Whether this is one of the 2 heightmaps used only until carvers (`WORLD_SURFACE_WG`, `OCEAN_FLOOR_WG`).
    pub const fn is_worldgen(self) -> bool {
        matches!(self, Self::WorldSurfaceWg | Self::OceanFloorWg)
    }

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

    /// Returns whether `state` satisfies this heightmap's predicate (`Heightmap.Types.isOpaque`).
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

/// Per block state: not air (`WORLD_SURFACE`), holds a fluid, blocks motion (`OCEAN_FLOOR`; with a fluid,
/// `MOTION_BLOCKING`) and is leaves, which `MOTION_BLOCKING_NO_LEAVES` leaves out.
fn block_heightmap_flags() -> &'static [u8] {
    static FLAGS: OnceLock<Vec<u8>> = OnceLock::new();
    FLAGS.get_or_init(|| {
        let states = block_states();
        let behavior = block_behavior();
        (0..states.len())
            .map(|id| {
                if states.is_air(id) {
                    return 0;
                }
                let (name, _) = states.properties(id).expect("known block state");
                let mut f = FLAG_NOT_AIR;
                if fluid(id) != Fluid::Empty {
                    f |= FLAG_HAS_FLUID;
                }
                if behavior.blocks_motion(id) {
                    f |= FLAG_BLOCKS_MOTION;
                }
                if name.ends_with("_leaves") {
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
        self.record(local_x, block_y, local_z, state, true);
    }

    #[inline]
    fn record(&mut self, local_x: usize, block_y: i32, local_z: usize, state: u32, worldgen: bool) {
        let flags = block_heightmap_flags()[state as usize];
        if flags == 0 {
            return;
        }
        let idx = (local_z << 4) | local_x;
        let top = block_y + 1;
        if (flags & FLAG_NOT_AIR) != 0 {
            if worldgen && top > self.world_surface_wg[idx] {
                self.world_surface_wg[idx] = top;
            }
            if top > self.world_surface[idx] {
                self.world_surface[idx] = top;
            }
        }
        if (flags & FLAG_BLOCKS_MOTION) != 0 {
            if worldgen && top > self.ocean_floor_wg[idx] {
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
    carving_mask: Option<CarvingMask>,
    /// Whether block changes leave `WORLD_SURFACE_WG` and `OCEAN_FLOOR_WG` as they are.
    worldgen_heightmaps_frozen: bool,
    /// Block entities that features created, by local X, absolute Y and local Z.
    block_entities: Vec<((u8, i32, u8), Compound)>,
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
            carving_mask: None,
            worldgen_heightmaps_frozen: false,
            block_entities: Vec::new(),
        }
    }

    /// Rebuilds a chunk from saved sections, from `min_y` upward, and recomputes its heightmaps from the
    /// blocks, as vanilla primes them when it loads a proto-chunk. A missing section is air in biome 0.
    /// Returns `None` when there are more sections than the height holds.
    pub fn from_sections(
        chunk_x: i32,
        chunk_z: i32,
        min_y: i32,
        height: i32,
        biome_count: usize,
        sections: Vec<Option<Section>>,
    ) -> Option<Self> {
        let mut chunk = Self::new(chunk_x, chunk_z, min_y, height, biome_count);
        if sections.len() > chunk.sections.len() {
            return None;
        }
        for (slot, section) in chunk.sections.iter_mut().zip(sections) {
            if let Some(section) = section {
                *slot = section;
            }
        }
        chunk.recompute_heightmaps();
        Some(chunk)
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

    /// Takes ownership of the generated sections, from the minimum section upward.
    pub fn into_sections(self) -> Vec<Section> {
        self.sections
    }

    /// Sets the saved data of the block entity at `(local_x, block_y, local_z)`, as features create them. A
    /// later change of that block's state removes it, or for a block with a block entity, replaces it with the
    /// `DUMMY` placeholder of a block entity not created yet.
    pub fn set_block_entity(&mut self, local_x: u8, block_y: i32, local_z: u8, entity: Compound) {
        let position = (local_x, block_y, local_z);
        self.block_entities.retain(|(at, _)| *at != position);
        self.block_entities.push((position, entity));
    }

    /// The block entities features created, in the order they were set.
    pub fn block_entities(&self) -> impl Iterator<Item = &Compound> {
        self.block_entities.iter().map(|(_, entity)| entity)
    }

    /// Takes the block entities features created, in the order they were set.
    pub fn take_block_entities(&mut self) -> Vec<Compound> {
        std::mem::take(&mut self.block_entities)
            .into_iter()
            .map(|(_, entity)| entity)
            .collect()
    }

    /// Current heightmaps of this chunk.
    pub const fn heightmaps(&self) -> &Heightmaps {
        &self.heightmaps
    }

    /// Local X, absolute Y, and local Z requiring postprocessing after generation (fluids and structure fences).
    pub fn fluid_postprocessing(&self) -> &[(u8, i32, u8)] {
        &self.fluid_postprocessing
    }

    /// The carving mask once the carving stage has completed.
    pub fn carving_mask(&self) -> Option<&CarvingMask> {
        self.carving_mask.as_ref()
    }

    /// Stops block changes from updating `WORLD_SURFACE_WG` and `OCEAN_FLOOR_WG`, as vanilla does once a chunk
    /// has run carvers: a proto-chunk then updates only the heightmaps its status lists, the 4 final ones.
    /// Features and structures that read the worldgen heightmaps see the terrain carvers left.
    pub fn freeze_worldgen_heightmaps(&mut self) {
        self.worldgen_heightmaps_frozen = true;
    }

    pub(crate) fn mark_fluid_postprocessing(&mut self, x: u8, y: i32, z: u8) {
        self.fluid_postprocessing.push((x, y, z));
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
        // `WorldGenRegion.setBlock`: a block with a block entity gets the placeholder vanilla saves until its block
        // entity is created; any other block removes the block entity of the block it replaces.
        let position = (local_x as u8, block_y, local_z as u8);
        if block_behavior().has_block_entity(state) {
            let mut dummy = Compound::new();
            dummy.insert("x", self.chunk_x * 16 + local_x as i32);
            dummy.insert("y", block_y);
            dummy.insert("z", self.chunk_z * 16 + local_z as i32);
            dummy.insert("id", "DUMMY");
            self.set_block_entity(position.0, block_y, position.2, dummy);
        } else if !self.block_entities.is_empty() {
            self.block_entities.retain(|(at, _)| *at != position);
        }
        let top = block_y + 1;
        let worldgen = !self.worldgen_heightmaps_frozen;
        let removed_from_heightmap = HeightmapType::ALL.into_iter().any(|kind| {
            (worldgen || !kind.is_worldgen())
                && self.heightmaps.get(kind, local_x, local_z) == top
                && kind.matches(prev)
                && !kind.matches(state)
        });
        if removed_from_heightmap {
            self.recompute_column_heightmaps(local_x, local_z);
        } else {
            self.heightmaps
                .record(local_x, block_y, local_z, state, worldgen);
        }
    }

    fn recompute_column_heightmaps(&mut self, local_x: usize, local_z: usize) {
        let idx = (local_z << 4) | local_x;
        let worldgen = !self.worldgen_heightmaps_frozen;
        if worldgen {
            self.heightmaps.world_surface_wg[idx] = self.min_y;
            self.heightmaps.ocean_floor_wg[idx] = self.min_y;
        }
        self.heightmaps.world_surface[idx] = self.min_y;
        self.heightmaps.ocean_floor[idx] = self.min_y;
        self.heightmaps.motion_blocking[idx] = self.min_y;
        self.heightmaps.motion_blocking_no_leaves[idx] = self.min_y;

        let top_y = self.min_y + self.height - 1;
        for by in (self.min_y..=top_y).rev() {
            let state = self.block_state(local_x, by, local_z);
            if state != 0 {
                self.heightmaps
                    .record(local_x, by, local_z, state, worldgen);
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
    fn mark_fluid_postprocessing(&mut self, local_x: usize, block_y: i32, local_z: usize) {
        GeneratedChunk::mark_fluid_postprocessing(self, local_x as u8, block_y, local_z as u8);
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
    world_seed: i64,
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
            world_seed,
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
        self.fill_from_noise_with_aquifer(chunk, noise_chunk, &mut aquifer);
    }

    /// Fills terrain while retaining the aquifer for the subsequent carving pass.
    pub fn fill_from_noise_with_aquifer(
        &self,
        chunk: &mut GeneratedChunk,
        noise_chunk: &mut NoiseChunk<'_>,
        aquifer: &mut Aquifer,
    ) {
        let cell_width = self.sampling.cell_width() as usize;
        let cell_height = self.sampling.cell_height() as usize;
        let cell_count_xz = self.sampling.cell_count_xz();
        let cell_count_y = self.sampling.cell_count_y();
        let first_block_x = chunk.chunk_x() * 16;
        let first_block_z = chunk.chunk_z() * 16;
        let first_cell_y = self.sampling.min_y.div_euclid(cell_height as i32);

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

                                let state = self.terrain_state(
                                    noise_chunk,
                                    aquifer,
                                    [block_x, block_y, block_z],
                                );

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

    /// The terrain block at the interpolated position of `noise_chunk`: the aquifer's fluid or air, an ore vein or
    /// the default block (`NoiseChunk.getInterpolatedState`).
    fn terrain_state(
        &self,
        noise_chunk: &mut NoiseChunk<'_>,
        aquifer: &mut Aquifer,
        [block_x, block_y, block_z]: [i32; 3],
    ) -> u32 {
        let final_density = noise_chunk.interpolated_density();
        match aquifer.compute_substance(noise_chunk, block_x, block_y, block_z, final_density) {
            Some(fluid_or_air) => fluid_or_air,
            None => self
                .ore_veinifier
                .as_ref()
                .and_then(|veinifier| veinifier.compute(noise_chunk, block_x, block_y, block_z))
                .unwrap_or(self.default_block),
        }
    }

    /// One above the highest block of a column that `kind` counts, from the noise alone, before surface rules and
    /// carvers (`NoiseBasedChunkGenerator.getBaseHeight`). It is the minimum Y if no block counts.
    pub fn base_height(&self, block_x: i32, block_z: i32, kind: HeightmapType) -> i32 {
        let mut height = self.sampling.min_y;
        self.iterate_noise_column(block_x, block_z, |block_y, state| {
            let found = kind.matches(state);
            if found {
                height = block_y + 1;
            }
            found
        });
        height
    }

    /// The blocks of a column from the noise alone, from the minimum Y up, before surface rules and carvers
    /// (`NoiseBasedChunkGenerator.getBaseColumn`).
    pub fn base_column(&self, block_x: i32, block_z: i32) -> Vec<u32> {
        let height = self.sampling.cell_count_y() * self.sampling.cell_height() as usize;
        let mut column = vec![0; height];
        self.iterate_noise_column(block_x, block_z, |block_y, state| {
            column[(block_y - self.sampling.min_y) as usize] = state;
            false
        });
        column
    }

    /// Visits the blocks of a column from the top down until `visit` returns true
    /// (`NoiseBasedChunkGenerator.iterateNoiseColumn`).
    fn iterate_noise_column(
        &self,
        block_x: i32,
        block_z: i32,
        mut visit: impl FnMut(i32, u32) -> bool,
    ) {
        let cell_width = self.sampling.cell_width();
        let cell_height = self.sampling.cell_height();
        let first_cell_y = self.sampling.min_y.div_euclid(cell_height);
        let in_cell_x = block_x.rem_euclid(cell_width);
        let in_cell_z = block_z.rem_euclid(cell_width);
        let mut noise_chunk = NoiseChunk::with_cells(
            &self.graph,
            &self.router,
            self.sampling,
            1,
            block_x - in_cell_x,
            block_z - in_cell_z,
        );
        let mut aquifer = self.create_aquifer(block_x >> 4, block_z >> 4);
        noise_chunk.initialize_for_first_cell_x();
        noise_chunk.advance_cell_x(0);
        for cell_y in (0..self.sampling.cell_count_y()).rev() {
            noise_chunk.select_cell_yz(cell_y, 0);
            for in_cell_y in (0..cell_height).rev() {
                let block_y = (first_cell_y + cell_y as i32) * cell_height + in_cell_y;
                noise_chunk.update_for_y(
                    in_cell_y as usize,
                    f64::from(in_cell_y) / f64::from(cell_height),
                );
                noise_chunk.update_for_x(
                    in_cell_x as usize,
                    f64::from(in_cell_x) / f64::from(cell_width),
                );
                noise_chunk.update_for_z(
                    in_cell_z as usize,
                    f64::from(in_cell_z) / f64::from(cell_width),
                );
                let state =
                    self.terrain_state(&mut noise_chunk, &mut aquifer, [block_x, block_y, block_z]);
                if visit(block_y, state) {
                    noise_chunk.stop_interpolation();
                    return;
                }
            }
        }
        noise_chunk.stop_interpolation();
    }

    /// The lowest Y of the noise settings.
    pub const fn min_y(&self) -> i32 {
        self.sampling.min_y
    }

    /// The sea level of the noise settings.
    pub const fn sea_level(&self) -> i32 {
        self.sea_level
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

    /// Evaluates the surface rule for a block of dirt newly exposed by a carver.
    pub fn top_material(
        &self,
        chunk: &GeneratedChunk,
        noise_chunk: &mut NoiseChunk<'_>,
        data: &WorldgenData,
        position: [i32; 3],
        above_has_fluid: bool,
    ) -> Option<u32> {
        self.surface_system.top_material(
            chunk,
            &self.biome_manager,
            Some(&self.biome_source),
            noise_chunk,
            &data.biomes,
            &self.surface_rule,
            position,
            above_has_fluid,
        )
    }

    /// Applies one configured carver from `source` to `chunk`, after its start decision has succeeded.
    /// The same mask and aquifer must be reused for all carvers in a target chunk.
    #[allow(clippy::too_many_arguments)]
    pub fn carve_from_source(
        &self,
        chunk: &mut GeneratedChunk,
        noise_chunk: &mut NoiseChunk<'_>,
        aquifer: &mut Aquifer,
        data: &WorldgenData,
        mask: &mut CarvingMask,
        config: &CarverConfig,
        random: &mut impl harpas_core::random::RandomSource,
        source: [i32; 2],
    ) -> Result<(), WorldgenDataError> {
        let min_y = chunk.min_y();
        let height = chunk.height();
        let (chunk_x, chunk_z) = (chunk.chunk_x(), chunk.chunk_z());
        let mut environment = GeneratorCarvingEnvironment {
            generator: self,
            data,
            noise_chunk,
            aquifer,
        };
        match config.shape {
            CarverShape::Cave { nether: false, .. } => {
                plan_cave_parts(
                    config,
                    random,
                    source[0],
                    source[1],
                    min_y,
                    height,
                    |part| match part {
                        CavePart::Room(room) => {
                            carve_ellipsoid(
                                chunk,
                                mask,
                                config,
                                room.ellipsoid(),
                                &mut environment,
                                |point, _| cave_should_skip(point, f64::from(room.floor_level)),
                            );
                        }
                        CavePart::Tunnel { path, floor_level } => {
                            path.for_each_ellipsoid(chunk_x, chunk_z, |ellipsoid| {
                                carve_ellipsoid(
                                    chunk,
                                    mask,
                                    config,
                                    ellipsoid,
                                    &mut environment,
                                    |point, _| cave_should_skip(point, f64::from(floor_level)),
                                );
                            });
                        }
                    },
                )
                .map_err(|message| WorldgenDataError::InvalidDefinition {
                    context: "cave carver".to_owned(),
                    message: message.to_owned(),
                })?;
            }
            CarverShape::Canyon(shape) => {
                let path = plan_canyon_path(config, random, source[0], source[1], min_y, height)
                    .map_err(|message| WorldgenDataError::InvalidDefinition {
                        context: "canyon carver".to_owned(),
                        message: message.to_owned(),
                    })?;
                path.for_each_ellipsoid(
                    &shape,
                    chunk_x,
                    chunk_z,
                    height as usize,
                    |ellipsoid, widths| {
                        carve_ellipsoid(
                            chunk,
                            mask,
                            config,
                            ellipsoid,
                            &mut environment,
                            |point, y| canyon_should_skip(point, widths[(y - min_y - 1) as usize]),
                        );
                    },
                );
            }
            CarverShape::Cave { nether: true, .. } => {
                return Err(WorldgenDataError::InvalidDefinition {
                    context: "nether cave carver".to_owned(),
                    message: "Nether cave placement is not supported yet".to_owned(),
                });
            }
        }
        Ok(())
    }

    /// Applies the configured carvers of the surrounding source chunks to a surface-generated chunk.
    pub fn apply_carvers(
        &self,
        chunk: &mut GeneratedChunk,
        noise_chunk: &mut NoiseChunk<'_>,
        aquifer: &mut Aquifer,
        data: &WorldgenData,
    ) -> Result<CarvingMask, WorldgenDataError> {
        let mut mask = CarvingMask::new(chunk.min_y(), chunk.height());
        let mut compiled = HashMap::new();
        let mut last_biome = None;
        let mut random = WorldgenRandom::new(LegacyRandom::new(0));
        let chunk_x = chunk.chunk_x();
        let chunk_z = chunk.chunk_z();
        for dx in -8_i32..=8 {
            for dz in -8_i32..=8 {
                let sx = chunk_x.wrapping_add(dx);
                let sz = chunk_z.wrapping_add(dz);
                let climate = noise_chunk.sample_climate(sx.wrapping_mul(4), 0, sz.wrapping_mul(4));
                let biome_id = self.biome_source.get_noise_biome(climate, &mut last_biome);
                let (_, biome) = data.biomes.get_index(biome_id as usize).ok_or_else(|| {
                    WorldgenDataError::InvalidDefinition {
                        context: "carver biome".to_owned(),
                        message: format!("unknown biome ID {biome_id}"),
                    }
                })?;
                for (index, name) in biome.carvers.iter().enumerate() {
                    let name = if name.contains(':') {
                        name.clone()
                    } else {
                        format!("minecraft:{name}")
                    };
                    if !compiled.contains_key(&name) {
                        compiled.insert(name.clone(), CarverConfig::compile(&name, data)?);
                    }
                    let config = &compiled[&name];
                    random.set_large_feature_seed(
                        self.world_seed.wrapping_add(index as i64),
                        sx,
                        sz,
                    );
                    if config.starts(&mut random) {
                        self.carve_from_source(
                            chunk,
                            noise_chunk,
                            aquifer,
                            data,
                            &mut mask,
                            config,
                            &mut random,
                            [sx, sz],
                        )?;
                    }
                }
            }
        }
        Ok(mask)
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

    /// Generates a chunk through the cave and canyon carving stage.
    pub fn generate_chunk_carved(
        &self,
        chunk_x: i32,
        chunk_z: i32,
        data: &WorldgenData,
    ) -> Result<GeneratedChunk, WorldgenDataError> {
        self.generate_chunk_carved_with_beardifier(chunk_x, chunk_z, data, Beardifier::default())
    }

    /// Generates through carvers with the structure density collected from the chunk's referenced starts.
    /// This adjusts terrain only; the caller handles persistence of starts/references and structure blocks.
    pub fn generate_chunk_carved_with_beardifier(
        &self,
        chunk_x: i32,
        chunk_z: i32,
        data: &WorldgenData,
        beardifier: Beardifier,
    ) -> Result<GeneratedChunk, WorldgenDataError> {
        let mut chunk = GeneratedChunk::new(
            chunk_x,
            chunk_z,
            self.sampling.min_y,
            self.sampling.height,
            data.biomes.len().max(1),
        );
        let mut noise_chunk = self
            .create_noise_chunk(chunk_x, chunk_z)
            .with_beardifier(beardifier);
        let mut aquifer = self.create_aquifer(chunk_x, chunk_z);
        self.populate_biomes(&mut chunk, &mut noise_chunk);
        self.fill_from_noise_with_aquifer(&mut chunk, &mut noise_chunk, &mut aquifer);
        self.build_surface(&mut chunk, &mut noise_chunk, data);
        chunk.carving_mask =
            Some(self.apply_carvers(&mut chunk, &mut noise_chunk, &mut aquifer, data)?);
        chunk.freeze_worldgen_heightmaps();
        Ok(chunk)
    }
}

struct GeneratorCarvingEnvironment<'a, 'graph> {
    generator: &'a NoiseBasedChunkGenerator,
    data: &'a WorldgenData,
    noise_chunk: &'a mut NoiseChunk<'graph>,
    aquifer: &'a mut Aquifer,
}

impl CarvingEnvironment for GeneratorCarvingEnvironment<'_, '_> {
    fn compute_substance(&mut self, x: i32, y: i32, z: i32) -> Option<u32> {
        self.aquifer
            .compute_substance(self.noise_chunk, x, y, z, 0.0)
    }

    fn should_schedule_fluid_update(&self) -> bool {
        self.aquifer.should_schedule_fluid_update()
    }

    fn has_fluid(&self, state: u32) -> bool {
        block_heightmap_flags()
            .get(state as usize)
            .is_some_and(|flags| flags & FLAG_HAS_FLUID != 0)
    }

    fn top_material(
        &mut self,
        chunk: &GeneratedChunk,
        x: i32,
        y: i32,
        z: i32,
        above_has_fluid: bool,
    ) -> Option<u32> {
        self.generator.top_material(
            chunk,
            self.noise_chunk,
            self.data,
            [x, y, z],
            above_has_fluid,
        )
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

    #[test]
    fn stronghold_decoration_blocks_have_vanillas_heightmap_predicates() {
        for name in ["torch", "stone_button", "end_portal"] {
            let state = block_states()
                .default_state(&format!("minecraft:{name}"))
                .unwrap();
            assert!(HeightmapType::WorldSurface.matches(state));
            assert!(HeightmapType::WorldSurfaceWg.matches(state));
            for kind in [
                HeightmapType::OceanFloor,
                HeightmapType::OceanFloorWg,
                HeightmapType::MotionBlocking,
                HeightmapType::MotionBlockingNoLeaves,
            ] {
                assert!(!kind.matches(state), "{name} {kind:?}");
            }
        }
    }
}
