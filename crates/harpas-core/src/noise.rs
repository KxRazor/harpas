//! Deterministic gradient and simplex noise samplers identical to vanilla's
//!
//! World generation builds terrain density, climate fields, aquifers, ore veins and surface patterns on two
//! noise families (`docs/engine-core.md#7-determinism`):
//! - Ken Perlin's 2002 improved 3D gradient noise ([`ImprovedNoise`]), layered into multi-octave
//!   [`PerlinNoise`], paired and frequency-offset in [`NormalNoise`], and blended across three legacy samplers
//!   in [`BlendedNoise`];
//! - 2D and 3D simplex noise ([`SimplexNoise`]) and its multi-octave wrapper ([`PerlinSimplexNoise`]).
//!
//! Floating-point operations preserve Java's evaluation order, constants (such as `(double) 1.0e-7f` in
//! [`ImprovedNoise`]'s vertical smear step) and [`RandomSource`] consumption counts.

use crate::math::{clamped_lerp, floor_f64, lerp3, lfloor, smoothstep};
use crate::random::{LegacyRandom, RandomSource};

/// `(double) 1.0E-7F` used by vanilla's `ImprovedNoise` vertical step quantization (`0x3e7ad7f2a0000000`).
const SHIFT_UP_EPSILON: f64 = 1.0e-7_f32 as f64;

/// Coordinate wrap period `2^25` applied by [`PerlinNoise::wrap`] to avoid far-coordinate precision loss.
const PERLIN_WRAP_PERIOD: f64 = 33_554_432.0;

/// Frequency ratio `337 / 331` offsetting the second [`PerlinNoise`] inside [`NormalNoise`].
const NORMAL_INPUT_FACTOR: f64 = 1.018_126_888_217_522_7;

/// Target standard deviation `1 / 6` of [`NormalNoise`].
const NORMAL_TARGET_DEVIATION: f64 = 1.0 / 6.0;

/// Base coordinate multiplier of [`BlendedNoise`].
const BLENDED_COORDINATE_SCALE: f64 = 684.412;

/// Number of `next_int` draws consumed when a legacy octave is skipped (`6` for three doubles + `256` swaps).
const SKIPPED_OCTAVE_DRAWS: u32 = 262;

/// `1 / 3`, the 3D simplex skew factor `(√4 − 1) / 3`.
const SIMPLEX_F3: f64 = 1.0 / 3.0;

/// `1 / 6`, the 3D simplex unskew factor `(1 − 1 / √4) / 3`.
const SIMPLEX_G3: f64 = 1.0 / 6.0;

/// Perlin's 16 3D cube-edge gradient vectors; the first 12 distinct edges also feed [`SimplexNoise`].
const GRADIENT: [[i32; 3]; 16] = [
    [1, 1, 0],
    [-1, 1, 0],
    [1, -1, 0],
    [-1, -1, 0],
    [1, 0, 1],
    [-1, 0, 1],
    [1, 0, -1],
    [-1, 0, -1],
    [0, 1, 1],
    [0, -1, 1],
    [0, 1, -1],
    [0, -1, -1],
    [1, 1, 0],
    [0, -1, 1],
    [-1, 1, 0],
    [0, -1, -1],
];

#[inline]
const fn dot(gradient: [i32; 3], x: f64, y: f64, z: f64) -> f64 {
    (gradient[0] as f64) * x + (gradient[1] as f64) * y + (gradient[2] as f64) * z
}

#[inline]
const fn grad_dot(hash: i32, x: f64, y: f64, z: f64) -> f64 {
    dot(GRADIENT[(hash & 15) as usize], x, y, z)
}

fn init_origin_and_permutation<R: RandomSource + ?Sized>(
    rng: &mut R,
) -> (f64, f64, f64, [u8; 256]) {
    let xo = rng.next_double() * 256.0;
    let yo = rng.next_double() * 256.0;
    let zo = rng.next_double() * 256.0;
    let mut p = [0_u8; 256];
    for (i, slot) in p.iter_mut().enumerate() {
        *slot = i as u8;
    }
    for i in 0..256_usize {
        let j = rng.next_int_bounded((256 - i) as i32) as usize;
        p.swap(i, i + j);
    }
    (xo, yo, zo, p)
}

/// Single-octave 3D improved Perlin gradient noise (`ImprovedNoise`).
#[derive(Debug, Clone, PartialEq)]
pub struct ImprovedNoise {
    p: [u8; 256],
    xo: f64,
    yo: f64,
    zo: f64,
}

impl ImprovedNoise {
    /// Seeds a new origin offset `(xo, yo, zo)` in `[0.0, 256.0)^3` and 256-entry permutation table from `rng`.
    pub fn new<R: RandomSource + ?Sized>(rng: &mut R) -> Self {
        let (xo, yo, zo, p) = init_origin_and_permutation(rng);
        Self { p, xo, yo, zo }
    }

    /// Random origin offset along X.
    pub const fn xo(&self) -> f64 {
        self.xo
    }

    /// Random origin offset along Y.
    pub const fn yo(&self) -> f64 {
        self.yo
    }

    /// Random origin offset along Z.
    pub const fn zo(&self) -> f64 {
        self.zo
    }

    #[inline]
    const fn perm(&self, index: i32) -> i32 {
        self.p[(index & 0xff) as usize] as i32
    }

    /// Evaluates 3D improved Perlin noise at `(x, y, z)` without vertical step smearing.
    pub fn noise(&self, x: f64, y: f64, z: f64) -> f64 {
        self.noise_with_y_scale(x, y, z, 0.0, 0.0)
    }

    /// Evaluates 3D improved Perlin noise at `(x, y, z)` with vanilla's vertical smearing `(y_scale, y_max)`.
    ///
    /// When `y_scale != 0.0`, the corner gradient vectors receive a quantized `local_y - y_fudge` offset while
    /// the quintic [`smoothstep`] interpolation weight still uses the unfudged `local_y`.
    pub fn noise_with_y_scale(&self, x: f64, y: f64, z: f64, y_scale: f64, y_max: f64) -> f64 {
        let x = x + self.xo;
        let y = y + self.yo;
        let z = z + self.zo;
        let grid_x = floor_f64(x);
        let grid_y = floor_f64(y);
        let grid_z = floor_f64(z);
        let local_x = x - (grid_x as f64);
        let local_y = y - (grid_y as f64);
        let local_z = z - (grid_z as f64);

        let y_fudge = if y_scale != 0.0 {
            let clamped_y = if y_max >= 0.0 && y_max < local_y {
                y_max
            } else {
                local_y
            };
            (floor_f64(clamped_y / y_scale + SHIFT_UP_EPSILON) as f64) * y_scale
        } else {
            0.0
        };

        self.sample_and_lerp(
            grid_x,
            grid_y,
            grid_z,
            local_x,
            local_y - y_fudge,
            local_z,
            local_y,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn sample_and_lerp(
        &self,
        grid_x: i32,
        grid_y: i32,
        grid_z: i32,
        dx: f64,
        weird_dy: f64,
        dz: f64,
        dy_for_fade: f64,
    ) -> f64 {
        let i = self.perm(grid_x);
        let j = self.perm(grid_x.wrapping_add(1));
        let k = self.perm(i.wrapping_add(grid_y));
        let l = self.perm(i.wrapping_add(grid_y).wrapping_add(1));
        let m = self.perm(j.wrapping_add(grid_y));
        let n = self.perm(j.wrapping_add(grid_y).wrapping_add(1));

        let d000 = grad_dot(self.perm(k.wrapping_add(grid_z)), dx, weird_dy, dz);
        let d100 = grad_dot(self.perm(m.wrapping_add(grid_z)), dx - 1.0, weird_dy, dz);
        let d010 = grad_dot(self.perm(l.wrapping_add(grid_z)), dx, weird_dy - 1.0, dz);
        let d110 = grad_dot(
            self.perm(n.wrapping_add(grid_z)),
            dx - 1.0,
            weird_dy - 1.0,
            dz,
        );
        let d001 = grad_dot(
            self.perm(k.wrapping_add(grid_z).wrapping_add(1)),
            dx,
            weird_dy,
            dz - 1.0,
        );
        let d101 = grad_dot(
            self.perm(m.wrapping_add(grid_z).wrapping_add(1)),
            dx - 1.0,
            weird_dy,
            dz - 1.0,
        );
        let d011 = grad_dot(
            self.perm(l.wrapping_add(grid_z).wrapping_add(1)),
            dx,
            weird_dy - 1.0,
            dz - 1.0,
        );
        let d111 = grad_dot(
            self.perm(n.wrapping_add(grid_z).wrapping_add(1)),
            dx - 1.0,
            weird_dy - 1.0,
            dz - 1.0,
        );

        let fx = smoothstep(dx);
        let fy = smoothstep(dy_for_fade);
        let fz = smoothstep(dz);
        lerp3(fx, fy, fz, d000, d100, d010, d110, d001, d101, d011, d111)
    }
}

/// Multi-octave 3D Perlin noise (`PerlinNoise`).
///
/// Leading and trailing `0.0` entries in `amplitudes` are preserved intentionally: their presence skips creating
/// an [`ImprovedNoise`] for that octave while still counting toward `amplitudes.len()` when computing the
/// lowest-frequency normalization factor `2^(N - 1) / (2^N - 1)`.
#[derive(Debug, Clone, PartialEq)]
pub struct PerlinNoise {
    noise_levels: Box<[Option<ImprovedNoise>]>,
    first_octave: i32,
    amplitudes: Box<[f64]>,
    lowest_freq_value_factor: f64,
    lowest_freq_input_factor: f64,
    max_value: f64,
}

impl PerlinNoise {
    /// Wraps `value` into `[-16_777_216.0, 16_777_216.0)` with period `2^25`, matching `PerlinNoise.wrap`.
    pub const fn wrap(value: f64) -> f64 {
        value - (lfloor(value / PERLIN_WRAP_PERIOD + 0.5) as f64) * PERLIN_WRAP_PERIOD
    }

    /// Creates a multi-octave sampler using vanilla's modern positional seeding (`"octave_<k>"`).
    ///
    /// # Panics
    ///
    /// Panics if `amplitudes` is empty.
    pub fn new<R: RandomSource + ?Sized>(
        rng: &mut R,
        first_octave: i32,
        amplitudes: &[f64],
    ) -> Self {
        assert!(
            !amplitudes.is_empty(),
            "PerlinNoise requires at least one amplitude"
        );
        let positional = rng.fork_positional_source();
        let mut noise_levels = Vec::with_capacity(amplitudes.len());
        for (i, &amplitude) in amplitudes.iter().enumerate() {
            if amplitude != 0.0 {
                let octave = first_octave.wrapping_add(i as i32);
                let mut octave_rng = positional.from_hash_of(&format!("octave_{octave}"));
                noise_levels.push(Some(ImprovedNoise::new(&mut octave_rng)));
            } else {
                noise_levels.push(None);
            }
        }
        Self::finish(noise_levels.into_boxed_slice(), first_octave, amplitudes)
    }

    /// Creates a multi-octave sampler using vanilla's sequential legacy seeding (`PerlinNoise.createLegacyFor...`).
    ///
    /// Octave `0` is always drawn from `rng` first, followed by octaves `-1` down to `first_octave`, advancing
    /// `rng` by 262 [`RandomSource::next_int`] calls whenever an octave is out of range or has zero amplitude.
    ///
    /// # Panics
    ///
    /// Panics if `amplitudes` is empty or contains a non-zero amplitude at a positive octave (`> 0`).
    pub fn new_legacy<R: RandomSource + ?Sized>(
        rng: &mut R,
        first_octave: i32,
        amplitudes: &[f64],
    ) -> Self {
        assert!(
            !amplitudes.is_empty(),
            "PerlinNoise requires at least one amplitude"
        );
        let n = amplitudes.len();
        let zero_idx = -first_octave;
        let mut noise_levels: Vec<Option<ImprovedNoise>> = vec![None; n];

        let zero_noise = ImprovedNoise::new(rng);
        if zero_idx >= 0 && (zero_idx as usize) < n && amplitudes[zero_idx as usize] != 0.0 {
            noise_levels[zero_idx as usize] = Some(zero_noise);
        }

        let positive_start = (zero_idx + 1).max(0) as usize;
        for (idx, &amp) in amplitudes.iter().enumerate().skip(positive_start) {
            assert!(
                amp == 0.0,
                "legacy PerlinNoise does not support positive octave {}",
                first_octave + (idx as i32)
            );
        }

        if zero_idx > 0 {
            for k in (0..zero_idx).rev() {
                let ku = k as usize;
                if ku < n && amplitudes[ku] != 0.0 {
                    noise_levels[ku] = Some(ImprovedNoise::new(rng));
                } else {
                    rng.consume_count(SKIPPED_OCTAVE_DRAWS);
                }
            }
        }

        Self::finish(noise_levels.into_boxed_slice(), first_octave, amplitudes)
    }

    fn finish(
        noise_levels: Box<[Option<ImprovedNoise>]>,
        first_octave: i32,
        amplitudes: &[f64],
    ) -> Self {
        let n = amplitudes.len() as i32;
        let lowest_freq_input_factor = 2.0_f64.powi(first_octave);
        let lowest_freq_value_factor = 2.0_f64.powi(n - 1) / (2.0_f64.powi(n) - 1.0);
        let mut sampler = Self {
            noise_levels,
            first_octave,
            amplitudes: amplitudes.to_vec().into_boxed_slice(),
            lowest_freq_value_factor,
            lowest_freq_input_factor,
            max_value: 0.0,
        };
        sampler.max_value = sampler.edge_value(2.0);
        sampler
    }

    /// Lowest-frequency octave exponent (`2^first_octave`).
    pub const fn first_octave(&self) -> i32 {
        self.first_octave
    }

    /// Per-octave amplitude weights starting at [`Self::first_octave`].
    pub fn amplitudes(&self) -> &[f64] {
        &self.amplitudes
    }

    /// Upper bound `edge_value(2.0)` used by density-function range propagation.
    pub const fn max_value(&self) -> f64 {
        self.max_value
    }

    /// Upper bound `edge_value(y_scale + 2.0)` accounting for vertical smearing (`maxBrokenValue`).
    pub fn max_broken_value(&self, y_scale: f64) -> f64 {
        self.edge_value(y_scale + 2.0)
    }

    /// Accumulates `amplitude * edge * value_factor` across active octaves (`PerlinNoise.edgeValue`).
    pub fn edge_value(&self, edge: f64) -> f64 {
        let mut total = 0.0;
        let mut value_factor = self.lowest_freq_value_factor;
        for (i, level) in self.noise_levels.iter().enumerate() {
            if level.is_some() {
                total += self.amplitudes[i] * edge * value_factor;
            }
            value_factor /= 2.0;
        }
        total
    }

    /// Returns the `index`-th octave counting backward from the highest frequency (`getOctaveNoise`).
    pub fn octave_noise(&self, index: usize) -> Option<&ImprovedNoise> {
        let slot = self.noise_levels.len().checked_sub(1 + index)?;
        self.noise_levels[slot].as_ref()
    }

    /// Evaluates the multi-octave Perlin noise at `(x, y, z)`.
    pub fn get_value(&self, x: f64, y: f64, z: f64) -> f64 {
        self.get_value_with_y_scale(x, y, z, 0.0, 0.0, false)
    }

    /// Evaluates the multi-octave Perlin noise with vertical smearing and optional Y-origin pinning.
    ///
    /// When `use_origin` is `true`, each octave samples at `y = -octave.yo()` so that `y + yo == 0.0` (used by
    /// legacy 2D surface noise slices).
    pub fn get_value_with_y_scale(
        &self,
        x: f64,
        y: f64,
        z: f64,
        y_scale: f64,
        y_max: f64,
        use_origin: bool,
    ) -> f64 {
        let mut total = 0.0;
        let mut input_factor = self.lowest_freq_input_factor;
        let mut value_factor = self.lowest_freq_value_factor;
        for (i, level) in self.noise_levels.iter().enumerate() {
            if let Some(noise) = level {
                let sample = noise.noise_with_y_scale(
                    Self::wrap(x * input_factor),
                    if use_origin {
                        -noise.yo()
                    } else {
                        Self::wrap(y * input_factor)
                    },
                    Self::wrap(z * input_factor),
                    y_scale * input_factor,
                    y_max * input_factor,
                );
                total += self.amplitudes[i] * sample * value_factor;
            }
            input_factor *= 2.0;
            value_factor /= 2.0;
        }
        total
    }
}

/// Octave definition from `data/<namespace>/worldgen/noise/<name>.json` (`NormalNoise.NoiseParameters`).
#[derive(Debug, Clone, PartialEq)]
pub struct NoiseParameters {
    /// Exponent of two (`2^first_octave`) for the lowest-frequency octave.
    pub first_octave: i32,
    /// Per-octave weights starting at `first_octave`, preserving zero entries.
    pub amplitudes: Vec<f64>,
}

impl NoiseParameters {
    /// Creates noise parameters from a starting octave and amplitude list.
    pub fn new(first_octave: i32, amplitudes: impl Into<Vec<f64>>) -> Self {
        Self {
            first_octave,
            amplitudes: amplitudes.into(),
        }
    }
}

/// Sum of two frequency-offset [`PerlinNoise`] samplers scaled to target deviation `1 / 6` (`NormalNoise`).
#[derive(Debug, Clone, PartialEq)]
pub struct NormalNoise {
    value_factor: f64,
    first: PerlinNoise,
    second: PerlinNoise,
    max_value: f64,
    parameters: NoiseParameters,
}

impl NormalNoise {
    /// Creates a [`NormalNoise`] sampler using modern positional octave seeding.
    ///
    /// # Panics
    ///
    /// Panics if `amplitudes` is empty.
    pub fn new<R: RandomSource + ?Sized>(
        rng: &mut R,
        first_octave: i32,
        amplitudes: &[f64],
    ) -> Self {
        let first = PerlinNoise::new(rng, first_octave, amplitudes);
        let second = PerlinNoise::new(rng, first_octave, amplitudes);
        Self::from_parts(first, second, first_octave, amplitudes)
    }

    /// Creates a [`NormalNoise`] sampler from [`NoiseParameters`] using modern positional octave seeding.
    pub fn from_parameters<R: RandomSource + ?Sized>(
        rng: &mut R,
        parameters: &NoiseParameters,
    ) -> Self {
        Self::new(rng, parameters.first_octave, &parameters.amplitudes)
    }

    /// Creates a [`NormalNoise`] sampler using sequential legacy octave seeding (`createLegacyNetherBiome`).
    ///
    /// # Panics
    ///
    /// Panics if `amplitudes` is empty or contains a non-zero positive octave.
    pub fn new_legacy<R: RandomSource + ?Sized>(
        rng: &mut R,
        first_octave: i32,
        amplitudes: &[f64],
    ) -> Self {
        let first = PerlinNoise::new_legacy(rng, first_octave, amplitudes);
        let second = PerlinNoise::new_legacy(rng, first_octave, amplitudes);
        Self::from_parts(first, second, first_octave, amplitudes)
    }

    fn from_parts(
        first: PerlinNoise,
        second: PerlinNoise,
        first_octave: i32,
        amplitudes: &[f64],
    ) -> Self {
        let mut min_idx = usize::MAX;
        let mut max_idx = 0_usize;
        for (i, &amp) in amplitudes.iter().enumerate() {
            if amp != 0.0 {
                min_idx = min_idx.min(i);
                max_idx = max_idx.max(i);
            }
        }
        let active_span = if min_idx <= max_idx {
            (max_idx - min_idx) as i32
        } else {
            0
        };
        let value_factor = NORMAL_TARGET_DEVIATION / Self::expected_deviation(active_span);
        let max_value = (first.max_value() + second.max_value()) * value_factor;
        Self {
            value_factor,
            first,
            second,
            max_value,
            parameters: NoiseParameters::new(first_octave, amplitudes),
        }
    }

    fn expected_deviation(octave_span: i32) -> f64 {
        0.1 * (1.0 + 1.0 / ((octave_span + 1) as f64))
    }

    /// Upper bound `(first.max_value() + second.max_value()) * value_factor`.
    pub const fn max_value(&self) -> f64 {
        self.max_value
    }

    /// Parameters used to build this sampler.
    pub const fn parameters(&self) -> &NoiseParameters {
        &self.parameters
    }

    /// Evaluates the paired Perlin samplers at `(x, y, z)` and `(x, y, z) * (337 / 331)`.
    pub fn get_value(&self, x: f64, y: f64, z: f64) -> f64 {
        let x2 = x * NORMAL_INPUT_FACTOR;
        let y2 = y * NORMAL_INPUT_FACTOR;
        let z2 = z * NORMAL_INPUT_FACTOR;
        (self.first.get_value(x, y, z) + self.second.get_value(x2, y2, z2)) * self.value_factor
    }
}

/// Legacy three-sampler 3D terrain noise (`BlendedNoise` / `minecraft:old_blended_noise`).
#[derive(Debug, Clone, PartialEq)]
pub struct BlendedNoise {
    min_limit_noise: PerlinNoise,
    max_limit_noise: PerlinNoise,
    main_noise: PerlinNoise,
    xz_multiplier: f64,
    y_multiplier: f64,
    xz_factor: f64,
    y_factor: f64,
    smear_scale_multiplier: f64,
    max_value: f64,
    xz_scale: f64,
    y_scale: f64,
}

impl BlendedNoise {
    /// Seeds `min_limit_noise` (`16` octaves at `-15`), `max_limit_noise` (`16` octaves at `-15`), and
    /// `main_noise` (`8` octaves at `-7`) sequentially from `rng` using legacy octave order.
    pub fn new<R: RandomSource + ?Sized>(
        rng: &mut R,
        xz_scale: f64,
        y_scale: f64,
        xz_factor: f64,
        y_factor: f64,
        smear_scale_multiplier: f64,
    ) -> Self {
        let min_limit_noise = PerlinNoise::new_legacy(rng, -15, &[1.0; 16]);
        let max_limit_noise = PerlinNoise::new_legacy(rng, -15, &[1.0; 16]);
        let main_noise = PerlinNoise::new_legacy(rng, -7, &[1.0; 8]);
        let xz_multiplier = BLENDED_COORDINATE_SCALE * xz_scale;
        let y_multiplier = BLENDED_COORDINATE_SCALE * y_scale;
        let max_value = min_limit_noise.max_broken_value(y_multiplier);
        Self {
            min_limit_noise,
            max_limit_noise,
            main_noise,
            xz_multiplier,
            y_multiplier,
            xz_factor,
            y_factor,
            smear_scale_multiplier,
            max_value,
            xz_scale,
            y_scale,
        }
    }

    /// Re-seeds a [`BlendedNoise`] instance with the same scale and factor parameters (`withNewRandom`).
    pub fn with_new_random<R: RandomSource + ?Sized>(&self, rng: &mut R) -> Self {
        Self::new(
            rng,
            self.xz_scale,
            self.y_scale,
            self.xz_factor,
            self.y_factor,
            self.smear_scale_multiplier,
        )
    }

    /// Upper bound `min_limit_noise.max_broken_value(684.412 * y_scale)`.
    pub const fn max_value(&self) -> f64 {
        self.max_value
    }

    /// Horizontal scale parameter (`xz_scale`).
    pub const fn xz_scale(&self) -> f64 {
        self.xz_scale
    }

    /// Vertical scale parameter (`y_scale`).
    pub const fn y_scale(&self) -> f64 {
        self.y_scale
    }

    /// Horizontal main-noise divisor (`xz_factor`).
    pub const fn xz_factor(&self) -> f64 {
        self.xz_factor
    }

    /// Vertical main-noise divisor (`y_factor`).
    pub const fn y_factor(&self) -> f64 {
        self.y_factor
    }

    /// Vertical smear multiplier (`smear_scale_multiplier`).
    pub const fn smear_scale_multiplier(&self) -> f64 {
        self.smear_scale_multiplier
    }

    /// Evaluates blended 3D terrain noise at block coordinates `(x, y, z)`.
    pub fn sample(&self, x: f64, y: f64, z: f64) -> f64 {
        let limit_x = x * self.xz_multiplier;
        let limit_y = y * self.y_multiplier;
        let limit_z = z * self.xz_multiplier;
        let main_x = limit_x / self.xz_factor;
        let main_y = limit_y / self.y_factor;
        let main_z = limit_z / self.xz_factor;
        let smear_limit = self.y_multiplier * self.smear_scale_multiplier;
        let smear_main = smear_limit / self.y_factor;

        let mut main_val = 0.0;
        let mut scale = 1.0;
        for i in 0..8 {
            if let Some(octave) = self.main_noise.octave_noise(i) {
                main_val += octave.noise_with_y_scale(
                    PerlinNoise::wrap(main_x * scale),
                    PerlinNoise::wrap(main_y * scale),
                    PerlinNoise::wrap(main_z * scale),
                    smear_main * scale,
                    main_y * scale,
                ) / scale;
            }
            scale /= 2.0;
        }

        let blend = (main_val / 10.0 + 1.0) / 2.0;
        let skip_max = blend <= 0.0;
        let skip_min = blend >= 1.0;
        let mut min_val = 0.0;
        let mut max_val = 0.0;
        scale = 1.0;
        for i in 0..16 {
            let wx = PerlinNoise::wrap(limit_x * scale);
            let wy = PerlinNoise::wrap(limit_y * scale);
            let wz = PerlinNoise::wrap(limit_z * scale);
            let y_smear = smear_limit * scale;
            if !skip_min && let Some(octave) = self.min_limit_noise.octave_noise(i) {
                min_val += octave.noise_with_y_scale(wx, wy, wz, y_smear, limit_y * scale) / scale;
            }
            if !skip_max && let Some(octave) = self.max_limit_noise.octave_noise(i) {
                max_val += octave.noise_with_y_scale(wx, wy, wz, y_smear, limit_y * scale) / scale;
            }
            scale /= 2.0;
        }

        clamped_lerp(min_val / 512.0, max_val / 512.0, blend) / 128.0
    }
}

/// Single-octave 2D and 3D simplex noise (`SimplexNoise`).
///
/// Unlike [`ImprovedNoise`], [`Self::get_value_2d`] and [`Self::get_value_3d`] do not add `(xo, yo, zo)`
/// automatically; callers such as `end_islands` sample around `(0, 0)` directly while [`PerlinSimplexNoise`]
/// adds `(xo, yo)` when `use_offsets` is enabled.
#[derive(Debug, Clone, PartialEq)]
pub struct SimplexNoise {
    p: [u8; 256],
    xo: f64,
    yo: f64,
    zo: f64,
}

impl SimplexNoise {
    /// Seeds `(xo, yo, zo)` in `[0.0, 256.0)^3` and a 256-entry permutation table from `rng`.
    pub fn new<R: RandomSource + ?Sized>(rng: &mut R) -> Self {
        let (xo, yo, zo, p) = init_origin_and_permutation(rng);
        Self { p, xo, yo, zo }
    }

    /// Random origin offset along X.
    pub const fn xo(&self) -> f64 {
        self.xo
    }

    /// Random origin offset along Y.
    pub const fn yo(&self) -> f64 {
        self.yo
    }

    /// Random origin offset along Z.
    pub const fn zo(&self) -> f64 {
        self.zo
    }

    #[inline]
    const fn perm(&self, index: i32) -> i32 {
        self.p[(index & 0xff) as usize] as i32
    }

    #[inline]
    fn corner_noise_3d(grad_idx: usize, x: f64, y: f64, z: f64, falloff: f64) -> f64 {
        let mut t = falloff - x * x - y * y - z * z;
        if t < 0.0 {
            0.0
        } else {
            t *= t;
            t * t * dot(GRADIENT[grad_idx], x, y, z)
        }
    }

    /// Evaluates 2D simplex noise at `(x, y)`, scaled by `70.0` into approximately `[-1.0, 1.0]`.
    pub fn get_value_2d(&self, x: f64, y: f64) -> f64 {
        let sqrt_3 = 3.0_f64.sqrt();
        let f2 = 0.5 * (sqrt_3 - 1.0);
        let g2 = (3.0 - sqrt_3) / 6.0;

        let s = (x + y) * f2;
        let i = floor_f64(x + s);
        let j = floor_f64(y + s);
        let t = (i.wrapping_add(j) as f64) * g2;
        let x0_origin = (i as f64) - t;
        let y0_origin = (j as f64) - t;
        let x0 = x - x0_origin;
        let y0 = y - y0_origin;

        let (i1, j1) = if x0 > y0 { (1, 0) } else { (0, 1) };
        let x1 = x0 - (i1 as f64) + g2;
        let y1 = y0 - (j1 as f64) + g2;
        let x2 = x0 - 1.0 + 2.0 * g2;
        let y2 = y0 - 1.0 + 2.0 * g2;

        let ii = i & 0xff;
        let jj = j & 0xff;
        let gi0 = (self.perm(ii + self.perm(jj)) % 12) as usize;
        let gi1 = (self.perm(ii + i1 + self.perm(jj + j1)) % 12) as usize;
        let gi2 = (self.perm(ii + 1 + self.perm(jj + 1)) % 12) as usize;

        let n0 = Self::corner_noise_3d(gi0, x0, y0, 0.0, 0.5);
        let n1 = Self::corner_noise_3d(gi1, x1, y1, 0.0, 0.5);
        let n2 = Self::corner_noise_3d(gi2, x2, y2, 0.0, 0.5);
        70.0 * (n0 + n1 + n2)
    }

    /// Evaluates 3D simplex noise at `(x, y, z)`, scaled by `32.0` into approximately `[-1.0, 1.0]`.
    pub fn get_value_3d(&self, x: f64, y: f64, z: f64) -> f64 {
        let s = (x + y + z) * SIMPLEX_F3;
        let i = floor_f64(x + s);
        let j = floor_f64(y + s);
        let k = floor_f64(z + s);
        let t = (i.wrapping_add(j).wrapping_add(k) as f64) * SIMPLEX_G3;
        let x0_origin = (i as f64) - t;
        let y0_origin = (j as f64) - t;
        let z0_origin = (k as f64) - t;
        let x0 = x - x0_origin;
        let y0 = y - y0_origin;
        let z0 = z - z0_origin;

        let (i1, j1, k1, i2, j2, k2) = if x0 >= y0 {
            if y0 >= z0 {
                (1, 0, 0, 1, 1, 0)
            } else if x0 >= z0 {
                (1, 0, 0, 1, 0, 1)
            } else {
                (0, 0, 1, 1, 0, 1)
            }
        } else if y0 < z0 {
            (0, 0, 1, 0, 1, 1)
        } else if x0 < z0 {
            (0, 1, 0, 0, 1, 1)
        } else {
            (0, 1, 0, 1, 1, 0)
        };

        let x1 = x0 - (i1 as f64) + SIMPLEX_G3;
        let y1 = y0 - (j1 as f64) + SIMPLEX_G3;
        let z1 = z0 - (k1 as f64) + SIMPLEX_G3;
        let x2 = x0 - (i2 as f64) + 2.0 * SIMPLEX_G3;
        let y2 = y0 - (j2 as f64) + 2.0 * SIMPLEX_G3;
        let z2 = z0 - (k2 as f64) + 2.0 * SIMPLEX_G3;
        let x3 = x0 - 1.0 + 3.0 * SIMPLEX_G3;
        let y3 = y0 - 1.0 + 3.0 * SIMPLEX_G3;
        let z3 = z0 - 1.0 + 3.0 * SIMPLEX_G3;

        let ii = i & 0xff;
        let jj = j & 0xff;
        let kk = k & 0xff;
        let gi0 = (self.perm(ii + self.perm(jj + self.perm(kk))) % 12) as usize;
        let gi1 = (self.perm(ii + i1 + self.perm(jj + j1 + self.perm(kk + k1))) % 12) as usize;
        let gi2 = (self.perm(ii + i2 + self.perm(jj + j2 + self.perm(kk + k2))) % 12) as usize;
        let gi3 = (self.perm(ii + 1 + self.perm(jj + 1 + self.perm(kk + 1))) % 12) as usize;

        let n0 = Self::corner_noise_3d(gi0, x0, y0, z0, 0.6);
        let n1 = Self::corner_noise_3d(gi1, x1, y1, z1, 0.6);
        let n2 = Self::corner_noise_3d(gi2, x2, y2, z2, 0.6);
        let n3 = Self::corner_noise_3d(gi3, x3, y3, z3, 0.6);
        32.0 * (n0 + n1 + n2 + n3)
    }
}

/// Multi-octave 2D simplex noise (`PerlinSimplexNoise`), used by badlands clay bands and biome foliage noise.
#[derive(Debug, Clone, PartialEq)]
pub struct PerlinSimplexNoise {
    noise_levels: Box<[Option<SimplexNoise>]>,
    highest_freq_value_factor: f64,
    highest_freq_input_factor: f64,
}

impl PerlinSimplexNoise {
    /// Creates a multi-octave 2D simplex sampler from a set of integer octave exponents (for example `&[0]` or
    /// `&[-3, -2, -1, 0]`).
    ///
    /// # Panics
    ///
    /// Panics if `octaves` is empty.
    pub fn new<R: RandomSource + ?Sized>(rng: &mut R, octaves: &[i32]) -> Self {
        assert!(
            !octaves.is_empty(),
            "PerlinSimplexNoise requires at least one octave"
        );
        let mut sorted = octaves.to_vec();
        sorted.sort_unstable();
        sorted.dedup();

        let low_freq_octaves = -sorted[0];
        let high_freq_octaves = *sorted.last().expect("non-empty octaves");
        let total_octaves = low_freq_octaves + high_freq_octaves + 1;
        assert!(
            total_octaves >= 1,
            "total number of octaves must be at least 1"
        );

        let first_noise = SimplexNoise::new(rng);
        let zero_idx = high_freq_octaves;
        let total_usize = total_octaves as usize;
        let mut noise_levels: Vec<Option<SimplexNoise>> = vec![None; total_usize];

        if zero_idx >= 0 && (zero_idx as usize) < total_usize && sorted.binary_search(&0).is_ok() {
            noise_levels[zero_idx as usize] = Some(first_noise.clone());
        }

        for (idx, slot) in noise_levels
            .iter_mut()
            .enumerate()
            .skip((zero_idx + 1).max(0) as usize)
        {
            let octave = zero_idx - (idx as i32);
            if sorted.binary_search(&octave).is_ok() {
                *slot = Some(SimplexNoise::new(rng));
            } else {
                rng.consume_count(SKIPPED_OCTAVE_DRAWS);
            }
        }

        if high_freq_octaves > 0 {
            let positive_seed =
                (first_noise.get_value_3d(first_noise.xo(), first_noise.yo(), first_noise.zo())
                    * (i64::MAX as f64)) as i64;
            let mut high_freq_rng = LegacyRandom::new(positive_seed);
            for idx in (0..zero_idx).rev() {
                let ku = idx as usize;
                let octave = zero_idx - idx;
                if ku < total_usize && sorted.binary_search(&octave).is_ok() {
                    noise_levels[ku] = Some(SimplexNoise::new(&mut high_freq_rng));
                } else {
                    high_freq_rng.consume_count(SKIPPED_OCTAVE_DRAWS);
                }
            }
        }

        let highest_freq_input_factor = 2.0_f64.powi(high_freq_octaves);
        let highest_freq_value_factor = 1.0 / (2.0_f64.powi(total_octaves) - 1.0);
        Self {
            noise_levels: noise_levels.into_boxed_slice(),
            highest_freq_value_factor,
            highest_freq_input_factor,
        }
    }

    /// Evaluates 2D multi-octave simplex noise at `(x, y)`, optionally adding each octave's `(xo, yo)` offset.
    pub fn get_value(&self, x: f64, y: f64, use_offsets: bool) -> f64 {
        let mut total = 0.0;
        let mut input_factor = self.highest_freq_input_factor;
        let mut value_factor = self.highest_freq_value_factor;
        for level in &self.noise_levels {
            if let Some(simplex) = level {
                let sx = x * input_factor + if use_offsets { simplex.xo } else { 0.0 };
                let sy = y * input_factor + if use_offsets { simplex.yo } else { 0.0 };
                total += simplex.get_value_2d(sx, sy) * value_factor;
            }
            input_factor /= 2.0;
            value_factor *= 2.0;
        }
        total
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::random::XoroshiroRandom;

    #[test]
    fn shift_up_epsilon_matches_java_float_to_double_widening() {
        assert_eq!(SHIFT_UP_EPSILON.to_bits(), 0x3e7a_d7f2_a000_0000);
        assert_ne!(SHIFT_UP_EPSILON.to_bits(), 1.0e-7_f64.to_bits());
    }

    #[test]
    fn improved_noise_stays_bounded_and_respects_y_scale_smearing() {
        let mut rng = XoroshiroRandom::new(2026);
        let noise = ImprovedNoise::new(&mut rng);
        for ix in -16..=16 {
            for iy in -8..=8 {
                for iz in -16..=16 {
                    let v = noise.noise(ix as f64 * 0.37, iy as f64 * 0.53, iz as f64 * 0.29);
                    assert!((-1.0..=1.0).contains(&v), "out-of-range sample {v}");
                }
            }
        }
        let plain = noise.noise(12.34, 56.78, -90.12);
        let smeared = noise.noise_with_y_scale(12.34, 56.78, -90.12, 0.25, 56.78);
        assert_ne!(plain.to_bits(), smeared.to_bits());
    }

    #[test]
    fn perlin_noise_preserves_trailing_zero_amplitudes_in_normalization_factor() {
        // `ridge.json` in vanilla 26.1 has trailing zeros `[-7, [1, 2, 1, 0, 0, 0]]`, which scale by 32/63
        // rather than 4/7.
        let mut rng_a = XoroshiroRandom::new(42);
        let mut rng_b = XoroshiroRandom::new(42);
        let trimmed = PerlinNoise::new(&mut rng_a, -7, &[1.0, 2.0, 1.0]);
        let untrimmed = PerlinNoise::new(&mut rng_b, -7, &[1.0, 2.0, 1.0, 0.0, 0.0, 0.0]);

        let a = trimmed.get_value(128.0, 64.0, -256.0);
        let b = untrimmed.get_value(128.0, 64.0, -256.0);
        let expected_ratio = (32.0 / 63.0) / (4.0 / 7.0);
        assert!(((b / a) - expected_ratio).abs() < 1e-12);
    }

    #[test]
    fn perlin_wrap_wraps_at_two_to_the_twenty_fifth() {
        assert_eq!(PerlinNoise::wrap(0.0), 0.0);
        assert_eq!(PerlinNoise::wrap(16_777_215.5), 16_777_215.5);
        assert_eq!(PerlinNoise::wrap(16_777_216.0), -16_777_216.0);
        assert_eq!(PerlinNoise::wrap(33_554_432.25), 0.25);
        assert_eq!(PerlinNoise::wrap(-33_554_431.75), 0.25);
    }

    #[test]
    fn normal_noise_and_blended_noise_produce_deterministic_samples_and_bounds() {
        let mut rng = XoroshiroRandom::new(-123_456_789);
        let params = NoiseParameters::new(-9, vec![1.0, 0.0, 2.0, 1.0, 0.0]);
        let normal = NormalNoise::from_parameters(&mut rng, &params);
        let v = normal.get_value(100.0, 64.0, -200.0);
        assert!(v.abs() <= normal.max_value());

        let mut legacy = LegacyRandom::new(2026);
        let blended = BlendedNoise::new(&mut legacy, 0.25, 0.125, 80.0, 160.0, 8.0);
        // `684.412 * 0.125 + 2.0 = 87.5515`, and `edge_value(x)` for `[1.0; 16]` equals `x`.
        assert!((blended.max_value() - 87.5515).abs() < 1e-12);
        let sample = blended.sample(32.0, 48.0, -16.0);
        assert!(sample.is_finite());

        let mut legacy_again = LegacyRandom::new(2026);
        let reseeded = blended.with_new_random(&mut legacy_again);
        assert_eq!(
            sample.to_bits(),
            reseeded.sample(32.0, 48.0, -16.0).to_bits()
        );
    }

    #[test]
    fn simplex_and_perlin_simplex_noise_cover_2d_3d_and_positive_negative_octaves() {
        let mut rng = LegacyRandom::new(2345);
        let simplex = SimplexNoise::new(&mut rng);
        assert_eq!(simplex.get_value_2d(0.0, 0.0), 0.0);
        assert_eq!(simplex.get_value_3d(0.0, 0.0, 0.0), 0.0);

        for i in -20..=20 {
            let x = i as f64 * 0.41;
            let y = i as f64 * -0.27;
            let z = i as f64 * 0.19;
            let v2 = simplex.get_value_2d(x, y);
            let v3 = simplex.get_value_3d(x, y, z);
            assert!((-1.05..=1.05).contains(&v2), "2D simplex sample {v2}");
            assert!((-1.05..=1.05).contains(&v3), "3D simplex sample {v3}");
        }

        let mut rng_multi = XoroshiroRandom::new(777);
        let multi = PerlinSimplexNoise::new(&mut rng_multi, &[-3, -1, 0, 2]);
        let with_offsets = multi.get_value(16.5, -42.25, true);
        let without_offsets = multi.get_value(16.5, -42.25, false);
        assert_ne!(with_offsets.to_bits(), without_offsets.to_bits());
    }
}
