//! Loading and saving overworld chunks without doing disk I/O on the logic thread.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use harpas_anvil::{
    Chunk, Region, VersionError, check_chunk, dimension_dir, overworld_spawn, read_chunk,
    read_level, recover, region_of,
};
use harpas_core::nbt::{Compound, Nbt};
use log::{error, warn};

/// Keep disk requests bounded even when players move faster than the disk can respond.
const QUEUE_CAPACITY: usize = 64;
const MAX_WORKERS: usize = 4;
/// How long a changed chunk that left every view waits for the other chunks of its region to leave too.
/// Each save rewrites the whole region file, so they are written together.
const UNLOAD_SAVE_DELAY: Duration = Duration::from_secs(5);

/// Coordinates of a chunk column in the overworld.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ChunkPos {
    /// East-west chunk coordinate.
    pub x: i32,
    /// North-south chunk coordinate.
    pub z: i32,
}

impl ChunkPos {
    pub(crate) fn at(x: f64, z: f64) -> Self {
        Self {
            x: (x.floor() as i32) >> 4,
            z: (z.floor() as i32) >> 4,
        }
    }

    pub(crate) fn within(self, center: Self, distance: u8) -> bool {
        (self.x - center.x).abs() <= i32::from(distance)
            && (self.z - center.z).abs() <= i32::from(distance)
    }

    /// Nearest chunks first, in the square currently used by Java's view updates.
    pub(crate) fn view(self, distance: u8) -> Vec<Self> {
        let distance = i32::from(distance);
        let mut chunks = Vec::with_capacity(((distance * 2 + 1).pow(2)) as usize);
        chunks.push(self);
        for radius in 1..=distance {
            for x in -radius..=radius {
                chunks.push(Self {
                    x: self.x + x,
                    z: self.z - radius,
                });
                chunks.push(Self {
                    x: self.x + x,
                    z: self.z + radius,
                });
            }
            for z in (-radius + 1)..radius {
                chunks.push(Self {
                    x: self.x - radius,
                    z: self.z + z,
                });
                chunks.push(Self {
                    x: self.x + radius,
                    z: self.z + z,
                });
            }
        }
        chunks
    }
}

/// The result of requesting an existing chunk.
#[derive(Debug)]
pub enum ChunkState {
    /// Waiting for a worker or for a place in the bounded work queue.
    Loading,
    /// The chunk was loaded and is owned by the logic thread.
    Loaded(Arc<Chunk>),
    /// No chunk is stored at these coordinates and no generator was configured.
    Missing,
    /// The chunk exists or its region was accessed, but it could not be read.
    Failed(io::Error),
}

/// Generates a previously missing chunk on a chunk worker, outside the logic thread.
pub type ChunkGenerator = dyn Fn(ChunkPos, &ChunkReader<'_>) -> io::Result<Chunk> + Send + Sync;

/// Reads a saved neighbor under the same region locks as the chunk workers and saver.
pub type ChunkReader<'a> = dyn Fn(ChunkPos) -> io::Result<Option<Compound>> + 'a;

/// A chunk produced by a [`ChunkSource`].
pub struct Generated {
    /// The chunk, ready for the logic thread.
    pub chunk: Chunk,
    /// Whether it differs from the saved copy, so it must be saved again.
    pub changed: bool,
}

/// Produces the chunks a world does not have yet, or has only part way through generation, on the chunk
/// workers.
pub trait ChunkSource: Send + Sync {
    /// Whether a saved chunk with this `Status` must be passed to [`ChunkSource::generate`] instead of being
    /// used as saved.
    fn resumes(&self, status: &str) -> bool;

    /// Returns a chunk that no later generation step will change. `saved` is the chunk's saved copy, if any.
    fn generate(
        &self,
        pos: ChunkPos,
        saved: Option<Compound>,
        storage: &ChunkStorage,
    ) -> io::Result<Generated>;

    /// Saves any chunks the source holds in memory. Called on shutdown, once no worker is running.
    fn flush(&self, storage: &ChunkStorage) -> io::Result<()>;
}

/// A [`ChunkGenerator`] whose chunks are complete as generated.
struct GeneratorSource(Arc<ChunkGenerator>);

/// A source whose chunks are complete as `generator` returns them, and which resumes no saved chunk.
pub fn generator_source(generator: Arc<ChunkGenerator>) -> Arc<dyn ChunkSource> {
    Arc::new(GeneratorSource(generator))
}

impl ChunkSource for GeneratorSource {
    fn resumes(&self, _status: &str) -> bool {
        false
    }

    fn generate(
        &self,
        pos: ChunkPos,
        _saved: Option<Compound>,
        storage: &ChunkStorage,
    ) -> io::Result<Generated> {
        (self.0)(pos, &|source| storage.read(source)).map(|chunk| Generated {
            chunk,
            changed: true,
        })
    }

    fn flush(&self, _storage: &ChunkStorage) -> io::Result<()> {
        Ok(())
    }
}

/// One world's overworld region files, read and written under the locks shared by the chunk workers and
/// the saver.
pub struct ChunkStorage {
    dir: PathBuf,
    locks: RegionLocks,
    biomes: Vec<String>,
    safety: GenerationSafety,
}

impl ChunkStorage {
    pub(crate) fn new(dir: PathBuf, biomes: Vec<String>) -> Self {
        Self {
            dir,
            locks: RegionLocks::new(),
            biomes,
            safety: GenerationSafety::default(),
        }
    }

    fn read_unchecked(&self, pos: ChunkPos) -> io::Result<Option<Compound>> {
        let (x, z) = region_of(pos.x, pos.z);
        let _guard = self
            .locks
            .for_region(x, z)
            .read()
            .unwrap_or_else(|e| e.into_inner());
        read_chunk(&self.dir, pos.x, pos.z)
    }

    /// Reads a saved chunk and checks that its version is supported.
    pub fn read(&self, pos: ChunkPos) -> io::Result<Option<Compound>> {
        let chunk = self.read_unchecked(pos)?;
        if let Some(nbt) = &chunk {
            check_chunk(nbt).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        }
        Ok(chunk)
    }

    /// Decodes a saved chunk with the server's biome registry.
    pub fn decode(&self, nbt: Compound) -> io::Result<Chunk> {
        Chunk::read(nbt, &self.biome_names()).map(|(chunk, _)| chunk)
    }

    /// Writes chunks, one region file at a time.
    pub fn write(&self, chunks: &[(ChunkPos, &Chunk)]) -> io::Result<()> {
        let mut regions: BTreeMap<(i32, i32), Vec<(ChunkPos, &Chunk)>> = BTreeMap::new();
        for &(pos, chunk) in chunks {
            regions
                .entry(region_of(pos.x, pos.z))
                .or_default()
                .push((pos, chunk));
        }
        let biomes = self.biome_names();
        for ((x, z), chunks) in regions {
            let _guard = self
                .locks
                .for_region(x, z)
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let mut region = Region::open(&self.dir, x, z)?;
            for (pos, chunk) in chunks {
                region.set_chunk(pos.x, pos.z, &chunk.to_nbt(&biomes));
            }
            region.save()?;
        }
        Ok(())
    }

    /// Refuses to generate a chunk near one that needs terrain blending, which is not supported.
    pub fn check_generation(&self, pos: ChunkPos) -> io::Result<()> {
        self.safety.check(&self.dir, &self.locks, pos)
    }

    fn biome_names(&self) -> Vec<&str> {
        self.biomes.iter().map(String::as_str).collect()
    }
}

fn status(chunk: &Compound) -> Option<&str> {
    match chunk.get("Status") {
        Some(Nbt::String(status)) => Some(status),
        _ => None,
    }
}

type Loaded = io::Result<Option<(Chunk, Vec<String>, bool)>>;
type Load = dyn Fn(ChunkPos) -> Loaded + Send + Sync;

struct Completion {
    pos: ChunkPos,
    result: Loaded,
}

/// Fixed-size lock striping prevents reads from racing a region and its external chunk files while saving.
struct RegionLocks([RwLock<()>; 64]);

impl RegionLocks {
    fn new() -> Self {
        Self(std::array::from_fn(|_| RwLock::new(())))
    }

    fn for_region(&self, x: i32, z: i32) -> &RwLock<()> {
        let index = (x as u32).wrapping_mul(0x9e37_79b9) ^ (z as u32).wrapping_mul(0x85eb_ca6b);
        &self.0[index as usize % self.0.len()]
    }
}

/// Cache neighbor checks without holding a lock across disk reads or terrain generation.
#[derive(Default)]
struct GenerationSafety(Mutex<HashMap<ChunkPos, bool>>);

impl GenerationSafety {
    fn check(&self, dir: &Path, locks: &RegionLocks, target: ChunkPos) -> io::Result<()> {
        for dx in -8_i32..=8 {
            for dz in -8_i32..=8 {
                let pos = ChunkPos {
                    x: target.x.wrapping_add(dx),
                    z: target.z.wrapping_add(dz),
                };
                let cached = self
                    .0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&pos)
                    .copied();
                let needs_blending = if let Some(cached) = cached {
                    cached
                } else {
                    let (x, z) = region_of(pos.x, pos.z);
                    let nbt = {
                        let _guard = locks
                            .for_region(x, z)
                            .read()
                            .unwrap_or_else(|e| e.into_inner());
                        read_chunk(dir, pos.x, pos.z)?
                    };
                    let needs_blending = if let Some(nbt) = nbt {
                        check_chunk(&nbt)
                            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                        nbt.get("blending_data").is_some()
                            || nbt.get("below_zero_retrogen").is_some()
                    } else {
                        false
                    };
                    let mut cache = self.0.lock().unwrap_or_else(|e| e.into_inner());
                    if cache.len() >= 8_192 {
                        cache.clear();
                    }
                    cache.insert(pos, needs_blending);
                    needs_blending
                };
                if needs_blending {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        format!(
                            "chunk {}, {} needs terrain blending, which is not supported",
                            pos.x, pos.z
                        ),
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct Changes {
    revision: u64,
    saved: u64,
    in_flight: bool,
    requested: bool,
    /// When the chunk left every view with unsaved changes.
    unwanted_since: Option<Instant>,
}

struct Snapshot {
    pos: ChunkPos,
    revision: u64,
    chunk: Arc<Chunk>,
}

struct SaveJob {
    snapshots: Vec<Snapshot>,
}

struct Saved {
    job: SaveJob,
    result: io::Result<()>,
}

type Save = dyn Fn(&SaveJob) -> io::Result<()> + Send + Sync;

/// The logic thread's resident chunks and their asynchronous loader and saver.
/// Call [`World::flush`] before dropping a world whose chunks may have changed.
pub struct World {
    region_dir: PathBuf,
    spawn: [i32; 3],
    generates: bool,
    chunks: HashMap<ChunkPos, ChunkState>,
    in_flight: HashSet<ChunkPos>,
    changes: HashMap<ChunkPos, Changes>,
    /// The regions with a save queued or being written. A region has one save at a time, so the changes
    /// made meanwhile are written together by the next one.
    saving: HashSet<(i32, i32)>,
    unload_save_delay: Duration,
    source: Option<(Arc<dyn ChunkSource>, Arc<ChunkStorage>)>,
    loader: Loader,
    saver: Saver,
}

impl World {
    /// Opens the overworld in `world_dir`. `biomes` must be the server's biome registry in numeric ID order.
    /// Reading a region happens only after a chunk is requested by [`World::update`].
    pub fn open(world_dir: &Path, biomes: &[&str]) -> io::Result<Self> {
        Self::open_inner(world_dir, biomes, None)
    }

    /// Opens an overworld and generates absent chunks on the bounded chunk worker pool.
    /// Existing chunks are read unchanged, and newly generated chunks are saved on unload or flush.
    pub fn open_with_generator(
        world_dir: &Path,
        biomes: &[&str],
        generator: Arc<ChunkGenerator>,
    ) -> io::Result<Self> {
        Self::open_inner(
            world_dir,
            biomes,
            Some(Arc::new(GeneratorSource(generator))),
        )
    }

    /// Opens an overworld whose absent chunks, and chunks saved part way through generation, come from
    /// `source` on the bounded chunk worker pool. Chunks it returns are saved on unload or flush.
    pub fn open_with_source(
        world_dir: &Path,
        biomes: &[&str],
        source: Arc<dyn ChunkSource>,
    ) -> io::Result<Self> {
        Self::open_inner(world_dir, biomes, Some(source))
    }

    fn open_inner(
        world_dir: &Path,
        biomes: &[&str],
        generator: Option<Arc<dyn ChunkSource>>,
    ) -> io::Result<Self> {
        let spawn = read_level(world_dir)?
            .and_then(|(data, _)| overworld_spawn(&data))
            .unwrap_or([0, 65, 0]);
        if !biomes.contains(&"minecraft:plains") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the biome registry must include minecraft:plains",
            ));
        }
        let region_dir = dimension_dir(world_dir, "minecraft:overworld").join("region");
        // Saves interrupted by a crash are completed or discarded before any worker reads the regions.
        let recovery = recover(&region_dir)?;
        for path in &recovery.completed {
            warn!("Completed the interrupted save of {}", path.display());
        }
        for path in &recovery.discarded {
            warn!("Discarded {}, left by an interrupted save", path.display());
        }
        let storage = Arc::new(ChunkStorage::new(
            region_dir.clone(),
            biomes.iter().map(|name| (*name).to_owned()).collect(),
        ));
        let load_storage = Arc::clone(&storage);
        let generates = generator.is_some();
        let source = generator
            .clone()
            .map(|source| (source, Arc::clone(&storage)));
        let load = Arc::new(move |pos: ChunkPos| {
            let storage = &*load_storage;
            let nbt = storage.read_unchecked(pos)?;
            match (nbt, generator.as_ref()) {
                (None, Some(source)) => {
                    storage.check_generation(pos)?;
                    let generated = source.generate(pos, None, storage)?;
                    Ok(Some((generated.chunk, Vec::new(), generated.changed)))
                }
                (None, None) => Ok(None),
                (Some(nbt), Some(source)) if status(&nbt).is_some_and(|s| source.resumes(s)) => {
                    check_chunk(&nbt)
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                    let generated = source.generate(pos, Some(nbt), storage)?;
                    Ok(Some((generated.chunk, Vec::new(), generated.changed)))
                }
                (Some(nbt), _) => Chunk::read(nbt, &storage.biome_names())
                    .map(|(chunk, warnings)| Some((chunk, warnings, false))),
            }
        });
        let save = Arc::new(move |job: &SaveJob| {
            let chunks: Vec<_> = job
                .snapshots
                .iter()
                .map(|snapshot| (snapshot.pos, &*snapshot.chunk))
                .collect();
            storage.write(&chunks)
        });
        let workers = thread::available_parallelism()
            .map_or(1, usize::from)
            .min(MAX_WORKERS);
        Ok(Self {
            region_dir,
            spawn,
            generates,
            chunks: HashMap::new(),
            in_flight: HashSet::new(),
            changes: HashMap::new(),
            saving: HashSet::new(),
            unload_save_delay: UNLOAD_SAVE_DELAY,
            source,
            loader: Loader::start(workers, load)?,
            saver: Saver::start(save)?,
        })
    }

    pub(crate) fn spawn_position(&self) -> crate::player::Position {
        crate::player::Position {
            x: f64::from(self.spawn[0]) + 0.5,
            y: f64::from(self.spawn[1]),
            z: f64::from(self.spawn[2]) + 0.5,
            ..crate::player::Position::default()
        }
    }

    /// The current state of a chunk. A changed chunk stays resident until its pending save finishes.
    pub fn chunk(&self, pos: ChunkPos) -> Option<&ChunkState> {
        self.chunks.get(&pos)
    }

    /// Whether missing chunks are generated by this world.
    pub(crate) fn generates(&self) -> bool {
        self.generates
    }

    /// Whether the chunk was generated through carvers or features, without light, and still needs its light
    /// computed.
    pub(crate) fn needs_light(&self, pos: ChunkPos) -> bool {
        matches!(
            self.chunks.get(&pos),
            Some(ChunkState::Loaded(chunk))
                if matches!(
                    chunk.rest.get("Status"),
                    Some(Nbt::String(status))
                        if status == "minecraft:carvers" || status == "minecraft:features"
                )
        )
    }

    /// The eight neighbours of a loaded chunk, or `None` while one is still loading.
    pub(crate) fn light_neighbors(&self, pos: ChunkPos) -> Option<[Option<Arc<Chunk>>; 8]> {
        let mut neighbors: [Option<Arc<Chunk>>; 8] = std::array::from_fn(|_| None);
        for (index, (dx, dz)) in crate::player::NEIGHBORS.iter().enumerate() {
            let neighbor = ChunkPos {
                x: pos.x + dx,
                z: pos.z + dz,
            };
            let Some(ChunkState::Loaded(chunk)) = self.chunks.get(&neighbor) else {
                return None;
            };
            neighbors[index] = Some(Arc::clone(chunk));
        }
        Some(neighbors)
    }

    /// Edits a loaded chunk, marking its complete contents as changed for the next save.
    /// Previously published snapshots remain immutable while the edit uses copy-on-write.
    pub fn chunk_mut(&mut self, pos: ChunkPos) -> Option<&mut Chunk> {
        let ChunkState::Loaded(chunk) = self.chunks.get_mut(&pos)? else {
            return None;
        };
        let changes = self.changes.entry(pos).or_default();
        changes.revision = changes
            .revision
            .checked_add(1)
            .expect("chunk revision exhausted");
        Some(Arc::make_mut(chunk))
    }

    /// Requests an asynchronous save of all changed chunks without waiting for disk. If the bounded queue is
    /// full, later updates take the remaining snapshots as space becomes available.
    pub fn autosave(&mut self) -> io::Result<()> {
        for changes in self.changes.values_mut() {
            changes.requested = true;
        }
        self.queue_saves()
    }

    /// Waits for the chunk workers' current requests and for all changed chunks to reach disk, then saves the
    /// chunks the generator holds in memory. Intended for shutdown, not the tick loop.
    pub fn flush(&mut self) -> io::Result<()> {
        let loading: HashSet<_> = self.in_flight.clone();
        while !self.in_flight.is_empty() {
            let completion = self
                .loader
                .completed
                .recv()
                .map_err(|_| io::Error::other("chunk workers stopped"))?;
            self.finish_load(completion, &loading)?;
        }
        self.save_changes()?;
        if let Some((source, storage)) = &self.source {
            source.flush(storage)?;
        }
        Ok(())
    }

    fn save_changes(&mut self) -> io::Result<()> {
        while !self.changes.is_empty() {
            for changes in self.changes.values_mut() {
                changes.requested = true;
            }
            self.queue_saves()?;
            let completion = self
                .saver
                .completed
                .recv()
                .map_err(|_| io::Error::other("chunk save worker stopped"))?;
            self.finish_save(completion)?;
        }
        Ok(())
    }

    fn queue_saves(&mut self) -> io::Result<()> {
        let mut regions: BTreeMap<(i32, i32), Vec<ChunkPos>> = BTreeMap::new();
        for (&pos, changes) in &self.changes {
            if changes.requested && !changes.in_flight {
                regions
                    .entry(region_of(pos.x, pos.z))
                    .or_default()
                    .push(pos);
            }
        }
        for (region, positions) in regions {
            if self.saving.contains(&region) {
                continue;
            }
            let snapshots = positions
                .iter()
                .map(|pos| {
                    let ChunkState::Loaded(chunk) = &self.chunks[pos] else {
                        unreachable!("changed chunks remain loaded until saved");
                    };
                    Snapshot {
                        pos: *pos,
                        revision: self.changes[pos].revision,
                        chunk: Arc::clone(chunk),
                    }
                })
                .collect();
            let job = SaveJob { snapshots };
            match self
                .saver
                .requests
                .as_ref()
                .expect("saver is running")
                .try_send(job)
            {
                Ok(()) => {
                    self.saving.insert(region);
                    for pos in positions {
                        let changes = self.changes.get_mut(&pos).expect("changed chunk");
                        changes.in_flight = true;
                        changes.requested = false;
                    }
                }
                Err(TrySendError::Full(_)) => break,
                Err(TrySendError::Disconnected(_)) => {
                    return Err(io::Error::other("chunk save worker stopped"));
                }
            }
        }
        Ok(())
    }

    fn finish_save(&mut self, completion: Saved) -> io::Result<()> {
        let success = completion.result.is_ok();
        if let Some(snapshot) = completion.job.snapshots.first() {
            self.saving
                .remove(&region_of(snapshot.pos.x, snapshot.pos.z));
        }
        for snapshot in completion.job.snapshots {
            let changes = self
                .changes
                .get_mut(&snapshot.pos)
                .expect("saved chunk remains loaded");
            changes.in_flight = false;
            if success {
                changes.saved = snapshot.revision;
                if changes.revision == changes.saved {
                    self.changes.remove(&snapshot.pos);
                }
            }
        }
        completion.result.map_err(|error| {
            io::Error::new(error.kind(), format!("Cannot save the world: {error}"))
        })
    }

    /// Applies one finished request. A generated chunk is kept until it is saved even when it is no longer
    /// wanted: its neighbors may already depend on it, so generating it again could differ.
    fn finish_load(
        &mut self,
        completion: Completion,
        wanted: &HashSet<ChunkPos>,
    ) -> io::Result<()> {
        let pos = completion.pos;
        self.in_flight.remove(&pos);
        let prefix = format!(
            "chunk {}, {} in {}",
            pos.x,
            pos.z,
            self.region_dir.display()
        );
        let mut generated = false;
        let state = match completion.result {
            Ok(Some((chunk, warnings, is_generated))) => {
                generated = is_generated;
                for warning in warnings {
                    warn!("{prefix}: {warning}");
                }
                ChunkState::Loaded(Arc::new(chunk))
            }
            Ok(None) => ChunkState::Missing,
            Err(error) => {
                if error
                    .get_ref()
                    .is_some_and(|source| source.is::<VersionError>())
                {
                    return Err(io::Error::other(format!("Cannot load {prefix}: {error}")));
                }
                error!("Cannot load {prefix}: {error}");
                ChunkState::Failed(error)
            }
        };
        if generated {
            self.changes.insert(
                pos,
                Changes {
                    revision: 1,
                    ..Changes::default()
                },
            );
            self.chunks.insert(pos, state);
        } else if wanted.contains(&pos) {
            self.chunks.insert(pos, state);
        }
        Ok(())
    }

    /// Applies finished reads and writes, saves the changed chunks that left every view, one region at a
    /// time, and queues requests without waiting for disk.
    /// An incompatible chunk or an unavailable worker pool is fatal and returned to the caller, even if the
    /// incompatible chunk is no longer wanted when its read finishes.
    pub fn update(&mut self, wanted: impl IntoIterator<Item = ChunkPos>) -> io::Result<()> {
        let mut wanted_set = HashSet::new();
        let wanted: Vec<_> = wanted
            .into_iter()
            .filter(|pos| wanted_set.insert(*pos))
            .collect();
        loop {
            match self.saver.completed.try_recv() {
                Ok(completion) => self.finish_save(completion)?,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err(io::Error::other("chunk save worker stopped"));
                }
            }
        }
        self.chunks
            .retain(|pos, _| wanted_set.contains(pos) || self.changes.contains_key(pos));
        // A region is saved once its first unwanted chunk has waited long enough, with every chunk that
        // left the views since.
        let now = Instant::now();
        let mut due = HashSet::new();
        for (pos, changes) in &mut self.changes {
            if wanted_set.contains(pos) {
                changes.unwanted_since = None;
            } else if now.duration_since(*changes.unwanted_since.get_or_insert(now))
                >= self.unload_save_delay
            {
                due.insert(region_of(pos.x, pos.z));
            }
        }
        for (pos, changes) in &mut self.changes {
            if changes.unwanted_since.is_some() && due.contains(&region_of(pos.x, pos.z)) {
                changes.requested = true;
            }
        }
        self.queue_saves()?;
        loop {
            match self.loader.completed.try_recv() {
                Ok(completion) => self.finish_load(completion, &wanted_set)?,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err(io::Error::other("chunk workers stopped"));
                }
            }
        }
        for &pos in &wanted {
            self.chunks.entry(pos).or_insert(ChunkState::Loading);
        }
        for pos in wanted {
            let state = &self.chunks[&pos];
            if !matches!(state, ChunkState::Loading) || self.in_flight.contains(&pos) {
                continue;
            }
            match self
                .loader
                .requests
                .as_ref()
                .expect("loader is running")
                .try_send(pos)
            {
                Ok(()) => {
                    self.in_flight.insert(pos);
                }
                Err(TrySendError::Full(_)) => break,
                Err(TrySendError::Disconnected(_)) => {
                    return Err(io::Error::other("chunk workers stopped"));
                }
            }
        }
        Ok(())
    }
}

/// A single writer preserves the order of saves and prevents two region rewrites from losing each other.
struct Saver {
    requests: Option<SyncSender<SaveJob>>,
    completed: Receiver<Saved>,
    thread: Option<JoinHandle<()>>,
}

impl Saver {
    fn start(save: Arc<Save>) -> io::Result<Self> {
        let (requests, receiver) = mpsc::sync_channel::<SaveJob>(QUEUE_CAPACITY);
        let (sender, completed) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("harpas-chunk-save".to_owned())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    let result =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| save(&job)))
                            .unwrap_or_else(|_| {
                                Err(io::Error::other("chunk save worker panicked"))
                            });
                    if sender.send(Saved { job, result }).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            requests: Some(requests),
            completed,
            thread: Some(thread),
        })
    }
}

impl Drop for Saver {
    fn drop(&mut self) {
        self.requests.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Loader {
    requests: Option<SyncSender<ChunkPos>>,
    completed: Receiver<Completion>,
    stopping: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Loader {
    fn start(workers: usize, load: Arc<Load>) -> io::Result<Self> {
        let (requests, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let receiver = Arc::new(Mutex::new(receiver));
        let (sender, completed) = mpsc::channel();
        let stopping = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::with_capacity(workers);
        for index in 0..workers {
            let receiver = Arc::clone(&receiver);
            let sender = sender.clone();
            let load = Arc::clone(&load);
            let stopping = Arc::clone(&stopping);
            let thread = thread::Builder::new()
                .name(format!("harpas-chunk-{index}"))
                .spawn(move || {
                    loop {
                        let request = receiver.lock().unwrap_or_else(|e| e.into_inner()).recv();
                        let Ok(pos) = request else { break };
                        if stopping.load(Ordering::Relaxed) {
                            break;
                        }
                        // One malformed chunk must not silently kill a worker and leave its request in flight.
                        let result =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| load(pos)))
                                .unwrap_or_else(|_| Err(io::Error::other("chunk worker panicked")));
                        if sender.send(Completion { pos, result }).is_err() {
                            break;
                        }
                    }
                });
            match thread {
                Ok(thread) => threads.push(thread),
                Err(error) => {
                    drop(requests);
                    for thread in threads {
                        let _ = thread.join();
                    }
                    return Err(error);
                }
            }
        }
        Ok(Self {
            requests: Some(requests),
            completed,
            stopping,
            threads,
        })
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        self.requests.take();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Condvar, mpsc};

    use harpas_anvil::{ChunkSection, DATA_VERSION, Region, region_of};
    use harpas_core::block::block_states;
    use harpas_core::chunk::{PalettedContainer, Section, Strategy};
    use harpas_core::nbt::{Compound, Nbt};

    use super::*;
    use crate::player::{GameMode, Input, Movement, Output, Profile};
    use crate::{Event, Game, tick};

    const BIOMES: &[&str] = &["minecraft:plains", "minecraft:forest"];

    struct Folder(PathBuf);

    impl Folder {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "harpas-engine-world-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn region_dir(&self) -> PathBuf {
            dimension_dir(&self.0, "minecraft:overworld").join("region")
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn chunk(pos: ChunkPos, version: i32) -> Chunk {
        let mut rest = Compound::new();
        rest.insert("DataVersion", version);
        rest.insert("xPos", pos.x);
        rest.insert("zPos", pos.z);
        rest.insert("Status", "minecraft:full");
        let states = block_states();
        let blocks = PalettedContainer::filled(
            Strategy::block_states(states.global_bits()),
            states.default_state("minecraft:stone").unwrap(),
        );
        let biomes = PalettedContainer::filled(Strategy::biomes(1), 1);
        let mut section_rest = Compound::new();
        section_rest.insert("SkyLight", Nbt::ByteArray(vec![-1; 2048]));
        Chunk {
            sections: vec![ChunkSection {
                y: -4,
                section: Some(Section::new(blocks, biomes)),
                rest: section_rest,
            }],
            rest,
        }
    }

    fn save(folder: &Folder, chunks: &[(ChunkPos, i32)]) {
        fs::create_dir_all(folder.region_dir()).unwrap();
        for &(pos, version) in chunks {
            let (x, z) = region_of(pos.x, pos.z);
            let mut region = Region::open(&folder.region_dir(), x, z).unwrap();
            region.set_chunk(pos.x, pos.z, &chunk(pos, version).to_nbt(BIOMES));
            region.save().unwrap();
        }
    }

    fn wait_until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for chunk workers"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn fake_world(workers: usize, load: Arc<Load>) -> World {
        fake_world_with_save(workers, load, Arc::new(|_| Ok(())))
    }

    fn fake_world_with_save(workers: usize, load: Arc<Load>, save: Arc<Save>) -> World {
        World {
            region_dir: PathBuf::from("test"),
            spawn: [0, 65, 0],
            generates: false,
            chunks: HashMap::new(),
            in_flight: HashSet::new(),
            changes: HashMap::new(),
            saving: HashSet::new(),
            unload_save_delay: Duration::ZERO,
            source: None,
            loader: Loader::start(workers, load).unwrap(),
            saver: Saver::start(save).unwrap(),
        }
    }

    fn load_chunks(world: &mut World, wanted: &[ChunkPos]) {
        wait_until(|| {
            world.update(wanted.iter().copied()).unwrap();
            wanted
                .iter()
                .all(|pos| matches!(world.chunk(*pos), Some(ChunkState::Loaded(_))))
        });
    }

    fn profile() -> Profile {
        Profile {
            uuid: 1,
            name: "Steve".into(),
            properties: Vec::new(),
            game_mode: GameMode::Creative,
        }
    }

    #[test]
    fn view_is_a_nearest_first_square_even_at_negative_coordinates() {
        let center = ChunkPos { x: -1, z: -2 };
        let view = center.view(2);
        assert_eq!(view.len(), 25);
        assert_eq!(view[0], center);
        assert_eq!(view.iter().copied().collect::<HashSet<_>>().len(), 25);
        assert!(view.iter().all(|pos| pos.within(center, 2)));
        let distance = |pos: &ChunkPos| (pos.x - center.x).abs().max((pos.z - center.z).abs());
        assert!(
            view.windows(2)
                .all(|pair| distance(&pair[0]) <= distance(&pair[1]))
        );
    }

    #[test]
    fn client_view_distance_applies_from_join_without_lowering_the_server_maximum() {
        let folder = Folder::new();
        let (output, _received) = mpsc::channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: profile(),
                view_distance: 3,
                initial_view_distance: 2,
                allow_flight: false,
                output,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            stopped: false,
        };
        tick(&mut game, 1);
        tick(&mut game, 2);
        assert_eq!(game.players[0].view_distance, 2);
        assert_eq!(game.players[0].max_view_distance, 3);
        let world = game.world.as_ref().unwrap();
        assert_eq!(world.chunks.len(), 25);
        assert!(world.chunk(ChunkPos { x: 3, z: 0 }).is_none());
    }

    #[test]
    fn joining_players_start_at_the_saved_overworld_spawn() {
        let folder = Folder::new();
        let mut spawn = Compound::new();
        spawn.insert("dimension", "minecraft:overworld");
        spawn.insert("pos", Nbt::IntArray(vec![-32, 71, 8]));
        let mut level = Compound::new();
        level.insert("DataVersion", DATA_VERSION);
        level.insert("spawn", spawn);
        harpas_anvil::write_level(&folder.0, &level).unwrap();
        let (output, _received) = mpsc::channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: profile(),
                view_distance: 2,
                initial_view_distance: 2,
                allow_flight: false,
                output,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            stopped: false,
        };
        tick(&mut game, 1);
        let position = game.players[0].player.position;
        assert_eq!([position.x, position.y, position.z], [-31.5, 71.0, 8.5]);
    }

    #[test]
    fn reads_existing_chunks_and_distinguishes_missing_and_corrupt_entries() {
        let folder = Folder::new();
        let existing = ChunkPos { x: -1, z: -33 };
        let corrupt = ChunkPos { x: -2, z: -33 };
        let missing = ChunkPos { x: -3, z: -33 };
        save(
            &folder,
            &[(existing, DATA_VERSION), (corrupt, DATA_VERSION)],
        );
        let path = folder.region_dir().join("r.-1.-2.mca");
        let mut data = fs::read(&path).unwrap();
        let slot = ((corrupt.x & 31) + (corrupt.z & 31) * 32) as usize * 4;
        data[slot..slot + 4].copy_from_slice(&0x00000101_u32.to_be_bytes());
        fs::write(path, data).unwrap();

        let mut world = World::open(&folder.0, BIOMES).unwrap();
        let wanted = [existing, corrupt, missing];
        wait_until(|| {
            world.update(wanted).unwrap();
            wanted.iter().all(|pos| {
                world
                    .chunk(*pos)
                    .is_some_and(|state| !matches!(state, ChunkState::Loading))
            })
        });
        let Some(ChunkState::Loaded(chunk)) = world.chunk(existing) else {
            panic!("existing chunk was not loaded");
        };
        let section = chunk.sections[0].section.as_ref().unwrap();
        assert_eq!(
            section.block_state(0, 0, 0),
            block_states().default_state("minecraft:stone").unwrap()
        );
        assert_eq!(section.biome(0, 0, 0), 1);
        assert!(chunk.sections[0].rest.get("SkyLight").is_some());
        assert_eq!(
            chunk.rest.get("Status"),
            Some(&Nbt::String("minecraft:full".into()))
        );
        assert!(matches!(world.chunk(missing), Some(ChunkState::Missing)));
        assert!(matches!(world.chunk(corrupt), Some(ChunkState::Failed(_))));
    }

    #[test]
    fn generates_only_absent_chunks_on_workers_and_persists_them() {
        let folder = Folder::new();
        let existing = ChunkPos { x: 0, z: 0 };
        let corrupt = ChunkPos { x: 33, z: 0 };
        let truncated = ChunkPos { x: 64, z: 0 };
        let missing = ChunkPos { x: 2, z: 0 };
        let failed = ChunkPos { x: 3, z: 0 };
        save(
            &folder,
            &[(existing, DATA_VERSION), (corrupt, DATA_VERSION)],
        );
        let path = folder.region_dir().join("r.1.0.mca");
        let mut data = fs::read(&path).unwrap();
        data[4..8].copy_from_slice(&0x00000101_u32.to_be_bytes());
        fs::write(path, data).unwrap();
        fs::write(folder.region_dir().join("r.2.0.mca"), b"broken").unwrap();

        let calls = Arc::new(Mutex::new(Vec::new()));
        let generator = {
            let calls = Arc::clone(&calls);
            Arc::new(move |pos, read: &ChunkReader<'_>| {
                let saved = read(existing)?.expect("the saved neighbor is available to generation");
                assert_eq!(saved.get("xPos"), Some(&Nbt::Int(existing.x)));
                let thread = thread::current();
                assert!(thread.name().unwrap().starts_with("harpas-chunk-"));
                calls.lock().unwrap().push(pos);
                if pos == failed {
                    return Err(io::Error::other("generation failed"));
                }
                let mut generated = chunk(pos, DATA_VERSION);
                generated.rest.insert("GeneratedMarker", 42);
                Ok(generated)
            })
        };
        let mut world = World::open_with_generator(&folder.0, BIOMES, generator).unwrap();
        let wanted = [existing, corrupt, truncated, missing, failed];
        wait_until(|| {
            world.update(wanted).unwrap();
            wanted.iter().all(|pos| {
                world
                    .chunk(*pos)
                    .is_some_and(|state| !matches!(state, ChunkState::Loading))
            })
        });
        assert!(matches!(world.chunk(existing), Some(ChunkState::Loaded(_))));
        assert!(matches!(world.chunk(corrupt), Some(ChunkState::Failed(_))));
        assert!(matches!(
            world.chunk(truncated),
            Some(ChunkState::Failed(_))
        ));
        assert!(matches!(world.chunk(failed), Some(ChunkState::Failed(_))));
        assert!(world.changes.contains_key(&missing));
        assert!(!world.changes.contains_key(&existing));
        let mut generated_positions = calls.lock().unwrap().clone();
        generated_positions.sort_by_key(|pos| pos.x);
        assert_eq!(generated_positions, vec![missing, failed]);
        world.flush().unwrap();
        drop(world);

        let mut reopened = World::open(&folder.0, BIOMES).unwrap();
        load_chunks(&mut reopened, &[existing, missing]);
        let Some(ChunkState::Loaded(chunk)) = reopened.chunk(missing) else {
            panic!("generated chunk was not persisted");
        };
        assert_eq!(chunk.rest.get("GeneratedMarker"), Some(&Nbt::Int(42)));
        let Some(ChunkState::Loaded(chunk)) = reopened.chunk(existing) else {
            panic!("existing chunk was not loaded");
        };
        assert!(chunk.rest.get("GeneratedMarker").is_none());
    }

    #[test]
    fn unwanted_generated_results_are_saved_before_they_are_dropped() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (started, received) = mpsc::channel();
        let load = {
            let gate = Arc::clone(&gate);
            Arc::new(move |pos| {
                started.send(()).unwrap();
                let (lock, signal) = &*gate;
                let guard = lock.lock().unwrap();
                let _ = signal
                    .wait_timeout_while(guard, Duration::from_secs(3), |open| !*open)
                    .unwrap();
                Ok(Some((chunk(pos, DATA_VERSION), Vec::new(), true)))
            })
        };
        let saved = Arc::new(Mutex::new(Vec::new()));
        let save = {
            let saved = Arc::clone(&saved);
            Arc::new(move |job: &SaveJob| {
                saved
                    .lock()
                    .unwrap()
                    .extend(job.snapshots.iter().map(|snapshot| snapshot.pos));
                Ok(())
            })
        };
        let mut world = fake_world_with_save(1, load, save);
        let pos = ChunkPos { x: 0, z: 0 };
        world.update([pos]).unwrap();
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        world.update([]).unwrap();
        let (lock, signal) = &*gate;
        *lock.lock().unwrap() = true;
        signal.notify_all();
        // Its neighbors may already depend on it, so it is saved rather than generated again later.
        wait_until(|| {
            world.update([]).unwrap();
            world.in_flight.is_empty() && world.changes.is_empty()
        });
        world.update([]).unwrap();
        assert!(world.chunk(pos).is_none());
        assert_eq!(*saved.lock().unwrap(), vec![pos]);
    }

    #[test]
    fn generation_refuses_neighbors_that_require_terrain_blending() {
        let folder = Folder::new();
        let old = ChunkPos { x: 0, z: 0 };
        let missing = ChunkPos { x: 1, z: 0 };
        let mut previous = chunk(old, DATA_VERSION);
        previous.rest.insert("blending_data", Compound::new());
        let mut region = Region::open(&folder.region_dir(), 0, 0).unwrap();
        region.set_chunk(old.x, old.z, &previous.to_nbt(BIOMES));
        region.save().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let generator = {
            let calls = Arc::clone(&calls);
            Arc::new(move |pos, _: &ChunkReader<'_>| {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(chunk(pos, DATA_VERSION))
            })
        };
        let mut world = World::open_with_generator(&folder.0, BIOMES, generator).unwrap();
        wait_until(|| {
            world.update([missing]).unwrap();
            matches!(world.chunk(missing), Some(ChunkState::Failed(_)))
        });
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(world.changes.is_empty());
    }

    #[test]
    fn opening_completes_interrupted_saves_before_reading() {
        let folder = Folder::new();
        let pos = ChunkPos { x: 1, z: 2 };
        save(&folder, &[(pos, DATA_VERSION)]);
        // A save that crashed right after its commit: the new region is still staged under its journal.
        let dir = folder.region_dir();
        fs::rename(dir.join("r.0.0.mca"), dir.join("r.0.0.mca.tmp")).unwrap();
        fs::write(
            dir.join("r.0.0.mca.journal"),
            "harpas region journal 1\ninstall r.0.0.mca\nend\n",
        )
        .unwrap();
        fs::write(dir.join("c.5.5.mcc.tmp"), b"uncommitted").unwrap();

        let mut world = World::open(&folder.0, BIOMES).unwrap();
        load_chunks(&mut world, &[pos]);
        let mut names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["r.0.0.mca"]);
    }

    #[test]
    fn parallel_reads_do_not_block_updates_and_stale_results_are_discarded() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (started, received) = mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let load = {
            let gate = Arc::clone(&gate);
            let calls = Arc::clone(&calls);
            Arc::new(move |pos| {
                calls.fetch_add(1, Ordering::Relaxed);
                started
                    .send(thread::current().name().unwrap().to_owned())
                    .unwrap();
                let (lock, signal) = &*gate;
                let guard = lock.lock().unwrap();
                let _ = signal
                    .wait_timeout_while(guard, Duration::from_secs(3), |open| !*open)
                    .unwrap();
                Ok(Some((chunk(pos, DATA_VERSION), Vec::new(), false)))
            })
        };
        let mut world = fake_world(2, load);
        let a = ChunkPos { x: 0, z: 0 };
        let b = ChunkPos { x: 1, z: 0 };
        world.update([a, a, b]).unwrap();
        let first = received.recv_timeout(Duration::from_secs(2)).unwrap();
        let second = received.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_ne!(
            first, second,
            "both workers should be able to read in parallel"
        );
        world.update([a, b]).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert!(matches!(world.chunk(a), Some(ChunkState::Loading)));

        // A player leaves while one read is still in progress.
        world.update([b]).unwrap();
        assert!(world.chunk(a).is_none());
        let (lock, signal) = &*gate;
        *lock.lock().unwrap() = true;
        signal.notify_all();
        wait_until(|| {
            world.update([b]).unwrap();
            world.in_flight.is_empty()
        });
        assert!(world.chunk(a).is_none());
        assert!(matches!(world.chunk(b), Some(ChunkState::Loaded(_))));

        wait_until(|| {
            world.update([a, b]).unwrap();
            matches!(world.chunk(a), Some(ChunkState::Loaded(_)))
        });
        assert_eq!(calls.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn full_work_queue_is_bounded_and_retries_every_wanted_chunk() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (started, received) = mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let load = {
            let gate = Arc::clone(&gate);
            let calls = Arc::clone(&calls);
            Arc::new(move |_| {
                calls.fetch_add(1, Ordering::Relaxed);
                let _ = started.send(());
                let (lock, signal) = &*gate;
                let guard = lock.lock().unwrap();
                let _ = signal
                    .wait_timeout_while(guard, Duration::from_secs(3), |open| !*open)
                    .unwrap();
                Ok(None)
            })
        };
        let mut world = fake_world(1, load);
        let wanted: Vec<_> = (0..80).map(|x| ChunkPos { x, z: 0 }).collect();
        world.update(wanted.iter().copied()).unwrap();
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(world.in_flight.len() <= QUEUE_CAPACITY + 1);
        assert!(
            wanted
                .iter()
                .all(|pos| matches!(world.chunk(*pos), Some(ChunkState::Loading)))
        );
        let (lock, signal) = &*gate;
        *lock.lock().unwrap() = true;
        signal.notify_all();
        wait_until(|| {
            world.update(wanted.iter().copied()).unwrap();
            wanted
                .iter()
                .all(|pos| matches!(world.chunk(*pos), Some(ChunkState::Missing)))
        });
        assert_eq!(calls.load(Ordering::Relaxed), wanted.len());
    }

    #[test]
    fn snapshots_do_not_block_edits_or_lose_changes_during_unload() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (started, received) = mpsc::channel();
        let writes = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let load = {
            let calls = Arc::clone(&calls);
            Arc::new(move |pos| {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(Some((chunk(pos, DATA_VERSION), Vec::new(), false)))
            })
        };
        let save = {
            let gate = Arc::clone(&gate);
            let writes = Arc::clone(&writes);
            Arc::new(move |job: &SaveJob| {
                assert_eq!(thread::current().name(), Some("harpas-chunk-save"));
                let marker = job.snapshots[0].chunk.rest.get("Marker").cloned();
                started.send(()).unwrap();
                let (lock, signal) = &*gate;
                let guard = lock.lock().unwrap();
                let _ = signal
                    .wait_timeout_while(guard, Duration::from_secs(5), |open| !*open)
                    .unwrap();
                writes.lock().unwrap().push(marker);
                Ok(())
            })
        };
        let mut world = fake_world_with_save(1, load, save);
        let pos = ChunkPos { x: 0, z: 0 };
        load_chunks(&mut world, &[pos]);
        world.chunk_mut(pos).unwrap().rest.insert("Marker", 1);
        world.autosave().unwrap();
        received.recv_timeout(Duration::from_secs(2)).unwrap();

        // The first snapshot remains unchanged while the worker is blocked and the player leaves and returns.
        let before = Instant::now();
        world.chunk_mut(pos).unwrap().rest.insert("Marker", 2);
        world.update([]).unwrap();
        world.update([pos]).unwrap();
        assert!(before.elapsed() < Duration::from_secs(1));
        let Some(ChunkState::Loaded(current)) = world.chunk(pos) else {
            panic!("a changed chunk must remain loaded until it is saved");
        };
        assert_eq!(current.rest.get("Marker"), Some(&Nbt::Int(2)));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        world.update([]).unwrap();
        let (lock, signal) = &*gate;
        *lock.lock().unwrap() = true;
        signal.notify_all();
        wait_until(|| {
            world.update([]).unwrap();
            world.chunk(pos).is_none()
        });
        assert_eq!(
            *writes.lock().unwrap(),
            vec![Some(Nbt::Int(1)), Some(Nbt::Int(2))]
        );
    }

    #[test]
    fn chunks_leaving_the_views_are_saved_together_and_a_region_has_one_save_at_a_time() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let jobs = Arc::new(Mutex::new(Vec::new()));
        let load = Arc::new(|pos| Ok(Some((chunk(pos, DATA_VERSION), Vec::new(), false))));
        let save = {
            let gate = Arc::clone(&gate);
            let jobs = Arc::clone(&jobs);
            Arc::new(move |job: &SaveJob| {
                let mut saved: Vec<_> = job.snapshots.iter().map(|snapshot| snapshot.pos).collect();
                saved.sort_by_key(|pos| pos.x);
                jobs.lock().unwrap().push(saved);
                let (lock, signal) = &*gate;
                let guard = lock.lock().unwrap();
                let _ = signal
                    .wait_timeout_while(guard, Duration::from_secs(5), |open| !*open)
                    .unwrap();
                Ok(())
            })
        };
        let mut world = fake_world_with_save(1, load, save);
        world.unload_save_delay = Duration::from_secs(3600);
        let positions: Vec<_> = (0..4).map(|x| ChunkPos { x, z: 0 }).collect();
        load_chunks(&mut world, &positions);
        for pos in &positions {
            world.chunk_mut(*pos).unwrap().rest.insert("Marker", 1);
        }

        // Two chunks leave the views one after the other and wait, still resident, for the delay.
        world.update(positions[1..].iter().copied()).unwrap();
        world.update(positions[2..].iter().copied()).unwrap();
        thread::sleep(Duration::from_millis(20));
        world.update(positions[2..].iter().copied()).unwrap();
        assert!(jobs.lock().unwrap().is_empty());
        assert!(matches!(
            world.chunk(positions[0]),
            Some(ChunkState::Loaded(_))
        ));

        // Once the delay has passed, one save writes both.
        world.unload_save_delay = Duration::ZERO;
        world.update(positions[2..].iter().copied()).unwrap();
        wait_until(|| jobs.lock().unwrap().len() == 1);
        assert_eq!(jobs.lock().unwrap()[0], positions[..2]);

        // The chunks that leave while the region is being written share its next save.
        world.update(positions[3..].iter().copied()).unwrap();
        world.update([]).unwrap();
        world.update([]).unwrap();
        assert_eq!(jobs.lock().unwrap().len(), 1);
        let (lock, signal) = &*gate;
        *lock.lock().unwrap() = true;
        signal.notify_all();
        wait_until(|| {
            world.update([]).unwrap();
            world.changes.is_empty()
        });
        assert_eq!(
            *jobs.lock().unwrap(),
            vec![positions[..2].to_vec(), positions[2..].to_vec()]
        );
        assert!(world.chunk(positions[3]).is_none());
    }

    #[test]
    fn autosave_queue_is_bounded_and_flushes_every_region() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (started, received) = mpsc::channel();
        let writes = Arc::new(Mutex::new(HashSet::new()));
        let load = Arc::new(|pos| Ok(Some((chunk(pos, DATA_VERSION), Vec::new(), false))));
        let save = {
            let gate = Arc::clone(&gate);
            let writes = Arc::clone(&writes);
            Arc::new(move |job: &SaveJob| {
                let _ = started.send(());
                let (lock, signal) = &*gate;
                let guard = lock.lock().unwrap();
                let _ = signal
                    .wait_timeout_while(guard, Duration::from_secs(5), |open| !*open)
                    .unwrap();
                writes.lock().unwrap().insert(job.snapshots[0].pos);
                Ok(())
            })
        };
        let mut world = fake_world_with_save(2, load, save);
        let wanted: Vec<_> = (0..80).map(|x| ChunkPos { x: x * 32, z: 0 }).collect();
        load_chunks(&mut world, &wanted);
        for pos in &wanted {
            world.chunk_mut(*pos).unwrap().rest.insert("Marker", 1);
        }
        world.autosave().unwrap();
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(
            world
                .changes
                .values()
                .filter(|change| change.in_flight)
                .count()
                <= QUEUE_CAPACITY + 1
        );
        assert!(world.changes.values().any(|change| change.requested));
        let (lock, signal) = &*gate;
        *lock.lock().unwrap() = true;
        signal.notify_all();
        world.flush().unwrap();
        assert_eq!(*writes.lock().unwrap(), wanted.into_iter().collect());
    }

    #[test]
    fn a_failed_save_keeps_the_chunk_dirty_for_a_retry() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let load = Arc::new(|pos| Ok(Some((chunk(pos, DATA_VERSION), Vec::new(), false))));
        let save = {
            let attempts = Arc::clone(&attempts);
            Arc::new(move |_: &SaveJob| {
                if attempts.fetch_add(1, Ordering::Relaxed) == 0 {
                    Err(io::Error::other("disk unavailable"))
                } else {
                    Ok(())
                }
            })
        };
        let mut world = fake_world_with_save(1, load, save);
        let pos = ChunkPos { x: 0, z: 0 };
        load_chunks(&mut world, &[pos]);
        world.chunk_mut(pos).unwrap().rest.insert("Marker", 1);
        world.autosave().unwrap();
        wait_until(|| match world.update([pos]) {
            Err(error) => {
                assert!(error.to_string().contains("disk unavailable"));
                true
            }
            Ok(()) => false,
        });
        assert!(world.changes.contains_key(&pos));
        assert!(matches!(world.chunk(pos), Some(ChunkState::Loaded(_))));
        world.flush().unwrap();
        assert!(world.changes.is_empty());
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn autosave_unload_and_shutdown_persist_edits_and_preserve_neighbors() {
        let folder = Folder::new();
        let a = ChunkPos { x: -1, z: -33 };
        let b = ChunkPos { x: -2, z: -33 };
        let c = ChunkPos { x: -3, z: -33 };
        let neighbor = ChunkPos { x: -4, z: -33 };
        let wanted = [a, b, c, neighbor];
        save(&folder, &wanted.map(|pos| (pos, DATA_VERSION)));
        let mut world = World::open(&folder.0, BIOMES).unwrap();
        load_chunks(&mut world, &wanted);
        let dirt = block_states().default_state("minecraft:dirt").unwrap();
        world.chunk_mut(a).unwrap().sections[0]
            .section
            .as_mut()
            .unwrap()
            .set_block_state(0, 0, 0, dirt);
        world.chunk_mut(a).unwrap().rest.insert("Marker", 1);
        world.autosave().unwrap();
        wait_until(|| {
            world.update(wanted).unwrap();
            !world.changes.contains_key(&a)
        });
        let saved_a = read_chunk(&folder.region_dir(), a.x, a.z).unwrap().unwrap();
        let (saved_a, _) = Chunk::read(saved_a, BIOMES).unwrap();
        assert_eq!(saved_a.rest.get("Marker"), Some(&Nbt::Int(1)));
        assert_eq!(
            saved_a.sections[0]
                .section
                .as_ref()
                .unwrap()
                .block_state(0, 0, 0),
            dirt
        );

        world.chunk_mut(b).unwrap().rest.insert("Marker", 2);
        world.unload_save_delay = Duration::ZERO;
        wait_until(|| {
            world.update([a, c, neighbor]).unwrap();
            world.chunk(b).is_none()
        });
        assert_eq!(
            read_chunk(&folder.region_dir(), b.x, b.z)
                .unwrap()
                .unwrap()
                .get("Marker"),
            Some(&Nbt::Int(2))
        );

        world.chunk_mut(c).unwrap().rest.insert("Marker", 3);
        let (handle, logic) = crate::start_game(Some(world)).unwrap();
        handle.shutdown("Server closed").unwrap();
        logic.join().unwrap().unwrap();
        assert_eq!(
            read_chunk(&folder.region_dir(), c.x, c.z)
                .unwrap()
                .unwrap()
                .get("Marker"),
            Some(&Nbt::Int(3))
        );
        assert_eq!(
            read_chunk(&folder.region_dir(), neighbor.x, neighbor.z)
                .unwrap()
                .unwrap(),
            chunk(neighbor, DATA_VERSION).to_nbt(BIOMES)
        );
    }

    #[test]
    fn the_six_thousandth_tick_saves_changed_chunks() {
        let folder = Folder::new();
        let pos = ChunkPos { x: 0, z: 0 };
        save(&folder, &[(pos, DATA_VERSION)]);
        let (output, _received) = mpsc::channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: profile(),
                view_distance: 2,
                initial_view_distance: 2,
                allow_flight: false,
                output,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            stopped: false,
        };
        tick(&mut game, 1);
        wait_until(|| {
            tick(&mut game, 2);
            matches!(
                game.world.as_ref().unwrap().chunk(pos),
                Some(ChunkState::Loaded(_))
            )
        });
        game.world
            .as_mut()
            .unwrap()
            .chunk_mut(pos)
            .unwrap()
            .rest
            .insert("Marker", 42);
        tick(&mut game, 5_999);
        assert!(!game.world.as_ref().unwrap().changes[&pos].in_flight);
        tick(&mut game, 6_000);
        assert!(game.world.as_ref().unwrap().changes[&pos].in_flight);
        wait_until(|| {
            tick(&mut game, 6_001);
            !game.world.as_ref().unwrap().changes.contains_key(&pos)
        });
        assert_eq!(
            read_chunk(&folder.region_dir(), pos.x, pos.z)
                .unwrap()
                .unwrap()
                .get("Marker"),
            Some(&Nbt::Int(42))
        );
    }

    #[test]
    fn a_damaged_neighbor_stops_shutdown_without_overwriting_the_region() {
        let folder = Folder::new();
        let pos = ChunkPos { x: 0, z: 0 };
        let neighbor = ChunkPos { x: 1, z: 0 };
        save(&folder, &[(pos, DATA_VERSION), (neighbor, DATA_VERSION)]);
        let mut world = World::open(&folder.0, BIOMES).unwrap();
        load_chunks(&mut world, &[pos]);
        world.chunk_mut(pos).unwrap().rest.insert("Marker", 1);
        let path = folder.region_dir().join("r.0.0.mca");
        let mut damaged = fs::read(&path).unwrap();
        damaged[4..8].copy_from_slice(&0x0000_0101_u32.to_be_bytes());
        fs::write(&path, &damaged).unwrap();
        let (handle, logic) = crate::start_game(Some(world)).unwrap();
        handle.shutdown("Server closed").unwrap();
        let error = logic.join().unwrap().unwrap_err();
        assert!(error.to_string().contains("Cannot save the world"));
        assert!(error.to_string().contains("chunk 1, 0"));
        assert_eq!(fs::read(path).unwrap(), damaged);
    }

    #[test]
    fn game_loads_chunks_in_view_and_releases_them_after_the_player_leaves() {
        let folder = Folder::new();
        let origin = ChunkPos { x: 0, z: 0 };
        let west = ChunkPos { x: -1, z: 0 };
        save(&folder, &[(origin, DATA_VERSION), (west, DATA_VERSION)]);
        let (output, _received) = mpsc::channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: profile(),
                view_distance: 10,
                initial_view_distance: 10,
                allow_flight: false,
                output,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            stopped: false,
        };
        tick(&mut game, 1); // Joining happens after the dimension phase.
        assert!(game.world.as_ref().unwrap().chunk(origin).is_none());
        wait_until(|| {
            tick(&mut game, 2);
            matches!(
                game.world.as_ref().unwrap().chunk(origin),
                Some(ChunkState::Loaded(_))
            )
        });
        game.pending.push(Event::Input {
            entity_id: 1,
            input: Input::ConfirmTeleport(1),
        });
        game.pending.push(Event::Input {
            entity_id: 1,
            input: Input::Movement(Movement {
                position: Some([-0.5, 65.0, 0.5]),
                rotation: None,
                on_ground: true,
            }),
        });
        tick(&mut game, 3);
        wait_until(|| {
            tick(&mut game, 4);
            matches!(
                game.world.as_ref().unwrap().chunk(west),
                Some(ChunkState::Loaded(_))
            )
        });
        assert!(matches!(
            game.world.as_ref().unwrap().chunk(origin),
            Some(ChunkState::Loaded(_))
        ));
        game.pending.push(Event::Left(1));
        tick(&mut game, 5);
        tick(&mut game, 6);
        assert!(game.world.as_ref().unwrap().chunk(west).is_none());
        assert!(game.world.as_ref().unwrap().chunk(origin).is_none());
    }

    #[test]
    fn game_sends_each_viewed_chunk_once_and_waits_for_the_previous_batch() {
        let folder = Folder::new();
        let origin = ChunkPos { x: 0, z: 0 };
        save(&folder, &[(origin, DATA_VERSION)]);
        let (output, received) = mpsc::channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: profile(),
                view_distance: 2,
                initial_view_distance: 2,
                allow_flight: false,
                output,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            stopped: false,
        };
        tick(&mut game, 1);
        let mut first = None;
        wait_until(|| {
            tick(&mut game, 2);
            for output in received.try_iter() {
                if let Output::ChunkBatch(batch) = output {
                    first = Some(batch);
                }
            }
            first.is_some()
        });
        let first = first.unwrap();
        assert!(!first.is_empty() && first.len() <= crate::CHUNKS_PER_BATCH);
        for _ in 0..20 {
            tick(&mut game, 3);
        }
        assert!(
            !received
                .try_iter()
                .any(|output| matches!(output, Output::ChunkBatch(_)))
        );
        let mut seen: HashSet<_> = first.iter().map(|chunk| chunk.position).collect();
        let mut found_origin = first
            .iter()
            .any(|chunk| chunk.position == origin && chunk.chunk.is_some());
        wait_until(|| {
            game.pending.push(Event::Input {
                entity_id: 1,
                input: Input::ChunkBatchReceived(8.0),
            });
            tick(&mut game, 4);
            tick(&mut game, 5);
            for output in received.try_iter() {
                if let Output::ChunkBatch(batch) = output {
                    assert!(batch.len() <= crate::CHUNKS_PER_BATCH);
                    for chunk in batch {
                        assert!(chunk.position.within(origin, 2));
                        assert!(seen.insert(chunk.position), "a chunk was sent twice");
                        found_origin |= chunk.position == origin && chunk.chunk.is_some();
                    }
                }
            }
            seen.len() == 25
        });
        assert!(found_origin);
    }

    #[test]
    fn client_acknowledgements_control_batch_rate_and_invalid_rates_are_bounded() {
        let folder = Folder::new();
        let (output, received) = mpsc::channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: profile(),
                view_distance: 2,
                initial_view_distance: 2,
                allow_flight: false,
                output,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            stopped: false,
        };
        tick(&mut game, 1);
        let mut first_batch = false;
        wait_until(|| {
            tick(&mut game, 2);
            first_batch |= received
                .try_iter()
                .any(|output| matches!(output, Output::ChunkBatch(_)));
            first_batch
                && game
                    .world
                    .as_ref()
                    .unwrap()
                    .chunks
                    .values()
                    .all(|state| !matches!(state, ChunkState::Loading))
        });
        game.players[0].chunk_credit = 0.0;
        game.pending.push(Event::Input {
            entity_id: 1,
            input: Input::ChunkBatchReceived(0.25),
        });
        tick(&mut game, 3);
        for number in 4..=6 {
            tick(&mut game, number);
            assert!(
                !received
                    .try_iter()
                    .any(|output| matches!(output, Output::ChunkBatch(_)))
            );
        }
        tick(&mut game, 7);
        assert!(
            received
                .try_iter()
                .any(|output| matches!(output, Output::ChunkBatch(batch) if batch.len() == 1))
        );
        game.pending.push(Event::Input {
            entity_id: 1,
            input: Input::ChunkBatchReceived(f32::NAN),
        });
        tick(&mut game, 8);
        assert_eq!(game.players[0].chunks_per_tick, 0.01);
        game.pending.push(Event::Input {
            entity_id: 1,
            input: Input::ChunkBatchReceived(8.0),
        });
        tick(&mut game, 9);
        assert_eq!(
            game.players[0].chunks_per_tick, 0.01,
            "unsolicited acknowledgements are ignored"
        );
    }

    #[test]
    fn changing_view_distance_forgets_and_resends_only_the_outer_ring() {
        let folder = Folder::new();
        let (output, received) = mpsc::channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: profile(),
                view_distance: 3,
                initial_view_distance: 3,
                allow_flight: false,
                output,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            stopped: false,
        };
        tick(&mut game, 1);
        let mut initial = HashSet::new();
        wait_until(|| {
            game.pending.push(Event::Input {
                entity_id: 1,
                input: Input::ChunkBatchReceived(8.0),
            });
            tick(&mut game, 2);
            for output in received.try_iter() {
                if let Output::ChunkBatch(batch) = output {
                    for chunk in batch {
                        assert!(initial.insert(chunk.position));
                    }
                }
            }
            initial.len() == 49
        });
        game.pending.push(Event::Input {
            entity_id: 1,
            input: Input::ChunkBatchReceived(8.0),
        });
        game.pending.push(Event::Input {
            entity_id: 1,
            input: Input::ViewDistance(2),
        });
        tick(&mut game, 3);
        tick(&mut game, 4);
        assert_eq!(game.players[0].sent_chunks.len(), 25);
        assert!(
            received
                .try_iter()
                .any(|output| matches!(output, Output::ViewDistance(2)))
        );
        game.pending.push(Event::Input {
            entity_id: 1,
            input: Input::ViewDistance(3),
        });
        tick(&mut game, 5);
        let mut resent = HashSet::new();
        wait_until(|| {
            game.pending.push(Event::Input {
                entity_id: 1,
                input: Input::ChunkBatchReceived(8.0),
            });
            tick(&mut game, 6);
            for output in received.try_iter() {
                if let Output::ChunkBatch(batch) = output {
                    for chunk in batch {
                        let pos = chunk.position;
                        assert!(!pos.within(ChunkPos { x: 0, z: 0 }, 2));
                        assert!(pos.within(ChunkPos { x: 0, z: 0 }, 3));
                        assert!(resent.insert(pos));
                    }
                }
            }
            resent.len() == 24
        });
    }

    #[test]
    fn incompatible_chunk_stops_the_running_logic_thread() {
        let folder = Folder::new();
        save(&folder, &[(ChunkPos { x: 0, z: 0 }, DATA_VERSION - 1)]);
        let (handle, logic) = crate::start_with_world(&folder.0, BIOMES).unwrap();
        let session = handle.player_joined(1, profile(), 10, false).unwrap();
        wait_until(|| logic.is_finished());
        assert!(matches!(
            session.try_recv(),
            Some(Output::Disconnect("Cannot access the world"))
        ));
        logic.join().unwrap().unwrap();
    }

    #[test]
    fn incompatible_chunk_remains_fatal_when_a_player_leaves_before_the_read_finishes() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (started, received) = mpsc::channel();
        let load = {
            let gate = Arc::clone(&gate);
            Arc::new(move |_| {
                started.send(()).unwrap();
                let (lock, signal) = &*gate;
                let guard = lock.lock().unwrap();
                let _ = signal
                    .wait_timeout_while(guard, Duration::from_secs(3), |open| !*open)
                    .unwrap();
                let mut nbt = Compound::new();
                nbt.insert("DataVersion", DATA_VERSION - 1);
                let error = harpas_anvil::check_chunk(&nbt).unwrap_err();
                Err(io::Error::new(io::ErrorKind::InvalidData, error))
            })
        };
        let mut world = fake_world(1, load);
        let pos = ChunkPos { x: 0, z: 0 };
        world.update([pos]).unwrap();
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        world.update([]).unwrap();
        let (lock, signal) = &*gate;
        *lock.lock().unwrap() = true;
        signal.notify_all();
        let mut message = None;
        wait_until(|| match world.update([]) {
            Err(error) => {
                message = Some(error.to_string());
                true
            }
            Ok(()) => false,
        });
        assert!(message.unwrap().contains("--forceUpgrade"));
    }
}
