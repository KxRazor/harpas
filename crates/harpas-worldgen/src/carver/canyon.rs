//! Deterministic paths for canyon carvers.

use harpas_core::math::{cos_f32, sin_f32};
use harpas_core::random::{LegacyRandom, RandomSource};

use super::{CanyonShape, CarverConfig, CarverShape, CarvingEllipsoid};

/// A canyon path and its independently seeded width and direction changes.
#[derive(Debug, Clone, Copy)]
pub struct CanyonPath {
    /// Seed for the canyon path.
    pub seed: i64,
    /// Starting position in absolute block coordinates.
    pub center: [f64; 3],
    /// Base canyon thickness.
    pub thickness: f32,
    /// Starting horizontal angle in radians.
    pub yaw: f32,
    /// Starting vertical angle in radians.
    pub pitch: f32,
    /// First path step to evaluate.
    pub first_step: i32,
    /// Exclusive final path step.
    pub end_step: i32,
    /// Vertical scale of the canyon.
    pub y_scale: f64,
}

/// Samples the path of a configured canyon after its source-chunk start decision succeeds.
pub fn plan_canyon_path(
    config: &CarverConfig,
    random: &mut impl RandomSource,
    source_x: i32,
    source_z: i32,
    min_y: i32,
    height: i32,
) -> Result<CanyonPath, &'static str> {
    let CarverShape::Canyon(shape) = config.shape else {
        return Err("a canyon configuration is required");
    };
    let x = source_x
        .wrapping_mul(16)
        .wrapping_add(random.next_int_bounded(16));
    let y = config.y.sample(random, min_y, height);
    let z = source_z
        .wrapping_mul(16)
        .wrapping_add(random.next_int_bounded(16));
    let yaw = random.next_float() * std::f32::consts::TAU;
    let pitch = shape.vertical_rotation.sample(random);
    let y_scale = f64::from(config.y_scale.sample(random));
    let thickness = shape.thickness.sample(random);
    let end_step = (112.0 * shape.distance_factor.sample(random)) as i32;
    Ok(CanyonPath {
        seed: random.next_long(),
        center: [f64::from(x), f64::from(y), f64::from(z)],
        thickness,
        yaw,
        pitch,
        first_step: 0,
        end_step,
        y_scale,
    })
}

impl CanyonPath {
    /// Visits possible ellipsoids in a target chunk and supplies the width factor for each vertical layer.
    pub fn for_each_ellipsoid(
        self,
        shape: &CanyonShape,
        chunk_x: i32,
        chunk_z: i32,
        height: usize,
        mut visit: impl FnMut(CarvingEllipsoid, &[f32]),
    ) {
        if self.end_step < 1 || self.first_step < 0 || self.first_step >= self.end_step {
            return;
        }
        let mut random = LegacyRandom::new(self.seed);
        let widths = shape.width_factors(&mut random, height);
        let [mut x, mut y, mut z] = self.center;
        let (mut yaw, mut pitch) = (self.yaw, self.pitch);
        let (mut pitch_change, mut yaw_change) = (0.0_f32, 0.0_f32);
        for step in self.first_step..self.end_step {
            let radius = 1.5
                + f64::from(
                    sin_f32((step as f32) * std::f32::consts::PI / self.end_step as f32)
                        * self.thickness,
                );
            let horizontal_radius =
                radius * f64::from(shape.horizontal_radius_factor.sample(&mut random));
            let progress = step as f32 / self.end_step as f32;
            let center = 1.0 - (0.5 - progress).abs() * 2.0;
            let vertical_factor =
                shape.vertical_radius_default_factor + shape.vertical_radius_center_factor * center;
            let vertical_radius = radius
                * self.y_scale
                * f64::from(vertical_factor)
                * f64::from(0.75 + random.next_float() * 0.25);
            let cos_pitch = cos_f32(pitch);
            x += f64::from(cos_f32(yaw) * cos_pitch);
            y += f64::from(sin_f32(pitch));
            z += f64::from(sin_f32(yaw) * cos_pitch);
            pitch *= 0.7;
            pitch += pitch_change * 0.05;
            yaw += yaw_change * 0.05;
            pitch_change *= 0.8;
            yaw_change *= 0.5;
            pitch_change += (random.next_float() - random.next_float()) * random.next_float() * 2.0;
            yaw_change += (random.next_float() - random.next_float()) * random.next_float() * 4.0;
            if random.next_int_bounded(4) == 0 {
                continue;
            }
            let dx = x - (f64::from(chunk_x) * 16.0 + 8.0);
            let dz = z - (f64::from(chunk_z) * 16.0 + 8.0);
            let remaining = f64::from(self.end_step - step);
            let reach = f64::from(self.thickness + 2.0 + 16.0);
            if dx * dx + dz * dz - remaining * remaining > reach * reach {
                return;
            }
            visit(
                CarvingEllipsoid {
                    center: [x, y, z],
                    horizontal_radius,
                    vertical_radius,
                },
                &widths,
            );
        }
    }
}
