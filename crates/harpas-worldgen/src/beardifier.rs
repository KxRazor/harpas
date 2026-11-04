//! Structure terrain adaptation, measured against vanilla's `Beardifier`.
//!
//! The caller supplies the referenced starts in their iteration order. Pieces and junctions retain that
//! order: changing the order of additions can change the density at a terrain boundary.

use std::sync::OnceLock;

use crate::data::{PoolElementDef, Projection, WorldgenData, WorldgenDataError};
use crate::structure::{BoundingBox, StructureStart};

const RADIUS: i32 = 12;
const KERNEL_SIZE: usize = 24;

#[derive(Debug, Clone, Copy)]
enum Adjustment {
    None,
    Bury,
    BeardThin,
    BeardBox,
    Encapsulate,
}

#[derive(Debug, Clone)]
struct Rigid {
    bounds: BoundingBox,
    adjustment: Adjustment,
    ground_level_delta: i32,
}

/// The immutable structure contribution to a chunk's terrain density (`Beardifier`).
/// An empty sampler is equivalent to `generate-structures=false`.
#[derive(Debug, Clone, Default)]
pub struct Beardifier {
    pieces: Vec<Rigid>,
    junctions: Vec<[i32; 3]>,
    affected_box: Option<BoundingBox>,
}

impl Beardifier {
    /// Collects the nearby rigid pieces and jigsaw junctions of the chunk's **referenced** starts
    /// (`Beardifier.forStructuresInChunk`). Starts must be supplied in the same order as vanilla's
    /// structure manager; this method neither discovers starts nor computes references.
    pub fn for_starts<'a>(
        chunk_x: i32,
        chunk_z: i32,
        data: &WorldgenData,
        starts: impl IntoIterator<Item = &'a StructureStart>,
    ) -> Result<Self, WorldgenDataError> {
        let min_x = chunk_x * 16;
        let min_z = chunk_z * 16;
        let mut result = Self::default();
        for start in starts {
            let definition = data.structures.get(&start.structure).ok_or_else(|| {
                WorldgenDataError::InvalidDefinition {
                    context: start.structure.clone(),
                    message: "structure definition is missing".into(),
                }
            })?;
            let adjustment = match definition.terrain_adaptation.as_str() {
                "none" => Adjustment::None,
                "bury" => Adjustment::Bury,
                "beard_thin" => Adjustment::BeardThin,
                "beard_box" => Adjustment::BeardBox,
                "encapsulate" => Adjustment::Encapsulate,
                other => {
                    return Err(WorldgenDataError::InvalidDefinition {
                        context: start.structure.clone(),
                        message: format!("unsupported terrain adaptation `{other}`"),
                    });
                }
            };
            if matches!(adjustment, Adjustment::None) {
                continue;
            }
            for piece in &start.pieces {
                let bounds = piece.bounding_box;
                if !bounds.intersects_columns(
                    min_x - RADIUS,
                    min_z - RADIUS,
                    min_x + 15 + RADIUS,
                    min_z + 15 + RADIUS,
                ) {
                    continue;
                }
                if let Some(jigsaw) = &piece.jigsaw {
                    let rigid = match &jigsaw.element {
                        PoolElementDef::Single { projection, .. }
                        | PoolElementDef::List { projection, .. }
                        | PoolElementDef::Feature { projection, .. } => {
                            *projection == Projection::Rigid
                        }
                        PoolElementDef::Empty => false,
                    };
                    if rigid {
                        result.pieces.push(Rigid {
                            bounds,
                            adjustment,
                            ground_level_delta: jigsaw.ground_level_delta,
                        });
                        result.include_box(bounds);
                    }
                    // Junctions on terrain-matching pieces participate too. These bounds are strict,
                    // unlike the inclusive piece/chunk intersection above.
                    for junction in &piece.junctions {
                        let [x, _, z] = junction.source;
                        if x > min_x - RADIUS
                            && x < min_x + 15 + RADIUS
                            && z > min_z - RADIUS
                            && z < min_z + 15 + RADIUS
                        {
                            result.junctions.push(junction.source);
                            result.include_box(BoundingBox {
                                min: junction.source,
                                max: junction.source,
                            });
                        }
                    }
                } else {
                    result.pieces.push(Rigid {
                        bounds,
                        adjustment,
                        ground_level_delta: 0,
                    });
                    result.include_box(bounds);
                }
            }
        }
        // Vanilla clips scalar samples to the collected bounds enlarged by the kernel's full size.
        if let Some(bounds) = &mut result.affected_box {
            bounds.min = bounds.min.map(|v| v - KERNEL_SIZE as i32);
            bounds.max = bounds.max.map(|v| v + KERNEL_SIZE as i32);
        }
        Ok(result)
    }

    fn include_box(&mut self, bounds: BoundingBox) {
        if let Some(current) = &mut self.affected_box {
            for axis in 0..3 {
                current.min[axis] = current.min[axis].min(bounds.min[axis]);
                current.max[axis] = current.max[axis].max(bounds.max[axis]);
            }
        } else {
            self.affected_box = Some(bounds);
        }
    }

    /// Whether there are no nearby pieces or junctions that change the terrain.
    pub fn is_empty(&self) -> bool {
        self.affected_box.is_none()
    }

    /// Structure density at a block position, before aquifer evaluation and after noise interpolation.
    pub fn compute(&self, x: i32, y: i32, z: i32) -> f64 {
        let Some(bounds) = self.affected_box else {
            return 0.0;
        };
        if !(0..3).all(|axis| {
            let value = [x, y, z][axis];
            value >= bounds.min[axis] && value <= bounds.max[axis]
        }) {
            return 0.0;
        }
        let mut density = 0.0;
        for piece in &self.pieces {
            let bounds = piece.bounds;
            let dx = distance(x, bounds.min[0], bounds.max[0]);
            let dz = distance(z, bounds.min[2], bounds.max[2]);
            let ground = bounds.min[1] + piece.ground_level_delta;
            let dy = y - ground;
            density += match piece.adjustment {
                Adjustment::None => 0.0,
                Adjustment::Bury => bury(f64::from(dx), f64::from(dy) / 2.0, f64::from(dz)),
                Adjustment::BeardThin => beard(dx, dy, dz, dy) * 0.8,
                Adjustment::BeardBox => beard(dx, distance(y, ground, bounds.max[1]), dz, dy) * 0.8,
                Adjustment::Encapsulate => {
                    bury(
                        f64::from(dx) / 2.0,
                        f64::from(distance(y, bounds.min[1], bounds.max[1])) / 2.0,
                        f64::from(dz) / 2.0,
                    ) * 0.8
                }
            };
        }
        for &[jx, jy, jz] in &self.junctions {
            density += beard(x - jx, y - jy, z - jz, y - jy) * 0.4;
        }
        density
    }
}

fn distance(value: i32, min: i32, max: i32) -> i32 {
    0.max(min - value).max(value - max)
}

fn bury(dx: f64, dy: f64, dz: f64) -> f64 {
    let length = (dx * dx + dy * dy + dz * dz).sqrt();
    if length > 6.0 {
        0.0
    } else {
        1.0 - length / 6.0
    }
}

fn beard(dx: i32, dy: i32, dz: i32, ground_dy: i32) -> f64 {
    if !(-RADIUS..RADIUS).contains(&dx)
        || !(-RADIUS..RADIUS).contains(&dy)
        || !(-RADIUS..RADIUS).contains(&dz)
    {
        return 0.0;
    }
    let index = ((dz + RADIUS) as usize * KERNEL_SIZE + (dx + RADIUS) as usize) * KERNEL_SIZE
        + (dy + RADIUS) as usize;
    let vertical = f64::from(ground_dy) + 0.5;
    let squared = f64::from(dx * dx + dz * dz);
    let factor = -vertical * fast_inv_sqrt(vertical * vertical / 2.0 + squared / 2.0) / 2.0;
    factor * f64::from(kernel()[index])
}

fn fast_inv_sqrt(value: f64) -> f64 {
    let half = 0.5 * value;
    let estimate = f64::from_bits(6_910_469_410_427_058_090 - (value.to_bits() >> 1));
    estimate * (1.5 - half * estimate * estimate)
}

fn kernel() -> &'static [f32] {
    static KERNEL: OnceLock<Vec<f32>> = OnceLock::new();
    KERNEL.get_or_init(|| {
        let mut values = Vec::with_capacity(KERNEL_SIZE.pow(3));
        for z in -RADIUS..RADIUS {
            for x in -RADIUS..RADIUS {
                for y in -RADIUS..RADIUS {
                    let vertical = f64::from(y) + 0.5;
                    let squared = f64::from(x * x + z * z);
                    values.push((-(vertical * vertical / 16.0 + squared / 16.0)).exp() as f32);
                }
            }
        }
        values
    })
}
