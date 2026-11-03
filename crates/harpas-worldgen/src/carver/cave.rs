//! Starting positions and child tunnels for configured cave carvers.

use harpas_core::random::RandomSource;

use super::{CarverConfig, CarverShape, CarvingEllipsoid, CaveTunnel};

/// A cave room and its normalized floor cutoff.
#[derive(Debug, Clone, Copy)]
pub struct CaveRoom {
    /// Room center in world coordinates.
    pub center: [f64; 3],
    /// Radius parameter before adding the fixed room margin.
    pub width: f32,
    /// Vertical radius multiplier.
    pub y_scale: f64,
    /// Normalized floor cutoff shared with this cave's tunnels.
    pub floor_level: f32,
}

impl CaveRoom {
    /// The ellipsoid occupied by this room before the floor cutoff is applied.
    pub fn ellipsoid(self) -> CarvingEllipsoid {
        let radius = 1.5 + f64::from(self.width);
        CarvingEllipsoid {
            center: [self.center[0] + 1.0, self.center[1], self.center[2]],
            horizontal_radius: radius,
            vertical_radius: radius * self.y_scale,
        }
    }
}

/// One independently shaped part of a cave system.
#[derive(Debug, Clone, Copy)]
pub enum CavePart {
    /// A room at the source position.
    Room(CaveRoom),
    /// A tunnel, including its floor cutoff.
    Tunnel {
        /// The tunnel's path and radius parameters.
        path: CaveTunnel,
        /// Normalized floor cutoff shared with the originating room.
        floor_level: f32,
    },
}

/// Samples an Overworld cave carver after its source-chunk start decision succeeds.
///
/// The callback receives parts in generation order. Nether caves use different constants and are rejected until
/// their placement has been verified independently.
pub fn plan_cave_parts(
    config: &CarverConfig,
    random: &mut impl RandomSource,
    source_x: i32,
    source_z: i32,
    min_y: i32,
    height: i32,
    mut visit: impl FnMut(CavePart),
) -> Result<(), &'static str> {
    let CarverShape::Cave {
        nether: false,
        horizontal_radius,
        vertical_radius,
        floor_level,
    } = config.shape
    else {
        return Err("only Overworld cave placement is supported");
    };
    let first = random.next_int_bounded(15) + 1;
    let second = random.next_int_bounded(first) + 1;
    let caves = random.next_int_bounded(second);
    for _ in 0..caves {
        let x = source_x
            .wrapping_mul(16)
            .wrapping_add(random.next_int_bounded(16));
        let y = config.y.sample(random, min_y, height);
        let z = source_z
            .wrapping_mul(16)
            .wrapping_add(random.next_int_bounded(16));
        let center = [f64::from(x), f64::from(y), f64::from(z)];
        let horizontal = horizontal_radius.sample(random);
        let vertical = vertical_radius.sample(random);
        let floor = floor_level.sample(random);
        let mut tunnels = 1;
        if random.next_int_bounded(4) == 0 {
            let y_scale = f64::from(config.y_scale.sample(random));
            let width = 1.0 + random.next_float() * 6.0;
            visit(CavePart::Room(CaveRoom {
                center,
                width,
                y_scale,
                floor_level: floor,
            }));
            tunnels += random.next_int_bounded(4);
        }
        for _ in 0..tunnels {
            let yaw = random.next_float() * std::f32::consts::TAU;
            let pitch = (random.next_float() - 0.5) / 4.0;
            let mut thickness = random.next_float() * 2.0 + random.next_float();
            if random.next_int_bounded(10) == 0 {
                thickness *= random.next_float() * random.next_float() * 3.0 + 1.0;
            }
            let end_step = 112 - random.next_int_bounded(28);
            let path = CaveTunnel {
                seed: random.next_long(),
                center,
                radius_scale: [f64::from(horizontal), f64::from(vertical)],
                thickness,
                yaw,
                pitch,
                first_step: 0,
                end_step,
                y_scale: 1.0,
            };
            visit(CavePart::Tunnel {
                path,
                floor_level: floor,
            });
        }
    }
    Ok(())
}
