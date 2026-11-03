//! Worldgen datapack loader for vanilla `26.1.zip` or loose JSON files
//!
//! Reads the JSON definitions under `data/<namespace>/worldgen/` from a provisioned vanilla data archive
//! (`.harpas/vanilla-data/26.1.zip`), a directory on disk, or an in-memory map of relative paths to JSON strings.
//! No Mojang data is embedded in the binary (`docs/world.md#21-generated-worldgen-data`).

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Seek};
use std::path::Path;

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
}

fn is_relevant_worldgen_json(path: &str) -> bool {
    classify_worldgen_path(path).is_some()
}

fn classify_worldgen_path(path: &str) -> Option<(&str, &str, &str)> {
    let rest = path.strip_prefix("data/")?;
    let (namespace, after_ns) = rest.split_once('/')?;
    let after_worldgen = after_ns.strip_prefix("worldgen/")?;
    let (category, file_part) = after_worldgen.split_once('/')?;
    let rel_id = file_part.strip_suffix(".json")?;
    match category {
        "noise"
        | "density_function"
        | "noise_settings"
        | "biome"
        | "multi_noise_biome_source_parameter_list"
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
