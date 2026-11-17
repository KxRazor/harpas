//! Water and lava flowing as vanilla's `FlowingFluid`, `WaterFluid`, `LavaFluid` and `LiquidBlock` make them.
//!
//! A fluid tick runs at a block whose fluid is still of the scheduled type. A flowing fluid first takes the level
//! its neighbors give it (`getNewLiquid`): the most of its own kind beside it, less the drop-off, or a falling
//! fluid under more of it, or a source between two sources over solid ground where the fluid converts. Then it
//! spreads (`spread`): down if it can, and otherwise, or from a source with three sources around it, to the sides
//! nearest a hole it can fall into within the slope distance. A liquid block whose neighbors change, or that is
//! placed, schedules its fluid's tick; lava beside water becomes obsidian from a source or cobblestone otherwise,
//! lava on soul soil beside blue ice becomes basalt, and lava flowing down into water makes stone, each with a
//! fizz.
//!
//! Water has a drop-off of 1, a slope distance of 4 and a tick delay of 5, and converts to sources (the
//! `water_source_conversion` game rule's default); lava in the Overworld has a drop-off of 2, a slope distance of
//! 2 and a tick delay of 30, which flowing lava makes four times longer three times out of four when it rises,
//! and does not convert (`lava_source_conversion`'s default). Not done yet: the blocks water washes away dropping
//! their items, lava setting fire to its surroundings, game rules other than their defaults, and the Nether and
//! the End, where lava flows faster.

use harpas_core::block::block_states;
use harpas_core::block_behavior::{ShapeBox, block_behavior};
use harpas_core::random::RandomSource;
use harpas_worldgen::placement::{Fluid, fluid};
use harpas_worldgen::shape_update::Direction;

use crate::block::{self, BlockPos};
use crate::neighbor::{self, LevelEvent};
use crate::world::World;

/// Vanilla's world event for lava hardening or burning something away, with its sound (`LevelEvent.LAVA_FIZZ`).
pub(crate) const LAVA_FIZZ: i32 = 1501;

/// A fluid type that ticks: the source and flowing forms of water and lava are different types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum FluidType {
    Water,
    FlowingWater,
    Lava,
    FlowingLava,
}

impl FluidType {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Water => "minecraft:water",
            Self::FlowingWater => "minecraft:flowing_water",
            Self::Lava => "minecraft:lava",
            Self::FlowingLava => "minecraft:flowing_lava",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        [
            Self::Water,
            Self::FlowingWater,
            Self::Lava,
            Self::FlowingLava,
        ]
        .into_iter()
        .find(|kind| kind.name() == name)
    }

    fn is_water(self) -> bool {
        matches!(self, Self::Water | Self::FlowingWater)
    }

    /// The fluid's family (`FlowingFluid`), which `isSame` compares.
    fn family(self) -> Family {
        if self.is_water() {
            Family::Water
        } else {
            Family::Lava
        }
    }

    /// The game ticks between a change and the fluid's tick (`getTickDelay`).
    pub(crate) fn tick_delay(self) -> i32 {
        self.family().tick_delay()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Water,
    Lava,
}

impl Family {
    fn source(self) -> FluidType {
        match self {
            Self::Water => FluidType::Water,
            Self::Lava => FluidType::Lava,
        }
    }

    fn flowing(self) -> FluidType {
        match self {
            Self::Water => FluidType::FlowingWater,
            Self::Lava => FluidType::FlowingLava,
        }
    }

    fn tick_delay(self) -> i32 {
        match self {
            Self::Water => 5,
            Self::Lava => 30,
        }
    }

    fn drop_off(self) -> i32 {
        match self {
            Self::Water => 1,
            Self::Lava => 2,
        }
    }

    fn slope_find_distance(self) -> i32 {
        match self {
            Self::Water => 4,
            Self::Lava => 2,
        }
    }

    fn converts_to_source(self) -> bool {
        self == Self::Water
    }
}

/// A block's fluid (`FluidState`): none, or a type with an amount from 1 to 8 and whether it falls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FluidState {
    kind: Option<FluidType>,
    amount: i32,
    falling: bool,
}

impl FluidState {
    const EMPTY: Self = Self {
        kind: None,
        amount: 0,
        falling: false,
    };

    fn source(family: Family) -> Self {
        Self {
            kind: Some(family.source()),
            amount: 8,
            falling: false,
        }
    }

    fn flowing(family: Family, amount: i32, falling: bool) -> Self {
        Self {
            kind: Some(family.flowing()),
            amount,
            falling,
        }
    }

    /// The fluid a block state holds (`BlockState.getFluidState`). A liquid block's level is the source at 0,
    /// flowing fluid of amount `8 - level` up to 7, and falling fluid of amount 8 from 8 on; other blocks hold
    /// a water source when waterlogged, as seagrass and kelp do.
    pub(crate) fn of(state: u32) -> Self {
        let family = match fluid(state) {
            Fluid::Empty => return Self::EMPTY,
            Fluid::Water | Fluid::FlowingWater => Family::Water,
            Fluid::Lava | Fluid::FlowingLava => Family::Lava,
        };
        let Some((name, properties)) = block_states().properties(state) else {
            return Self::EMPTY;
        };
        if name != "minecraft:water" && name != "minecraft:lava" {
            return Self::source(family);
        }
        let level = properties
            .iter()
            .find(|(key, _)| *key == "level")
            .and_then(|(_, value)| value.parse::<i32>().ok())
            .unwrap_or(0);
        match level {
            0 => Self::source(family),
            1..=7 => Self::flowing(family, 8 - level, false),
            _ => Self::flowing(family, 8, true),
        }
    }

    pub(crate) fn kind(self) -> Option<FluidType> {
        self.kind
    }

    fn is_empty(self) -> bool {
        self.kind.is_none()
    }

    fn is_source(self) -> bool {
        matches!(self.kind, Some(FluidType::Water | FluidType::Lava))
    }

    fn family(self) -> Option<Family> {
        self.kind.map(FluidType::family)
    }

    fn is_water(self) -> bool {
        self.family() == Some(Family::Water)
    }

    fn is_lava(self) -> bool {
        self.family() == Some(Family::Lava)
    }

    /// The liquid block that holds this fluid alone (`createLegacyBlock`).
    pub(crate) fn legacy_block(self) -> u32 {
        let states = block_states();
        let Some(family) = self.family() else {
            return states.default_state("minecraft:air").expect("air");
        };
        let level = if self.is_source() {
            0
        } else {
            8 - self.amount.min(8) + if self.falling { 8 } else { 0 }
        };
        let name = match family {
            Family::Water => "minecraft:water",
            Family::Lava => "minecraft:lava",
        };
        let level = level.to_string();
        states
            .state(name, [("level", level.as_str())])
            .expect("a liquid block")
            .0
    }
}

/// Whether a block holds fluids beside its own block (`LiquidBlockContainer`): waterloggable blocks, kelp and
/// seagrass.
fn is_container(state: u32) -> bool {
    let Some((name, properties)) = block_states().properties(state) else {
        return false;
    };
    properties.iter().any(|(key, _)| *key == "waterlogged")
        || matches!(
            name,
            "minecraft:kelp"
                | "minecraft:kelp_plant"
                | "minecraft:seagrass"
                | "minecraft:tall_seagrass"
        )
}

/// `FlowingFluid.canHoldAnyFluid`: a block a fluid may flow into, replacing it unless it holds fluids itself.
fn can_hold_any_fluid(state: u32) -> bool {
    if is_container(state) {
        return true;
    }
    if block_behavior().blocks_motion(state) {
        return false;
    }
    let Some(name) = block_states().name(state) else {
        return false;
    };
    let door = name.ends_with("_door");
    let sign = name.ends_with("_sign") && !name.ends_with("_hanging_sign");
    !door
        && !sign
        && !matches!(
            name,
            "minecraft:ladder"
                | "minecraft:sugar_cane"
                | "minecraft:bubble_column"
                | "minecraft:nether_portal"
                | "minecraft:end_portal"
                | "minecraft:end_gateway"
                | "minecraft:structure_void"
        )
}

/// `FlowingFluid.canHoldSpecificFluid`: a block that holds fluids takes only a water source when it is not
/// waterlogged yet (`SimpleWaterloggedBlock.canPlaceLiquid`); seagrass and kelp take none.
fn can_hold_specific_fluid(state: u32, kind: Option<FluidType>) -> bool {
    if !is_container(state) {
        return true;
    }
    kind == Some(FluidType::Water)
        && block_states()
            .properties(state)
            .is_some_and(|(_, properties)| properties.contains(&("waterlogged", "false")))
}

fn can_hold_fluid(state: u32, kind: Option<FluidType>) -> bool {
    can_hold_any_fluid(state) && can_hold_specific_fluid(state, kind)
}

fn axis_index(direction: Direction) -> usize {
    match direction {
        Direction::West | Direction::East => 0,
        Direction::Down | Direction::Up => 1,
        Direction::North | Direction::South => 2,
    }
}

fn positive(direction: Direction) -> bool {
    matches!(
        direction,
        Direction::Up | Direction::South | Direction::East
    )
}

/// The full collision cube (`Shapes.block()`).
fn is_full_block(shape: &[ShapeBox]) -> bool {
    matches!(shape, [b] if b.min == [0, 0, 0] && b.max == [32, 32, 32])
}

/// Marks in `cells` the face of `shape` on the side of `axis` at `at`, in 1/32 of a block: the boxes of the
/// shape's last layer towards that side, when the shape reaches it.
fn mark_face(shape: &[ShapeBox], axis: usize, top: bool, cells: &mut [[bool; 32]; 32]) {
    let reach = if top {
        shape.iter().map(|b| b.max[axis]).max()
    } else {
        shape.iter().map(|b| b.min[axis]).min()
    };
    if reach != Some(if top { 32 } else { 0 }) {
        return;
    }
    let [u, v] = match axis {
        0 => [1, 2],
        1 => [0, 2],
        _ => [0, 1],
    };
    for b in shape {
        let on_face = if top {
            b.max[axis] == 32
        } else {
            b.min[axis] == 0
        };
        if !on_face {
            continue;
        }
        let clamp = |n: i8| n.clamp(0, 32) as usize;
        for row in &mut cells[clamp(b.min[u])..clamp(b.max[u])] {
            for cell in &mut row[clamp(b.min[v])..clamp(b.max[v])] {
                *cell = true;
            }
        }
    }
}

/// `FlowingFluid.canPassThroughWall`: fluid passes between two blocks unless either is a full cube or their
/// collision shapes together cover the face between them (`Shapes.mergedFaceOccludes`).
fn can_pass_through_wall(direction: Direction, state: u32, neighbor: u32) -> bool {
    let behavior = block_behavior();
    let from = behavior.collision_shape(state);
    let to = behavior.collision_shape(neighbor);
    if is_full_block(to) || is_full_block(from) {
        return false;
    }
    if from.is_empty() && to.is_empty() {
        return true;
    }
    let axis = axis_index(direction);
    let (low, high) = if positive(direction) {
        (from, to)
    } else {
        (to, from)
    };
    let mut cells = [[false; 32]; 32];
    mark_face(low, axis, true, &mut cells);
    mark_face(high, axis, false, &mut cells);
    !cells.iter().all(|row| row.iter().all(|cell| *cell))
}

/// The state with one property set to another value.
fn with_property(state: u32, key: &str, value: &str) -> u32 {
    let states = block_states();
    let Some((name, properties)) = states.properties(state) else {
        return state;
    };
    let properties = properties
        .into_iter()
        .map(|(k, v)| if k == key { (k, value) } else { (k, v) });
    states
        .state(name, properties)
        .map_or(state, |(state, _)| state)
}

fn step(pos: BlockPos, direction: Direction) -> BlockPos {
    let [x, y, z] = direction.step([pos.x, pos.y, pos.z]);
    BlockPos { x, y, z }
}

/// `Direction.Plane.HORIZONTAL`.
const HORIZONTAL: [Direction; 4] = [
    Direction::North,
    Direction::East,
    Direction::South,
    Direction::West,
];

/// `LiquidBlock.POSSIBLE_FLOW_DIRECTIONS`.
const POSSIBLE_FLOW_DIRECTIONS: [Direction; 5] = [
    Direction::Down,
    Direction::South,
    Direction::North,
    Direction::East,
    Direction::West,
];

impl World {
    fn state_at(&self, pos: BlockPos) -> u32 {
        self.block_state(pos).unwrap_or_else(block::void_air)
    }

    fn fluid_at(&self, pos: BlockPos) -> FluidState {
        FluidState::of(self.state_at(pos))
    }

    /// `FluidState.getHeight`: a full block under more of the same fluid, and its amount out of 9 otherwise.
    fn fluid_height(&self, pos: BlockPos, fluid: FluidState) -> f32 {
        if fluid.is_empty() {
            return 0.0;
        }
        let above = self.fluid_at(step(pos, Direction::Up));
        if above.family() == fluid.family() {
            1.0
        } else {
            fluid.amount as f32 / 9.0
        }
    }

    /// Schedules a fluid tick `delay` game ticks from now (`Level.scheduleTick`).
    pub(crate) fn schedule_fluid_tick(&mut self, pos: BlockPos, kind: FluidType, delay: i32) {
        if self.fluid_ticks.schedule(kind, pos, self.game_time, delay) {
            self.mark_changed(pos.chunk());
        }
    }

    fn fizz(&mut self, pos: BlockPos) {
        self.level_events.push(LevelEvent {
            event: LAVA_FIZZ,
            pos,
            data: 0,
        });
    }

    /// Runs a fluid tick (`ServerLevel.tickFluid`): only if the block still holds the scheduled type.
    pub(crate) fn tick_fluid(&mut self, pos: BlockPos, kind: FluidType) {
        let state = self.state_at(pos);
        let fluid = FluidState::of(state);
        if fluid.kind() == Some(kind) {
            self.flowing_tick(pos, state, fluid);
        }
    }

    /// `FlowingFluid.tick`.
    fn flowing_tick(&mut self, pos: BlockPos, mut state: u32, mut fluid: FluidState) {
        let Some(family) = fluid.family() else {
            return;
        };
        if !fluid.is_source() {
            let new = self.new_liquid(family, pos, state);
            let delay = self.spread_delay(family, pos, fluid, new);
            if new.is_empty() {
                fluid = new;
                state = block_states().default_state("minecraft:air").expect("air");
                self.set_block(pos, state, neighbor::UPDATE_ALL, neighbor::UPDATE_LIMIT);
            } else if new != fluid {
                fluid = new;
                state = new.legacy_block();
                self.set_block(pos, state, neighbor::UPDATE_ALL, neighbor::UPDATE_LIMIT);
                if let Some(kind) = new.kind() {
                    self.schedule_fluid_tick(pos, kind, delay);
                }
            }
        }
        self.spread(family, pos, state, fluid);
    }

    /// `getSpreadDelay`: lava rising from flowing to flowing waits four times longer three times out of four.
    fn spread_delay(
        &mut self,
        family: Family,
        pos: BlockPos,
        old: FluidState,
        new: FluidState,
    ) -> i32 {
        let delay = family.tick_delay();
        if family == Family::Lava
            && !old.is_empty()
            && !new.is_empty()
            && !old.falling
            && !new.falling
            && self.fluid_height(pos, new) > self.fluid_height(pos, old)
            && self.random.next_int_bounded(4) != 0
        {
            return delay * 4;
        }
        delay
    }

    /// `FlowingFluid.getNewLiquid`.
    fn new_liquid(&self, family: Family, pos: BlockPos, state: u32) -> FluidState {
        let mut max_amount = 0;
        let mut sources = 0;
        for direction in HORIZONTAL {
            let neighbor_pos = step(pos, direction);
            let neighbor = self.state_at(neighbor_pos);
            let neighbor_fluid = FluidState::of(neighbor);
            if neighbor_fluid.family() == Some(family)
                && can_pass_through_wall(direction, state, neighbor)
            {
                if neighbor_fluid.is_source() {
                    sources += 1;
                }
                max_amount = max_amount.max(neighbor_fluid.amount);
            }
        }
        if sources >= 2 && family.converts_to_source() {
            let below = self.state_at(step(pos, Direction::Down));
            let below_fluid = FluidState::of(below);
            if block_behavior().is_solid(below)
                || (below_fluid.family() == Some(family) && below_fluid.is_source())
            {
                return FluidState::source(family);
            }
        }
        let above_pos = step(pos, Direction::Up);
        let above = self.state_at(above_pos);
        let above_fluid = FluidState::of(above);
        if above_fluid.family() == Some(family)
            && can_pass_through_wall(Direction::Up, state, above)
        {
            return FluidState::flowing(family, 8, true);
        }
        let amount = max_amount - family.drop_off();
        if amount <= 0 {
            FluidState::EMPTY
        } else {
            FluidState::flowing(family, amount, false)
        }
    }

    /// `FlowingFluid.spread`.
    fn spread(&mut self, family: Family, pos: BlockPos, state: u32, fluid: FluidState) {
        if fluid.is_empty() {
            return;
        }
        let below_pos = step(pos, Direction::Down);
        let below = self.state_at(below_pos);
        let below_fluid = FluidState::of(below);
        if self.can_maybe_pass_through(family, Direction::Down, state, below, below_fluid) {
            let new = self.new_liquid(family, below_pos, below);
            if self.can_be_replaced_with(below_pos, below_fluid, new.kind(), Direction::Down)
                && can_hold_specific_fluid(below, new.kind())
            {
                self.spread_to(family, below_pos, below, Direction::Down, new);
                if self.source_neighbor_count(family, pos) >= 3 {
                    self.spread_to_sides(family, pos, fluid, state);
                }
                return;
            }
        }
        if fluid.is_source() || !self.is_water_hole(family, state, below) {
            self.spread_to_sides(family, pos, fluid, state);
        }
    }

    /// `Fluid.canBeReplacedWith`, asked of the fluid already in a block: water gives way to anything but water
    /// from above, lava at least four ninths high to water, and nothing to anything.
    fn can_be_replaced_with(
        &self,
        pos: BlockPos,
        existing: FluidState,
        kind: Option<FluidType>,
        direction: Direction,
    ) -> bool {
        let water = kind.is_some_and(FluidType::is_water);
        match existing.family() {
            None => true,
            Some(Family::Water) => direction == Direction::Down && !water,
            Some(Family::Lava) => self.fluid_height(pos, existing) >= 0.444_444_45 && water,
        }
    }

    fn is_source_of(family: Family, fluid: FluidState) -> bool {
        fluid.family() == Some(family) && fluid.is_source()
    }

    /// `FlowingFluid.canMaybePassThrough`.
    fn can_maybe_pass_through(
        &self,
        family: Family,
        direction: Direction,
        state: u32,
        neighbor: u32,
        neighbor_fluid: FluidState,
    ) -> bool {
        !Self::is_source_of(family, neighbor_fluid)
            && can_hold_any_fluid(neighbor)
            && can_pass_through_wall(direction, state, neighbor)
    }

    /// `FlowingFluid.isWaterHole`: a block the fluid can fall out of.
    fn is_water_hole(&self, family: Family, state: u32, below: u32) -> bool {
        if !can_pass_through_wall(Direction::Down, state, below) {
            return false;
        }
        FluidState::of(below).family() == Some(family)
            || can_hold_fluid(below, Some(family.flowing()))
    }

    fn source_neighbor_count(&self, family: Family, pos: BlockPos) -> usize {
        HORIZONTAL
            .iter()
            .filter(|&&direction| Self::is_source_of(family, self.fluid_at(step(pos, direction))))
            .count()
    }

    /// `FlowingFluid.spreadToSides`.
    fn spread_to_sides(&mut self, family: Family, pos: BlockPos, fluid: FluidState, state: u32) {
        let amount = if fluid.falling {
            7
        } else {
            fluid.amount - family.drop_off()
        };
        if amount <= 0 {
            return;
        }
        for (direction, new) in self.spread_directions(family, pos, state) {
            let target = step(pos, direction);
            let target_state = self.state_at(target);
            self.spread_to(family, target, target_state, direction, new);
        }
    }

    /// `FlowingFluid.getSpread`: the sides nearest a hole within the slope distance, in `Direction` order, each
    /// with the fluid it would take.
    fn spread_directions(
        &self,
        family: Family,
        pos: BlockPos,
        state: u32,
    ) -> Vec<(Direction, FluidState)> {
        let mut best = 1000;
        let mut found: Vec<(Direction, FluidState)> = Vec::new();
        for direction in HORIZONTAL {
            let neighbor_pos = step(pos, direction);
            let neighbor = self.state_at(neighbor_pos);
            let neighbor_fluid = FluidState::of(neighbor);
            if !self.can_maybe_pass_through(family, direction, state, neighbor, neighbor_fluid) {
                continue;
            }
            let new = self.new_liquid(family, neighbor_pos, neighbor);
            if !can_hold_specific_fluid(neighbor, new.kind()) {
                continue;
            }
            let distance = if self.is_hole(family, neighbor_pos) {
                0
            } else {
                self.slope_distance(family, neighbor_pos, 1, direction.opposite(), neighbor)
            };
            if distance < best {
                found.clear();
            }
            if distance <= best {
                if self.can_be_replaced_with(neighbor_pos, neighbor_fluid, new.kind(), direction) {
                    found.retain(|(d, _)| *d != direction);
                    found.push((direction, new));
                }
                best = distance;
            }
        }
        // An `EnumMap` iterates in `Direction` order: north, south, west, east.
        found.sort_by_key(|(direction, _)| Direction::ALL.iter().position(|d| d == direction));
        found
    }

    /// `SpreadContext.isHole`.
    fn is_hole(&self, family: Family, pos: BlockPos) -> bool {
        self.is_water_hole(
            family,
            self.state_at(pos),
            self.state_at(step(pos, Direction::Down)),
        )
    }

    /// `FlowingFluid.getSlopeDistance`.
    fn slope_distance(
        &self,
        family: Family,
        pos: BlockPos,
        distance: i32,
        excluded: Direction,
        state: u32,
    ) -> i32 {
        let mut best = 1000;
        for direction in HORIZONTAL {
            if direction == excluded {
                continue;
            }
            let neighbor_pos = step(pos, direction);
            let neighbor = self.state_at(neighbor_pos);
            let neighbor_fluid = FluidState::of(neighbor);
            if self.can_maybe_pass_through(family, direction, state, neighbor, neighbor_fluid)
                && can_hold_specific_fluid(neighbor, Some(family.flowing()))
            {
                if self.is_hole(family, neighbor_pos) {
                    return distance;
                }
                if distance < family.slope_find_distance() {
                    let found = self.slope_distance(
                        family,
                        neighbor_pos,
                        distance + 1,
                        direction.opposite(),
                        neighbor,
                    );
                    best = best.min(found);
                }
            }
        }
        best
    }

    /// `FlowingFluid.spreadTo`, with `LavaFluid`'s stone when lava flows down into water.
    fn spread_to(
        &mut self,
        family: Family,
        pos: BlockPos,
        state: u32,
        direction: Direction,
        new: FluidState,
    ) {
        if family == Family::Lava
            && direction == Direction::Down
            && FluidState::of(state).is_water()
        {
            if matches!(
                block_states().name(state),
                Some("minecraft:water" | "minecraft:lava")
            ) {
                let stone = block_states()
                    .default_state("minecraft:stone")
                    .expect("stone");
                self.set_block(pos, stone, neighbor::UPDATE_ALL, neighbor::UPDATE_LIMIT);
            }
            self.fizz(pos);
            return;
        }
        if is_container(state) {
            // `SimpleWaterloggedBlock.placeLiquid`: only a water source waterlogs it.
            if new.kind() == Some(FluidType::Water) && can_hold_specific_fluid(state, new.kind()) {
                let waterlogged = with_property(state, "waterlogged", "true");
                self.set_block(
                    pos,
                    waterlogged,
                    neighbor::UPDATE_ALL,
                    neighbor::UPDATE_LIMIT,
                );
                self.schedule_fluid_tick(pos, FluidType::Water, Family::Water.tick_delay());
            }
            return;
        }
        if !block_states().is_air(state) && family == Family::Lava {
            // `LavaFluid.beforeDestroyingBlock`; water's would drop the block's items.
            self.fizz(pos);
        }
        self.set_block(
            pos,
            new.legacy_block(),
            neighbor::UPDATE_ALL,
            neighbor::UPDATE_LIMIT,
        );
    }

    /// `LiquidBlock.shouldSpreadLiquid`: lava beside water hardens instead of flowing.
    fn should_spread_liquid(&mut self, pos: BlockPos, state: u32) -> bool {
        let fluid = FluidState::of(state);
        if !fluid.is_lava() {
            return true;
        }
        let on_soul_soil = block_states().name(self.state_at(step(pos, Direction::Down)))
            == Some("minecraft:soul_soil");
        for direction in POSSIBLE_FLOW_DIRECTIONS {
            let neighbor = step(pos, direction.opposite());
            let hardened = if self.fluid_at(neighbor).is_water() {
                Some(if fluid.is_source() {
                    "minecraft:obsidian"
                } else {
                    "minecraft:cobblestone"
                })
            } else if on_soul_soil
                && block_states().name(self.state_at(neighbor)) == Some("minecraft:blue_ice")
            {
                Some("minecraft:basalt")
            } else {
                None
            };
            if let Some(name) = hardened {
                let block = block_states().default_state(name).expect("a vanilla block");
                self.set_block(pos, block, neighbor::UPDATE_ALL, neighbor::UPDATE_LIMIT);
                self.fizz(pos);
                return false;
            }
        }
        true
    }

    /// A liquid block placed, or whose neighbor changed, schedules its fluid's tick (`LiquidBlock.onPlace` and
    /// `neighborChanged`).
    pub(crate) fn liquid_changed(&mut self, pos: BlockPos, state: u32) {
        if !matches!(
            block_states().name(state),
            Some("minecraft:water" | "minecraft:lava")
        ) {
            return;
        }
        if self.should_spread_liquid(pos, state)
            && let Some(kind) = FluidState::of(state).kind()
        {
            self.schedule_fluid_tick(pos, kind, kind.tick_delay());
        }
    }

    /// The fluid ticks a shape update schedules: a liquid block next to a source, or beside a new one
    /// (`LiquidBlock.updateShape`), and a block that holds water (`SimpleWaterloggedBlock` blocks, kelp and
    /// seagrass).
    pub(crate) fn fluid_shape_update(&mut self, pos: BlockPos, state: u32, neighbor: u32) {
        let fluid = FluidState::of(state);
        let Some(kind) = fluid.kind() else {
            return;
        };
        let liquid = matches!(
            block_states().name(state),
            Some("minecraft:water" | "minecraft:lava")
        );
        if !liquid || fluid.is_source() || FluidState::of(neighbor).is_source() {
            self.schedule_fluid_tick(pos, kind, kind.tick_delay());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(name: &str, properties: &[(&str, &str)]) -> u32 {
        block_states()
            .state(name, properties.iter().copied())
            .unwrap()
            .0
    }

    #[test]
    fn liquid_levels_map_to_fluid_amounts_and_back() {
        let water = |level: &str| state("minecraft:water", &[("level", level)]);
        assert_eq!(
            FluidState::of(water("0")),
            FluidState::source(Family::Water)
        );
        assert_eq!(
            FluidState::of(water("3")),
            FluidState::flowing(Family::Water, 5, false)
        );
        assert_eq!(
            FluidState::of(water("12")),
            FluidState::flowing(Family::Water, 8, true)
        );
        assert_eq!(
            FluidState::flowing(Family::Water, 5, false).legacy_block(),
            water("3")
        );
        assert_eq!(
            FluidState::flowing(Family::Water, 8, true).legacy_block(),
            water("8")
        );
        assert_eq!(
            FluidState::of(state("minecraft:oak_slab", &[("waterlogged", "true")])),
            FluidState::source(Family::Water)
        );
        assert!(FluidState::of(state("minecraft:stone", &[])).is_empty());
    }

    #[test]
    fn fluid_passes_beside_partial_shapes_but_not_through_full_faces() {
        let air = state("minecraft:air", &[]);
        let stone = state("minecraft:stone", &[]);
        let bottom = state("minecraft:oak_slab", &[("type", "bottom")]);
        let top = state("minecraft:oak_slab", &[("type", "top")]);
        assert!(can_pass_through_wall(Direction::East, air, air));
        assert!(!can_pass_through_wall(Direction::East, air, stone));
        // A top slab covers the face between it and the air above it; a bottom slab, only its own bottom face.
        assert!(!can_pass_through_wall(Direction::Down, air, top));
        assert!(can_pass_through_wall(Direction::Down, air, bottom));
        assert!(!can_pass_through_wall(Direction::Up, air, bottom));
        assert!(!can_pass_through_wall(Direction::Down, bottom, stone));
        assert!(can_pass_through_wall(Direction::East, air, bottom));
        assert!(!can_pass_through_wall(Direction::Up, top, air));
        assert!(can_pass_through_wall(Direction::Down, top, air));
        assert!(!can_hold_any_fluid(stone));
        assert!(!can_hold_any_fluid(state("minecraft:oak_door", &[])));
        assert!(can_hold_any_fluid(state("minecraft:short_grass", &[])));
        assert!(can_hold_fluid(bottom, Some(FluidType::Water)));
        assert!(!can_hold_fluid(bottom, Some(FluidType::FlowingWater)));
    }
}
