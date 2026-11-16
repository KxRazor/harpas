//! Sculk patches (`SculkPatchFeature`), spread by vanilla's world generation `SculkSpreader`.
//!
//! A patch starts where sculk is, or in air or a water source beside a full block. Each round adds charge cursors at
//! the origin and updates them a number of times. A cursor on sculk or a sculk vein first spreads veins from its block,
//! when the round converts blocks, then uses charge: a vein turns a replaceable block beside one of its faces into
//! sculk, sculk sometimes grows a sensor or shrieker above it, and both lose charge as they decay. A cursor that still
//! has charge moves to a random sculk or vein block among its 18 face and edge neighbors, preferring one whose faces
//! touch replaceable blocks, and stops beyond 15 blocks from the origin. After the rounds, the patch may put a catalyst
//! at its origin, over a full block, and shriekers around it.
//!
//! The constants are vanilla's world generation spreader's: growth costs 50 charge, no growth within 1 block of the
//! origin, a charge decay rate of 5 and an additional decay rate of 10, replacing `#sculk_replaceable_world_gen`.

use harpas_core::block::block_states;
use harpas_core::block_behavior::{Face, block_behavior};
use harpas_core::random::{RandomSource, WorldgenRandom};
use serde_json::Value;

use crate::data::{IntProviderDef, WorldgenData, WorldgenDataError, parse_int_provider};
use crate::feature::FeatureWorld;
use crate::multiface::{DEFAULT_SPREAD_ORDER, SpreadType, Spreader, has_face, shuffle};
use crate::placement::{Fluid, fluid, sample_int_provider};
use crate::shape_update::{Direction, can_attach_to, same_block, with_property};

/// `SculkSpreader.createWorldGenSpreader`.
const GROWTH_SPAWN_COST: i32 = 50;
const NO_GROWTH_RADIUS: i32 = 1;
const CHARGE_DECAY_RATE: i32 = 5;
const ADDITIONAL_DECAY_RATE: i32 = 10;
/// `SculkSpreader.MAX_CURSORS`, `MAX_CHARGE` and `MAX_CURSOR_DISTANCE`.
const MAX_CURSORS: usize = 32;
const MAX_CHARGE: i32 = 1000;
const MAX_CURSOR_DISTANCE: i32 = 1024;
/// `SculkSpreader.MAX_GROWTH_RATE_RADIUS`.
const MAX_GROWTH_RATE_RADIUS: i32 = 24;

/// A compiled `minecraft:sculk_patch` configuration.
#[derive(Debug)]
pub struct SculkPatch {
    charge_count: i32,
    amount_per_charge: i32,
    spread_attempts: i32,
    growth_rounds: i32,
    spread_rounds: i32,
    extra_rare_growths: IntProviderDef,
    catalyst_chance: f32,
    blocks: SculkBlocks,
}

/// The blocks and tags sculk spreading reads.
#[derive(Debug)]
struct SculkBlocks {
    sculk: u32,
    vein: u32,
    catalyst: u32,
    shrieker: u32,
    sensor: u32,
    water: u32,
    /// `#sculk_replaceable_world_gen`, which veins turn into sculk.
    replaceable_world_gen: Vec<bool>,
    /// `#sculk_replaceable`, which a vein's faces must touch for cursors to prefer it.
    replaceable: Vec<bool>,
    /// `SculkVeinBlock.getSpreader` and `getSameSpaceSpreader`.
    vein_spreader: Spreader,
    same_space_spreader: Spreader,
}

impl SculkPatch {
    /// Compiles a sculk patch configuration; None when its growth count provider is not ported.
    pub(crate) fn compile(
        name: &str,
        config: &Value,
        data: &WorldgenData,
    ) -> Result<Option<Self>, WorldgenDataError> {
        let invalid = |message: String| WorldgenDataError::InvalidDefinition {
            context: name.to_string(),
            message,
        };
        let int = |key: &str| -> Result<i32, WorldgenDataError> {
            config
                .get(key)
                .and_then(Value::as_i64)
                .and_then(|value| i32::try_from(value).ok())
                .ok_or_else(|| invalid(format!("`{key}` must be an integer")))
        };
        let extra_rare_growths = parse_int_provider(
            name,
            config
                .get("extra_rare_growths")
                .ok_or_else(|| invalid("missing field `extra_rare_growths`".to_string()))?,
        )?;
        if matches!(extra_rare_growths, IntProviderDef::Other(_)) {
            return Ok(None);
        }
        let catalyst_chance = config
            .get("catalyst_chance")
            .and_then(Value::as_f64)
            .ok_or_else(|| invalid("`catalyst_chance` must be a number".to_string()))?
            as f32;
        let state = |name: &str| block_states().default_state(name).expect("a vanilla block");
        Ok(Some(Self {
            charge_count: int("charge_count")?,
            amount_per_charge: int("amount_per_charge")?,
            spread_attempts: int("spread_attempts")?,
            growth_rounds: int("growth_rounds")?,
            spread_rounds: int("spread_rounds")?,
            extra_rare_growths,
            catalyst_chance,
            blocks: SculkBlocks {
                sculk: state("minecraft:sculk"),
                vein: state("minecraft:sculk_vein"),
                catalyst: state("minecraft:sculk_catalyst"),
                shrieker: state("minecraft:sculk_shrieker"),
                sensor: state("minecraft:sculk_sensor"),
                water: state("minecraft:water"),
                replaceable_world_gen: data
                    .block_tag_states("minecraft:sculk_replaceable_world_gen")?,
                replaceable: data.block_tag_states("minecraft:sculk_replaceable")?,
                vein_spreader: Spreader::sculk_vein(data, &DEFAULT_SPREAD_ORDER)?,
                same_space_spreader: Spreader::sculk_vein(data, &[SpreadType::SamePosition])?,
            },
        }))
    }
}

/// What a block does with a charge cursor (`SculkBehaviour`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Behaviour {
    /// `SculkBlock`.
    Sculk,
    /// `SculkVeinBlock`.
    Vein,
    /// `SculkBehaviour.DEFAULT`, for every other block.
    Default,
}

/// A charge cursor (`SculkSpreader.ChargeCursor`).
#[derive(Debug, Clone)]
struct Cursor {
    pos: [i32; 3],
    charge: i32,
    update_delay: i32,
    decay_delay: i32,
    /// The faces of the block the cursor moved onto, once it has moved onto sculk or a vein.
    facings: Option<Vec<Direction>>,
}

/// `ChargeCursor.NON_CORNER_NEIGHBOURS`: the 18 offsets around a block with a zero coordinate, in
/// `BlockPos.betweenClosed` order.
fn non_corner_neighbours() -> Vec<[i32; 3]> {
    let mut offsets = Vec::with_capacity(18);
    for z in -1..=1 {
        for y in -1..=1 {
            for x in -1..=1 {
                if (x == 0 || y == 0 || z == 0) && [x, y, z] != [0, 0, 0] {
                    offsets.push([x, y, z]);
                }
            }
        }
    }
    offsets
}

fn state_at(world: &impl FeatureWorld, [x, y, z]: [i32; 3]) -> u32 {
    world.block_state(x, y, z)
}

fn set(world: &mut impl FeatureWorld, [x, y, z]: [i32; 3], state: u32) {
    world.set_block_state(x, y, z, state);
}

fn add(position: [i32; 3], [dx, dy, dz]: [i32; 3]) -> [i32; 3] {
    [position[0] + dx, position[1] + dy, position[2] + dz]
}

/// `Vec3i.distSqr`.
fn dist_sqr(a: [i32; 3], b: [i32; 3]) -> f64 {
    (0..3).map(|i| f64::from(a[i] - b[i]).powi(2)).sum()
}

/// `Vec3i.closerThan`.
fn closer_than(a: [i32; 3], b: [i32; 3], distance: f64) -> bool {
    dist_sqr(a, b) < distance * distance
}

/// `BlockState.isCollisionShapeFullBlock`.
fn collision_full_block(state: u32) -> bool {
    block_behavior()
        .collision_shape(state)
        .iter()
        .any(|shape| shape.min == [0; 3] && shape.max == [32; 3])
}

fn sturdy(state: u32, direction: Direction) -> bool {
    let face = match direction {
        Direction::Down => Face::Down,
        Direction::Up => Face::Up,
        Direction::North => Face::North,
        Direction::South => Face::South,
        Direction::West => Face::West,
        Direction::East => Face::East,
    };
    block_behavior().is_face_sturdy(state, face)
}

/// Whether a state holds source water (`FluidState.is(Fluids.WATER)`).
fn source_water(state: u32) -> bool {
    fluid(state) == Fluid::Water
}

impl SculkBlocks {
    fn behaviour(&self, state: u32) -> Behaviour {
        if same_block(state, self.sculk) {
            Behaviour::Sculk
        } else if same_block(state, self.vein) {
            Behaviour::Vein
        } else {
            Behaviour::Default
        }
    }

    fn is_sculk_behaviour(&self, state: u32) -> bool {
        self.behaviour(state) != Behaviour::Default
    }

    /// `SculkVeinBlock.hasSubstrateAccess`: a vein with a face against a sculk replaceable block.
    fn has_substrate_access(&self, world: &impl FeatureWorld, state: u32, pos: [i32; 3]) -> bool {
        same_block(state, self.vein)
            && Direction::ALL.into_iter().any(|direction| {
                has_face(state, direction)
                    && self.replaceable[state_at(world, direction.step(pos)) as usize]
            })
    }

    /// `SculkBehaviour.attemptSpreadVein`.
    fn attempt_spread_vein(
        &self,
        world: &mut impl FeatureWorld,
        behaviour: Behaviour,
        pos: [i32; 3],
        state: u32,
        facings: Option<&[Direction]>,
    ) -> bool {
        if behaviour == Behaviour::Default {
            match facings {
                None => {
                    let current = state_at(world, pos);
                    return self
                        .same_space_spreader
                        .spread_all(world, current, pos, true)
                        > 0;
                }
                Some(facings) if !facings.is_empty() => {
                    return (block_states().is_air(state) || source_water(state))
                        && self.regrow(world, pos, state, facings);
                }
                Some(_) => {}
            }
        }
        self.vein_spreader.spread_all(world, state, pos, true) > 0
    }

    /// `SculkVeinBlock.regrow`: a vein with the given faces that can still attach.
    fn regrow(
        &self,
        world: &mut impl FeatureWorld,
        pos: [i32; 3],
        state: u32,
        facings: &[Direction],
    ) -> bool {
        let mut vein = self.vein;
        let mut placed = false;
        for &direction in facings {
            if can_attach_to(world, direction.step(pos), direction) {
                vein = with_property(vein, direction.name(), "true");
                placed = true;
            }
        }
        if !placed {
            return false;
        }
        if fluid(state) != Fluid::Empty {
            vein = with_property(vein, "waterlogged", "true");
        }
        set(world, pos, vein);
        true
    }

    /// `SculkBehaviour.attemptUseCharge`: the cursor's charge after this update.
    fn attempt_use_charge<R: RandomSource>(
        &self,
        behaviour: Behaviour,
        cursor: &Cursor,
        world: &mut impl FeatureWorld,
        origin: [i32; 3],
        random: &mut WorldgenRandom<R>,
        convert: bool,
    ) -> i32 {
        let charge = cursor.charge;
        match behaviour {
            Behaviour::Default => {
                if cursor.decay_delay > 0 {
                    charge
                } else {
                    0
                }
            }
            Behaviour::Vein => {
                if convert && self.attempt_place_sculk(world, cursor.pos, random) {
                    return charge - 1;
                }
                if random.next_int_bounded(CHARGE_DECAY_RATE) == 0 {
                    (charge as f32 * 0.5).floor() as i32
                } else {
                    charge
                }
            }
            Behaviour::Sculk => {
                if charge == 0 || random.next_int_bounded(CHARGE_DECAY_RATE) != 0 {
                    return charge;
                }
                let pos = cursor.pos;
                let near = closer_than(pos, origin, f64::from(NO_GROWTH_RADIUS));
                if near || !self.can_place_growth(world, pos) {
                    if random.next_int_bounded(ADDITIONAL_DECAY_RATE) != 0 {
                        return charge;
                    }
                    return charge
                        - if near {
                            1
                        } else {
                            decay_penalty(pos, origin, charge)
                        };
                }
                if random.next_int_bounded(GROWTH_SPAWN_COST) < charge {
                    let above = Direction::Up.step(pos);
                    let growth = self.random_growth_state(world, above, random);
                    set(world, above, growth);
                }
                (charge - GROWTH_SPAWN_COST).max(0)
            }
        }
    }

    /// `SculkBlock.getRandomGrowthState` for world generation: a shrieker that can summon one time in 11,
    /// otherwise a sensor, waterlogged where there is any fluid.
    fn random_growth_state<R: RandomSource>(
        &self,
        world: &impl FeatureWorld,
        pos: [i32; 3],
        random: &mut WorldgenRandom<R>,
    ) -> u32 {
        let state = if random.next_int_bounded(11) == 0 {
            with_property(self.shrieker, "can_summon", "true")
        } else {
            self.sensor
        };
        if fluid(state_at(world, pos)) != Fluid::Empty {
            with_property(state, "waterlogged", "true")
        } else {
            state
        }
    }

    /// `SculkBlock.canPlaceGrowth`: air or water above, and at most two sensors or shriekers nearby.
    fn can_place_growth(&self, world: &impl FeatureWorld, pos: [i32; 3]) -> bool {
        let above = state_at(world, Direction::Up.step(pos));
        if !block_states().is_air(above) && !(same_block(above, self.water) && source_water(above))
        {
            return false;
        }
        let mut count = 0;
        for z in -4..=4 {
            for y in 0..=2 {
                for x in -4..=4 {
                    let state = state_at(world, add(pos, [x, y, z]));
                    if same_block(state, self.sensor) || same_block(state, self.shrieker) {
                        count += 1;
                    }
                    if count > 2 {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// `SculkVeinBlock.attemptPlaceSculk`: turns a replaceable block behind one of the vein's faces, shuffled, into
    /// sculk, spreads veins from it and discharges the veins around it.
    fn attempt_place_sculk<R: RandomSource>(
        &self,
        world: &mut impl FeatureWorld,
        pos: [i32; 3],
        random: &mut WorldgenRandom<R>,
    ) -> bool {
        let state = state_at(world, pos);
        let mut directions = Direction::ALL;
        shuffle(&mut directions, random);
        for direction in directions {
            if !has_face(state, direction) {
                continue;
            }
            let at = direction.step(pos);
            if !self.replaceable_world_gen[state_at(world, at) as usize] {
                continue;
            }
            set(world, at, self.sculk);
            self.vein_spreader.spread_all(world, self.sculk, at, true);
            let opposite = direction.opposite();
            for around in Direction::ALL {
                if around == opposite {
                    continue;
                }
                let neighbor = around.step(at);
                let neighbor_state = state_at(world, neighbor);
                if same_block(neighbor_state, self.vein) {
                    self.vein_discharged(world, neighbor_state, neighbor);
                }
            }
            return true;
        }
        false
    }

    /// `SculkVeinBlock.onDischarged`: faces against sculk go, and a vein left without faces gives way to air or
    /// water.
    fn vein_discharged(&self, world: &mut impl FeatureWorld, state: u32, pos: [i32; 3]) {
        if !same_block(state, self.vein) {
            return;
        }
        let mut state = state;
        for direction in Direction::ALL {
            if has_face(state, direction)
                && same_block(state_at(world, direction.step(pos)), self.sculk)
            {
                state = with_property(state, direction.name(), "false");
            }
        }
        if !Direction::ALL
            .into_iter()
            .any(|direction| has_face(state, direction))
        {
            state = if fluid(state_at(world, pos)) == Fluid::Empty {
                block_states().default_state("minecraft:air").expect("air")
            } else {
                self.water
            };
        }
        set(world, pos, state);
    }

    /// `SculkBehaviour.onDischarged`.
    fn discharged(
        &self,
        world: &mut impl FeatureWorld,
        behaviour: Behaviour,
        state: u32,
        pos: [i32; 3],
    ) {
        if behaviour == Behaviour::Vein {
            self.vein_discharged(world, state, pos);
        }
    }

    /// `ChargeCursor.getValidMovementPos`.
    fn valid_movement_pos<R: RandomSource>(
        &self,
        world: &impl FeatureWorld,
        pos: [i32; 3],
        random: &mut WorldgenRandom<R>,
    ) -> Option<[i32; 3]> {
        let mut offsets = non_corner_neighbours();
        for i in (2..=offsets.len()).rev() {
            let j = random.next_int_bounded(i as i32) as usize;
            offsets.swap(i - 1, j);
        }
        let mut chosen = pos;
        for offset in offsets {
            let neighbor = add(pos, offset);
            let state = state_at(world, neighbor);
            if self.is_sculk_behaviour(state) && movement_unobstructed(world, pos, neighbor) {
                chosen = neighbor;
                if self.has_substrate_access(world, state, neighbor) {
                    break;
                }
            }
        }
        (chosen != pos).then_some(chosen)
    }
}

/// `SculkBlock.getDecayPenalty`: more decay farther from the origin.
fn decay_penalty(pos: [i32; 3], origin: [i32; 3], charge: i32) -> i32 {
    let distance = dist_sqr(pos, origin).sqrt() as f32 - NO_GROWTH_RADIUS as f32;
    let square = distance * distance;
    let range = (MAX_GROWTH_RATE_RADIUS - NO_GROWTH_RADIUS).pow(2);
    let factor = (square / range as f32).min(1.0);
    ((charge as f32 * factor * 0.5) as i32).max(1)
}

/// `ChargeCursor.isMovementUnobstructed`: a face neighbor, or an edge neighbor with an open side toward it.
fn movement_unobstructed(world: &impl FeatureWorld, from: [i32; 3], to: [i32; 3]) -> bool {
    let delta = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    if delta.iter().map(|d| d.abs()).sum::<i32>() == 1 {
        return true;
    }
    let x = if delta[0] < 0 {
        Direction::West
    } else {
        Direction::East
    };
    let y = if delta[1] < 0 {
        Direction::Down
    } else {
        Direction::Up
    };
    let z = if delta[2] < 0 {
        Direction::North
    } else {
        Direction::South
    };
    let open = |direction: Direction| {
        let neighbor = direction.step(from);
        !sturdy(state_at(world, neighbor), direction.opposite())
    };
    if delta[0] == 0 {
        open(y) || open(z)
    } else if delta[1] == 0 {
        open(x) || open(z)
    } else {
        open(x) || open(y)
    }
}

impl Cursor {
    fn new(pos: [i32; 3], charge: i32) -> Self {
        Self {
            pos,
            charge,
            update_delay: 0,
            decay_delay: 1,
            facings: None,
        }
    }

    /// `ChargeCursor.update` for the world generation spreader.
    fn update<R: RandomSource>(
        &mut self,
        blocks: &SculkBlocks,
        world: &mut impl FeatureWorld,
        origin: [i32; 3],
        random: &mut WorldgenRandom<R>,
        convert: bool,
    ) {
        if self.charge <= 0 {
            return;
        }
        if self.update_delay > 0 {
            self.update_delay -= 1;
            return;
        }
        let mut state = state_at(world, self.pos);
        let mut behaviour = blocks.behaviour(state);
        if convert
            && blocks.attempt_spread_vein(
                world,
                behaviour,
                self.pos,
                state,
                self.facings.as_deref(),
            )
            && behaviour != Behaviour::Sculk
        {
            // Every behaviour but sculk's may change its block as it spreads.
            state = state_at(world, self.pos);
            behaviour = blocks.behaviour(state);
        }
        self.charge = blocks.attempt_use_charge(behaviour, self, world, origin, random, convert);
        if self.charge <= 0 {
            blocks.discharged(world, behaviour, state, self.pos);
            return;
        }
        if let Some(next) = blocks.valid_movement_pos(world, self.pos, random) {
            blocks.discharged(world, behaviour, state, self.pos);
            self.pos = next;
            if !closer_than(next, [origin[0], next[1], origin[2]], 15.0) {
                self.charge = 0;
                return;
            }
            state = state_at(world, next);
        }
        if blocks.is_sculk_behaviour(state) {
            self.facings = Some(
                Direction::ALL
                    .into_iter()
                    .filter(|&direction| {
                        same_block(state, blocks.vein) && has_face(state, direction)
                    })
                    .collect(),
            );
        }
        // The delays come from the behaviour of the block the cursor used its charge on.
        self.decay_delay = match behaviour {
            Behaviour::Default => (self.decay_delay - 1).max(0),
            Behaviour::Sculk | Behaviour::Vein => 1,
        };
        self.update_delay = 1;
    }
}

/// `SculkPatchFeature.canSpreadFrom`: sculk, or air or a water source beside a full block.
fn can_spread_from(world: &impl FeatureWorld, blocks: &SculkBlocks, pos: [i32; 3]) -> bool {
    let state = state_at(world, pos);
    if blocks.is_sculk_behaviour(state) {
        return true;
    }
    if !block_states().is_air(state) && !(same_block(state, blocks.water) && source_water(state)) {
        return false;
    }
    Direction::ALL
        .into_iter()
        .any(|direction| collision_full_block(state_at(world, direction.step(pos))))
}

/// Places a sculk patch at `origin` (`SculkPatchFeature.place`).
pub(crate) fn place_sculk_patch<R: RandomSource>(
    world: &mut impl FeatureWorld,
    patch: &SculkPatch,
    random: &mut WorldgenRandom<R>,
    origin: [i32; 3],
) -> bool {
    let blocks = &patch.blocks;
    if !can_spread_from(world, blocks, origin) {
        return false;
    }
    let mut cursors: Vec<Cursor> = Vec::new();
    for round in 0..patch.spread_rounds + patch.growth_rounds {
        for _ in 0..patch.charge_count {
            // `SculkSpreader.addCursors`: charge in cursors of at most 1000, while fewer than 32 exist.
            let mut charge = patch.amount_per_charge;
            while charge > 0 {
                let amount = charge.min(MAX_CHARGE);
                if cursors.len() < MAX_CURSORS {
                    cursors.push(Cursor::new(origin, amount));
                }
                charge -= amount;
            }
        }
        let convert = round < patch.spread_rounds;
        for _ in 0..patch.spread_attempts {
            // `SculkSpreader.updateCursors`: the world generation spreader keeps every cursor that has charge left,
            // without merging.
            let mut kept = Vec::with_capacity(cursors.len());
            for mut cursor in cursors {
                let far = (0..3)
                    .map(|i| (cursor.pos[i] - origin[i]).abs())
                    .max()
                    .unwrap_or(0);
                if far > MAX_CURSOR_DISTANCE {
                    continue;
                }
                cursor.update(blocks, world, origin, random, convert);
                if cursor.charge > 0 {
                    kept.push(cursor);
                }
            }
            cursors = kept;
        }
        cursors.clear();
    }
    let below = Direction::Down.step(origin);
    if random.next_float() <= patch.catalyst_chance && collision_full_block(state_at(world, below))
    {
        // The block below must be full, but the catalyst takes the origin's place.
        set(world, origin, blocks.catalyst);
    }
    let growths = sample_int_provider(&patch.extra_rare_growths, random).unwrap_or(0);
    for _ in 0..growths {
        let dx = random.next_int_bounded(5) - 2;
        let dz = random.next_int_bounded(5) - 2;
        let pos = add(origin, [dx, 0, dz]);
        if block_states().is_air(state_at(world, pos))
            && sturdy(state_at(world, Direction::Down.step(pos)), Direction::Up)
        {
            set(
                world,
                pos,
                with_property(blocks.shrieker, "can_summon", "true"),
            );
        }
    }
    true
}
