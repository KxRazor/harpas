//! Blocks that cling to the faces of their neighbors: `minecraft:vines` (`VinesFeature`) and
//! `minecraft:multiface_growth` (`MultifaceGrowthFeature`) with glow lichen.
//!
//! A vine takes the first face, in `Direction` order without down, whose neighbor it can attach to, in an air block.
//! Glow lichen, in air or water, tries its configured directions in a shuffled order and attaches to the first
//! neighbor in `can_be_placed_on`. Failing that, for each direction it looks at the neighbor there, `search_range`
//! times over, as vanilla's search sets its position from the origin each time; an air, water or glow lichen
//! neighbor tries the other directions, shuffled again without the opposite one. A placed lichen is marked for
//! postprocessing and, by `chance_of_spreading`, spreads once with `MultifaceSpreader`: from its face toward one of
//! the six directions, shuffled, into its own block, the block beside it or around the corner, the first that can
//! take a face there.

use harpas_core::block::block_states;
use harpas_core::random::{RandomSource, WorldgenRandom};
use serde_json::Value;

use crate::configured::FeatureCompiler;
use crate::data::WorldgenDataError;
use crate::feature::FeatureWorld;
use crate::placement::{Fluid, StateSet, fluid};
use crate::shape_update::{Direction, can_attach_to, property, same_block, with_property};
use crate::tree_decorator::HORIZONTAL;

/// A compiled `minecraft:multiface_growth` for glow lichen.
#[derive(Debug)]
pub struct CompiledMultifaceGrowth {
    /// The default state of the block placed.
    block: u32,
    search_range: i32,
    chance_of_spreading: f32,
    can_be_placed_on: StateSet,
    /// Up for the ceiling, down for the floor, then the horizontal directions for walls, in vanilla's order.
    valid_directions: Vec<Direction>,
}

fn is_air<W: FeatureWorld>(world: &W, [x, y, z]: [i32; 3]) -> bool {
    block_states().is_air(world.block_state(x, y, z))
}

fn is_water(state: u32) -> bool {
    block_states()
        .properties(state)
        .is_some_and(|(name, _)| name == "minecraft:water")
}

/// `VinesFeature.place`.
pub fn place_vines<W: FeatureWorld>(world: &mut W, origin: [i32; 3]) -> bool {
    if !is_air(world, origin) {
        return false;
    }
    let Some(vine) = block_states().default_state("minecraft:vine") else {
        return false;
    };
    for direction in Direction::ALL {
        if direction == Direction::Down || !can_attach_to(world, direction.step(origin), direction)
        {
            continue;
        }
        let [x, y, z] = origin;
        world.set_block_state(x, y, z, with_property(vine, direction.name(), "true"));
        return true;
    }
    false
}

/// `Util.shuffle`: from the end, each element swaps with a random one at or before it.
fn shuffle<R: RandomSource>(directions: &mut [Direction], random: &mut WorldgenRandom<R>) {
    for i in (2..=directions.len()).rev() {
        let j = random.next_int_bounded(i as i32) as usize;
        directions.swap(i - 1, j);
    }
}

fn axis(direction: Direction) -> u8 {
    match direction {
        Direction::Down | Direction::Up => 1,
        Direction::North | Direction::South => 2,
        Direction::West | Direction::East => 0,
    }
}

impl CompiledMultifaceGrowth {
    fn is_block(&self, state: u32) -> bool {
        same_block(state, self.block)
    }

    /// `MultifaceBlock.hasFace`: false for another block.
    fn has_face(&self, state: u32, direction: Direction) -> bool {
        self.is_block(state) && property(state, direction.name()) == Some("true")
    }

    /// `MultifaceBlock.isValidStateForPlacement`: the face is not there yet and its neighbor can be attached to.
    fn valid_for_placement<W: FeatureWorld>(
        &self,
        world: &W,
        state: u32,
        position: [i32; 3],
        direction: Direction,
    ) -> bool {
        !self.has_face(state, direction)
            && can_attach_to(world, direction.step(position), direction)
    }

    /// `MultifaceBlock.getStateForPlacement`: the current block with one more face, or a new one, waterlogged in a
    /// water source.
    fn state_for_placement<W: FeatureWorld>(
        &self,
        world: &W,
        current: u32,
        position: [i32; 3],
        direction: Direction,
    ) -> Option<u32> {
        if !self.valid_for_placement(world, current, position, direction) {
            return None;
        }
        let base = if self.is_block(current) {
            current
        } else if fluid(current) == Fluid::Water {
            with_property(self.block, "waterlogged", "true")
        } else {
            self.block
        };
        Some(with_property(base, direction.name(), "true"))
    }

    /// `MultifaceGrowthFeature.placeGrowthIfPossible`.
    fn place_growth<W: FeatureWorld, R: RandomSource>(
        &self,
        world: &mut W,
        position: [i32; 3],
        state: u32,
        random: &mut WorldgenRandom<R>,
        directions: &[Direction],
    ) -> bool {
        for &direction in directions {
            let [nx, ny, nz] = direction.step(position);
            if !self
                .can_be_placed_on
                .contains(world.block_state(nx, ny, nz))
            {
                continue;
            }
            let Some(placed) = self.state_for_placement(world, state, position, direction) else {
                return false;
            };
            let [x, y, z] = position;
            world.set_block_state(x, y, z, placed);
            world.mark_postprocessing(x, y, z);
            if random.next_float() < self.chance_of_spreading {
                self.spread_toward_random_direction(world, placed, position, direction, random);
            }
            return true;
        }
        false
    }

    /// `MultifaceSpreader.spreadFromFaceTowardRandomDirection`, marking what it places for postprocessing.
    fn spread_toward_random_direction<W: FeatureWorld, R: RandomSource>(
        &self,
        world: &mut W,
        state: u32,
        position: [i32; 3],
        face: Direction,
        random: &mut WorldgenRandom<R>,
    ) {
        let mut directions = Direction::ALL;
        shuffle(&mut directions, random);
        for spread in directions {
            if axis(spread) == axis(face)
                || !self.has_face(state, face)
                || self.has_face(state, spread)
            {
                continue;
            }
            // `SAME_POSITION`, `SAME_PLANE` and `WRAP_AROUND`.
            let beside = spread.step(position);
            let targets = [
                (position, spread),
                (beside, face),
                (face.step(beside), spread.opposite()),
            ];
            let target = targets.into_iter().find(|&(at, toward)| {
                let current = world.block_state(at[0], at[1], at[2]);
                let replaceable = block_states().is_air(current)
                    || self.is_block(current)
                    || (is_water(current) && fluid(current) == Fluid::Water);
                replaceable && self.valid_for_placement(world, current, at, toward)
            });
            let Some((at, toward)) = target else {
                continue;
            };
            let current = world.block_state(at[0], at[1], at[2]);
            if let Some(placed) = self.state_for_placement(world, current, at, toward) {
                world.mark_postprocessing(at[0], at[1], at[2]);
                world.set_block_state(at[0], at[1], at[2], placed);
                return;
            }
        }
    }

    fn shuffled<R: RandomSource>(
        &self,
        random: &mut WorldgenRandom<R>,
        except: Option<Direction>,
    ) -> Vec<Direction> {
        let mut directions: Vec<_> = self
            .valid_directions
            .iter()
            .copied()
            .filter(|&direction| Some(direction) != except)
            .collect();
        shuffle(&mut directions, random);
        directions
    }
}

/// `MultifaceGrowthFeature.place`.
pub fn place_multiface_growth<W: FeatureWorld, R: RandomSource>(
    world: &mut W,
    growth: &CompiledMultifaceGrowth,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    let air_or_water = |state: u32| block_states().is_air(state) || is_water(state);
    let state = world.block_state(origin[0], origin[1], origin[2]);
    if !air_or_water(state) {
        return false;
    }
    let directions = growth.shuffled(random, None);
    if growth.place_growth(world, origin, state, random, &directions) {
        return true;
    }
    for &direction in &directions {
        let others = growth.shuffled(random, Some(direction.opposite()));
        // Vanilla sets the position from the origin each time, so it tries the same neighbor `search_range` times.
        let position = direction.step(origin);
        for _ in 0..growth.search_range {
            let state = world.block_state(position[0], position[1], position[2]);
            if !air_or_water(state) && !growth.is_block(state) {
                break;
            }
            if growth.place_growth(world, position, state, random, &others) {
                return true;
            }
        }
    }
    false
}

impl FeatureCompiler<'_> {
    /// Compiles a `minecraft:multiface_growth` configuration; None for blocks other than glow lichen, whose
    /// spreaders are not ported yet.
    pub(crate) fn multiface_growth(
        &mut self,
        name: &str,
        config: &Value,
    ) -> Result<Option<CompiledMultifaceGrowth>, WorldgenDataError> {
        let invalid = |message: String| WorldgenDataError::InvalidDefinition {
            context: name.to_string(),
            message,
        };
        let field = |key: &str| {
            config
                .get(key)
                .ok_or_else(|| invalid(format!("missing field `{key}`")))
        };
        let flag = |key: &str| -> Result<bool, WorldgenDataError> {
            match config.get(key) {
                Some(value) => value
                    .as_bool()
                    .ok_or_else(|| invalid(format!("`{key}` must be a boolean"))),
                None => Ok(false),
            }
        };
        let block_name = field("block")?
            .as_str()
            .ok_or_else(|| invalid("`block` must be a block".to_string()))?;
        if block_name != "minecraft:glow_lichen" {
            return Ok(None);
        }
        let block = block_states()
            .default_state(block_name)
            .ok_or_else(|| invalid(format!("unknown block {block_name}")))?;
        let mut valid_directions = Vec::new();
        if flag("can_place_on_ceiling")? {
            valid_directions.push(Direction::Up);
        }
        if flag("can_place_on_floor")? {
            valid_directions.push(Direction::Down);
        }
        if flag("can_place_on_wall")? {
            valid_directions.extend(HORIZONTAL);
        }
        let search_range = match config.get("search_range") {
            Some(value) => value
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())
                .ok_or_else(|| invalid("`search_range` must be an integer".to_string()))?,
            None => 10,
        };
        let chance_of_spreading = match config.get("chance_of_spreading") {
            Some(value) => value
                .as_f64()
                .ok_or_else(|| invalid("`chance_of_spreading` must be a number".to_string()))?
                as f32,
            None => 0.5,
        };
        Ok(Some(CompiledMultifaceGrowth {
            block,
            search_range,
            chance_of_spreading,
            can_be_placed_on: self.block_set(name, field("can_be_placed_on")?)?,
            valid_directions,
        }))
    }
}
