//! Scheduled ticks in vanilla's order (`LevelTicks`, `LevelChunkTicks` and `ScheduledTick`).
//!
//! A tick is scheduled for a block position and a type (a block, or a fluid) to run at a game time, with a
//! priority and a sub-tick order that counts every tick the level schedules. A position has at most one tick of
//! each type. Each chunk keeps its ticks in a priority queue ordered by game time, then priority, then sub-tick
//! order. When the ticks are due, the chunks whose first tick is due are drained into one list, choosing between
//! the chunks by priority and sub-tick order only, and at most 65,536 ticks run each game tick.
//!
//! The queues are Java's `PriorityQueue`, ported with the same binary heap, because a chunk saves its ticks in the
//! order of the heap's array and a chunk read back numbers them in that order.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use harpas_core::nbt::{Compound, List, Nbt};

use crate::block::BlockPos;
use crate::world::ChunkPos;

/// The most ticks of one type that run in one game tick (`ServerLevel.tick`).
pub(crate) const MAX_TICKS: usize = 65_536;

/// A tick to run at a block (`ScheduledTick`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScheduledTick<T> {
    pub(crate) kind: T,
    pub(crate) pos: BlockPos,
    /// The game time it runs at.
    pub(crate) trigger: i64,
    /// `TickPriority`: lower runs first, 0 is normal.
    pub(crate) priority: i32,
    /// Its place among the ticks scheduled in the same game tick.
    pub(crate) sub_tick: i64,
}

impl<T> ScheduledTick<T> {
    /// `DRIVER_TICK_COMPARATOR`: game time, then priority, then sub-tick order.
    fn driver_order(&self, other: &Self) -> Ordering {
        self.trigger
            .cmp(&other.trigger)
            .then(self.priority.cmp(&other.priority))
            .then(self.sub_tick.cmp(&other.sub_tick))
    }

    /// `INTRA_TICK_DRAIN_ORDER`: priority, then sub-tick order.
    fn drain_order(&self, other: &Self) -> Ordering {
        self.priority
            .cmp(&other.priority)
            .then(self.sub_tick.cmp(&other.sub_tick))
    }
}

/// Java's `PriorityQueue`: a binary heap in an array, with its `siftUp` and `siftDown`.
#[derive(Debug, Clone)]
pub(crate) struct JavaPriorityQueue<T> {
    heap: Vec<T>,
}

impl<T> Default for JavaPriorityQueue<T> {
    fn default() -> Self {
        Self { heap: Vec::new() }
    }
}

impl<T> JavaPriorityQueue<T> {
    pub(crate) fn peek(&self) -> Option<&T> {
        self.heap.first()
    }

    /// The elements in the order of the heap's array, as Java iterates them.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &T> {
        self.heap.iter()
    }

    pub(crate) fn add(&mut self, item: T, order: impl Fn(&T, &T) -> Ordering) {
        self.heap.push(item);
        let mut k = self.heap.len() - 1;
        while k > 0 {
            let parent = (k - 1) >> 1;
            if order(&self.heap[k], &self.heap[parent]) != Ordering::Less {
                break;
            }
            self.heap.swap(k, parent);
            k = parent;
        }
    }

    pub(crate) fn poll(&mut self, order: impl Fn(&T, &T) -> Ordering) -> Option<T> {
        if self.heap.is_empty() {
            return None;
        }
        let last = self.heap.pop()?;
        if self.heap.is_empty() {
            return Some(last);
        }
        let result = std::mem::replace(&mut self.heap[0], last);
        // `siftDown` from the root.
        let n = self.heap.len();
        let half = n >> 1;
        let mut k = 0;
        while k < half {
            let mut child = 2 * k + 1;
            let right = child + 1;
            if right < n && order(&self.heap[child], &self.heap[right]) == Ordering::Greater {
                child = right;
            }
            if order(&self.heap[k], &self.heap[child]) != Ordering::Greater {
                break;
            }
            self.heap.swap(k, child);
            k = child;
        }
        Some(result)
    }
}

/// A tick read from a saved chunk (`SavedTick`): its delay counts from the game time the chunk was saved at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SavedTick<T> {
    kind: T,
    pos: BlockPos,
    delay: i32,
    priority: i32,
}

/// The ticks of one chunk (`LevelChunkTicks`).
#[derive(Debug)]
pub(crate) struct ChunkTicks<T> {
    queue: JavaPriorityQueue<ScheduledTick<T>>,
    /// One tick per position and type.
    scheduled: HashSet<(T, BlockPos)>,
}

impl<T> Default for ChunkTicks<T> {
    fn default() -> Self {
        Self {
            queue: JavaPriorityQueue::default(),
            scheduled: HashSet::new(),
        }
    }
}

impl<T: Copy + Eq + Hash> ChunkTicks<T> {
    /// The ticks read from a chunk, given sub-tick orders from minus their count up to minus one in the order
    /// they were saved (`LevelChunkTicks.unpack`).
    fn unpack(saved: Vec<SavedTick<T>>, game_time: i64) -> Self {
        let mut ticks = Self::default();
        let count = saved.len() as i64;
        for (index, tick) in saved.into_iter().enumerate() {
            let tick = ScheduledTick {
                kind: tick.kind,
                pos: tick.pos,
                trigger: game_time + i64::from(tick.delay),
                priority: tick.priority,
                sub_tick: index as i64 - count,
            };
            if ticks.scheduled.insert((tick.kind, tick.pos)) {
                ticks.queue.add(tick, ScheduledTick::driver_order);
            }
        }
        ticks
    }

    fn peek(&self) -> Option<&ScheduledTick<T>> {
        self.queue.peek()
    }

    fn poll(&mut self) -> Option<ScheduledTick<T>> {
        let tick = self.queue.poll(ScheduledTick::driver_order)?;
        self.scheduled.remove(&(tick.kind, tick.pos));
        Some(tick)
    }

    /// Schedules a tick unless the position already has one of its type; returns whether it was scheduled.
    fn schedule(&mut self, tick: ScheduledTick<T>) -> bool {
        if !self.scheduled.insert((tick.kind, tick.pos)) {
            return false;
        }
        self.queue.add(tick, ScheduledTick::driver_order);
        true
    }

    /// The ticks as a chunk saves them at `game_time`, in the order of the queue's array (`pack`).
    fn pack(&self, game_time: i64) -> Vec<SavedTick<T>> {
        self.queue
            .iter()
            .map(|tick| SavedTick {
                kind: tick.kind,
                pos: tick.pos,
                delay: (tick.trigger - game_time) as i32,
                priority: tick.priority,
            })
            .collect()
    }
}

/// The ticks of one type of a level (`LevelTicks`).
#[derive(Debug)]
pub(crate) struct LevelTicks<T> {
    containers: HashMap<ChunkPos, ChunkTicks<T>>,
    /// The game time of the first tick of each chunk that may have one due, in the order chunks were added.
    next_tick: Vec<(ChunkPos, i64)>,
    /// The ticks counted so far, for the sub-tick order of the next one (`Level.nextSubTickCount`).
    sub_tick_count: i64,
}

impl<T> Default for LevelTicks<T> {
    fn default() -> Self {
        Self {
            containers: HashMap::new(),
            next_tick: Vec::new(),
            sub_tick_count: 0,
        }
    }
}

impl<T: Copy + Eq + Hash> LevelTicks<T> {
    /// Adds the ticks of a chunk that was read (`addContainer`).
    pub(crate) fn add_chunk(&mut self, chunk: ChunkPos, saved: Vec<SavedTick<T>>, game_time: i64) {
        let ticks = ChunkTicks::unpack(saved, game_time);
        if let Some(first) = ticks.peek() {
            self.set_next(chunk, first.trigger);
        }
        self.containers.insert(chunk, ticks);
    }

    /// Forgets the ticks of the chunks `keep` does not keep, which left (`removeContainer`).
    pub(crate) fn retain_chunks(&mut self, keep: impl Fn(ChunkPos) -> bool) {
        self.containers.retain(|pos, _| keep(*pos));
        self.next_tick.retain(|(pos, _)| keep(*pos));
    }

    pub(crate) fn has_chunk(&self, chunk: ChunkPos) -> bool {
        self.containers.contains_key(&chunk)
    }

    /// The chunk's ticks as it saves them at `game_time`.
    pub(crate) fn pack(&self, chunk: ChunkPos, game_time: i64) -> Vec<SavedTick<T>> {
        self.containers
            .get(&chunk)
            .map_or_else(Vec::new, |ticks| ticks.pack(game_time))
    }

    /// Schedules a tick `delay` game ticks after `game_time`, at normal priority (`Level.scheduleTick`). A
    /// position whose chunk has no ticks, because it is not loaded, gets none, as in vanilla. Returns whether
    /// the tick was scheduled.
    pub(crate) fn schedule(&mut self, kind: T, pos: BlockPos, game_time: i64, delay: i32) -> bool {
        let sub_tick = self.sub_tick_count;
        self.sub_tick_count += 1;
        let tick = ScheduledTick {
            kind,
            pos,
            trigger: game_time + i64::from(delay),
            priority: 0,
            sub_tick,
        };
        let chunk = pos.chunk();
        let Some(ticks) = self.containers.get_mut(&chunk) else {
            return false;
        };
        if !ticks.schedule(tick) {
            return false;
        }
        // `onTickAdded`: a new first tick moves the chunk's next tick.
        if ticks.peek() == Some(&tick) {
            self.set_next(chunk, tick.trigger);
        }
        true
    }

    /// Whether a tick of `kind` is scheduled at `pos` (`hasScheduledTick`).
    #[cfg(test)]
    pub(crate) fn has_scheduled(&self, kind: T, pos: BlockPos) -> bool {
        self.containers
            .get(&pos.chunk())
            .is_some_and(|ticks| ticks.scheduled.contains(&(kind, pos)))
    }

    fn set_next(&mut self, chunk: ChunkPos, trigger: i64) {
        match self.next_tick.iter_mut().find(|(pos, _)| *pos == chunk) {
            Some(entry) => entry.1 = trigger,
            None => self.next_tick.push((chunk, trigger)),
        }
    }

    /// The ticks due at `game_time` in the chunks `ticking` accepts, in the order they run (`collectTicks`).
    pub(crate) fn collect(
        &mut self,
        game_time: i64,
        ticking: impl Fn(ChunkPos) -> bool,
    ) -> Vec<ScheduledTick<T>> {
        // `sortContainersToTick`.
        let container_order =
            |a: &ChunkPos, b: &ChunkPos, containers: &HashMap<_, ChunkTicks<T>>| {
                let first = |pos| containers[pos].peek().expect("a chunk to drain has a tick");
                ScheduledTick::drain_order(first(a), first(b))
            };
        let mut to_drain = JavaPriorityQueue::<ChunkPos>::default();
        let mut kept = Vec::with_capacity(self.next_tick.len());
        for (chunk, next) in std::mem::take(&mut self.next_tick) {
            if next > game_time {
                kept.push((chunk, next));
                continue;
            }
            let Some(first) = self.containers.get(&chunk).and_then(ChunkTicks::peek) else {
                continue;
            };
            if first.trigger > game_time {
                kept.push((chunk, first.trigger));
            } else if ticking(chunk) {
                let containers = &self.containers;
                to_drain.add(chunk, |a, b| container_order(a, b, containers));
            } else {
                kept.push((chunk, next));
            }
        }
        self.next_tick = kept;
        // `drainContainers`.
        let mut collected = Vec::new();
        while collected.len() < MAX_TICKS {
            let containers = &self.containers;
            let Some(chunk) = to_drain.poll(|a, b| container_order(a, b, containers)) else {
                break;
            };
            let ticks = self.containers.get_mut(&chunk).expect("a chunk to drain");
            collected.push(ticks.poll().expect("a chunk to drain has a tick"));
            // `drainFromCurrentContainer`: the chunk's next ticks while they come before the next chunk's first.
            let other = to_drain.peek().map(|pos| {
                *self.containers[pos]
                    .peek()
                    .expect("a chunk to drain has a tick")
            });
            let ticks = self.containers.get_mut(&chunk).expect("a chunk to drain");
            while collected.len() < MAX_TICKS {
                let Some(&tick) = ticks.peek() else { break };
                if tick.trigger > game_time
                    || other.is_some_and(|other| tick.drain_order(&other) == Ordering::Greater)
                {
                    break;
                }
                ticks.poll();
                collected.push(tick);
            }
            if let Some(&next) = ticks.peek() {
                if next.trigger <= game_time && collected.len() < MAX_TICKS {
                    let containers = &self.containers;
                    to_drain.add(chunk, |a, b| container_order(a, b, containers));
                } else {
                    self.set_next(chunk, next.trigger);
                }
            }
        }
        // `rescheduleLeftoverContainers`.
        for chunk in to_drain.iter().copied().collect::<Vec<_>>() {
            if let Some(next) = self.containers[&chunk].peek().map(|tick| tick.trigger) {
                self.set_next(chunk, next);
            }
        }
        collected
    }
}

/// Reads a chunk's saved ticks (`SavedTick.loadTickList`): those whose `i` names no known type, or that lack a
/// field, are left out.
pub(crate) fn read_saved<T>(
    list: Option<&Nbt>,
    kind: impl Fn(&str) -> Option<T>,
) -> Vec<SavedTick<T>> {
    let Some(Nbt::List(List::Compound(ticks))) = list else {
        return Vec::new();
    };
    ticks
        .iter()
        .filter_map(|tick| {
            let int = |name| match tick.get(name) {
                Some(Nbt::Int(n)) => Some(*n),
                _ => None,
            };
            let Some(Nbt::String(id)) = tick.get("i") else {
                return None;
            };
            Some(SavedTick {
                kind: kind(id)?,
                pos: BlockPos {
                    x: int("x")?,
                    y: int("y")?,
                    z: int("z")?,
                },
                delay: int("t")?,
                priority: int("p")?,
            })
        })
        .collect()
}

/// A chunk's ticks as vanilla saves them (`SavedTick.save`).
pub(crate) fn write_saved<T>(ticks: &[SavedTick<T>], name: impl Fn(&T) -> &str) -> Nbt {
    if ticks.is_empty() {
        return Nbt::List(List::Empty);
    }
    Nbt::List(List::Compound(
        ticks
            .iter()
            .map(|tick| {
                let mut saved = Compound::new();
                saved.insert("i", name(&tick.kind));
                saved.insert("x", tick.pos.x);
                saved.insert("y", tick.pos.y);
                saved.insert("z", tick.pos.z);
                saved.insert("t", tick.delay);
                saved.insert("p", tick.priority);
                saved
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_queue_keeps_javas_heap_array_order() {
        // Java's `PriorityQueue<Integer>` after adding 5, 3, 8, 1, 9, 2 is [1, 3, 2, 5, 9, 8]; polling once leaves
        // [2, 3, 8, 5, 9].
        let mut queue = JavaPriorityQueue::default();
        for n in [5, 3, 8, 1, 9, 2] {
            queue.add(n, i32::cmp);
        }
        assert_eq!(
            queue.iter().copied().collect::<Vec<_>>(),
            [1, 3, 2, 5, 9, 8]
        );
        assert_eq!(queue.poll(i32::cmp), Some(1));
        assert_eq!(queue.iter().copied().collect::<Vec<_>>(), [2, 3, 8, 5, 9]);
        let mut sorted = Vec::new();
        while let Some(n) = queue.poll(i32::cmp) {
            sorted.push(n);
        }
        assert_eq!(sorted, [2, 3, 5, 8, 9]);
    }

    fn at(x: i32, z: i32) -> BlockPos {
        BlockPos { x, y: 64, z }
    }

    #[test]
    fn due_ticks_run_by_priority_and_sub_tick_order_across_chunks() {
        let mut ticks = LevelTicks::<u8>::default();
        let (a, b) = (ChunkPos { x: 0, z: 0 }, ChunkPos { x: 1, z: 0 });
        ticks.add_chunk(a, Vec::new(), 0);
        ticks.add_chunk(b, Vec::new(), 0);
        assert!(ticks.schedule(1, at(0, 0), 0, 5));
        assert!(ticks.schedule(1, at(16, 0), 0, 2));
        assert!(ticks.schedule(1, at(1, 0), 0, 2));
        // One tick per position and type.
        assert!(!ticks.schedule(1, at(1, 0), 0, 1));
        assert!(ticks.schedule(2, at(1, 0), 0, 1));
        // Outside every loaded chunk.
        assert!(!ticks.schedule(1, at(100, 0), 0, 1));
        assert!(ticks.collect(0, |_| true).is_empty());
        let order = |ticks: Vec<ScheduledTick<u8>>| -> Vec<_> {
            ticks.iter().map(|t| (t.kind, t.pos.x)).collect()
        };
        assert_eq!(order(ticks.collect(1, |_| true)), [(2, 1)]);
        // Due at 2 in both chunks: by sub-tick order, the one scheduled first, in the other chunk, runs first.
        assert_eq!(order(ticks.collect(2, |_| true)), [(1, 16), (1, 1)]);
        // A chunk that does not tick keeps its ticks until it does; one that is late runs them all.
        assert!(ticks.collect(9, |chunk| chunk != a).is_empty());
        assert!(ticks.has_scheduled(1, at(0, 0)));
        assert_eq!(order(ticks.collect(10, |_| true)), [(1, 0)]);
        assert!(!ticks.has_scheduled(1, at(0, 0)));
    }

    #[test]
    fn ticks_are_saved_with_their_delays_and_read_back_before_new_ones() {
        let mut ticks = LevelTicks::<u8>::default();
        let chunk = ChunkPos { x: 0, z: 0 };
        ticks.add_chunk(chunk, Vec::new(), 100);
        ticks.schedule(7, at(3, 4), 100, 30);
        ticks.schedule(7, at(5, 6), 100, 30);
        let saved = ticks.pack(chunk, 110);
        let nbt = write_saved(&saved, |_| "minecraft:water");
        let read = read_saved(Some(&nbt), |id| (id == "minecraft:water").then_some(7_u8));
        assert_eq!(read, saved);
        assert_eq!(read[0].delay, 20);
        // Read back at another game time, with a tick scheduled meanwhile for the same time: the saved ones,
        // numbered below zero, run first, in the order they were saved.
        let mut reread = LevelTicks::<u8>::default();
        reread.add_chunk(chunk, read, 1000);
        reread.schedule(7, at(9, 9), 1000, 20);
        let run: Vec<_> = reread
            .collect(1020, |_| true)
            .iter()
            .map(|t| t.pos.x)
            .collect();
        assert_eq!(run, [3, 5, 9]);
    }
}
