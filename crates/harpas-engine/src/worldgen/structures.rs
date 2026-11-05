//! Shared structure starts for worker-generated chunks. Saved neighbors take precedence over regeneration.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::sync::{Arc, Mutex};

use harpas_anvil::Chunk;
use harpas_core::nbt::{Compound, Nbt};
use harpas_worldgen::{
    Beardifier, GeneratedChunk, HeightmapType, NoiseBasedChunkGenerator, StructureStart,
    StructureState, StructureTemplates, WorldgenData,
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
    shipwreck_heights: StartCache,
}

pub(super) struct ChunkStructures {
    pub beardifier: Beardifier,
    pub starts: Vec<StructureStart>,
    pub neighbor_pieces: Vec<StructureStart>,
    references: Compound,
    initial_shipwreck_burials: HashSet<String>,
}

impl ChunkStructures {
    pub(super) fn empty() -> Self {
        Self {
            beardifier: Beardifier::default(),
            starts: Vec::new(),
            neighbor_pieces: Vec::new(),
            references: Compound::new(),
            initial_shipwreck_burials: HashSet::new(),
        }
    }

    pub(super) fn place_blocks(
        &mut self,
        generated: &mut GeneratedChunk,
        seed: i64,
        terrain_chunks: &HashMap<ChunkPos, GeneratedChunk>,
        templates: &StructureTemplates,
    ) -> Vec<Compound> {
        let height_at = |x: i32, z: i32| {
            terrain_chunks[&crate::world::ChunkPos {
                x: x >> 4,
                z: z >> 4,
            }]
                .heightmaps()
                .get(
                    HeightmapType::MotionBlockingNoLeaves,
                    (x & 15) as usize,
                    (z & 15) as usize,
                )
        };
        self.starts
            .iter_mut()
            .chain(self.neighbor_pieces.iter_mut())
            .flat_map(|start| {
                if matches!(
                    start.structure.as_str(),
                    "minecraft:shipwreck" | "minecraft:shipwreck_beached"
                ) {
                    return start.place_prepared_shipwreck_in_chunk(
                        generated,
                        seed,
                        templates,
                        &mut |kind, x, z| {
                            terrain_chunks[&ChunkPos {
                                x: x >> 4,
                                z: z >> 4,
                            }]
                                .heightmaps()
                                .get(
                                    kind,
                                    (x & 15) as usize,
                                    (z & 15) as usize,
                                )
                        },
                        &mut |x, y, z| {
                            terrain_chunks[&ChunkPos {
                                x: x >> 4,
                                z: z >> 4,
                            }]
                                .block_state(
                                    (x & 15) as usize,
                                    y,
                                    (z & 15) as usize,
                                )
                        },
                        self.initial_shipwreck_burials.contains(&start.structure),
                    );
                }
                if matches!(
                    start.structure.as_str(),
                    "minecraft:ocean_ruin_cold" | "minecraft:ocean_ruin_warm"
                ) {
                    return start.place_ocean_ruin_in_chunk(
                        generated,
                        seed,
                        templates,
                        &mut |kind, x, z| {
                            terrain_chunks[&ChunkPos {
                                x: x >> 4,
                                z: z >> 4,
                            }]
                                .heightmaps()
                                .get(
                                    kind,
                                    (x & 15) as usize,
                                    (z & 15) as usize,
                                )
                        },
                        &mut |x, y, z| {
                            terrain_chunks[&ChunkPos {
                                x: x >> 4,
                                z: z >> 4,
                            }]
                                .block_state(
                                    (x & 15) as usize,
                                    y,
                                    (z & 15) as usize,
                                )
                        },
                    );
                }
                if start.structure == "minecraft:igloo" {
                    return start.place_igloo_in_chunk(
                        generated,
                        seed,
                        templates,
                        &mut |x, z| {
                            terrain_chunks[&ChunkPos {
                                x: x >> 4,
                                z: z >> 4,
                            }]
                                .heightmaps()
                                .get(
                                    HeightmapType::WorldSurfaceWg,
                                    (x & 15) as usize,
                                    (z & 15) as usize,
                                )
                        },
                        &mut |x, y, z| {
                            terrain_chunks[&ChunkPos {
                                x: x >> 4,
                                z: z >> 4,
                            }]
                                .block_state(
                                    (x & 15) as usize,
                                    y,
                                    (z & 15) as usize,
                                )
                        },
                    );
                }
                start.prepare_pyramid_height(
                    seed,
                    generated.min_y() + generated.height() - 1,
                    &mut |x, z| height_at(x, z),
                );
                // These chunks remain at CARVERS until the entire feature stage is implemented. Vanilla
                // clears container positions on replay; retain the incoming flags so that it recreates the
                // containers with the same deterministic loot seeds when it resumes FEATURES.
                let deferred: Vec<_> = if matches!(
                    start.structure.as_str(),
                    "minecraft:desert_pyramid" | "minecraft:jungle_pyramid"
                ) {
                    start
                        .pieces
                        .iter()
                        .map(|piece| {
                            if piece.kind == "minecraft:tejp" {
                                [
                                    "placedMainChest",
                                    "placedHiddenChest",
                                    "placedTrap1",
                                    "placedTrap2",
                                ]
                                .map(str::to_owned)
                                .to_vec()
                            } else {
                                (0..4).map(|i| format!("hasPlacedChest{i}")).collect()
                            }
                            .into_iter()
                            .map(|key| {
                                let value = piece.save_data.get(&key).cloned();
                                (key, value)
                            })
                            .collect::<Vec<_>>()
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                let entities = start.place_in_chunk(
                    generated,
                    seed,
                    &mut |x, z| height_at(x, z),
                    &mut |x, y, z| {
                        terrain_chunks[&crate::world::ChunkPos {
                            x: x >> 4,
                            z: z >> 4,
                        }]
                            .block_state(
                                (x & 15) as usize,
                                y,
                                (z & 15) as usize,
                            )
                    },
                );
                for (piece, flags) in start.pieces.iter_mut().zip(deferred) {
                    for (key, value) in flags {
                        if let Some(value) = value {
                            piece.save_data.insert(key, value);
                        } else {
                            piece.save_data.remove(&key);
                        }
                    }
                }
                entities
            })
            .collect()
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
            shipwreck_heights: StartCache::new(4096),
        })
    }

    pub(super) fn prepare_shipwrecks(
        &self,
        placements: &mut ChunkStructures,
        pos: ChunkPos,
        seed: i64,
        terrain: &HashMap<ChunkPos, GeneratedChunk>,
    ) -> io::Result<()> {
        let originals: Vec<_> = placements
            .starts
            .iter()
            .chain(&placements.neighbor_pieces)
            .filter(|s| {
                matches!(
                    s.structure.as_str(),
                    "minecraft:shipwreck" | "minecraft:shipwreck_beached"
                )
            })
            .cloned()
            .collect();
        for start in placements
            .starts
            .iter_mut()
            .chain(&mut placements.neighbor_pieces)
        {
            if !matches!(
                start.structure.as_str(),
                "minecraft:shipwreck" | "minecraft:shipwreck_beached"
            ) {
                continue;
            }
            let origin = ChunkPos {
                x: start.chunk.0,
                z: start.chunk.1,
            };
            if origin == pos
                && start.pieces.iter().any(|p| {
                    p.save_data.get("isBeached") == Some(&Nbt::Byte(1))
                        && p.save_data.get("height_adjusted") != Some(&Nbt::Byte(1))
                })
            {
                placements
                    .initial_shipwreck_burials
                    .insert(start.structure.clone());
            }
            let prepared = self.shipwreck_heights.get(origin, || {
                let mut ships: Vec<_> = originals
                    .iter()
                    .filter(|s| s.chunk == start.chunk)
                    .cloned()
                    .collect();
                for ship in &mut ships {
                    ship.prepare_shipwreck_height(seed, &self.templates, &mut |kind, x, z| {
                        terrain[&ChunkPos {
                            x: x >> 4,
                            z: z >> 4,
                        }]
                            .heightmaps()
                            .get(
                                kind,
                                (x & 15) as usize,
                                (z & 15) as usize,
                            )
                    });
                }
                Ok(ships)
            })?;
            *start = prepared
                .iter()
                .find(|s| s.structure == start.structure)
                .expect("prepared shipwreck")
                .clone();
        }
        Ok(())
    }

    pub(super) fn terrain_for_placements(
        &self,
        chunk: &GeneratedChunk,
        placements: &ChunkStructures,
        generator: &NoiseBasedChunkGenerator,
        data: &WorldgenData,
        read: &ChunkReader<'_>,
    ) -> io::Result<HashMap<ChunkPos, GeneratedChunk>> {
        let mut terrain_chunks = HashMap::new();
        // Pyramids and ocean templates sample neighboring heights and blocks. Igloo ladders and
        // rotated shipwrecks can sample outside their piece bounds.
        // Resolve these reads before placing blocks, preferring saved terrain.
        for start in placements.starts.iter().chain(&placements.neighbor_pieces) {
            if !matches!(
                start.structure.as_str(),
                "minecraft:desert_pyramid"
                    | "minecraft:jungle_pyramid"
                    | "minecraft:igloo"
                    | "minecraft:shipwreck"
                    | "minecraft:shipwreck_beached"
                    | "minecraft:ocean_ruin_cold"
                    | "minecraft:ocean_ruin_warm"
            ) {
                continue;
            }
            let mut neighbors: Vec<_> = start
                .pieces
                .iter()
                .flat_map(|piece| {
                    // Shipwreck edge updates can inspect the neighbors of an existing block just
                    // outside the template, requiring two columns beyond the piece bounds.
                    let padding = match start.structure.as_str() {
                        "minecraft:shipwreck" | "minecraft:shipwreck_beached" => 2,
                        "minecraft:igloo" => 1,
                        "minecraft:ocean_ruin_cold" | "minecraft:ocean_ruin_warm" => 2,
                        _ => 0,
                    };
                    (((piece.bounding_box.min[0] - padding) >> 4)
                        ..=((piece.bounding_box.max[0] + padding) >> 4))
                        .flat_map(move |x| {
                            (((piece.bounding_box.min[2] - padding) >> 4)
                                ..=((piece.bounding_box.max[2] + padding) >> 4))
                                .map(move |z| ChunkPos { x, z })
                        })
                })
                .collect();
            if start.structure == "minecraft:igloo" {
                neighbors.extend(start.igloo_height_samples().map(|(x, z)| ChunkPos {
                    x: x >> 4,
                    z: z >> 4,
                }));
            }
            if matches!(
                start.structure.as_str(),
                "minecraft:shipwreck" | "minecraft:shipwreck_beached"
            ) {
                neighbors.extend(
                    start
                        .shipwreck_height_samples(&self.templates)
                        .map(|(x, z)| ChunkPos {
                            x: x >> 4,
                            z: z >> 4,
                        }),
                );
            }
            for neighbor @ ChunkPos { x, z } in neighbors {
                if terrain_chunks.contains_key(&neighbor) {
                    continue;
                }
                if neighbor
                    == (ChunkPos {
                        x: chunk.chunk_x(),
                        z: chunk.chunk_z(),
                    })
                {
                    terrain_chunks.insert(neighbor, chunk.clone());
                    continue;
                }
                if let Some(saved) = read(neighbor)?
                    && saved.get("sections").is_some()
                {
                    let names: Vec<_> = data.biomes.keys().map(String::as_str).collect();
                    let (saved, _) = Chunk::read(saved, &names)?;
                    let mut terrain = GeneratedChunk::new(
                        x,
                        z,
                        generator.min_y(),
                        chunk.height(),
                        data.biomes.len(),
                    );
                    for section in saved.sections {
                        if let Some(blocks) = section.section {
                            let index = i32::from(section.y) - terrain.min_section_y();
                            if let Some(target) = usize::try_from(index)
                                .ok()
                                .and_then(|i| terrain.sections_mut().get_mut(i))
                            {
                                *target = blocks;
                            }
                        }
                    }
                    terrain.recompute_heightmaps();
                    terrain_chunks.insert(neighbor, terrain);
                    continue;
                }
                let nearby = self.for_chunk(neighbor, generator, data, read)?;
                let terrain = generator
                    .generate_chunk_carved_with_beardifier(x, z, data, nearby.beardifier)
                    .map_err(io::Error::other)?;
                terrain_chunks.insert(neighbor, terrain);
            }
        }
        Ok(terrain_chunks)
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
        // Template hulls, pyramids and igloos can span several chunks; saved huts and temples can too.
        // Place their intersection without moving or duplicating the source chunk's saved start.
        let neighbor_pieces = referenced
            .iter()
            .filter(|s| {
                s.chunk != (pos.x, pos.z)
                    && matches!(
                        s.structure.as_str(),
                        "minecraft:swamp_hut"
                            | "minecraft:desert_pyramid"
                            | "minecraft:jungle_pyramid"
                            | "minecraft:igloo"
                            | "minecraft:shipwreck"
                            | "minecraft:shipwreck_beached"
                            | "minecraft:ocean_ruin_cold"
                            | "minecraft:ocean_ruin_warm"
                    )
            })
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
            neighbor_pieces,
            references,
            initial_shipwreck_burials: HashSet::new(),
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
