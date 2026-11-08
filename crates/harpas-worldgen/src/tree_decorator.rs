//! Tree decorators (`TreeDecorator`): `trunk_vine`, `leave_vine`, `cocoa`, `beehive`, `place_on_ground`,
//! `attached_to_logs` and `alter_ground`.
//!
//! A tree runs its decorators in order once its trunk and foliage are in place, and a fallen tree runs them on
//! its stump and on its log. They read lists of the tree's logs, leaves and roots taken in Java's iteration order
//! of their `HashSet`s and sorted by height, which keeps that order between blocks at the same height. A tree's
//! decorations join its decoration set, which `updateLeaves` and `updateShapeAtEdge` read; a fallen tree keeps no
//! such set.

use harpas_core::block::block_states;
use harpas_core::block_behavior::block_behavior;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_core::random::{RandomSource, WorldgenRandom};
use serde_json::Value;

use crate::configured::{FeatureCompiler, StateProvider};
use crate::data::WorldgenDataError;
use crate::feature::FeatureWorld;
use crate::generator::HeightmapType;
use crate::java_hash::BlockPosSet;
use crate::shape_update::{Direction, with_property};
use crate::tree::TreeBlocks;

/// `TreeDecorator`.
#[derive(Debug)]
pub(crate) enum Decorator {
    /// `trunk_vine`: vines on the sides of the trunk.
    TrunkVine,
    /// `leave_vine`: vines hanging from the sides of the leaves, with the probability of each side.
    LeaveVine(f32),
    /// `cocoa`: cocoa beside the lowest three blocks of the trunk, with the probability of any.
    Cocoa(f32),
    /// `beehive`: a bee nest beside the trunk under the leaves, with its probability. The nest's block entity is
    /// saved as `BlockEntity.saveWithFullMetadata` writes it.
    Beehive(f32),
    /// `place_on_ground`: blocks on solid ground around the base of the trunk.
    PlaceOnGround {
        tries: i32,
        radius: i32,
        height: i32,
        provider: Box<StateProvider>,
    },
    /// `alter_ground`: the ground in rounded squares around the base of the trunk, where the provider has a
    /// state, such as podzol around giant spruces.
    AlterGround(Box<StateProvider>),
    /// `attached_to_logs`: with the probability, a block on the air beside each log, on a side drawn from
    /// `directions`, with the logs in a shuffled order.
    AttachedToLogs {
        probability: f32,
        provider: Box<StateProvider>,
        directions: Vec<Direction>,
    },
}

/// `TreeDecorator.Context`: the roots, trunk and foliage of a tree, in Java's iteration order of their sets and
/// then sorted by height.
pub(crate) struct DecoratorContext {
    logs: Vec<[i32; 3]>,
    leaves: Vec<[i32; 3]>,
    roots: Vec<[i32; 3]>,
}

impl DecoratorContext {
    pub(crate) fn new(trunk: &BlockPosSet, foliage: &BlockPosSet, roots: &BlockPosSet) -> Self {
        let sorted = |set: &BlockPosSet| {
            let mut positions: Vec<[i32; 3]> = set.iter().collect();
            positions.sort_by_key(|position| position[1]);
            positions
        };
        Self {
            logs: sorted(trunk),
            leaves: sorted(foliage),
            roots: sorted(roots),
        }
    }

    /// `TreeFeature.getLowestTrunkOrRootOfTree`: the trunk, without roots; with roots, the roots, after the trunk
    /// when the lowest root and the lowest log are at the same height.
    fn lowest_trunk_or_roots(&self) -> Vec<[i32; 3]> {
        if self.roots.is_empty() {
            self.logs.clone()
        } else if self
            .logs
            .first()
            .is_some_and(|log| log[1] == self.roots[0][1])
        {
            self.logs.iter().chain(&self.roots).copied().collect()
        } else {
            self.roots.clone()
        }
    }
}

/// What decorators place with: the world, the feature's random source, the tree blocks and, for a tree, the
/// decoration set its decoration setter adds to.
pub(crate) struct Decorating<'a, W, R: RandomSource> {
    pub(crate) world: &'a mut W,
    pub(crate) random: &'a mut WorldgenRandom<R>,
    pub(crate) blocks: &'a TreeBlocks,
    pub(crate) decorations: Option<&'a mut BlockPosSet>,
}

impl<W: FeatureWorld, R: RandomSource> Decorating<'_, W, R> {
    /// Runs each decorator in order on the same context.
    pub(crate) fn run(&mut self, decorators: &[Decorator], context: &DecoratorContext) {
        for decorator in decorators {
            match decorator {
                Decorator::TrunkVine => self.trunk_vine(context),
                Decorator::LeaveVine(probability) => self.leave_vine(context, *probability),
                Decorator::Cocoa(probability) => self.cocoa(context, *probability),
                Decorator::Beehive(probability) => self.beehive(context, *probability),
                Decorator::PlaceOnGround {
                    tries,
                    radius,
                    height,
                    provider,
                } => self.place_on_ground(context, *tries, *radius, *height, provider),
                Decorator::AttachedToLogs {
                    probability,
                    provider,
                    directions,
                } => self.attached_to_logs(context, *probability, provider, directions),
                Decorator::AlterGround(provider) => self.alter_ground(context, provider),
            }
        }
    }

    fn state(&self, [x, y, z]: [i32; 3]) -> u32 {
        self.world.block_state(x, y, z)
    }

    fn is_air(&self, position: [i32; 3]) -> bool {
        block_states().is_air(self.state(position))
    }

    /// The decoration setter.
    fn set(&mut self, position: [i32; 3], state: u32) -> bool {
        if let Some(decorations) = self.decorations.as_deref_mut() {
            decorations.insert(position);
        }
        self.world
            .set_block_state(position[0], position[1], position[2], state)
    }

    /// `TreeDecorator.Context.placeVine`: a vine attached on one side.
    fn place_vine(&mut self, position: [i32; 3], side: &str) {
        let state = with_property(self.blocks.vine_default, side, "true");
        self.set(position, state);
    }

    /// `TrunkVineDecorator`: beside each log, in two of three cases for each side, a vine on the air there.
    fn trunk_vine(&mut self, context: &DecoratorContext) {
        for &log in &context.logs {
            for (direction, side) in VINE_SIDES {
                if self.random.next_int_bounded(3) > 0 {
                    let position = direction.step(log);
                    if self.is_air(position) {
                        self.place_vine(position, side);
                    }
                }
            }
        }
    }

    /// `LeaveVineDecorator`: beside each leaf, with the probability for each side, a vine on the air there that
    /// hangs down through up to four more blocks of air.
    fn leave_vine(&mut self, context: &DecoratorContext, probability: f32) {
        for &leaf in &context.leaves {
            for (direction, side) in VINE_SIDES {
                if self.random.next_float() < probability {
                    let position = direction.step(leaf);
                    if self.is_air(position) {
                        self.hanging_vine(position, side);
                    }
                }
            }
        }
    }

    /// `LeaveVineDecorator.addHangingVine`.
    fn hanging_vine(&mut self, mut position: [i32; 3], side: &str) {
        self.place_vine(position, side);
        position[1] -= 1;
        for _ in 0..4 {
            if !self.is_air(position) {
                break;
            }
            self.place_vine(position, side);
            position[1] -= 1;
        }
    }

    /// `CocoaDecorator`: with the probability, on the logs up to two blocks above the lowest one, cocoa of a
    /// random age facing the log, on the air beside it, in a quarter of the cases for each side.
    fn cocoa(&mut self, context: &DecoratorContext, probability: f32) {
        if self.random.next_float() >= probability {
            return;
        }
        let Some(lowest) = context.logs.first() else {
            return;
        };
        for &log in context.logs.iter().filter(|log| log[1] - lowest[1] <= 2) {
            for direction in HORIZONTAL {
                if self.random.next_float() > 0.25 {
                    continue;
                }
                let position = direction.opposite().step(log);
                if self.is_air(position) {
                    let age = self.random.next_int_bounded(3).to_string();
                    let state = with_property(self.blocks.cocoa, "age", &age);
                    let state = with_property(state, "facing", direction.name());
                    self.set(position, state);
                }
            }
        }
    }

    /// `BeehiveDecorator`: with the probability, a bee nest facing south with two or three bees, beside a log
    /// below the lowest leaves (or a random log near the bottom of a tree without leaves), on the first
    /// position in a shuffled list that is air with air to its south.
    fn beehive(&mut self, context: &DecoratorContext, probability: f32) {
        let (Some(first), Some(last)) = (context.logs.first(), context.logs.last()) else {
            return;
        };
        if self.random.next_float() >= probability {
            return;
        }
        let y = match context.leaves.first() {
            Some(leaf) => (leaf[1] - 1).max(first[1] + 1),
            None => (first[1] + 1 + self.random.next_int_bounded(3)).min(last[1]),
        };
        let mut positions: Vec<[i32; 3]> = context
            .logs
            .iter()
            .filter(|log| log[1] == y)
            .flat_map(|&log| BEE_NEST_SIDES.map(|direction| direction.step(log)))
            .collect();
        if positions.is_empty() {
            return;
        }
        shuffle(&mut positions, self.random);
        let Some(&nest) = positions.iter().find(|&&position| {
            self.is_air(position) && self.is_air(Direction::South.step(position))
        }) else {
            return;
        };
        let state = with_property(self.blocks.bee_nest, "facing", "south");
        if !self.set(nest, state) {
            return;
        }
        let count = 2 + self.random.next_int_bounded(2);
        let bees: Vec<Compound> = (0..count)
            .map(|_| bee_occupant(self.random.next_int_bounded(599)))
            .collect();
        let mut entity = Compound::new();
        entity.insert("id", "minecraft:beehive");
        entity.insert("x", nest[0]);
        entity.insert("y", nest[1]);
        entity.insert("z", nest[2]);
        entity.insert("bees", Nbt::List(List::Compound(bees)));
        entity.insert("components", Compound::new());
        self.world
            .set_block_entity(nest[0], nest[1], nest[2], entity);
    }

    /// `PlaceOnGroundDecorator`: tries random positions in a box around the lowest layer of the trunk (or the
    /// roots) and places a block above each that is solid ground under air or a vine, at or above the
    /// `MOTION_BLOCKING_NO_LEAVES` surface.
    fn place_on_ground(
        &mut self,
        context: &DecoratorContext,
        tries: i32,
        radius: i32,
        height: i32,
        provider: &StateProvider,
    ) {
        let lowest = context.lowest_trunk_or_roots();
        let Some(&[x, y, z]) = lowest.first() else {
            return;
        };
        let (mut min_x, mut max_x, mut min_z, mut max_z) = (x, x, z, z);
        for position in lowest.iter().filter(|position| position[1] == y) {
            min_x = min_x.min(position[0]);
            max_x = max_x.max(position[0]);
            min_z = min_z.min(position[2]);
            max_z = max_z.max(position[2]);
        }
        for _ in 0..tries {
            let position = [
                self.random
                    .next_int_between_inclusive(min_x - radius, max_x + radius),
                self.random
                    .next_int_between_inclusive(y - height, y + height),
                self.random
                    .next_int_between_inclusive(min_z - radius, max_z + radius),
            ];
            let above = Direction::Up.step(position);
            let state = self.state(above);
            if (block_states().is_air(state) || self.blocks.vine.contains(state))
                && block_behavior().is_solid_render(self.state(position))
                && self.world.heightmap(
                    HeightmapType::MotionBlockingNoLeaves,
                    position[0],
                    position[2],
                ) <= above[1]
            {
                let state = provider.state(self.world, self.random, above);
                self.set(above, state);
            }
        }
    }

    /// `AlterGroundDecorator`: around each block of the lowest layer of the trunk (or the roots), four rounded
    /// squares at its corners, then five more at random positions on the edge of an eight-by-eight square.
    fn alter_ground(&mut self, context: &DecoratorContext, provider: &StateProvider) {
        let lowest = context.lowest_trunk_or_roots();
        let Some(&[_, y, _]) = lowest.first() else {
            return;
        };
        for &[x, _, z] in lowest.iter().filter(|position| position[1] == y) {
            for [dx, dz] in [[-1, -1], [2, -1], [-1, 2], [2, 2]] {
                self.ground_circle(provider, [x + dx, y, z + dz]);
            }
            for _ in 0..5 {
                let cell = self.random.next_int_bounded(64);
                let (cx, cz) = (cell % 8, cell / 8);
                if cx == 0 || cx == 7 || cz == 0 || cz == 7 {
                    self.ground_circle(provider, [x - 3 + cx, y, z - 3 + cz]);
                }
            }
        }
    }

    /// `AlterGroundDecorator.placeCircle`: a five-by-five square without its corners.
    fn ground_circle(&mut self, provider: &StateProvider, [x, y, z]: [i32; 3]) {
        for dx in -2_i32..=2 {
            for dz in -2_i32..=2 {
                if dx.abs() != 2 || dz.abs() != 2 {
                    self.ground_block(provider, [x + dx, y, z + dz]);
                }
            }
        }
    }

    /// `AlterGroundDecorator.placeBlockAt`: from two blocks above down to three below, the first position where
    /// the provider has a state takes it; below the starting height, a block that is not air stops the search.
    fn ground_block(&mut self, provider: &StateProvider, [x, y, z]: [i32; 3]) {
        for dy in (-3..=2).rev() {
            let position = [x, y + dy, z];
            if let Some(state) = provider.optional_state(self.world, self.random, position) {
                self.set(position, state);
                return;
            }
            if !self.is_air(position) && dy < 0 {
                return;
            }
        }
    }

    /// `AttachedToLogsDecorator`.
    fn attached_to_logs(
        &mut self,
        context: &DecoratorContext,
        probability: f32,
        provider: &StateProvider,
        directions: &[Direction],
    ) {
        let mut logs = context.logs.clone();
        shuffle(&mut logs, self.random);
        for log in logs {
            let direction =
                directions[self.random.next_int_bounded(directions.len() as i32) as usize];
            let position = direction.step(log);
            if self.random.next_float() <= probability && self.is_air(position) {
                let state = provider.state(self.world, self.random, position);
                self.set(position, state);
            }
        }
    }
}

/// The sides of a block a vine decorator tries, in vanilla's order, with the side of the vine that faces the
/// block.
const VINE_SIDES: [(Direction, &str); 4] = [
    (Direction::West, "east"),
    (Direction::East, "west"),
    (Direction::North, "south"),
    (Direction::South, "north"),
];

/// `Direction.Plane.HORIZONTAL`.
pub(crate) const HORIZONTAL: [Direction; 4] = [
    Direction::North,
    Direction::East,
    Direction::South,
    Direction::West,
];

/// `BeehiveDecorator.SPAWN_DIRECTIONS`: the horizontal directions except the one opposite the nest's facing.
const BEE_NEST_SIDES: [Direction; 3] = [Direction::East, Direction::South, Direction::West];

/// `BeehiveBlockEntity.Occupant.create`: a bee that has spent `ticks` in the hive, saved as
/// `BeehiveBlockEntity` saves its bees.
fn bee_occupant(ticks: i32) -> Compound {
    let mut entity_data = Compound::new();
    entity_data.insert("id", "minecraft:bee");
    let mut bee = Compound::new();
    bee.insert("entity_data", entity_data);
    bee.insert("ticks_in_hive", ticks);
    bee.insert("min_ticks_in_hive", 600);
    bee
}

/// `Util.shuffle`.
fn shuffle<T>(values: &mut [T], random: &mut impl RandomSource) {
    for size in (2..=values.len()).rev() {
        let other = random.next_int_bounded(size as i32) as usize;
        values.swap(size - 1, other);
    }
}

impl FeatureCompiler<'_> {
    /// Compiles a list of tree decorators; None when one of them is not ported yet.
    pub(crate) fn decorators(
        &mut self,
        name: &str,
        json: Option<&Value>,
    ) -> Result<Option<Vec<Decorator>>, WorldgenDataError> {
        let mut decorators = Vec::new();
        for json in json.and_then(Value::as_array).into_iter().flatten() {
            let Some(decorator) = self.decorator(name, json)? else {
                return Ok(None);
            };
            decorators.push(decorator);
        }
        Ok(Some(decorators))
    }

    /// Compiles a tree decorator; None for a type not ported yet.
    fn decorator(
        &mut self,
        name: &str,
        json: &Value,
    ) -> Result<Option<Decorator>, WorldgenDataError> {
        let probability = || {
            json.get("probability")
                .and_then(Value::as_f64)
                .map(|probability| probability as f32)
                .ok_or_else(|| invalid(name, "a decorator needs a `probability`"))
        };
        let integer = |key: &str, default: i32| match json.get(key) {
            Some(value) => value
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())
                .ok_or_else(|| invalid(name, format!("`{key}` must be an integer"))),
            None => Ok(default),
        };
        let kind = json
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid(name, "a decorator needs a `type`"))?;
        Ok(Some(match kind {
            "minecraft:trunk_vine" => Decorator::TrunkVine,
            "minecraft:leave_vine" => Decorator::LeaveVine(probability()?),
            "minecraft:cocoa" => Decorator::Cocoa(probability()?),
            "minecraft:beehive" => Decorator::Beehive(probability()?),
            "minecraft:place_on_ground" => {
                let Some(provider) = self.decorator_provider(name, json, "block_state_provider")?
                else {
                    return Ok(None);
                };
                Decorator::PlaceOnGround {
                    tries: integer("tries", 128)?,
                    radius: integer("radius", 2)?,
                    height: integer("height", 1)?,
                    provider: Box::new(provider),
                }
            }
            "minecraft:alter_ground" => {
                let Some(provider) = self.decorator_provider(name, json, "provider")? else {
                    return Ok(None);
                };
                Decorator::AlterGround(Box::new(provider))
            }
            "minecraft:attached_to_logs" => {
                let Some(provider) = self.decorator_provider(name, json, "block_provider")? else {
                    return Ok(None);
                };
                let directions = json
                    .get("directions")
                    .and_then(Value::as_array)
                    .filter(|directions| !directions.is_empty())
                    .ok_or_else(|| invalid(name, "`attached_to_logs` needs `directions`"))?
                    .iter()
                    .map(|direction| {
                        direction
                            .as_str()
                            .and_then(Direction::from_name)
                            .ok_or_else(|| invalid(name, "unknown direction"))
                    })
                    .collect::<Result<_, _>>()?;
                Decorator::AttachedToLogs {
                    probability: probability()?,
                    provider: Box::new(provider),
                    directions,
                }
            }
            _ => return Ok(None),
        }))
    }

    fn decorator_provider(
        &mut self,
        name: &str,
        json: &Value,
        key: &str,
    ) -> Result<Option<StateProvider>, WorldgenDataError> {
        let field = json
            .get(key)
            .ok_or_else(|| invalid(name, format!("missing field `{key}`")))?;
        self.provider(name, field)
    }
}

fn invalid(context: &str, message: impl Into<String>) -> WorldgenDataError {
    WorldgenDataError::InvalidDefinition {
        context: context.to_string(),
        message: message.into(),
    }
}
