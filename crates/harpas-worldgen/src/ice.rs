//! The features of frozen biomes: `minecraft:spike` (`SpikeFeature`), the ice spikes of ice spike plains;
//! `minecraft:iceberg` (`IcebergFeature`), the packed and blue ice icebergs of frozen oceans; and
//! `minecraft:blue_ice` (`BlueIceFeature`), the blue ice that grows under packed ice in the water.
//!
//! A spike stands on its `can_place_on` block, moved down through air: a cone of four to seven blocks below a
//! taller narrowing column, with ragged edges, and for a wide spike, by a chance of one in sixty, ten to forty blocks
//! higher; under it a column of the block reaches down from the cone to the first block it may not replace, above
//! y 50, in steps. An iceberg is centered at the sea level: an ellipse or a round shape above the water, smoothed of
//! loose blocks, a steeper one below, with snow on its upper part, then often a cut through it of air above the water
//! and water below. Blue ice starts below the sea level in or above water beside packed ice and grows into the air,
//! water and ice around it next to other blue ice.
//!
//! The angles use Rust's `sin` and `cos`, as `FancyTrunkPlacer` does, where vanilla calls `Math.sin` and `Math.cos`.

use std::cmp::Ordering;

use harpas_core::block::block_states;
use harpas_core::math::ceil_f32;
use harpas_core::random::{RandomSource, WorldgenRandom};
use serde_json::Value;

use crate::configured::FeatureCompiler;
use crate::data::{WorldgenDataError, parse_block_state_def};
use crate::feature::FeatureWorld;
use crate::placement::BlockPredicate;
use crate::shape_update::{Direction, same_block};
use crate::surface::resolve_block_state;

/// The blocks the ice features read and place.
#[derive(Debug)]
pub struct IceBlocks {
    air: u32,
    water: u32,
    ice: u32,
    packed_ice: u32,
    blue_ice: u32,
    snow_block: u32,
    snow: u32,
}

impl IceBlocks {
    fn new() -> Result<Self, WorldgenDataError> {
        let state = |name: &str| {
            block_states()
                .default_state(name)
                .ok_or_else(|| WorldgenDataError::InvalidDefinition {
                    context: name.to_string(),
                    message: "unknown block".to_string(),
                })
        };
        Ok(Self {
            air: state("minecraft:air")?,
            water: state("minecraft:water")?,
            ice: state("minecraft:ice")?,
            packed_ice: state("minecraft:packed_ice")?,
            blue_ice: state("minecraft:blue_ice")?,
            snow_block: state("minecraft:snow_block")?,
            snow: state("minecraft:snow")?,
        })
    }

    /// `IcebergFeature.isIcebergState`.
    fn is_iceberg(&self, state: u32) -> bool {
        [self.packed_ice, self.snow_block, self.blue_ice]
            .iter()
            .any(|&block| same_block(state, block))
    }
}

/// A compiled frozen-biome feature.
#[derive(Debug)]
pub enum CompiledIce {
    /// `minecraft:spike`.
    Spike {
        /// The block of the spike.
        state: u32,
        /// Where the spike may stand.
        can_place_on: BlockPredicate,
        /// What the spike may replace, besides air.
        can_replace: BlockPredicate,
    },
    /// `minecraft:iceberg`, of the given block.
    Iceberg {
        /// The block of the iceberg.
        state: u32,
        /// The sea level, the iceberg's center height.
        sea_level: i32,
        /// The blocks it reads.
        blocks: IceBlocks,
    },
    /// `minecraft:blue_ice`.
    BlueIce {
        /// The sea level, which blue ice stays below.
        sea_level: i32,
        /// The blocks it reads.
        blocks: IceBlocks,
    },
}

/// Places a frozen-biome feature.
pub fn place_ice<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    ice: &CompiledIce,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    match ice {
        CompiledIce::Spike {
            state,
            can_place_on,
            can_replace,
        } => place_spike(world, *state, can_place_on, can_replace, random, origin),
        CompiledIce::Iceberg {
            state,
            sea_level,
            blocks,
        } => {
            Iceberg {
                world,
                random,
                blocks,
                state: *state,
            }
            .place([origin[0], *sea_level, origin[2]]);
            true
        }
        CompiledIce::BlueIce { sea_level, blocks } => {
            place_blue_ice(world, *sea_level, blocks, random, origin)
        }
    }
}

fn get<W: FeatureWorld>(world: &W, [x, y, z]: [i32; 3]) -> u32 {
    world.block_state(x, y, z)
}

fn set<W: FeatureWorld>(world: &mut W, [x, y, z]: [i32; 3], state: u32) {
    world.set_block_state(x, y, z, state);
}

fn offset([x, y, z]: [i32; 3], dx: i32, dy: i32, dz: i32) -> [i32; 3] {
    [x + dx, y + dy, z + dz]
}

/// `SpikeFeature.place`.
fn place_spike<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    state: u32,
    can_place_on: &BlockPredicate,
    can_replace: &BlockPredicate,
    random: &mut WorldgenRandom<R>,
    mut origin: [i32; 3],
) -> bool {
    let is_air = |world: &W, position| block_states().is_air(get(world, position));
    while is_air(world, origin) && origin[1] > world.min_y() + 2 {
        origin[1] -= 1;
    }
    if !can_place_on.test(world, origin) {
        return false;
    }
    origin[1] += random.next_int_bounded(4);
    let height = random.next_int_bounded(4) + 7;
    let width = height / 4 + random.next_int_bounded(2);
    if width > 1 && random.next_int_bounded(60) == 0 {
        origin[1] += 10 + random.next_int_bounded(30);
    }
    let replaceable =
        |world: &W, position| is_air(world, position) || can_replace.test(world, position);
    for dy in 0..height {
        let scale = (1.0 - dy as f32 / height as f32) * width as f32;
        let radius = ceil_f32(scale);
        for dx in -radius..=radius {
            let fx = dx.abs() as f32 - 0.25;
            for dz in -radius..=radius {
                let fz = dz.abs() as f32 - 0.25;
                if (dx != 0 || dz != 0) && fx * fx + fz * fz > scale * scale {
                    continue;
                }
                let edge = dx == -radius || dx == radius || dz == -radius || dz == radius;
                if edge && random.next_float() > 0.75 {
                    continue;
                }
                let above = offset(origin, dx, dy, dz);
                if replaceable(world, above) {
                    set(world, above, state);
                }
                if dy != 0 && radius > 1 {
                    let below = offset(origin, dx, -dy, dz);
                    if replaceable(world, below) {
                        set(world, below, state);
                    }
                }
            }
        }
    }
    let radius = (width - 1).clamp(0, 1);
    for dx in -radius..=radius {
        for dz in -radius..=radius {
            let mut position = offset(origin, dx, -1, dz);
            let mut run = if dx.abs() == 1 && dz.abs() == 1 {
                random.next_int_bounded(5)
            } else {
                50
            };
            while position[1] > 50 {
                let current = get(world, position);
                if !block_states().is_air(current)
                    && !can_replace.test(world, position)
                    && current != state
                {
                    break;
                }
                set(world, position, state);
                position[1] -= 1;
                run -= 1;
                if run <= 0 {
                    position[1] -= random.next_int_bounded(5) + 1;
                    run = random.next_int_bounded(5);
                }
            }
        }
    }
    true
}

/// `BlueIceFeature.place`.
fn place_blue_ice<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    sea_level: i32,
    blocks: &IceBlocks,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    let is = |world: &W, position, block| same_block(get(world, position), block);
    if origin[1] > sea_level - 1 {
        return false;
    }
    if !is(world, origin, blocks.water) && !is(world, offset(origin, 0, -1, 0), blocks.water) {
        return false;
    }
    let beside_packed_ice = Direction::ALL.iter().any(|&direction| {
        direction != Direction::Down && is(world, direction.step(origin), blocks.packed_ice)
    });
    if !beside_packed_ice {
        return false;
    }
    set(world, origin, blocks.blue_ice);
    for _ in 0..200 {
        let dy = random.next_int_bounded(5) - random.next_int_bounded(6);
        let mut radius = 3;
        if dy < 2 {
            radius += dy / 2;
        }
        if radius < 1 {
            continue;
        }
        let dx = random.next_int_bounded(radius) - random.next_int_bounded(radius);
        let dz = random.next_int_bounded(radius) - random.next_int_bounded(radius);
        let position = offset(origin, dx, dy, dz);
        let current = get(world, position);
        let open = block_states().is_air(current)
            || [blocks.water, blocks.packed_ice, blocks.ice]
                .iter()
                .any(|&block| same_block(current, block));
        if !open {
            continue;
        }
        if Direction::ALL
            .iter()
            .any(|&direction| is(world, direction.step(position), blocks.blue_ice))
        {
            set(world, position, blocks.blue_ice);
        }
    }
    true
}

/// `IcebergFeature`, with the world, random source and block it places with.
struct Iceberg<'a, W, R: RandomSource> {
    world: &'a mut W,
    random: &'a mut WorldgenRandom<R>,
    blocks: &'a IceBlocks,
    state: u32,
}

/// The shape every block of one iceberg shares.
#[derive(Clone, Copy)]
struct Shape {
    ellipse: bool,
    ellipse_c: i32,
    angle: f64,
    snow_on_top: bool,
}

impl<W: FeatureWorld, R: RandomSource> Iceberg<'_, W, R> {
    /// `IcebergFeature.place`, from its origin at the sea level.
    fn place(&mut self, origin: [i32; 3]) {
        let random = &mut *self.random;
        let snow_on_top = random.next_double() > 0.7;
        let angle = random.next_double() * 2.0 * std::f64::consts::PI;
        let ellipse_a = 11 - random.next_int_bounded(5);
        let ellipse_c = 3 + random.next_int_bounded(3);
        let ellipse = random.next_double() > 0.7;
        let mut over_water = if ellipse {
            random.next_int_bounded(6) + 6
        } else {
            random.next_int_bounded(15) + 3
        };
        if !ellipse && random.next_double() > 0.9 {
            over_water += random.next_int_bounded(19) + 7;
        }
        let under_water = (over_water + random.next_int_bounded(11)).min(18);
        let width = (over_water + random.next_int_bounded(7) - random.next_int_bounded(5)).min(11);
        let a = if ellipse { ellipse_a } else { 11 };
        let shape = Shape {
            ellipse,
            ellipse_c,
            angle,
            snow_on_top,
        };
        for x in -a..a {
            for z in -a..a {
                for y in 0..over_water {
                    let radius = if ellipse {
                        radius_ellipse(y, over_water, width)
                    } else {
                        self.radius_round(y, over_water, width)
                    };
                    if ellipse || x < radius {
                        self.iceberg_block(origin, over_water, [x, y, z], radius, a, shape);
                    }
                }
            }
        }
        self.smooth(origin, width, over_water, ellipse, ellipse_a);
        for x in -a..a {
            for z in -a..a {
                let mut y = -1;
                while y > -under_water {
                    let a = if ellipse {
                        let squared = (f64::from(y) * f64::from(y)) as f32;
                        ceil_f32(a as f32 * (1.0 - squared / (under_water as f32 * 8.0)))
                    } else {
                        a
                    };
                    let radius = self.radius_steep(-y, under_water, width);
                    if x < radius {
                        self.iceberg_block(origin, under_water, [x, y, z], radius, a, shape);
                    }
                    y -= 1;
                }
            }
        }
        let cut = if ellipse {
            self.random.next_double() > 0.1
        } else {
            self.random.next_double() > 0.7
        };
        if cut {
            self.cut_out(width, over_water, origin, shape, ellipse_a);
        }
    }

    /// `IcebergFeature.heightDependentRadiusRound`.
    fn radius_round(&mut self, y: i32, height: i32, width: i32) -> i32 {
        let k = 3.5 - self.random.next_float();
        let squared = (f64::from(y) * f64::from(y)) as f32;
        let mut f = (1.0 - squared / (height as f32 * k)) * width as f32;
        if height > 15 + self.random.next_int_bounded(5) {
            let l = if y < 3 + self.random.next_int_bounded(6) {
                y / 2
            } else {
                y
            };
            f = (1.0 - l as f32 / (height as f32 * k * 0.4)) * width as f32;
        }
        ceil_f32(f / 2.0)
    }

    /// `IcebergFeature.heightDependentRadiusSteep`.
    fn radius_steep(&mut self, y: i32, height: i32, width: i32) -> i32 {
        let k = 1.0 + self.random.next_float() / 2.0;
        let f = (1.0 - y as f32 / (height as f32 * k)) * width as f32;
        ceil_f32(f / 2.0)
    }

    /// `IcebergFeature.generateIcebergBlock`.
    fn iceberg_block(
        &mut self,
        origin: [i32; 3],
        height: i32,
        [x, y, z]: [i32; 3],
        radius: i32,
        a: i32,
        shape: Shape,
    ) {
        let distance = if shape.ellipse {
            let c = ellipse_c(y, height, shape.ellipse_c);
            distance_ellipse(x, z, [0, 0, 0], a, c, shape.angle)
        } else {
            let f = 10.0 * self.random.next_float().clamp(0.2, 0.8) / radius as f32;
            f64::from(f) + square(x) + square(z) - square(radius)
        };
        // Under the water an ellipse's `a` can reach zero, and the distance at its center is then NaN, which
        // vanilla's `distance < 0` rejects.
        if distance.partial_cmp(&0.0) != Some(Ordering::Less) {
            return;
        }
        let position = offset(origin, x, y, z);
        let threshold = if shape.ellipse {
            -0.5
        } else {
            f64::from(-6 - self.random.next_int_bounded(3))
        };
        if distance > threshold && self.random.next_double() > 0.9 {
            return;
        }
        self.iceberg_set(position, height - y, height, shape);
    }

    /// `IcebergFeature.setIcebergBlock`: snow in the upper part when it has snow on top, otherwise its block.
    fn iceberg_set(&mut self, position: [i32; 3], depth: i32, height: i32, shape: Shape) {
        let blocks = self.blocks;
        let current = get(self.world, position);
        let open = block_states().is_air(current)
            || [blocks.snow_block, blocks.ice, blocks.water]
                .iter()
                .any(|&block| same_block(current, block));
        if !open {
            return;
        }
        let random = &mut *self.random;
        let randomness = !shape.ellipse || random.next_double() > 0.05;
        let divisor = if shape.ellipse { 3 } else { 2 };
        let snow = shape.snow_on_top
            && !same_block(current, blocks.water)
            && f64::from(depth)
                <= f64::from(random.next_int_bounded((height / divisor).max(1)))
                    + f64::from(height) * 0.6
            && randomness;
        set(
            self.world,
            position,
            if snow { blocks.snow_block } else { self.state },
        );
    }

    /// `IcebergFeature.smooth`: removes iceberg blocks over air, with the block above, and those with at least
    /// three sides open.
    fn smooth(&mut self, origin: [i32; 3], width: i32, height: i32, ellipse: bool, ellipse_a: i32) {
        let blocks = self.blocks;
        let radius = if ellipse { ellipse_a } else { width / 2 };
        for x in -radius..=radius {
            for z in -radius..=radius {
                for y in 0..=height {
                    let position = offset(origin, x, y, z);
                    let current = get(self.world, position);
                    let iceberg = blocks.is_iceberg(current);
                    if !iceberg && !same_block(current, blocks.snow) {
                        continue;
                    }
                    if block_states().is_air(get(self.world, offset(position, 0, -1, 0))) {
                        set(self.world, position, blocks.air);
                        set(self.world, offset(position, 0, 1, 0), blocks.air);
                    } else if iceberg {
                        let open = [[-1, 0], [1, 0], [0, -1], [0, 1]]
                            .iter()
                            .filter(|[dx, dz]| {
                                !blocks.is_iceberg(get(self.world, offset(position, *dx, 0, *dz)))
                            })
                            .count();
                        if open >= 3 {
                            set(self.world, position, blocks.air);
                        }
                    }
                }
            }
        }
    }

    /// `IcebergFeature.generateCutOut`: carves an ellipse through the iceberg from a random offset, air above the
    /// water and water below.
    fn cut_out(&mut self, width: i32, height: i32, origin: [i32; 3], shape: Shape, ellipse_a: i32) {
        let random = &mut *self.random;
        let x_sign = if random.next_bool() { -1 } else { 1 };
        let z_sign = if random.next_bool() { -1 } else { 1 };
        let side = |random: &mut WorldgenRandom<R>| {
            let mut offset = random.next_int_bounded((width / 2 - 2).max(1));
            if random.next_bool() {
                offset = width / 2 + 1 - random.next_int_bounded((width - width / 2 - 1).max(1));
            }
            offset
        };
        let mut x_offset = side(random);
        let mut z_offset = side(random);
        if shape.ellipse {
            x_offset = random.next_int_bounded((ellipse_a - 5).max(1));
            z_offset = x_offset;
        }
        let cut = [x_sign * x_offset, 0, z_sign * z_offset];
        let angle = if shape.ellipse {
            shape.angle + std::f64::consts::FRAC_PI_2
        } else {
            random.next_double() * 2.0 * std::f64::consts::PI
        };
        for y in 0..height - 3 {
            let radius = self.radius_round(y, height, width);
            self.carve(
                radius,
                y,
                origin,
                false,
                angle,
                cut,
                ellipse_a,
                shape.ellipse_c,
            );
        }
        let mut y = -1;
        while y > -height + self.random.next_int_bounded(5) {
            let radius = self.radius_steep(-y, height, width);
            self.carve(
                radius,
                y,
                origin,
                true,
                angle,
                cut,
                ellipse_a,
                shape.ellipse_c,
            );
            y -= 1;
        }
    }

    /// `IcebergFeature.carve`.
    #[allow(clippy::too_many_arguments)]
    fn carve(
        &mut self,
        radius: i32,
        y: i32,
        origin: [i32; 3],
        under_water: bool,
        angle: f64,
        cut: [i32; 3],
        ellipse_a: i32,
        ellipse_c: i32,
    ) {
        let blocks = self.blocks;
        let a = radius + 1 + ellipse_a / 3;
        let c = (radius - 3).min(3) + ellipse_c / 2 - 1;
        for x in -a..a {
            for z in -a..a {
                // NaN, from an ellipse whose `c` is zero, is rejected as in vanilla.
                if distance_ellipse(x, z, cut, a, c, angle).partial_cmp(&0.0)
                    != Some(Ordering::Less)
                {
                    continue;
                }
                let position = offset(origin, x, y, z);
                let current = get(self.world, position);
                if !blocks.is_iceberg(current) && !same_block(current, blocks.snow_block) {
                    continue;
                }
                if under_water {
                    set(self.world, position, blocks.water);
                } else {
                    set(self.world, position, blocks.air);
                    // `removeFloatingSnowLayer`.
                    let above = offset(position, 0, 1, 0);
                    if same_block(get(self.world, above), blocks.snow) {
                        set(self.world, above, blocks.air);
                    }
                }
            }
        }
    }
}

fn square(value: i32) -> f64 {
    f64::from(value) * f64::from(value)
}

/// `IcebergFeature.heightDependentRadiusEllipse`.
fn radius_ellipse(y: i32, height: i32, width: i32) -> i32 {
    let squared = (f64::from(y) * f64::from(y)) as f32;
    let f = (1.0 - squared / (height as f32 * 1.0)) * width as f32;
    ceil_f32(f / 2.0)
}

/// `IcebergFeature.getEllipseC`: the ellipse narrows in the top three layers.
fn ellipse_c(y: i32, height: i32, c: i32) -> i32 {
    if y > 0 && height - y <= 3 {
        c - (4 - (height - y))
    } else {
        c
    }
}

/// `IcebergFeature.signedDistanceEllipse`.
fn distance_ellipse(x: i32, z: i32, center: [i32; 3], a: i32, c: i32, angle: f64) -> f64 {
    let dx = f64::from(x - center[0]);
    let dz = f64::from(z - center[2]);
    let u = (dx * angle.cos() - dz * angle.sin()) / f64::from(a);
    let v = (dx * angle.sin() + dz * angle.cos()) / f64::from(c);
    u * u + v * v - 1.0
}

impl FeatureCompiler<'_> {
    /// Compiles `minecraft:spike`, `minecraft:iceberg` or `minecraft:blue_ice`; None for parts not ported yet.
    pub(crate) fn ice(
        &mut self,
        name: &str,
        kind: &str,
        config: &Value,
    ) -> Result<Option<CompiledIce>, WorldgenDataError> {
        let field = |key: &str| {
            config
                .get(key)
                .ok_or_else(|| WorldgenDataError::InvalidDefinition {
                    context: name.to_string(),
                    message: format!("missing field `{key}`"),
                })
        };
        let state = |key: &str| -> Result<u32, WorldgenDataError> {
            resolve_block_state(&parse_block_state_def(name, field(key)?)?, name)
        };
        // Both features read the Overworld's sea level, as `ChunkGenerator.getSeaLevel` and
        // `WorldGenRegion.getSeaLevel` give it there.
        let sea_level = self
            .data()
            .noise_settings
            .get("minecraft:overworld")
            .map_or(63, |settings| settings.sea_level);
        Ok(Some(match kind {
            "minecraft:spike" => {
                let (Some(can_place_on), Some(can_replace)) = (
                    self.predicate(name, field("can_place_on")?)?,
                    self.predicate(name, field("can_replace")?)?,
                ) else {
                    return Ok(None);
                };
                CompiledIce::Spike {
                    state: state("state")?,
                    can_place_on,
                    can_replace,
                }
            }
            "minecraft:iceberg" => CompiledIce::Iceberg {
                state: state("state")?,
                sea_level,
                blocks: IceBlocks::new()?,
            },
            "minecraft:blue_ice" => CompiledIce::BlueIce {
                sea_level,
                blocks: IceBlocks::new()?,
            },
            _ => return Ok(None),
        }))
    }
}
