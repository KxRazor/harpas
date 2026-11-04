//! Worldgen datapack loader for vanilla `26.1.zip` or loose JSON files
//!
//! Reads the JSON definitions under `data/<namespace>/worldgen/` from a provisioned vanilla data archive
//! (`.harpas/vanilla-data/26.1.zip`), a directory on disk, or an in-memory map of relative paths to JSON strings.
//! No Mojang data is embedded in the binary (`docs/world.md#21-generated-worldgen-data`).

use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Seek};
use std::path::Path;

use harpas_core::block::block_states;
use harpas_core::noise::NoiseParameters;
use indexmap::IndexMap;
use serde_json::Value;

use crate::biome::{Parameter, ParameterPoint, quantize_coord};
use crate::density::{
    BinaryOp, DensityFunctionDef, MarkerKind, NoiseRouterDef, RarityValueMapper, UnaryOp,
};
use crate::spline::{CubicSplineDef, SplinePointDef};

/// Error returned when loading or parsing worldgen JSON data fails.
#[derive(Debug)]
pub enum WorldgenDataError {
    /// Underlying filesystem I/O error.
    Io(io::Error),
    /// Error reading a zip archive.
    Zip(zip::result::ZipError),
    /// Invalid JSON syntax in an entry.
    Json {
        /// Path of the offending file inside the archive or directory.
        path: String,
        /// Underlying `serde_json` error.
        source: serde_json::Error,
    },
    /// Schema or structural error inside a parsed JSON definition.
    InvalidDefinition {
        /// Path or resource identifier where the error occurred.
        context: String,
        /// Description of the structural issue.
        message: String,
    },
}

impl fmt::Display for WorldgenDataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "worldgen data I/O error: {err}"),
            Self::Zip(err) => write!(f, "worldgen zip archive error: {err}"),
            Self::Json { path, source } => write!(f, "invalid JSON in {path}: {source}"),
            Self::InvalidDefinition { context, message } => {
                write!(f, "invalid worldgen definition in {context}: {message}")
            }
        }
    }
}

impl Error for WorldgenDataError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Zip(err) => Some(err),
            Self::Json { source, .. } => Some(source),
            Self::InvalidDefinition { .. } => None,
        }
    }
}

impl From<io::Error> for WorldgenDataError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<zip::result::ZipError> for WorldgenDataError {
    fn from(value: zip::result::ZipError) -> Self {
        Self::Zip(value)
    }
}

/// Grid dimensions and cell sizes of a noise generator (`noise` object inside `noise_settings/*.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoiseSamplingSettings {
    /// Minimum build and generation Y coordinate (for example `-64` in Overworld).
    pub min_y: i32,
    /// Total generation height in blocks (for example `384` in Overworld).
    pub height: i32,
    /// Horizontal cell size in quarts (`cell_width = size_horizontal * 4`, `1` -> `4` blocks in Overworld).
    pub size_horizontal: i32,
    /// Vertical cell size in quarts (`cell_height = size_vertical * 4`, `2` -> `8` blocks in Overworld).
    pub size_vertical: i32,
}

impl NoiseSamplingSettings {
    /// Width of one interpolation cell in blocks (`size_horizontal * 4`).
    pub const fn cell_width(self) -> i32 {
        self.size_horizontal * 4
    }

    /// Height of one interpolation cell in blocks (`size_vertical * 4`).
    pub const fn cell_height(self) -> i32 {
        self.size_vertical * 4
    }

    /// Number of vertical interpolation cells in a column (`height / cell_height()`).
    pub const fn cell_count_y(self) -> usize {
        (self.height / self.cell_height()) as usize
    }

    /// Number of horizontal interpolation cells across a 16-wide chunk (`16 / cell_width()`).
    pub const fn cell_count_xz(self) -> usize {
        (16 / self.cell_width()) as usize
    }
}

/// A block state definition as written in worldgen JSON (`Name` + optional `Properties`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlockStateDef {
    /// Block identifier, such as `"minecraft:stone"` or `"minecraft:water"`.
    pub name: String,
    /// Block state properties in deterministic key order, such as `{"level": "0"}`.
    pub properties: BTreeMap<String, String>,
}

impl BlockStateDef {
    /// Creates a block state definition with no properties.
    pub fn simple(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            properties: BTreeMap::new(),
        }
    }
}

/// Parsed contents of a `worldgen/noise_settings/<name>.json` file.
#[derive(Debug, Clone, PartialEq)]
pub struct NoiseSettingsDef {
    /// Noise cell grid dimensions (`min_y`, `height`, `size_horizontal`, `size_vertical`).
    pub noise: NoiseSamplingSettings,
    /// Default solid block placed where `final_density > 0.0` (e.g. `minecraft:stone`).
    pub default_block: BlockStateDef,
    /// Default fluid block placed at or below `sea_level` (e.g. `minecraft:water[level=0]`).
    pub default_fluid: BlockStateDef,
    /// The 15 density function channels of the noise router.
    pub noise_router: NoiseRouterDef,
    /// Raw surface rule JSON tree (compiled in `SurfaceSystem`).
    pub surface_rule: Value,
    /// Climate target ranges used to locate the world spawn point.
    pub spawn_target: Vec<ParameterPoint>,
    /// Sea level Y coordinate (`63` in Overworld).
    pub sea_level: i32,
    /// Whether natural mob spawning during chunk generation is disabled.
    pub disable_mob_generation: bool,
    /// Whether `NoiseBasedAquifer` is active (`true` in Overworld).
    pub aquifers_enabled: bool,
    /// Whether `OreVeinifier` is active (`true` in Overworld).
    pub ore_veins_enabled: bool,
    /// Whether `LegacyRandom` is used instead of `XoroshiroRandom` (`false` in Overworld).
    pub legacy_random_source: bool,
}

/// How a biome modifies its base temperature with height and 2D Simplex noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TemperatureModifier {
    /// Standard temperature (`"none"`).
    #[default]
    None,
    /// Frozen ocean/ice patch temperature pockets (`"frozen"`).
    Frozen,
}

/// Parsed contents of a `worldgen/biome/<name>.json` file relevant to terrain, biomes, and carvers.
#[derive(Debug, Clone, PartialEq)]
pub struct BiomeDef {
    /// Whether the biome has rain or snow precipitation.
    pub has_precipitation: bool,
    /// Base biome temperature (for example `0.8` in `minecraft:plains` or `0.0` in `minecraft:frozen_ocean`).
    pub temperature: f32,
    /// Optional temperature modifier (`none` or `frozen`).
    pub temperature_modifier: TemperatureModifier,
    /// Biome downfall / humidity value.
    pub downfall: f32,
    /// Configured carver identifiers assigned to this biome (e.g. `["minecraft:cave", "minecraft:canyon"]`).
    pub carvers: Vec<String>,
}

/// An entry in an explicit `multi_noise_biome_source_parameter_list` `"biomes"` array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BiomeParameterEntryDef {
    /// Biome resource location (e.g. `"minecraft:plains"`).
    pub biome: String,
    /// 7D climate hyper-rectangle for this entry.
    pub parameters: ParameterPoint,
}

/// Parsed contents of `worldgen/multi_noise_biome_source_parameter_list/<name>.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MultiNoiseParameterListDef {
    /// Uses a built-in parameter preset such as `"minecraft:overworld"` or `"minecraft:nether"`.
    Preset(String),
    /// Uses an explicit list of `(ParameterPoint, biome)` pairs from the datapack.
    Biomes(Vec<BiomeParameterEntryDef>),
}

/// Biome source specification inside a dimension generator definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BiomeSourceDef {
    /// `minecraft:multi_noise` with either a `preset` identifier or an inline `biomes` list.
    MultiNoise(MultiNoiseParameterListDef),
    /// `minecraft:fixed` with a single biome identifier.
    Fixed(String),
    /// `minecraft:the_end`.
    TheEnd,
    /// Any other custom biome source type.
    Other(String),
}

/// Dimension generator configuration inside `worldgen/world_preset/*.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DimensionDef {
    /// Dimension type identifier (e.g. `"minecraft:overworld"`).
    pub dimension_type: String,
    /// Generator type identifier (e.g. `"minecraft:noise"`).
    pub generator_type: String,
    /// Noise settings identifier (e.g. `"minecraft:overworld"`), when `generator_type == "minecraft:noise"`.
    pub settings: Option<String>,
    /// Biome source configuration, when `generator_type == "minecraft:noise"`.
    pub biome_source: Option<BiomeSourceDef>,
}

/// Parsed contents of `worldgen/world_preset/<name>.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldPresetDef {
    /// Map from dimension identifier (such as `"minecraft:overworld"`) to its generator definition.
    pub dimensions: IndexMap<String, DimensionDef>,
}

/// A configured cave or canyon carver, retaining its data-driven settings for compilation.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfiguredCarverDef {
    /// Registered carver type, such as `minecraft:cave` or `minecraft:canyon`.
    pub carver_type: String,
    /// The carver's configuration object.
    pub config: Value,
}

/// One block or nested block-tag reference in a data pack tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockTagEntry {
    /// Block identifier, or a tag identifier prefixed with `#`.
    pub id: String,
    /// Whether an unresolved identifier is an error.
    pub required: bool,
}

/// A block or biome tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockTagDef {
    /// Whether this tag replaces an earlier definition in a layered data pack.
    pub replace: bool,
    /// Element and nested tag references, in data-pack order.
    pub values: Vec<BlockTagEntry>,
}

/// Parsed contents of a `worldgen/structure/<name>.json` file relevant to structure placement.
#[derive(Debug, Clone, PartialEq)]
pub struct StructureDef {
    /// The structure type, such as `minecraft:desert_pyramid`.
    pub kind: String,
    /// Biome IDs and `#`-prefixed tags where the structure may generate.
    pub biomes: Vec<String>,
    /// How the structure changes the terrain around it, such as `none` or `beard_thin`.
    pub terrain_adaptation: String,
    /// The settings of a `minecraft:jigsaw` structure.
    pub jigsaw: Option<JigsawDef>,
    /// Whether a `minecraft:shipwreck` structure is on a beach (`is_beached`).
    pub is_beached: bool,
    /// The settings of a `minecraft:ocean_ruin` structure.
    pub ocean_ruin: Option<OceanRuinDef>,
}

/// The settings of a `minecraft:ocean_ruin` structure (`OceanRuinStructure`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OceanRuinDef {
    /// Whether the ruins are of the cold kind rather than warm (`biome_temp`).
    pub cold: bool,
    /// The chance of a large ruin.
    pub large_probability: f32,
    /// The chance of small ruins around a large one.
    pub cluster_probability: f32,
}

/// The settings of a `minecraft:jigsaw` structure (`JigsawStructure`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JigsawDef {
    /// The pool the first piece comes from.
    pub start_pool: String,
    /// The jigsaw of the first piece that sits at the start position, if any.
    pub start_jigsaw_name: Option<String>,
    /// How many pieces deep the structure may grow (`size`).
    pub max_depth: i32,
    /// The Y of the start position, or `None` for a height provider Harpas does not support.
    pub start_height: Option<StartHeightDef>,
    /// Whether pieces reserve room above for taller children (`use_expansion_hack`).
    pub use_expansion_hack: bool,
    /// The heightmap the first piece stands on, if any, such as `WORLD_SURFACE_WG`.
    pub project_start_to_heightmap: Option<String>,
    /// How far pieces may reach from the center horizontally and vertically.
    pub max_distance_from_center: (i32, i32),
    /// Pools the structure renames for each start (`pool_aliases`).
    pub pool_aliases: Vec<PoolAliasDef>,
    /// Blocks kept free below and above the structure (`dimension_padding`).
    pub dimension_padding: (i32, i32),
}

/// The height a jigsaw structure starts at (`HeightProvider`), for the providers jigsaw structures use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartHeightDef {
    /// `minecraft:constant`, or an anchor on its own.
    Constant(crate::surface::VerticalAnchor),
    /// `minecraft:uniform`: any height in a range, both ends included.
    Uniform {
        /// The lowest height.
        min_inclusive: crate::surface::VerticalAnchor,
        /// The highest height.
        max_inclusive: crate::surface::VerticalAnchor,
    },
}

/// A pool renaming of a jigsaw structure (`PoolAliasBinding`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolAliasDef {
    /// `minecraft:direct`: always the same pool.
    Direct {
        /// The pool name jigsaws use.
        alias: String,
        /// The pool used instead.
        target: String,
    },
    /// `minecraft:random`: one of several pools, by weight.
    Random {
        /// The pool name jigsaws use.
        alias: String,
        /// The pools to choose from, with their weights.
        targets: Vec<(String, i32)>,
    },
    /// `minecraft:random_group`: one of several groups of renamings, by weight.
    RandomGroup {
        /// The groups to choose from, with their weights.
        groups: Vec<(Vec<PoolAliasDef>, i32)>,
    },
}

/// Parsed contents of a `worldgen/template_pool/<name>.json` file (`StructureTemplatePool`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplatePoolDef {
    /// The pool to use when none of this pool's elements fit.
    pub fallback: String,
    /// The elements and their weights, in data-pack order.
    pub elements: Vec<(PoolElementDef, i32)>,
}

impl TemplatePoolDef {
    /// Each element repeated by its weight, in data-pack order (`StructureTemplatePool.templates`).
    pub fn templates(&self) -> Vec<&PoolElementDef> {
        self.elements
            .iter()
            .flat_map(|(element, weight)| std::iter::repeat_n(element, *weight as usize))
            .collect()
    }

    /// The weighted elements in a random order (`StructureTemplatePool.getShuffledTemplates`).
    pub fn shuffled_templates(
        &self,
        random: &mut impl harpas_core::random::RandomSource,
    ) -> Vec<&PoolElementDef> {
        let mut templates = self.templates();
        for size in (2..=templates.len()).rev() {
            let other = random.next_int_bounded(size as i32) as usize;
            templates.swap(size - 1, other);
        }
        templates
    }
}

/// How a pool element follows the terrain (`StructureTemplatePool.Projection`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Projection {
    /// Placed as it is.
    Rigid,
    /// Each column moved to the terrain's surface.
    TerrainMatching,
}

/// An element of a template pool (`StructurePoolElement`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolElementDef {
    /// `minecraft:single_pool_element`, or `minecraft:legacy_single_pool_element` when `legacy`.
    Single {
        /// The template's resource location.
        location: String,
        /// The processor list's resource location, or `None` when it is written inline.
        processors: Option<String>,
        /// How it follows the terrain.
        projection: Projection,
        /// Whether it is the legacy variant, which places air blocks differently.
        legacy: bool,
    },
    /// `minecraft:feature_pool_element`: a placed feature instead of a template.
    Feature {
        /// The placed feature's resource location.
        feature: String,
        /// How it follows the terrain.
        projection: Projection,
    },
    /// `minecraft:list_pool_element`: several elements placed at the same spot.
    List {
        /// The elements, which take the list's projection.
        elements: Vec<PoolElementDef>,
        /// How they follow the terrain.
        projection: Projection,
    },
    /// `minecraft:empty_pool_element`: nothing.
    Empty,
}

/// Parsed contents of a `worldgen/structure_set/<name>.json` file.
#[derive(Debug, Clone, PartialEq)]
pub struct StructureSetDef {
    /// Structures to choose from, in data-pack order.
    pub structures: Vec<StructureSetEntryDef>,
    /// Where the set's structures may start.
    pub placement: StructurePlacementDef,
}

/// One weighted structure of a structure set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructureSetEntryDef {
    /// The structure's resource location.
    pub structure: String,
    /// Relative chance of choosing this structure.
    pub weight: i32,
}

/// The chance of skipping a placement (`StructurePlacement.FrequencyReductionMethod`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FrequencyReductionMethod {
    /// `default`.
    #[default]
    Default,
    /// `legacy_type_1`, originally for pillager outposts.
    LegacyType1,
    /// `legacy_type_2`, with a fixed salt.
    LegacyType2,
    /// `legacy_type_3`, comparing a double.
    LegacyType3,
}

/// A structure set whose nearby starts forbid a placement (`StructurePlacement.ExclusionZone`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExclusionZoneDef {
    /// The other structure set's resource location.
    pub other_set: String,
    /// Chunks to search around a placement in each direction.
    pub chunk_count: i32,
}

/// Fields shared by all structure placements (`StructurePlacement`).
#[derive(Debug, Clone, PartialEq)]
pub struct StructurePlacementCommon {
    /// Added to the seed of the placement's random generators.
    pub salt: i32,
    /// Chance that a placement goes ahead.
    pub frequency: f32,
    /// How `frequency` is decided for a chunk.
    pub frequency_reduction_method: FrequencyReductionMethod,
    /// A structure set that this placement must stay away from.
    pub exclusion_zone: Option<ExclusionZoneDef>,
    /// Offset from the chunk's corner that locating a structure reports.
    pub locate_offset: [i32; 3],
}

/// How a structure set chooses its chunks.
#[derive(Debug, Clone, PartialEq)]
pub enum StructurePlacementKind {
    /// `minecraft:random_spread`: one candidate chunk in each square region.
    RandomSpread {
        /// Size of the region in chunks.
        spacing: i32,
        /// Chunks kept free between the candidates of neighboring regions.
        separation: i32,
        /// Whether candidates tend to the middle of the region.
        triangular: bool,
    },
    /// `minecraft:concentric_rings`: a fixed list of chunks around the origin.
    ConcentricRings {
        /// Distance between rings, in units of 6 chunks.
        distance: i32,
        /// Positions in the first ring.
        spread: i32,
        /// Total positions.
        count: i32,
        /// Biome IDs and `#`-prefixed tags to move each position towards.
        preferred_biomes: Vec<String>,
    },
}

/// Parsed `placement` of a structure set.
#[derive(Debug, Clone, PartialEq)]
pub struct StructurePlacementDef {
    /// Fields shared by all placements.
    pub common: StructurePlacementCommon,
    /// The placement type and its own fields.
    pub kind: StructurePlacementKind,
}

/// All worldgen registries loaded from a datapack (`26.1.zip` or directory).
#[derive(Debug, Clone, Default)]
pub struct WorldgenData {
    /// `minecraft:worldgen/noise` definitions keyed by resource location (e.g. `"minecraft:continentalness"`).
    pub noises: IndexMap<String, NoiseParameters>,
    /// `minecraft:worldgen/density_function` definitions keyed by resource location.
    pub density_functions: IndexMap<String, DensityFunctionDef>,
    /// `minecraft:worldgen/noise_settings` definitions keyed by resource location (e.g. `"minecraft:overworld"`).
    pub noise_settings: IndexMap<String, NoiseSettingsDef>,
    /// `minecraft:worldgen/biome` definitions in deterministic lexicographical order matching Java 26.1 registry IDs.
    pub biomes: IndexMap<String, BiomeDef>,
    /// `minecraft:worldgen/multi_noise_biome_source_parameter_list` definitions keyed by resource location.
    pub parameter_lists: IndexMap<String, MultiNoiseParameterListDef>,
    /// `minecraft:worldgen/world_preset` definitions keyed by resource location (e.g. `"minecraft:normal"`).
    pub world_presets: IndexMap<String, WorldPresetDef>,
    /// Configured cave and canyon carvers, keyed by resource location.
    pub configured_carvers: IndexMap<String, ConfiguredCarverDef>,
    /// Block tags, including nested tags used to select replaceable terrain.
    pub block_tags: IndexMap<String, BlockTagDef>,
    /// Biome tags, including nested tags, which select where structures generate.
    pub biome_tags: IndexMap<String, BlockTagDef>,
    /// `minecraft:worldgen/structure` definitions, as far as structure placement needs them.
    pub structures: IndexMap<String, StructureDef>,
    /// `minecraft:worldgen/structure_set` definitions in lexicographical order.
    pub structure_sets: IndexMap<String, StructureSetDef>,
    /// `minecraft:worldgen/template_pool` definitions, the pieces jigsaw structures choose from.
    pub template_pools: IndexMap<String, TemplatePoolDef>,
}

impl WorldgenData {
    /// Loads worldgen definitions from a `.zip` archive on disk (such as `.harpas/vanilla-data/26.1.zip`).
    pub fn from_zip_path(path: impl AsRef<Path>) -> Result<Self, WorldgenDataError> {
        let file = File::open(path)?;
        Self::from_zip_reader(file)
    }

    /// Loads worldgen definitions from any seekable `.zip` reader.
    pub fn from_zip_reader<R: Read + Seek>(reader: R) -> Result<Self, WorldgenDataError> {
        let mut archive = zip::ZipArchive::new(reader)?;
        let mut raw_files = BTreeMap::new();
        for idx in 0..archive.len() {
            let mut entry = archive.by_index(idx)?;
            if !entry.is_file() {
                continue;
            }
            let name = entry.name()?.replace('\\', "/");
            if !is_relevant_worldgen_json(&name) {
                continue;
            }
            let mut contents = String::new();
            entry.read_to_string(&mut contents)?;
            raw_files.insert(name, contents);
        }
        Self::from_json_entries(raw_files)
    }

    /// Loads worldgen definitions from an extracted datapack directory on disk.
    pub fn from_dir(root: impl AsRef<Path>) -> Result<Self, WorldgenDataError> {
        let root = root.as_ref();
        let mut raw_files = BTreeMap::new();
        collect_dir_json_files(root, root, &mut raw_files)?;
        Self::from_json_entries(raw_files)
    }

    /// Loads worldgen definitions from an iterator of `(relative_path, json_text)` pairs.
    ///
    /// Paths are sorted lexicographically before insertion so registry indices (especially `biomes`) are
    /// deterministic regardless of filesystem or zip entry order.
    pub fn from_json_entries<I, K, V>(entries: I) -> Result<Self, WorldgenDataError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let mut sorted: BTreeMap<String, String> = BTreeMap::new();
        for (k, v) in entries {
            let norm = k.into().replace('\\', "/");
            let trimmed = norm.trim_start_matches('/').to_string();
            sorted.insert(trimmed, v.into());
        }

        let mut data = Self::default();
        for (path, text) in sorted {
            let Some((namespace, category, rel_id)) = classify_worldgen_path(&path) else {
                continue;
            };
            let resource_id = format!("{namespace}:{rel_id}");
            let json: Value =
                serde_json::from_str(&text).map_err(|source| WorldgenDataError::Json {
                    path: path.clone(),
                    source,
                })?;

            match category {
                "noise" => {
                    let params = parse_noise_parameters(&resource_id, &json)?;
                    data.noises.insert(resource_id, params);
                }
                "density_function" => {
                    let df = parse_density_function(&resource_id, &json)?;
                    data.density_functions.insert(resource_id, df);
                }
                "noise_settings" => {
                    let ns = parse_noise_settings(&resource_id, &json)?;
                    data.noise_settings.insert(resource_id, ns);
                }
                "biome" => {
                    let biome = parse_biome(&resource_id, &json)?;
                    data.biomes.insert(resource_id, biome);
                }
                "multi_noise_biome_source_parameter_list" => {
                    let plist = parse_parameter_list(&resource_id, &json)?;
                    data.parameter_lists.insert(resource_id, plist);
                }
                "world_preset" => {
                    let preset = parse_world_preset(&resource_id, &json)?;
                    data.world_presets.insert(resource_id, preset);
                }
                "configured_carver" => {
                    let obj = json.as_object().ok_or_else(|| {
                        invalid_def(&resource_id, "configured carver must be an object")
                    })?;
                    let carver_type = req_str(obj, "type", &resource_id)?.to_string();
                    let config = req_field(obj, "config", &resource_id)?.clone();
                    if !config.is_object() {
                        return Err(invalid_def(&resource_id, "carver config must be an object"));
                    }
                    data.configured_carvers.insert(
                        resource_id,
                        ConfiguredCarverDef {
                            carver_type,
                            config,
                        },
                    );
                }
                "block_tag" => {
                    data.block_tags
                        .insert(resource_id.clone(), parse_tag(&resource_id, &json)?);
                }
                "biome_tag" => {
                    data.biome_tags
                        .insert(resource_id.clone(), parse_tag(&resource_id, &json)?);
                }
                "structure" => {
                    data.structures
                        .insert(resource_id.clone(), parse_structure(&resource_id, &json)?);
                }
                "template_pool" => {
                    data.template_pools.insert(
                        resource_id.clone(),
                        parse_template_pool(&resource_id, &json)?,
                    );
                }
                "structure_set" => {
                    data.structure_sets.insert(
                        resource_id.clone(),
                        parse_structure_set(&resource_id, &json)?,
                    );
                }
                _ => {}
            }
        }

        Ok(data)
    }

    /// Returns the numeric biome ID (`0..biomes.len()`) for `biome_name` (such as `"minecraft:plains"`).
    pub fn biome_id(&self, biome_name: &str) -> Option<u32> {
        self.biomes
            .get_index_of(biome_name)
            .and_then(|idx| u32::try_from(idx).ok())
    }

    /// Returns the biome identifier for a numeric biome ID (`0..biomes.len()`).
    pub fn biome_name(&self, biome_id: u32) -> Option<&str> {
        self.biomes
            .get_index(biome_id as usize)
            .map(|(name, _)| name.as_str())
    }

    /// Resolves a block tag and nested references to a membership table indexed by global block state ID.
    /// Unknown required entries and cyclic references are errors; optional missing entries are skipped.
    pub fn block_tag_states(&self, tag: &str) -> Result<Vec<bool>, WorldgenDataError> {
        let mut blocks = HashSet::new();
        self.collect_block_tag(
            tag.strip_prefix('#').unwrap_or(tag),
            &mut HashSet::new(),
            &mut HashSet::new(),
            &mut blocks,
        )?;
        let states = block_states();
        Ok((0..states.len())
            .map(|id| {
                states
                    .properties(id)
                    .is_some_and(|(name, _)| blocks.contains(name))
            })
            .collect())
    }

    fn collect_block_tag(
        &self,
        tag: &str,
        active: &mut HashSet<String>,
        done: &mut HashSet<String>,
        blocks: &mut HashSet<String>,
    ) -> Result<(), WorldgenDataError> {
        collect_tag(
            &self.block_tags,
            "block",
            &|name| block_states().default_state(name).is_some(),
            tag,
            active,
            done,
            blocks,
        )
    }

    /// Resolves biome IDs and `#`-prefixed tags to biome registry IDs.
    /// Unknown required entries and cyclic references are errors; optional missing entries are skipped.
    pub fn resolve_biomes(&self, selector: &[String]) -> Result<HashSet<u32>, WorldgenDataError> {
        let mut biomes = HashSet::new();
        let (mut active, mut done) = (HashSet::new(), HashSet::new());
        for entry in selector {
            if let Some(tag) = entry.strip_prefix('#') {
                collect_tag(
                    &self.biome_tags,
                    "biome",
                    &|name| self.biomes.contains_key(name),
                    tag,
                    &mut active,
                    &mut done,
                    &mut biomes,
                )?;
            } else {
                let name = resource_name(entry);
                if !self.biomes.contains_key(&name) {
                    return Err(invalid_def(&name, "unknown biome"));
                }
                biomes.insert(name);
            }
        }
        Ok(biomes
            .iter()
            .filter_map(|name| self.biome_id(name))
            .collect())
    }
}

fn collect_tag(
    tags: &IndexMap<String, BlockTagDef>,
    kind: &str,
    exists: &impl Fn(&str) -> bool,
    tag: &str,
    active: &mut HashSet<String>,
    done: &mut HashSet<String>,
    elements: &mut HashSet<String>,
) -> Result<(), WorldgenDataError> {
    let tag = resource_name(tag);
    if done.contains(&tag) {
        return Ok(());
    }
    let definition = tags
        .get(&tag)
        .ok_or_else(|| invalid_def(&tag, format!("unknown {kind} tag")))?;
    if !active.insert(tag.clone()) {
        return Err(invalid_def(&tag, format!("cyclic {kind} tag reference")));
    }
    for entry in &definition.values {
        if let Some(child) = entry.id.strip_prefix('#') {
            let child = resource_name(child);
            if !entry.required && !tags.contains_key(&child) {
                continue;
            }
            collect_tag(tags, kind, exists, &child, active, done, elements)?;
        } else {
            let name = resource_name(&entry.id);
            if !exists(&name) {
                if entry.required {
                    return Err(invalid_def(&tag, format!("unknown {kind} `{name}`")));
                }
                continue;
            }
            elements.insert(name);
        }
    }
    active.remove(&tag);
    done.insert(tag);
    Ok(())
}

fn parse_tag(context: &str, json: &Value) -> Result<BlockTagDef, WorldgenDataError> {
    let obj = json
        .as_object()
        .ok_or_else(|| invalid_def(context, "tag must be an object"))?;
    let replace = match obj.get("replace") {
        Some(value) => value
            .as_bool()
            .ok_or_else(|| invalid_def(context, "tag `replace` must be a boolean"))?,
        None => false,
    };
    let entries = obj
        .get("values")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_def(context, "tag has no values array"))?;
    let mut values = Vec::with_capacity(entries.len());
    for entry in entries {
        let (id, required) = if let Some(id) = entry.as_str() {
            (id, true)
        } else {
            let obj = entry
                .as_object()
                .ok_or_else(|| invalid_def(context, "invalid tag entry"))?;
            let id = req_str(obj, "id", context)?;
            let required = match obj.get("required") {
                Some(value) => value
                    .as_bool()
                    .ok_or_else(|| invalid_def(context, "tag `required` must be a boolean"))?,
                None => true,
            };
            (id, required)
        };
        values.push(BlockTagEntry {
            id: id.to_string(),
            required,
        });
    }
    Ok(BlockTagDef { replace, values })
}

/// A vanilla `HolderSet` of biomes: a `#`-prefixed tag, one ID or a list of IDs.
fn parse_biome_selector(context: &str, json: &Value) -> Result<Vec<String>, WorldgenDataError> {
    if let Some(entry) = json.as_str() {
        return Ok(vec![entry.to_string()]);
    }
    json.as_array()
        .ok_or_else(|| invalid_def(context, "biomes must be a string or an array"))?
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| invalid_def(context, "biome entries must be strings"))
        })
        .collect()
}

fn parse_structure(context: &str, json: &Value) -> Result<StructureDef, WorldgenDataError> {
    let biomes = json
        .get("biomes")
        .ok_or_else(|| invalid_def(context, "structure has no biomes"))?;
    Ok(StructureDef {
        kind: json
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_def(context, "structure has no type"))?
            .to_string(),
        biomes: parse_biome_selector(context, biomes)?,
        terrain_adaptation: match json.get("terrain_adaptation") {
            Some(value) => value
                .as_str()
                .ok_or_else(|| invalid_def(context, "terrain_adaptation must be a string"))?
                .to_string(),
            None => "none".to_string(),
        },
        jigsaw: match json.get("type").and_then(Value::as_str) {
            Some("minecraft:jigsaw") => Some(parse_jigsaw(context, json)?),
            _ => None,
        },
        is_beached: json
            .get("is_beached")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        ocean_ruin: match json.get("type").and_then(Value::as_str) {
            Some("minecraft:ocean_ruin") => Some(parse_ocean_ruin(context, json)?),
            _ => None,
        },
    })
}

fn parse_ocean_ruin(context: &str, json: &Value) -> Result<OceanRuinDef, WorldgenDataError> {
    let probability = |key: &str| {
        json.get(key)
            .and_then(Value::as_f64)
            .map(|value| value as f32)
            .ok_or_else(|| invalid_def(context, format!("`{key}` must be a number")))
    };
    Ok(OceanRuinDef {
        cold: match json.get("biome_temp").and_then(Value::as_str) {
            Some("cold") => true,
            Some("warm") => false,
            _ => return Err(invalid_def(context, "biome_temp must be cold or warm")),
        },
        large_probability: probability("large_probability")?,
        cluster_probability: probability("cluster_probability")?,
    })
}

fn parse_jigsaw(context: &str, json: &Value) -> Result<JigsawDef, WorldgenDataError> {
    let integer = |value: Option<&Value>, what: &str| {
        value
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .ok_or_else(|| invalid_def(context, format!("`{what}` must be a 32-bit integer")))
    };
    let start_height = json
        .get("start_height")
        .ok_or_else(|| invalid_def(context, "jigsaw has no start_height"))?;
    let anchor = |key: &str| {
        crate::surface::parse_vertical_anchor(
            context,
            start_height
                .get(key)
                .ok_or_else(|| invalid_def(context, format!("height provider has no `{key}`")))?,
        )
    };
    let start_height = match start_height.get("type").and_then(Value::as_str) {
        None => Some(StartHeightDef::Constant(
            crate::surface::parse_vertical_anchor(context, start_height)?,
        )),
        Some("minecraft:constant") => Some(StartHeightDef::Constant(anchor("value")?)),
        Some("minecraft:uniform") => Some(StartHeightDef::Uniform {
            min_inclusive: anchor("min_inclusive")?,
            max_inclusive: anchor("max_inclusive")?,
        }),
        Some(_) => None,
    };
    let max_distance = json
        .get("max_distance_from_center")
        .ok_or_else(|| invalid_def(context, "jigsaw has no max_distance_from_center"))?;
    let max_distance_from_center = if max_distance.is_object() {
        let horizontal = integer(max_distance.get("horizontal"), "horizontal")?;
        let vertical = match max_distance.get("vertical") {
            Some(value) => integer(Some(value), "vertical")?,
            None => 4064,
        };
        (horizontal, vertical)
    } else {
        let value = integer(Some(max_distance), "max_distance_from_center")?;
        (value, value)
    };
    let dimension_padding = match json.get("dimension_padding") {
        None => (0, 0),
        Some(padding) if padding.is_object() => (
            padding
                .get("bottom")
                .map_or(Ok(0), |value| integer(Some(value), "bottom"))?,
            padding
                .get("top")
                .map_or(Ok(0), |value| integer(Some(value), "top"))?,
        ),
        Some(padding) => {
            let value = integer(Some(padding), "dimension_padding")?;
            (value, value)
        }
    };
    Ok(JigsawDef {
        start_pool: json
            .get("start_pool")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_def(context, "jigsaw has no start_pool"))?
            .to_string(),
        start_jigsaw_name: json
            .get("start_jigsaw_name")
            .and_then(Value::as_str)
            .map(str::to_string),
        max_depth: integer(json.get("size"), "size")?,
        start_height,
        use_expansion_hack: json
            .get("use_expansion_hack")
            .and_then(Value::as_bool)
            .ok_or_else(|| invalid_def(context, "jigsaw has no use_expansion_hack"))?,
        project_start_to_heightmap: json
            .get("project_start_to_heightmap")
            .and_then(Value::as_str)
            .map(str::to_string),
        max_distance_from_center,
        pool_aliases: match json.get("pool_aliases") {
            Some(aliases) => aliases
                .as_array()
                .ok_or_else(|| invalid_def(context, "pool_aliases must be an array"))?
                .iter()
                .map(|alias| parse_pool_alias(context, alias))
                .collect::<Result<_, _>>()?,
            None => Vec::new(),
        },
        dimension_padding,
    })
}

fn parse_pool_alias(context: &str, json: &Value) -> Result<PoolAliasDef, WorldgenDataError> {
    let string = |value: Option<&Value>, what: &str| {
        value
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| invalid_def(context, format!("pool alias has no `{what}`")))
    };
    let weighted = |key: &str| {
        json.get(key)
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_def(context, format!("pool alias has no `{key}` array")))
    };
    let weight = |entry: &Value| {
        entry
            .get("weight")
            .and_then(Value::as_i64)
            .and_then(|weight| i32::try_from(weight).ok())
            .ok_or_else(|| invalid_def(context, "pool alias entry has no weight"))
    };
    Ok(match json.get("type").and_then(Value::as_str) {
        Some("minecraft:direct") => PoolAliasDef::Direct {
            alias: string(json.get("alias"), "alias")?,
            target: string(json.get("target"), "target")?,
        },
        Some("minecraft:random") => PoolAliasDef::Random {
            alias: string(json.get("alias"), "alias")?,
            targets: weighted("targets")?
                .iter()
                .map(|entry| Ok((string(entry.get("data"), "data")?, weight(entry)?)))
                .collect::<Result<_, WorldgenDataError>>()?,
        },
        Some("minecraft:random_group") => PoolAliasDef::RandomGroup {
            groups: weighted("groups")?
                .iter()
                .map(|entry| {
                    let bindings = entry
                        .get("data")
                        .and_then(Value::as_array)
                        .ok_or_else(|| invalid_def(context, "pool alias group has no data"))?
                        .iter()
                        .map(|binding| parse_pool_alias(context, binding))
                        .collect::<Result<_, _>>()?;
                    Ok((bindings, weight(entry)?))
                })
                .collect::<Result<_, WorldgenDataError>>()?,
        },
        _ => return Err(invalid_def(context, "unknown pool alias type")),
    })
}

fn parse_template_pool(context: &str, json: &Value) -> Result<TemplatePoolDef, WorldgenDataError> {
    let fallback = json
        .get("fallback")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_def(context, "template pool has no fallback"))?
        .to_string();
    let elements = json
        .get("elements")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_def(context, "template pool has no elements"))?
        .iter()
        .map(|entry| {
            let weight = entry
                .get("weight")
                .and_then(Value::as_i64)
                .filter(|weight| (1..=150).contains(weight))
                .ok_or_else(|| invalid_def(context, "pool element weight must be from 1 to 150"))?;
            let element = entry
                .get("element")
                .ok_or_else(|| invalid_def(context, "pool entry has no element"))?;
            Ok((parse_pool_element(context, element)?, weight as i32))
        })
        .collect::<Result<_, WorldgenDataError>>()?;
    Ok(TemplatePoolDef { fallback, elements })
}

fn parse_pool_element(context: &str, json: &Value) -> Result<PoolElementDef, WorldgenDataError> {
    let projection = || match json.get("projection").and_then(Value::as_str) {
        Some("rigid") => Ok(Projection::Rigid),
        Some("terrain_matching") => Ok(Projection::TerrainMatching),
        _ => Err(invalid_def(context, "pool element has no valid projection")),
    };
    let string = |key: &str| {
        json.get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| invalid_def(context, format!("pool element has no `{key}`")))
    };
    let element_type = json.get("element_type").and_then(Value::as_str);
    Ok(match element_type {
        Some(kind @ ("minecraft:single_pool_element" | "minecraft:legacy_single_pool_element")) => {
            PoolElementDef::Single {
                location: string("location")?,
                processors: json
                    .get("processors")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                projection: projection()?,
                legacy: kind == "minecraft:legacy_single_pool_element",
            }
        }
        Some("minecraft:feature_pool_element") => PoolElementDef::Feature {
            feature: string("feature")?,
            projection: projection()?,
        },
        Some("minecraft:list_pool_element") => {
            let projection = projection()?;
            let elements = json
                .get("elements")
                .and_then(Value::as_array)
                .filter(|elements| !elements.is_empty())
                .ok_or_else(|| invalid_def(context, "list pool element has no elements"))?
                .iter()
                .map(|element| {
                    let mut element = parse_pool_element(context, element)?;
                    element.set_projection(projection);
                    Ok(element)
                })
                .collect::<Result<_, WorldgenDataError>>()?;
            PoolElementDef::List {
                elements,
                projection,
            }
        }
        Some("minecraft:empty_pool_element") => PoolElementDef::Empty,
        _ => return Err(invalid_def(context, "unknown pool element type")),
    })
}

impl PoolElementDef {
    /// `StructurePoolElement.setProjection`, which a list passes on to its elements.
    fn set_projection(&mut self, new: Projection) {
        match self {
            Self::Single { projection, .. } | Self::Feature { projection, .. } => *projection = new,
            Self::List {
                elements,
                projection,
            } => {
                *projection = new;
                for element in elements {
                    element.set_projection(new);
                }
            }
            Self::Empty => {}
        }
    }
}

fn parse_structure_set(context: &str, json: &Value) -> Result<StructureSetDef, WorldgenDataError> {
    let integer = |obj: &Value, key: &str| -> Result<i32, WorldgenDataError> {
        obj.get(key)
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .ok_or_else(|| invalid_def(context, format!("`{key}` must be a 32-bit integer")))
    };
    let structures = json
        .get("structures")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_def(context, "structure set has no structures array"))?
        .iter()
        .map(|entry| {
            Ok(StructureSetEntryDef {
                structure: entry
                    .get("structure")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid_def(context, "structure entry has no structure"))?
                    .to_string(),
                weight: integer(entry, "weight")?,
            })
        })
        .collect::<Result<_, WorldgenDataError>>()?;
    let placement = json
        .get("placement")
        .ok_or_else(|| invalid_def(context, "structure set has no placement"))?;
    let frequency_reduction_method = match placement
        .get("frequency_reduction_method")
        .map(Value::as_str)
    {
        None | Some(Some("default")) => FrequencyReductionMethod::Default,
        Some(Some("legacy_type_1")) => FrequencyReductionMethod::LegacyType1,
        Some(Some("legacy_type_2")) => FrequencyReductionMethod::LegacyType2,
        Some(Some("legacy_type_3")) => FrequencyReductionMethod::LegacyType3,
        Some(_) => return Err(invalid_def(context, "unknown frequency reduction method")),
    };
    let exclusion_zone = match placement.get("exclusion_zone") {
        Some(zone) => Some(ExclusionZoneDef {
            other_set: zone
                .get("other_set")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_def(context, "exclusion zone has no other_set"))?
                .to_string(),
            chunk_count: integer(zone, "chunk_count")?,
        }),
        None => None,
    };
    let locate_offset = match placement.get("locate_offset") {
        Some(offset) => {
            let offset: Vec<_> = offset
                .as_array()
                .ok_or_else(|| invalid_def(context, "locate_offset must be an array"))?
                .iter()
                .filter_map(|value| value.as_i64().and_then(|value| i32::try_from(value).ok()))
                .collect();
            <[i32; 3]>::try_from(offset)
                .map_err(|_| invalid_def(context, "locate_offset must have 3 integers"))?
        }
        None => [0, 0, 0],
    };
    let common = StructurePlacementCommon {
        salt: integer(placement, "salt")?,
        frequency: match placement.get("frequency") {
            Some(value) => value
                .as_f64()
                .ok_or_else(|| invalid_def(context, "frequency must be a number"))?
                as f32,
            None => 1.0,
        },
        frequency_reduction_method,
        exclusion_zone,
        locate_offset,
    };
    let kind = match placement.get("type").and_then(Value::as_str) {
        Some("minecraft:random_spread") => StructurePlacementKind::RandomSpread {
            spacing: integer(placement, "spacing")?,
            separation: integer(placement, "separation")?,
            triangular: match placement.get("spread_type").map(Value::as_str) {
                None | Some(Some("linear")) => false,
                Some(Some("triangular")) => true,
                Some(_) => return Err(invalid_def(context, "unknown spread type")),
            },
        },
        Some("minecraft:concentric_rings") => StructurePlacementKind::ConcentricRings {
            distance: integer(placement, "distance")?,
            spread: integer(placement, "spread")?,
            count: integer(placement, "count")?,
            preferred_biomes: parse_biome_selector(
                context,
                placement
                    .get("preferred_biomes")
                    .ok_or_else(|| invalid_def(context, "rings have no preferred_biomes"))?,
            )?,
        },
        _ => return Err(invalid_def(context, "unknown structure placement type")),
    };
    Ok(StructureSetDef {
        structures,
        placement: StructurePlacementDef { common, kind },
    })
}

fn resource_name(name: &str) -> String {
    if name.contains(':') {
        name.to_string()
    } else {
        format!("minecraft:{name}")
    }
}

fn is_relevant_worldgen_json(path: &str) -> bool {
    classify_worldgen_path(path).is_some()
}

fn classify_worldgen_path(path: &str) -> Option<(&str, &str, &str)> {
    let rest = path.strip_prefix("data/")?;
    let (namespace, after_ns) = rest.split_once('/')?;
    if let Some(rel_id) = after_ns
        .strip_prefix("tags/block/")
        .and_then(|path| path.strip_suffix(".json"))
    {
        return Some((namespace, "block_tag", rel_id));
    }
    if let Some(rel_id) = after_ns
        .strip_prefix("tags/worldgen/biome/")
        .and_then(|path| path.strip_suffix(".json"))
    {
        return Some((namespace, "biome_tag", rel_id));
    }
    let after_worldgen = after_ns.strip_prefix("worldgen/")?;
    let (category, file_part) = after_worldgen.split_once('/')?;
    let rel_id = file_part.strip_suffix(".json")?;
    match category {
        "noise"
        | "density_function"
        | "noise_settings"
        | "biome"
        | "multi_noise_biome_source_parameter_list"
        | "configured_carver"
        | "structure"
        | "structure_set"
        | "template_pool"
        | "world_preset" => Some((namespace, category, rel_id)),
        _ => None,
    }
}

fn collect_dir_json_files(
    root: &Path,
    current: &Path,
    out: &mut BTreeMap<String, String>,
) -> Result<(), WorldgenDataError> {
    if !current.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_dir_json_files(root, &path, out)?;
        } else if path.extension().and_then(|s| s.to_str()) == Some("json") {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if is_relevant_worldgen_json(&rel) {
                let text = fs::read_to_string(&path)?;
                out.insert(rel, text);
            }
        }
    }
    Ok(())
}

fn invalid_def(context: &str, message: impl Into<String>) -> WorldgenDataError {
    WorldgenDataError::InvalidDefinition {
        context: context.to_string(),
        message: message.into(),
    }
}

fn req_field<'a>(
    obj: &'a serde_json::Map<String, Value>,
    key: &str,
    ctx: &str,
) -> Result<&'a Value, WorldgenDataError> {
    obj.get(key)
        .ok_or_else(|| invalid_def(ctx, format!("missing required field `{key}`")))
}

fn req_f64(
    obj: &serde_json::Map<String, Value>,
    key: &str,
    ctx: &str,
) -> Result<f64, WorldgenDataError> {
    req_field(obj, key, ctx)?.as_f64().ok_or_else(|| {
        invalid_def(
            ctx,
            format!("field `{key}` must be a floating-point number"),
        )
    })
}

fn req_i32(
    obj: &serde_json::Map<String, Value>,
    key: &str,
    ctx: &str,
) -> Result<i32, WorldgenDataError> {
    let val = req_field(obj, key, ctx)?
        .as_i64()
        .ok_or_else(|| invalid_def(ctx, format!("field `{key}` must be an integer")))?;
    i32::try_from(val).map_err(|_| invalid_def(ctx, format!("field `{key}` out of i32 range")))
}

fn req_bool(
    obj: &serde_json::Map<String, Value>,
    key: &str,
    ctx: &str,
) -> Result<bool, WorldgenDataError> {
    req_field(obj, key, ctx)?
        .as_bool()
        .ok_or_else(|| invalid_def(ctx, format!("field `{key}` must be a boolean")))
}

fn req_str<'a>(
    obj: &'a serde_json::Map<String, Value>,
    key: &str,
    ctx: &str,
) -> Result<&'a str, WorldgenDataError> {
    req_field(obj, key, ctx)?
        .as_str()
        .ok_or_else(|| invalid_def(ctx, format!("field `{key}` must be a string")))
}

fn parse_noise_parameters(ctx: &str, json: &Value) -> Result<NoiseParameters, WorldgenDataError> {
    let obj = json
        .as_object()
        .ok_or_else(|| invalid_def(ctx, "expected JSON object for noise parameters"))?;
    let first_octave = req_i32(obj, "firstOctave", ctx)?;
    let amps_arr = req_field(obj, "amplitudes", ctx)?
        .as_array()
        .ok_or_else(|| invalid_def(ctx, "`amplitudes` must be an array"))?;
    if amps_arr.is_empty() {
        return Err(invalid_def(ctx, "`amplitudes` must not be empty"));
    }
    let mut amplitudes = Vec::with_capacity(amps_arr.len());
    for (idx, val) in amps_arr.iter().enumerate() {
        let amp = val
            .as_f64()
            .ok_or_else(|| invalid_def(ctx, format!("amplitude[{idx}] must be a number")))?;
        amplitudes.push(amp);
    }
    Ok(NoiseParameters::new(first_octave, amplitudes))
}

/// Parses a `DensityFunctionDef` from a `serde_json::Value`.
pub fn parse_density_function(
    ctx: &str,
    json: &Value,
) -> Result<DensityFunctionDef, WorldgenDataError> {
    match json {
        Value::Number(num) => {
            let val = num
                .as_f64()
                .ok_or_else(|| invalid_def(ctx, "constant density function must be f64"))?;
            Ok(DensityFunctionDef::Constant(val))
        }
        Value::String(reference) => Ok(DensityFunctionDef::Reference(reference.clone())),
        Value::Object(obj) => {
            let ty = req_str(obj, "type", ctx)?;
            match ty {
                "minecraft:constant" => {
                    Ok(DensityFunctionDef::Constant(req_f64(obj, "argument", ctx)?))
                }
                "minecraft:abs" => Ok(DensityFunctionDef::Unary {
                    op: UnaryOp::Abs,
                    argument: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "argument", ctx)?,
                    )?),
                }),
                "minecraft:square" => Ok(DensityFunctionDef::Unary {
                    op: UnaryOp::Square,
                    argument: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "argument", ctx)?,
                    )?),
                }),
                "minecraft:cube" => Ok(DensityFunctionDef::Unary {
                    op: UnaryOp::Cube,
                    argument: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "argument", ctx)?,
                    )?),
                }),
                "minecraft:half_negative" => Ok(DensityFunctionDef::Unary {
                    op: UnaryOp::HalfNegative,
                    argument: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "argument", ctx)?,
                    )?),
                }),
                "minecraft:quarter_negative" => Ok(DensityFunctionDef::Unary {
                    op: UnaryOp::QuarterNegative,
                    argument: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "argument", ctx)?,
                    )?),
                }),
                "minecraft:squeeze" => Ok(DensityFunctionDef::Unary {
                    op: UnaryOp::Squeeze,
                    argument: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "argument", ctx)?,
                    )?),
                }),
                "minecraft:invert" => Ok(DensityFunctionDef::Unary {
                    op: UnaryOp::Invert,
                    argument: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "argument", ctx)?,
                    )?),
                }),
                "minecraft:add" => Ok(DensityFunctionDef::Binary {
                    op: BinaryOp::Add,
                    argument1: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "argument1", ctx)?,
                    )?),
                    argument2: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "argument2", ctx)?,
                    )?),
                }),
                "minecraft:mul" => Ok(DensityFunctionDef::Binary {
                    op: BinaryOp::Mul,
                    argument1: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "argument1", ctx)?,
                    )?),
                    argument2: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "argument2", ctx)?,
                    )?),
                }),
                "minecraft:min" => Ok(DensityFunctionDef::Binary {
                    op: BinaryOp::Min,
                    argument1: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "argument1", ctx)?,
                    )?),
                    argument2: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "argument2", ctx)?,
                    )?),
                }),
                "minecraft:max" => Ok(DensityFunctionDef::Binary {
                    op: BinaryOp::Max,
                    argument1: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "argument1", ctx)?,
                    )?),
                    argument2: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "argument2", ctx)?,
                    )?),
                }),
                "minecraft:interpolated" => Ok(DensityFunctionDef::Marker {
                    kind: MarkerKind::Interpolated,
                    argument: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "argument", ctx)?,
                    )?),
                }),
                "minecraft:flat_cache" => Ok(DensityFunctionDef::Marker {
                    kind: MarkerKind::FlatCache,
                    argument: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "argument", ctx)?,
                    )?),
                }),
                "minecraft:cache_2d" => Ok(DensityFunctionDef::Marker {
                    kind: MarkerKind::Cache2D,
                    argument: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "argument", ctx)?,
                    )?),
                }),
                "minecraft:cache_once" => Ok(DensityFunctionDef::Marker {
                    kind: MarkerKind::CacheOnce,
                    argument: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "argument", ctx)?,
                    )?),
                }),
                "minecraft:cache_all_in_cell" => Ok(DensityFunctionDef::Marker {
                    kind: MarkerKind::CacheAllInCell,
                    argument: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "argument", ctx)?,
                    )?),
                }),
                "minecraft:clamp" => Ok(DensityFunctionDef::Clamp {
                    input: Box::new(parse_density_function(ctx, req_field(obj, "input", ctx)?)?),
                    min: req_f64(obj, "min", ctx)?,
                    max: req_f64(obj, "max", ctx)?,
                }),
                "minecraft:y_clamped_gradient" => Ok(DensityFunctionDef::YClampedGradient {
                    from_y: req_i32(obj, "from_y", ctx)?,
                    to_y: req_i32(obj, "to_y", ctx)?,
                    from_value: req_f64(obj, "from_value", ctx)?,
                    to_value: req_f64(obj, "to_value", ctx)?,
                }),
                "minecraft:range_choice" => Ok(DensityFunctionDef::RangeChoice {
                    input: Box::new(parse_density_function(ctx, req_field(obj, "input", ctx)?)?),
                    min_inclusive: req_f64(obj, "min_inclusive", ctx)?,
                    max_exclusive: req_f64(obj, "max_exclusive", ctx)?,
                    when_in_range: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "when_in_range", ctx)?,
                    )?),
                    when_out_of_range: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "when_out_of_range", ctx)?,
                    )?),
                }),
                "minecraft:noise" => Ok(DensityFunctionDef::Noise {
                    noise: req_str(obj, "noise", ctx)?.to_string(),
                    xz_scale: req_f64(obj, "xz_scale", ctx)?,
                    y_scale: req_f64(obj, "y_scale", ctx)?,
                }),
                "minecraft:shifted_noise" => Ok(DensityFunctionDef::ShiftedNoise {
                    noise: req_str(obj, "noise", ctx)?.to_string(),
                    xz_scale: req_f64(obj, "xz_scale", ctx)?,
                    y_scale: req_f64(obj, "y_scale", ctx)?,
                    shift_x: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "shift_x", ctx)?,
                    )?),
                    shift_y: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "shift_y", ctx)?,
                    )?),
                    shift_z: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "shift_z", ctx)?,
                    )?),
                }),
                "minecraft:shift_a" => Ok(DensityFunctionDef::ShiftA {
                    argument: req_str(obj, "argument", ctx)?.to_string(),
                }),
                "minecraft:shift_b" => Ok(DensityFunctionDef::ShiftB {
                    argument: req_str(obj, "argument", ctx)?.to_string(),
                }),
                "minecraft:shift" => Ok(DensityFunctionDef::Shift {
                    argument: req_str(obj, "argument", ctx)?.to_string(),
                }),
                "minecraft:weird_scaled_sampler" => {
                    let mapper = match req_str(obj, "rarity_value_mapper", ctx)? {
                        "type_1" => RarityValueMapper::Type1,
                        "type_2" => RarityValueMapper::Type2,
                        other => {
                            return Err(invalid_def(
                                ctx,
                                format!("unknown rarity_value_mapper `{other}`"),
                            ));
                        }
                    };
                    Ok(DensityFunctionDef::WeirdScaledSampler {
                        input: Box::new(parse_density_function(
                            ctx,
                            req_field(obj, "input", ctx)?,
                        )?),
                        noise: req_str(obj, "noise", ctx)?.to_string(),
                        rarity_value_mapper: mapper,
                    })
                }
                "minecraft:old_blended_noise" => Ok(DensityFunctionDef::OldBlendedNoise {
                    xz_scale: req_f64(obj, "xz_scale", ctx)?,
                    y_scale: req_f64(obj, "y_scale", ctx)?,
                    xz_factor: req_f64(obj, "xz_factor", ctx)?,
                    y_factor: req_f64(obj, "y_factor", ctx)?,
                    smear_scale_multiplier: req_f64(obj, "smear_scale_multiplier", ctx)?,
                }),
                "minecraft:end_islands" => Ok(DensityFunctionDef::EndIslands),
                "minecraft:find_top_surface" => Ok(DensityFunctionDef::FindTopSurface {
                    density: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "density", ctx)?,
                    )?),
                    upper_bound: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "upper_bound", ctx)?,
                    )?),
                    lower_bound: req_i32(obj, "lower_bound", ctx)?,
                    cell_height: req_i32(obj, "cell_height", ctx)?,
                }),
                "minecraft:spline" => {
                    let spline_val = req_field(obj, "spline", ctx)?;
                    Ok(DensityFunctionDef::Spline(parse_cubic_spline(
                        ctx, spline_val,
                    )?))
                }
                "minecraft:blend_alpha" => Ok(DensityFunctionDef::BlendAlpha),
                "minecraft:blend_offset" => Ok(DensityFunctionDef::BlendOffset),
                "minecraft:blend_density" => Ok(DensityFunctionDef::BlendDensity {
                    argument: Box::new(parse_density_function(
                        ctx,
                        req_field(obj, "argument", ctx)?,
                    )?),
                }),
                other => Err(invalid_def(
                    ctx,
                    format!("unsupported density function type `{other}`"),
                )),
            }
        }
        _ => Err(invalid_def(
            ctx,
            "density function must be a number, string reference, or typed object",
        )),
    }
}

fn parse_cubic_spline(ctx: &str, json: &Value) -> Result<CubicSplineDef, WorldgenDataError> {
    match json {
        Value::Number(num) => {
            let val = num
                .as_f64()
                .ok_or_else(|| invalid_def(ctx, "spline constant must be a number"))?
                as f32;
            Ok(CubicSplineDef::Constant(val))
        }
        Value::Object(obj) => {
            let coordinate = Box::new(parse_density_function(
                ctx,
                req_field(obj, "coordinate", ctx)?,
            )?);
            let pts_arr = req_field(obj, "points", ctx)?
                .as_array()
                .ok_or_else(|| invalid_def(ctx, "spline `points` must be an array"))?;
            if pts_arr.is_empty() {
                return Err(invalid_def(ctx, "spline `points` must not be empty"));
            }
            let mut points = Vec::with_capacity(pts_arr.len());
            for (idx, pt_val) in pts_arr.iter().enumerate() {
                let pt_obj = pt_val.as_object().ok_or_else(|| {
                    invalid_def(ctx, format!("spline point[{idx}] must be an object"))
                })?;
                let location = req_f64(pt_obj, "location", ctx)? as f32;
                let derivative = pt_obj
                    .get("derivative")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0) as f32;
                let value = parse_cubic_spline(ctx, req_field(pt_obj, "value", ctx)?)?;
                points.push(SplinePointDef {
                    location,
                    value,
                    derivative,
                });
            }
            Ok(CubicSplineDef::Multipoint { coordinate, points })
        }
        _ => Err(invalid_def(
            ctx,
            "cubic spline must be a float number or multipoint object",
        )),
    }
}

fn parse_block_state_def(ctx: &str, json: &Value) -> Result<BlockStateDef, WorldgenDataError> {
    let obj = json
        .as_object()
        .ok_or_else(|| invalid_def(ctx, "block state definition must be an object"))?;
    let name = req_str(obj, "Name", ctx)?.to_string();
    let mut properties = BTreeMap::new();
    if let Some(props_val) = obj.get("Properties") {
        let props_obj = props_val
            .as_object()
            .ok_or_else(|| invalid_def(ctx, "`Properties` must be an object"))?;
        for (k, v) in props_obj {
            let val_str = v
                .as_str()
                .ok_or_else(|| invalid_def(ctx, format!("property `{k}` must be a string")))?;
            properties.insert(k.clone(), val_str.to_string());
        }
    }
    Ok(BlockStateDef { name, properties })
}

fn parse_noise_router(ctx: &str, json: &Value) -> Result<NoiseRouterDef, WorldgenDataError> {
    let obj = json
        .as_object()
        .ok_or_else(|| invalid_def(ctx, "`noise_router` must be an object"))?;
    Ok(NoiseRouterDef {
        barrier_noise: parse_density_function(ctx, req_field(obj, "barrier", ctx)?)?,
        fluid_level_floodedness_noise: parse_density_function(
            ctx,
            req_field(obj, "fluid_level_floodedness", ctx)?,
        )?,
        fluid_level_spread_noise: parse_density_function(
            ctx,
            req_field(obj, "fluid_level_spread", ctx)?,
        )?,
        lava_noise: parse_density_function(ctx, req_field(obj, "lava", ctx)?)?,
        temperature: parse_density_function(ctx, req_field(obj, "temperature", ctx)?)?,
        vegetation: parse_density_function(ctx, req_field(obj, "vegetation", ctx)?)?,
        continents: parse_density_function(ctx, req_field(obj, "continents", ctx)?)?,
        erosion: parse_density_function(ctx, req_field(obj, "erosion", ctx)?)?,
        depth: parse_density_function(ctx, req_field(obj, "depth", ctx)?)?,
        ridges: parse_density_function(ctx, req_field(obj, "ridges", ctx)?)?,
        preliminary_surface_level: parse_density_function(
            ctx,
            req_field(obj, "preliminary_surface_level", ctx)?,
        )?,
        final_density: parse_density_function(ctx, req_field(obj, "final_density", ctx)?)?,
        vein_toggle: parse_density_function(ctx, req_field(obj, "vein_toggle", ctx)?)?,
        vein_ridged: parse_density_function(ctx, req_field(obj, "vein_ridged", ctx)?)?,
        vein_gap: parse_density_function(ctx, req_field(obj, "vein_gap", ctx)?)?,
    })
}

fn parse_parameter(ctx: &str, json: &Value) -> Result<Parameter, WorldgenDataError> {
    if let Some(point) = json.as_f64() {
        return Ok(Parameter::point(point as f32));
    }
    if let Some(arr) = json.as_array()
        && arr.len() == 2
    {
        let min = arr[0]
            .as_f64()
            .ok_or_else(|| invalid_def(ctx, "parameter min must be a number"))?
            as f32;
        let max = arr[1]
            .as_f64()
            .ok_or_else(|| invalid_def(ctx, "parameter max must be a number"))?
            as f32;
        return Ok(Parameter::span(min, max));
    }
    Err(invalid_def(
        ctx,
        "climate parameter must be a float or [min, max] array",
    ))
}

fn parse_parameter_point(ctx: &str, json: &Value) -> Result<ParameterPoint, WorldgenDataError> {
    let obj = json
        .as_object()
        .ok_or_else(|| invalid_def(ctx, "parameter point must be an object"))?;
    let temperature = parse_parameter(ctx, req_field(obj, "temperature", ctx)?)?;
    let humidity = parse_parameter(ctx, req_field(obj, "humidity", ctx)?)?;
    let continentalness = parse_parameter(ctx, req_field(obj, "continentalness", ctx)?)?;
    let erosion = parse_parameter(ctx, req_field(obj, "erosion", ctx)?)?;
    let depth = parse_parameter(ctx, req_field(obj, "depth", ctx)?)?;
    let weirdness = parse_parameter(ctx, req_field(obj, "weirdness", ctx)?)?;
    let offset_f32 = req_f64(obj, "offset", ctx)? as f32;
    Ok(ParameterPoint {
        temperature,
        humidity,
        continentalness,
        erosion,
        depth,
        weirdness,
        offset: quantize_coord(offset_f32),
    })
}

fn parse_noise_settings(ctx: &str, json: &Value) -> Result<NoiseSettingsDef, WorldgenDataError> {
    let obj = json
        .as_object()
        .ok_or_else(|| invalid_def(ctx, "noise_settings must be an object"))?;
    let noise_obj = req_field(obj, "noise", ctx)?
        .as_object()
        .ok_or_else(|| invalid_def(ctx, "`noise` must be an object"))?;
    let noise = NoiseSamplingSettings {
        min_y: req_i32(noise_obj, "min_y", ctx)?,
        height: req_i32(noise_obj, "height", ctx)?,
        size_horizontal: req_i32(noise_obj, "size_horizontal", ctx)?,
        size_vertical: req_i32(noise_obj, "size_vertical", ctx)?,
    };
    let default_block = parse_block_state_def(ctx, req_field(obj, "default_block", ctx)?)?;
    let default_fluid = parse_block_state_def(ctx, req_field(obj, "default_fluid", ctx)?)?;
    let noise_router = parse_noise_router(ctx, req_field(obj, "noise_router", ctx)?)?;
    let surface_rule = req_field(obj, "surface_rule", ctx)?.clone();

    let mut spawn_target = Vec::new();
    if let Some(spawn_arr) = obj.get("spawn_target").and_then(Value::as_array) {
        for item in spawn_arr {
            spawn_target.push(parse_parameter_point(ctx, item)?);
        }
    }

    Ok(NoiseSettingsDef {
        noise,
        default_block,
        default_fluid,
        noise_router,
        surface_rule,
        spawn_target,
        sea_level: req_i32(obj, "sea_level", ctx)?,
        disable_mob_generation: req_bool(obj, "disable_mob_generation", ctx)?,
        aquifers_enabled: req_bool(obj, "aquifers_enabled", ctx)?,
        ore_veins_enabled: req_bool(obj, "ore_veins_enabled", ctx)?,
        legacy_random_source: req_bool(obj, "legacy_random_source", ctx)?,
    })
}

fn parse_biome(ctx: &str, json: &Value) -> Result<BiomeDef, WorldgenDataError> {
    let obj = json
        .as_object()
        .ok_or_else(|| invalid_def(ctx, "biome definition must be an object"))?;
    let has_precipitation = req_bool(obj, "has_precipitation", ctx)?;
    let temperature = req_f64(obj, "temperature", ctx)? as f32;
    let downfall = req_f64(obj, "downfall", ctx)? as f32;
    let temperature_modifier = match obj.get("temperature_modifier").and_then(Value::as_str) {
        None | Some("none") => TemperatureModifier::None,
        Some("frozen") => TemperatureModifier::Frozen,
        Some(other) => {
            return Err(invalid_def(
                ctx,
                format!("unknown temperature_modifier `{other}`"),
            ));
        }
    };

    let mut carvers = Vec::new();
    if let Some(carvers_val) = obj.get("carvers") {
        match carvers_val {
            Value::String(single) => carvers.push(single.clone()),
            Value::Array(arr) => {
                for item in arr {
                    if let Some(s) = item.as_str() {
                        carvers.push(s.to_string());
                    }
                }
            }
            Value::Object(map) => {
                if let Some(air) = map.get("air") {
                    match air {
                        Value::String(single) => carvers.push(single.clone()),
                        Value::Array(arr) => {
                            for item in arr {
                                if let Some(s) = item.as_str() {
                                    carvers.push(s.to_string());
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    Ok(BiomeDef {
        has_precipitation,
        temperature,
        temperature_modifier,
        downfall,
        carvers,
    })
}

fn parse_parameter_list(
    ctx: &str,
    json: &Value,
) -> Result<MultiNoiseParameterListDef, WorldgenDataError> {
    let obj = json
        .as_object()
        .ok_or_else(|| invalid_def(ctx, "parameter list must be an object"))?;
    if let Some(preset) = obj.get("preset").and_then(Value::as_str) {
        return Ok(MultiNoiseParameterListDef::Preset(preset.to_string()));
    }
    if let Some(biomes_arr) = obj.get("biomes").and_then(Value::as_array) {
        let mut entries = Vec::with_capacity(biomes_arr.len());
        for item in biomes_arr {
            let entry_obj = item
                .as_object()
                .ok_or_else(|| invalid_def(ctx, "biome parameter entry must be an object"))?;
            let biome = req_str(entry_obj, "biome", ctx)?.to_string();
            let parameters = parse_parameter_point(ctx, req_field(entry_obj, "parameters", ctx)?)?;
            entries.push(BiomeParameterEntryDef { biome, parameters });
        }
        return Ok(MultiNoiseParameterListDef::Biomes(entries));
    }
    Err(invalid_def(
        ctx,
        "multi_noise parameter list must contain `preset` or `biomes`",
    ))
}

fn parse_world_preset(ctx: &str, json: &Value) -> Result<WorldPresetDef, WorldgenDataError> {
    let obj = json
        .as_object()
        .ok_or_else(|| invalid_def(ctx, "world_preset must be an object"))?;
    let dims_obj = req_field(obj, "dimensions", ctx)?
        .as_object()
        .ok_or_else(|| invalid_def(ctx, "`dimensions` must be an object"))?;
    let mut dimensions = IndexMap::with_capacity(dims_obj.len());
    for (dim_id, dim_val) in dims_obj {
        let dim_obj = dim_val
            .as_object()
            .ok_or_else(|| invalid_def(ctx, "dimension entry must be an object"))?;
        let dimension_type = req_str(dim_obj, "type", ctx)?.to_string();
        let gen_obj = req_field(dim_obj, "generator", ctx)?
            .as_object()
            .ok_or_else(|| invalid_def(ctx, "`generator` must be an object"))?;
        let generator_type = req_str(gen_obj, "type", ctx)?.to_string();
        let settings = gen_obj
            .get("settings")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let biome_source = if let Some(bs_val) = gen_obj.get("biome_source")
            && let Some(bs_obj) = bs_val.as_object()
        {
            let bs_type = req_str(bs_obj, "type", ctx)?;
            Some(match bs_type {
                "minecraft:multi_noise" => {
                    BiomeSourceDef::MultiNoise(parse_parameter_list(ctx, bs_val)?)
                }
                "minecraft:fixed" => {
                    BiomeSourceDef::Fixed(req_str(bs_obj, "biome", ctx)?.to_string())
                }
                "minecraft:the_end" => BiomeSourceDef::TheEnd,
                other => BiomeSourceDef::Other(other.to_string()),
            })
        } else {
            None
        };
        dimensions.insert(
            dim_id.clone(),
            DimensionDef {
                dimension_type,
                generator_type,
                settings,
                biome_source,
            },
        );
    }
    Ok(WorldPresetDef { dimensions })
}
