//! Vanilla-exact 32-bit float `CubicSpline` (`minecraft:spline` density function)
//!
//! Vanilla evaluates `CubicSpline` entirely in `f32` (`float`) before widening the result back to `f64`
//! (`double`), including the `as f32` cast of each coordinate density function (`docs/world.md`).

use std::hash::{Hash, Hasher};

use crate::density::{DensityFunctionDef, NodeId};

/// Uncompiled JSON definition of a single knot in a multipoint [`CubicSplineDef`].
#[derive(Debug, Clone, PartialEq)]
pub struct SplinePointDef {
    /// Knot input coordinate (`f32`).
    pub location: f32,
    /// Value at this knot (either a constant float or a nested multipoint spline).
    pub value: CubicSplineDef,
    /// Slope (`dy/dx`) at this knot (`f32`).
    pub derivative: f32,
}

/// Uncompiled JSON definition of a cubic Hermite spline (`CubicSpline`).
#[derive(Debug, Clone, PartialEq)]
pub enum CubicSplineDef {
    /// Constant `f32` value.
    Constant(f32),
    /// Piecewise cubic Hermite spline parameterized by `coordinate`.
    Multipoint {
        /// Coordinate density function evaluated and cast to `f32`.
        coordinate: Box<DensityFunctionDef>,
        /// Ordered knot points.
        points: Vec<SplinePointDef>,
    },
}

/// Compiled `CubicSpline` operating on deduplicated [`NodeId`] coordinates in `f32` precision.
#[derive(Debug, Clone)]
pub enum CubicSpline {
    /// Constant `f32` leaf spline.
    Constant(f32),
    /// Multipoint cubic Hermite spline with precomputed `[min_value, max_value]` bounds.
    Multipoint {
        /// Coordinate density function node in the compiled DAG.
        coordinate: NodeId,
        /// Strictly non-decreasing knot locations.
        locations: Box<[f32]>,
        /// Nested spline at each knot.
        values: Box<[CubicSpline]>,
        /// Derivative at each knot.
        derivatives: Box<[f32]>,
        /// Precomputed lower bound of the spline in `f32`.
        min_value: f32,
        /// Precomputed upper bound of the spline in `f32`.
        max_value: f32,
    },
}

impl PartialEq for CubicSpline {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Constant(a), Self::Constant(b)) => a.to_bits() == b.to_bits(),
            (
                Self::Multipoint {
                    coordinate: c1,
                    locations: l1,
                    values: v1,
                    derivatives: d1,
                    min_value: min1,
                    max_value: max1,
                },
                Self::Multipoint {
                    coordinate: c2,
                    locations: l2,
                    values: v2,
                    derivatives: d2,
                    min_value: min2,
                    max_value: max2,
                },
            ) => {
                c1 == c2
                    && min1.to_bits() == min2.to_bits()
                    && max1.to_bits() == max2.to_bits()
                    && l1.len() == l2.len()
                    && l1
                        .iter()
                        .zip(l2.iter())
                        .all(|(a, b)| a.to_bits() == b.to_bits())
                    && d1.len() == d2.len()
                    && d1
                        .iter()
                        .zip(d2.iter())
                        .all(|(a, b)| a.to_bits() == b.to_bits())
                    && v1 == v2
            }
            _ => false,
        }
    }
}

impl Eq for CubicSpline {}

impl Hash for CubicSpline {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Constant(val) => {
                0u8.hash(state);
                val.to_bits().hash(state);
            }
            Self::Multipoint {
                coordinate,
                locations,
                values,
                derivatives,
                min_value,
                max_value,
            } => {
                1u8.hash(state);
                coordinate.hash(state);
                for loc in locations {
                    loc.to_bits().hash(state);
                }
                values.hash(state);
                for der in derivatives {
                    der.to_bits().hash(state);
                }
                min_value.to_bits().hash(state);
                max_value.to_bits().hash(state);
            }
        }
    }
}

impl CubicSpline {
    /// Creates a constant `f32` spline.
    pub const fn constant(value: f32) -> Self {
        Self::Constant(value)
    }

    /// Creates a multipoint spline and computes its exact `[min_value, max_value]` bounds as vanilla's
    /// `CubicSpline.Multipoint.create` does.
    ///
    /// `coord_min` and `coord_max` are the `min_value()` and `max_value()` of `coordinate` cast to `f32`.
    pub fn multipoint(
        coordinate: NodeId,
        coord_min: f32,
        coord_max: f32,
        locations: Vec<f32>,
        values: Vec<CubicSpline>,
        derivatives: Vec<f32>,
    ) -> Self {
        assert_eq!(locations.len(), values.len());
        assert_eq!(locations.len(), derivatives.len());
        assert!(
            !locations.is_empty(),
            "Multipoint spline must have at least 1 knot"
        );

        let (min_value, max_value) =
            calculate_min_max(coord_min, coord_max, &locations, &values, &derivatives);
        Self::Multipoint {
            coordinate,
            locations: locations.into_boxed_slice(),
            values: values.into_boxed_slice(),
            derivatives: derivatives.into_boxed_slice(),
            min_value,
            max_value,
        }
    }

    /// Minimum value (`f32`) of this spline.
    pub const fn min_value(&self) -> f32 {
        match self {
            Self::Constant(v) => *v,
            Self::Multipoint { min_value, .. } => *min_value,
        }
    }

    /// Maximum value (`f32`) of this spline.
    pub const fn max_value(&self) -> f32 {
        match self {
            Self::Constant(v) => *v,
            Self::Multipoint { max_value, .. } => *max_value,
        }
    }

    /// Evaluates this spline in `f32` using `eval_coord` to compute any referenced coordinate [`NodeId`].
    pub fn apply_with<F>(&self, eval_coord: &mut F) -> f32
    where
        F: FnMut(NodeId) -> f64,
    {
        match self {
            Self::Constant(val) => *val,
            Self::Multipoint {
                coordinate,
                locations,
                values,
                derivatives,
                ..
            } => {
                let x = eval_coord(*coordinate) as f32;
                let interval = find_interval_start(locations, x);
                if interval < 0 {
                    let v0 = values[0].apply_with(eval_coord);
                    linear_extend(x, locations, v0, derivatives, 0)
                } else {
                    let idx = interval as usize;
                    let last = locations.len() - 1;
                    if idx == last {
                        let v_last = values[last].apply_with(eval_coord);
                        linear_extend(x, locations, v_last, derivatives, last)
                    } else {
                        let l0 = locations[idx];
                        let l1 = locations[idx + 1];
                        let t = (x - l0) / (l1 - l0);
                        let v0 = values[idx].apply_with(eval_coord);
                        let v1 = values[idx + 1].apply_with(eval_coord);
                        let d0 = derivatives[idx];
                        let d1 = derivatives[idx + 1];
                        let a = d0 * (l1 - l0) - (v1 - v0);
                        let b = -d1 * (l1 - l0) + (v1 - v0);
                        lerp_f32(t, v0, v1) + t * (1.0 - t) * lerp_f32(t, a, b)
                    }
                }
            }
        }
    }
}

/// Vanilla's `Mth.lerp(float delta, float start, float end)`.
#[inline]
fn lerp_f32(delta: f32, start: f32, end: f32) -> f32 {
    start + delta * (end - start)
}

/// Vanilla's `CubicSpline.Multipoint.findIntervalStart`:
/// `Mth.binarySearch(0, locations.length, i -> x < locations[i]) - 1`.
#[inline]
fn find_interval_start(locations: &[f32], x: f32) -> isize {
    let mut left = 0usize;
    let mut len = locations.len();
    while len > 0 {
        let half = len / 2;
        let mid = left + half;
        if x < locations[mid] {
            len = half;
        } else {
            left = mid + 1;
            len -= half + 1;
        }
    }
    (left as isize) - 1
}

/// Vanilla's `CubicSpline.Multipoint.linearExtend`.
#[inline]
fn linear_extend(x: f32, locations: &[f32], value: f32, derivatives: &[f32], index: usize) -> f32 {
    let d = derivatives[index];
    if d == 0.0 {
        value
    } else {
        value + d * (x - locations[index])
    }
}

/// Vanilla's `CubicSpline.Multipoint.calculateMinMax`.
fn calculate_min_max(
    coord_min: f32,
    coord_max: f32,
    locations: &[f32],
    values: &[CubicSpline],
    derivatives: &[f32],
) -> (f32, f32) {
    let mut min_val = f32::INFINITY;
    let mut max_val = f32::NEG_INFINITY;
    let last = locations.len() - 1;

    if coord_min < locations[0] {
        let e1 = linear_extend(coord_min, locations, values[0].min_value(), derivatives, 0);
        let e2 = linear_extend(coord_min, locations, values[0].max_value(), derivatives, 0);
        min_val = min_val.min(e1.min(e2));
        max_val = max_val.max(e1.max(e2));
    }

    if coord_max > locations[last] {
        let e1 = linear_extend(
            coord_max,
            locations,
            values[last].min_value(),
            derivatives,
            last,
        );
        let e2 = linear_extend(
            coord_max,
            locations,
            values[last].max_value(),
            derivatives,
            last,
        );
        min_val = min_val.min(e1.min(e2));
        max_val = max_val.max(e1.max(e2));
    }

    for v in values {
        min_val = min_val.min(v.min_value());
        max_val = max_val.max(v.max_value());
    }

    for i in 0..last {
        let l0 = locations[i];
        let l1 = locations[i + 1];
        let span = l1 - l0;
        let v0 = &values[i];
        let v1 = &values[i + 1];
        let min_v0 = v0.min_value();
        let max_v0 = v0.max_value();
        let min_v1 = v1.min_value();
        let max_v1 = v1.max_value();
        let d0 = derivatives[i];
        let d1 = derivatives[i + 1];
        if d0 != 0.0 || d1 != 0.0 {
            let p0 = d0 * span;
            let p1 = d1 * span;
            let min_a = p0 - max_v1 + min_v0;
            let max_a = p0 - min_v1 + max_v0;
            let min_b = -p1 + min_v1 - max_v0;
            let max_b = -p1 + max_v1 - min_v0;
            let min_ext = min_a.min(min_b);
            let max_ext = max_a.max(max_b);
            min_val = min_val.min(min_v0.min(min_v1) + 0.25 * min_ext);
            max_val = max_val.max(max_v0.max(max_v1) + 0.25 * max_ext);
        }
    }

    (min_val, max_val)
}
