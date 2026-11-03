//! All 30 vanilla 26.1 density functions, DAG compiler, and `RandomState` seeding
//!
//! Compiles JSON [`DensityFunctionDef`] trees and named density function registries into a deduplicated
//! [`CompiledDensityGraph`], seeding every `NormalNoise`, `BlendedNoise`, and `SimplexNoise` sampler from the
//! world seed exactly as vanilla's `RandomState` does (`docs/world.md#22-the-noise-chunk-pipeline`).

use std::collections::HashMap;

use harpas_core::noise::{BlendedNoise, NoiseParameters, NormalNoise, SimplexNoise};
use harpas_core::random::{
    LegacyPositionalRandom, LegacyRandom, RandomSource, XoroshiroPositionalRandom, XoroshiroRandom,
};
use indexmap::IndexMap;

use crate::biome::{TargetPoint, quantize_coord};
use crate::data::WorldgenDataError;
use crate::spline::{CubicSpline, CubicSplineDef};

/// Identifier of a compiled node inside a [`CompiledDensityGraph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub u32);

/// Identifier of a seeded [`NormalNoise`] instance inside a [`CompiledDensityGraph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NoiseId(pub u32);

/// Identifier of a seeded [`BlendedNoise`] instance inside a [`CompiledDensityGraph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlendedNoiseId(pub u32);

/// Slot index for a `minecraft:flat_cache` node inside a `NoiseChunk`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FlatCacheId(pub u32);

/// Slot index for a `minecraft:cache_2d` node inside a `NoiseChunk`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cache2DId(pub u32);

/// Slot index for a `minecraft:cache_once` node inside a `NoiseChunk`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheOnceId(pub u32);

/// Slot index for a `minecraft:cache_all_in_cell` node inside a `NoiseChunk`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheAllInCellId(pub u32);

/// Slot index for a `minecraft:interpolated` node inside a `NoiseChunk`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InterpolatorId(pub u32);

/// Unary operations (`DensityFunctions.Mapped.Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    /// `|x|` (`minecraft:abs`).
    Abs,
    /// `x * x` (`minecraft:square`).
    Square,
    /// `x * x * x` (`minecraft:cube`).
    Cube,
    /// `if x > 0.0 { x } else { x * 0.5 }` (`minecraft:half_negative`).
    HalfNegative,
    /// `if x > 0.0 { x } else { x * 0.25 }` (`minecraft:quarter_negative`).
    QuarterNegative,
    /// `let c = x.clamp(-1.0, 1.0); c / 2.0 - c * c * c / 24.0` (`minecraft:squeeze`).
    Squeeze,
    /// `1.0 / x` (`minecraft:invert`).
    Invert,
}

impl UnaryOp {
    /// Applies this unary operation to `x`.
    #[inline]
    pub fn apply(self, x: f64) -> f64 {
        match self {
            Self::Abs => x.abs(),
            Self::Square => x * x,
            Self::Cube => x * x * x,
            Self::HalfNegative => {
                if x > 0.0 {
                    x
                } else {
                    x * 0.5
                }
            }
            Self::QuarterNegative => {
                if x > 0.0 {
                    x
                } else {
                    x * 0.25
                }
            }
            Self::Squeeze => {
                let c = x.clamp(-1.0, 1.0);
                c / 2.0 - c * c * c / 24.0
            }
            Self::Invert => 1.0 / x,
        }
    }

    /// Computes `(min_value, max_value)` from the input range `(min_in, max_in)` as `Mapped.create` does.
    pub fn bounds(self, min_in: f64, max_in: f64) -> (f64, f64) {
        let t_min = self.apply(min_in);
        let t_max = self.apply(max_in);
        match self {
            Self::Invert => {
                if min_in < 0.0 && max_in > 0.0 {
                    (f64::NEG_INFINITY, f64::INFINITY)
                } else {
                    (t_max, t_min)
                }
            }
            Self::Abs | Self::Square => (0.0_f64.max(min_in), t_min.max(t_max)),
            Self::Cube | Self::HalfNegative | Self::QuarterNegative | Self::Squeeze => {
                (t_min, t_max)
            }
        }
    }
}

/// Binary operations (`DensityFunctions.TwoArgumentSimpleFunction.Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    /// `a + b` (`minecraft:add`).
    Add,
    /// `if a == 0.0 { 0.0 } else { a * b }` (`minecraft:mul`).
    Mul,
    /// `if a < b.min_value() { a } else { a.min(b) }` (`minecraft:min`).
    Min,
    /// `if a > b.max_value() { a } else { a.max(b) }` (`minecraft:max`).
    Max,
}

impl BinaryOp {
    /// Computes `(min_value, max_value)` from argument ranges `(min1, max1)` and `(min2, max2)` as `Ap2.create` does.
    pub fn bounds(self, min1: f64, max1: f64, min2: f64, max2: f64) -> (f64, f64) {
        match self {
            Self::Add => (min1 + min2, max1 + max2),
            Self::Mul => {
                let min = if min1 > 0.0 && min2 > 0.0 {
                    min1 * min2
                } else if max1 < 0.0 && max2 < 0.0 {
                    max1 * max2
                } else {
                    (min1 * max2).min(max1 * min2)
                };
                let max = if min1 > 0.0 && min2 > 0.0 {
                    max1 * max2
                } else if max1 < 0.0 && max2 < 0.0 {
                    min1 * min2
                } else {
                    (min1 * min2).max(max1 * max2)
                };
                (min, max)
            }
            Self::Min => (min1.min(min2), max1.min(max2)),
            Self::Max => (min1.max(min2), max1.max(max2)),
        }
    }
}

/// `NoiseChunk` cache and interpolation marker types (`DensityFunctions.Marker.Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkerKind {
    /// Trilinear interpolation inside each `cell_width × cell_height × cell_width` cell (`minecraft:interpolated`).
    Interpolated,
    /// 2D quart-resolution `(x >> 2, z >> 2)` grid cache at `y = 0` (`minecraft:flat_cache`).
    FlatCache,
    /// Single-entry `(block_x, block_z)` cache (`minecraft:cache_2d`).
    Cache2D,
    /// Single-counter cache within a cell/corner evaluation (`minecraft:cache_once`).
    CacheOnce,
    /// Full cell array cache (`minecraft:cache_all_in_cell`).
    CacheAllInCell,
}

/// Rarity mapper used by `minecraft:weird_scaled_sampler`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RarityValueMapper {
    /// Spaghetti 2D thickness mapper (`"type_1"`), max multiplier `2.0`.
    Type1,
    /// Spaghetti 3D rarity mapper (`"type_2"`), max multiplier `3.0`.
    Type2,
}

impl RarityValueMapper {
    /// Maps an input density value to its rarity scale factor.
    #[inline]
    pub fn map(self, value: f64) -> f64 {
        match self {
            Self::Type1 => {
                if value < -0.5 {
                    0.75
                } else if value < 0.0 {
                    1.0
                } else if value < 0.5 {
                    1.5
                } else {
                    2.0
                }
            }
            Self::Type2 => {
                if value < -0.75 {
                    0.5
                } else if value < -0.5 {
                    0.75
                } else if value < 0.5 {
                    1.0
                } else if value < 0.75 {
                    2.0
                } else {
                    3.0
                }
            }
        }
    }

    /// Maximum multiplier returned by this mapper (`2.0` for `Type1`, `3.0` for `Type2`).
    #[inline]
    pub const fn max_multiplier(self) -> f64 {
        match self {
            Self::Type1 => 2.0,
            Self::Type2 => 3.0,
        }
    }
}

/// Uncompiled JSON AST of a density function (`data/minecraft/worldgen/density_function/**/*.json`).
#[derive(Debug, Clone, PartialEq)]
pub enum DensityFunctionDef {
    /// Constant `f64` value (number literal or `minecraft:constant`).
    Constant(f64),
    /// Named reference to another entry in `worldgen/density_function/` (e.g. `"minecraft:overworld/offset"`).
    Reference(String),
    /// Unary operation (`abs`, `square`, `cube`, `half_negative`, `quarter_negative`, `squeeze`, `invert`).
    Unary {
        /// Operation kind.
        op: UnaryOp,
        /// Input density function.
        argument: Box<DensityFunctionDef>,
    },
    /// Binary operation (`add`, `mul`, `min`, `max`).
    Binary {
        /// Operation kind.
        op: BinaryOp,
        /// Left operand.
        argument1: Box<DensityFunctionDef>,
        /// Right operand.
        argument2: Box<DensityFunctionDef>,
    },
    /// `NoiseChunk` cache or interpolation marker (`interpolated`, `flat_cache`, `cache_2d`, `cache_once`, `cache_all_in_cell`).
    Marker {
        /// Marker kind.
        kind: MarkerKind,
        /// Wrapped density function.
        argument: Box<DensityFunctionDef>,
    },
    /// Clamps `input` to `[min, max]` (`minecraft:clamp`).
    Clamp {
        /// Input density function.
        input: Box<DensityFunctionDef>,
        /// Lower bound.
        min: f64,
        /// Upper bound.
        max: f64,
    },
    /// Linearly maps `block_y` from `[from_y, to_y]` onto `[from_value, to_value]` and clamps (`minecraft:y_clamped_gradient`).
    YClampedGradient {
        /// Start Y coordinate.
        from_y: i32,
        /// End Y coordinate.
        to_y: i32,
        /// Value at and beyond `from_y`.
        from_value: f64,
        /// Value at and beyond `to_y`.
        to_value: f64,
    },
    /// Conditional branch on `min_inclusive <= input < max_exclusive` (`minecraft:range_choice`).
    RangeChoice {
        /// Condition density function.
        input: Box<DensityFunctionDef>,
        /// Inclusive lower threshold.
        min_inclusive: f64,
        /// Exclusive upper threshold.
        max_exclusive: f64,
        /// Branch evaluated when `input` is in `[min_inclusive, max_exclusive)`.
        when_in_range: Box<DensityFunctionDef>,
        /// Branch evaluated otherwise.
        when_out_of_range: Box<DensityFunctionDef>,
    },
    /// 3D `NormalNoise` evaluation scaled by `(xz_scale, y_scale, xz_scale)` (`minecraft:noise`).
    Noise {
        /// Noise parameter resource location (e.g. `"minecraft:continentalness"`).
        noise: String,
        /// Horizontal coordinate multiplier.
        xz_scale: f64,
        /// Vertical coordinate multiplier.
        y_scale: f64,
    },
    /// 3D `NormalNoise` evaluation with domain-warped coordinates (`minecraft:shifted_noise`).
    ShiftedNoise {
        /// Noise parameter resource location.
        noise: String,
        /// Horizontal coordinate multiplier.
        xz_scale: f64,
        /// Vertical coordinate multiplier.
        y_scale: f64,
        /// X offset added after scaling.
        shift_x: Box<DensityFunctionDef>,
        /// Y offset added after scaling.
        shift_y: Box<DensityFunctionDef>,
        /// Z offset added after scaling.
        shift_z: Box<DensityFunctionDef>,
    },
    /// `noise.get_value(x * 0.25, 0.0, z * 0.25) * 4.0` (`minecraft:shift_a`).
    ShiftA {
        /// Noise parameter resource location (usually `"minecraft:offset"`).
        argument: String,
    },
    /// `noise.get_value(z * 0.25, x * 0.25, 0.0) * 4.0` (`minecraft:shift_b`).
    ShiftB {
        /// Noise parameter resource location (usually `"minecraft:offset"`).
        argument: String,
    },
    /// `noise.get_value(x * 0.25, y * 0.25, z * 0.25) * 4.0` (`minecraft:shift`).
    Shift {
        /// Noise parameter resource location.
        argument: String,
    },
    /// Rarity-scaled absolute noise (`minecraft:weird_scaled_sampler`).
    WeirdScaledSampler {
        /// Input density function controlling rarity bucket.
        input: Box<DensityFunctionDef>,
        /// Noise parameter resource location.
        noise: String,
        /// Rarity bucket mapping (`type_1` or `type_2`).
        rarity_value_mapper: RarityValueMapper,
    },
    /// Legacy 16-octave dual-Perlin blended noise (`minecraft:old_blended_noise`).
    OldBlendedNoise {
        /// Horizontal scale.
        xz_scale: f64,
        /// Vertical scale.
        y_scale: f64,
        /// Horizontal factor.
        xz_factor: f64,
        /// Vertical factor.
        y_factor: f64,
        /// Vertical smearing multiplier.
        smear_scale_multiplier: f64,
    },
    /// End island 2D Simplex height sampler (`minecraft:end_islands`).
    EndIslands,
    /// Top surface Y scanner (`minecraft:find_top_surface`).
    FindTopSurface {
        /// 3D density function tested for `> 0.0`.
        density: Box<DensityFunctionDef>,
        /// 2D upper Y bound function.
        upper_bound: Box<DensityFunctionDef>,
        /// Lower Y bound in blocks.
        lower_bound: i32,
        /// Vertical step size in blocks.
        cell_height: i32,
    },
    /// 32-bit float cubic Hermite spline (`minecraft:spline`).
    Spline(CubicSplineDef),
    /// Chunk blending alpha (`1.0` with no chunk blender) (`minecraft:blend_alpha`).
    BlendAlpha,
    /// Chunk blending offset (`0.0` with no chunk blender) (`minecraft:blend_offset`).
    BlendOffset,
    /// Chunk blending density passthrough (`minecraft:blend_density`).
    BlendDensity {
        /// Inner density function.
        argument: Box<DensityFunctionDef>,
    },
}

/// Uncompiled JSON definition of the 15 channels in `noise_router`.
#[derive(Debug, Clone, PartialEq)]
pub struct NoiseRouterDef {
    /// Aquifer barrier noise (`barrier`).
    pub barrier_noise: DensityFunctionDef,
    /// Aquifer fluid level floodedness noise (`fluid_level_floodedness`).
    pub fluid_level_floodedness_noise: DensityFunctionDef,
    /// Aquifer fluid level spread noise (`fluid_level_spread`).
    pub fluid_level_spread_noise: DensityFunctionDef,
    /// Aquifer lava noise (`lava`).
    pub lava_noise: DensityFunctionDef,
    /// Climate temperature noise (`temperature`).
    pub temperature: DensityFunctionDef,
    /// Climate vegetation / humidity noise (`vegetation`).
    pub vegetation: DensityFunctionDef,
    /// Climate continentalness noise (`continents`).
    pub continents: DensityFunctionDef,
    /// Climate erosion noise (`erosion`).
    pub erosion: DensityFunctionDef,
    /// Climate depth function (`depth`).
    pub depth: DensityFunctionDef,
    /// Climate weirdness / ridges noise (`ridges`).
    pub ridges: DensityFunctionDef,
    /// Preliminary surface level height estimator (`preliminary_surface_level`).
    pub preliminary_surface_level: DensityFunctionDef,
    /// Final 3D terrain density (`final_density`).
    pub final_density: DensityFunctionDef,
    /// Ore vein type toggle (`vein_toggle`).
    pub vein_toggle: DensityFunctionDef,
    /// Ore vein ridged shape (`vein_ridged`).
    pub vein_ridged: DensityFunctionDef,
    /// Ore vein gap noise (`vein_gap`).
    pub vein_gap: DensityFunctionDef,
}

/// Compiled operation of a single node in [`CompiledDensityGraph`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DensityOp {
    /// Constant `f64` stored as raw IEEE 754 bits for exact hashing and equality.
    Constant(u64),
    /// Unary operation (`Mapped`).
    Unary {
        /// Operation kind.
        op: UnaryOp,
        /// Input node.
        argument: NodeId,
    },
    /// Binary operation (`Ap2`).
    Binary {
        /// Operation kind.
        op: BinaryOp,
        /// Left operand node.
        argument1: NodeId,
        /// Right operand node.
        argument2: NodeId,
    },
    /// `NoiseChunk` `FlatCache` marker.
    FlatCache {
        /// Slot index in `NoiseChunk`.
        slot: FlatCacheId,
        /// Wrapped node.
        argument: NodeId,
    },
    /// `NoiseChunk` `Cache2D` marker.
    Cache2D {
        /// Slot index in `NoiseChunk`.
        slot: Cache2DId,
        /// Wrapped node.
        argument: NodeId,
    },
    /// `NoiseChunk` `CacheOnce` marker.
    CacheOnce {
        /// Slot index in `NoiseChunk`.
        slot: CacheOnceId,
        /// Wrapped node.
        argument: NodeId,
    },
    /// `NoiseChunk` `CacheAllInCell` marker.
    CacheAllInCell {
        /// Slot index in `NoiseChunk`.
        slot: CacheAllInCellId,
        /// Wrapped node.
        argument: NodeId,
    },
    /// `NoiseChunk` `NoiseInterpolator` marker.
    Interpolated {
        /// Slot index in `NoiseChunk`.
        slot: InterpolatorId,
        /// Wrapped node.
        argument: NodeId,
    },
    /// `minecraft:clamp`.
    Clamp {
        /// Input node.
        input: NodeId,
        /// Lower bound bits.
        min_bits: u64,
        /// Upper bound bits.
        max_bits: u64,
    },
    /// `minecraft:y_clamped_gradient`.
    YClampedGradient {
        /// Start Y coordinate.
        from_y: i32,
        /// End Y coordinate.
        to_y: i32,
        /// Value at `from_y` (raw bits).
        from_value_bits: u64,
        /// Value at `to_y` (raw bits).
        to_value_bits: u64,
    },
    /// `minecraft:range_choice`.
    RangeChoice {
        /// Condition input node.
        input: NodeId,
        /// Inclusive lower threshold (raw bits).
        min_inclusive_bits: u64,
        /// Exclusive upper threshold (raw bits).
        max_exclusive_bits: u64,
        /// In-range branch node.
        when_in_range: NodeId,
        /// Out-of-range branch node.
        when_out_of_range: NodeId,
    },
    /// `minecraft:noise`.
    Noise {
        /// Seeded `NormalNoise` index.
        noise: NoiseId,
        /// Horizontal scale (raw bits).
        xz_scale_bits: u64,
        /// Vertical scale (raw bits).
        y_scale_bits: u64,
    },
    /// `minecraft:shifted_noise`.
    ShiftedNoise {
        /// Seeded `NormalNoise` index.
        noise: NoiseId,
        /// Horizontal scale (raw bits).
        xz_scale_bits: u64,
        /// Vertical scale (raw bits).
        y_scale_bits: u64,
        /// X shift node.
        shift_x: NodeId,
        /// Y shift node.
        shift_y: NodeId,
        /// Z shift node.
        shift_z: NodeId,
    },
    /// `minecraft:shift_a`.
    ShiftA {
        /// Seeded `NormalNoise` index.
        noise: NoiseId,
    },
    /// `minecraft:shift_b`.
    ShiftB {
        /// Seeded `NormalNoise` index.
        noise: NoiseId,
    },
    /// `minecraft:shift`.
    Shift {
        /// Seeded `NormalNoise` index.
        noise: NoiseId,
    },
    /// `minecraft:weird_scaled_sampler`.
    WeirdScaledSampler {
        /// Input node.
        input: NodeId,
        /// Seeded `NormalNoise` index.
        noise: NoiseId,
        /// Rarity value mapper.
        mapper: RarityValueMapper,
    },
    /// `minecraft:old_blended_noise`.
    OldBlendedNoise(BlendedNoiseId),
    /// `minecraft:end_islands`.
    EndIslands,
    /// `minecraft:find_top_surface`.
    FindTopSurface {
        /// 3D density node.
        density: NodeId,
        /// Upper Y bound node.
        upper_bound: NodeId,
        /// Lower Y bound in blocks.
        lower_bound: i32,
        /// Vertical step size in blocks.
        cell_height: i32,
    },
    /// `minecraft:spline`.
    Spline(CubicSpline),
}

/// A compiled node in [`CompiledDensityGraph`] with its precomputed `[min_value, max_value]` bounds.
#[derive(Debug, Clone)]
pub struct CompiledNode {
    /// Operation of this node.
    pub op: DensityOp,
    /// Lower bound (`DensityFunction.minValue()`).
    pub min_value: f64,
    /// Upper bound (`DensityFunction.maxValue()`).
    pub max_value: f64,
}

/// The 15 compiled [`NodeId`] roots of a `NoiseRouter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompiledNoiseRouter {
    /// Aquifer barrier noise (`barrier`).
    pub barrier_noise: NodeId,
    /// Aquifer fluid level floodedness noise (`fluid_level_floodedness`).
    pub fluid_level_floodedness_noise: NodeId,
    /// Aquifer fluid level spread noise (`fluid_level_spread`).
    pub fluid_level_spread_noise: NodeId,
    /// Aquifer lava noise (`lava`).
    pub lava_noise: NodeId,
    /// Climate temperature noise (`temperature`).
    pub temperature: NodeId,
    /// Climate vegetation / humidity noise (`vegetation`).
    pub vegetation: NodeId,
    /// Climate continentalness noise (`continents`).
    pub continents: NodeId,
    /// Climate erosion noise (`erosion`).
    pub erosion: NodeId,
    /// Climate depth function (`depth`).
    pub depth: NodeId,
    /// Climate weirdness / ridges noise (`ridges`).
    pub ridges: NodeId,
    /// Preliminary surface level height estimator (`preliminary_surface_level`).
    pub preliminary_surface_level: NodeId,
    /// Final 3D terrain density (`final_density`).
    pub final_density: NodeId,
    /// Ore vein type toggle (`vein_toggle`).
    pub vein_toggle: NodeId,
    /// Ore vein ridged shape (`vein_ridged`).
    pub vein_ridged: NodeId,
    /// Ore vein gap noise (`vein_gap`).
    pub vein_gap: NodeId,
}

/// Deduplicated DAG of compiled density functions and seeded noise samplers (`RandomState`).
#[derive(Debug, Clone)]
pub struct CompiledDensityGraph {
    world_seed: i64,
    legacy_random_source: bool,
    nodes: Vec<CompiledNode>,
    noises: Vec<NormalNoise>,
    noise_index_by_name: HashMap<String, NoiseId>,
    blended_noises: Vec<BlendedNoise>,
    end_islands_noise: Option<SimplexNoise>,
    flat_cache_args: Vec<NodeId>,
    cache_2d_args: Vec<NodeId>,
    cache_once_args: Vec<NodeId>,
    cache_all_in_cell_args: Vec<NodeId>,
    interpolator_args: Vec<NodeId>,
}

/// Structural key used during compilation before marker slot assignment to deduplicate identical nodes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum PreSlotKey {
    Constant(u64),
    Unary {
        op: UnaryOp,
        argument: NodeId,
    },
    Binary {
        op: BinaryOp,
        argument1: NodeId,
        argument2: NodeId,
    },
    Marker {
        kind: MarkerKind,
        argument: NodeId,
    },
    Clamp {
        input: NodeId,
        min_bits: u64,
        max_bits: u64,
    },
    YClampedGradient {
        from_y: i32,
        to_y: i32,
        from_value_bits: u64,
        to_value_bits: u64,
    },
    RangeChoice {
        input: NodeId,
        min_inclusive_bits: u64,
        max_exclusive_bits: u64,
        when_in_range: NodeId,
        when_out_of_range: NodeId,
    },
    Noise {
        noise: NoiseId,
        xz_scale_bits: u64,
        y_scale_bits: u64,
    },
    ShiftedNoise {
        noise: NoiseId,
        xz_scale_bits: u64,
        y_scale_bits: u64,
        shift_x: NodeId,
        shift_y: NodeId,
        shift_z: NodeId,
    },
    ShiftA(NoiseId),
    ShiftB(NoiseId),
    Shift(NoiseId),
    WeirdScaledSampler {
        input: NodeId,
        noise: NoiseId,
        mapper: RarityValueMapper,
    },
    OldBlendedNoise {
        xz_scale_bits: u64,
        y_scale_bits: u64,
        xz_factor_bits: u64,
        y_factor_bits: u64,
        smear_bits: u64,
    },
    EndIslands,
    FindTopSurface {
        density: NodeId,
        upper_bound: NodeId,
        lower_bound: i32,
        cell_height: i32,
    },
    Spline(CubicSpline),
}

enum AnyPositionalFactory {
    Xoroshiro(XoroshiroPositionalRandom),
    Legacy(LegacyPositionalRandom),
}

impl AnyPositionalFactory {
    fn fork_from_hash_of(&self, name: &str) -> Box<dyn RandomSource> {
        match self {
            Self::Xoroshiro(f) => Box::new(f.from_hash_of(name)),
            Self::Legacy(f) => Box::new(f.from_hash_of(name)),
        }
    }
}

struct GraphCompiler<'a> {
    graph: CompiledDensityGraph,
    positional: AnyPositionalFactory,
    noise_defs: &'a IndexMap<String, NoiseParameters>,
    df_defs: &'a IndexMap<String, DensityFunctionDef>,
    named_nodes: HashMap<String, NodeId>,
    interner: HashMap<PreSlotKey, NodeId>,
    visiting: Vec<String>,
}

impl<'a> GraphCompiler<'a> {
    fn new(
        world_seed: i64,
        legacy_random_source: bool,
        noise_defs: &'a IndexMap<String, NoiseParameters>,
        df_defs: &'a IndexMap<String, DensityFunctionDef>,
    ) -> Self {
        let positional = if legacy_random_source {
            AnyPositionalFactory::Legacy(LegacyRandom::new(world_seed).fork_positional())
        } else {
            AnyPositionalFactory::Xoroshiro(XoroshiroRandom::new(world_seed).fork_positional())
        };
        Self {
            graph: CompiledDensityGraph {
                world_seed,
                legacy_random_source,
                nodes: Vec::new(),
                noises: Vec::new(),
                noise_index_by_name: HashMap::new(),
                blended_noises: Vec::new(),
                end_islands_noise: None,
                flat_cache_args: Vec::new(),
                cache_2d_args: Vec::new(),
                cache_once_args: Vec::new(),
                cache_all_in_cell_args: Vec::new(),
                interpolator_args: Vec::new(),
            },
            positional,
            noise_defs,
            df_defs,
            named_nodes: HashMap::new(),
            interner: HashMap::new(),
            visiting: Vec::new(),
        }
    }

    fn get_or_create_noise(&mut self, name: &str) -> Result<NoiseId, WorldgenDataError> {
        if let Some(&id) = self.graph.noise_index_by_name.get(name) {
            return Ok(id);
        }

        let normal = if self.graph.legacy_random_source {
            if name == "minecraft:temperature" {
                let mut rng = LegacyRandom::new(self.graph.world_seed);
                Some(NormalNoise::new(&mut rng, -7, &[1.0, 1.0]))
            } else if name == "minecraft:vegetation" {
                let mut rng = LegacyRandom::new(self.graph.world_seed.wrapping_add(1));
                Some(NormalNoise::new(&mut rng, -7, &[1.0, 1.0]))
            } else if name == "minecraft:offset" {
                let mut rng = XoroshiroRandom::new(0)
                    .fork_positional()
                    .from_hash_of("minecraft:offset");
                Some(NormalNoise::new(&mut rng, 0, &[0.0]))
            } else {
                None
            }
        } else {
            None
        };

        let normal = match normal {
            Some(n) => n,
            None => {
                let params = self.noise_defs.get(name).ok_or_else(|| {
                    WorldgenDataError::InvalidDefinition {
                        context: name.to_string(),
                        message: format!("unknown noise parameter `{name}`"),
                    }
                })?;
                let mut rng = self.positional.fork_from_hash_of(name);
                NormalNoise::from_parameters(&mut *rng, params)
            }
        };

        let id = NoiseId(self.graph.noises.len() as u32);
        self.graph.noises.push(normal);
        self.graph.noise_index_by_name.insert(name.to_string(), id);
        Ok(id)
    }

    fn compile_spline(&mut self, def: &CubicSplineDef) -> Result<CubicSpline, WorldgenDataError> {
        match def {
            CubicSplineDef::Constant(val) => Ok(CubicSpline::constant(*val)),
            CubicSplineDef::Multipoint { coordinate, points } => {
                let coord_id = self.compile_def(coordinate)?;
                let coord_node = &self.graph.nodes[coord_id.0 as usize];
                let coord_min = coord_node.min_value as f32;
                let coord_max = coord_node.max_value as f32;
                let mut locations = Vec::with_capacity(points.len());
                let mut values = Vec::with_capacity(points.len());
                let mut derivatives = Vec::with_capacity(points.len());
                for pt in points {
                    locations.push(pt.location);
                    values.push(self.compile_spline(&pt.value)?);
                    derivatives.push(pt.derivative);
                }
                Ok(CubicSpline::multipoint(
                    coord_id,
                    coord_min,
                    coord_max,
                    locations,
                    values,
                    derivatives,
                ))
            }
        }
    }

    fn intern(&mut self, key: PreSlotKey, op: DensityOp, min_value: f64, max_value: f64) -> NodeId {
        if let Some(&existing) = self.interner.get(&key) {
            return existing;
        }
        let id = NodeId(self.graph.nodes.len() as u32);
        self.graph.nodes.push(CompiledNode {
            op,
            min_value,
            max_value,
        });
        self.interner.insert(key, id);
        id
    }

    fn compile_def(&mut self, def: &DensityFunctionDef) -> Result<NodeId, WorldgenDataError> {
        match def {
            DensityFunctionDef::Constant(val) => {
                let bits = canonical_f64_bits(*val);
                let key = PreSlotKey::Constant(bits);
                Ok(self.intern(key, DensityOp::Constant(bits), *val, *val))
            }
            DensityFunctionDef::BlendAlpha => {
                let bits = canonical_f64_bits(1.0);
                let key = PreSlotKey::Constant(bits);
                Ok(self.intern(key, DensityOp::Constant(bits), 1.0, 1.0))
            }
            DensityFunctionDef::BlendOffset => {
                let bits = canonical_f64_bits(0.0);
                let key = PreSlotKey::Constant(bits);
                Ok(self.intern(key, DensityOp::Constant(bits), 0.0, 0.0))
            }
            DensityFunctionDef::BlendDensity { argument } => self.compile_def(argument),
            DensityFunctionDef::Reference(name) => {
                if let Some(&id) = self.named_nodes.get(name) {
                    return Ok(id);
                }
                if self.visiting.iter().any(|s| s == name) {
                    return Err(WorldgenDataError::InvalidDefinition {
                        context: name.clone(),
                        message: format!("cyclic density function reference `{name}`"),
                    });
                }
                let target =
                    self.df_defs
                        .get(name)
                        .ok_or_else(|| WorldgenDataError::InvalidDefinition {
                            context: name.clone(),
                            message: format!("unknown density function reference `{name}`"),
                        })?;
                self.visiting.push(name.clone());
                let compiled = self.compile_def(target)?;
                self.visiting.pop();
                self.named_nodes.insert(name.clone(), compiled);
                Ok(compiled)
            }
            DensityFunctionDef::Unary { op, argument } => {
                let arg_id = self.compile_def(argument)?;
                let arg_node = &self.graph.nodes[arg_id.0 as usize];
                let (min_val, max_val) = op.bounds(arg_node.min_value, arg_node.max_value);
                let key = PreSlotKey::Unary {
                    op: *op,
                    argument: arg_id,
                };
                Ok(self.intern(
                    key,
                    DensityOp::Unary {
                        op: *op,
                        argument: arg_id,
                    },
                    min_val,
                    max_val,
                ))
            }
            DensityFunctionDef::Binary {
                op,
                argument1,
                argument2,
            } => {
                let a1 = self.compile_def(argument1)?;
                let a2 = self.compile_def(argument2)?;
                let n1 = &self.graph.nodes[a1.0 as usize];
                let n2 = &self.graph.nodes[a2.0 as usize];
                let (min_val, max_val) =
                    op.bounds(n1.min_value, n1.max_value, n2.min_value, n2.max_value);
                let key = PreSlotKey::Binary {
                    op: *op,
                    argument1: a1,
                    argument2: a2,
                };
                Ok(self.intern(
                    key,
                    DensityOp::Binary {
                        op: *op,
                        argument1: a1,
                        argument2: a2,
                    },
                    min_val,
                    max_val,
                ))
            }
            DensityFunctionDef::Marker { kind, argument } => {
                let arg_id = self.compile_def(argument)?;
                let key = PreSlotKey::Marker {
                    kind: *kind,
                    argument: arg_id,
                };
                if let Some(&existing) = self.interner.get(&key) {
                    return Ok(existing);
                }
                let (min_val, max_val) = {
                    let n = &self.graph.nodes[arg_id.0 as usize];
                    (n.min_value, n.max_value)
                };
                let op = match kind {
                    MarkerKind::FlatCache => {
                        let slot = FlatCacheId(self.graph.flat_cache_args.len() as u32);
                        self.graph.flat_cache_args.push(arg_id);
                        DensityOp::FlatCache {
                            slot,
                            argument: arg_id,
                        }
                    }
                    MarkerKind::Cache2D => {
                        let slot = Cache2DId(self.graph.cache_2d_args.len() as u32);
                        self.graph.cache_2d_args.push(arg_id);
                        DensityOp::Cache2D {
                            slot,
                            argument: arg_id,
                        }
                    }
                    MarkerKind::CacheOnce => {
                        let slot = CacheOnceId(self.graph.cache_once_args.len() as u32);
                        self.graph.cache_once_args.push(arg_id);
                        DensityOp::CacheOnce {
                            slot,
                            argument: arg_id,
                        }
                    }
                    MarkerKind::CacheAllInCell => {
                        let slot = CacheAllInCellId(self.graph.cache_all_in_cell_args.len() as u32);
                        self.graph.cache_all_in_cell_args.push(arg_id);
                        DensityOp::CacheAllInCell {
                            slot,
                            argument: arg_id,
                        }
                    }
                    MarkerKind::Interpolated => {
                        let slot = InterpolatorId(self.graph.interpolator_args.len() as u32);
                        self.graph.interpolator_args.push(arg_id);
                        DensityOp::Interpolated {
                            slot,
                            argument: arg_id,
                        }
                    }
                };
                Ok(self.intern(key, op, min_val, max_val))
            }
            DensityFunctionDef::Clamp { input, min, max } => {
                let in_id = self.compile_def(input)?;
                let min_bits = canonical_f64_bits(*min);
                let max_bits = canonical_f64_bits(*max);
                let key = PreSlotKey::Clamp {
                    input: in_id,
                    min_bits,
                    max_bits,
                };
                Ok(self.intern(
                    key,
                    DensityOp::Clamp {
                        input: in_id,
                        min_bits,
                        max_bits,
                    },
                    *min,
                    *max,
                ))
            }
            DensityFunctionDef::YClampedGradient {
                from_y,
                to_y,
                from_value,
                to_value,
            } => {
                let from_value_bits = canonical_f64_bits(*from_value);
                let to_value_bits = canonical_f64_bits(*to_value);
                let min_val = from_value.min(*to_value);
                let max_val = from_value.max(*to_value);
                let key = PreSlotKey::YClampedGradient {
                    from_y: *from_y,
                    to_y: *to_y,
                    from_value_bits,
                    to_value_bits,
                };
                Ok(self.intern(
                    key,
                    DensityOp::YClampedGradient {
                        from_y: *from_y,
                        to_y: *to_y,
                        from_value_bits,
                        to_value_bits,
                    },
                    min_val,
                    max_val,
                ))
            }
            DensityFunctionDef::RangeChoice {
                input,
                min_inclusive,
                max_exclusive,
                when_in_range,
                when_out_of_range,
            } => {
                let in_id = self.compile_def(input)?;
                let yes_id = self.compile_def(when_in_range)?;
                let no_id = self.compile_def(when_out_of_range)?;
                let min_val = self.graph.nodes[yes_id.0 as usize]
                    .min_value
                    .min(self.graph.nodes[no_id.0 as usize].min_value);
                let max_val = self.graph.nodes[yes_id.0 as usize]
                    .max_value
                    .max(self.graph.nodes[no_id.0 as usize].max_value);
                let min_inclusive_bits = canonical_f64_bits(*min_inclusive);
                let max_exclusive_bits = canonical_f64_bits(*max_exclusive);
                let key = PreSlotKey::RangeChoice {
                    input: in_id,
                    min_inclusive_bits,
                    max_exclusive_bits,
                    when_in_range: yes_id,
                    when_out_of_range: no_id,
                };
                Ok(self.intern(
                    key,
                    DensityOp::RangeChoice {
                        input: in_id,
                        min_inclusive_bits,
                        max_exclusive_bits,
                        when_in_range: yes_id,
                        when_out_of_range: no_id,
                    },
                    min_val,
                    max_val,
                ))
            }
            DensityFunctionDef::Noise {
                noise,
                xz_scale,
                y_scale,
            } => {
                let noise_id = self.get_or_create_noise(noise)?;
                let max_val = self.graph.noises[noise_id.0 as usize].max_value();
                let xz_scale_bits = canonical_f64_bits(*xz_scale);
                let y_scale_bits = canonical_f64_bits(*y_scale);
                let key = PreSlotKey::Noise {
                    noise: noise_id,
                    xz_scale_bits,
                    y_scale_bits,
                };
                Ok(self.intern(
                    key,
                    DensityOp::Noise {
                        noise: noise_id,
                        xz_scale_bits,
                        y_scale_bits,
                    },
                    -max_val,
                    max_val,
                ))
            }
            DensityFunctionDef::ShiftedNoise {
                noise,
                xz_scale,
                y_scale,
                shift_x,
                shift_y,
                shift_z,
            } => {
                let sx = self.compile_def(shift_x)?;
                let sy = self.compile_def(shift_y)?;
                let sz = self.compile_def(shift_z)?;
                let noise_id = self.get_or_create_noise(noise)?;
                let max_val = self.graph.noises[noise_id.0 as usize].max_value();
                let xz_scale_bits = canonical_f64_bits(*xz_scale);
                let y_scale_bits = canonical_f64_bits(*y_scale);
                let key = PreSlotKey::ShiftedNoise {
                    noise: noise_id,
                    xz_scale_bits,
                    y_scale_bits,
                    shift_x: sx,
                    shift_y: sy,
                    shift_z: sz,
                };
                Ok(self.intern(
                    key,
                    DensityOp::ShiftedNoise {
                        noise: noise_id,
                        xz_scale_bits,
                        y_scale_bits,
                        shift_x: sx,
                        shift_y: sy,
                        shift_z: sz,
                    },
                    -max_val,
                    max_val,
                ))
            }
            DensityFunctionDef::ShiftA { argument } => {
                let noise_id = self.get_or_create_noise(argument)?;
                let max_val = self.graph.noises[noise_id.0 as usize].max_value() * 4.0;
                let key = PreSlotKey::ShiftA(noise_id);
                Ok(self.intern(
                    key,
                    DensityOp::ShiftA { noise: noise_id },
                    -max_val,
                    max_val,
                ))
            }
            DensityFunctionDef::ShiftB { argument } => {
                let noise_id = self.get_or_create_noise(argument)?;
                let max_val = self.graph.noises[noise_id.0 as usize].max_value() * 4.0;
                let key = PreSlotKey::ShiftB(noise_id);
                Ok(self.intern(
                    key,
                    DensityOp::ShiftB { noise: noise_id },
                    -max_val,
                    max_val,
                ))
            }
            DensityFunctionDef::Shift { argument } => {
                let noise_id = self.get_or_create_noise(argument)?;
                let max_val = self.graph.noises[noise_id.0 as usize].max_value() * 4.0;
                let key = PreSlotKey::Shift(noise_id);
                Ok(self.intern(key, DensityOp::Shift { noise: noise_id }, -max_val, max_val))
            }
            DensityFunctionDef::WeirdScaledSampler {
                input,
                noise,
                rarity_value_mapper,
            } => {
                let in_id = self.compile_def(input)?;
                let noise_id = self.get_or_create_noise(noise)?;
                let max_val = rarity_value_mapper.max_multiplier()
                    * self.graph.noises[noise_id.0 as usize].max_value();
                let key = PreSlotKey::WeirdScaledSampler {
                    input: in_id,
                    noise: noise_id,
                    mapper: *rarity_value_mapper,
                };
                Ok(self.intern(
                    key,
                    DensityOp::WeirdScaledSampler {
                        input: in_id,
                        noise: noise_id,
                        mapper: *rarity_value_mapper,
                    },
                    0.0,
                    max_val,
                ))
            }
            DensityFunctionDef::OldBlendedNoise {
                xz_scale,
                y_scale,
                xz_factor,
                y_factor,
                smear_scale_multiplier,
            } => {
                let key = PreSlotKey::OldBlendedNoise {
                    xz_scale_bits: canonical_f64_bits(*xz_scale),
                    y_scale_bits: canonical_f64_bits(*y_scale),
                    xz_factor_bits: canonical_f64_bits(*xz_factor),
                    y_factor_bits: canonical_f64_bits(*y_factor),
                    smear_bits: canonical_f64_bits(*smear_scale_multiplier),
                };
                if let Some(&existing) = self.interner.get(&key) {
                    return Ok(existing);
                }
                let blended = if self.graph.legacy_random_source {
                    let mut rng = LegacyRandom::new(self.graph.world_seed);
                    BlendedNoise::new(
                        &mut rng,
                        *xz_scale,
                        *y_scale,
                        *xz_factor,
                        *y_factor,
                        *smear_scale_multiplier,
                    )
                } else {
                    let mut rng = self.positional.fork_from_hash_of("minecraft:terrain");
                    BlendedNoise::new(
                        &mut *rng,
                        *xz_scale,
                        *y_scale,
                        *xz_factor,
                        *y_factor,
                        *smear_scale_multiplier,
                    )
                };
                let max_val = blended.max_value();
                let blended_id = BlendedNoiseId(self.graph.blended_noises.len() as u32);
                self.graph.blended_noises.push(blended);
                Ok(self.intern(
                    key,
                    DensityOp::OldBlendedNoise(blended_id),
                    -max_val,
                    max_val,
                ))
            }
            DensityFunctionDef::EndIslands => {
                if self.graph.end_islands_noise.is_none() {
                    let mut rng = LegacyRandom::new(self.graph.world_seed);
                    rng.consume_count(17_292);
                    self.graph.end_islands_noise = Some(SimplexNoise::new(&mut rng));
                }
                Ok(self.intern(
                    PreSlotKey::EndIslands,
                    DensityOp::EndIslands,
                    -0.84375,
                    0.5625,
                ))
            }
            DensityFunctionDef::FindTopSurface {
                density,
                upper_bound,
                lower_bound,
                cell_height,
            } => {
                let density_id = self.compile_def(density)?;
                let upper_id = self.compile_def(upper_bound)?;
                let min_val = f64::from(*lower_bound);
                let max_val = min_val.max(self.graph.nodes[upper_id.0 as usize].max_value);
                let key = PreSlotKey::FindTopSurface {
                    density: density_id,
                    upper_bound: upper_id,
                    lower_bound: *lower_bound,
                    cell_height: *cell_height,
                };
                Ok(self.intern(
                    key,
                    DensityOp::FindTopSurface {
                        density: density_id,
                        upper_bound: upper_id,
                        lower_bound: *lower_bound,
                        cell_height: *cell_height,
                    },
                    min_val,
                    max_val,
                ))
            }
            DensityFunctionDef::Spline(spline_def) => {
                let compiled_spline = self.compile_spline(spline_def)?;
                let min_val = f64::from(compiled_spline.min_value());
                let max_val = f64::from(compiled_spline.max_value());
                let key = PreSlotKey::Spline(compiled_spline.clone());
                Ok(self.intern(key, DensityOp::Spline(compiled_spline), min_val, max_val))
            }
        }
    }
}

#[inline]
fn canonical_f64_bits(val: f64) -> u64 {
    if val == 0.0 {
        0.0_f64.to_bits()
    } else {
        val.to_bits()
    }
}

impl CompiledDensityGraph {
    /// Compiles a [`NoiseRouterDef`] along with its referenced density functions and noise parameters into a
    /// deduplicated [`CompiledDensityGraph`] and [`CompiledNoiseRouter`].
    pub fn compile_router(
        world_seed: i64,
        legacy_random_source: bool,
        router_def: &NoiseRouterDef,
        noise_defs: &IndexMap<String, NoiseParameters>,
        df_defs: &IndexMap<String, DensityFunctionDef>,
    ) -> Result<(Self, CompiledNoiseRouter), WorldgenDataError> {
        let mut compiler =
            GraphCompiler::new(world_seed, legacy_random_source, noise_defs, df_defs);
        let router = CompiledNoiseRouter {
            barrier_noise: compiler.compile_def(&router_def.barrier_noise)?,
            fluid_level_floodedness_noise: compiler
                .compile_def(&router_def.fluid_level_floodedness_noise)?,
            fluid_level_spread_noise: compiler.compile_def(&router_def.fluid_level_spread_noise)?,
            lava_noise: compiler.compile_def(&router_def.lava_noise)?,
            temperature: compiler.compile_def(&router_def.temperature)?,
            vegetation: compiler.compile_def(&router_def.vegetation)?,
            continents: compiler.compile_def(&router_def.continents)?,
            erosion: compiler.compile_def(&router_def.erosion)?,
            depth: compiler.compile_def(&router_def.depth)?,
            ridges: compiler.compile_def(&router_def.ridges)?,
            preliminary_surface_level: compiler
                .compile_def(&router_def.preliminary_surface_level)?,
            final_density: compiler.compile_def(&router_def.final_density)?,
            vein_toggle: compiler.compile_def(&router_def.vein_toggle)?,
            vein_ridged: compiler.compile_def(&router_def.vein_ridged)?,
            vein_gap: compiler.compile_def(&router_def.vein_gap)?,
        };
        Ok((compiler.graph, router))
    }

    /// Compiles a single [`DensityFunctionDef`] into a standalone [`CompiledDensityGraph`] and root [`NodeId`].
    pub fn compile_single(
        world_seed: i64,
        legacy_random_source: bool,
        def: &DensityFunctionDef,
        noise_defs: &IndexMap<String, NoiseParameters>,
        df_defs: &IndexMap<String, DensityFunctionDef>,
    ) -> Result<(Self, NodeId), WorldgenDataError> {
        let mut compiler =
            GraphCompiler::new(world_seed, legacy_random_source, noise_defs, df_defs);
        let root = compiler.compile_def(def)?;
        Ok((compiler.graph, root))
    }

    /// World seed used to initialize this graph.
    pub const fn world_seed(&self) -> i64 {
        self.world_seed
    }

    /// Whether `LegacyRandom` seeding was used.
    pub const fn legacy_random_source(&self) -> bool {
        self.legacy_random_source
    }

    /// All compiled nodes in topological order (children precede parents).
    pub fn nodes(&self) -> &[CompiledNode] {
        &self.nodes
    }

    /// Returns the compiled node for `id`.
    #[inline]
    pub fn node(&self, id: NodeId) -> &CompiledNode {
        &self.nodes[id.0 as usize]
    }

    /// Returns the seeded [`NormalNoise`] for `id`.
    #[inline]
    pub fn noise(&self, id: NoiseId) -> &NormalNoise {
        &self.noises[id.0 as usize]
    }

    /// Returns the seeded [`NormalNoise`] by resource location (e.g. `"minecraft:surface"`), if present.
    pub fn noise_by_name(&self, name: &str) -> Option<&NormalNoise> {
        self.noise_index_by_name.get(name).map(|&id| self.noise(id))
    }

    /// Returns the seeded [`BlendedNoise`] for `id`.
    #[inline]
    pub fn blended_noise(&self, id: BlendedNoiseId) -> &BlendedNoise {
        &self.blended_noises[id.0 as usize]
    }

    /// Returns the seeded End island [`SimplexNoise`], if used by the graph.
    #[inline]
    pub fn end_islands_noise(&self) -> Option<&SimplexNoise> {
        self.end_islands_noise.as_ref()
    }

    /// Inner argument nodes for each `FlatCache` slot.
    pub fn flat_cache_args(&self) -> &[NodeId] {
        &self.flat_cache_args
    }

    /// Inner argument nodes for each `Cache2D` slot.
    pub fn cache_2d_args(&self) -> &[NodeId] {
        &self.cache_2d_args
    }

    /// Inner argument nodes for each `CacheOnce` slot.
    pub fn cache_once_args(&self) -> &[NodeId] {
        &self.cache_once_args
    }

    /// Inner argument nodes for each `CacheAllInCell` slot.
    pub fn cache_all_in_cell_args(&self) -> &[NodeId] {
        &self.cache_all_in_cell_args
    }

    /// Inner argument nodes for each `Interpolated` (`NoiseInterpolator`) slot.
    pub fn interpolator_args(&self) -> &[NodeId] {
        &self.interpolator_args
    }

    /// Evaluates `node` at a single point `(block_x, block_y, block_z)` without `NoiseChunk` cell state
    /// (`DensityFunction.SinglePointContext`).
    pub fn eval_point_stateless(
        &self,
        node: NodeId,
        block_x: i32,
        block_y: i32,
        block_z: i32,
    ) -> f64 {
        let compiled = &self.nodes[node.0 as usize];
        match &compiled.op {
            DensityOp::Constant(bits) => f64::from_bits(*bits),
            DensityOp::Unary { op, argument } => {
                let v = self.eval_point_stateless(*argument, block_x, block_y, block_z);
                op.apply(v)
            }
            DensityOp::Binary {
                op,
                argument1,
                argument2,
            } => {
                let v1 = self.eval_point_stateless(*argument1, block_x, block_y, block_z);
                match op {
                    BinaryOp::Add => {
                        v1 + self.eval_point_stateless(*argument2, block_x, block_y, block_z)
                    }
                    BinaryOp::Mul => {
                        if v1 == 0.0 {
                            0.0
                        } else {
                            v1 * self.eval_point_stateless(*argument2, block_x, block_y, block_z)
                        }
                    }
                    BinaryOp::Min => {
                        let min2 = self.nodes[argument2.0 as usize].min_value;
                        if v1 < min2 {
                            v1
                        } else {
                            v1.min(self.eval_point_stateless(*argument2, block_x, block_y, block_z))
                        }
                    }
                    BinaryOp::Max => {
                        let max2 = self.nodes[argument2.0 as usize].max_value;
                        if v1 > max2 {
                            v1
                        } else {
                            v1.max(self.eval_point_stateless(*argument2, block_x, block_y, block_z))
                        }
                    }
                }
            }
            DensityOp::FlatCache { argument, .. } => {
                // Even outside a NoiseChunk, vanilla's FlatCache (or quart-aligned 2D functions) evaluates at y = 0.
                self.eval_point_stateless(*argument, block_x, 0, block_z)
            }
            DensityOp::Cache2D { argument, .. }
            | DensityOp::CacheOnce { argument, .. }
            | DensityOp::CacheAllInCell { argument, .. }
            | DensityOp::Interpolated { argument, .. } => {
                self.eval_point_stateless(*argument, block_x, block_y, block_z)
            }
            DensityOp::Clamp {
                input,
                min_bits,
                max_bits,
            } => {
                let v = self.eval_point_stateless(*input, block_x, block_y, block_z);
                v.clamp(f64::from_bits(*min_bits), f64::from_bits(*max_bits))
            }
            DensityOp::YClampedGradient {
                from_y,
                to_y,
                from_value_bits,
                to_value_bits,
            } => y_clamped_gradient(
                block_y,
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
                let v = self.eval_point_stateless(*input, block_x, block_y, block_z);
                let min_inc = f64::from_bits(*min_inclusive_bits);
                let max_exc = f64::from_bits(*max_exclusive_bits);
                if v >= min_inc && v < max_exc {
                    self.eval_point_stateless(*when_in_range, block_x, block_y, block_z)
                } else {
                    self.eval_point_stateless(*when_out_of_range, block_x, block_y, block_z)
                }
            }
            DensityOp::Noise {
                noise,
                xz_scale_bits,
                y_scale_bits,
            } => {
                let xz_scale = f64::from_bits(*xz_scale_bits);
                let y_scale = f64::from_bits(*y_scale_bits);
                self.noises[noise.0 as usize].get_value(
                    f64::from(block_x) * xz_scale,
                    f64::from(block_y) * y_scale,
                    f64::from(block_z) * xz_scale,
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
                let xz_scale = f64::from_bits(*xz_scale_bits);
                let y_scale = f64::from_bits(*y_scale_bits);
                let dx = f64::from(block_x) * xz_scale
                    + self.eval_point_stateless(*shift_x, block_x, block_y, block_z);
                let dy = f64::from(block_y) * y_scale
                    + self.eval_point_stateless(*shift_y, block_x, block_y, block_z);
                let dz = f64::from(block_z) * xz_scale
                    + self.eval_point_stateless(*shift_z, block_x, block_y, block_z);
                self.noises[noise.0 as usize].get_value(dx, dy, dz)
            }
            DensityOp::ShiftA { noise } => {
                self.noises[noise.0 as usize].get_value(
                    f64::from(block_x) * 0.25,
                    0.0,
                    f64::from(block_z) * 0.25,
                ) * 4.0
            }
            DensityOp::ShiftB { noise } => {
                self.noises[noise.0 as usize].get_value(
                    f64::from(block_z) * 0.25,
                    f64::from(block_x) * 0.25,
                    0.0,
                ) * 4.0
            }
            DensityOp::Shift { noise } => {
                self.noises[noise.0 as usize].get_value(
                    f64::from(block_x) * 0.25,
                    f64::from(block_y) * 0.25,
                    f64::from(block_z) * 0.25,
                ) * 4.0
            }
            DensityOp::WeirdScaledSampler {
                input,
                noise,
                mapper,
            } => {
                let v = self.eval_point_stateless(*input, block_x, block_y, block_z);
                let rarity = mapper.map(v);
                rarity
                    * self.noises[noise.0 as usize]
                        .get_value(
                            f64::from(block_x) / rarity,
                            f64::from(block_y) / rarity,
                            f64::from(block_z) / rarity,
                        )
                        .abs()
            }
            DensityOp::OldBlendedNoise(id) => self.blended_noises[id.0 as usize].sample(
                f64::from(block_x),
                f64::from(block_y),
                f64::from(block_z),
            ),
            DensityOp::EndIslands => {
                let simplex = self
                    .end_islands_noise
                    .as_ref()
                    .expect("end_islands_noise initialized");
                eval_end_islands(simplex, block_x, block_z)
            }
            DensityOp::FindTopSurface {
                density,
                upper_bound,
                lower_bound,
                cell_height,
            } => {
                let upper = self.eval_point_stateless(*upper_bound, block_x, block_y, block_z);
                let top_y =
                    ((upper / f64::from(*cell_height)).floor() as i32).saturating_mul(*cell_height);
                if top_y <= *lower_bound {
                    return f64::from(*lower_bound);
                }
                let mut y = top_y;
                while y >= *lower_bound {
                    if self.eval_point_stateless(*density, block_x, y, block_z) > 0.0 {
                        return f64::from(y);
                    }
                    y -= *cell_height;
                }
                f64::from(*lower_bound)
            }
            DensityOp::Spline(spline) => {
                let val_f32 = spline.apply_with(&mut |coord| {
                    self.eval_point_stateless(coord, block_x, block_y, block_z)
                });
                f64::from(val_f32)
            }
        }
    }

    /// Samples the 6 climate channels of `router` at quart coordinates `(quart_x, quart_y, quart_z)` without a
    /// `NoiseChunk` (`RandomState.sampler().sample(quartX, quartY, quartZ)`).
    pub fn sample_climate_stateless(
        &self,
        router: &CompiledNoiseRouter,
        quart_x: i32,
        quart_y: i32,
        quart_z: i32,
    ) -> TargetPoint {
        let bx = quart_x << 2;
        let by = quart_y << 2;
        let bz = quart_z << 2;
        let temperature =
            quantize_coord(self.eval_point_stateless(router.temperature, bx, by, bz) as f32);
        let humidity =
            quantize_coord(self.eval_point_stateless(router.vegetation, bx, by, bz) as f32);
        let continentalness =
            quantize_coord(self.eval_point_stateless(router.continents, bx, by, bz) as f32);
        let erosion = quantize_coord(self.eval_point_stateless(router.erosion, bx, by, bz) as f32);
        let depth = quantize_coord(self.eval_point_stateless(router.depth, bx, by, bz) as f32);
        let weirdness = quantize_coord(self.eval_point_stateless(router.ridges, bx, by, bz) as f32);
        TargetPoint {
            temperature,
            humidity,
            continentalness,
            erosion,
            depth,
            weirdness,
        }
    }
}

/// Vanilla's `Mth.clampedMap(double value, double fromMin, double fromMax, double toMin, double toMax)`.
#[inline]
pub(crate) fn y_clamped_gradient(
    block_y: i32,
    from_y: i32,
    to_y: i32,
    from_value: f64,
    to_value: f64,
) -> f64 {
    let delta = (f64::from(block_y) - f64::from(from_y)) / (f64::from(to_y) - f64::from(from_y));
    if delta < 0.0 {
        from_value
    } else if delta > 1.0 {
        to_value
    } else {
        from_value + delta * (to_value - from_value)
    }
}

/// Vanilla's `DensityFunctions.EndIslandDensityFunction.compute`.
pub(crate) fn eval_end_islands(simplex: &SimplexNoise, block_x: i32, block_z: i32) -> f64 {
    let x = block_x / 8;
    let z = block_z / 8;
    let section_x = x / 2;
    let section_z = z / 2;
    let sub_x = x % 2;
    let sub_z = z % 2;
    let dist_sq = i64::from(x) * i64::from(x) + i64::from(z) * i64::from(z);
    let mut height = 100.0_f32 - (dist_sq as f32).sqrt() * 8.0_f32;
    height = height.clamp(-100.0_f32, 80.0_f32);

    for ox in -12..=12i32 {
        for oz in -12..=12i32 {
            let gx = i64::from(section_x + ox);
            let gz = i64::from(section_z + oz);
            if gx * gx + gz * gz > 4096
                && simplex.get_value_2d(gx as f64, gz as f64) < f64::from(-0.9_f32)
            {
                let scale = ((gx.abs() as f32) * 3439.0_f32 + (gz.abs() as f32) * 147.0_f32)
                    % 13.0_f32
                    + 9.0_f32;
                let dx = (sub_x - ox * 2) as f32;
                let dz = (sub_z - oz * 2) as f32;
                let mut island = 100.0_f32 - (dx * dx + dz * dz).sqrt() * scale;
                island = island.clamp(-100.0_f32, 80.0_f32);
                height = height.max(island);
            }
        }
    }

    (f64::from(height) - 8.0) / 128.0
}
