//! Shared structure starts for worker-generated chunks. Saved neighbors take precedence over regeneration.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::{Arc, Mutex};

use harpas_core::nbt::{Compound, Nbt};
use harpas_worldgen::{
    Beardifier, NoiseBasedChunkGenerator, StructureStart, StructureState, StructureTemplates,
    WorldgenData,
};

use crate::world::{ChunkPos, ChunkReader};

type Starts = Arc<Vec<StructureStart>>;
type Entry = Arc<Mutex<Option<Starts>>>;

struct Entries {
    by_pos: HashMap<ChunkPos, Entry>,
    order: VecDeque<ChunkPos>,
}

/// Bounds both successful and empty starts; each cached entry has its own lock so unrelated starts run in parallel.
struct StartCache {
    capacity: usize,
    entries: Mutex<Entries>,
}

impl StartCache {
    fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            capacity,
            entries: Mutex::new(Entries {
                by_pos: HashMap::new(),
                order: VecDeque::new(),
            }),
        }
    }

    fn get(
        &self,
        pos: ChunkPos,
        load: impl FnOnce() -> io::Result<Vec<StructureStart>>,
    ) -> io::Result<Starts> {
        let entry = {
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = entries.by_pos.get(&pos) {
                Arc::clone(entry)
            } else {
                if entries.by_pos.len() == self.capacity {
                    let oldest = entries.order.pop_front().expect("full cache");
                    entries.by_pos.remove(&oldest);
                }
                let entry = Arc::new(Mutex::new(None));
                entries.by_pos.insert(pos, Arc::clone(&entry));
                entries.order.push_back(pos);
                entry
            }
        };
        let mut starts = entry.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(starts) = &*starts {
            return Ok(Arc::clone(starts));
        }
        let loaded = Arc::new(load()?);
        *starts = Some(Arc::clone(&loaded));
        Ok(loaded)
    }
}

pub(super) struct Structures {
    state: StructureState,
    templates: Arc<StructureTemplates>,
    starts: StartCache,
}

pub(super) struct ChunkStructures {
    pub beardifier: Beardifier,
    pub starts: Vec<StructureStart>,
    pub neighbor_huts: Vec<StructureStart>,
    references: Compound,
}

impl ChunkStructures {
    pub(super) fn empty() -> Self {
        Self {
            beardifier: Beardifier::default(),
            starts: Vec::new(),
            neighbor_huts: Vec::new(),
            references: Compound::new(),
        }
    }

    pub(super) fn into_nbt(self) -> Compound {
        let starts: Compound = self
            .starts
            .iter()
            .map(|start| (start.structure.clone(), Nbt::Compound(start.to_nbt())))
            .collect();
        let mut tag = Compound::new();
        tag.insert("starts", starts);
        tag.insert("References", self.references);
        tag
    }
}

impl Structures {
    pub(super) fn new(
        seed: i64,
        data: &WorldgenData,
        generator: &NoiseBasedChunkGenerator,
        templates: Arc<StructureTemplates>,
    ) -> io::Result<Self> {
        Ok(Self {
            state: StructureState::new(seed, data, generator).map_err(io::Error::other)?,
            templates,
            starts: StartCache::new(4096),
        })
    }

    fn starts(
        &self,
        pos: ChunkPos,
        generator: &NoiseBasedChunkGenerator,
        data: &WorldgenData,
        read: &ChunkReader<'_>,
    ) -> io::Result<Starts> {
        self.starts.get(pos, || {
            if let Some(chunk) = read(pos)? {
                // An empty proto-chunk has not run STRUCTURE_STARTS yet.
                if chunk.get("Status") != Some(&Nbt::from("minecraft:empty")) {
                    return saved_starts(pos, &chunk, data, &self.templates);
                }
            }
            self.state
                .starts_in_chunk(generator, data, &self.templates, pos.x, pos.z)
                .map_err(io::Error::other)
        })
    }

    pub(super) fn for_chunk(
        &self,
        pos: ChunkPos,
        generator: &NoiseBasedChunkGenerator,
        data: &WorldgenData,
        read: &ChunkReader<'_>,
    ) -> io::Result<ChunkStructures> {
        let own = self.starts(pos, generator, data, read)?;
        let mut nearby = Vec::new();
        for x in pos.x - 8..=pos.x + 8 {
            for z in pos.z - 8..=pos.z + 8 {
                nearby.push(self.starts(ChunkPos { x, z }, generator, data, read)?);
            }
        }
        let min_x = pos.x * 16;
        let min_z = pos.z * 16;
        let referenced: Vec<_> = nearby
            .iter()
            .flat_map(|starts| starts.iter())
            .filter(|start| {
                start
                    .bounding_box
                    .intersects_columns(min_x, min_z, min_x + 15, min_z + 15)
            })
            .collect();
        let beardifier = Beardifier::for_starts(pos.x, pos.z, data, referenced.iter().copied())
            .map_err(io::Error::other)?;
        // New huts fit in their source chunk. Saved, already height-adjusted pieces can cross a boundary;
        // place their intersection here without moving or duplicating the source chunk's saved start.
        let neighbor_huts = referenced
            .iter()
            .filter(|s| s.chunk != (pos.x, pos.z) && s.structure == "minecraft:swamp_hut")
            .map(|s| (*s).clone())
            .collect();
        let mut references = Compound::new();
        for start in referenced {
            let packed = i64::from(start.chunk.0 as u32) | (i64::from(start.chunk.1 as u32) << 32);
            let mut sources = match references.remove(&start.structure) {
                Some(Nbt::LongArray(sources)) => sources,
                _ => Vec::new(),
            };
            sources.push(packed);
            references.insert(start.structure.clone(), Nbt::LongArray(sources));
        }
        Ok(ChunkStructures {
            beardifier,
            starts: own.as_ref().clone(),
            neighbor_huts,
            references,
        })
    }
}

fn saved_starts(
    pos: ChunkPos,
    chunk: &Compound,
    data: &WorldgenData,
    templates: &StructureTemplates,
) -> io::Result<Vec<StructureStart>> {
    let Some(Nbt::Compound(structures)) = chunk.get("structures") else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "saved neighbor has no structures compound",
        ));
    };
    // Earlier Harpas proto-chunks stored an empty structures compound.
    let Some(starts) = structures.get("starts") else {
        return Ok(Vec::new());
    };
    let Nbt::Compound(starts) = starts else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid saved structure starts",
        ));
    };
    let mut result = Vec::new();
    for (name, tag) in starts.iter() {
        let Nbt::Compound(tag) = tag else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid saved structure start",
            ));
        };
        if let Some(start) = StructureStart::from_nbt(tag, data, templates)? {
            if start.structure != name || start.chunk != (pos.x, pos.z) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "saved structure start has the wrong ID or chunk",
                ));
            }
            result.push(start);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Barrier,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn concurrent_workers_share_empty_starts_and_errors_can_retry() {
        let cache = StartCache::new(2);
        let calls = AtomicUsize::new(0);
        let barrier = Barrier::new(4);
        std::thread::scope(|scope| {
            let threads: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        cache
                            .get(ChunkPos { x: -1, z: 2 }, || {
                                calls.fetch_add(1, Ordering::SeqCst);
                                Ok(Vec::new())
                            })
                            .unwrap()
                    })
                })
                .collect();
            let starts: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
            assert!(starts.iter().all(|s| Arc::ptr_eq(s, &starts[0])));
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let failed = ChunkPos { x: 0, z: 0 };
        assert!(
            cache
                .get(failed, || Err(io::Error::other("read failed")))
                .is_err()
        );
        assert!(cache.get(failed, || Ok(Vec::new())).is_ok());
        cache
            .get(ChunkPos { x: 1, z: 0 }, || Ok(Vec::new()))
            .unwrap();
        assert_eq!(cache.entries.lock().unwrap().by_pos.len(), 2);
        cache
            .get(ChunkPos { x: -1, z: 2 }, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Vec::new())
            })
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
