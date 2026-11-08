//! The FEATURES stage on the chunk workers.
//!
//! A chunk's features write into the 3x3 chunks around it, so a chunk decorates once all eight neighbors
//! have completed carvers, and it is final only when its eight neighbors have decorated too: from then on no
//! decoration writes into it. Until then it is a proto-chunk, held here in memory and shared by the workers,
//! or saved to its region file with vanilla's `carvers` or `features` status when memory runs short or the
//! server stops, so either Harpas or vanilla can resume it.
//!
//! The logic thread receives only final chunks, and a chunk leaves this store when it does. Every
//! decoration finds its 3x3 chunks here, so a decoration never writes into a chunk the logic thread owns.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use harpas_anvil::{Chunk, ChunkSection};
use harpas_core::nbt::{Compound, Nbt};
use harpas_worldgen::{
    FeatureIndex, GeneratedChunk, HeightmapType, RegionFeatureWorld, WorldgenData,
    apply_biome_decoration,
};

use crate::world::{ChunkGenerator, ChunkPos, ChunkSource, ChunkStorage, Generated};

const CARVERS: &str = "minecraft:carvers";
const FEATURES: &str = "minecraft:features";

/// Vanilla's proto-chunk statuses before `carvers`. A chunk saved at one of them has received no decoration
/// writes, so it is generated again from the seed.
const EARLIER: [&str; 6] = [
    "minecraft:empty",
    "minecraft:structure_starts",
    "minecraft:structure_references",
    "minecraft:biomes",
    "minecraft:noise",
    "minecraft:surface",
];

/// Proto-chunks kept in memory before the least recently used are saved. Around one player with a view
/// distance of 10, about 300 chunks are proto-chunks.
const CAPACITY: usize = 2_048;

/// The Overworld's height in blocks, the only dimension generated so far.
const OVERWORLD_HEIGHT: i32 = 384;

/// Saved decorated chunks remembered without reading their region again.
const DECORATED_CACHE: usize = 65_536;

/// Wraps a generator of `carvers` chunks so that chunks run vanilla's FEATURES stage before the logic thread
/// receives them.
pub fn decorated(
    generator: Arc<ChunkGenerator>,
    seed: i64,
    data: &WorldgenData,
) -> io::Result<Arc<dyn ChunkSource>> {
    let index = FeatureIndex::overworld(data).map_err(io::Error::other)?;
    Ok(Arc::new(Decorator::new(
        generator,
        seed,
        index,
        data.biomes.len(),
        CAPACITY,
    )))
}

/// A chunk that has completed carvers but is not final yet.
struct Proto {
    blocks: GeneratedChunk,
    /// The rest of the chunk, with the sections inside the world's height taken out into `blocks`.
    shell: Chunk,
    decorated: bool,
    used: u64,
}

enum Slot {
    /// A worker is generating, loading, decorating or saving the chunk.
    Busy,
    Ready(Box<Proto>),
}

#[derive(Default)]
struct State {
    slots: HashMap<ChunkPos, Slot>,
    /// Chunks given to the logic thread in this session. It saves them, so the region holds them only after
    /// that; until then nothing may load or generate them again.
    handed_out: HashSet<ChunkPos>,
    /// Chunks whose saved copy has decorated.
    decorated: HashSet<ChunkPos>,
    clock: u64,
}

pub(crate) struct Decorator {
    generator: Arc<ChunkGenerator>,
    seed: i64,
    index: FeatureIndex,
    biome_count: usize,
    capacity: usize,
    state: Mutex<State>,
    changed: Condvar,
}

/// Slots a worker marked busy. Dropping it without [`Claim::release`], after an error or a panic, forgets
/// them, so the other workers do not wait forever.
struct Claim<'a> {
    decorator: &'a Decorator,
    positions: Vec<ChunkPos>,
}

impl Claim<'_> {
    fn release(mut self, protos: impl IntoIterator<Item = (ChunkPos, Box<Proto>)>) {
        let mut state = self.decorator.lock();
        for (pos, proto) in protos {
            state.slots.insert(pos, Slot::Ready(proto));
        }
        self.positions.clear();
        drop(state);
        self.decorator.changed.notify_all();
    }

    fn remove(mut self) {
        let mut state = self.decorator.lock();
        for pos in self.positions.drain(..) {
            state.slots.remove(&pos);
        }
        drop(state);
        self.decorator.changed.notify_all();
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        if self.positions.is_empty() {
            return;
        }
        let mut state = self.decorator.lock();
        for pos in &self.positions {
            if matches!(state.slots.get(pos), Some(Slot::Busy)) {
                state.slots.remove(pos);
            }
        }
        drop(state);
        self.decorator.changed.notify_all();
    }
}

fn around(pos: ChunkPos) -> impl Iterator<Item = ChunkPos> {
    (-1..=1).flat_map(move |dx| {
        (-1..=1).map(move |dz| ChunkPos {
            x: pos.x + dx,
            z: pos.z + dz,
        })
    })
}

fn status(chunk: &Compound) -> Option<&str> {
    match chunk.get("Status") {
        Some(Nbt::String(status)) => Some(status),
        _ => None,
    }
}

impl Decorator {
    pub(crate) fn new(
        generator: Arc<ChunkGenerator>,
        seed: i64,
        index: FeatureIndex,
        biome_count: usize,
        capacity: usize,
    ) -> Self {
        Self {
            generator,
            seed,
            index,
            biome_count,
            capacity,
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn tick(&self) -> u64 {
        let mut state = self.lock();
        state.clock += 1;
        state.clock
    }

    fn wait<'a>(&self, state: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        self.changed.wait(state).unwrap_or_else(|e| e.into_inner())
    }

    /// Decorates every chunk around `pos`, then takes `pos` out of the store.
    fn complete(&self, pos: ChunkPos, storage: &ChunkStorage) -> io::Result<Generated> {
        for target in around(pos) {
            self.decorate(target, storage)?;
        }
        let mut state = self.lock();
        let proto = loop {
            match state.slots.get(&pos) {
                Some(Slot::Busy) => state = self.wait(state),
                Some(Slot::Ready(_)) => {
                    let Some(Slot::Ready(proto)) = state.slots.remove(&pos) else {
                        unreachable!("the slot is ready");
                    };
                    break Some(proto);
                }
                None => break None,
            }
        };
        state.handed_out.insert(pos);
        drop(state);
        let generated = match proto {
            Some(proto) => Generated {
                chunk: into_chunk(*proto)?,
                changed: true,
            },
            // Decorated earlier and saved, by this store or by the logic thread.
            None => {
                let saved = storage.read(pos)?.ok_or_else(|| {
                    io::Error::other(format!(
                        "decorated chunk {}, {} is neither loaded nor saved",
                        pos.x, pos.z
                    ))
                })?;
                Generated {
                    chunk: storage.decode(saved)?,
                    changed: false,
                }
            }
        };
        self.evict(storage)?;
        Ok(generated)
    }

    /// Runs the FEATURES stage of `target` unless it has run already, loading or generating its 3x3 chunks.
    fn decorate(&self, target: ChunkPos, storage: &ChunkStorage) -> io::Result<()> {
        loop {
            let mut state = self.lock();
            if state.handed_out.contains(&target) || state.decorated.contains(&target) {
                return Ok(());
            }
            if let Some(Slot::Ready(proto)) = state.slots.get(&target)
                && proto.decorated
            {
                return Ok(());
            }
            if around(target).any(|pos| matches!(state.slots.get(&pos), Some(Slot::Busy))) {
                drop(self.wait(state));
                continue;
            }
            // The target first: when its saved copy has decorated, its neighbors may be final and must not
            // be loaded.
            let missing = std::iter::once(target)
                .chain(around(target))
                .find(|pos| !state.slots.contains_key(pos));
            if let Some(missing) = missing {
                if state.handed_out.contains(&missing) {
                    return Err(io::Error::other(format!(
                        "chunk {}, {} is final but its neighbor {}, {} has not decorated",
                        missing.x, missing.z, target.x, target.z
                    )));
                }
                state.slots.insert(missing, Slot::Busy);
                drop(state);
                let claim = Claim {
                    decorator: self,
                    positions: vec![missing],
                };
                match self.load(missing, missing == target, storage)? {
                    Some(mut proto) => {
                        proto.used = self.tick();
                        claim.release([(missing, proto)]);
                    }
                    None => {
                        // The target's saved copy has decorated already.
                        claim.remove();
                        let mut state = self.lock();
                        if state.decorated.len() >= DECORATED_CACHE {
                            state.decorated.clear();
                        }
                        state.decorated.insert(target);
                    }
                }
                continue;
            }
            let positions: Vec<_> = around(target).collect();
            let mut protos = Vec::with_capacity(positions.len());
            for &pos in &positions {
                let Some(Slot::Ready(proto)) = state.slots.insert(pos, Slot::Busy) else {
                    unreachable!("every chunk around the target is ready");
                };
                protos.push((pos, proto));
            }
            drop(state);
            let claim = Claim {
                decorator: self,
                positions,
            };
            let protos = self.run_features(target, protos)?;
            claim.release(protos);
            return Ok(());
        }
    }

    /// Loads a chunk that has not decorated, or generates it through carvers. With `target`, returns `None`
    /// when the saved chunk has decorated already, without keeping it in memory.
    fn load(
        &self,
        pos: ChunkPos,
        target: bool,
        storage: &ChunkStorage,
    ) -> io::Result<Option<Box<Proto>>> {
        let saved = storage.read(pos)?;
        let (chunk, decorated) = match saved {
            Some(nbt) if status(&nbt) == Some(FEATURES) => {
                if target {
                    return Ok(None);
                }
                (storage.decode(nbt)?, true)
            }
            Some(nbt) if status(&nbt) == Some(CARVERS) => (storage.decode(nbt)?, false),
            Some(nbt) if !status(&nbt).is_some_and(|status| EARLIER.contains(&status)) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "chunk {}, {} has status {}, but a neighbor has not decorated",
                        pos.x,
                        pos.z,
                        status(&nbt).unwrap_or("none")
                    ),
                ));
            }
            _ => {
                storage.check_generation(pos)?;
                (
                    (self.generator)(pos, &|source| storage.read(source))?,
                    false,
                )
            }
        };
        let mut proto = into_proto(chunk, pos, self.biome_count)?;
        proto.decorated = decorated;
        Ok(Some(Box::new(proto)))
    }

    fn run_features(
        &self,
        target: ChunkPos,
        protos: Vec<(ChunkPos, Box<Proto>)>,
    ) -> io::Result<Vec<(ChunkPos, Box<Proto>)>> {
        let mut shells = HashMap::with_capacity(protos.len());
        let mut blocks = Vec::with_capacity(protos.len());
        for (pos, proto) in protos {
            let Proto {
                blocks: chunk,
                shell,
                decorated,
                ..
            } = *proto;
            shells.insert(pos, (shell, decorated));
            blocks.push(chunk);
        }
        let mut world = RegionFeatureWorld::with_stored_biomes(blocks, self.seed)
            .ok_or_else(|| io::Error::other("chunks around a target differ in height"))?;
        apply_biome_decoration(&mut world, &self.index, self.seed, target.x, target.z)
            .map_err(io::Error::other)?;
        let used = self.tick();
        Ok(world
            .into_chunks()
            .map(|chunk| {
                let pos = ChunkPos {
                    x: chunk.chunk_x(),
                    z: chunk.chunk_z(),
                };
                let (shell, decorated) = shells.remove(&pos).expect("a chunk of the region");
                let proto = Proto {
                    blocks: chunk,
                    shell,
                    decorated: decorated || pos == target,
                    used,
                };
                (pos, Box::new(proto))
            })
            .collect())
    }

    /// Saves the least recently used proto-chunks once the store holds more than its capacity.
    fn evict(&self, storage: &ChunkStorage) -> io::Result<()> {
        let mut state = self.lock();
        if state.slots.len() <= self.capacity {
            return Ok(());
        }
        let mut ready: Vec<_> = state
            .slots
            .iter()
            .filter_map(|(pos, slot)| match slot {
                Slot::Ready(proto) => Some((proto.used, *pos)),
                Slot::Busy => None,
            })
            .collect();
        ready.sort_unstable_by_key(|&(used, pos)| (used, pos.x, pos.z));
        let count = state.slots.len() - self.capacity * 3 / 4;
        let positions: Vec<_> = ready.into_iter().take(count).map(|(_, pos)| pos).collect();
        let protos = take(&mut state, &positions);
        drop(state);
        self.save(protos, storage)
    }

    /// Saves proto-chunks, then forgets them. They stay busy while they are written, so no worker reads
    /// their region before the write.
    fn save(&self, protos: Vec<(ChunkPos, Box<Proto>)>, storage: &ChunkStorage) -> io::Result<()> {
        let claim = Claim {
            decorator: self,
            positions: protos.iter().map(|(pos, _)| *pos).collect(),
        };
        let chunks = protos
            .into_iter()
            .map(|(pos, proto)| Ok((pos, into_chunk(*proto)?)))
            .collect::<io::Result<Vec<_>>>()?;
        let refs: Vec<_> = chunks.iter().map(|(pos, chunk)| (*pos, chunk)).collect();
        storage.write(&refs)?;
        let mut state = self.lock();
        for (pos, chunk) in &chunks {
            if status(&chunk.rest) == Some(FEATURES) {
                if state.decorated.len() >= DECORATED_CACHE {
                    state.decorated.clear();
                }
                state.decorated.insert(*pos);
            }
        }
        drop(state);
        claim.remove();
        Ok(())
    }

    #[cfg(test)]
    fn held(&self) -> usize {
        self.lock().slots.len()
    }
}

/// Takes ready protos out of the store, leaving their slots busy.
fn take(state: &mut State, positions: &[ChunkPos]) -> Vec<(ChunkPos, Box<Proto>)> {
    positions
        .iter()
        .map(|&pos| {
            let Some(Slot::Ready(proto)) = state.slots.insert(pos, Slot::Busy) else {
                unreachable!("only ready protos are taken");
            };
            (pos, proto)
        })
        .collect()
}

impl ChunkSource for Decorator {
    fn resumes(&self, status: &str) -> bool {
        status == CARVERS || status == FEATURES || EARLIER.contains(&status)
    }

    fn generate(
        &self,
        pos: ChunkPos,
        _saved: Option<Compound>,
        storage: &ChunkStorage,
    ) -> io::Result<Generated> {
        self.complete(pos, storage)
    }

    fn flush(&self, storage: &ChunkStorage) -> io::Result<()> {
        loop {
            let mut state = self.lock();
            let positions: Vec<_> = state
                .slots
                .iter()
                .filter(|(_, slot)| matches!(slot, Slot::Ready(_)))
                .map(|(pos, _)| *pos)
                .collect();
            if positions.is_empty() {
                if state.slots.is_empty() {
                    return Ok(());
                }
                drop(self.wait(state));
                continue;
            }
            let protos = take(&mut state, &positions);
            drop(state);
            self.save(protos, storage)?;
        }
    }
}

fn into_proto(mut shell: Chunk, pos: ChunkPos, biome_count: usize) -> io::Result<Proto> {
    let Some(Nbt::Int(min_section)) = shell.rest.get("yPos") else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("chunk {}, {} has no minimum section", pos.x, pos.z),
        ));
    };
    let min_section = *min_section;
    let (min_y, height) = (min_section * 16, OVERWORLD_HEIGHT);
    let count = (height / 16) as usize;
    let mut sections = vec![None; count];
    for section in &mut shell.sections {
        let index = i32::from(section.y) - min_section;
        if let Some(slot) = usize::try_from(index)
            .ok()
            .and_then(|index| sections.get_mut(index))
        {
            *slot = section.section.take();
        }
    }
    let mut blocks =
        GeneratedChunk::from_sections(pos.x, pos.z, min_y, height, biome_count, sections)
            .ok_or_else(|| io::Error::other("chunk sections do not fit the world height"))?;
    // A saved proto-chunk has run carvers, so its worldgen heightmaps no longer follow block changes.
    blocks.freeze_worldgen_heightmaps();
    Ok(Proto {
        blocks,
        shell,
        decorated: false,
        used: 0,
    })
}

fn into_chunk(proto: Proto) -> io::Result<Chunk> {
    let Proto {
        blocks,
        mut shell,
        decorated,
        ..
    } = proto;
    let min_section = blocks.min_section_y();
    let mut maps = Compound::new();
    for kind in [
        HeightmapType::WorldSurface,
        HeightmapType::OceanFloor,
        HeightmapType::MotionBlocking,
        HeightmapType::MotionBlockingNoLeaves,
    ] {
        maps.insert(
            kind.nbt_key(),
            Nbt::LongArray(blocks.heightmaps().pack(kind)),
        );
    }
    shell.rest.insert("Heightmaps", maps);
    if decorated {
        shell.rest.insert("Status", FEATURES);
    }
    for (index, section) in blocks.into_sections().into_iter().enumerate() {
        let y = i8::try_from(min_section + index as i32).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "section Y does not fit in Anvil",
            )
        })?;
        match shell.sections.iter_mut().find(|saved| saved.y == y) {
            Some(saved) => saved.section = Some(section),
            None => shell.sections.push(ChunkSection {
                y,
                section: Some(section),
                rest: Compound::new(),
            }),
        }
    }
    shell.sections.sort_by_key(|section| section.y);
    Ok(shell)
}

#[cfg(test)]
mod tests;
