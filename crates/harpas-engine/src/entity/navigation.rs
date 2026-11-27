//! Dry ground navigation for a cow following wheat, including jumps and drops.

use std::collections::{HashMap, VecDeque};

use harpas_core::block_behavior::{Face, block_behavior};

use super::{ANIMAL_WIDTH, Aabb, floor};
use crate::block::BlockPos;
use crate::fluid::FluidState;
use crate::world::World;

type Point = [f64; 2];

const SEARCH_RADIUS: i32 = 12;
const REPATH_INTERVAL: u8 = 10;
const MAX_DROP: i32 = 3;
/// Clearance for the peak of the cow's normal jump from level ground.
const JUMP_CLEARANCE: f64 = 1.26;

pub(super) struct CowSteering {
    pub(super) direction: Point,
    pub(super) jump: bool,
}

/// Transient waypoints, in reverse order. `None` means no route, and an empty route means direct steering.
#[derive(Debug, Default, Clone, PartialEq)]
pub(super) struct CowNavigation {
    route: Option<Vec<BlockPos>>,
    goal: Option<BlockPos>,
    landing: Option<BlockPos>,
    retry_in: u8,
}

impl CowNavigation {
    pub(super) fn steer(
        &mut self,
        world: &World,
        [x, y, z]: [f64; 3],
        [tx, ty, tz]: [f64; 3],
        height: f64,
        on_ground: bool,
    ) -> Option<CowSteering> {
        let level = floor(y + 1.0e-7);
        let ground = Ground { world, height };
        let from = [x, z];
        if let Some(landing) = self.landing {
            // The cow can straddle the edge before falling or when first landing after a jump.
            // Finish crossing before asking the ground search for full support again.
            let to = center(landing);
            if !on_ground
                || (!ground.can_walk(from, from, level)
                    && match level - landing.y {
                        0 => ground.is_clear(sweep(from, to, y, y + height)),
                        1..=MAX_DROP => ground.can_descend(from, to, level, landing.y),
                        _ => false,
                    })
            {
                return steer(from, to, false);
            }
            self.landing = None;
        }
        if !on_ground || (y - f64::from(level)).abs() > 1.0e-7 {
            *self = Self::default();
            return None;
        }
        let target = [tx, tz];
        let mut goal = BlockPos {
            x: floor(tx),
            y: floor(ty),
            z: floor(tz),
        };
        // A jumping player still attracts the cow along the ground below.
        if goal.y > level && !ground.can_walk(center(goal), center(goal), goal.y) {
            goal.y = level;
        }
        self.retry_in = self.retry_in.saturating_sub(1);
        if self.goal != Some(goal) || self.retry_in == 0 {
            // Prefer the player over the block centers whenever the straight approach is clear.
            if goal.y == level && ground.can_walk(from, target, level) {
                self.route = Some(Vec::new());
            } else if self.goal != Some(goal)
                || self
                    .route
                    .as_ref()
                    .and_then(|route| route.last())
                    .is_none_or(|&next| !ground.can_reach(from, level, next))
            {
                self.route = ground.detour(from, level, goal);
            }
            self.goal = Some(goal);
            self.retry_in = REPATH_INTERVAL;
        }

        let route = self.route.as_mut()?;
        while let &[.., after, next] = route.as_slice() {
            // Do not cut a corner until the cow's whole width fits in the next corridor.
            if next.y != level
                || distance(from, center(next)) > 0.25
                || !ground.can_reach(from, level, after)
            {
                break;
            }
            route.pop();
        }
        let Some(&next) = route.last() else {
            return steer(from, target, false);
        };
        let to = center(next);
        let descending = next.y < level;
        if descending && !ground.can_reach(from, level, next) {
            self.retry_in = 0;
            return None;
        }
        let jump =
            next.y == level + 1 && distance(from, to) < 1.0 && ground.can_jump(from, to, level);
        if jump || descending {
            self.landing = Some(next);
        }
        steer(from, to, jump)
    }
}

struct Ground<'w> {
    world: &'w World,
    height: f64,
}

impl Ground<'_> {
    /// A conservative sweep of the cow's box: every column under the rectangle must be supported,
    /// dry and loaded, and the box must have room for the cow's whole height. For a cardinal edge
    /// this rectangle is the actual sweep; diagonal shortcuts can reject more than necessary.
    fn can_walk(&self, from: Point, to: Point, level: i32) -> bool {
        let y = f64::from(level);
        let area @ [min, max] = sweep(from, to, y, y + self.height);
        let behavior = block_behavior();
        for x in floor(min[0])..max[0].ceil() as i32 {
            for z in floor(min[2])..max[2].ceil() as i32 {
                let below = BlockPos { x, y: level - 1, z };
                if !self
                    .world
                    .block_state(below)
                    .is_some_and(|state| behavior.is_collision_face_full(state, Face::Up))
                {
                    return false;
                }
            }
        }
        self.is_clear(area)
    }

    fn is_clear(&self, area @ [min, max]: Aabb) -> bool {
        for x in floor(min[0])..max[0].ceil() as i32 {
            for z in floor(min[2])..max[2].ceil() as i32 {
                for y in floor(min[1])..max[1].ceil() as i32 {
                    if self
                        .world
                        .block_state(BlockPos { x, y, z })
                        .is_none_or(|state| FluidState::of(state).kind().is_some())
                    {
                        return false;
                    }
                }
            }
        }
        !self.world.collides(area)
    }

    fn can_jump(&self, from: Point, to: Point, level: i32) -> bool {
        let y = f64::from(level);
        let top = y + JUMP_CLEARANCE + self.height;
        self.can_walk(to, to, level + 1)
            && self.is_clear(sweep(from, from, y, top))
            && self.is_clear(sweep(from, to, y + 1.0, top))
    }

    fn can_descend(&self, from: Point, to: Point, level: i32, landing_y: i32) -> bool {
        let y = f64::from(level);
        // The opening at the edge and the whole fall to the supported landing must be clear and dry.
        self.can_walk(to, to, landing_y)
            && self.is_clear(sweep(from, to, y, y + self.height))
            && self.is_clear(sweep(to, to, f64::from(landing_y), y))
    }

    fn can_reach(&self, from: Point, level: i32, next: BlockPos) -> bool {
        match level - next.y {
            0 => self.can_walk(from, center(next), level),
            -1 => self.can_jump(from, center(next), level),
            1..=MAX_DROP => self.can_descend(from, center(next), level, next.y),
            _ => false,
        }
    }

    /// Find a shortest cardinal route on dry ground, bounded around the starting cell.
    fn detour(&self, from: Point, level: i32, goal: BlockPos) -> Option<Vec<BlockPos>> {
        let [x, z] = from.map(floor);
        let start = BlockPos { x, y: level, z };
        if !self.can_walk(from, center(start), level) {
            return None;
        }
        let mut previous = HashMap::from([(start, start)]);
        let mut frontier = VecDeque::from([start]);
        // A route can dip into a trench or climb over a rise even when both ends are level.
        let heights = (level.min(goal.y) - MAX_DROP)..=(level.max(goal.y) + 1);
        while let Some(cell @ BlockPos { x, y, z }) = frontier.pop_front() {
            if cell == goal {
                let mut route = vec![cell];
                let mut cell = cell;
                while cell != start {
                    cell = previous[&cell];
                    route.push(cell);
                }
                return Some(route);
            }
            for [nx, nz] in [[x - 1, z], [x + 1, z], [x, z - 1], [x, z + 1]] {
                if (nx - start.x).abs().max((nz - start.z).abs()) > SEARCH_RADIUS {
                    continue;
                }
                let Some(next) = [y, y + 1]
                    .into_iter()
                    .chain((y - MAX_DROP..y).rev())
                    .filter(|y| heights.contains(y))
                    .map(|y| BlockPos { x: nx, y, z: nz })
                    .find(|&next| {
                        !previous.contains_key(&next) && self.can_reach(center(cell), y, next)
                    })
                else {
                    continue;
                };
                previous.insert(next, cell);
                frontier.push_back(next);
            }
        }
        None
    }
}

fn sweep([x, z]: Point, [tx, tz]: Point, bottom: f64, top: f64) -> Aabb {
    let half = ANIMAL_WIDTH / 2.0 - 1.0e-7;
    [
        [x.min(tx) - half, bottom, z.min(tz) - half],
        [x.max(tx) + half, top, z.max(tz) + half],
    ]
}

fn center(pos: BlockPos) -> Point {
    [f64::from(pos.x) + 0.5, f64::from(pos.z) + 0.5]
}

fn steer(from @ [x, z]: Point, to @ [tx, tz]: Point, jump: bool) -> Option<CowSteering> {
    let length = distance(from, to);
    (length > 0.0).then(|| CowSteering {
        direction: [(tx - x) / length, (tz - z) / length],
        jump,
    })
}

fn distance([x, z]: Point, [tx, tz]: Point) -> f64 {
    (tx - x).hypot(tz - z)
}
