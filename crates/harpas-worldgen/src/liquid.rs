//! The features that place liquids: `minecraft:spring_feature` (`SpringFeature`), the water and lava springs in
//! cave and mountain walls, and `minecraft:lake` (`LakeFeature`), the lava lakes on the surface and underground.
//!
//! A spring replaces air or a valid block that has a valid block above it and, if the configuration asks, below
//! it, when exactly `rock_count` of the blocks at its west, east, north, south and below are valid and exactly
//! `hole_count` of them are air. A lake carves four to seven ellipsoids into a 16x8x16 box whose corner is eight
//! blocks west and north of the origin and four below it: the lower half fills with the fluid and the upper half
//! with cave air, unless liquid touches the upper half or the lower half has a wall that is neither solid nor
//! the fluid; then the barrier block replaces the solid blocks around it, every one beside the fluid and half
//! of those beside the air.
//!
//! Vanilla also schedules a fluid tick at a spring and a block tick at each block of cave air in a lake; scheduled
//! ticks are not saved in proto-chunks yet. A lake of water freezes its surface in cold biomes, which is not
//! ported, so such a lake is not compiled; vanilla's data has none.

use harpas_core::block::block_states;
use harpas_core::block_behavior::block_behavior;
use harpas_core::random::{RandomSource, WorldgenRandom};
use serde_json::Value;

use crate::configured::{FeatureCompiler, StateProvider};
use crate::data::{WorldgenDataError, parse_holder_set};
use crate::feature::FeatureWorld;
use crate::placement::{Fluid, StateSet, fluid};

/// A compiled `minecraft:spring_feature`.
#[derive(Debug)]
pub struct CompiledSpring {
    /// The liquid block of the spring's fluid (`FluidState.createLegacyBlock`).
    state: u32,
    /// Whether the block below the spring must be valid.
    requires_block_below: bool,
    /// How many of the five blocks around the spring must be valid.
    rock_count: i32,
    /// How many of the five blocks around the spring must be air.
    hole_count: i32,
    /// The blocks a spring may flow out of.
    valid_blocks: StateSet,
}

/// A compiled `minecraft:lake`.
#[derive(Debug)]
pub struct CompiledLake {
    /// The fluid of the lower half.
    fluid: StateProvider,
    /// The block around the lake; air for none.
    barrier: StateProvider,
    /// `#features_cannot_replace`, which the lake leaves in place.
    cannot_replace: StateSet,
    /// `#lava_pool_stone_cannot_replace`, which the barrier leaves in place.
    barrier_cannot_replace: StateSet,
    /// The air of the upper half, `minecraft:cave_air`.
    cave_air: u32,
}

/// A compiled liquid feature.
#[derive(Debug)]
pub enum CompiledLiquid {
    /// `minecraft:spring_feature`.
    Spring(CompiledSpring),
    /// `minecraft:lake`.
    Lake(Box<CompiledLake>),
}

/// Places a liquid feature.
pub fn place_liquid<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    liquid: &CompiledLiquid,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    match liquid {
        CompiledLiquid::Spring(spring) => place_spring(world, spring, origin),
        CompiledLiquid::Lake(lake) => place_lake(world, lake, random, origin),
    }
}

fn is_air(state: u32) -> bool {
    block_states().is_air(state)
}

/// `SpringFeature.place`, without the fluid tick it schedules.
fn place_spring<W: FeatureWorld>(
    world: &mut W,
    spring: &CompiledSpring,
    [x, y, z]: [i32; 3],
) -> bool {
    let valid = |world: &W, x, y, z| spring.valid_blocks.contains(world.block_state(x, y, z));
    if !valid(world, x, y + 1, z) {
        return false;
    }
    if spring.requires_block_below && !valid(world, x, y - 1, z) {
        return false;
    }
    let state = world.block_state(x, y, z);
    if !is_air(state) && !spring.valid_blocks.contains(state) {
        return false;
    }
    let around = [
        [x - 1, y, z],
        [x + 1, y, z],
        [x, y, z - 1],
        [x, y, z + 1],
        [x, y - 1, z],
    ];
    let mut rocks = 0;
    let mut holes = 0;
    for [x, y, z] in around {
        let state = world.block_state(x, y, z);
        rocks += i32::from(spring.valid_blocks.contains(state));
        holes += i32::from(is_air(state));
    }
    if rocks != spring.rock_count || holes != spring.hole_count {
        return false;
    }
    world.set_block_state(x, y, z, spring.state);
    true
}

/// The cells of a lake's box: 16 along X and Z and 8 along Y.
struct LakeShape([bool; 2048]);

impl LakeShape {
    fn index(x: i32, y: i32, z: i32) -> usize {
        ((x * 16 + z) * 8 + y) as usize
    }

    fn filled(&self, x: i32, y: i32, z: i32) -> bool {
        self.0[Self::index(x, y, z)]
    }

    /// Whether an empty cell has a filled cell at one of its faces.
    fn is_edge(&self, x: i32, y: i32, z: i32) -> bool {
        !self.filled(x, y, z)
            && (x < 15 && self.filled(x + 1, y, z)
                || x > 0 && self.filled(x - 1, y, z)
                || z < 15 && self.filled(x, y, z + 1)
                || z > 0 && self.filled(x, y, z - 1)
                || y < 7 && self.filled(x, y + 1, z)
                || y > 0 && self.filled(x, y - 1, z))
    }
}

/// The cells of a lake's box in vanilla's order: X, then Z, then Y.
fn lake_cells() -> impl Iterator<Item = (i32, i32, i32)> {
    (0..16).flat_map(|x| (0..16).flat_map(move |z| (0..8).map(move |y| (x, y, z))))
}

/// `Feature.markAboveForPostProcessing`: marks the two blocks above a position, up to the first air.
fn mark_above_for_postprocessing<W: FeatureWorld>(world: &mut W, [x, y, z]: [i32; 3]) {
    for y in [y + 1, y + 2] {
        if is_air(world.block_state(x, y, z)) {
            return;
        }
        world.mark_postprocessing(x, y, z);
    }
}

/// `LakeFeature.place`, without the block ticks it schedules.
fn place_lake<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    lake: &CompiledLake,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    if origin[1] <= world.min_y() + 4 {
        return false;
    }
    let corner = [origin[0] - 8, origin[1] - 4, origin[2] - 8];
    let at = |x: i32, y: i32, z: i32| [corner[0] + x, corner[1] + y, corner[2] + z];

    let mut shape = LakeShape([false; 2048]);
    let spots = random.next_int_bounded(4) + 4;
    for _ in 0..spots {
        let x_radius = random.next_double() * 6.0 + 3.0;
        let y_radius = random.next_double() * 4.0 + 2.0;
        let z_radius = random.next_double() * 6.0 + 3.0;
        let x_center = random.next_double() * (16.0 - x_radius - 2.0) + 1.0 + x_radius / 2.0;
        let y_center = random.next_double() * (8.0 - y_radius - 4.0) + 2.0 + y_radius / 2.0;
        let z_center = random.next_double() * (16.0 - z_radius - 2.0) + 1.0 + z_radius / 2.0;
        for x in 1..15 {
            for z in 1..15 {
                for y in 1..7 {
                    let dx = (f64::from(x) - x_center) / (x_radius / 2.0);
                    let dy = (f64::from(y) - y_center) / (y_radius / 2.0);
                    let dz = (f64::from(z) - z_center) / (z_radius / 2.0);
                    if dx * dx + dy * dy + dz * dz < 1.0 {
                        shape.0[LakeShape::index(x, y, z)] = true;
                    }
                }
            }
        }
    }

    let fluid = lake.fluid.state(world, random, corner);
    for (x, y, z) in lake_cells() {
        if !shape.is_edge(x, y, z) {
            continue;
        }
        let [bx, by, bz] = at(x, y, z);
        let state = world.block_state(bx, by, bz);
        if y >= 4 && block_behavior().is_liquid(state) {
            return false;
        }
        if y < 4 && !block_behavior().is_solid(state) && state != fluid {
            return false;
        }
    }

    for (x, y, z) in lake_cells() {
        if !shape.filled(x, y, z) {
            continue;
        }
        let [bx, by, bz] = at(x, y, z);
        if lake.cannot_replace.contains(world.block_state(bx, by, bz)) {
            continue;
        }
        if y >= 4 {
            world.set_block_state(bx, by, bz, lake.cave_air);
            mark_above_for_postprocessing(world, [bx, by, bz]);
        } else {
            world.set_block_state(bx, by, bz, fluid);
        }
    }

    let barrier = lake.barrier.state(world, random, corner);
    if !is_air(barrier) {
        for (x, y, z) in lake_cells() {
            if !shape.is_edge(x, y, z) || (y >= 4 && random.next_int_bounded(2) == 0) {
                continue;
            }
            let [bx, by, bz] = at(x, y, z);
            let state = world.block_state(bx, by, bz);
            if block_behavior().is_solid(state) && !lake.barrier_cannot_replace.contains(state) {
                world.set_block_state(bx, by, bz, barrier);
                mark_above_for_postprocessing(world, [bx, by, bz]);
            }
        }
    }
    true
}

fn invalid(context: &str, message: impl Into<String>) -> WorldgenDataError {
    WorldgenDataError::InvalidDefinition {
        context: context.to_string(),
        message: message.into(),
    }
}

impl FeatureCompiler<'_> {
    /// Compiles `minecraft:spring_feature` or `minecraft:lake`; None for parts not ported yet.
    pub(crate) fn liquid(
        &mut self,
        name: &str,
        kind: &str,
        config: &Value,
    ) -> Result<Option<CompiledLiquid>, WorldgenDataError> {
        let field = |key: &str| {
            config
                .get(key)
                .ok_or_else(|| invalid(name, format!("missing field `{key}`")))
        };
        let tag = |tag: &str| -> Result<StateSet, WorldgenDataError> {
            Ok(StateSet::from_membership(
                &self.data().block_tag_states(tag)?,
            ))
        };
        if kind == "minecraft:lake" {
            let cannot_replace = tag("minecraft:features_cannot_replace")?;
            let barrier_cannot_replace = tag("minecraft:lava_pool_stone_cannot_replace")?;
            let (Some(fluid_provider), Some(barrier)) = (
                self.provider(name, field("fluid")?)?,
                self.provider(name, field("barrier")?)?,
            ) else {
                return Ok(None);
            };
            // Only a provider of one state shows here that the lake never holds water, which would freeze.
            let StateProvider::Simple(state) = fluid_provider else {
                return Ok(None);
            };
            if matches!(fluid(state), Fluid::Water | Fluid::FlowingWater) {
                return Ok(None);
            }
            let cave_air = block_states()
                .default_state("minecraft:cave_air")
                .ok_or_else(|| invalid(name, "unknown block minecraft:cave_air"))?;
            return Ok(Some(CompiledLiquid::Lake(Box::new(CompiledLake {
                fluid: fluid_provider,
                barrier,
                cannot_replace,
                barrier_cannot_replace,
                cave_air,
            }))));
        }

        // A source fluid's block is the liquid block at level 0, its default state, whether falling or not.
        let fluid_name = field("state")?
            .get("Name")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid(name, "a spring needs a fluid state"))?;
        let block = match fluid_name.strip_prefix("minecraft:").unwrap_or(fluid_name) {
            "water" => "minecraft:water",
            "lava" => "minecraft:lava",
            _ => return Ok(None),
        };
        let state = block_states()
            .default_state(block)
            .ok_or_else(|| invalid(name, format!("unknown block {block}")))?;
        let entries = parse_holder_set(name, field("valid_blocks")?)?;
        let valid_blocks = match entries.as_slice() {
            [entry] if entry.starts_with('#') => tag(entry)?,
            names => {
                let states = block_states();
                let membership: Vec<bool> = (0..states.len())
                    .map(|id| {
                        states
                            .properties(id)
                            .is_some_and(|(block, _)| names.iter().any(|name| name == block))
                    })
                    .collect();
                StateSet::from_membership(&membership)
            }
        };
        let boolean =
            |key: &str, default: bool| config.get(key).and_then(Value::as_bool).unwrap_or(default);
        let count = |key: &str| {
            config.get(key).and_then(Value::as_i64).map_or(
                Ok(if key == "rock_count" { 4 } else { 1 }),
                |count| {
                    i32::try_from(count)
                        .map_err(|_| invalid(name, format!("`{key}` is out of range")))
                },
            )
        };
        Ok(Some(CompiledLiquid::Spring(CompiledSpring {
            state,
            requires_block_below: boolean("requires_block_below", true),
            rock_count: count("rock_count")?,
            hole_count: count("hole_count")?,
            valid_blocks,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::RegionFeatureWorld;
    use crate::generator::GeneratedChunk;

    fn default_state(name: &str) -> u32 {
        block_states().default_state(name).unwrap()
    }

    fn spring() -> CompiledSpring {
        let stone = default_state("minecraft:stone");
        let membership: Vec<bool> = (0..block_states().len()).map(|id| id == stone).collect();
        CompiledSpring {
            state: default_state("minecraft:water"),
            requires_block_below: true,
            rock_count: 4,
            hole_count: 1,
            valid_blocks: StateSet::from_membership(&membership),
        }
    }

    /// A chunk of stone from y 0 to 15, with air at the given blocks.
    fn stone_with_holes(holes: &[[i32; 3]]) -> RegionFeatureWorld {
        let mut chunk = GeneratedChunk::new(0, 0, -64, 384, 1);
        for x in 0..16 {
            for z in 0..16 {
                for y in 0..16 {
                    chunk.set_block_state(x, y, z, default_state("minecraft:stone"));
                }
            }
        }
        for &[x, y, z] in holes {
            chunk.set_block_state(x as usize, y, z as usize, default_state("minecraft:air"));
        }
        RegionFeatureWorld::new([chunk], 0).unwrap()
    }

    #[test]
    fn a_spring_needs_one_opening_in_a_wall() {
        let water = default_state("minecraft:water");
        let mut world = stone_with_holes(&[[9, 8, 8]]);
        assert!(place_spring(&mut world, &spring(), [8, 8, 8]));
        assert_eq!(world.block_state(8, 8, 8), water);

        // Enclosed in stone, open on two sides, or without stone above, there is no spring.
        let mut world = stone_with_holes(&[]);
        assert!(!place_spring(&mut world, &spring(), [8, 8, 8]));
        let mut world = stone_with_holes(&[[9, 8, 8], [7, 8, 8]]);
        assert!(!place_spring(&mut world, &spring(), [8, 8, 8]));
        let mut world = stone_with_holes(&[[9, 8, 8], [8, 9, 8]]);
        assert!(!place_spring(&mut world, &spring(), [8, 8, 8]));
        assert_ne!(world.block_state(8, 8, 8), water);
    }

    #[test]
    fn an_edge_cell_is_empty_beside_a_filled_one() {
        let mut shape = LakeShape([false; 2048]);
        shape.0[LakeShape::index(5, 3, 5)] = true;
        assert!(!shape.is_edge(5, 3, 5));
        for (x, y, z) in [
            (4, 3, 5),
            (6, 3, 5),
            (5, 2, 5),
            (5, 4, 5),
            (5, 3, 4),
            (5, 3, 6),
        ] {
            assert!(shape.is_edge(x, y, z));
        }
        assert!(!shape.is_edge(6, 4, 5));
        assert_eq!(lake_cells().take(9).last(), Some((0, 0, 1)));
    }
}
