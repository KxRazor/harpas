//! Deterministic paths for cave-carver tunnels.

use harpas_core::math::{cos_f32, sin_f32};
use harpas_core::random::{LegacyRandom, RandomSource};

use super::CarvingEllipsoid;

/// A cave tunnel and its independently seeded path.
#[derive(Debug, Clone, Copy)]
pub struct CaveTunnel {
    /// Seed for this tunnel's path, separate from the source chunk's random stream.
    pub seed: i64,
    /// Starting position in absolute block coordinates.
    pub center: [f64; 3],
    /// Horizontal and vertical radius multipliers.
    pub radius_scale: [f64; 2],
    /// Base tunnel thickness.
    pub thickness: f32,
    /// Starting horizontal angle in radians.
    pub yaw: f32,
    /// Starting vertical angle in radians.
    pub pitch: f32,
    /// First path step to evaluate.
    pub first_step: i32,
    /// Exclusive final path step.
    pub end_step: i32,
    /// Vertical scale applied to the path's ellipsoids.
    pub y_scale: f64,
}

impl CaveTunnel {
    /// Visits ellipsoids that may intersect `chunk_x, chunk_z`, including any branch paths.
    pub fn for_each_ellipsoid(
        self,
        chunk_x: i32,
        chunk_z: i32,
        mut visit: impl FnMut(CarvingEllipsoid),
    ) {
        self.walk(chunk_x, chunk_z, &mut visit);
    }

    fn walk(self, chunk_x: i32, chunk_z: i32, visit: &mut impl FnMut(CarvingEllipsoid)) {
        if self.end_step < 2 || self.first_step < 0 || self.first_step >= self.end_step {
            return;
        }
        let mut random = LegacyRandom::new(self.seed);
        let branch = random.next_int_bounded(self.end_step / 2) + self.end_step / 4;
        let gentle_pitch = random.next_int_bounded(6) == 0;
        let [mut x, mut y, mut z] = self.center;
        let (mut yaw, mut pitch) = (self.yaw, self.pitch);
        let (mut pitch_change, mut yaw_change) = (0.0_f32, 0.0_f32);
        for step in self.first_step..self.end_step {
            let radius = 1.5
                + f64::from(
                    sin_f32((step as f32) * std::f32::consts::PI / self.end_step as f32)
                        * self.thickness,
                );
            let vertical_radius = radius * self.y_scale;
            let cos_pitch = cos_f32(pitch);
            x += f64::from(cos_f32(yaw) * cos_pitch);
            y += f64::from(sin_f32(pitch));
            z += f64::from(sin_f32(yaw) * cos_pitch);
            pitch *= if gentle_pitch { 0.92 } else { 0.7 };
            pitch += pitch_change * 0.1;
            yaw += yaw_change * 0.1;
            pitch_change *= 0.9;
            yaw_change *= 0.75;
            pitch_change += (random.next_float() - random.next_float()) * random.next_float() * 2.0;
            yaw_change += (random.next_float() - random.next_float()) * random.next_float() * 4.0;
            if step == branch && self.thickness > 1.0 {
                for direction in [-1.0_f32, 1.0] {
                    let child = Self {
                        seed: random.next_long(),
                        center: [x, y, z],
                        radius_scale: self.radius_scale,
                        thickness: random.next_float() * 0.5 + 0.5,
                        yaw: yaw + direction * std::f32::consts::FRAC_PI_2,
                        pitch: pitch / 3.0,
                        first_step: step,
                        end_step: self.end_step,
                        y_scale: 1.0,
                    };
                    child.walk(chunk_x, chunk_z, visit);
                }
                return;
            }
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
            visit(CarvingEllipsoid {
                center: [x, y, z],
                horizontal_radius: radius * self.radius_scale[0],
                vertical_radius: vertical_radius * self.radius_scale[1],
            });
        }
    }
}
