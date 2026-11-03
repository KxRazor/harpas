//! Chunk-local primitives for the cave and canyon carving pass.

use harpas_core::random::RandomSource;

use crate::surface::VerticalAnchor;

mod canyon;
mod carve;
mod cave;
mod config;
mod tunnel;
pub use canyon::{CanyonPath, plan_canyon_path};
pub use carve::{CarvingEnvironment, carve_ellipsoid};
pub use cave::{CavePart, CaveRoom, plan_cave_parts};
pub use config::{CanyonShape, CarverConfig, CarverShape};
pub use tunnel::CaveTunnel;

/// Floating-point distributions used in cave and canyon configurations.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CarverFloatProvider {
    /// One fixed value; sampling consumes no random numbers.
    Constant(f32),
    /// A uniform value in `[min, max)`.
    Uniform {
        /// Inclusive minimum.
        min: f32,
        /// Exclusive maximum.
        max: f32,
    },
    /// A trapezoidal distribution from `min` to `max` with a central plateau.
    Trapezoid {
        /// Lower bound.
        min: f32,
        /// Upper bound.
        max: f32,
        /// Width of the central plateau.
        plateau: f32,
    },
}

impl CarverFloatProvider {
    /// Samples the provider using the supplied Java-compatible random source.
    pub fn sample(self, random: &mut impl RandomSource) -> f32 {
        match self {
            Self::Constant(value) => value,
            Self::Uniform { min, max } => uniform(random, min, max),
            Self::Trapezoid { min, max, plateau } => {
                let width = max - min;
                let slope = (width - plateau) / 2.0;
                let remaining = width - slope;
                min + random.next_float() * remaining + random.next_float() * slope
            }
        }
    }
}

fn uniform(random: &mut impl RandomSource, min: f32, max: f32) -> f32 {
    if min >= max {
        min
    } else {
        min + random.next_float() * (max - min)
    }
}

/// Whether the cave shape excludes an ellipsoid candidate, including its flattened floor.
pub fn cave_should_skip([x, y, z]: [f64; 3], floor_level: f64) -> bool {
    y <= floor_level || x * x + y * y + z * z >= 1.0
}

/// Whether the canyon shape excludes an ellipsoid candidate at a layer with `width_factor`.
pub fn canyon_should_skip([x, y, z]: [f64; 3], width_factor: f32) -> bool {
    (x * x + z * z) * f64::from(width_factor) + y * y / 6.0 >= 1.0
}

/// A fixed height or a uniform, inclusive range between generation-height anchors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarverHeightProvider {
    /// A single anchor; sampling consumes no random numbers.
    Constant(VerticalAnchor),
    /// A uniform range including both anchors.
    Uniform {
        /// Inclusive lower anchor.
        min: VerticalAnchor,
        /// Inclusive upper anchor.
        max: VerticalAnchor,
    },
}

impl CarverHeightProvider {
    /// Samples a world Y coordinate for a dimension whose bottom and height are `min_y` and `height`.
    pub fn sample(self, random: &mut impl RandomSource, min_y: i32, height: i32) -> i32 {
        match self {
            Self::Constant(anchor) => anchor.resolve(min_y, height),
            Self::Uniform { min, max } => {
                let min = min.resolve(min_y, height);
                let max = max.resolve(min_y, height);
                if max < min {
                    min
                } else {
                    random.next_int_between_inclusive(min, max)
                }
            }
        }
    }
}

/// The positions already visited by carvers in a chunk.
///
/// The coordinates are local in X and Z and absolute in Y. The packed representation uses the same
/// X-then-Z-then-Y bit order as a Java 26.1 carving mask.
#[derive(Debug, Clone)]
pub struct CarvingMask {
    min_y: i32,
    height: i32,
    words: Vec<u64>,
}

impl CarvingMask {
    /// Creates an empty mask for `height` block layers beginning at `min_y`.
    pub fn new(min_y: i32, height: i32) -> Self {
        assert!(height > 0, "a carving mask needs a positive height");
        Self {
            min_y,
            height,
            words: vec![0; height as usize * 4],
        }
    }

    fn index(&self, x: u8, y: i32, z: u8) -> Option<usize> {
        let relative_y = i64::from(y) - i64::from(self.min_y);
        if x >= 16 || z >= 16 || relative_y < 0 || relative_y >= i64::from(self.height) {
            return None;
        }
        Some((relative_y as usize) * 256 + usize::from(z) * 16 + usize::from(x))
    }

    /// Whether a position has already been marked. Out-of-range positions are not marked.
    pub fn contains(&self, x: u8, y: i32, z: u8) -> bool {
        let Some(index) = self.index(x, y, z) else {
            return false;
        };
        self.words[index / 64] & (1_u64 << (index % 64)) != 0
    }

    /// Marks a position, returning `true` only when the position was in range and newly marked.
    pub fn insert(&mut self, x: u8, y: i32, z: u8) -> bool {
        let Some(index) = self.index(x, y, z) else {
            return false;
        };
        let word = &mut self.words[index / 64];
        let bit = 1_u64 << (index % 64);
        let was_unset = *word & bit == 0;
        *word |= bit;
        was_unset
    }

    /// Returns the packed words, omitting trailing zero words like Java's `BitSet.toLongArray`.
    pub fn to_raw_data(&self) -> Vec<i64> {
        let len = self
            .words
            .iter()
            .rposition(|&word| word != 0)
            .map_or(0, |last| last + 1);
        self.words[..len].iter().map(|&word| word as i64).collect()
    }
}

/// A single ellipsoid tested by a cave or canyon carver.
#[derive(Debug, Clone, Copy)]
pub struct CarvingEllipsoid {
    /// Absolute block-space center, in X, Y, Z order.
    pub center: [f64; 3],
    /// Horizontal X and Z radius.
    pub horizontal_radius: f64,
    /// Vertical radius.
    pub vertical_radius: f64,
}

impl CarvingEllipsoid {
    /// Visits the chunk-local candidate blocks in vanilla order. `normalized` contains the three coordinates
    /// passed to the carver's shape-specific skip check; the check decides whether the candidate is carved.
    pub fn for_each_candidate(
        self,
        chunk_x: i32,
        chunk_z: i32,
        min_y: i32,
        height: i32,
        mut visit: impl FnMut(u8, i32, u8, [f64; 3]),
    ) {
        let [cx, cy, cz] = self.center;
        let horizontal = self.horizontal_radius;
        let vertical = self.vertical_radius;
        if height <= 0
            || !cx.is_finite()
            || !cy.is_finite()
            || !cz.is_finite()
            || !horizontal.is_finite()
            || !vertical.is_finite()
            || horizontal <= 0.0
            || vertical <= 0.0
        {
            return;
        }
        let origin_x = i64::from(chunk_x) * 16;
        let origin_z = i64::from(chunk_z) * 16;
        let first_x = ((cx - horizontal).floor() as i64)
            .saturating_sub(origin_x)
            .saturating_sub(1)
            .clamp(0, 16);
        let end_x = ((cx + horizontal).floor() as i64)
            .saturating_sub(origin_x)
            .saturating_add(1)
            .clamp(0, 16);
        let first_z = ((cz - horizontal).floor() as i64)
            .saturating_sub(origin_z)
            .saturating_sub(1)
            .clamp(0, 16);
        let end_z = ((cz + horizontal).floor() as i64)
            .saturating_sub(origin_z)
            .saturating_add(1)
            .clamp(0, 16);
        let lower = ((cy - vertical).floor() as i64)
            .saturating_sub(1)
            .max(i64::from(min_y) + 1);
        let upper = ((cy + vertical).floor() as i64)
            .saturating_add(1)
            .min(i64::from(min_y) + i64::from(height) - 8)
            .min(i64::from(i32::MAX));
        if lower >= upper {
            return;
        }
        for x in first_x..end_x {
            let dx = ((origin_x + x) as f64 + 0.5 - cx) / horizontal;
            for z in first_z..end_z {
                let dz = ((origin_z + z) as f64 + 0.5 - cz) / horizontal;
                if dx * dx + dz * dz >= 1.0 {
                    continue;
                }
                for y in (lower + 1..=upper).rev() {
                    let dy = (y as f64 - 0.5 - cy) / vertical;
                    visit(x as u8, y as i32, z as u8, [dx, dy, dz]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_packs_local_positions_and_ignores_out_of_range_coordinates() {
        let mut mask = CarvingMask::new(-64, 384);
        assert!(mask.insert(0, -64, 0));
        assert!(mask.insert(15, -64, 0));
        assert!(mask.insert(0, -64, 1));
        assert!(mask.insert(0, -63, 0));
        assert!(!mask.insert(0, -63, 0));
        assert!(!mask.insert(16, -63, 0));
        assert!(!mask.insert(0, -65, 0));
        assert_eq!(mask.to_raw_data(), vec![0x1_8001, 0, 0, 0, 1]);
        assert!(mask.contains(15, -64, 0));
        assert!(!mask.contains(15, -63, 0));
        assert!(!mask.contains(0, 320, 0));
    }
}
