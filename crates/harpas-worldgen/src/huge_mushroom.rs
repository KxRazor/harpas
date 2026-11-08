//! `minecraft:huge_brown_mushroom` and `minecraft:huge_red_mushroom` (`AbstractHugeMushroomFeature`).
//!
//! A huge mushroom draws its height, four to six blocks and doubled one time in twelve, and needs to be inside
//! the world, on a block its predicate accepts, with only air or leaves in the space its cap and stem take. It
//! places its cap, then its stem, each block only over air or a block in `#replaceable_by_mushrooms`. A brown
//! cap is one flat layer with its corners cut and its sides facing out; a red cap is a dome four layers high,
//! with the faces of each block set by where it lies. Every state is drawn from its provider, the stem's at the
//! origin.

use harpas_core::block::block_states;
use harpas_core::random::{RandomSource, WorldgenRandom};
use serde_json::Value;

use crate::configured::{FeatureCompiler, StateProvider};
use crate::data::WorldgenDataError;
use crate::feature::FeatureWorld;
use crate::placement::{BlockPredicate, StateSet};
use crate::shape_update::{property, with_property};

/// Which cap a huge mushroom grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cap {
    /// `HugeBrownMushroomFeature`.
    Brown,
    /// `HugeRedMushroomFeature`.
    Red,
}

/// A compiled huge mushroom.
#[derive(Debug)]
pub struct CompiledHugeMushroom {
    cap: Cap,
    can_place_on: BlockPredicate,
    cap_provider: StateProvider,
    stem_provider: StateProvider,
    foliage_radius: i32,
    /// `#replaceable_by_mushrooms`.
    replaceable: StateSet,
    /// `#leaves`.
    leaves: StateSet,
}

/// `AbstractHugeMushroomFeature.place`.
pub fn place_huge_mushroom<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    mushroom: &CompiledHugeMushroom,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    // `getTreeHeight`.
    let mut height = random.next_int_bounded(3) + 4;
    if random.next_int_bounded(12) == 0 {
        height *= 2;
    }
    if !valid_position(world, mushroom, origin, height) {
        return false;
    }
    match mushroom.cap {
        Cap::Brown => brown_cap(world, mushroom, random, origin, height),
        Cap::Red => red_cap(world, mushroom, random, origin, height),
    }
    // `placeTrunk`.
    for dy in 0..height {
        let state = mushroom.stem_provider.state(world, random, origin);
        place_block(
            world,
            mushroom,
            [origin[0], origin[1] + dy, origin[2]],
            state,
        );
    }
    true
}

/// `AbstractHugeMushroomFeature.isValidPosition`.
fn valid_position(
    world: &impl FeatureWorld,
    mushroom: &CompiledHugeMushroom,
    [x, y, z]: [i32; 3],
    height: i32,
) -> bool {
    let max_y = world.min_y() + world.height() - 1;
    if y < world.min_y() + 1 || y + height + 1 > max_y {
        return false;
    }
    if !mushroom.can_place_on.test(world, [x, y - 1, z]) {
        return false;
    }
    for dy in 0..=height {
        let radius = mushroom.radius_for_height(dy);
        for dx in -radius..=radius {
            for dz in -radius..=radius {
                let state = world.block_state(x + dx, y + dy, z + dz);
                if !block_states().is_air(state) && !mushroom.leaves.contains(state) {
                    return false;
                }
            }
        }
    }
    true
}

impl CompiledHugeMushroom {
    /// `getTreeRadiusForHeight`, as `isValidPosition` calls it with a tree height of -1: a red mushroom checks
    /// only its stem's column, a brown one its cap's radius above three blocks.
    fn radius_for_height(&self, dy: i32) -> i32 {
        match self.cap {
            Cap::Brown if dy > 3 => self.foliage_radius,
            Cap::Brown | Cap::Red => 0,
        }
    }
}

/// `AbstractHugeMushroomFeature.placeMushroomBlock`.
fn place_block(
    world: &mut impl FeatureWorld,
    mushroom: &CompiledHugeMushroom,
    [x, y, z]: [i32; 3],
    state: u32,
) {
    let current = world.block_state(x, y, z);
    if block_states().is_air(current) || mushroom.replaceable.contains(current) {
        world.set_block_state(x, y, z, state);
    }
}

fn flag(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

/// Whether a state has every one of the faces.
fn has_faces(state: u32, faces: &[&str]) -> bool {
    faces.iter().all(|face| property(state, face).is_some())
}

/// `HugeBrownMushroomFeature.makeCap`.
fn brown_cap<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    mushroom: &CompiledHugeMushroom,
    random: &mut WorldgenRandom<R>,
    [x, y, z]: [i32; 3],
    height: i32,
) {
    let radius = mushroom.foliage_radius;
    for dx in -radius..=radius {
        for dz in -radius..=radius {
            let (min_x, max_x, min_z, max_z) =
                (dx == -radius, dx == radius, dz == -radius, dz == radius);
            let (edge_x, edge_z) = (min_x || max_x, min_z || max_z);
            if edge_x && edge_z {
                continue;
            }
            let position = [x + dx, y + height, z + dz];
            let mut state = mushroom.cap_provider.state(world, random, position);
            if has_faces(state, &["west", "east", "north", "south"]) {
                let west = min_x || (edge_z && dx == 1 - radius);
                let east = max_x || (edge_z && dx == radius - 1);
                let north = min_z || (edge_x && dz == 1 - radius);
                let south = max_z || (edge_x && dz == radius - 1);
                for (face, value) in [
                    ("west", west),
                    ("east", east),
                    ("north", north),
                    ("south", south),
                ] {
                    state = with_property(state, face, flag(value));
                }
            }
            place_block(world, mushroom, position, state);
        }
    }
}

/// `HugeRedMushroomFeature.makeCap`.
fn red_cap<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    mushroom: &CompiledHugeMushroom,
    random: &mut WorldgenRandom<R>,
    [x, y, z]: [i32; 3],
    height: i32,
) {
    for dy in height - 3..=height {
        let radius = if dy < height {
            mushroom.foliage_radius
        } else {
            mushroom.foliage_radius - 1
        };
        let inner = mushroom.foliage_radius - 2;
        for dx in -radius..=radius {
            for dz in -radius..=radius {
                let edge_x = dx == -radius || dx == radius;
                let edge_z = dz == -radius || dz == radius;
                if dy < height && edge_x == edge_z {
                    continue;
                }
                let position = [x + dx, y + dy, z + dz];
                let mut state = mushroom.cap_provider.state(world, random, position);
                if has_faces(state, &["west", "east", "north", "south", "up"]) {
                    for (face, value) in [
                        ("up", dy >= height - 1),
                        ("west", dx < -inner),
                        ("east", dx > inner),
                        ("north", dz < -inner),
                        ("south", dz > inner),
                    ] {
                        state = with_property(state, face, flag(value));
                    }
                }
                place_block(world, mushroom, position, state);
            }
        }
    }
}

impl FeatureCompiler<'_> {
    /// Compiles a `huge_brown_mushroom` or `huge_red_mushroom` configuration; None for parts not ported yet.
    pub(crate) fn huge_mushroom(
        &mut self,
        name: &str,
        red: bool,
        config: &Value,
    ) -> Result<Option<CompiledHugeMushroom>, WorldgenDataError> {
        let field = |key: &str| {
            config
                .get(key)
                .ok_or_else(|| WorldgenDataError::InvalidDefinition {
                    context: name.to_string(),
                    message: format!("missing field `{key}`"),
                })
        };
        let (Some(can_place_on), Some(cap_provider), Some(stem_provider)) = (
            self.predicate(name, field("can_place_on")?)?,
            self.provider(name, field("cap_provider")?)?,
            self.provider(name, field("stem_provider")?)?,
        ) else {
            return Ok(None);
        };
        let foliage_radius = match config.get("foliage_radius") {
            Some(value) => value
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())
                .ok_or_else(|| WorldgenDataError::InvalidDefinition {
                    context: name.to_string(),
                    message: "`foliage_radius` must be an integer".to_string(),
                })?,
            None => 2,
        };
        let tag = |tag: &str| -> Result<StateSet, WorldgenDataError> {
            Ok(StateSet::from_membership(
                &self.data().block_tag_states(tag)?,
            ))
        };
        Ok(Some(CompiledHugeMushroom {
            cap: if red { Cap::Red } else { Cap::Brown },
            can_place_on,
            cap_provider,
            stem_provider,
            foliage_radius,
            replaceable: tag("minecraft:replaceable_by_mushrooms")?,
            leaves: tag("minecraft:leaves")?,
        }))
    }
}
