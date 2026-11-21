//! Shared structure starts for worker-generated chunks. Saved neighbors take precedence over regeneration.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::sync::{Arc, Mutex};

use harpas_anvil::Chunk;
use harpas_core::nbt::{Compound, Nbt};
use harpas_worldgen::{
    Beardifier, GeneratedChunk, HeightmapType, NoiseBasedChunkGenerator, RuinedPortalBlocks,
    RuinedPortalGenerator, StructureStart, StructureState, StructureTemplates, WorldgenData,
};

use crate::world::{ChunkPos, ChunkReader};

type Starts = Arc<Vec<StructureStart>>;
type Entry<T> = Arc<Mutex<Option<Arc<T>>>>;
type StartCache = Cache<Vec<StructureStart>>;
type PortalPlans = Vec<RuinedPortalBlocks>;

struct Entries<T> {
    by_pos: HashMap<ChunkPos, Entry<T>>,
    order: VecDeque<ChunkPos>,
}

/// Bounds shared generation results; each entry has its own lock so unrelated sources run in parallel.
struct Cache<T> {
    capacity: usize,
    entries: Mutex<Entries<T>>,
}

impl<T> Cache<T> {
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

    fn get(&self, pos: ChunkPos, load: impl FnOnce() -> io::Result<T>) -> io::Result<Arc<T>> {
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
    seed: i64,
    /// The world's height in blocks.
    height: i32,
    state: StructureState,
    templates: Arc<StructureTemplates>,
    starts: StartCache,
    shipwreck_heights: StartCache,
    pyramid_heights: StartCache,
    portals: RuinedPortalGenerator,
    portal_blocks: Cache<PortalPlans>,
}

pub(super) struct ChunkStructures {
    pub beardifier: Beardifier,
    pub starts: Vec<StructureStart>,
    references: Compound,
}

impl ChunkStructures {
    pub(super) fn empty() -> Self {
        Self {
            beardifier: Beardifier::default(),
            starts: Vec::new(),
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
    pub(super) fn buried_treasure_at(
        &self,
        pos: ChunkPos,
        set: &str,
        generator: &NoiseBasedChunkGenerator,
        data: &WorldgenData,
        read: &ChunkReader<'_>,
    ) -> io::Result<bool> {
        let saved = read(pos)?;
        if let Some(saved) = &saved
            && saved.get("Status") != Some(&Nbt::from("minecraft:empty"))
        {
            return Ok(saved_starts(pos, saved, data, &self.templates)?
                .iter()
                .any(|start| start.structure == "minecraft:buried_treasure"));
        }
        if !self.state.is_structure_chunk(set, pos.x, pos.z) {
            return Ok(false);
        }
        Ok(self
            .starts(pos, generator, data, &|_| Ok(None))?
            .iter()
            .any(|start| start.structure == "minecraft:buried_treasure"))
    }

    pub(super) fn new(
        seed: i64,
        data: &WorldgenData,
        generator: &NoiseBasedChunkGenerator,
        templates: Arc<StructureTemplates>,
    ) -> io::Result<Self> {
        let height = data
            .noise_settings
            .get("minecraft:overworld")
            .map_or(384, |settings| settings.noise.height);
        Ok(Self {
            seed,
            height,
            state: StructureState::new(seed, data, generator).map_err(io::Error::other)?,
            templates,
            starts: StartCache::new(4096),
            shipwreck_heights: StartCache::new(4096),
            pyramid_heights: StartCache::new(4096),
            portals: RuinedPortalGenerator::new(data).map_err(io::Error::other)?,
            // Portal results include terrain writes, so retain fewer entries than the compact start records.
            portal_blocks: Cache::new(256),
        })
    }

    /// Gives the shipwrecks among `starts` their shared height: every chunk a shipwreck reaches places it at the
    /// height its source chunk chooses first in vanilla, from the terrain before decoration. Saved heights are
    /// kept. Returns the shipwrecks of `pos` whose placement there must still draw the burial offset.
    fn prepare_shipwrecks(
        &self,
        starts: &mut [StructureStart],
        pos: ChunkPos,
        generator: &NoiseBasedChunkGenerator,
        data: &WorldgenData,
        read: &ChunkReader<'_>,
    ) -> io::Result<Vec<String>> {
        let originals: Vec<_> = starts
            .iter()
            .filter(|s| is_shipwreck(&s.structure))
            .cloned()
            .collect();
        let mut burials = Vec::new();
        for start in starts.iter_mut().filter(|s| is_shipwreck(&s.structure)) {
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
                burials.push(start.structure.clone());
            }
            let prepared = self.shipwreck_heights.get(origin, || {
                let mut ships: Vec<_> = originals
                    .iter()
                    .filter(|s| s.chunk == start.chunk)
                    .cloned()
                    .collect();
                let terrain = self.terrain_around(pos, None, &ships, generator, data, read)?;
                for ship in &mut ships {
                    ship.prepare_shipwreck_height(self.seed, &self.templates, &mut |kind, x, z| {
                        terrain_height(&terrain, kind, x, z, generator.min_y())
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
        Ok(burials)
    }

    /// Places each ruined portal whose blocks reach `pos`, once for all the chunks it reaches, against the
    /// terrain before decoration. Returns each portal's structure with the placement.
    fn portal_plans(
        &self,
        portals: &[StructureStart],
        generator: &NoiseBasedChunkGenerator,
        data: &WorldgenData,
        read: &ChunkReader<'_>,
    ) -> io::Result<Vec<(String, Arc<PortalPlans>, usize)>> {
        let mut result = Vec::new();
        let mut sources = HashSet::new();
        for start in portals {
            if !sources.insert(start.chunk) {
                continue;
            }
            let origin = ChunkPos {
                x: start.chunk.0,
                z: start.chunk.1,
            };
            let owned: Vec<_> = portals.iter().filter(|s| s.chunk == start.chunk).collect();
            let plans = self.portal_blocks.get(origin, || {
                let mut terrain = HashMap::new();
                for portal in &owned {
                    let bounds = portal
                        .ruined_portal_terrain_bounds()
                        .expect("a ruined portal");
                    for x in bounds.min[0] >> 4..=bounds.max[0] >> 4 {
                        for z in bounds.min[2] >> 4..=bounds.max[2] >> 4 {
                            let pos = ChunkPos { x, z };
                            if let std::collections::hash_map::Entry::Vacant(entry) =
                                terrain.entry(pos)
                            {
                                entry.insert(self.terrain(
                                    pos,
                                    self.height,
                                    generator,
                                    data,
                                    read,
                                )?);
                            }
                        }
                    }
                }
                Ok(owned
                    .iter()
                    .map(|portal| {
                        self.portals.prepare(
                            portal,
                            self.seed,
                            &self.templates,
                            generator.min_y(),
                            self.height,
                            &mut |x, y, z| terrain_block(&terrain, x, y, z),
                        )
                    })
                    .collect())
            })?;
            for (index, portal) in owned.iter().enumerate() {
                result.push((portal.structure.clone(), Arc::clone(&plans), index));
            }
        }
        Ok(result)
    }

    /// The terrain that `starts` read around `pos` when placed there: strongholds orienting chests, and
    /// shipwrecks and ocean ruins fitting their templates and connecting their edges. `own` is the chunk at `pos`
    /// itself, when there is one; otherwise its terrain is loaded like the others.
    fn terrain_around(
        &self,
        pos: ChunkPos,
        own: Option<&GeneratedChunk>,
        starts: &[StructureStart],
        generator: &NoiseBasedChunkGenerator,
        data: &WorldgenData,
        read: &ChunkReader<'_>,
    ) -> io::Result<HashMap<ChunkPos, GeneratedChunk>> {
        let mut terrain_chunks = HashMap::new();
        for start in starts {
            let mut neighbors: Vec<_> = if start.structure == "minecraft:stronghold" {
                let Some(chunk) = own else {
                    continue;
                };
                start
                    .stronghold_chest_neighbors(chunk)
                    .into_iter()
                    .map(|[x, _, z]| ChunkPos {
                        x: x >> 4,
                        z: z >> 4,
                    })
                    .collect()
            } else {
                if !is_shipwreck(&start.structure) && !is_ocean_ruin(&start.structure) {
                    continue;
                }
                // Edge updates can inspect the neighbors of an existing block just outside the template,
                // requiring two columns beyond the piece bounds.
                let mut neighbors: Vec<_> = start
                    .pieces
                    .iter()
                    .flat_map(|piece| {
                        let b = piece.bounding_box;
                        ((b.min[0] - 2) >> 4..=(b.max[0] + 2) >> 4).flat_map(move |x| {
                            ((b.min[2] - 2) >> 4..=(b.max[2] + 2) >> 4)
                                .map(move |z| ChunkPos { x, z })
                        })
                    })
                    .collect();
                if is_shipwreck(&start.structure) {
                    neighbors.extend(start.shipwreck_height_samples(&self.templates).map(
                        |(x, z)| ChunkPos {
                            x: x >> 4,
                            z: z >> 4,
                        },
                    ));
                }
                neighbors
            };
            neighbors.sort_by_key(|p| (p.x, p.z));
            neighbors.dedup();
            for neighbor in neighbors {
                if terrain_chunks.contains_key(&neighbor) {
                    continue;
                }
                let terrain = match own {
                    Some(chunk) if neighbor == pos => chunk.clone(),
                    _ => self.terrain(neighbor, self.height, generator, data, read)?,
                };
                terrain_chunks.insert(neighbor, terrain);
            }
        }
        Ok(terrain_chunks)
    }

    /// A chunk's terrain for structure placement: its saved blocks where it has them, otherwise the carved
    /// terrain generated again.
    fn terrain(
        &self,
        pos @ ChunkPos { x, z }: ChunkPos,
        height: i32,
        generator: &NoiseBasedChunkGenerator,
        data: &WorldgenData,
        read: &ChunkReader<'_>,
    ) -> io::Result<GeneratedChunk> {
        if let Some(saved) = read(pos)?
            && saved.get("sections").is_some()
        {
            let names: Vec<_> = data.biomes.keys().map(String::as_str).collect();
            let (saved, _) = Chunk::read(saved, &names)?;
            let mut terrain =
                GeneratedChunk::new(x, z, generator.min_y(), height, data.biomes.len());
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
            return Ok(terrain);
        }
        let nearby = self.for_chunk(pos, generator, data, read)?;
        generator
            .generate_chunk_carved_with_beardifier(x, z, data, nearby.beardifier)
            .map_err(io::Error::other)
    }

    /// A desert pyramid with its shared height. Every chunk the pyramid reaches places it at the same height:
    /// the lowest `MOTION_BLOCKING_NO_LEAVES` height of its footprint on the terrain before decoration, as if
    /// vanilla's source chunk placed it first. A saved height is kept.
    fn prepared_pyramid(
        &self,
        start: StructureStart,
        generator: &NoiseBasedChunkGenerator,
        data: &WorldgenData,
        read: &ChunkReader<'_>,
    ) -> io::Result<StructureStart> {
        let unprepared = start.pieces.iter().any(|piece| {
            piece.kind == "minecraft:tedp"
                && !matches!(piece.save_data.get("HPos"), Some(Nbt::Int(y)) if *y >= 0)
        });
        if !unprepared {
            return Ok(start);
        }
        let origin = ChunkPos {
            x: start.chunk.0,
            z: start.chunk.1,
        };
        let prepared = self.pyramid_heights.get(origin, || {
            let b = start.bounding_box;
            let mut terrain = HashMap::new();
            for x in b.min[0] >> 4..=b.max[0] >> 4 {
                for z in b.min[2] >> 4..=b.max[2] >> 4 {
                    let pos = ChunkPos { x, z };
                    terrain.insert(pos, self.terrain(pos, self.height, generator, data, read)?);
                }
            }
            let mut pyramid = start.clone();
            pyramid.prepare_pyramid_height(
                self.seed,
                generator.min_y() + self.height - 1,
                &mut |x, z| {
                    terrain[&ChunkPos {
                        x: x >> 4,
                        z: z >> 4,
                    }]
                        .heightmaps()
                        .get(
                            HeightmapType::MotionBlockingNoLeaves,
                            (x & 15) as usize,
                            (z & 15) as usize,
                        )
                },
            );
            Ok(vec![pyramid])
        })?;
        Ok(prepared[0].clone())
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

    /// The starts whose bounds reach a chunk: those its structure references point to, which its decoration
    /// places. A new desert pyramid comes with its shared height.
    pub(super) fn referenced(
        &self,
        pos: ChunkPos,
        generator: &NoiseBasedChunkGenerator,
        data: &WorldgenData,
        read: &ChunkReader<'_>,
    ) -> io::Result<DecorationStarts> {
        let (min_x, min_z) = (pos.x * 16, pos.z * 16);
        let mut referenced = Vec::new();
        let mut portals = Vec::new();
        for x in pos.x - 8..=pos.x + 8 {
            for z in pos.z - 8..=pos.z + 8 {
                let starts = self.starts(ChunkPos { x, z }, generator, data, read)?;
                for start in starts.iter() {
                    if start
                        .bounding_box
                        .intersects_columns(min_x, min_z, min_x + 15, min_z + 15)
                    {
                        referenced.push(start.clone());
                    }
                    // A portal's spread and drip columns can reach chunks beyond its references.
                    if start.ruined_portal_terrain_bounds().is_some_and(|bounds| {
                        bounds.intersects_columns(min_x, min_z, min_x + 15, min_z + 15)
                    }) {
                        portals.push(start.clone());
                    }
                }
            }
        }
        let mut starts = referenced
            .into_iter()
            .map(|start| {
                if start.structure == "minecraft:desert_pyramid" {
                    self.prepared_pyramid(start, generator, data, read)
                } else {
                    Ok(start)
                }
            })
            .collect::<io::Result<Vec<_>>>()?;
        let initial_shipwreck_burials =
            self.prepare_shipwrecks(&mut starts, pos, generator, data, read)?;
        // Strongholds orient chests from the blocks around this chunk's own terrain before decoration.
        let own = if starts.iter().any(|s| s.structure == "minecraft:stronghold") {
            Some(self.terrain(pos, self.height, generator, data, read)?)
        } else {
            None
        };
        let terrain = self.terrain_around(pos, own.as_ref(), &starts, generator, data, read)?;
        let portals = self.portal_plans(&portals, generator, data, read)?;
        Ok(DecorationStarts {
            starts,
            terrain,
            portals,
            initial_shipwreck_burials,
            min_y: generator.min_y(),
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
            references,
        })
    }
}

/// The starts whose pieces reach a chunk, which its decoration places, with what they read around the chunk.
pub struct DecorationStarts {
    /// The starts, with the shared heights of new pyramids and shipwrecks fixed.
    pub starts: Vec<StructureStart>,
    /// The terrain before decoration that shipwrecks and ocean ruins read around the chunk.
    pub(crate) terrain: HashMap<ChunkPos, GeneratedChunk>,
    /// Ruined portals placed beforehand whose blocks reach the chunk: each portal's structure, the placements
    /// of its source chunk's portals and its own among them.
    pub(crate) portals: Vec<(String, Arc<PortalPlans>, usize)>,
    /// The shipwrecks of the chunk whose placement still draws the burial offset.
    pub(crate) initial_shipwreck_burials: Vec<String>,
    /// The world's lowest Y.
    pub(crate) min_y: i32,
}

impl DecorationStarts {
    /// Only these starts, without anything read around the chunk.
    pub fn new(starts: Vec<StructureStart>, min_y: i32) -> Self {
        Self {
            starts,
            terrain: HashMap::new(),
            portals: Vec::new(),
            initial_shipwreck_burials: Vec::new(),
            min_y,
        }
    }
}

fn is_shipwreck(structure: &str) -> bool {
    matches!(
        structure,
        "minecraft:shipwreck" | "minecraft:shipwreck_beached"
    )
}

fn is_ocean_ruin(structure: &str) -> bool {
    matches!(
        structure,
        "minecraft:ocean_ruin_cold" | "minecraft:ocean_ruin_warm"
    )
}

/// A block of loaded terrain; air where it holds no chunk.
pub(super) fn terrain_block(
    terrain: &HashMap<ChunkPos, GeneratedChunk>,
    x: i32,
    y: i32,
    z: i32,
) -> u32 {
    terrain
        .get(&ChunkPos {
            x: x >> 4,
            z: z >> 4,
        })
        .map_or(0, |chunk| {
            chunk.block_state((x & 15) as usize, y, (z & 15) as usize)
        })
}

/// A height of loaded terrain; the bottom of the world where it holds no chunk.
pub(super) fn terrain_height(
    terrain: &HashMap<ChunkPos, GeneratedChunk>,
    kind: HeightmapType,
    x: i32,
    z: i32,
    min_y: i32,
) -> i32 {
    terrain
        .get(&ChunkPos {
            x: x >> 4,
            z: z >> 4,
        })
        .map_or(min_y, |chunk| {
            chunk
                .heightmaps()
                .get(kind, (x & 15) as usize, (z & 15) as usize)
        })
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
