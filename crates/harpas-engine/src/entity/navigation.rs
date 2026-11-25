//! Same-level, dry ground navigation for a cow following wheat. Jumps, drops and path costs for
//! terrain such as water are not modeled yet.

use std::collections::{HashMap, VecDeque, hash_map::Entry};

use harpas_core::block_behavior::{Face, block_behavior};

use super::{ANIMAL_WIDTH, floor};
use crate::block::BlockPos;
use crate::fluid::FluidState;
use crate::world::World;

type Cell = [i32; 2];
type Point = [f64; 2];

const SEARCH_RADIUS: i32 = 12;
const REPATH_INTERVAL: u8 = 10;

/// Transient waypoints, in reverse order. `None` means no route, and an empty route means direct steering.
#[derive(Debug, Default, Clone, PartialEq)]
pub(super) struct CowNavigation {
    route: Option<Vec<Point>>,
    goal: Option<BlockPos>,
    retry_in: u8,
}

impl CowNavigation {
    pub(super) fn direction(
        &mut self,
        world: &World,
        [x, y, z]: [f64; 3],
        target @ [tx, tz]: Point,
        height: f64,
    ) -> Option<Point> {
        let level = floor(y + 1.0e-7);
        if (y - f64::from(level)).abs() > 1.0e-7 {
            *self = Self::default();
            return None;
        }
        let ground = Ground {
            world,
            level,
            height,
        };
        let from = [x, z];
        let goal = BlockPos {
            x: floor(tx),
            y: level,
            z: floor(tz),
        };
        self.retry_in = self.retry_in.saturating_sub(1);
        if self.goal != Some(goal) || self.retry_in == 0 {
            // Prefer the player over the block centers whenever the straight approach is clear.
            if ground.can_walk(from, target) {
                self.route = Some(Vec::new());
            } else if self.goal != Some(goal)
                || self
                    .route
                    .as_ref()
                    .and_then(|route| route.last())
                    .is_none_or(|&next| !ground.can_walk(from, next))
            {
                self.route = ground.detour(from, [goal.x, goal.z]);
            }
            self.goal = Some(goal);
            self.retry_in = REPATH_INTERVAL;
        }

        let route = self.route.as_mut()?;
        while let [.., after, next] = route.as_slice() {
            // Do not cut a corner until the cow's whole width fits in the next corridor.
            if distance(from, *next) > 0.25 || !ground.can_walk(from, *after) {
                break;
            }
            route.pop();
        }
        let [nx, nz] = route.last().copied().unwrap_or(target);
        let length = distance(from, [nx, nz]);
        (length > 0.0).then(|| [(nx - x) / length, (nz - z) / length])
    }
}

struct Ground<'w> {
    world: &'w World,
    level: i32,
    height: f64,
}

impl Ground<'_> {
    /// A conservative sweep of the cow's box: every column under the rectangle must be supported,
    /// dry and loaded, and the box must have room for the cow's whole height. For a cardinal edge
    /// this rectangle is the actual sweep; diagonal shortcuts can reject more than necessary.
    fn can_walk(&self, [x, z]: Point, [tx, tz]: Point) -> bool {
        let half = ANIMAL_WIDTH / 2.0 - 1.0e-7;
        let y = f64::from(self.level);
        let min = [x.min(tx) - half, y, z.min(tz) - half];
        let max = [x.max(tx) + half, y + self.height, z.max(tz) + half];
        let behavior = block_behavior();
        for x in floor(min[0])..max[0].ceil() as i32 {
            for z in floor(min[2])..max[2].ceil() as i32 {
                let below = BlockPos {
                    x,
                    y: self.level - 1,
                    z,
                };
                if !self
                    .world
                    .block_state(below)
                    .is_some_and(|state| behavior.is_collision_face_full(state, Face::Up))
                {
                    return false;
                }
                for y in self.level..max[1].ceil() as i32 {
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
        !self.world.collides([min, max])
    }

    /// Find a shortest cardinal detour on the current level, bounded around the cow's starting cell.
    fn detour(&self, from: Point, goal: Cell) -> Option<Vec<Point>> {
        let start = from.map(floor);
        if !self.can_walk(from, center(start)) {
            return None;
        }
        let mut previous = HashMap::from([(start, start)]);
        let mut frontier = VecDeque::from([start]);
        while let Some(cell @ [x, z]) = frontier.pop_front() {
            if cell == goal {
                let mut route = vec![center(cell)];
                let mut cell = cell;
                while cell != start {
                    cell = previous[&cell];
                    route.push(center(cell));
                }
                return Some(route);
            }
            for next in [[x - 1, z], [x + 1, z], [x, z - 1], [x, z + 1]] {
                if (next[0] - start[0]).abs().max((next[1] - start[1]).abs()) <= SEARCH_RADIUS
                    && let Entry::Vacant(parent) = previous.entry(next)
                    && self.can_walk(center(cell), center(next))
                {
                    parent.insert(cell);
                    frontier.push_back(next);
                }
            }
        }
        None
    }
}

fn center(cell: Cell) -> Point {
    cell.map(|n| f64::from(n) + 0.5)
}

fn distance([x, z]: Point, [tx, tz]: Point) -> f64 {
    (tx - x).hypot(tz - z)
}
