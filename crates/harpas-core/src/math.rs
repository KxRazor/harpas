//! Java math semantics and vanilla math helpers
//!
//! Rust and Java share IEEE 754 `f32`/`f64` arithmetic and JLS §5.1.3 saturating float-to-integer casts (`as i32`,
//! `as i64`), but differ on integer overflow (`MIN / -1`), floored division for negative divisors, shift-distance
//! masking, half-way rounding (`Math.round` ties toward `+∞`), and platform transcendental functions (`libm`).
//! World generation and game logic call these helpers wherever Java's exact result matters
//! (`docs/engine-core.md#7-determinism`).

use std::sync::OnceLock;

/// Scale factor converting radians to indices in the 65,536-entry sine table (`65536 / 2π` as `f32`).
const SIN_SCALE: f32 = 10_430.378_f32;

/// Java's truncating 32-bit signed division (`x / y`), where `i32::MIN / -1 == i32::MIN`.
///
/// # Panics
///
/// Panics if `y == 0`.
pub const fn div_i32(x: i32, y: i32) -> i32 {
    x.wrapping_div(y)
}

/// Java's truncating 32-bit signed remainder (`x % y`), where `i32::MIN % -1 == 0`.
///
/// # Panics
///
/// Panics if `y == 0`.
pub const fn rem_i32(x: i32, y: i32) -> i32 {
    x.wrapping_rem(y)
}

/// Java's truncating 64-bit signed division (`x / y`), where `i64::MIN / -1 == i64::MIN`.
///
/// # Panics
///
/// Panics if `y == 0`.
pub const fn div_i64(x: i64, y: i64) -> i64 {
    x.wrapping_div(y)
}

/// Java's truncating 64-bit signed remainder (`x % y`), where `i64::MIN % -1 == 0`.
///
/// # Panics
///
/// Panics if `y == 0`.
pub const fn rem_i64(x: i64, y: i64) -> i64 {
    x.wrapping_rem(y)
}

/// Java's `Math.floorDiv(int, int)`: division rounded toward `-∞`.
///
/// Unlike [`i32::div_euclid`], a negative divisor rounds toward `-∞` rather than producing a non-negative
/// remainder, and `(i32::MIN, -1)` returns `i32::MIN` instead of panicking.
///
/// # Panics
///
/// Panics if `y == 0`.
pub const fn floor_div_i32(x: i32, y: i32) -> i32 {
    let q = x.wrapping_div(y);
    let r = x.wrapping_rem(y);
    if (x ^ y) < 0 && r != 0 {
        q.wrapping_sub(1)
    } else {
        q
    }
}

/// Java's `Math.floorMod(int, int)`: remainder of [`floor_div_i32`], carrying the sign of `y`.
///
/// # Panics
///
/// Panics if `y == 0`.
pub const fn floor_mod_i32(x: i32, y: i32) -> i32 {
    let r = x.wrapping_rem(y);
    if (x ^ y) < 0 && r != 0 {
        r.wrapping_add(y)
    } else {
        r
    }
}

/// Java's `Math.floorDiv(long, long)`: division rounded toward `-∞`.
///
/// # Panics
///
/// Panics if `y == 0`.
pub const fn floor_div_i64(x: i64, y: i64) -> i64 {
    let q = x.wrapping_div(y);
    let r = x.wrapping_rem(y);
    if (x ^ y) < 0 && r != 0 {
        q.wrapping_sub(1)
    } else {
        q
    }
}

/// Java's `Math.floorMod(long, long)`: remainder of [`floor_div_i64`], carrying the sign of `y`.
///
/// # Panics
///
/// Panics if `y == 0`.
pub const fn floor_mod_i64(x: i64, y: i64) -> i64 {
    let r = x.wrapping_rem(y);
    if (x ^ y) < 0 && r != 0 {
        r.wrapping_add(y)
    } else {
        r
    }
}

/// Java's `Math.floorMod(long, int)`: 64-bit dividend reduced modulo a 32-bit divisor.
///
/// # Panics
///
/// Panics if `y == 0`.
pub const fn floor_mod_i64_i32(x: i64, y: i32) -> i32 {
    floor_mod_i64(x, y as i64) as i32
}

/// Java's `x << bits` on `int`, masking `bits` to its low 5 bits.
pub const fn shl_i32(x: i32, bits: i32) -> i32 {
    x.wrapping_shl(bits as u32)
}

/// Java's arithmetic `x >> bits` on `int`, masking `bits` to its low 5 bits.
pub const fn shr_i32(x: i32, bits: i32) -> i32 {
    x.wrapping_shr(bits as u32)
}

/// Java's logical `x >>> bits` on `int`, zero-extending and masking `bits` to its low 5 bits.
pub const fn ushr_i32(x: i32, bits: i32) -> i32 {
    (x as u32).wrapping_shr(bits as u32) as i32
}

/// Java's `x << bits` on `long`, masking `bits` to its low 6 bits.
pub const fn shl_i64(x: i64, bits: i32) -> i64 {
    x.wrapping_shl(bits as u32)
}

/// Java's arithmetic `x >> bits` on `long`, masking `bits` to its low 6 bits.
pub const fn shr_i64(x: i64, bits: i32) -> i64 {
    x.wrapping_shr(bits as u32)
}

/// Java's logical `x >>> bits` on `long`, zero-extending and masking `bits` to its low 6 bits.
pub const fn ushr_i64(x: i64, bits: i32) -> i64 {
    (x as u64).wrapping_shr(bits as u32) as i64
}

/// Java's `Math.round(float)`: nearest `i32` with ties rounded toward `+∞`, `NaN` mapped to `0`, and out-of-range
/// values saturated to `[i32::MIN, i32::MAX]`.
pub const fn round_f32(x: f32) -> i32 {
    if x.is_nan() {
        return 0;
    }
    let bits = x.to_bits();
    let negative = (bits >> 31) != 0;
    let exp = ((bits >> 23) & 0xff) as i32 - 127;
    if exp < -1 {
        return 0;
    }
    if exp == -1 {
        return if negative {
            if (bits & 0x007f_ffff) == 0 { 0 } else { -1 }
        } else {
            1
        };
    }
    if exp >= 31 {
        return if negative { i32::MIN } else { i32::MAX };
    }
    if exp >= 23 {
        return x as i32;
    }
    let mantissa = (bits & 0x007f_ffff) | 0x0080_0000;
    let frac_bits = (23 - exp) as u32;
    let int_part = (mantissa >> frac_bits) as i32;
    let half = 1_u32 << (frac_bits - 1);
    let frac = mantissa & ((1_u32 << frac_bits) - 1);
    if negative {
        -(int_part + (frac > half) as i32)
    } else {
        int_part + (frac >= half) as i32
    }
}

/// Java's `Math.round(double)`: nearest `i64` with ties rounded toward `+∞`, `NaN` mapped to `0`, and
/// out-of-range values saturated to `[i64::MIN, i64::MAX]`.
pub const fn round_f64(x: f64) -> i64 {
    if x.is_nan() {
        return 0;
    }
    let bits = x.to_bits();
    let negative = (bits >> 63) != 0;
    let exp = ((bits >> 52) & 0x7ff) as i32 - 1023;
    if exp < -1 {
        return 0;
    }
    if exp == -1 {
        return if negative {
            if (bits & 0x000f_ffff_ffff_ffff) == 0 {
                0
            } else {
                -1
            }
        } else {
            1
        };
    }
    if exp >= 63 {
        return if negative { i64::MIN } else { i64::MAX };
    }
    if exp >= 52 {
        return x as i64;
    }
    let mantissa = (bits & 0x000f_ffff_ffff_ffff) | 0x0010_0000_0000_0000;
    let frac_bits = (52 - exp) as u32;
    let int_part = (mantissa >> frac_bits) as i64;
    let half = 1_u64 << (frac_bits - 1);
    let frac = mantissa & ((1_u64 << frac_bits) - 1);
    if negative {
        -(int_part + (frac > half) as i64)
    } else {
        int_part + (frac >= half) as i64
    }
}

/// Deterministic natural logarithm across platforms, accurate within 1 ULP as required by Java's `Math.log`.
pub fn ln(x: f64) -> f64 {
    if x.is_nan() || x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }
    if x == f64::INFINITY {
        return f64::INFINITY;
    }
    let mut bits = x.to_bits();
    let mut exp = ((bits >> 52) & 0x7ff) as i32;
    if exp == 0 {
        bits = (x * ((1_u64 << 54) as f64)).to_bits();
        exp = ((bits >> 52) & 0x7ff) as i32 - 54;
    }
    let mut k = exp - 1023;
    let mut m = f64::from_bits((bits & 0x000f_ffff_ffff_ffff) | 0x3ff0_0000_0000_0000);
    if m > std::f64::consts::SQRT_2 {
        m *= 0.5;
        k += 1;
    }

    // Range-reduced m lies in [sqrt(2)/2, sqrt(2)], so |u| = |(m - 1)/(m + 1)| < 0.1716.
    let num = m - 1.0;
    let s = m + 1.0;
    let v = s - m;
    let den_lo = (m - (s - v)) + (1.0 - v);
    let u_hi = num / s;

    let p_hi = u_hi * s;
    let split = 134_217_729.0_f64;
    let ua = u_hi * split;
    let u1 = ua - (ua - u_hi);
    let u2 = u_hi - u1;
    let sa = s * split;
    let s1 = sa - (sa - s);
    let s2 = s - s1;
    let p_lo = ((u1 * s1 - p_hi) + u1 * s2 + u2 * s1) + u2 * s2;
    let u_lo = (num - p_hi - p_lo - u_hi * den_lo) / s;

    let u2_sq = u_hi * u_hi;
    let mut poly = 2.0 / 23.0;
    let mut denom = 21;
    while denom >= 3 {
        poly = poly * u2_sq + 2.0 / (denom as f64);
        denom -= 2;
    }
    let tail = (u_hi * u2_sq) * poly + 2.0 * u2_sq * u_lo;

    let ln2_hi = f64::from_bits(0x3fe6_2e42_fee0_0000);
    let ln2_lo = 1.9082149292705877e-10_f64;
    let kf = k as f64;
    let a = kf * ln2_hi;
    let b = 2.0 * u_hi;
    let sum_hi = a + b;
    let bv = sum_hi - a;
    let sum_err = (a - (sum_hi - bv)) + (b - bv);
    sum_hi + (sum_err + kf * ln2_lo + 2.0 * u_lo + tail)
}

/// Largest `i32` less than or equal to `x`, matching vanilla's `Mth.floor(float)`.
pub const fn floor_f32(x: f32) -> i32 {
    let i = x as i32;
    if x < i as f32 { i.wrapping_sub(1) } else { i }
}

/// Largest `i32` less than or equal to `x`, matching vanilla's `Mth.floor(double)`.
pub const fn floor_f64(x: f64) -> i32 {
    let i = x as i32;
    if x < i as f64 { i.wrapping_sub(1) } else { i }
}

/// Largest `i64` less than or equal to `x`, matching vanilla's `Mth.lfloor(double)`.
pub const fn lfloor(x: f64) -> i64 {
    let i = x as i64;
    if x < i as f64 { i.wrapping_sub(1) } else { i }
}

/// Smallest `i32` greater than or equal to `x`, matching vanilla's `Mth.ceil(float)`.
pub const fn ceil_f32(x: f32) -> i32 {
    let i = x as i32;
    if x > i as f32 { i.wrapping_add(1) } else { i }
}

/// Smallest `i32` greater than or equal to `x`, matching vanilla's `Mth.ceil(double)`.
pub const fn ceil_f64(x: f64) -> i32 {
    let i = x as i32;
    if x > i as f64 { i.wrapping_add(1) } else { i }
}

/// Fractional part of `x` in `[0.0, 1.0)`, matching vanilla's `Mth.frac(float)`.
pub const fn frac_f32(x: f32) -> f32 {
    x - (floor_f32(x) as f32)
}

/// Fractional part of `x` in `[0.0, 1.0)`, matching vanilla's `Mth.frac(double)`.
pub const fn frac_f64(x: f64) -> f64 {
    x - (lfloor(x) as f64)
}

/// Linear interpolation `start + delta * (end - start)` in `f32`, keeping Java's operation order.
pub const fn lerp_f32(delta: f32, start: f32, end: f32) -> f32 {
    start + delta * (end - start)
}

/// Linear interpolation `start + delta * (end - start)` in `f64`, keeping Java's operation order.
pub const fn lerp_f64(delta: f64, start: f64, end: f64) -> f64 {
    start + delta * (end - start)
}

/// Bilinear interpolation on a unit square, interpolating along `dx` first and then `dy`.
pub const fn lerp2(dx: f64, dy: f64, v00: f64, v10: f64, v01: f64, v11: f64) -> f64 {
    lerp_f64(dy, lerp_f64(dx, v00, v10), lerp_f64(dx, v01, v11))
}

/// Trilinear interpolation on a unit cube, interpolating along `dx`, then `dy`, then `dz`.
#[allow(clippy::too_many_arguments)]
pub const fn lerp3(
    dx: f64,
    dy: f64,
    dz: f64,
    v000: f64,
    v100: f64,
    v010: f64,
    v110: f64,
    v001: f64,
    v101: f64,
    v011: f64,
    v111: f64,
) -> f64 {
    lerp_f64(
        dz,
        lerp2(dx, dy, v000, v100, v010, v110),
        lerp2(dx, dy, v001, v101, v011, v111),
    )
}

/// Linear interpolation clamped to `[start, end]` when `delta` leaves `[0.0, 1.0]`.
pub const fn clamped_lerp(start: f64, end: f64, delta: f64) -> f64 {
    if delta < 0.0 {
        start
    } else if delta > 1.0 {
        end
    } else {
        lerp_f64(delta, start, end)
    }
}

/// Position of `value` relative to `[start, end]`: `(value - start) / (end - start)`.
pub const fn inverse_lerp(value: f64, start: f64, end: f64) -> f64 {
    (value - start) / (end - start)
}

/// Linearly maps `value` from `[from_start, from_end]` into `[to_start, to_end]`.
pub const fn map(value: f64, from_start: f64, from_end: f64, to_start: f64, to_end: f64) -> f64 {
    lerp_f64(inverse_lerp(value, from_start, from_end), to_start, to_end)
}

/// Linearly maps `value` from `[from_start, from_end]` into `[to_start, to_end]`, clamping outside the input range.
pub const fn clamped_map(
    value: f64,
    from_start: f64,
    from_end: f64,
    to_start: f64,
    to_end: f64,
) -> f64 {
    clamped_lerp(to_start, to_end, inverse_lerp(value, from_start, from_end))
}

/// Perlin's quintic fade curve `6x^5 - 15x^4 + 10x^3`, evaluated in Java's left-to-right order.
pub const fn smoothstep(x: f64) -> f64 {
    x * x * x * (x * (x * 6.0 - 15.0) + 10.0)
}

/// Wraps an angle in degrees into `[-180.0, 180.0)` in `f32`, as vanilla's `Mth.wrapDegrees(float)`.
pub const fn wrap_degrees_f32(degrees: f32) -> f32 {
    let mut wrapped = degrees % 360.0_f32;
    if wrapped >= 180.0_f32 {
        wrapped -= 360.0_f32;
    }
    if wrapped < -180.0_f32 {
        wrapped += 360.0_f32;
    }
    wrapped
}

/// Wraps an angle in degrees into `[-180.0, 180.0)` in `f64`, as vanilla's `Mth.wrapDegrees(double)`.
pub const fn wrap_degrees_f64(degrees: f64) -> f64 {
    let mut wrapped = degrees % 360.0_f64;
    if wrapped >= 180.0_f64 {
        wrapped -= 360.0_f64;
    }
    if wrapped < -180.0_f64 {
        wrapped += 360.0_f64;
    }
    wrapped
}

fn sin_table() -> &'static [f32; 65_536] {
    static TABLE: OnceLock<Box<[f32; 65_536]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let values: Vec<f32> = (0..65_536_u32)
            .map(|i| ((i as f64) * std::f64::consts::PI * 2.0 / 65_536.0).sin() as f32)
            .collect();
        values
            .into_boxed_slice()
            .try_into()
            .expect("65,536 entries")
    })
}

/// Sine from vanilla's 65,536-entry `f32` lookup table (`Mth.sin`).
pub fn sin_f32(radians: f32) -> f32 {
    sin_table()[((radians * SIN_SCALE) as i32 & 0xffff) as usize]
}

/// Cosine from vanilla's 65,536-entry `f32` lookup table (`Mth.cos`).
pub fn cos_f32(radians: f32) -> f32 {
    sin_table()[((radians * SIN_SCALE + 16_384.0_f32) as i32 & 0xffff) as usize]
}

/// Vanilla's deterministic block-coordinate hash (`Mth.getSeed`), preserving Java's 32-bit `x * 3129871` overflow.
pub const fn position_seed(x: i32, y: i32, z: i32) -> i64 {
    let l = (x.wrapping_mul(3_129_871) as i64) ^ (z as i64).wrapping_mul(116_129_781) ^ (y as i64);
    let l = l
        .wrapping_mul(l)
        .wrapping_mul(42_317_861)
        .wrapping_add(l.wrapping_mul(11));
    l >> 16
}

/// Java's `String.hashCode()` over UTF-16 code units.
pub fn java_string_hash(s: &str) -> i32 {
    let mut hash = 0_i32;
    for unit in s.encode_utf16() {
        hash = hash.wrapping_mul(31).wrapping_add(i32::from(unit));
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncating_and_floored_division_follow_java_at_extremes_and_negative_divisors() {
        assert_eq!(div_i32(i32::MIN, -1), i32::MIN);
        assert_eq!(rem_i32(i32::MIN, -1), 0);
        assert_eq!(div_i64(i64::MIN, -1), i64::MIN);
        assert_eq!(rem_i64(i64::MIN, -1), 0);

        assert_eq!(floor_div_i32(-7, 4), -2);
        assert_eq!(floor_mod_i32(-7, 4), 1);
        assert_eq!(floor_div_i32(-7, -4), 1);
        assert_eq!(floor_mod_i32(-7, -4), -3);
        assert_eq!(floor_div_i32(7, -4), -2);
        assert_eq!(floor_mod_i32(7, -4), -1);
        assert_eq!(floor_div_i32(i32::MIN, -1), i32::MIN);
        assert_eq!(floor_mod_i32(i32::MIN, -1), 0);

        assert_eq!(floor_div_i64(-7, -4), 1);
        assert_eq!(floor_mod_i64(-7, -4), -3);
        assert_eq!(floor_div_i64(i64::MIN, -1), i64::MIN);
        assert_eq!(floor_mod_i64(i64::MIN, -1), 0);
        assert_eq!(floor_mod_i64_i32(-1, 16), 15);
    }

    #[test]
    fn shifts_mask_counts_and_zero_extend_logical_right_shift() {
        assert_eq!(shl_i32(1, 33), 2);
        assert_eq!(shl_i32(1, -31), 2);
        assert_eq!(shr_i32(-8, 33), -4);
        assert_eq!(ushr_i32(-1, 24), 0xff);
        assert_eq!(ushr_i32(i32::MIN, 33), 0x4000_0000);

        assert_eq!(shl_i64(1, 65), 2);
        assert_eq!(shr_i64(-8, 65), -4);
        assert_eq!(ushr_i64(-1, 56), 0xff);
    }

    #[test]
    fn java_round_breaks_ties_toward_positive_infinity_without_double_rounding() {
        assert_eq!(round_f32(0.5), 1);
        assert_eq!(round_f32(-0.5), 0);
        assert_eq!(round_f32(-1.5), -1);
        assert_eq!(round_f32(f32::from_bits(0x3eff_ffff)), 0); // 0.5 - 2^-25
        assert_eq!(round_f32(f32::NAN), 0);
        assert_eq!(round_f32(f32::INFINITY), i32::MAX);
        assert_eq!(round_f32(f32::NEG_INFINITY), i32::MIN);

        assert_eq!(round_f64(0.5), 1);
        assert_eq!(round_f64(-0.5), 0);
        assert_eq!(round_f64(-1.5), -1);
        assert_eq!(round_f64(f64::from_bits(0x3fdf_ffff_ffff_ffff)), 0); // 0.49999999999999994
        assert_eq!(round_f64(-4_503_599_627_370_495.5), -4_503_599_627_370_495);
        assert_eq!(round_f64(f64::NAN), 0);
        assert_eq!(round_f64(f64::INFINITY), i64::MAX);
        assert_eq!(round_f64(f64::NEG_INFINITY), i64::MIN);
    }

    #[test]
    fn deterministic_ln_handles_special_values_and_stays_within_one_ulp() {
        assert!(ln(f64::NAN).is_nan());
        assert!(ln(-1.0).is_nan());
        assert_eq!(ln(0.0), f64::NEG_INFINITY);
        assert_eq!(ln(-0.0), f64::NEG_INFINITY);
        assert_eq!(ln(1.0), 0.0);
        assert_eq!(ln(f64::INFINITY), f64::INFINITY);

        let mut state = 0x1234_5678_9abc_def0_u64;
        for _ in 0..10_000 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let sample = ((state >> 11) as f64) * f64::from_bits(0x3ca0_0000_0000_0000);
            if sample > 0.0 {
                let got = ln(sample).to_bits();
                let reference = sample.ln().to_bits();
                assert!(got.abs_diff(reference) <= 1, "sample={sample}");
            }
        }
        let subnormal = f64::from_bits(1);
        assert!(ln(subnormal).to_bits().abs_diff(subnormal.ln().to_bits()) <= 1);
    }

    #[test]
    fn floor_ceil_frac_interpolation_and_angles_match_vanilla() {
        assert_eq!(floor_f32(1.9), 1);
        assert_eq!(floor_f32(-1.1), -2);
        assert_eq!(floor_f32(-1.0), -1);
        assert_eq!(floor_f32(f32::NAN), 0);
        assert_eq!(floor_f64(-0.0001), -1);
        assert_eq!(lfloor(-3_000_000_000.25), -3_000_000_001);

        assert_eq!(ceil_f32(1.1), 2);
        assert_eq!(ceil_f32(-1.1), -1);
        assert_eq!(ceil_f64(2.0), 2);
        assert_eq!(frac_f32(-0.25), 0.75);
        assert_eq!(frac_f64(-0.25), 0.75);

        assert_eq!(clamped_lerp(10.0, 20.0, -0.5), 10.0);
        assert_eq!(clamped_lerp(10.0, 20.0, 0.25), 12.5);
        assert_eq!(clamped_lerp(10.0, 20.0, 1.5), 20.0);
        assert_eq!(clamped_map(5.0, 0.0, 10.0, 100.0, 200.0), 150.0);
        assert_eq!(map(15.0, 0.0, 10.0, 100.0, 200.0), 250.0);
        assert_eq!(
            lerp3(0.5, 0.5, 0.5, 0.0, 2.0, 2.0, 4.0, 2.0, 4.0, 4.0, 6.0),
            3.0
        );
        assert_eq!(smoothstep(0.5), 0.5);

        assert_eq!(wrap_degrees_f32(180.0), -180.0);
        assert_eq!(wrap_degrees_f32(-180.0), -180.0);
        assert_eq!(wrap_degrees_f64(540.0), -180.0);
        assert_eq!(wrap_degrees_f64(-181.0), 179.0);
    }

    #[test]
    fn sin_table_position_seed_and_string_hash_match_java_25_vectors() {
        let mut fnv = 0xcbf2_9ce4_8422_2325_u64;
        for entry in sin_table().iter() {
            fnv = (fnv ^ u64::from(entry.to_bits())).wrapping_mul(0x0000_0100_0000_01b3);
        }
        assert_eq!(fnv, 0x21cb_003c_caa3_4307);
        assert_eq!(sin_f32(0.0), 0.0);
        assert_eq!(cos_f32(0.0), 1.0);
        assert_eq!(sin_f32(f32::NAN), 0.0);

        // Large coordinates trigger 32-bit wrapping in x * 3129871 before promotion to i64.
        assert_eq!(position_seed(0, 64, 0), 2_644_866);
        assert_eq!(position_seed(1_000, -32, -2_500), 69_472_454_561_102);

        assert_eq!(java_string_hash(""), 0);
        assert_eq!(java_string_hash("minecraft:overworld"), 1_104_210_353);
        assert_eq!(java_string_hash("😀"), 1_772_899);
    }
}
