//! Loading and saving overworld chunks without doing disk I/O on the logic thread.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use harpas_anvil::{
    Chunk, VersionError, check_chunk, dimension_dir, overworld_spawn, read_chunk,
    read_entity_chunk, read_level, recover, region_of, write_chunks,
};
use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, Nbt};
use log::{error, warn};

use crate::block::{self, BlockPos};
use crate::entity::{ItemEntity, Kind};
use crate::entity_data::{EntityChunk, SavedItem, SavedPig};
use crate::fluid::FluidType;
use crate::furnace::Furnace;
use crate::item_data::ItemCodec;
use crate::light::{self, Light, Lighter};
use crate::neighbor;
use crate::ticks::{self, LevelTicks};
use harpas_core::random::{LegacyRandom, Seed128, XoroshiroRandom};
use harpas_worldgen::shape_update::ShapeUpdates;

/// Keep disk requests bounded even when players move faster than the disk can respond.
const QUEUE_CAPACITY: usize = 64;

/// The status of a chunk that ran FEATURES and has no light yet.
const FEATURES: &str = "minecraft:features";

/// Most workers that read and generate chunks, and as many that light them. More than this was not measured.
const MAX_WORKERS: usize = 16;

/// The chunk workers for a machine with `parallelism` hardware threads: two thirds of them. Generation gains
/// little from the rest, which the logic thread and the connections need (`docs/engine-core.md`).
fn worker_count(parallelism: usize) -> usize {
    (parallelism * 2 / 3).clamp(1, MAX_WORKERS)
}
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

/// The states of `air`, `cave_air` and `void_air`.
fn air_states() -> [u32; 3] {
    static AIR: OnceLock<[u32; 3]> = OnceLock::new();
    *AIR.get_or_init(|| {
        ["minecraft:air", "minecraft:cave_air", "minecraft:void_air"]
            .map(|name| block_states().default_state(name).expect("an air block"))
    })
}

/// The changed blocks of one chunk section, with their current states.
pub(crate) type SectionChanges = Vec<(BlockPos, u32)>;

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

    /// Finds the buried treasure nearest an unopened chest that rolled a treasure map, and builds the preview.
    fn treasure_map(
        &self,
        _origin: BlockPos,
        _storage: &ChunkStorage,
    ) -> io::Result<Option<(crate::MapData, [i32; 2])>> {
        Ok(None)
    }

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
            let chunks: Vec<_> = chunks
                .iter()
                .map(|(pos, chunk)| ((pos.x, pos.z), chunk.to_nbt(&biomes)))
                .collect();
            let chunks: Vec<_> = chunks.iter().map(|(pos, nbt)| (*pos, nbt)).collect();
            write_chunks(&self.dir, x, z, &chunks)?;
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

type Terrain = (Chunk, Vec<String>, bool);
type Loaded = io::Result<(Option<Terrain>, Option<EntityChunk>)>;
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
    terrain_revision: u64,
    entity_revision: u64,
    in_flight: bool,
    requested: bool,
    /// When the chunk left every view with unsaved changes.
    unwanted_since: Option<Instant>,
}

impl Changes {
    fn next_revision(&mut self) -> u64 {
        self.revision = self
            .revision
            .checked_add(1)
            .expect("chunk revision exhausted");
        self.revision
    }

    fn terrain_changed(&mut self) {
        self.terrain_revision = self.next_revision();
    }

    fn entities_changed(&mut self) {
        self.entity_revision = self.next_revision();
    }
}

/// Where the light of a chunk generated without it stands.
enum LightState {
    /// A worker has the request with this ID.
    Computing(u64),
    /// The light, or none when computing it failed.
    Ready(Option<Arc<Light>>),
}

struct Snapshot {
    pos: ChunkPos,
    revision: u64,
    chunk: Option<Arc<Chunk>>,
    /// The light to save with a final chunk that ran FEATURES, once the workers have computed it.
    light: Option<Arc<Light>>,
    /// Whether a lit chunk is waiting to be lit again, so that its saved light is out of date.
    stale_light: bool,
    /// The chunk's fluid ticks as it saves them, or none for a chunk without them.
    fluid_ticks: Option<Nbt>,
    entities: Option<EntityChunk>,
}

impl Snapshot {
    /// The chunk as it is written: with its light and at vanilla's `light` status, once it has light. A chunk
    /// whose light is out of date is saved without `isLightOn`, so that vanilla lights it again when it loads it.
    fn saved(&self) -> Option<Cow<'_, Chunk>> {
        let saved = self.saved_light(self.chunk.as_deref()?);
        Some(match &self.fluid_ticks {
            Some(ticks) => {
                let mut chunk = saved.into_owned();
                chunk.rest.insert("fluid_ticks", ticks.clone());
                Cow::Owned(chunk)
            }
            None => saved,
        })
    }

    fn saved_light<'a>(&self, chunk: &'a Chunk) -> Cow<'a, Chunk> {
        match &self.light {
            Some(light) => Cow::Owned(light::with_light(chunk, light)),
            None if self.stale_light => {
                let mut chunk = chunk.clone();
                chunk.rest.insert("isLightOn", Nbt::Byte(0));
                Cow::Owned(chunk)
            }
            None => Cow::Borrowed(chunk),
        }
    }
}

struct SaveJob {
    snapshots: Vec<Snapshot>,
    item_codec: Arc<ItemCodec>,
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
    pub(crate) survival_rules: crate::survival::SurvivalRules,
    generates: bool,
    chunks: HashMap<ChunkPos, ChunkState>,
    /// The chunks to keep loaded, most wanted first and each once, as [`World::want`] left them.
    wanted: Vec<ChunkPos>,
    pub(crate) wanted_set: HashSet<ChunkPos>,
    in_flight: HashSet<ChunkPos>,
    changes: HashMap<ChunkPos, Changes>,
    /// The regions with a save queued or being written. A region has one save at a time, so the changes
    /// made meanwhile are written together by the next one.
    saving: HashSet<(i32, i32)>,
    unload_save_delay: Duration,
    /// The light of the loaded chunks that were generated without it.
    light: HashMap<ChunkPos, LightState>,
    /// The blocks changed since the last "sending block changes" step, each once, in the order they first
    /// changed.
    block_changes: Vec<BlockPos>,
    changed_blocks: HashSet<BlockPos>,
    /// The shape rules of the blocks, without which changes leave their neighbors as they are.
    pub(crate) shape_updates: Option<Arc<ShapeUpdates>>,
    pub(crate) updater: neighbor::Updater,
    /// World events played by block changes, for the logic thread to send.
    pub(crate) level_events: Vec<neighbor::LevelEvent>,
    /// The sections whose light changed when a lit chunk was lit again, for the logic thread to send.
    light_updates: Vec<(ChunkPos, Arc<Light>)>,
    /// The game time, read from `level.dat` (`Time`) and counted every tick.
    pub(crate) game_time: i64,
    /// The scheduled fluid ticks of the loaded chunks. Their block ticks stay saved as they were until blocks
    /// that tick exist.
    pub(crate) fluid_ticks: LevelTicks<FluidType>,
    /// Loaded furnaces that can cook, are burning or have progress left to cool.
    pub(crate) active_furnaces: BTreeSet<BlockPos>,
    /// The level's random source, which lava's spread delay draws from.
    pub(crate) random: LegacyRandom,
    pub(crate) item_uuids: XoroshiroRandom,
    /// The entities other than players.
    pub(crate) entities: crate::entity::Entities,
    entity_data: HashMap<ChunkPos, EntityChunk>,
    item_codec: Arc<RwLock<Arc<ItemCodec>>>,
    /// Loot tables for broken blocks and supported chests.
    pub(crate) loot: Option<Arc<crate::loot::LootTables>>,
    pub(crate) maps: crate::maps::Maps,
    /// The world seed, read from the world's generation settings, which seeds the random sequences.
    pub(crate) seed: i64,
    /// The crafting recipes, smelting recipes and furnace fuels.
    pub(crate) recipes: Option<Arc<crate::recipe::Recipes>>,
    pub(crate) mining: Option<Arc<crate::mining::MiningRules>>,
    pub(crate) climbable_blocks: Vec<bool>,
    pub(crate) player_store: Option<crate::player_data::PlayerStore>,
    /// The world's named random sequences (`RandomSequences`), which loot tables draw from.
    pub(crate) random_sequences: HashMap<String, harpas_core::random::RandomSequence>,
    /// The loaded chunks that need light and have not been given to the workers yet.
    unlit: HashSet<ChunkPos>,
    next_light_job: u64,
    source: Option<(Arc<dyn ChunkSource>, Arc<ChunkStorage>)>,
    loader: Loader,
    saver: Saver,
    lighter: Lighter,
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
        let level = read_level(world_dir)?;
        let game_rules = harpas_anvil::read_game_rules(world_dir)?;
        let survival_rules = crate::survival::SurvivalRules::read(
            game_rules.as_ref(),
            level.as_ref().map(|(data, _)| data),
        );
        let spawn = level
            .as_ref()
            .and_then(|(data, _)| overworld_spawn(data))
            .unwrap_or([0, 65, 0]);
        let seed = harpas_anvil::read_world_seed(world_dir)?.map_or(0, |(seed, _)| seed);
        let game_time = match level.as_ref().and_then(|(data, _)| data.get("Time")) {
            Some(Nbt::Long(time)) => *time,
            _ => 0,
        };
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
        let mut uuid_seed = [0; 16];
        getrandom::fill(&mut uuid_seed).map_err(io::Error::other)?;
        let item_uuids = XoroshiroRandom::from_seed_128(Seed128::new(
            u64::from_le_bytes(uuid_seed[..8].try_into().unwrap()),
            u64::from_le_bytes(uuid_seed[8..].try_into().unwrap()),
        ));
        let entity_dir = dimension_dir(world_dir, "minecraft:overworld").join("entities");
        recover(&entity_dir)?;
        let load_entity_dir = entity_dir.clone();
        let item_codec = Arc::new(RwLock::new(Arc::new(ItemCodec::default())));
        let load_codec = Arc::clone(&item_codec);
        let load_storage = Arc::clone(&storage);
        let generates = generator.is_some();
        let source = generator
            .clone()
            .map(|source| (source, Arc::clone(&storage)));
        let maps = crate::maps::Maps::start(world_dir, source.clone())?;
        let load = Arc::new(move |pos: ChunkPos| {
            let storage = &*load_storage;
            let nbt = storage.read_unchecked(pos)?;
            let loaded = match (nbt, generator.as_ref()) {
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
            }?;
            let codec = load_codec.read().unwrap_or_else(|e| e.into_inner()).clone();
            let (x, z) = region_of(pos.x, pos.z);
            let _guard = storage
                .locks
                .for_region(x, z)
                .read()
                .unwrap_or_else(|e| e.into_inner());
            let entities = read_entity_chunk(&load_entity_dir, pos.x, pos.z)
                .map_err(|error| {
                    io::Error::new(error.kind(), format!("entity region {x}, {z}: {error}"))
                })?
                .map(|root| EntityChunk::read(root, pos, &codec))
                .transpose()?;
            Ok((loaded, entities))
        });
        let save = Arc::new(move |job: &SaveJob| {
            let saved: Vec<_> = job.snapshots.iter().map(Snapshot::saved).collect();
            let chunks: Vec<_> = job
                .snapshots
                .iter()
                .zip(&saved)
                .filter_map(|(snapshot, chunk)| Some((snapshot.pos, chunk.as_deref()?)))
                .collect();
            let entities: Vec<_> = job
                .snapshots
                .iter()
                .filter_map(|s| {
                    s.entities.as_ref().map(|entities| {
                        entities
                            .to_nbt(s.pos, &job.item_codec)
                            .map(|root| ((s.pos.x, s.pos.z), root))
                    })
                })
                .collect::<io::Result<_>>()?;
            if !chunks.is_empty() {
                storage.write(&chunks)?;
            }
            if let Some((pos, _)) = entities.first() {
                let (x, z) = region_of(pos.0, pos.1);
                let entities: Vec<_> = entities.iter().map(|(pos, root)| (*pos, root)).collect();
                let _guard = storage
                    .locks
                    .for_region(x, z)
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                write_chunks(&entity_dir, x, z, &entities)?;
            }
            Ok(())
        });
        let workers = worker_count(thread::available_parallelism().map_or(1, usize::from));
        Ok(Self {
            region_dir,
            spawn,
            survival_rules,
            generates,
            chunks: HashMap::new(),
            wanted: Vec::new(),
            wanted_set: HashSet::new(),
            in_flight: HashSet::new(),
            changes: HashMap::new(),
            saving: HashSet::new(),
            unload_save_delay: UNLOAD_SAVE_DELAY,
            light: HashMap::new(),
            block_changes: Vec::new(),
            changed_blocks: HashSet::new(),
            shape_updates: None,
            updater: neighbor::Updater::default(),
            level_events: Vec::new(),
            light_updates: Vec::new(),
            game_time,
            fluid_ticks: LevelTicks::default(),
            active_furnaces: BTreeSet::new(),
            random: LegacyRandom::new(0),
            item_uuids,
            entities: crate::entity::Entities::default(),
            entity_data: HashMap::new(),
            item_codec,
            loot: None,
            maps,
            seed,
            recipes: None,
            mining: None,
            climbable_blocks: Vec::new(),
            player_store: Some(crate::player_data::PlayerStore::start(world_dir, spawn)?),
            random_sequences: HashMap::new(),
            unlit: HashSet::new(),
            next_light_job: 0,
            source,
            loader: Loader::start(workers, load)?,
            saver: Saver::start(save)?,
            lighter: Lighter::start(workers, QUEUE_CAPACITY)?,
        })
    }

    /// Configures saved item components before requesting chunks or loading player data.
    pub fn set_item_codec(&mut self, codec: Arc<ItemCodec>) {
        *self.item_codec.write().unwrap_or_else(|e| e.into_inner()) = Arc::clone(&codec);
        if let Some(store) = &mut self.player_store {
            store.codec = codec;
        }
    }

    pub(crate) fn item_codec(&self) -> Arc<ItemCodec> {
        self.item_codec
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(crate) fn effect_ids(&self) -> [Option<i32>; crate::player::EffectKind::ALL.len()] {
        self.block_item_codec().effect_ids()
    }

    /// Incorporates entity mutations before snapshots or eviction. The entity and terrain revisions share
    /// the existing saver, so a chunk cannot reload an older entity snapshot while a write is outstanding.
    fn sync_entity_changes(&mut self) -> io::Result<()> {
        let chunks = &self.chunks;
        let mut failed = None;
        self.entities
            .changed_chunks
            .retain(|pos| match chunks.get(pos) {
                Some(ChunkState::Loaded(_) | ChunkState::Missing) => {
                    self.entity_data.entry(*pos).or_default();
                    self.changes.entry(*pos).or_default().entities_changed();
                    false
                }
                Some(ChunkState::Failed(error)) => {
                    failed = Some(io::Error::new(
                        error.kind(),
                        format!("Cannot save entities at {}, {}: {error}", pos.x, pos.z),
                    ));
                    true
                }
                Some(ChunkState::Loading) | None => true,
            });
        failed.map_or(Ok(()), Err)
    }

    fn snapshot_entities(
        &self,
        positions: impl Iterator<Item = ChunkPos>,
    ) -> HashMap<ChunkPos, EntityChunk> {
        let mut saved: HashMap<_, _> = positions
            .map(|pos| (pos, self.entity_data[&pos].clone()))
            .collect();
        if saved.is_empty() {
            return saved;
        }
        for &handle in &self.entities.order {
            let i = self.entities.slot(handle);
            let position = self.entities.position[i];
            let Some(chunk) = saved.get_mut(&ChunkPos::at(position[0], position[2])) else {
                continue;
            };
            let uuid = self.entities.uuid[i];
            let velocity = self.entities.velocity[i];
            let on_ground = self.entities.on_ground[i];
            match self.entities.kind[i].as_ref().expect("live entity") {
                Kind::Item(item) => chunk.items.push(SavedItem {
                    uuid,
                    position,
                    velocity,
                    on_ground,
                    stack: item.stack.clone(),
                    age: item.age,
                    pickup_delay: item.pickup_delay,
                    health: item.health,
                    rest: Arc::clone(&item.rest),
                }),
                Kind::Pig(pig) if pig.health > 0.0 => chunk.pigs.push(SavedPig {
                    uuid,
                    position,
                    velocity,
                    on_ground,
                    pig: pig.clone(),
                }),
                Kind::Pig(_) => {}
            }
        }
        saved
    }

    fn load_entities(&mut self, pos: ChunkPos, mut saved: EntityChunk) -> io::Result<()> {
        let mut uuids = saved
            .items
            .iter()
            .map(|item| item.uuid)
            .chain(saved.pigs.iter().map(|pig| pig.uuid));
        if uuids.any(|uuid| {
            self.entities
                .order
                .iter()
                .any(|&h| self.entities.uuid[self.entities.slot(h)] == uuid)
        }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "duplicate loaded entity UUID",
            ));
        }
        let items = std::mem::take(&mut saved.items).into_iter().map(|item| {
            (
                item.uuid,
                item.position,
                item.velocity,
                item.on_ground,
                Kind::Item(ItemEntity {
                    stack: item.stack,
                    age: item.age,
                    pickup_delay: item.pickup_delay,
                    health: item.health,
                    rest: item.rest,
                }),
            )
        });
        let pigs = std::mem::take(&mut saved.pigs).into_iter().map(|pig| {
            (
                pig.uuid,
                pig.position,
                pig.velocity,
                pig.on_ground,
                Kind::Pig(pig.pig),
            )
        });
        let ids = (0..items.len() + pigs.len())
            .map(|_| {
                crate::next_entity_id().ok_or_else(|| io::Error::other("entity IDs exhausted"))
            })
            .collect::<io::Result<Vec<_>>>()?;
        for (id, (uuid, position, velocity, on_ground, kind)) in
            ids.into_iter().zip(items.chain(pigs))
        {
            let handle = self.entities.add(id, uuid, position, velocity, kind);
            let i = self.entities.slot(handle);
            self.entities.on_ground[i] = on_ground;
        }
        self.entity_data.insert(pos, saved);
        Ok(())
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

    /// The block state at `pos`, or none when its chunk is not loaded or the block is outside the world's
    /// height. A loaded chunk without a section at that height is air there.
    pub fn block_state(&self, pos: BlockPos) -> Option<u32> {
        if !(block::MIN_Y..=block::MAX_Y).contains(&pos.y) {
            return None;
        }
        match self.chunks.get(&pos.chunk())? {
            ChunkState::Loaded(chunk) => Some(block::get(chunk, pos)),
            _ => None,
        }
    }

    /// Whether eyes at `eye` are under water, as vanilla's `Entity.updateFluidOnEyes`: the water of the block a
    /// ninth of a block below them reaches above that point.
    pub(crate) fn eye_in_water(&self, [x, y, z]: [f64; 3]) -> bool {
        self.point_in_water([x, y - f64::from(0.111_111_11_f32), z])
    }

    pub(crate) fn point_in_water(&self, [x, y, z]: [f64; 3]) -> bool {
        let pos = BlockPos {
            x: x.floor() as i32,
            y: y.floor() as i32,
            z: z.floor() as i32,
        };
        let state = |pos| self.block_state(pos).unwrap_or_else(block::void_air);
        block::water_height(
            state(pos),
            state(BlockPos {
                y: pos.y + 1,
                ..pos
            }),
        )
        .is_some_and(|height| f64::from(pos.y) + f64::from(height) > y)
    }

    pub(crate) fn on_climbable(&self, position: crate::player::Position) -> bool {
        let pos = BlockPos {
            x: position.x.floor() as i32,
            y: position.y.floor() as i32,
            z: position.z.floor() as i32,
        };
        let Some(state) = self.block_state(pos) else {
            return false;
        };
        if self.climbable_blocks.get(state as usize) == Some(&true) {
            return true;
        }
        let states = block_states();
        if !states
            .name(state)
            .is_some_and(|name| name.ends_with("_trapdoor"))
        {
            return false;
        }
        // An open trapdoor continues the ladder directly below it when both face the same way.
        let Some((_, trapdoor)) = states.properties(state) else {
            return false;
        };
        if !trapdoor.contains(&("open", "true")) {
            return false;
        }
        self.block_state(BlockPos {
            y: pos.y - 1,
            ..pos
        })
        .and_then(|state| states.properties(state))
        .is_some_and(|(name, ladder)| {
            name == "minecraft:ladder"
                && ladder
                    .iter()
                    .any(|property| property.0 == "facing" && trapdoor.contains(property))
        })
    }

    /// Sets the block state at `pos` in a loaded chunk, as vanilla's `LevelChunk.setBlockState`, and records
    /// the change for the next "sending block changes" step, without updating the neighbors. Returns the old
    /// state, or none when nothing changed: the chunk is not loaded, the block is outside the world's height, or
    /// it already had `state`.
    pub fn set_block_state(&mut self, pos: BlockPos, state: u32) -> Option<u32> {
        let old = self.block_state(pos)?;
        self.set_block(
            pos,
            state,
            neighbor::UPDATE_CLIENTS | neighbor::UPDATE_KNOWN_SHAPE,
            0,
        )
        .then_some(old)
    }

    /// Gives the world the block loot tables, so that broken blocks drop items.
    pub fn set_loot_tables(&mut self, loot: Arc<crate::loot::LootTables>) {
        self.loot = Some(loot);
    }

    /// Gives the world the shape rules of the blocks, so that a change updates the shapes of its neighbors.
    pub fn set_shape_updates(&mut self, shapes: Arc<ShapeUpdates>) {
        self.shape_updates = Some(shapes);
    }

    /// Records that the block at `pos` must be sent again to the players who have its chunk, as vanilla's
    /// `ChunkHolder.blockChanged`.
    pub(crate) fn block_changed(&mut self, pos: BlockPos) {
        if self.changed_blocks.insert(pos) {
            self.block_changes.push(pos);
        }
    }

    /// Takes the blocks changed since the last call, grouped by chunk in the order the chunks first changed and
    /// by section from the bottom up, each with its current state. A chunk that left meanwhile is left out.
    pub(crate) fn take_block_changes(&mut self) -> Vec<(ChunkPos, Vec<SectionChanges>)> {
        self.changed_blocks.clear();
        let mut index = HashMap::new();
        let mut chunks: Vec<(ChunkPos, BTreeMap<i32, SectionChanges>)> = Vec::new();
        for pos in std::mem::take(&mut self.block_changes) {
            let Some(state) = self.block_state(pos) else {
                continue;
            };
            let chunk = *index.entry(pos.chunk()).or_insert_with(|| {
                chunks.push((pos.chunk(), BTreeMap::new()));
                chunks.len() - 1
            });
            chunks[chunk]
                .1
                .entry(pos.y >> 4)
                .or_default()
                .push((pos, state));
        }
        chunks
            .into_iter()
            .map(|(chunk, sections)| (chunk, sections.into_values().collect()))
            .collect()
    }

    /// Whether a player at `position` touches nothing but air, as vanilla's `noBlocksAround`: every block that
    /// the player's box reaches, grown by a sixteenth of a block and by 0.55 more downward, is air. The box is
    /// that of a standing player. A chunk that is not loaded yet counts as having blocks, so that a player is
    /// never taken to be flying over terrain the server has not read.
    pub(crate) fn no_blocks_around(&self, position: &crate::player::Position) -> bool {
        const HALF_WIDTH: f64 = 0.3 + 0.0625;
        let block = |n: f64| n.floor() as i32;
        let air = air_states();
        for x in block(position.x - HALF_WIDTH)..=block(position.x + HALF_WIDTH) {
            for z in block(position.z - HALF_WIDTH)..=block(position.z + HALF_WIDTH) {
                let pos = ChunkPos {
                    x: x.div_euclid(16),
                    z: z.div_euclid(16),
                };
                let chunk = match self.chunks.get(&pos) {
                    Some(ChunkState::Loaded(chunk)) => chunk,
                    Some(ChunkState::Missing) => continue,
                    Some(ChunkState::Loading | ChunkState::Failed(_)) | None => return false,
                };
                for y in block(position.y - 0.0625 - 0.55)..=block(position.y + 1.8 + 0.0625) {
                    let state = chunk
                        .sections
                        .iter()
                        .find(|section| i32::from(section.y) == y.div_euclid(16))
                        .and_then(|section| section.section.as_ref())
                        .map(|section| {
                            section.block_state(
                                (x & 15) as usize,
                                (y & 15) as usize,
                                (z & 15) as usize,
                            )
                        });
                    if state.is_some_and(|state| !air.contains(&state)) {
                        return false;
                    }
                }
            }
        }
        true
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

    /// The computed light of a chunk that needs it: `None` until the workers have it, and `Some(None)` when
    /// computing it failed, so the chunk is sent without it.
    pub(crate) fn light(&self, pos: ChunkPos) -> Option<Option<Arc<Light>>> {
        match self.light.get(&pos) {
            Some(LightState::Ready(light)) => Some(light.clone()),
            _ => None,
        }
    }

    /// Whether the chunk is saved with its light once the workers have it: a chunk at `features` from a source
    /// that resumes them, which hands out only final chunks. A world that does not decorate holds chunks at
    /// `features` whose neighbors may decorate later, and chunks at `carvers` have not run FEATURES, so vanilla
    /// must still reach `light` for them itself.
    fn saves_light(&self, pos: ChunkPos) -> bool {
        self.source
            .as_ref()
            .is_some_and(|(source, _)| source.resumes(FEATURES))
            && matches!(
                self.chunks.get(&pos),
                Some(ChunkState::Loaded(chunk))
                    if chunk.rest.get("Status") == Some(&Nbt::from(FEATURES))
            )
    }

    /// The light to save with the chunk, if it is saved with light and the workers have computed it.
    fn saved_light(&self, pos: ChunkPos) -> Option<Arc<Light>> {
        match self.light.get(&pos) {
            Some(LightState::Ready(Some(light))) if self.saves_light(pos) => {
                Some(Arc::clone(light))
            }
            _ => None,
        }
    }

    /// The chunks a chunk is lit from, clockwise from the northwest, or `None` while it has to wait for
    /// them. A generating world lights a chunk once its eight neighbors are loaded. A world that does not
    /// generate uses them if all eight are loaded, and none otherwise, once none is still being read.
    fn light_neighbors(&self, pos: ChunkPos) -> Option<[Option<Arc<Chunk>>; 8]> {
        let mut neighbors: [Option<Arc<Chunk>>; 8] = std::array::from_fn(|_| None);
        // A lit chunk is lit again only from its eight neighbors as they are, a missing one being air, so that
        // a neighbor not loaded never counts as air.
        if self.relights(pos) {
            for (index, (dx, dz)) in crate::player::NEIGHBORS.iter().enumerate() {
                let neighbor = ChunkPos {
                    x: pos.x + dx,
                    z: pos.z + dz,
                };
                match self.chunks.get(&neighbor) {
                    Some(ChunkState::Loaded(chunk)) => neighbors[index] = Some(Arc::clone(chunk)),
                    Some(ChunkState::Missing) => {}
                    _ => return None,
                }
            }
            return Some(neighbors);
        }
        let mut complete = true;
        for (index, (dx, dz)) in crate::player::NEIGHBORS.iter().enumerate() {
            let neighbor = ChunkPos {
                x: pos.x + dx,
                z: pos.z + dz,
            };
            match self.chunks.get(&neighbor) {
                Some(ChunkState::Loaded(chunk)) => neighbors[index] = Some(Arc::clone(chunk)),
                Some(ChunkState::Loading) => return None,
                _ if self.generates => return None,
                _ => complete = false,
            }
        }
        Some(if complete {
            neighbors
        } else {
            std::array::from_fn(|_| None)
        })
    }

    /// Forgets the light computed from the chunk at `pos`, which changed: its own and its neighbors'. A lit
    /// chunk being lit again is lit again from the changed chunk.
    fn invalidate_light(&mut self, pos: ChunkPos) {
        let around = crate::player::NEIGHBORS.iter().chain(&[(0, 0)]);
        for &(dx, dz) in around {
            let pos = ChunkPos {
                x: pos.x + dx,
                z: pos.z + dz,
            };
            let removed = self.light.remove(&pos);
            let relighting =
                matches!(removed, Some(LightState::Computing(_))) && self.relights(pos);
            if self.needs_light(pos) || relighting {
                self.unlit.insert(pos);
            }
        }
    }

    /// Whether the chunk is loaded and lit, with its light in its sections: at vanilla's `light`, `spawn` or
    /// `full` status. Such a chunk is lit again when a block near it changes its light.
    pub(crate) fn relights(&self, pos: ChunkPos) -> bool {
        matches!(
            self.chunks.get(&pos),
            Some(ChunkState::Loaded(chunk))
                if matches!(
                    chunk.rest.get("Status"),
                    Some(Nbt::String(status))
                        if matches!(
                            status.as_str(),
                            "minecraft:light" | "minecraft:spawn" | "minecraft:full"
                        )
                )
        )
    }

    /// Lights again the lit chunks whose light a change of the block at `pos` can reach: those within 15
    /// blocks of it (`LightEngine.checkBlock`). They are saved again, with their new light or, until then,
    /// without `isLightOn`.
    pub(crate) fn relight_around(&mut self, pos: BlockPos) {
        for x in (pos.x - 15) >> 4..=(pos.x + 15) >> 4 {
            for z in (pos.z - 15) >> 4..=(pos.z + 15) >> 4 {
                let chunk = ChunkPos { x, z };
                if !self.relights(chunk) {
                    continue;
                }
                self.light.remove(&chunk);
                self.unlit.insert(chunk);
                let changes = self.changes.entry(chunk).or_default();
                changes.terrain_changed();
            }
        }
    }

    /// Counts a game tick and runs the scheduled fluid ticks that are due in the chunks some player sees
    /// (`ServerLevel.tickTime`, then `fluidTicks.tick`). Vanilla runs them in the chunks within the simulation
    /// distance; until it exists, that is every chunk in a player's view.
    pub(crate) fn tick_scheduled(&mut self) {
        self.game_time += 1;
        let wanted = &self.wanted_set;
        let chunks = &self.chunks;
        let due = self.fluid_ticks.collect(self.game_time, |pos| {
            wanted.contains(&pos) && matches!(chunks.get(&pos), Some(ChunkState::Loaded(_)))
        });
        for tick in due {
            self.mark_changed(tick.pos.chunk());
            self.tick_fluid(tick.pos, tick.kind);
        }
    }

    /// Tick block entities in the same chunks as scheduled fluid ticks.
    pub(crate) fn tick_furnaces(&mut self) -> Vec<(BlockPos, Furnace)> {
        let Some(recipes) = self.recipes.clone() else {
            return Vec::new();
        };
        let ticking: Vec<_> = self
            .active_furnaces
            .iter()
            .copied()
            .filter(|pos| self.wanted_set.contains(&pos.chunk()))
            .collect();
        let mut changed = Vec::new();
        for pos in ticking {
            match self.tick_furnace(pos, &recipes) {
                Ok(Some(furnace)) => changed.push((pos, furnace)),
                Ok(None) => {}
                Err(error) => {
                    self.active_furnaces.remove(&pos);
                    warn!("Cannot tick furnace at {pos:?}: {error}");
                }
            }
        }
        changed
    }

    /// The fluid ticks to save with a chunk: its own, when it has some or was read with the list, so that a chunk
    /// without them is saved as it was.
    fn saved_fluid_ticks(&self, pos: ChunkPos, chunk: &Chunk) -> Option<Nbt> {
        let ticks = self.fluid_ticks.pack(pos, self.game_time);
        (!ticks.is_empty() || chunk.rest.get("fluid_ticks").is_some())
            .then(|| ticks::write_saved(&ticks, |kind| kind.name()))
    }

    /// Marks a chunk as changed for the next save without editing it, as when its scheduled ticks change.
    pub(crate) fn mark_changed(&mut self, pos: ChunkPos) {
        if !matches!(self.chunks.get(&pos), Some(ChunkState::Loaded(_))) {
            return;
        }
        let changes = self.changes.entry(pos).or_default();
        changes.terrain_changed();
    }

    /// Whether a lit chunk waits to be lit again.
    fn light_pending(&self, pos: ChunkPos) -> bool {
        self.relights(pos)
            && (self.unlit.contains(&pos)
                || matches!(self.light.get(&pos), Some(LightState::Computing(_))))
    }

    /// Takes the sections whose light changed since the last call, by chunk.
    pub(crate) fn take_light_updates(&mut self) -> Vec<(ChunkPos, Arc<Light>)> {
        std::mem::take(&mut self.light_updates)
    }

    /// Keeps the light the workers computed for a chunk, unless the chunk changed or left since the request.
    fn finish_light(&mut self, lit: light::Lit) {
        // A chunk that changed or left since the request has no entry with this request's ID.
        if !matches!(self.light.get(&lit.pos), Some(LightState::Computing(id)) if *id == lit.id) {
            return;
        }
        if self.relights(lit.pos) {
            self.light.remove(&lit.pos);
            self.finish_relight(lit);
            return;
        }
        if lit.light.is_none() {
            error!(
                "Cannot compute the light of chunk {}, {}; it is sent without light",
                lit.pos.x, lit.pos.z
            );
        }
        if let Some(light) = &lit.light {
            // For the players who had the chunk before it changed.
            self.light_updates.push((lit.pos, Arc::clone(light)));
        }
        let saved = lit.light.is_some() && self.saves_light(lit.pos);
        self.light.insert(lit.pos, LightState::Ready(lit.light));
        // Saved again, now with its light.
        if saved {
            let changes = self.changes.entry(lit.pos).or_default();
            changes.terrain_changed();
        }
    }

    /// Writes the new light of a lit chunk into its sections, and keeps the sections that changed for the
    /// players. A chunk whose light cannot be computed is saved without `isLightOn`.
    fn finish_relight(&mut self, lit: light::Lit) {
        let Some(ChunkState::Loaded(chunk)) = self.chunks.get_mut(&lit.pos) else {
            return;
        };
        let chunk = Arc::make_mut(chunk);
        match lit.light {
            Some(light) => {
                let changed = light::changed_sections(&light::saved_light(chunk, &light), &light);
                light::write_light(chunk, &light);
                if !changed.sections.is_empty() {
                    self.light_updates.push((lit.pos, Arc::new(changed)));
                }
            }
            None => {
                error!(
                    "Cannot compute the light of chunk {}, {}; vanilla will light it again",
                    lit.pos.x, lit.pos.z
                );
                chunk.rest.insert("isLightOn", Nbt::Byte(0));
            }
        }
        let changes = self.changes.entry(lit.pos).or_default();
        changes.terrain_changed();
    }

    /// Waits until every loaded chunk saved with light whose neighbors are loaded has it, so that shutdown
    /// saves it.
    fn complete_light(&mut self) -> io::Result<()> {
        loop {
            self.update_light()?;
            let saved = |pos| self.saves_light(pos) || self.relights(pos);
            let pending = self
                .light
                .iter()
                .any(|(&pos, state)| matches!(state, LightState::Computing(_)) && saved(pos))
                || self
                    .unlit
                    .iter()
                    .any(|&pos| saved(pos) && self.light_neighbors(pos).is_some());
            if !pending {
                return Ok(());
            }
            let lit = self
                .lighter
                .completed
                .recv()
                .map_err(|_| io::Error::other("chunk light workers stopped"))?;
            self.finish_light(lit);
        }
    }

    /// Takes the light the workers computed and gives them the chunks whose neighbors have arrived.
    fn update_light(&mut self) -> io::Result<()> {
        loop {
            match self.lighter.completed.try_recv() {
                Ok(lit) => self.finish_light(lit),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err(io::Error::other("chunk light workers stopped"));
                }
            }
        }
        let mut queued = Vec::new();
        for &pos in &self.unlit {
            let Some(ChunkState::Loaded(chunk)) = self.chunks.get(&pos) else {
                queued.push(pos);
                continue;
            };
            let Some(neighbors) = self.light_neighbors(pos) else {
                continue;
            };
            let job = light::Job {
                pos,
                id: self.next_light_job,
                chunk: Arc::clone(chunk),
                neighbors,
            };
            match self
                .lighter
                .requests
                .as_ref()
                .expect("lighter is running")
                .try_send(job)
            {
                Ok(()) => {
                    self.light
                        .insert(pos, LightState::Computing(self.next_light_job));
                    self.next_light_job += 1;
                    queued.push(pos);
                }
                Err(TrySendError::Full(_)) => break,
                Err(TrySendError::Disconnected(_)) => {
                    return Err(io::Error::other("chunk light workers stopped"));
                }
            }
        }
        for pos in queued {
            self.unlit.remove(&pos);
        }
        Ok(())
    }

    /// Edits a loaded chunk, marking its complete contents as changed for the next save.
    /// Previously published snapshots remain immutable while the edit uses copy-on-write.
    pub fn chunk_mut(&mut self, pos: ChunkPos) -> Option<&mut Chunk> {
        let ChunkState::Loaded(_) = self.chunks.get(&pos)? else {
            return None;
        };
        self.invalidate_light(pos);
        self.chunk_contents_mut(pos)
    }

    /// Edits saved metadata without invalidating light, which only depends on block states.
    pub(crate) fn chunk_contents_mut(&mut self, pos: ChunkPos) -> Option<&mut Chunk> {
        self.mark_changed(pos);
        let ChunkState::Loaded(chunk) = self.chunks.get_mut(&pos)? else {
            return None;
        };
        Some(Arc::make_mut(chunk))
    }

    /// Requests an asynchronous save of all changed chunks without waiting for disk. If the bounded queue is
    /// full, later updates take the remaining snapshots as space becomes available.
    pub fn autosave(&mut self) -> io::Result<()> {
        self.sync_entity_changes()?;
        for changes in self.changes.values_mut() {
            changes.requested = true;
        }
        self.queue_saves()
    }

    /// Waits for the chunk workers' current requests, for the light of the chunks saved with it, and for all
    /// changed chunks to reach disk, then saves the chunks the generator holds in memory. Intended for shutdown,
    /// not the tick loop.
    pub fn flush(&mut self) -> io::Result<()> {
        let chunks = self.flush_chunks();
        let players = self
            .player_store
            .as_mut()
            .map_or(Ok(()), crate::player_data::PlayerStore::flush);
        chunks.and(players)
    }

    fn flush_chunks(&mut self) -> io::Result<()> {
        loop {
            self.sync_entity_changes()?;
            let pending: Vec<_> = self.entities.changed_chunks.iter().copied().collect();
            self.queue_loads(pending)?;
            if self.in_flight.is_empty() {
                break;
            }
            let completion = self
                .loader
                .completed
                .recv()
                .map_err(|_| io::Error::other("chunk workers stopped"))?;
            let loading = HashSet::from([completion.pos]);
            self.finish_load(completion, &loading)?;
        }
        self.complete_light()?;
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
        regions.retain(|region, _| !self.saving.contains(region));
        let mut entity_snapshots =
            self.snapshot_entities(regions.values().flatten().copied().filter(|pos| {
                let changes = &self.changes[pos];
                changes.entity_revision > changes.saved
            }));
        for (region, positions) in regions {
            let snapshots = positions
                .iter()
                .map(|pos| {
                    let changes = &self.changes[pos];
                    let chunk = (changes.terrain_revision > changes.saved).then(|| {
                        let Some(ChunkState::Loaded(chunk)) = self.chunks.get(pos) else {
                            unreachable!("changed terrain remains loaded until saved");
                        };
                        chunk
                    });
                    Snapshot {
                        pos: *pos,
                        revision: changes.revision,
                        chunk: chunk.cloned(),
                        light: chunk.and_then(|_| self.saved_light(*pos)),
                        stale_light: chunk.is_some() && self.light_pending(*pos),
                        fluid_ticks: chunk.and_then(|chunk| self.saved_fluid_ticks(*pos, chunk)),
                        entities: entity_snapshots.remove(pos),
                    }
                })
                .collect();
            let job = SaveJob {
                snapshots,
                item_codec: self
                    .item_codec
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
            };
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
        let mut entities = None;
        let state = match completion.result {
            Ok((Some((chunk, warnings, is_generated)), loaded_entities)) => {
                entities = loaded_entities;
                generated = is_generated;
                for warning in warnings {
                    warn!("{prefix}: {warning}");
                }
                ChunkState::Loaded(Arc::new(chunk))
            }
            Ok((None, loaded_entities)) => {
                entities = loaded_entities;
                ChunkState::Missing
            }
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
        let kept =
            generated || wanted.contains(&pos) || self.entities.changed_chunks.contains(&pos);
        if generated {
            self.changes.insert(
                pos,
                Changes {
                    revision: 1,
                    terrain_revision: 1,
                    ..Changes::default()
                },
            );
            self.chunks.insert(pos, state);
        } else if kept {
            self.chunks.insert(pos, state);
        }
        if kept && let Some(entities) = entities {
            self.load_entities(pos, entities)?;
        }
        if kept && self.needs_light(pos) {
            self.light.remove(&pos);
            self.unlit.insert(pos);
        }
        if let Some(ChunkState::Loaded(chunk)) = self.chunks.get(&pos)
            && !self.fluid_ticks.has_chunk(pos)
        {
            let saved = ticks::read_saved(chunk.rest.get("fluid_ticks"), FluidType::from_name);
            self.fluid_ticks.add_chunk(pos, saved, self.game_time);
        }
        if kept {
            self.load_furnaces(pos);
        }
        // A lit chunk saved while its light was out of date is lit again, as vanilla would.
        if kept
            && self.relights(pos)
            && matches!(
                self.chunks.get(&pos),
                Some(ChunkState::Loaded(chunk)) if chunk.rest.get("isLightOn") != Some(&Nbt::Byte(1))
            )
        {
            self.unlit.insert(pos);
        }
        Ok(())
    }

    /// [Wants](World::want) `wanted` and [advances](World::advance) the world.
    pub fn update(&mut self, wanted: impl IntoIterator<Item = ChunkPos>) -> io::Result<()> {
        self.want(wanted);
        self.advance()
    }

    /// Sets the chunks to keep loaded, most wanted first. They stay wanted until the next call, so a caller
    /// whose views did not change only [advances](World::advance) the world.
    pub fn want(&mut self, wanted: impl IntoIterator<Item = ChunkPos>) {
        self.wanted.clear();
        self.wanted_set.clear();
        for pos in wanted {
            if self.wanted_set.insert(pos) {
                self.wanted.push(pos);
            }
        }
    }

    /// Applies finished reads and writes, saves the changed chunks that left every view, one region at a
    /// time, and queues requests without waiting for disk.
    /// An incompatible chunk or an unavailable worker pool is fatal and returned to the caller, even if the
    /// incompatible chunk is no longer wanted when its read finishes.
    pub fn advance(&mut self) -> io::Result<()> {
        let wanted = std::mem::take(&mut self.wanted);
        let wanted_set = std::mem::take(&mut self.wanted_set);
        let result = self.advance_for(&wanted, &wanted_set);
        self.wanted = wanted;
        self.wanted_set = wanted_set;
        result
    }

    fn advance_for(
        &mut self,
        wanted: &[ChunkPos],
        wanted_set: &HashSet<ChunkPos>,
    ) -> io::Result<()> {
        self.sync_entity_changes()?;
        loop {
            match self.saver.completed.try_recv() {
                Ok(completion) => self.finish_save(completion)?,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err(io::Error::other("chunk save worker stopped"));
                }
            }
        }
        self.chunks.retain(|pos, _| {
            wanted_set.contains(pos)
                || self.changes.contains_key(pos)
                || self.entities.changed_chunks.contains(pos)
        });
        // Only evict entities after their complete snapshot reached disk.
        for handle in self.entities.order.clone() {
            let [x, _, z] = self.entities.position[self.entities.slot(handle)];
            let pos = ChunkPos::at(x, z);
            if !self.chunks.contains_key(&pos) && !self.entities.changed_chunks.contains(&pos) {
                self.entities.remove(handle);
                self.entities.changed_chunks.remove(&pos);
            }
        }
        self.entity_data
            .retain(|pos, _| self.chunks.contains_key(pos));
        self.light.retain(|pos, _| self.chunks.contains_key(pos));
        self.unlit.retain(|pos| self.chunks.contains_key(pos));
        let chunks = &self.chunks;
        self.fluid_ticks
            .retain_chunks(|pos| chunks.contains_key(&pos));
        self.active_furnaces
            .retain(|pos| chunks.contains_key(&pos.chunk()));
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
                Ok(completion) => self.finish_load(completion, wanted_set)?,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err(io::Error::other("chunk workers stopped"));
                }
            }
        }
        self.update_light()?;
        // Keep drops made before the player's chunk arrives, and items crossing a view boundary, until
        // their destination entity region has been read and merged with the new items.
        let pending: Vec<_> = self.entities.changed_chunks.iter().copied().collect();
        for &pos in wanted {
            self.chunks.entry(pos).or_insert(ChunkState::Loading);
        }
        self.queue_loads(pending.into_iter().chain(wanted.iter().copied()))
    }

    fn queue_loads(&mut self, positions: impl IntoIterator<Item = ChunkPos>) -> io::Result<()> {
        for pos in positions {
            let state = self.chunks.entry(pos).or_insert(ChunkState::Loading);
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
    use harpas_core::chunk::{PalettedContainer, Section, Strategy};
    use harpas_core::nbt::{Compound, Nbt};

    use super::*;
    use crate::block;
    use crate::neighbor;
    use crate::player::{GameMode, Input, Movement, Output, Profile};
    use crate::{Event, Game, tick};
    use harpas_worldgen::shape_update::ShapeUpdates;

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
        rest.insert("isLightOn", Nbt::Byte(1));
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

    /// Waits for the chunk workers, which a loaded CI runner can take many seconds to schedule. The wait ends as
    /// soon as `condition` holds, so the limit only costs time when a test fails.
    fn wait_until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
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
            survival_rules: Default::default(),
            generates: false,
            chunks: HashMap::new(),
            wanted: Vec::new(),
            wanted_set: HashSet::new(),
            in_flight: HashSet::new(),
            changes: HashMap::new(),
            saving: HashSet::new(),
            unload_save_delay: Duration::ZERO,
            light: HashMap::new(),
            block_changes: Vec::new(),
            changed_blocks: HashSet::new(),
            shape_updates: None,
            updater: neighbor::Updater::default(),
            level_events: Vec::new(),
            light_updates: Vec::new(),
            game_time: 0,
            fluid_ticks: LevelTicks::default(),
            active_furnaces: BTreeSet::new(),
            random: LegacyRandom::new(0),
            item_uuids: XoroshiroRandom::new(0),
            entities: crate::entity::Entities::default(),
            entity_data: HashMap::new(),
            item_codec: Arc::new(RwLock::new(Arc::default())),
            loot: None,
            maps: crate::maps::Maps::start(Path::new("test"), None).unwrap(),
            seed: 0,
            recipes: None,
            mining: None,
            climbable_blocks: Vec::new(),
            player_store: None,
            random_sequences: HashMap::new(),
            unlit: HashSet::new(),
            next_light_job: 0,
            source: None,
            loader: Loader::start(workers, load).unwrap(),
            saver: Saver::start(save).unwrap(),
            lighter: Lighter::start(workers, QUEUE_CAPACITY).unwrap(),
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
    fn only_a_player_touching_nothing_but_air_is_floating() {
        let folder = Folder::new();
        let stone = ChunkPos { x: 0, z: 0 };
        let void = ChunkPos { x: 5, z: 5 };
        save(&folder, &[(stone, DATA_VERSION)]);
        let mut world = World::open(&folder.0, BIOMES).unwrap();
        wait_until(|| {
            world.update([stone, void]).unwrap();
            [stone, void].iter().all(|pos| {
                world
                    .chunk(*pos)
                    .is_some_and(|state| !matches!(state, ChunkState::Loading))
            })
        });
        let at = |x: f64, y: f64, z: f64| crate::player::Position {
            x,
            y,
            z,
            ..crate::player::Position::default()
        };

        // The stone ends at Y -48. Just above it is not floating, even off the ground; a block higher is.
        assert!(!world.no_blocks_around(&at(8.5, -47.5, 8.5)));
        assert!(world.no_blocks_around(&at(8.5, -47.0, 8.5)));
        // Stone above the head counts too.
        assert!(!world.no_blocks_around(&at(8.5, -65.5, 8.5)));
        assert!(world.no_blocks_around(&at(8.5, -66.0, 8.5)));
        // A chunk with nothing saved is empty, and one that was never requested is not known to be.
        assert!(world.no_blocks_around(&at(88.5, -60.0, 88.5)));
        assert!(!world.no_blocks_around(&at(0.2, -47.0, 8.5)));
        assert!(!world.no_blocks_around(&at(-8.5, -47.0, 8.5)));

        // A shaft of air in the stone, wide enough for the player's box.
        let states = block_states();
        let air = states.default_state("minecraft:air").unwrap();
        let shaft = at(8.5, -54.0, 8.5);
        fn set(world: &mut World, pos: ChunkPos, [x, y, z]: [usize; 3], state: u32) {
            world.chunk_mut(pos).unwrap().sections[0]
                .section
                .as_mut()
                .unwrap()
                .set_block_state(x, y, z, state);
        }
        for x in 7..=9 {
            for z in 7..=9 {
                for y in 6..16 {
                    set(&mut world, stone, [x, y, z], air);
                }
            }
        }
        assert!(world.no_blocks_around(&shaft));
        // Swimming and climbing: water or a ladder in the player's own block.
        for name in ["minecraft:water", "minecraft:ladder"] {
            let block = states.default_state(name).unwrap();
            set(&mut world, stone, [8, 10, 8], block);
            assert!(!world.no_blocks_around(&shaft), "{name}");
            set(&mut world, stone, [8, 10, 8], air);
        }

        // The flight check follows it: hovering in the shaft is flying, and treading water is not.
        let hover = |world: &World| {
            let (output, receiver) = crate::player::output_channel();
            let mut profile = profile();
            profile.game_mode = GameMode::Survival;
            let mut state = crate::player::PlayerState::new(1, profile, 10, false, output);
            state.awaiting_teleport = None;
            state.player.position = at(shaft.x, shaft.y - 0.01, shaft.z);
            state.tick_start = state.player.position;
            let movement = Movement {
                position: Some([shaft.x, shaft.y, shaft.z]),
                rotation: None,
                on_ground: false,
            };
            state.accepts(movement, Some(world));
            for _ in 0..100 {
                state.check_flight();
            }
            receiver
                .try_iter()
                .any(|message| matches!(message, Output::Disconnect(_)))
        };
        assert!(hover(&world));
        let water = states.default_state("minecraft:water").unwrap();
        set(&mut world, stone, [8, 10, 8], water);
        assert!(!hover(&world));
    }

    #[test]
    fn two_thirds_of_the_hardware_threads_work_on_chunks() {
        let counts = [1, 2, 3, 4, 6, 8, 12, 16, 24, 96].map(worker_count);
        assert_eq!(counts, [1, 1, 2, 2, 4, 5, 8, 10, 16, 16]);
    }

    #[test]
    fn fall_distance_keeps_accumulating_until_the_player_reaches_a_surface() {
        let pos = ChunkPos { x: 0, z: 0 };
        let mut world = fake_world(1, Arc::new(|_| Ok((None, None))));
        world
            .chunks
            .insert(pos, ChunkState::Loaded(Arc::new(chunk(pos, DATA_VERSION))));
        let (output, received) = crate::player::output_channel();
        let mut profile = profile();
        profile.game_mode = GameMode::Survival;
        let player = crate::player::PlayerState::new(1, profile, 2, false, output);
        let mut game = Game {
            pending: Vec::new(),
            players: vec![player],
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(1)),
            world: Some(world),
            viewers: Vec::new(),
            stopped: false,
        };
        let at = |y, on_ground| crate::player::Position {
            x: 8.5,
            y,
            z: 8.5,
            on_ground,
            ..Default::default()
        };
        game.check_fall(0, at(-41.0, false), at(-45.0, true));
        assert_eq!(game.players[0].vitals.health, 20.0);
        game.check_fall(0, at(-45.0, true), at(-48.0, true));
        assert_eq!(game.players[0].vitals.health, 16.0);
        assert!(matches!(
            received.try_recv(),
            Ok(Output::Damage {
                entity_id: 1,
                kind: crate::player::DamageKind::Fall,
            })
        ));
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
        let (output, _received) = crate::player::output_channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: profile(),
                view_distance: 3,
                initial_view_distance: 2,
                allow_flight: false,
                output,
                force_game_mode_on_login: None,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            viewers: Vec::new(),
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
    fn a_view_is_kept_within_a_chunk_and_follows_the_player_into_the_next() {
        let folder = Folder::new();
        let (output, _received) = crate::player::output_channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: profile(),
                view_distance: 2,
                initial_view_distance: 2,
                allow_flight: false,
                output,
                force_game_mode_on_login: None,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            viewers: Vec::new(),
            stopped: false,
        };
        let mut number = 0;
        // The connection sends Play Login and its initial teleport after player data has loaded.
        wait_until(|| {
            number += 1;
            tick(&mut game, number);
            game.players[0].data_ready
        });
        let mut move_to = |game: &mut Game, x: f64| {
            game.pending.push(Event::Input {
                entity_id: 1,
                input: Input::Movement(Movement {
                    position: Some([x, 65.0, 0.5]),
                    rotation: None,
                    on_ground: true,
                }),
            });
            // The move is applied after the first tick's dimension phase.
            for _ in 0..2 {
                number += 1;
                tick(game, number);
            }
        };
        game.pending.push(Event::Input {
            entity_id: 1,
            input: Input::ConfirmTeleport(1),
        });
        move_to(&mut game, 0.5);
        let origin = ChunkPos { x: 0, z: 0 };
        assert_eq!(game.players[0].view.key, Some((origin, 2)));
        let view = game.players[0].view.chunks.as_ptr();

        move_to(&mut game, 3.5);
        assert_eq!(game.players[0].view.key, Some((origin, 2)));
        assert_eq!(game.players[0].view.chunks.as_ptr(), view);
        assert_eq!(game.world.as_ref().unwrap().chunks.len(), 25);

        move_to(&mut game, -0.5);
        let west = ChunkPos { x: -1, z: 0 };
        assert_eq!(game.players[0].view.key, Some((west, 2)));
        assert_eq!(game.players[0].view.chunks, west.view(2));
        let world = game.world.as_ref().unwrap();
        assert_eq!(world.chunks.len(), 25);
        assert!(world.chunk(ChunkPos { x: -3, z: 0 }).is_some());
        assert!(world.chunk(ChunkPos { x: 2, z: 0 }).is_none());
        assert!(
            game.players[0]
                .sent_chunks
                .iter()
                .all(|pos| pos.within(west, 2))
        );
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
        let (output, _received) = crate::player::output_channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: profile(),
                view_distance: 2,
                initial_view_distance: 2,
                allow_flight: false,
                output,
                force_game_mode_on_login: None,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            viewers: Vec::new(),
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
                Ok((Some((chunk(pos, DATA_VERSION), Vec::new(), true)), None))
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
                Ok((Some((chunk(pos, DATA_VERSION), Vec::new(), false)), None))
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
                Ok((None, None))
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
                Ok((Some((chunk(pos, DATA_VERSION), Vec::new(), false)), None))
            })
        };
        let save = {
            let gate = Arc::clone(&gate);
            let writes = Arc::clone(&writes);
            Arc::new(move |job: &SaveJob| {
                assert_eq!(thread::current().name(), Some("harpas-chunk-save"));
                let marker = job.snapshots[0]
                    .chunk
                    .as_ref()
                    .unwrap()
                    .rest
                    .get("Marker")
                    .cloned();
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
        let load = Arc::new(|pos| Ok((Some((chunk(pos, DATA_VERSION), Vec::new(), false)), None)));
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
    fn workers_light_generated_chunks_once_and_again_when_a_chunk_of_the_volume_changes() {
        let load = Arc::new(|pos| {
            let mut chunk = chunk(pos, DATA_VERSION);
            chunk.rest.insert("Status", "minecraft:features");
            Ok((Some((chunk, Vec::new(), false)), None))
        });
        let mut world = fake_world(2, load);
        world.generates = true;
        let center = ChunkPos { x: 0, z: 0 };
        let wanted = center.view(1);
        load_chunks(&mut world, &wanted);
        wait_until(|| {
            world.update(wanted.iter().copied()).unwrap();
            world.light(center).is_some()
        });
        let first = world.light(center).unwrap().unwrap();
        assert_eq!(first.sections.len(), 26);
        // The chunks around it wait for neighbors that are not loaded.
        let corner = ChunkPos { x: 1, z: 1 };
        assert!(world.light(corner).is_none());
        // The light is computed once and kept.
        world.update(wanted.iter().copied()).unwrap();
        assert!(Arc::ptr_eq(&first, &world.light(center).unwrap().unwrap()));

        // A change in a neighbor drops the light computed from it until the workers light the chunk again.
        world.chunk_mut(corner).unwrap().rest.insert("Marker", 1);
        assert!(world.light(center).is_none());
        wait_until(|| {
            world.update(wanted.iter().copied()).unwrap();
            world.light(center).is_some()
        });
        assert!(!Arc::ptr_eq(&first, &world.light(center).unwrap().unwrap()));

        // The light leaves with its chunk.
        wait_until(|| {
            world.update([]).unwrap();
            world.chunks.is_empty()
        });
        assert!(world.light.is_empty() && world.unlit.is_empty());

        // A world that does not generate lights a chunk without waiting for neighbors nobody asked for.
        world.generates = false;
        wait_until(|| {
            world.update([center]).unwrap();
            world.light(center).is_some()
        });
    }

    /// A source that decorates, so every chunk at `features` it hands out is final.
    struct Decorating;

    impl ChunkSource for Decorating {
        fn resumes(&self, status: &str) -> bool {
            status == FEATURES
        }

        fn generate(
            &self,
            _pos: ChunkPos,
            _saved: Option<Compound>,
            _storage: &ChunkStorage,
        ) -> io::Result<Generated> {
            Err(io::Error::other("the test loads every chunk"))
        }

        fn flush(&self, _storage: &ChunkStorage) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn final_chunks_that_ran_features_are_saved_with_their_light_at_shutdown() {
        let center = ChunkPos { x: 0, z: 0 };
        let wanted = center.view(1);
        for decorates in [true, false] {
            let saved = Arc::new(Mutex::new(HashMap::new()));
            let save = {
                let saved = Arc::clone(&saved);
                Arc::new(move |job: &SaveJob| {
                    for snapshot in &job.snapshots {
                        let chunk = snapshot.saved().unwrap().into_owned();
                        saved.lock().unwrap().insert(snapshot.pos, chunk);
                    }
                    Ok(())
                })
            };
            let load = Arc::new(|pos| {
                let mut chunk = chunk(pos, DATA_VERSION);
                chunk.rest.insert("Status", FEATURES);
                Ok((Some((chunk, Vec::new(), false)), None))
            });
            let mut world = fake_world_with_save(2, load, save);
            world.generates = true;
            let source: Arc<dyn ChunkSource> = if decorates {
                Arc::new(Decorating)
            } else {
                generator_source(Arc::new(|_, _: &ChunkReader<'_>| {
                    Err(io::Error::other("the test loads every chunk"))
                }))
            };
            let storage = Arc::new(ChunkStorage::new(PathBuf::from("test"), Vec::new()));
            world.source = Some((source, storage));
            load_chunks(&mut world, &wanted);
            // Shutdown waits for the light of the chunk whose neighbors are loaded, then saves it.
            world.flush().unwrap();
            let saved = saved.lock().unwrap();
            if !decorates {
                // Chunks at `features` that the source did not hand out may still take their neighbors' writes.
                assert!(saved.is_empty());
                continue;
            }
            assert_eq!(saved.keys().copied().collect::<Vec<_>>(), vec![center]);
            let Some(ChunkState::Loaded(chunk)) = world.chunk(center) else {
                panic!("the center stays loaded");
            };
            let light = world.light(center).unwrap().unwrap();
            assert_eq!(saved[&center], light::with_light(chunk, &light));
            assert_eq!(
                saved[&center].rest.get("Status"),
                Some(&Nbt::from("minecraft:light"))
            );
            // The loaded chunk keeps its status and its light in memory, computed again if it changes.
            assert!(world.needs_light(center));
        }
    }

    #[test]
    fn autosave_queue_is_bounded_and_flushes_every_region() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (started, received) = mpsc::channel();
        let writes = Arc::new(Mutex::new(HashSet::new()));
        let load = Arc::new(|pos| Ok((Some((chunk(pos, DATA_VERSION), Vec::new(), false)), None)));
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
        let load = Arc::new(|pos| Ok((Some((chunk(pos, DATA_VERSION), Vec::new(), false)), None)));
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
        let (output, _received) = crate::player::output_channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: profile(),
                view_distance: 2,
                initial_view_distance: 2,
                allow_flight: false,
                output,
                force_game_mode_on_login: None,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            viewers: Vec::new(),
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
    fn a_damaged_neighbor_is_left_as_it_is_when_the_region_is_saved() {
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
        logic.join().unwrap().unwrap();
        // The save wrote the changed chunk beside the damaged entry, which is still there to repair.
        let saved = fs::read(path).unwrap();
        assert_eq!(saved[4..8], damaged[4..8]);
        let region = Region::open(&folder.region_dir(), 0, 0).unwrap();
        assert!(region.chunk(neighbor.x, neighbor.z).is_err());
        let chunk = region.chunk(pos.x, pos.z).unwrap().unwrap();
        assert_eq!(chunk.get("Marker"), Some(&Nbt::Int(1)));
    }

    #[test]
    fn a_region_with_an_entry_past_its_end_stops_shutdown_without_being_overwritten() {
        let folder = Folder::new();
        let pos = ChunkPos { x: 0, z: 0 };
        let neighbor = ChunkPos { x: 1, z: 0 };
        save(&folder, &[(pos, DATA_VERSION), (neighbor, DATA_VERSION)]);
        let mut world = World::open(&folder.0, BIOMES).unwrap();
        load_chunks(&mut world, &[pos]);
        world.chunk_mut(pos).unwrap().rest.insert("Marker", 1);
        let path = folder.region_dir().join("r.0.0.mca");
        let mut damaged = fs::read(&path).unwrap();
        // A new chunk written at the end of the file would otherwise be read as this one.
        damaged[4..8].copy_from_slice(&0x0010_0001_u32.to_be_bytes());
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
        let (output, _received) = crate::player::output_channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: profile(),
                view_distance: 10,
                initial_view_distance: 10,
                allow_flight: false,
                output,
                force_game_mode_on_login: None,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            viewers: Vec::new(),
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
        let (output, received) = crate::player::output_channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: profile(),
                view_distance: 2,
                initial_view_distance: 2,
                allow_flight: false,
                output,
                force_game_mode_on_login: None,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            viewers: Vec::new(),
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
        let (output, received) = crate::player::output_channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: profile(),
                view_distance: 2,
                initial_view_distance: 2,
                allow_flight: false,
                output,
                force_game_mode_on_login: None,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            viewers: Vec::new(),
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
        let (output, received) = crate::player::output_channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: profile(),
                view_distance: 3,
                initial_view_distance: 3,
                allow_flight: false,
                output,
                force_game_mode_on_login: None,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            viewers: Vec::new(),
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
    fn a_panic_on_the_logic_thread_still_saves_the_world() {
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
        let load = Arc::new(|pos| Ok((Some((chunk(pos, DATA_VERSION), Vec::new(), false)), None)));
        let mut world = fake_world_with_save(1, load, save);
        let pos = ChunkPos { x: 0, z: 0 };
        load_chunks(&mut world, &[pos]);
        world.chunk_mut(pos).unwrap().rest.insert("Marker", 1);

        let (handle, logic) = crate::start_game(Some(world)).unwrap();
        handle.events.send(crate::Event::Panic).unwrap();
        let error = logic.join().unwrap().unwrap_err();
        assert!(error.to_string().contains("panicked"));
        assert_eq!(*saved.lock().unwrap(), vec![pos]);
    }

    #[test]
    fn incompatible_chunk_stops_the_running_logic_thread() {
        let folder = Folder::new();
        save(&folder, &[(ChunkPos { x: 0, z: 0 }, DATA_VERSION - 1)]);
        let (handle, logic) =
            crate::start_with_world(&folder.0, BIOMES, crate::GameData::default()).unwrap();
        let session = handle.player_joined(1, profile(), 10, false).unwrap();
        wait_until(|| logic.is_finished());
        // The chunks around the incompatible one may be read, and sent, a tick before it.
        let mut outputs = std::iter::from_fn(|| session.try_recv());
        assert!(outputs.any(|output| output == Output::Disconnect("Cannot access the world")));
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

    #[test]
    fn players_in_survival_break_blocks_as_fast_as_vanilla_while_others_see_the_cracks() {
        let folder = Folder::new();
        let pos = ChunkPos { x: 0, z: 0 };
        save(&folder, &[(pos, DATA_VERSION)]);
        let (miner, from_miner) = crate::player::output_channel();
        let (watcher, from_watcher) = crate::player::output_channel();
        let joined = |entity_id: i32, output| Event::Joined {
            entity_id,
            profile: Profile {
                uuid: entity_id as u128,
                game_mode: GameMode::Survival,
                ..profile()
            },
            view_distance: 2,
            initial_view_distance: 2,
            allow_flight: false,
            output,
            force_game_mode_on_login: None,
        };
        let mut game = Game {
            pending: vec![joined(1, miner), joined(2, watcher)],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            viewers: Vec::new(),
            stopped: false,
        };
        let mut number = 1;
        tick(&mut game, number);
        wait_until(|| {
            // As the clients acknowledge each batch, so that a chunk read after the first batch is sent too.
            for player in &mut game.players {
                player.awaiting_chunk_batch = false;
            }
            number += 1;
            tick(&mut game, number);
            game.players.iter().all(|p| p.sent_chunks.contains(&pos))
        });
        for player in &mut game.players {
            player.player.position = crate::player::Position {
                x: 0.5,
                y: -48.0,
                z: 0.5,
                on_ground: true,
                ..player.player.position
            };
        }
        let states = block_states();
        let dirt = states.default_state("minecraft:dirt").unwrap();
        let grass = states.default_state("minecraft:short_grass").unwrap();
        let air = states.default_state("minecraft:air").unwrap();
        let world = game.world.as_mut().unwrap();
        let grass_at = BlockPos { x: 1, y: -49, z: 0 };
        let early = BlockPos { x: 2, y: -49, z: 0 };
        let in_time = BlockPos { x: 0, y: -49, z: 2 };
        world.set_block_state(grass_at, grass);
        world.set_block_state(early, dirt);
        world.set_block_state(in_time, dirt);
        let mut step = |game: &mut Game, inputs: &[(block::DigAction, BlockPos)]| {
            for &(action, position) in inputs {
                game.pending.push(Event::Input {
                    entity_id: 1,
                    input: Input::Dig {
                        action,
                        position,
                        sequence: 1,
                    },
                });
            }
            number += 1;
            tick(game, number);
        };
        step(&mut game, &[]);
        from_miner.try_iter().for_each(drop);
        from_watcher.try_iter().for_each(drop);
        let block = |game: &Game, pos| game.world.as_ref().unwrap().block_state(pos);

        // A block of hardness 0 breaks as soon as it is hit.
        step(&mut game, &[(block::DigAction::Start, grass_at)]);
        assert_eq!(block(&game, grass_at), Some(air));

        // Dirt takes 15 ticks by hand: 1 / 0.5 / 30 of it each tick. A client that says it finished at once
        // is too early, so the server breaks the block when its own progress gets there.
        step(
            &mut game,
            &[
                (block::DigAction::Start, early),
                (block::DigAction::Finish, early),
            ],
        );
        for _ in 0..13 {
            step(&mut game, &[]);
            assert_eq!(block(&game, early), Some(dirt));
        }
        step(&mut game, &[]);
        assert_eq!(block(&game, early), Some(air));
        let stages: Vec<_> = from_watcher
            .try_iter()
            .filter_map(|output| match output {
                Output::BlockDestruction {
                    entity_id: 1,
                    position,
                    stage,
                } if position == early => Some(stage),
                _ => None,
            })
            .collect();
        // The tick it breaks reaches stage 10, which removes the cracks.
        assert_eq!(stages, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);

        // Finishing once 70% of the block is broken breaks it at once: 10 ticks after the start, the server
        // counts 11 ticks of progress, 11 / 15 of the block.
        step(&mut game, &[(block::DigAction::Start, in_time)]);
        for _ in 0..9 {
            step(&mut game, &[]);
            assert_eq!(block(&game, in_time), Some(dirt));
        }
        step(&mut game, &[(block::DigAction::Finish, in_time)]);
        assert_eq!(block(&game, in_time), Some(air));
        assert!(
            from_miner
                .try_iter()
                .all(|output| !matches!(output, Output::BlockUpdate { .. }))
        );
    }

    #[test]
    fn survival_pickaxes_progress_from_wood_to_obsidian() {
        use crate::inventory::{HOTBAR, ItemStack};
        use harpas_core::block_behavior::Face;
        use harpas_core::item::items;
        use harpas_worldgen::{BlockTagDef, BlockTagEntry, WorldgenData};
        use serde_json::json;

        let folder = Folder::new();
        let target = BlockPos { x: 8, y: -51, z: 9 };
        let table = BlockPos {
            x: 7,
            z: 8,
            ..target
        };
        let furnace = BlockPos {
            x: 9,
            z: 8,
            ..target
        };
        save(&folder, &[(target.chunk(), DATA_VERSION)]);
        let mut rules = WorldgenData::default();
        for (tag, blocks) in [
            (
                "mineable/pickaxe",
                &["stone", "coal_ore", "iron_ore", "diamond_ore", "obsidian"][..],
            ),
            ("needs_stone_tool", &["iron_ore"][..]),
            ("needs_iron_tool", &["diamond_ore"][..]),
            ("needs_diamond_tool", &["obsidian"][..]),
            (
                "incorrect_for_wooden_tool",
                &["#needs_stone_tool", "#incorrect_for_stone_tool"][..],
            ),
            (
                "incorrect_for_stone_tool",
                &["#needs_iron_tool", "#incorrect_for_iron_tool"][..],
            ),
            ("incorrect_for_iron_tool", &["#needs_diamond_tool"][..]),
            ("incorrect_for_diamond_tool", &[][..]),
        ] {
            rules.block_tags.insert(
                format!("minecraft:{tag}"),
                BlockTagDef {
                    replace: false,
                    values: blocks
                        .iter()
                        .map(|name| BlockTagEntry {
                            id: name.strip_prefix('#').map_or_else(
                                || format!("minecraft:{name}"),
                                |tag| format!("#minecraft:{tag}"),
                            ),
                            required: true,
                        })
                        .collect(),
                },
            );
        }
        let pickaxe_recipe = |material, pickaxe| {
            json!({
                "type": "minecraft:crafting_shaped", "pattern": ["MMM", " S ", " S "],
                "key": {"M": material, "S": "minecraft:stick"}, "result": {"id": pickaxe},
            })
        };
        let mut loot = crate::loot::LootTables::default();
        for (block, item) in [
            ("stone", "cobblestone"),
            ("coal_ore", "coal"),
            ("iron_ore", "raw_iron"),
            ("diamond_ore", "diamond"),
            ("obsidian", "obsidian"),
        ] {
            loot.insert(
                &format!("minecraft:{block}"),
                &json!({"pools": [{"rolls": 1,
                    "entries": [{"type": "minecraft:item", "name": format!("minecraft:{item}")}]
                }]}),
            );
        }
        let mut world = World::open(&folder.0, BIOMES).unwrap();
        world.mining = Some(Arc::new(crate::mining::MiningRules::new(&rules).unwrap()));
        world.loot = Some(Arc::new(loot));
        world.recipes = Some(Arc::new(crate::recipe::Recipes::from_json(
            &HashMap::new(),
            &[
                pickaxe_recipe("minecraft:oak_planks", "minecraft:wooden_pickaxe"),
                pickaxe_recipe("minecraft:cobblestone", "minecraft:stone_pickaxe"),
                pickaxe_recipe("minecraft:iron_ingot", "minecraft:iron_pickaxe"),
                pickaxe_recipe("minecraft:diamond", "minecraft:diamond_pickaxe"),
                json!({"type": "minecraft:smelting", "ingredient": "minecraft:raw_iron", "result": {"id": "minecraft:iron_ingot"}}),
            ],
        )));
        let (sender, received) = crate::player::output_channel();
        let mut game = Game {
            pending: Vec::new(),
            players: Vec::new(),
            online_players: Arc::default(),
            world: Some(world),
            viewers: Vec::new(),
            stopped: false,
        };
        game.join(
            1,
            Profile {
                game_mode: GameMode::Survival,
                ..profile()
            },
            (2, 2),
            false,
            sender,
            None,
        );
        wait_until(|| {
            game.players[0].awaiting_chunk_batch = false;
            tick(&mut game, 1);
            game.players[0].sent_chunks.contains(&target.chunk())
        });
        let position = crate::player::Position {
            x: 8.5,
            y: -51.0,
            z: 7.5,
            on_ground: true,
            ..Default::default()
        };
        game.players[0].awaiting_teleport = None;
        game.players[0].player.position = position;
        let state = |name: &str| {
            block_states()
                .default_state(&format!("minecraft:{name}"))
                .unwrap()
        };
        let stack = |name: &str, count| ItemStack {
            item: items().id(name).unwrap(),
            count,
            ..Default::default()
        };
        let world = game.world.as_mut().unwrap();
        world.set_block_state(table, state("crafting_table"));
        world.set_block_state(furnace, state("furnace"));
        game.players[0].inventory.slots[HOTBAR] = stack("minecraft:oak_planks", 3);
        game.players[0].inventory.slots[HOTBAR + 1] = stack("minecraft:stick", 8);

        let hotbar = |game: &Game, name| {
            game.players[0].inventory.slots[HOTBAR..HOTBAR + 9]
                .iter()
                .position(|stack| stack.name() == Some(name))
        };
        let click = |game: &mut Game, slot, button, mode| {
            game.click_container(
                0,
                game.players[0].container_id(),
                -1,
                slot,
                button,
                mode,
                Vec::new(),
                None,
            );
        };
        let craft = |game: &mut Game, material, product| {
            game.use_item_on(0, false, table, Face::Up, [0.5; 3], 1);
            assert_ne!(game.players[0].container_id(), 0);
            click(game, 37 + hotbar(game, material).unwrap() as i16, 0, 0);
            for slot in 1..=3 {
                click(game, slot, 1, 0);
            }
            let sticks = 37 + hotbar(game, "minecraft:stick").unwrap() as i16;
            click(game, sticks, 0, 0);
            click(game, 5, 1, 0);
            click(game, 8, 1, 0);
            click(game, sticks, 0, 0);
            click(game, 0, 0, 1);
            game.close_container(0, game.players[0].container_id());
            game.players[0].inventory.selected = hotbar(game, product).unwrap();
        };
        let mine = |game: &mut Game, block, ticks, drop| {
            let old = state(block);
            game.world.as_mut().unwrap().set_block_state(target, old);
            game.dig(0, block::DigAction::Start, target, 1);
            game.dig(0, block::DigAction::Finish, target, 2);
            for _ in 1..ticks {
                assert_eq!(game.block_at(target), old);
                game.tick_digging();
            }
            assert_eq!(game.block_at(target), state("air"));
            let world = game.world.as_mut().unwrap();
            if let Some(name) = drop {
                let [handle] = world.entities.order[..] else {
                    panic!("expected one drop")
                };
                assert_eq!(world.entities.item(handle).unwrap().stack, stack(name, 1));
                for _ in 0..10 {
                    world.tick_entities();
                }
                let [x, y, z] = world.entities.position[world.entities.slot(handle)];
                game.players[0].player.position = crate::player::Position {
                    x,
                    y,
                    z,
                    ..position
                };
                game.pick_up_items();
                game.players[0].player.position = position;
            }
            assert!(game.world.as_ref().unwrap().entities.order.is_empty());
        };
        let damage = |game: &Game| {
            let inventory = &game.players[0].inventory;
            let stack = &inventory.slots[inventory.hand_slot(false)];
            let tag = ItemCodec::default().write(stack).unwrap();
            let Some(Nbt::Compound(components)) = tag.get("components") else {
                panic!("missing tool components")
            };
            components.get("minecraft:damage").cloned()
        };

        craft(
            &mut game,
            "minecraft:oak_planks",
            "minecraft:wooden_pickaxe",
        );
        for _ in 0..3 {
            mine(&mut game, "stone", 23, Some("minecraft:cobblestone"));
        }
        mine(&mut game, "coal_ore", 45, Some("minecraft:coal"));
        mine(&mut game, "iron_ore", 150, None);
        assert_eq!(damage(&game), Some(Nbt::Int(5)));
        assert_eq!(hotbar(&game, "minecraft:raw_iron"), None);
        let inventory = &mut game.players[0].inventory;
        let slot = inventory.hand_slot(false);
        inventory.slots[slot] = ItemCodec::default()
            .read(&Compound::from_iter([
                ("id", Nbt::from("minecraft:wooden_pickaxe")),
                (
                    "components",
                    Nbt::from(Compound::from_iter([("minecraft:damage", 58)])),
                ),
            ]))
            .unwrap();
        mine(&mut game, "stone", 23, Some("minecraft:cobblestone"));
        assert_eq!(hotbar(&game, "minecraft:wooden_pickaxe"), None);
        assert!(
            received
                .try_iter()
                .any(|output| output == Output::MainHandItemBroken(1))
        );

        craft(
            &mut game,
            "minecraft:cobblestone",
            "minecraft:stone_pickaxe",
        );
        mine(&mut game, "diamond_ore", 75, None);
        assert_eq!(hotbar(&game, "minecraft:diamond"), None);
        for _ in 0..3 {
            mine(&mut game, "iron_ore", 23, Some("minecraft:raw_iron"));
        }
        assert_eq!(damage(&game), Some(Nbt::Int(4)));
        game.use_item_on(0, false, furnace, Face::Up, [0.5; 3], 3);
        for name in ["minecraft:raw_iron", "minecraft:coal"] {
            let slot = 30 + hotbar(&game, name).unwrap() as i16;
            click(&mut game, slot, 0, 1);
        }
        let world = game.world.as_mut().unwrap();
        for _ in 0..600 {
            world.tick_furnaces();
        }
        assert_eq!(
            world.furnace(furnace).unwrap().slots[2],
            stack("minecraft:iron_ingot", 3)
        );
        click(&mut game, 2, 0, 1);
        game.close_container(0, game.players[0].container_id());
        craft(&mut game, "minecraft:iron_ingot", "minecraft:iron_pickaxe");
        for _ in 0..3 {
            mine(&mut game, "diamond_ore", 15, Some("minecraft:diamond"));
        }
        mine(&mut game, "obsidian", 834, None);
        assert_eq!(damage(&game), Some(Nbt::Int(4)));
        assert_eq!(hotbar(&game, "minecraft:obsidian"), None);

        craft(&mut game, "minecraft:diamond", "minecraft:diamond_pickaxe");
        mine(&mut game, "obsidian", 188, Some("minecraft:obsidian"));
        assert!(hotbar(&game, "minecraft:obsidian").is_some());
        assert_eq!(damage(&game), Some(Nbt::Int(1)));

        game.leave(1);
        game.world
            .as_mut()
            .unwrap()
            .player_store
            .as_mut()
            .unwrap()
            .flush()
            .unwrap();
        let (sender, _received) = crate::player::output_channel();
        game.join(
            2,
            Profile {
                game_mode: GameMode::Survival,
                ..profile()
            },
            (2, 2),
            false,
            sender,
            None,
        );
        wait_until(|| {
            tick(&mut game, 2);
            game.players[0].data_ready
        });
        assert_eq!(
            Some(game.players[0].inventory.selected),
            hotbar(&game, "minecraft:diamond_pickaxe")
        );
        assert_eq!(damage(&game), Some(Nbt::Int(1)));
    }

    #[test]
    fn players_in_creative_break_blocks_that_others_see_and_the_world_saves() {
        let folder = Folder::new();
        let pos = ChunkPos { x: 0, z: 0 };
        save(&folder, &[(pos, DATA_VERSION)]);
        let (creative, from_creative) = crate::player::output_channel();
        let (survival, from_survival) = crate::player::output_channel();
        let joined = |entity_id: i32, game_mode, output| Event::Joined {
            entity_id,
            profile: Profile {
                uuid: entity_id as u128,
                game_mode,
                ..profile()
            },
            view_distance: 2,
            initial_view_distance: 2,
            allow_flight: false,
            output,
            force_game_mode_on_login: None,
        };
        let mut game = Game {
            pending: vec![
                joined(1, GameMode::Creative, creative),
                joined(2, GameMode::Survival, survival),
            ],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            viewers: Vec::new(),
            stopped: false,
        };
        let mut number = 1;
        tick(&mut game, number);
        wait_until(|| {
            // As the clients acknowledge each batch, so that a chunk read after the first batch is sent too.
            for player in &mut game.players {
                player.awaiting_chunk_batch = false;
            }
            number += 1;
            tick(&mut game, number);
            game.players.iter().all(|p| p.sent_chunks.contains(&pos))
        });
        for player in &mut game.players {
            player.player.position = crate::player::Position {
                x: 0.5,
                y: -48.0,
                z: 0.5,
                ..player.player.position
            };
        }
        from_creative.try_iter().for_each(drop);
        from_survival.try_iter().for_each(drop);

        let stone = block_states().default_state("minecraft:stone").unwrap();
        let air = block_states().default_state("minecraft:air").unwrap();
        let dig = |game: &mut Game, entity_id, action, position, sequence| {
            game.pending.push(Event::Input {
                entity_id,
                input: Input::Dig {
                    action,
                    position,
                    sequence,
                },
            });
        };
        let broken = BlockPos { x: 1, y: -49, z: 0 };
        let far = BlockPos { x: 9, y: -49, z: 0 };
        dig(&mut game, 1, block::DigAction::Start, broken, 3);
        dig(&mut game, 1, block::DigAction::Start, far, 4);
        dig(
            &mut game,
            2,
            block::DigAction::Start,
            BlockPos { x: 0, ..broken },
            7,
        );
        number += 1;
        tick(&mut game, number);
        let world = game.world.as_ref().unwrap();
        assert_eq!(world.block_state(broken), Some(air));
        assert_eq!(world.block_state(far), Some(stone));
        assert_eq!(world.block_state(BlockPos { x: 0, ..broken }), Some(stone));
        // Stone takes a player in Survival many ticks to break, so the other player sees it start to crack.
        assert_eq!(
            from_creative.try_iter().collect::<Vec<_>>(),
            [
                Output::BlockDestruction {
                    entity_id: 2,
                    position: BlockPos { x: 0, ..broken },
                    stage: 0,
                },
                Output::AcknowledgeBlockChanges(4),
            ]
        );
        assert_eq!(
            from_survival.try_iter().collect::<Vec<_>>(),
            [
                Output::WorldEvent {
                    event: 2001,
                    position: broken,
                    data: stone as i32,
                },
                Output::AcknowledgeBlockChanges(7),
            ]
        );
        // The change reaches everyone who has the chunk at the next "sending block changes".
        number += 1;
        tick(&mut game, number);
        for received in [&from_creative, &from_survival] {
            assert_eq!(
                received.try_iter().collect::<Vec<_>>(),
                [Output::SectionBlocks(Arc::from([(broken, air)]))]
            );
        }

        let mut world = game.world.take().unwrap();
        world.flush().unwrap();
        drop(world);
        let mut reopened = World::open(&folder.0, BIOMES).unwrap();
        load_chunks(&mut reopened, &[pos]);
        assert_eq!(reopened.block_state(broken), Some(air));
    }

    /// The shape rules with the tags they read: those the tests use hold their blocks, the others are empty.
    fn shape_updates() -> Arc<ShapeUpdates> {
        use harpas_worldgen::data::{BlockTagDef, BlockTagEntry};
        let tag = |blocks: &[&str]| BlockTagDef {
            replace: false,
            values: blocks
                .iter()
                .map(|id| BlockTagEntry {
                    id: format!("minecraft:{id}"),
                    required: true,
                })
                .collect(),
        };
        let mut data = harpas_worldgen::WorldgenData::default();
        let blocks: [(&str, &[&str]); 25] = [
            ("supports_vegetation", &["dirt", "grass_block"]),
            ("supports_mangrove_propagule", &[]),
            ("supports_hanging_mangrove_propagule", &[]),
            ("supports_dry_vegetation", &[]),
            ("cannot_support_seagrass", &[]),
            ("overrides_mushroom_light_requirement", &[]),
            ("cannot_support_snow_layer", &[]),
            ("support_override_snow_layer", &[]),
            ("supports_lily_pad", &[]),
            ("supports_cocoa", &[]),
            ("cannot_support_kelp", &[]),
            ("supports_bamboo", &[]),
            ("supports_azalea", &[]),
            ("supports_big_dripleaf", &[]),
            ("supports_small_dripleaf", &[]),
            ("snow", &["snow", "snow_block"]),
            ("leaves", &[]),
            ("shulker_boxes", &[]),
            ("fences", &[]),
            ("wooden_fences", &[]),
            ("walls", &[]),
            ("fence_gates", &[]),
            ("wall_post_override", &[]),
            ("water", &[]),
            ("lava", &[]),
        ];
        for (name, blocks) in blocks {
            data.block_tags
                .insert(format!("minecraft:{name}"), tag(blocks));
        }
        for name in ["supports_lily_pad", "water"] {
            data.fluid_tags
                .insert(format!("minecraft:{name}"), tag(&[]));
        }
        Arc::new(ShapeUpdates::new(&data).unwrap())
    }

    #[test]
    fn breaking_a_block_breaks_what_it_held_and_updates_its_neighbors() {
        let folder = Folder::new();
        let pos = ChunkPos { x: 0, z: 0 };
        save(&folder, &[(pos, DATA_VERSION)]);
        let (breaker, from_breaker) = crate::player::output_channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: profile(),
                view_distance: 2,
                initial_view_distance: 2,
                allow_flight: false,
                output: breaker,
                force_game_mode_on_login: None,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            viewers: Vec::new(),
            stopped: false,
        };
        game.world
            .as_mut()
            .unwrap()
            .set_shape_updates(shape_updates());
        let mut number = 1;
        tick(&mut game, number);
        wait_until(|| {
            // As the client acknowledges each batch, so that a chunk read after the first batch is sent too.
            game.players[0].awaiting_chunk_batch = false;
            number += 1;
            tick(&mut game, number);
            game.players[0].sent_chunks.contains(&pos)
        });
        game.players[0].player.position = crate::player::Position {
            x: 7.5,
            y: -48.0,
            z: 7.5,
            ..game.players[0].player.position
        };
        let states = block_states();
        let state = |name: &str, properties: &[(&str, &str)]| {
            states.state(name, properties.iter().copied()).unwrap().0
        };
        let air = state("minecraft:air", &[]);
        let torch = state("minecraft:torch", &[]);
        let lower = state("minecraft:oak_door", &[("half", "lower")]);
        let upper = state("minecraft:oak_door", &[("half", "upper")]);
        let grass = |snowy| state("minecraft:grass_block", &[("snowy", snowy)]);
        let snow = state("minecraft:snow", &[]);
        let at = |x, y, z| BlockPos { x, y, z };
        let world = game.world.as_mut().unwrap();
        world.set_block_state(at(5, -49, 7), torch);
        world.set_block_state(at(9, -50, 7), lower);
        world.set_block_state(at(9, -49, 7), upper);
        world.set_block_state(at(7, -50, 9), grass("true"));
        world.set_block_state(at(7, -49, 9), snow);
        number += 1;
        tick(&mut game, number);
        from_breaker.try_iter().for_each(drop);
        let mut dig = |game: &mut Game, position| {
            game.pending.push(Event::Input {
                entity_id: 1,
                input: Input::Dig {
                    action: block::DigAction::Start,
                    position,
                    sequence: 1,
                },
            });
            number += 1;
            tick(game, number);
        };
        let block = |game: &Game, pos| game.world.as_ref().unwrap().block_state(pos);
        let events = |received: Vec<Output>| -> Vec<(BlockPos, u32)> {
            received
                .into_iter()
                .filter_map(|output| match output {
                    Output::WorldEvent {
                        event: 2001,
                        position,
                        data,
                    } => Some((position, data as u32)),
                    _ => None,
                })
                .collect()
        };

        // A torch breaks with the block below it, and the breaker sees it break.
        dig(&mut game, at(5, -50, 7));
        assert_eq!(block(&game, at(5, -49, 7)), Some(air));
        assert_eq!(
            events(from_breaker.try_iter().collect()),
            [(at(5, -49, 7), torch)]
        );

        // The lower half of a door breaks with the block below it, and the upper half with the lower one.
        dig(&mut game, at(9, -51, 7));
        assert_eq!(block(&game, at(9, -50, 7)), Some(air));
        assert_eq!(block(&game, at(9, -49, 7)), Some(air));
        assert_eq!(
            events(from_breaker.try_iter().collect()),
            [(at(9, -50, 7), lower), (at(9, -49, 7), upper)]
        );

        // A grass block is no longer snowy once the snow on it is gone.
        dig(&mut game, at(7, -49, 9));
        assert_eq!(block(&game, at(7, -50, 9)), Some(grass("false")));
        // Every change reaches the player at the next "sending block changes", in the order it happened: the
        // door's, sent while the snow broke, then the snow's.
        number += 1;
        tick(&mut game, number);
        let changed: Vec<_> = from_breaker
            .try_iter()
            .filter_map(|output| match output {
                Output::SectionBlocks(blocks) => Some(blocks.to_vec()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(
            changed,
            [
                (at(9, -51, 7), air),
                (at(9, -50, 7), air),
                (at(9, -49, 7), air),
                (at(7, -49, 9), air),
                (at(7, -50, 9), grass("false")),
            ]
        );
    }

    /// The block light saved at a block of a loaded chunk, as vanilla's `DataLayer` packs it.
    fn saved_block_light(world: &World, pos: BlockPos) -> u8 {
        let Some(ChunkState::Loaded(chunk)) = world.chunk(pos.chunk()) else {
            panic!("chunk not loaded")
        };
        let section = chunk
            .sections
            .iter()
            .find(|section| i32::from(section.y) == pos.y >> 4)
            .unwrap();
        let Some(Nbt::ByteArray(bytes)) = section.rest.get("BlockLight") else {
            return 0;
        };
        let index = ((pos.y & 15) << 8 | (pos.z & 15) << 4 | (pos.x & 15)) as usize;
        (bytes[index >> 1] as u8 >> ((index & 1) * 4)) & 15
    }

    #[test]
    fn a_block_that_gives_off_light_lights_its_chunk_again_and_the_light_is_saved() {
        let folder = Folder::new();
        let pos = ChunkPos { x: 0, z: 0 };
        save(&folder, &[(pos, DATA_VERSION)]);
        let (output, received) = crate::player::output_channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: profile(),
                view_distance: 2,
                initial_view_distance: 2,
                allow_flight: false,
                output,
                force_game_mode_on_login: None,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            viewers: Vec::new(),
            stopped: false,
        };
        let mut number = 1;
        tick(&mut game, number);
        wait_until(|| {
            // As the client acknowledges each batch, so that a chunk read after the first batch is sent too.
            game.players[0].awaiting_chunk_batch = false;
            number += 1;
            tick(&mut game, number);
            game.players[0].sent_chunks.contains(&pos)
        });
        received.try_iter().for_each(drop);
        let states = block_states();
        let glowstone = states.default_state("minecraft:glowstone").unwrap();
        let stone = states.default_state("minecraft:stone").unwrap();
        let lamp = BlockPos { x: 8, y: -56, z: 8 };
        let beside = BlockPos { x: 9, ..lamp };
        let world = game.world.as_mut().unwrap();
        assert_eq!(saved_block_light(world, lamp), 0);
        world.set_block_state(lamp, glowstone);
        // Until it is lit again, the chunk is saved without `isLightOn`, so that vanilla would light it.
        assert!(world.light_pending(pos));
        let ChunkState::Loaded(chunk) = &world.chunks[&pos] else {
            panic!("chunk not loaded")
        };
        let snapshot = Snapshot {
            pos,
            revision: 0,
            chunk: Some(Arc::clone(chunk)),
            light: None,
            stale_light: true,
            fluid_ticks: None,
            entities: None,
        };
        assert_eq!(
            snapshot.saved().unwrap().rest.get("isLightOn"),
            Some(&Nbt::Byte(0))
        );
        // A player who has the chunk at the edge of its view is sent the new light; one who has its
        // neighbors too lights the block itself.
        let sent = &mut game.players[0].sent_chunks;
        sent.extend((-1..=1).flat_map(|x| (-1..=1).map(move |z| ChunkPos { x, z })));
        assert!(!game.players[0].on_tracked_border(pos));
        game.players[0].sent_chunks.retain(|sent| *sent == pos);
        assert!(game.players[0].on_tracked_border(pos));
        let mut updates = Vec::new();
        wait_until(|| {
            number += 1;
            tick(&mut game, number);
            updates.extend(received.try_iter().filter_map(|output| match output {
                Output::LightUpdate { position, light } => Some((position, light)),
                _ => None,
            }));
            !game.world.as_ref().unwrap().light_pending(pos)
        });
        let world = game.world.as_ref().unwrap();
        assert_eq!(saved_block_light(world, lamp), 15);
        // Stone around it lets no light through.
        assert_eq!(saved_block_light(world, beside), 0);
        let [(position, light)] = &updates[..] else {
            panic!("expected one light update, got {updates:?}")
        };
        assert_eq!(*position, pos);
        // Only the sections that changed: the glowstone's, and the light section below the world, which the
        // test chunk was saved without and is dark under the stone.
        assert_eq!(
            light.sections.iter().map(|s| s.y).collect::<Vec<_>>(),
            [-5, -4]
        );

        // Breaking it, the light goes too.
        game.world.as_mut().unwrap().set_block_state(lamp, stone);
        wait_until(|| {
            number += 1;
            tick(&mut game, number);
            !game.world.as_ref().unwrap().light_pending(pos)
        });
        game.world
            .as_mut()
            .unwrap()
            .set_block_state(lamp, glowstone);
        wait_until(|| {
            number += 1;
            tick(&mut game, number);
            !game.world.as_ref().unwrap().light_pending(pos)
        });

        // The light is saved, with `isLightOn`, and read back.
        let mut world = game.world.take().unwrap();
        world.flush().unwrap();
        drop(world);
        let mut reopened = World::open(&folder.0, BIOMES).unwrap();
        load_chunks(&mut reopened, &[pos]);
        assert_eq!(saved_block_light(&reopened, lamp), 15);
        assert!(!reopened.light_pending(pos));
    }

    #[test]
    fn water_spreads_falls_into_new_holes_and_hardens_lava() {
        let folder = Folder::new();
        let pos = ChunkPos { x: 0, z: 0 };
        save(&folder, &[(pos, DATA_VERSION)]);
        let (output, received) = crate::player::output_channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: profile(),
                view_distance: 2,
                initial_view_distance: 2,
                allow_flight: false,
                output,
                force_game_mode_on_login: None,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            viewers: Vec::new(),
            stopped: false,
        };
        let mut number = 1;
        tick(&mut game, number);
        wait_until(|| {
            game.players[0].awaiting_chunk_batch = false;
            number += 1;
            tick(&mut game, number);
            game.players[0].sent_chunks.contains(&pos)
        });
        game.players[0].player.position = crate::player::Position {
            x: 7.5,
            y: -48.0,
            z: 7.5,
            ..game.players[0].player.position
        };
        let states = block_states();
        let state = |name: &str, properties: &[(&str, &str)]| {
            states.state(name, properties.iter().copied()).unwrap().0
        };
        let air = state("minecraft:air", &[]);
        let water = |level: &str| state("minecraft:water", &[("level", level)]);
        let at = |x, y, z| BlockPos { x, y, z };
        // A room from x and z 4 to 12, y -51 to -49, over a stone floor.
        let world = game.world.as_mut().unwrap();
        for x in 4..=12 {
            for z in 4..=12 {
                for y in -51..=-49 {
                    world.set_block_state(at(x, y, z), air);
                }
            }
        }
        let mut run = |game: &mut Game, ticks: u32| {
            for _ in 0..ticks {
                number += 1;
                tick(game, number);
            }
        };
        let block = |game: &Game, pos| game.world.as_ref().unwrap().block_state(pos).unwrap();

        // A source spreads one block every five ticks, a level lower each block, until the drop-off ends it.
        let world = game.world.as_mut().unwrap();
        world.set_block(
            at(8, -51, 8),
            water("0"),
            neighbor::UPDATE_ALL,
            neighbor::UPDATE_LIMIT,
        );
        run(&mut game, 4);
        assert_eq!(block(&game, at(9, -51, 8)), air);
        run(&mut game, 1);
        for side in [at(9, -51, 8), at(7, -51, 8), at(8, -51, 9), at(8, -51, 7)] {
            assert_eq!(block(&game, side), water("1"));
        }
        run(&mut game, 60);
        assert_eq!(block(&game, at(12, -51, 8)), water("4"));
        assert_eq!(block(&game, at(11, -51, 7)), water("4"));
        // Seven blocks away is the last water; eight away is too far.
        assert_eq!(block(&game, at(12, -51, 11)), water("7"));
        assert_eq!(block(&game, at(12, -51, 12)), air);

        // Water beside a new hole flows down into it.
        let floor = at(11, -52, 8);
        game.world.as_mut().unwrap().set_block(
            floor,
            air,
            neighbor::UPDATE_ALL,
            neighbor::UPDATE_LIMIT,
        );
        run(&mut game, 20);
        assert_eq!(block(&game, floor), water("8"));

        // A lava source placed among water hardens at once into obsidian, with a fizz.
        received.try_iter().for_each(drop);
        let lava = state("minecraft:lava", &[("level", "0")]);
        let fizzes = |received: &crate::player::OutputReceiver| {
            received
                .try_iter()
                .filter(|output| {
                    matches!(
                        output,
                        Output::WorldEvent {
                            event: crate::fluid::LAVA_FIZZ,
                            ..
                        }
                    )
                })
                .count()
        };
        let world = game.world.as_mut().unwrap();
        world.set_block(
            at(6, -51, 8),
            lava,
            neighbor::UPDATE_ALL,
            neighbor::UPDATE_LIMIT,
        );
        assert_eq!(
            block(&game, at(6, -51, 8)),
            state("minecraft:obsidian", &[])
        );
        run(&mut game, 1);
        assert_eq!(fizzes(&received), 1);

        // Lava over the water touches none from the sides, so it waits for its tick, 30 later, and flows down
        // into the water, which becomes stone.
        let world = game.world.as_mut().unwrap();
        world.set_block(
            at(8, -50, 8),
            lava,
            neighbor::UPDATE_ALL,
            neighbor::UPDATE_LIMIT,
        );
        run(&mut game, 29);
        assert_eq!(block(&game, at(8, -51, 8)), water("0"));
        run(&mut game, 1);
        assert_eq!(block(&game, at(8, -51, 8)), state("minecraft:stone", &[]));
        assert_eq!(fizzes(&received), 1);
    }

    #[test]
    fn pending_fluid_ticks_are_saved_and_run_after_the_world_is_read_again() {
        let folder = Folder::new();
        let pos = ChunkPos { x: 0, z: 0 };
        save(&folder, &[(pos, DATA_VERSION)]);
        let states = block_states();
        let air = states.default_state("minecraft:air").unwrap();
        let water = states.default_state("minecraft:water").unwrap();
        let source = BlockPos { x: 8, y: -50, z: 8 };
        let side = BlockPos { x: 9, ..source };
        let mut world = World::open(&folder.0, BIOMES).unwrap();
        load_chunks(&mut world, &[pos]);
        world.set_block_state(side, air);
        world.set_block_state(BlockPos { y: -49, ..side }, air);
        world.set_block_state(BlockPos { y: -49, ..source }, air);
        world.set_block(source, water, neighbor::UPDATE_ALL, neighbor::UPDATE_LIMIT);
        world.tick_scheduled();
        world.flush().unwrap();
        let saved = read_chunk(&folder.region_dir(), 0, 0).unwrap().unwrap();
        let Some(Nbt::List(harpas_core::nbt::List::Compound(ticks))) = saved.get("fluid_ticks")
        else {
            panic!("no fluid ticks saved")
        };
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].get("i"), Some(&Nbt::from("minecraft:water")));
        // Five ticks after it was scheduled, one has passed.
        assert_eq!(ticks[0].get("t"), Some(&Nbt::Int(4)));
        drop(world);

        let mut reopened = World::open(&folder.0, BIOMES).unwrap();
        load_chunks(&mut reopened, &[pos]);
        // The four ticks it had left.
        for _ in 0..3 {
            reopened.tick_scheduled();
        }
        assert_eq!(reopened.block_state(side), Some(air));
        reopened.tick_scheduled();
        assert_eq!(
            reopened.block_state(side),
            Some(states.state("minecraft:water", [("level", "1")]).unwrap().0)
        );
    }

    #[test]
    fn players_place_blocks_from_their_creative_inventory_as_vanilla_places_them() {
        use crate::inventory::ItemStack;
        use harpas_core::block_behavior::Face;
        use harpas_core::item::items;

        let folder = Folder::new();
        let pos = ChunkPos { x: 0, z: 0 };
        save(&folder, &[(pos, DATA_VERSION)]);
        let (output, received) = crate::player::output_channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: profile(),
                view_distance: 2,
                initial_view_distance: 2,
                allow_flight: false,
                output,
                force_game_mode_on_login: None,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            viewers: Vec::new(),
            stopped: false,
        };
        game.world
            .as_mut()
            .unwrap()
            .set_shape_updates(shape_updates());
        let mut number = 1;
        tick(&mut game, number);
        wait_until(|| {
            game.players[0].awaiting_chunk_batch = false;
            number += 1;
            tick(&mut game, number);
            game.players[0].sent_chunks.contains(&pos)
        });
        game.pending.push(Event::Input {
            entity_id: 1,
            input: Input::ConfirmTeleport(1),
        });
        number += 1;
        tick(&mut game, number);
        let states = block_states();
        let state = |name: &str, properties: &[(&str, &str)]| {
            states.state(name, properties.iter().copied()).unwrap().0
        };
        let at = |x, y, z| BlockPos { x, y, z };
        // A room from x and z 4 to 12, y -51 to -49, over a stone floor, with the player in its middle, looking
        // south.
        let world = game.world.as_mut().unwrap();
        for x in 4..=12 {
            for z in 4..=12 {
                for y in -51..=-49 {
                    world.set_block_state(at(x, y, z), state("minecraft:air", &[]));
                }
            }
        }
        game.players[0].player.position = crate::player::Position {
            x: 8.5,
            y: -51.0,
            z: 8.5,
            yaw: 0.0,
            ..game.players[0].player.position
        };
        received.try_iter().for_each(drop);
        let stack = |name: &str, count| ItemStack {
            item: items().id(name).unwrap(),
            count,
            components: Arc::default(),
        };
        let mut sequence = 0;
        let mut place =
            |game: &mut Game, item: &str, hit: BlockPos, face: Face, cursor: [f32; 3]| {
                sequence += 1;
                game.pending.push(Event::Input {
                    entity_id: 1,
                    input: Input::SetCreativeSlot {
                        slot: 36,
                        stack: stack(item, 64),
                    },
                });
                game.pending.push(Event::Input {
                    entity_id: 1,
                    input: Input::UseItemOn {
                        off_hand: false,
                        position: hit,
                        face,
                        cursor,
                        sequence,
                    },
                });
                number += 1;
                tick(game, number);
            };
        let block = |game: &Game, pos| game.world.as_ref().unwrap().block_state(pos).unwrap();
        let top = [0.5, 1.0, 0.5];

        // A block without properties, on the floor; the clicked block and the one beside the face are sent back.
        place(
            &mut game,
            "minecraft:oak_planks",
            at(5, -52, 7),
            Face::Up,
            top,
        );
        assert_eq!(
            block(&game, at(5, -51, 7)),
            state("minecraft:oak_planks", &[])
        );
        let sent: Vec<_> = received.try_iter().collect();
        assert!(sent.contains(&Output::BlockUpdate {
            position: at(5, -51, 7),
            state: state("minecraft:oak_planks", &[]),
        }));
        assert!(sent.contains(&Output::AcknowledgeBlockChanges(1)));
        // A log placed against the side of a block lies along that side's axis.
        place(
            &mut game,
            "minecraft:oak_log",
            at(5, -51, 7),
            Face::East,
            [1.0, 0.5, 0.5],
        );
        assert_eq!(
            block(&game, at(6, -51, 7)),
            state("minecraft:oak_log", &[("axis", "x")])
        );
        // Stairs face the way the player looks, upside down when placed against the top half of a side.
        place(
            &mut game,
            "minecraft:oak_stairs",
            at(5, -52, 10),
            Face::Up,
            top,
        );
        assert_eq!(
            block(&game, at(5, -51, 10)),
            state(
                "minecraft:oak_stairs",
                &[("facing", "south"), ("half", "bottom")]
            )
        );
        place(
            &mut game,
            "minecraft:oak_stairs",
            at(5, -51, 10),
            Face::East,
            [1.0, 0.75, 0.5],
        );
        assert_eq!(
            block(&game, at(6, -51, 10)),
            state(
                "minecraft:oak_stairs",
                &[("facing", "south"), ("half", "top")]
            )
        );
        // A slab on the floor is a bottom slab; another on its top makes it double.
        place(
            &mut game,
            "minecraft:stone_slab",
            at(10, -52, 10),
            Face::Up,
            top,
        );
        assert_eq!(
            block(&game, at(10, -51, 10)),
            state("minecraft:stone_slab", &[("type", "bottom")])
        );
        place(
            &mut game,
            "minecraft:stone_slab",
            at(10, -51, 10),
            Face::Up,
            [0.5, 0.5, 0.5],
        );
        assert_eq!(
            block(&game, at(10, -51, 10)),
            state("minecraft:stone_slab", &[("type", "double")])
        );
        // Not where the player stands, nor a block whose placement is not ported.
        place(&mut game, "minecraft:stone", at(8, -52, 8), Face::Up, top);
        assert_eq!(block(&game, at(8, -51, 8)), state("minecraft:air", &[]));
        place(
            &mut game,
            "minecraft:oak_door",
            at(11, -52, 6),
            Face::Up,
            top,
        );
        assert_eq!(block(&game, at(11, -51, 6)), state("minecraft:air", &[]));

        // In Survival the stack goes down by one, and the client is told at the player's next tick.
        game.players[0].player.profile.game_mode = GameMode::Survival;
        game.players[0]
            .inventory
            .set_by_client(36, stack("minecraft:stone", 2));
        received.try_iter().for_each(drop);
        game.pending.push(Event::Input {
            entity_id: 1,
            input: Input::UseItemOn {
                off_hand: false,
                position: at(5, -52, 5),
                face: Face::Up,
                cursor: top,
                sequence: 9,
            },
        });
        number += 1;
        tick(&mut game, number);
        number += 1;
        tick(&mut game, number);
        assert_eq!(block(&game, at(5, -51, 5)), state("minecraft:stone", &[]));
        assert!(received.try_iter().any(|output| matches!(
            output,
            Output::SetSlot { slot: 36, stack, .. } if stack.count == 1
        )));

        // A stack larger than its item holds cannot come from a vanilla client.
        game.players[0].player.profile.game_mode = GameMode::Creative;
        game.pending.push(Event::Input {
            entity_id: 1,
            input: Input::SetCreativeSlot {
                slot: 36,
                stack: stack("minecraft:diamond_sword", 2),
            },
        });
        number += 1;
        tick(&mut game, number);
        assert!(
            received
                .try_iter()
                .any(|output| output == Output::Disconnect("Network Protocol Error"))
        );
    }

    #[test]
    fn dropped_items_fall_land_merge_and_despawn_while_players_see_them() {
        use crate::entity::ItemEntity;
        use crate::inventory::ItemStack;
        use harpas_core::item::items;

        let folder = Folder::new();
        let pos = ChunkPos { x: 0, z: 0 };
        save(&folder, &[(pos, DATA_VERSION)]);
        let (output, received) = crate::player::output_channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: profile(),
                view_distance: 2,
                initial_view_distance: 2,
                allow_flight: false,
                output,
                force_game_mode_on_login: None,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            viewers: Vec::new(),
            stopped: false,
        };
        let mut number = 1;
        tick(&mut game, number);
        wait_until(|| {
            game.players[0].awaiting_chunk_batch = false;
            number += 1;
            tick(&mut game, number);
            game.players[0].sent_chunks.contains(&pos)
        });
        // Far enough not to pick the items up, near enough to see them.
        game.players[0].player.position = crate::player::Position {
            x: 24.5,
            y: -49.0,
            z: 8.5,
            ..game.players[0].player.position
        };
        received.try_iter().for_each(drop);
        let stone = |count| ItemStack {
            item: items().id("minecraft:stone").unwrap(),
            count,
            components: Arc::default(),
        };
        let mut run = |game: &mut Game, ticks: u32| {
            for _ in 0..ticks {
                number += 1;
                tick(game, number);
            }
        };

        // An item let go two blocks above the stone floor, whose top is at y -48, falls onto it and stays.
        let world = game.world.as_mut().unwrap();
        let first = world.add_item(1000, [8.5, -46.0, 8.5], [0.0; 3], ItemEntity::new(stone(3)));
        run(&mut game, 1);
        let spawned: Vec<_> = received
            .try_iter()
            .filter(|output| matches!(output, Output::SpawnItem { .. }))
            .collect();
        assert!(matches!(
            &spawned[..],
            [Output::SpawnItem { entity_id: 1000, stack, .. }] if stack.count == 3
        ));
        run(&mut game, 40);
        let world = game.world.as_ref().unwrap();
        let i = world.entities.slot(first);
        assert_eq!(world.entities.position[i][1], -48.0);
        assert!(world.entities.on_ground[i]);
        // Its viewer was sent where it went, by moves or a full position.
        assert!(received.try_iter().any(|output| matches!(
            output,
            Output::EntitySync {
                entity_id: 1000,
                on_ground: true,
                ..
            } | Output::EntityMove {
                entity_id: 1000,
                ..
            }
        )));

        // Another stack of the same item next to it merges into the larger one, and the viewer is told.
        let world = game.world.as_mut().unwrap();
        let second = world.add_item(1001, [8.7, -48.0, 8.5], [0.0; 3], ItemEntity::new(stone(2)));
        run(&mut game, 41);
        let world = game.world.as_ref().unwrap();
        assert!(!world.entities.contains(second));
        assert_eq!(world.entities.item(first).unwrap().stack.count, 5);
        let sent: Vec<_> = received.try_iter().collect();
        assert!(sent.contains(&Output::RemoveEntity(1001)));
        assert!(sent.iter().any(|output| matches!(
            output,
            Output::ItemChanged { entity_id: 1000, stack } if stack.count == 5
        )));

        // After 6,000 ticks on the ground it is gone.
        run(&mut game, 6000);
        assert!(!game.world.as_ref().unwrap().entities.contains(first));
        assert!(
            received
                .try_iter()
                .any(|output| output == Output::RemoveEntity(1000))
        );
    }

    #[test]
    fn players_drop_items_where_they_look() {
        use crate::inventory::ItemStack;
        use harpas_core::item::items;

        let folder = Folder::new();
        let pos = ChunkPos { x: 0, z: 0 };
        save(&folder, &[(pos, DATA_VERSION)]);
        let (output, _received) = crate::player::output_channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: profile(),
                view_distance: 2,
                initial_view_distance: 2,
                allow_flight: false,
                output,
                force_game_mode_on_login: None,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            viewers: Vec::new(),
            stopped: false,
        };
        let mut number = 1;
        tick(&mut game, number);
        // Chunk and player-data workers complete independently. Wait for both before seeding the inventory,
        // or the pending player load can replace these test items before the drop inputs are processed.
        wait_until(|| {
            number += 1;
            tick(&mut game, number);
            game.players[0].data_ready
                && matches!(
                    game.world.as_ref().unwrap().chunk(pos),
                    Some(ChunkState::Loaded(_))
                )
        });
        game.players[0].player.position = crate::player::Position {
            x: 8.5,
            y: -49.0,
            z: 8.5,
            yaw: 0.0,
            pitch: 0.0,
            ..game.players[0].player.position
        };
        let stone = items().id("minecraft:stone").unwrap();
        game.players[0].inventory.set_by_client(
            36,
            ItemStack {
                item: stone,
                count: 10,
                components: Arc::default(),
            },
        );
        game.pending.push(Event::Input {
            entity_id: 1,
            input: Input::DropSelected { all: false },
        });
        game.pending.push(Event::Input {
            entity_id: 1,
            input: Input::DropSelected { all: true },
        });
        number += 1;
        tick(&mut game, number);
        assert!(game.players[0].inventory.slots[36].is_empty());
        let world = game.world.as_ref().unwrap();
        let counts: Vec<_> = world
            .entities
            .order
            .iter()
            .map(|&handle| world.entities.item(handle).unwrap().stack.count)
            .collect();
        assert_eq!(counts, [1, 9]);
        for &handle in &world.entities.order {
            let i = world.entities.slot(handle);
            // From 0.3 below the eyes, towards the south the player faces, and picked up after 40 ticks.
            assert_eq!(
                world.entities.position[i][1],
                -49.0 + 1.62 - f64::from(0.3_f32)
            );
            assert!(world.entities.velocity[i][2] > 0.2);
            assert_eq!(world.entities.item(handle).unwrap().pickup_delay, 40);
        }
    }

    #[test]
    fn broken_blocks_drop_their_loot_and_players_pick_it_up() {
        use harpas_core::item::items;
        use serde_json::json;

        let folder = Folder::new();
        let pos = ChunkPos { x: 0, z: 0 };
        save(&folder, &[(pos, DATA_VERSION)]);
        let (output, received) = crate::player::output_channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: Profile {
                    game_mode: GameMode::Survival,
                    ..profile()
                },
                view_distance: 2,
                initial_view_distance: 2,
                allow_flight: false,
                output,
                force_game_mode_on_login: None,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            viewers: Vec::new(),
            stopped: false,
        };
        let mut loot = crate::loot::LootTables::default();
        let drops_itself = |name: &str| {
            json!({"pools": [{"rolls": 1.0, "bonus_rolls": 0.0, "conditions": [{"condition": "minecraft:survives_explosion"}],
                "entries": [{"type": "minecraft:item", "name": name}]}], "random_sequence": format!("{name}_blocks")})
        };
        loot.insert("minecraft:dandelion", &drops_itself("minecraft:dandelion"));
        loot.insert("minecraft:torch", &drops_itself("minecraft:torch"));
        let world = game.world.as_mut().unwrap();
        world.set_shape_updates(shape_updates());
        world.set_loot_tables(Arc::new(loot));
        let mut number = 1;
        tick(&mut game, number);
        wait_until(|| {
            game.players[0].awaiting_chunk_batch = false;
            number += 1;
            tick(&mut game, number);
            game.players[0].sent_chunks.contains(&pos)
        });
        game.players[0].player.position = crate::player::Position {
            x: 8.5,
            y: -50.0,
            z: 8.5,
            on_ground: true,
            ..game.players[0].player.position
        };
        let states = block_states();
        let state = |name: &str| states.default_state(name).unwrap();
        let flower = BlockPos { x: 7, y: -49, z: 8 };
        let glass = BlockPos { x: 9, y: -50, z: 8 };
        let torch = BlockPos { x: 9, y: -49, z: 8 };
        let world = game.world.as_mut().unwrap();
        world.set_block_state(flower, state("minecraft:dandelion"));
        world.set_block_state(glass, state("minecraft:glass"));
        world.set_block_state(torch, state("minecraft:torch"));
        // One tick, with a dig if any.
        let mut step = |game: &mut Game, dig: Option<(block::DigAction, BlockPos)>| {
            if let Some((action, position)) = dig {
                game.pending.push(Event::Input {
                    entity_id: 1,
                    input: Input::Dig {
                        action,
                        position,
                        sequence: 1,
                    },
                });
            }
            number += 1;
            tick(game, number);
        };
        let dropped = |game: &Game| -> Vec<&'static str> {
            let world = game.world.as_ref().unwrap();
            world
                .entities
                .order
                .iter()
                .map(|&h| world.entities.item(h).unwrap().stack.name().unwrap())
                .collect()
        };
        // A flower breaks at once and drops itself.
        step(&mut game, Some((block::DigAction::Start, flower)));
        assert_eq!(dropped(&game), ["minecraft:dandelion"]);
        // Glass, which needs silk touch, drops nothing, but the torch on it breaks with it and drops itself.
        step(&mut game, Some((block::DigAction::Start, glass)));
        for _ in 0..8 {
            step(&mut game, None);
        }
        step(&mut game, Some((block::DigAction::Finish, glass)));
        let world = game.world.as_ref().unwrap();
        assert_eq!(world.block_state(glass), Some(state("minecraft:air")));
        assert_eq!(world.block_state(torch), Some(state("minecraft:air")));
        assert_eq!(dropped(&game).last(), Some(&"minecraft:torch"));

        // Once their pickup delay is over, the player standing by picks them up, and is told.
        for _ in 0..20 {
            step(&mut game, None);
        }
        assert!(game.world.as_ref().unwrap().entities.order.is_empty());
        let inventory = &game.players[0].inventory;
        let held: Vec<_> = (36..45)
            .filter(|&slot| !inventory.slots[slot].is_empty())
            .map(|slot| {
                (
                    inventory.slots[slot].name().unwrap(),
                    inventory.slots[slot].count,
                )
            })
            .collect();
        assert_eq!(held, [("minecraft:dandelion", 1), ("minecraft:torch", 1)]);
        let sent: Vec<_> = received.try_iter().collect();
        assert_eq!(
            sent.iter()
                .filter(|o| matches!(
                    o,
                    Output::PickupItem {
                        collector: 1,
                        count: 1,
                        ..
                    }
                ))
                .count(),
            2
        );
        assert!(sent.iter().any(|o| matches!(
            o,
            Output::SetSlot { slot: 36, stack, .. } if stack.item == items().id("minecraft:dandelion").unwrap()
        )));
    }

    #[test]
    fn double_chest_shares_slots_across_chunks_and_splits_on_break() {
        use crate::chest::CHEST;
        use crate::inventory::{CHEST_SLOTS, HOTBAR, ItemStack};
        use harpas_core::block_behavior::Face;
        use harpas_core::item::items;

        let folder = Folder::new();
        let left = BlockPos {
            x: 15,
            y: -51,
            z: 9,
        };
        let right = BlockPos { x: 16, ..left };
        save(
            &folder,
            &[(left.chunk(), DATA_VERSION), (right.chunk(), DATA_VERSION)],
        );
        let (first_output, first) = crate::player::output_channel();
        let (second_output, second) = crate::player::output_channel();
        let mut game = Game {
            pending: Vec::new(),
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            viewers: Vec::new(),
            stopped: false,
        };
        game.join(1, profile(), (2, 2), false, first_output, None);
        let mut other = profile();
        other.uuid = 2;
        game.join(2, other, (2, 2), false, second_output, None);
        wait_until(|| {
            for player in &mut game.players {
                player.awaiting_chunk_batch = false;
            }
            tick(&mut game, 1);
            game.players.iter().all(|player| {
                [left, right]
                    .into_iter()
                    .all(|pos| player.sent_chunks.contains(&pos.chunk()))
            })
        });
        for (index, player) in game.players.iter_mut().enumerate() {
            player.awaiting_teleport = None;
            player.player.position = crate::player::Position {
                x: 15.5 + index as f64,
                y: -51.0,
                z: 7.5,
                ..Default::default()
            };
            player.player.profile.game_mode = GameMode::Survival;
        }
        let states = block_states();
        let air = states.default_state("minecraft:air").unwrap();
        let above_right = BlockPos {
            y: right.y + 1,
            ..right
        };
        let world = game.world.as_mut().unwrap();
        for pos in [
            left,
            right,
            BlockPos {
                y: left.y + 1,
                ..left
            },
        ] {
            assert!(world.set_block_state(pos, air).is_some());
        }
        let stack = |name: &str, count| ItemStack {
            item: items().id(name).unwrap(),
            count,
            ..ItemStack::empty()
        };
        game.players[0].inventory.slots[HOTBAR] = stack(CHEST, 2);
        game.use_item_on(
            0,
            false,
            BlockPos {
                y: left.y - 1,
                ..left
            },
            Face::Up,
            [0.5; 3],
            1,
        );
        game.players[0].secondary_use = true;
        game.players[0].player.position.yaw = 90.0;
        game.use_item_on(0, false, left, Face::East, [0.5; 3], 2);
        game.players[0].secondary_use = false;
        assert!(game.players[0].inventory.slots[HOTBAR].is_empty());
        let chest_state = |kind| {
            states
                .state(CHEST, [("facing", "north"), ("type", kind)])
                .unwrap()
                .0
        };
        let world = game.world.as_mut().unwrap();
        assert_eq!(world.block_state(left), Some(chest_state("left")));
        assert_eq!(world.block_state(right), Some(chest_state("right")));

        world
            .save_chest(
                left,
                &[stack("minecraft:stone", 60), stack("minecraft:diamond", 1)],
            )
            .unwrap();
        world
            .save_chest(right, &[stack("minecraft:stone", 61)])
            .unwrap();
        game.use_item_on(0, false, left, Face::Up, [0.5; 3], 3);
        assert_eq!(
            game.players[0].container_id(),
            0,
            "the other half is obstructed"
        );
        assert!(
            game.world
                .as_mut()
                .unwrap()
                .set_block_state(above_right, air)
                .is_some()
        );
        first.try_iter().for_each(drop);
        second.try_iter().for_each(drop);
        game.players[0].inventory.slots[HOTBAR] = stack("minecraft:stone", 9);
        for (index, (pos, output)) in [(left, &first), (right, &second)].into_iter().enumerate() {
            game.use_item_on(index, false, pos, Face::Up, [0.5; 3], 4);
            assert_eq!(
                output.try_recv(),
                Ok(Output::OpenChest {
                    container_id: 1,
                    double: true,
                    title: None
                })
            );
            let Output::InventoryContent { slots, .. } = output.try_recv().unwrap() else {
                panic!("expected the inventory contents");
            };
            assert_eq!(slots.len(), 90);
            assert_eq!(slots[0], stack("minecraft:stone", 61));
            assert_eq!(slots[CHEST_SLOTS], stack("minecraft:stone", 60));
        }
        // One shift-click fills partial stacks in both chunks before placing the rest in the right half.
        game.click_container(0, 1, -1, 81, 0, 1, Vec::new(), None);
        game.send_inventory_changes();
        let slots = &game.players[1].block_menu.as_ref().unwrap().contents.slots;
        assert_eq!(slots[0], stack("minecraft:stone", 64));
        assert_eq!(slots[1], stack("minecraft:stone", 2));
        assert_eq!(slots[CHEST_SLOTS], stack("minecraft:stone", 64));
        for slot in [0, CHEST_SLOTS as i16] {
            assert!(second.try_iter().any(
                |o| matches!(o, Output::SetSlot { container_id: 1, slot: changed, stack: item, .. }
                if changed == slot && item == stack("minecraft:stone", 64))
            ));
        }
        game.players[0].player.profile.game_mode = GameMode::Creative;
        assert!(game.destroy_block(0, right));
        assert!(game.players.iter().all(|p| p.block_menu.is_none()));
        assert!(first.try_iter().any(|o| o == Output::CloseContainer(1)));
        assert!(second.try_iter().any(|o| o == Output::CloseContainer(1)));
        let world = game.world.as_ref().unwrap();
        assert_eq!(world.block_state(left), Some(chest_state("single")));
        assert!(world.chest_tag(right).unwrap().is_none());
        let dropped: Vec<_> = world
            .entities
            .order
            .iter()
            .filter_map(|&h| world.entities.item(h))
            .collect();
        assert!(
            dropped
                .iter()
                .all(|item| item.stack.name() == Some("minecraft:stone"))
        );
        assert_eq!(dropped.iter().map(|item| item.stack.count).sum::<i32>(), 66);
        game.use_item_on(1, false, left, Face::Up, [0.5; 3], 5);
        assert_eq!(
            second.try_recv(),
            Ok(Output::OpenChest {
                container_id: 2,
                double: false,
                title: None
            })
        );
        let Output::InventoryContent { slots, .. } = second.try_recv().unwrap() else {
            panic!("expected the surviving chest contents");
        };
        assert_eq!(slots.len(), 63);
        assert_eq!(slots[0], stack("minecraft:stone", 64));
        assert_eq!(slots[1], stack("minecraft:diamond", 1));
    }

    #[test]
    fn furnace_menu_cooks_and_resumes_from_anvil() {
        use crate::furnace::FURNACE;
        use crate::inventory::{HOTBAR, ItemStack};
        use harpas_core::block_behavior::Face;
        use harpas_core::item::items;

        let folder = Folder::new();
        let pos = BlockPos { x: 8, y: -51, z: 9 };
        save(&folder, &[(pos.chunk(), DATA_VERSION)]);
        let recipe = serde_json::json!({
            "type": "minecraft:smelting", "ingredient": "minecraft:raw_iron",
            "result": {"id": "minecraft:iron_ingot"}, "cookingtime": 200,
        });
        let recipes = Arc::new(crate::recipe::Recipes::from_json(
            &HashMap::new(),
            [&recipe],
        ));
        let mut world = World::open(&folder.0, BIOMES).unwrap();
        world.recipes = Some(recipes.clone());
        let (sender, received) = crate::player::output_channel();
        let mut game = Game {
            pending: Vec::new(),
            players: Vec::new(),
            online_players: Arc::default(),
            world: Some(world),
            viewers: Vec::new(),
            stopped: false,
        };
        game.join(1, profile(), (2, 2), false, sender, None);
        wait_until(|| {
            game.players[0].awaiting_chunk_batch = false;
            tick(&mut game, 1);
            game.players[0].sent_chunks.contains(&pos.chunk())
        });
        let player = &mut game.players[0];
        player.awaiting_teleport = None;
        player.player.position = crate::player::Position {
            x: 8.5,
            y: -51.0,
            z: 7.5,
            ..Default::default()
        };
        player.player.profile.game_mode = GameMode::Survival;
        let stack = |name: &str, count| ItemStack {
            item: items().id(name).unwrap(),
            count,
            ..ItemStack::default()
        };
        player.inventory.slots[HOTBAR] = stack(FURNACE, 1);
        player.inventory.slots[HOTBAR + 1] = stack("minecraft:raw_iron", 3);
        player.inventory.slots[HOTBAR + 2] = stack("minecraft:coal", 1);
        let world = game.world.as_mut().unwrap();
        let air = block_states().default_state("minecraft:air").unwrap();
        world.set_block_state(pos, air);
        let floor = BlockPos {
            y: pos.y - 1,
            ..pos
        };
        game.use_item_on(0, false, floor, Face::Up, [0.5; 3], 1);
        assert!(game.players[0].inventory.slots[HOTBAR].is_empty());
        let world = game.world.as_mut().unwrap();
        assert_eq!(
            block_states().name(world.block_state(pos).unwrap()),
            Some(FURNACE)
        );
        let mut furnace = world.furnace(pos).unwrap();
        furnace.slots[2] = stack("minecraft:iron_ingot", 64);
        world.save_furnace(pos, &furnace).unwrap();

        received.try_iter().for_each(drop);
        game.use_item_on(0, false, pos, Face::Up, [0.5; 3], 2);
        assert_eq!(
            received.try_recv(),
            Ok(Output::OpenFurnace {
                container_id: 1,
                title: None
            })
        );
        assert!(
            matches!(received.try_recv(), Ok(Output::InventoryContent { container_id: 1, slots, .. }) if slots.len() == 39)
        );
        let click = |game: &mut Game, slot, mode| {
            game.click_container(0, 1, -1, slot, 0, mode, Vec::new(), None);
        };
        click(&mut game, 31, 1); // Raw iron from the hotbar.
        click(&mut game, 32, 1); // Coal from the hotbar.
        for n in 1..=3 {
            tick(&mut game, n);
        }
        let furnace = game.world.as_ref().unwrap().furnace(pos).unwrap();
        assert_eq!(furnace.properties(), [0, 0, 0, 200]);
        assert_eq!(
            furnace.slots,
            [
                stack("minecraft:raw_iron", 3),
                stack("minecraft:coal", 1),
                stack("minecraft:iron_ingot", 64)
            ]
        );

        click(&mut game, 2, 1);
        assert_eq!(
            game.players[0].inventory.slots[HOTBAR + 8],
            stack("minecraft:iron_ingot", 64)
        );
        received.try_iter().for_each(drop);
        for n in 1..=80 {
            tick(&mut game, n);
        }
        assert!(received.try_iter().any(|output| output
            == Output::ContainerProperty {
                container_id: 1,
                property: 2,
                value: 80,
            }));
        let mut world = game.world.take().unwrap();
        let before = world.furnace(pos).unwrap();
        assert_eq!(before.properties(), [1521, 1600, 80, 200]);
        assert!(before.slots[1].is_empty());
        assert_eq!(
            block_states().light_emission(world.block_state(pos).unwrap()),
            13
        );
        world.flush().unwrap();
        drop(world);

        let mut world = World::open(&folder.0, BIOMES).unwrap();
        world.recipes = Some(recipes);
        load_chunks(&mut world, &[pos.chunk()]);
        assert_eq!(world.furnace(pos).unwrap(), before);
        for _ in 0..119 {
            world.tick_furnaces();
        }
        assert!(world.furnace(pos).unwrap().slots[2].is_empty());
        world.tick_furnaces();
        let cooked = world.furnace(pos).unwrap();
        assert_eq!(cooked.properties(), [1401, 1600, 0, 200]);
        assert_eq!(cooked.slots[0], stack("minecraft:raw_iron", 2));
        assert_eq!(cooked.slots[2], stack("minecraft:iron_ingot", 1));

        game.world = Some(world);
        click(&mut game, 2, 1);
        assert_eq!(
            game.players[0].inventory.slots[HOTBAR + 7],
            stack("minecraft:iron_ingot", 1)
        );
        assert!(game.world.as_ref().unwrap().furnace(pos).unwrap().slots[2].is_empty());
        assert!(game.destroy_block(0, pos));
        assert!(game.players[0].block_menu.is_none());
        let world = game.world.as_ref().unwrap();
        assert!(!world.active_furnaces.contains(&pos));
        assert!(world.container_tag(pos, FURNACE).unwrap().is_none());
    }

    #[test]
    fn simple_chest_shares_items_between_viewers_and_drops_them_on_break() {
        use crate::inventory::{CHEST_SLOTS, HOTBAR, ItemStack};
        use harpas_core::block_behavior::Face;
        use harpas_core::item::items;

        let folder = Folder::new();
        let chunk_pos = ChunkPos { x: 0, z: 0 };
        save(&folder, &[(chunk_pos, DATA_VERSION)]);
        let (first_output, first) = crate::player::output_channel();
        let (second_output, second) = crate::player::output_channel();
        let mut game = Game {
            pending: Vec::new(),
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            viewers: Vec::new(),
            stopped: false,
        };
        game.join(1, profile(), (2, 2), false, first_output, None);
        let mut other = profile();
        other.uuid = 2;
        game.join(2, other, (2, 2), false, second_output, None);
        wait_until(|| {
            // Acknowledge each batch so the saved chunk can arrive after its empty neighbors.
            for player in &mut game.players {
                player.awaiting_chunk_batch = false;
            }
            tick(&mut game, 1);
            game.players
                .iter()
                .all(|player| player.sent_chunks.contains(&chunk_pos))
        });
        for (index, player) in game.players.iter_mut().enumerate() {
            player.awaiting_teleport = None;
            player.player.position = crate::player::Position {
                x: 7.5 + index as f64,
                y: -51.0,
                z: 7.5,
                ..Default::default()
            };
            player.player.profile.game_mode = GameMode::Survival;
        }
        let chest = BlockPos { x: 8, y: -51, z: 9 };
        let below = BlockPos {
            y: chest.y - 1,
            ..chest
        };
        let above = BlockPos {
            y: chest.y + 1,
            ..chest
        };
        let states = block_states();
        let air = states.default_state("minecraft:air").unwrap();
        let stone = states.default_state("minecraft:stone").unwrap();
        let world = game.world.as_mut().unwrap();
        world.set_block_state(chest, air);
        let stack = |name: &str, count| ItemStack {
            item: items().id(name).unwrap(),
            count,
            ..ItemStack::empty()
        };
        game.players[0].inventory.slots[HOTBAR] = stack(crate::chest::CHEST, 1);
        game.use_item_on(0, false, below, Face::Up, [0.5; 3], 1);
        assert!(game.players[0].inventory.slots[HOTBAR].is_empty());
        let world = game.world.as_mut().unwrap();
        let placed = world.block_state(chest).unwrap();
        assert!(
            states
                .properties(placed)
                .unwrap()
                .1
                .contains(&("type", "single"))
        );
        assert!(
            states
                .properties(placed)
                .unwrap()
                .1
                .contains(&("facing", "north"))
        );
        assert!(world.chest_tag(chest).unwrap().is_some());

        game.use_item_on(0, false, chest, Face::Up, [0.5; 3], 2);
        assert_eq!(game.players[0].container_id(), 0, "blocked by stone");
        assert_eq!(
            game.world.as_mut().unwrap().set_block_state(above, air),
            Some(stone)
        );
        first.try_iter().for_each(drop);
        second.try_iter().for_each(drop);
        game.players[0].inventory.slots[HOTBAR] = stack("minecraft:diamond", 9);
        for (index, output) in [&first, &second].into_iter().enumerate() {
            game.use_item_on(index, false, chest, Face::Up, [0.5; 3], 3);
            assert_eq!(
                output.try_recv(),
                Ok(Output::OpenChest {
                    container_id: 1,
                    double: false,
                    title: None
                })
            );
            assert!(
                matches!(output.try_recv(), Ok(Output::InventoryContent { container_id: 1, slots, .. }) if slots.len() == 63)
            );
            output.try_iter().for_each(drop);
        }
        let click = |game: &mut Game, index: usize, slot, button, mode| {
            let id = game.players[index]
                .block_menu
                .as_ref()
                .unwrap()
                .contents
                .state_id();
            game.click_container(index, 1, id, slot, button, mode, Vec::new(), None);
        };
        // Shift-click into slot zero, then take half from a second viewer's screen before the next tick.
        click(&mut game, 0, 54, 0, 1);
        click(&mut game, 1, 0, 1, 0);
        assert_eq!(
            game.players[1].inventory.carried,
            stack("minecraft:diamond", 5)
        );
        game.send_inventory_changes();
        assert!(first.try_iter().any(|output| matches!(
            output, Output::SetSlot { container_id: 1, slot: 0, stack: changed, .. }
            if changed == stack("minecraft:diamond", 4)
        )));
        click(&mut game, 0, 0, 0, 1);
        assert_eq!(
            game.players[0].inventory.slots[HOTBAR + 8],
            stack("minecraft:diamond", 4)
        );
        assert!(game.world.as_ref().unwrap().chest_items(chest).unwrap()[0].is_empty());
        // The last visible slot is the last hotbar slot. Shift-click it back into the empty chest.
        click(&mut game, 0, 62, 0, 1);
        assert!(game.players[0].inventory.slots[HOTBAR + 8].is_empty());
        let world = game.world.as_ref().unwrap();
        let items = world.chest_items(chest).unwrap();
        assert_eq!(items[0], stack("minecraft:diamond", 4));
        assert!(items[1..CHEST_SLOTS].iter().all(ItemStack::is_empty));

        // Breaking in Creative still scatters the stored items and closes both screens.
        game.players[0].player.profile.game_mode = GameMode::Creative;
        assert!(game.destroy_block(0, chest));
        assert!(game.players.iter().all(|p| p.block_menu.is_none()));
        assert!(first.try_iter().any(|o| o == Output::CloseContainer(1)));
        assert!(second.try_iter().any(|o| o == Output::CloseContainer(1)));
        assert_eq!(
            game.players[1].inventory.slots[HOTBAR],
            stack("minecraft:diamond", 5)
        );
        let world = game.world.as_ref().unwrap();
        assert!(world.chest_tag(chest).unwrap().is_none());
        let dropped: Vec<_> = world
            .entities
            .order
            .iter()
            .filter_map(|&h| world.entities.item(h))
            .map(|item| item.stack.clone())
            .collect();
        assert_eq!(dropped, [stack("minecraft:diamond", 4)]);

        // Without the loot data, trying to open or break the chest preserves its seed.
        let tag = Compound::from_iter([
            ("id", Nbt::from(crate::chest::CHEST)),
            ("x", Nbt::Int(chest.x)),
            ("y", Nbt::Int(chest.y)),
            ("z", Nbt::Int(chest.z)),
            ("LootTable", Nbt::from("minecraft:chests/simple_dungeon")),
            ("LootTableSeed", Nbt::Long(23)),
        ]);
        let place_unopened = |world: &mut World| {
            assert!(world.set_block_state(chest, placed).is_some());
            let chunk = world.chunk_contents_mut(chunk_pos).unwrap();
            chunk.rest.insert(
                "block_entities",
                Nbt::List(harpas_core::nbt::List::Compound(vec![tag.clone()])),
            );
        };
        place_unopened(game.world.as_mut().unwrap());
        game.use_item_on(0, false, chest, Face::Up, [0.5; 3], 4);
        assert_eq!(game.players[0].container_id(), 0);
        assert!(!game.destroy_block(0, chest));
        assert_eq!(
            game.world.as_ref().unwrap().chest_tag(chest).unwrap(),
            Some(&tag)
        );

        use std::io::{Cursor, Write};
        use zip::write::SimpleFileOptions;
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (path, json) in [
            (
                "loot_table/chests/simple_dungeon",
                r#"{"pools":[{"rolls":1,"entries":[{"type":"minecraft:item","name":"minecraft:diamond"}]}]}"#,
            ),
            (
                "tags/enchantment/on_random_loot",
                r#"{"values":["minecraft:sharpness"]}"#,
            ),
            ("enchantment/sharpness", r#"{"max_level":5}"#),
        ] {
            zip.start_file(
                format!("data/minecraft/{path}.json"),
                SimpleFileOptions::default(),
            )
            .unwrap();
            zip.write_all(json.as_bytes()).unwrap();
        }
        let loot = crate::loot::LootTables::from_zip_reader(zip.finish().unwrap()).unwrap();
        game.world.as_mut().unwrap().set_loot_tables(Arc::new(loot));

        game.players[0].player.profile.game_mode = GameMode::Spectator;
        game.use_item_on(0, false, chest, Face::Up, [0.5; 3], 5);
        assert_eq!(game.players[0].container_id(), 0);
        assert_eq!(
            game.world.as_ref().unwrap().chest_tag(chest).unwrap(),
            Some(&tag)
        );

        game.players[0].player.profile.game_mode = GameMode::Survival;
        game.use_item_on(0, false, chest, Face::Up, [0.5; 3], 6);
        let world = game.world.as_ref().unwrap();
        let slot = world
            .chest_items(chest)
            .unwrap()
            .iter()
            .position(|item| *item == stack("minecraft:diamond", 1))
            .unwrap();
        let menu = game.players[0].container_id();
        assert_ne!(menu, 0);
        game.click_container(0, menu, -1, slot as i16, 0, 1, Vec::new(), None);
        game.close_container(0, menu);
        game.use_item_on(0, false, chest, Face::Up, [0.5; 3], 7);
        assert_ne!(game.players[0].container_id(), 0);
        assert!(
            game.world
                .as_ref()
                .unwrap()
                .chest_items(chest)
                .unwrap()
                .iter()
                .all(ItemStack::is_empty)
        );

        // A spent chest does not roll loot again. Breaking an unopened one drops its roll once.
        assert!(game.destroy_block(0, chest));
        let world = game.world.as_mut().unwrap();
        assert_eq!(world.entities.order.len(), 1);
        place_unopened(world);
        assert!(game.destroy_block(0, chest));
        assert!(!game.destroy_block(0, chest));
        let world = game.world.as_ref().unwrap();
        assert_eq!(world.block_state(chest), Some(air));
        assert!(world.chest_tag(chest).unwrap().is_none());
        let dropped: Vec<_> = world
            .entities
            .order
            .iter()
            .filter_map(|&h| world.entities.item(h))
            .map(|item| item.stack.clone())
            .collect();
        assert_eq!(
            dropped,
            [stack("minecraft:diamond", 4), stack("minecraft:diamond", 1)]
        );
    }

    #[test]
    fn double_chest_unpacks_two_loot_tables_with_independent_random_sequences() {
        use std::io::{Cursor, Write};

        use crate::chest::{CHEST, Chest};
        use crate::item_data::ItemCodec;
        use crate::loot::LootTables;
        use harpas_core::random::RandomSequence;
        use serde_json::json;
        use zip::write::SimpleFileOptions;

        let folder = Folder::new();
        let left = BlockPos { x: 8, y: -51, z: 9 };
        let right = BlockPos { x: 9, ..left };
        save(&folder, &[(left.chunk(), DATA_VERSION)]);
        let mut world = World::open(&folder.0, BIOMES).unwrap();
        load_chunks(&mut world, &[left.chunk()]);
        let halves = [
            (right, "right", "chests/simple_dungeon", "minecraft:diamond"),
            (
                left,
                "left",
                "chests/desert_pyramid",
                "minecraft:gold_ingot",
            ),
        ];
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let mut tags = Vec::new();
        for (pos, kind, table, item) in halves {
            let state = block_states()
                .state(CHEST, [("facing", "north"), ("type", kind)])
                .unwrap()
                .0;
            world.set_block_state(pos, state).unwrap();
            tags.push(Compound::from_iter([
                ("id", Nbt::from(CHEST)),
                ("x", Nbt::Int(pos.x)),
                ("y", Nbt::Int(pos.y)),
                ("z", Nbt::Int(pos.z)),
                ("LootTable", Nbt::from(table)),
            ]));
            zip.start_file(
                format!("data/minecraft/loot_table/{table}.json"),
                SimpleFileOptions::default(),
            )
            .unwrap();
            write!(
                zip,
                "{}",
                json!({
                    "random_sequence": format!("minecraft:{table}"),
                    "pools": [{"rolls": 1, "entries": [{"type": "minecraft:item", "name": item}]}],
                })
            )
            .unwrap();
        }
        for (path, json) in [
            (
                "tags/enchantment/on_random_loot",
                r#"{"values":["minecraft:sharpness"]}"#,
            ),
            ("enchantment/sharpness", r#"{"max_level":5}"#),
        ] {
            zip.start_file(
                format!("data/minecraft/{path}.json"),
                SimpleFileOptions::default(),
            )
            .unwrap();
            zip.write_all(json.as_bytes()).unwrap();
        }
        let loot = Arc::new(LootTables::from_zip_reader(zip.finish().unwrap()).unwrap());
        world.set_loot_tables(loot.clone());

        tags[1].insert("LootTableSeed", "invalid");
        let write_tags = |world: &mut World, tags: &[Compound]| {
            world.chunk_contents_mut(left.chunk()).unwrap().rest.insert(
                "block_entities",
                Nbt::List(harpas_core::nbt::List::Compound(tags.to_vec())),
            );
        };
        write_tags(&mut world, &tags);
        let chest = Chest::at(&world, left).unwrap();
        assert!(chest.is_double());
        assert!(world.open_chest_menu(chest, false).is_err());
        assert_eq!(world.chest_tag(right).unwrap(), Some(&tags[0]));
        assert!(world.random_sequences.is_empty());

        tags[1].remove("LootTableSeed");
        write_tags(&mut world, &tags);
        let (menu, _) = world.open_chest_menu(chest, false).unwrap();
        let chests = loot.chests.as_ref().unwrap();
        let codec = ItemCodec::default();
        let mut expected = Vec::new();
        for (pos, _, id, _) in halves {
            let name = format!("minecraft:{id}");
            let mut sequence = RandomSequence::new(world.seed, Some(&name));
            expected.extend(
                chests
                    .fill(chests.get(id).unwrap(), &codec, sequence.random(), None)
                    .unwrap(),
            );
            assert_eq!(world.random_sequences.get(&name), Some(&sequence));
            assert!(
                world
                    .chest_tag(pos)
                    .unwrap()
                    .unwrap()
                    .get("LootTable")
                    .is_none()
            );
        }
        assert_eq!(menu, expected);
    }

    #[test]
    fn chests_search_for_a_treasure_map_only_when_their_loot_rolls_one() {
        use std::io::{Cursor, Write};

        use crate::chest::{CHEST, Chest};
        use crate::inventory::ItemStack;
        use crate::loot::LootTables;
        use harpas_core::random::{LegacyRandom, RandomSource};
        use zip::write::SimpleFileOptions;

        let folder = Folder::new();
        let pos = BlockPos { x: 8, y: -51, z: 9 };
        save(&folder, &[(pos.chunk(), DATA_VERSION)]);
        let mut world = World::open(&folder.0, BIOMES).unwrap();
        load_chunks(&mut world, &[pos.chunk()]);
        let state = block_states()
            .state(CHEST, [("facing", "north"), ("type", "single")])
            .unwrap()
            .0;
        world.set_block_state(pos, state).unwrap();

        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (path, json) in [
            (
                "loot_table/chests/underwater_ruin_small",
                r#"{"pools":[{"rolls":1,"entries":[
                    {"type":"minecraft:item","name":"minecraft:map","functions":[
                        {"function":"minecraft:exploration_map","decoration":"minecraft:red_x",
                         "zoom":1,"skip_existing_chunks":false},
                        {"function":"minecraft:set_name","target":"item_name",
                         "name":{"translate":"filled_map.buried_treasure"}}]},
                    {"type":"minecraft:empty"}]}]}"#,
            ),
            (
                "tags/enchantment/on_random_loot",
                r#"{"values":["minecraft:sharpness"]}"#,
            ),
            ("enchantment/sharpness", r#"{"max_level":5}"#),
        ] {
            zip.start_file(
                format!("data/minecraft/{path}.json"),
                SimpleFileOptions::default(),
            )
            .unwrap();
            zip.write_all(json.as_bytes()).unwrap();
        }
        let loot = LootTables::from_zip_reader(zip.finish().unwrap()).unwrap();
        world.set_loot_tables(Arc::new(loot));
        world.set_item_codec(Arc::new(crate::item_data::ItemCodec::new(&[
            "minecraft:item_name".to_owned(),
        ])));

        // The pool's only draw picks the map, the first entry, on 0.
        let seed = |rolls_map: bool| {
            (1..)
                .find(|&seed| (LegacyRandom::new(seed).next_int_bounded(2) == 0) == rolls_map)
                .unwrap()
        };
        for rolls_map in [false, true] {
            let tag = Compound::from_iter([
                ("id", Nbt::from(CHEST)),
                ("x", Nbt::Int(pos.x)),
                ("y", Nbt::Int(pos.y)),
                ("z", Nbt::Int(pos.z)),
                (
                    "LootTable",
                    Nbt::from("minecraft:chests/underwater_ruin_small"),
                ),
                ("LootTableSeed", Nbt::Long(seed(rolls_map))),
            ]);
            world.chunk_contents_mut(pos.chunk()).unwrap().rest.insert(
                "block_entities",
                Nbt::List(harpas_core::nbt::List::Compound(vec![tag.clone()])),
            );
            let chest = Chest::at(&world, pos).unwrap();
            let opened = world.open_chest_menu(chest, false);
            if !rolls_map {
                let (menu, _) = opened.unwrap();
                assert!(menu.iter().all(ItemStack::is_empty));
                continue;
            }
            // The search must finish before the chest publishes its items. This world has no
            // generator, so it finds nothing and the chest keeps the named empty map.
            assert_eq!(opened.unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
            assert_eq!(world.chest_tag(pos).unwrap(), Some(&tag));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let menu = loop {
                match world.open_chest_menu(chest, false) {
                    Ok((menu, _)) => break menu,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(std::time::Instant::now() < deadline);
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            let maps: Vec<_> = menu.iter().filter(|stack| !stack.is_empty()).collect();
            assert_eq!(maps.len(), 1);
            assert_eq!(maps[0].name(), Some("minecraft:map"));
        }
    }

    #[test]
    fn crafting_tables_have_independent_menus_and_return_items_when_they_close() {
        use crate::inventory::ItemStack;
        use harpas_core::block_behavior::Face;
        use harpas_core::item::items;

        let folder = Folder::new();
        let chunk_pos = ChunkPos { x: 0, z: 0 };
        save(&folder, &[(chunk_pos, DATA_VERSION)]);
        let (output, received) = crate::player::output_channel();
        let mut game = Game {
            pending: vec![Event::Joined {
                entity_id: 1,
                profile: profile(),
                view_distance: 2,
                initial_view_distance: 2,
                allow_flight: false,
                output,
                force_game_mode_on_login: None,
            }],
            players: Vec::new(),
            online_players: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            world: Some(World::open(&folder.0, BIOMES).unwrap()),
            viewers: Vec::new(),
            stopped: false,
        };
        tick(&mut game, 1);
        wait_until(|| {
            game.players[0].awaiting_chunk_batch = false;
            tick(&mut game, 2);
            game.players[0].sent_chunks.contains(&chunk_pos)
        });
        let table = BlockPos { x: 8, y: -51, z: 9 };
        let crafting = block_states()
            .default_state("minecraft:crafting_table")
            .unwrap();
        let stone = block_states().default_state("minecraft:stone").unwrap();
        game.world
            .as_mut()
            .unwrap()
            .set_block_state(table, crafting);
        game.players[0].awaiting_teleport = None;
        game.players[0].player.position = crate::player::Position {
            x: 8.5,
            y: -51.0,
            z: 8.5,
            ..Default::default()
        };
        game.players[0].player.profile.game_mode = GameMode::Survival;
        received.try_iter().for_each(drop);
        game.use_item_on(0, false, table, Face::Up, [0.5; 3], 1);
        assert_eq!(received.try_recv(), Ok(Output::OpenCraftingTable(1)));
        assert!(
            matches!(received.try_recv(), Ok(Output::InventoryContent { container_id: 1, ref slots, .. }) if slots.len() == 46)
        );
        let stack = |name: &str, count| ItemStack {
            item: items().id(name).unwrap(),
            count,
            components: Default::default(),
        };
        // Correct predictions send nothing; a stale state resends this table, and a wrong prediction is
        // corrected with the table's window ID rather than the inventory's ID.
        game.players[0].inventory.slots[36] = stack("minecraft:stone", 8);
        game.send_inventory_changes();
        received.try_iter().for_each(drop);
        let menu_state = game.players[0]
            .block_menu
            .as_ref()
            .unwrap()
            .contents
            .state_id();
        let predicted = Some(crate::inventory::HashedStack {
            item: items().id("minecraft:stone").unwrap(),
            count: 8,
            ..Default::default()
        });
        game.click_container(0, 1, menu_state, 37, 0, 0, vec![(37, None)], predicted);
        assert!(received.try_recv().is_err());
        assert_eq!(game.players[0].inventory.carried.count, 8);
        game.click_container(0, 1, -1, 37, 0, 0, Vec::new(), None);
        assert!(matches!(
            received.try_recv(),
            Ok(Output::InventoryContent {
                container_id: 1,
                ..
            })
        ));
        let menu_state = game.players[0]
            .block_menu
            .as_ref()
            .unwrap()
            .contents
            .state_id();
        game.click_container(0, 1, menu_state, 37, 0, 0, vec![(37, None)], None);
        assert_eq!(
            received.try_recv(),
            Ok(Output::SetCarried(stack("minecraft:stone", 8)))
        );
        game.click_container(0, 1, menu_state, 37, 0, 0, Vec::new(), None);
        received.try_iter().for_each(drop);
        game.players[0].inventory.slots[36] = ItemStack::empty();
        game.players[0].with_menu(|menu| {
            menu.slots[9] = stack("minecraft:oak_log", 3);
            menu.carried = stack("minecraft:cobblestone", 5);
        });
        // A second viewer has a separate grid, even at the same table.
        let (output, other) = crate::player::output_channel();
        let mut second = profile();
        second.uuid = 2;
        second.name = "Other".into();
        game.join(2, second, (2, 2), false, output, None);
        wait_until(|| {
            game.poll_player_data();
            game.players[1].data_ready
        });
        game.players[1].player.position = game.players[0].player.position;
        game.open_crafting_table(1, table);
        assert!(
            game.players[1].block_menu.as_ref().unwrap().contents.slots[..10]
                .iter()
                .all(ItemStack::is_empty)
        );
        // Stale window IDs cannot close or modify the active menu.
        game.close_container(0, 0);
        game.click_container(0, 0, -1, 9, 0, 0, Vec::new(), None);
        assert_eq!(
            game.players[0].block_menu.as_ref().unwrap().contents.slots[9].count,
            3
        );
        // A pickup updates the inventory while the menu is open, and the table sees it in its own layout.
        game.players[0].inventory.slots[36] = stack("minecraft:wheat", 2);
        game.send_inventory_changes();
        assert!(received.try_iter().any(|o| matches!(o, Output::SetSlot { container_id: 1, slot: 37, stack, .. } if stack.count == 2)));
        // Exactly eight blocks from the table's center remains valid, an extra fraction closes it.
        game.players[0].player.position = crate::player::Position {
            x: 16.5,
            y: -50.5,
            z: 9.5,
            ..Default::default()
        };
        game.send_inventory_changes();
        assert!(game.players[0].block_menu.is_some());
        game.players[0].player.position.x += 0.001;
        game.send_inventory_changes();
        assert!(game.players[0].block_menu.is_none());
        assert!(received.try_iter().any(|o| o == Output::CloseContainer(1)));
        let count = |inventory: &crate::inventory::Inventory, name: &str| {
            inventory.slots[9..45]
                .iter()
                .filter(|s| s.name() == Some(name))
                .map(|s| s.count)
                .sum::<i32>()
        };
        assert_eq!(count(&game.players[0].inventory, "minecraft:oak_log"), 3);
        assert_eq!(
            count(&game.players[0].inventory, "minecraft:cobblestone"),
            5
        );
        game.close_container(0, 1);
        assert_eq!(count(&game.players[0].inventory, "minecraft:oak_log"), 3);
        // Replacing a table also invalidates menus; returned ingredients are not left in the block.
        game.players[1].with_menu(|menu| menu.slots[8] = stack("minecraft:diamond", 2));
        game.world.as_mut().unwrap().set_block_state(table, stone);
        game.send_inventory_changes();
        assert!(game.players[1].block_menu.is_none());
        assert!(other.try_iter().any(|o| o == Output::CloseContainer(1)));
        assert_eq!(count(&game.players[1].inventory, "minecraft:diamond"), 2);
        // Opening another table closes the previous grid, increments the ID and rejects old clicks.
        game.world
            .as_mut()
            .unwrap()
            .set_block_state(table, crafting);
        game.players[0].player.position = game.players[1].player.position;
        game.open_crafting_table(0, table);
        assert_eq!(game.players[0].container_id(), 2);
        game.players[0].with_menu(|menu| menu.slots[7] = stack("minecraft:iron_ingot", 4));
        game.open_crafting_table(0, table);
        assert_eq!(game.players[0].container_id(), 3);
        assert_eq!(count(&game.players[0].inventory, "minecraft:iron_ingot"), 4);
        game.close_container(0, 2);
        assert_eq!(game.players[0].container_id(), 3);
        // Secondary use with an item places against the table; bare hands still open it. Spectators do not.
        game.close_container(0, 3);
        game.players[0].secondary_use = true;
        game.players[0].inventory.slots[36] = stack("minecraft:stone", 1);
        game.use_item_on(0, false, table, Face::Up, [0.5; 3], 2);
        assert!(game.players[0].block_menu.is_none());
        game.players[0].inventory.slots[36] = ItemStack::empty();
        game.use_item_on(0, false, table, Face::Up, [0.5; 3], 3);
        assert_eq!(game.players[0].container_id(), 4);
        game.close_container(0, 4);
        game.players[0].player.profile.game_mode = GameMode::Spectator;
        game.use_item_on(0, false, table, Face::Up, [0.5; 3], 4);
        assert!(game.players[0].block_menu.is_none());
        // Disconnecting drops the transient grid and cursor instead of leaving them in a discarded menu.
        game.players[0].player.profile.game_mode = GameMode::Survival;
        game.open_crafting_table(0, table);
        game.players[0].with_menu(|menu| {
            menu.slots[9] = stack("minecraft:emerald", 3);
            menu.carried = stack("minecraft:apple", 1);
        });
        game.leave(1);
        let world = game.world.as_ref().unwrap();
        let dropped: Vec<_> = world
            .entities
            .order
            .iter()
            .filter_map(|&h| world.entities.item(h))
            .map(|i| (i.stack.name(), i.stack.count))
            .collect();
        assert_eq!(
            dropped,
            [(Some("minecraft:apple"), 1), (Some("minecraft:emerald"), 3)]
        );
    }
    fn item_stack(count: i32) -> crate::inventory::ItemStack {
        crate::inventory::ItemStack {
            item: harpas_core::item::items().id("minecraft:stone").unwrap(),
            count,
            components: Arc::default(),
        }
    }

    fn entity_document(folder: &Folder, pos: ChunkPos) -> EntityChunk {
        let dir = dimension_dir(&folder.0, "minecraft:overworld").join("entities");
        EntityChunk::read(
            read_chunk(&dir, pos.x, pos.z).unwrap().unwrap(),
            pos,
            &ItemCodec::default(),
        )
        .unwrap()
    }

    #[test]
    fn saved_adult_pigs_survive_chunk_unload_and_reload() {
        use crate::player::PigVariant;
        use harpas_core::nbt::List;

        let folder = Folder::new();
        let pos = ChunkPos { x: -1, z: -1 };
        save(&folder, &[(pos, DATA_VERSION)]);
        let entities = dimension_dir(&folder.0, "minecraft:overworld").join("entities");
        let pig = Compound::from_iter([
            ("id", Nbt::from("minecraft:pig")),
            ("UUID", Nbt::IntArray(vec![1, 2, 3, 4])),
            ("Pos", Nbt::List(List::Double(vec![-0.5, 70.25, -15.5]))),
            ("Motion", Nbt::List(List::Double(vec![0.0; 3]))),
            ("OnGround", Nbt::Byte(1)),
            ("Rotation", Nbt::List(List::Float(vec![-90.0, 22.5]))),
            ("Health", Nbt::Float(7.5)),
            ("Age", Nbt::Int(0)),
            ("variant", Nbt::from("minecraft:warm")),
            ("future_field", Nbt::from("retained")),
        ]);
        let mut baby = pig.clone();
        baby.insert("UUID", Nbt::IntArray(vec![1, 2, 3, 5]));
        baby.insert("Age", Nbt::Int(-1200));
        let mut unknown = baby.clone();
        unknown.insert("id", "minecraft:cow");
        let document = Compound::from_iter([
            ("DataVersion", Nbt::Int(DATA_VERSION)),
            ("Position", Nbt::IntArray(vec![pos.x, pos.z])),
            (
                "Entities",
                List::Compound(vec![pig.clone(), baby.clone(), unknown.clone()]).into(),
            ),
        ]);
        write_chunks(&entities, -1, -1, &[((pos.x, pos.z), &document)]).unwrap();

        let mut world = World::open(&folder.0, BIOMES).unwrap();
        world.unload_save_delay = Duration::ZERO;
        load_chunks(&mut world, &[pos]);
        let [handle] = world.entities.order[..] else {
            panic!("expected one active adult pig");
        };
        let first_id = world.entities.network_id[world.entities.slot(handle)];
        // Dropping an item dirties the same entity chunk and makes unloading save the pig too.
        world.add_item(
            6000,
            [-1.5, 70.25, -15.5],
            [0.0; 3],
            ItemEntity::new(item_stack(1)),
        );
        world.want([]);
        wait_until(|| {
            world.advance().unwrap();
            world.chunk(pos).is_none()
        });
        assert!(world.entities.order.is_empty());
        let saved = read_entity_chunk(&entities, pos.x, pos.z).unwrap().unwrap();
        let Some(Nbt::List(List::Compound(entries))) = saved.get("Entities") else {
            panic!("expected saved entities");
        };
        assert_eq!(entries.len(), 4);
        assert!(entries.contains(&pig) && entries.contains(&baby) && entries.contains(&unknown));

        load_chunks(&mut world, &[pos]);
        let i = world
            .entities
            .kind
            .iter()
            .position(|kind| matches!(kind, Some(Kind::Pig(_))))
            .unwrap();
        let Some(Kind::Pig(loaded)) = &world.entities.kind[i] else {
            unreachable!()
        };
        let reloaded_id = world.entities.network_id[i];
        assert_ne!(reloaded_id, first_id);
        assert_eq!(
            world.entities.uuid[i],
            0x0000_0001_0000_0002_0000_0003_0000_0004
        );
        assert_eq!(world.entities.position[i], [-0.5, 70.25, -15.5]);
        assert_eq!(
            (loaded.rotation, loaded.health, loaded.variant),
            ([-90.0, 22.5], 7.5, PigVariant::Warm)
        );

        // A save during the death animation must already omit the adult pig.
        let handle = world.entities.pig_target(reloaded_id).unwrap();
        let Some(Kind::Pig(loaded)) = &mut world.entities.kind[i] else {
            unreachable!();
        };
        loaded.health = 0.0;
        world.entities.mark_changed(handle);
        world.flush().unwrap();
        assert!(world.entities.contains(handle));
        let saved = read_entity_chunk(&entities, pos.x, pos.z).unwrap().unwrap();
        let Some(Nbt::List(List::Compound(entries))) = saved.get("Entities") else {
            panic!("expected saved entities");
        };
        assert_eq!(entries.len(), 3);
        assert!(entries.contains(&baby) && entries.contains(&unknown));
        assert!(!entries.contains(&pig));
        for _ in 0..19 {
            world.tick_entities();
            assert!(world.entities.contains(handle));
        }
        assert!(
            world
                .tick_entities()
                .iter()
                .any(|removal| removal.network_id == reloaded_id)
        );
        assert!(!world.entities.contains(handle));
    }

    #[test]
    fn item_entities_survive_shutdown_unload_and_reload_with_their_state() {
        use crate::entity::ItemEntity;
        let folder = Folder::new();
        let pos = ChunkPos { x: -1, z: -1 };
        save(&folder, &[(pos, DATA_VERSION)]);
        let entities = dimension_dir(&folder.0, "minecraft:overworld").join("entities");
        fs::create_dir_all(&entities).unwrap();
        fs::write(entities.join("r.-1.-1.mca"), []).unwrap();
        let mut world = World::open(&folder.0, BIOMES).unwrap();
        world.unload_save_delay = Duration::ZERO;
        load_chunks(&mut world, &[pos]);
        let mut item = ItemEntity::new(item_stack(7));
        item.age = 1234;
        item.pickup_delay = 32767;
        item.health = 2.0;
        Arc::make_mut(&mut item.rest).insert("future_field", "retained");
        let handle = world.add_item(6000, [-0.5, 70.25, -15.5], [0.125, -0.25, 0.5], item);
        let i = world.entities.slot(handle);
        world.entities.on_ground[i] = true;
        let uuid = world.entities.uuid[i];
        world.autosave().unwrap();
        wait_until(|| {
            world.advance().unwrap();
            world.changes.is_empty()
        });
        let saved = entity_document(&folder, pos);
        let item = &saved.items[0];
        assert_eq!(
            (item.uuid, item.age, item.pickup_delay, item.health),
            (uuid, 1234, 32767, 2.0)
        );
        assert_eq!(item.position, [-0.5, 70.25, -15.5]);
        assert_eq!(item.velocity, [0.125, -0.25, 0.5]);
        assert!(item.on_ground);
        assert_eq!(item.stack.count, 7);
        assert_eq!(item.rest.get("future_field"), Some(&Nbt::from("retained")));
        world.update([]).unwrap();
        assert!(world.entities.order.is_empty());
        assert!(world.chunk(pos).is_none());
        load_chunks(&mut world, &[pos]);
        let h = world.entities.order[0];
        let i = world.entities.slot(h);
        assert_eq!(world.entities.uuid[i], uuid);
        assert_ne!(world.entities.network_id[i], 6000);
        assert_eq!(world.entities.item(h).unwrap().age, 1234);
        assert!(world.entities.on_ground[i]);
        // The first drop after restarting the world must not reuse the saved entity's UUID.
        drop(world);
        let mut world = World::open(&folder.0, BIOMES).unwrap();
        load_chunks(&mut world, &[pos]);
        let restored = world.entities.order[0];
        let new = world.add_item(
            6001,
            [-8.0, 80.0, -8.0],
            [0.0; 3],
            ItemEntity::new(item_stack(3)),
        );
        assert_ne!(world.entities.uuid[world.entities.slot(new)], uuid);
        // Clearing the last items must replace the old document, even without block edits.
        world.entities.remove(restored);
        world.entities.remove(new);
        world.flush().unwrap();
        assert!(entity_document(&folder, pos).items.is_empty());
        drop(world);
        let mut world = World::open(&folder.0, BIOMES).unwrap();
        load_chunks(&mut world, &[pos]);
        assert!(world.entities.order.is_empty());
        let h = world.add_item(
            6001,
            [-8.0, 80.0, -8.0],
            [0.0; 3],
            ItemEntity::new(item_stack(3)),
        );
        let uuid = world.entities.uuid[world.entities.slot(h)];
        world.flush().unwrap();
        drop(world);
        let mut world = World::open(&folder.0, BIOMES).unwrap();
        load_chunks(&mut world, &[pos]);
        assert_eq!(world.entities.order.len(), 1);
        assert_eq!(
            world.entities.uuid[world.entities.slot(world.entities.order[0])],
            uuid
        );
    }

    #[test]
    fn shutdown_saves_drops_made_before_the_destination_chunk_is_loaded() {
        use crate::entity::ItemEntity;
        let folder = Folder::new();
        let pos = ChunkPos { x: -1, z: 0 };
        let drop_at = |world: &mut World, id, count| {
            world.add_item(
                id,
                [-1.25, 80.0, 0.5],
                [0.0; 3],
                ItemEntity::new(item_stack(count)),
            );
        };
        // There may be no terrain chunk, but the entity region can already contain other items.
        for count in [2, 3] {
            let mut world = World::open(&folder.0, BIOMES).unwrap();
            assert!(world.chunk(pos).is_none());
            drop_at(&mut world, 6000 + count, count);
            world.update([]).unwrap();
            world.flush().unwrap();
        }
        let doc = entity_document(&folder, pos);
        let mut counts: Vec<_> = doc.items.iter().map(|item| item.stack.count).collect();
        counts.sort_unstable();
        assert_eq!(counts, [2, 3]);
        assert!(
            read_chunk(&folder.region_dir(), pos.x, pos.z)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn moving_items_change_both_entity_chunks_and_do_not_duplicate_on_reload() {
        use crate::entity::ItemEntity;
        let folder = Folder::new();
        let from = ChunkPos { x: -1, z: 0 };
        let into = ChunkPos { x: 0, z: 0 };
        save(&folder, &[(from, DATA_VERSION), (into, DATA_VERSION)]);
        let mut world = World::open(&folder.0, BIOMES).unwrap();
        load_chunks(&mut world, &[from, into]);
        let mut item = ItemEntity::new(item_stack(4));
        item.pickup_delay = 200;
        let h = world.add_item(7000, [-0.05, 70.0, 8.0], [0.2, 0.0, 0.0], item);
        world.flush().unwrap();
        assert_eq!(entity_document(&folder, from).items.len(), 1);
        world.tick_entities();
        assert!(world.entities.position[world.entities.slot(h)][0] > 0.0);
        world.flush().unwrap();
        assert!(entity_document(&folder, from).items.is_empty());
        assert_eq!(entity_document(&folder, into).items.len(), 1);
        world.unload_save_delay = Duration::ZERO;
        world.update([]).unwrap();
        assert!(world.entities.order.is_empty());
        load_chunks(&mut world, &[into, from]);
        assert_eq!(world.entities.order.len(), 1);
        assert_eq!(
            world
                .entities
                .item(world.entities.order[0])
                .unwrap()
                .stack
                .count,
            4
        );
        // A subsequent crossing back through negative coordinates clears the other document too.
        let i = world.entities.slot(world.entities.order[0]);
        world.entities.velocity[i][0] = -0.3;
        world.tick_entities();
        world.flush().unwrap();
        assert_eq!(entity_document(&folder, from).items.len(), 1);
        assert!(entity_document(&folder, into).items.is_empty());
    }

    #[test]
    fn falling_pig_crosses_chunks_and_reloads_from_its_destination() {
        use harpas_core::nbt::List;

        let folder = Folder::new();
        let from = ChunkPos { x: 0, z: 0 };
        let into = ChunkPos { x: 1, z: 0 };
        save(&folder, &[(from, DATA_VERSION), (into, DATA_VERSION)]);
        let dir = dimension_dir(&folder.0, "minecraft:overworld").join("entities");
        let pig = Compound::from_iter([
            ("id", Nbt::from("minecraft:pig")),
            ("UUID", Nbt::IntArray(vec![1, 2, 3, 4])),
            ("Pos", List::Double(vec![15.7, -46.0, 4.5]).into()),
            ("Motion", List::Double(vec![0.4, 0.0, 0.0]).into()),
            ("Rotation", List::Float(vec![90.0, -22.5]).into()),
            ("Health", Nbt::Float(10.0)),
        ]);
        let initial = Compound::from_iter([
            ("DataVersion", Nbt::Int(DATA_VERSION)),
            ("Position", Nbt::IntArray(vec![0, 0])),
            ("Entities", List::Compound(vec![pig]).into()),
        ]);
        write_chunks(&dir, 0, 0, &[((0, 0), &initial)]).unwrap();
        let mut world = World::open(&folder.0, BIOMES).unwrap();
        load_chunks(&mut world, &[from, into]);
        let handle = world.entities.order[0];
        let i = world.entities.slot(handle);

        world.tick_entities();
        assert!(world.entities.position[i][0] > 16.0);
        assert_eq!(world.entities.position[i][1], -46.0);
        assert_eq!(world.entities.velocity[i][1], -0.0784000015258789);
        world.tick_entities();
        world.flush().unwrap();
        assert!(entity_document(&folder, from).pigs.is_empty());
        let [airborne] = &entity_document(&folder, into).pigs[..] else {
            panic!("expected one airborne pig in the destination");
        };
        assert_eq!(airborne.pig.fall_distance, 0.07840000092983246);
        assert!(!airborne.on_ground);

        for _ in 0..20 {
            world.tick_entities();
        }
        assert!(world.entities.on_ground[i]);
        let position = world.entities.position[i];
        assert_eq!(position[1], -48.0);
        assert!((position[0] - 18.462759423634665).abs() < 1.0e-10);
        world.flush().unwrap();
        drop(world);

        let mut world = World::open(&folder.0, BIOMES).unwrap();
        load_chunks(&mut world, &[into, from]);
        let [handle] = world.entities.order[..] else {
            panic!("expected exactly one pig after reopening both chunks");
        };
        let i = world.entities.slot(handle);
        assert_eq!(world.entities.uuid[i], airborne.uuid);
        assert_eq!(world.entities.position[i], position);
        assert!(world.entities.on_ground[i]);
        assert_eq!(
            entity_document(&folder, into).pigs[0].pig.fall_distance,
            0.0
        );
    }

    #[test]
    fn entity_snapshots_remain_immutable_and_unload_waits_for_newer_changes() {
        use crate::entity::ItemEntity;
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (started, received) = mpsc::channel();
        let writes = Arc::new(Mutex::new(Vec::new()));
        let save = {
            let gate = Arc::clone(&gate);
            let writes = Arc::clone(&writes);
            Arc::new(move |job: &SaveJob| {
                assert!(job.snapshots[0].chunk.is_none());
                let snapshot = job.snapshots[0].entities.as_ref().unwrap();
                started.send(()).unwrap();
                let (lock, signal) = &*gate;
                let guard = lock.lock().unwrap();
                let _ = signal
                    .wait_timeout_while(guard, Duration::from_secs(5), |open| !*open)
                    .unwrap();
                writes.lock().unwrap().push(
                    snapshot
                        .items
                        .iter()
                        .map(|i| i.stack.count)
                        .collect::<Vec<_>>(),
                );
                Ok(())
            })
        };
        let load = Arc::new(|pos| Ok((Some((chunk(pos, DATA_VERSION), Vec::new(), false)), None)));
        let mut world = fake_world_with_save(1, load, save);
        let pos = ChunkPos { x: 0, z: 0 };
        load_chunks(&mut world, &[pos]);
        let h = world.add_item(
            8000,
            [8.0, 80.0, 8.0],
            [0.0; 3],
            ItemEntity::new(item_stack(5)),
        );
        world.autosave().unwrap();
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        world.entities.set_stack(h, item_stack(2));
        world.update([]).unwrap();
        world.update([pos]).unwrap();
        assert!(world.entities.contains(h));
        assert!(matches!(world.chunk(pos), Some(ChunkState::Loaded(_))));
        world.entities.remove(h);
        world.update([]).unwrap();
        let (lock, signal) = &*gate;
        *lock.lock().unwrap() = true;
        signal.notify_all();
        wait_until(|| {
            world.update([]).unwrap();
            world.chunk(pos).is_none()
        });
        assert_eq!(*writes.lock().unwrap(), vec![vec![5], Vec::new()]);
    }

    #[test]
    fn opaque_entities_and_items_with_unsupported_components_are_preserved() {
        use crate::entity::ItemEntity;
        use harpas_core::nbt::List;
        let folder = Folder::new();
        let pos = ChunkPos { x: 0, z: 0 };
        save(&folder, &[(pos, DATA_VERSION)]);
        let mut world = World::open(&folder.0, BIOMES).unwrap();
        load_chunks(&mut world, &[pos]);
        world.add_item(
            9000,
            [8.0, 80.0, 8.0],
            [0.0; 3],
            ItemEntity::new(item_stack(1)),
        );
        world.flush().unwrap();
        drop(world);
        let dir = dimension_dir(&folder.0, "minecraft:overworld").join("entities");
        let mut root = read_chunk(&dir, 0, 0).unwrap().unwrap();
        let mut pig = Compound::new();
        pig.insert("id", "minecraft:pig");
        pig.insert("future_pig_field", 42);
        if let Some(Nbt::List(List::Compound(entities))) = root.remove("Entities") {
            let mut entries = entities;
            entries.push(pig.clone());
            root.insert("Entities", Nbt::List(List::Compound(entries)));
        }
        root.insert("DataVersion", 4790);
        root.insert("future_chunk_field", "kept");
        write_chunks(&dir, 0, 0, &[((0, 0), &root)]).unwrap();
        let mut world = World::open(&folder.0, BIOMES).unwrap();
        load_chunks(&mut world, &[pos]);
        world.entities.remove(world.entities.order[0]);
        world.flush().unwrap();
        drop(world);
        let saved = read_chunk(&dir, 0, 0).unwrap().unwrap();
        assert_eq!(
            saved.get("Entities"),
            Some(&Nbt::List(List::Compound(vec![pig])))
        );
        assert_eq!(saved.get("DataVersion"), Some(&Nbt::Int(4790)));
        assert_eq!(saved.get("future_chunk_field"), Some(&Nbt::from("kept")));
        // Put back an item with an unsupported component; it cannot be silently loaded and stripped.
        if let Some(Nbt::List(List::Compound(mut entries))) = root.remove("Entities") {
            if let Some(Nbt::Compound(mut item)) = entries[0].remove("Item") {
                let mut components = Compound::new();
                components.insert("minecraft:unknown_future_component", Nbt::Int(5));
                item.insert("components", Nbt::Compound(components));
                entries[0].insert("Item", Nbt::Compound(item));
            }
            root.insert("Entities", Nbt::List(List::Compound(entries)));
        }
        write_chunks(&dir, 0, 0, &[((0, 0), &root)]).unwrap();
        let mut world = World::open(&folder.0, BIOMES).unwrap();
        load_chunks(&mut world, &[pos]);
        assert!(world.entities.order.is_empty());
        let new = world.add_item(
            9001,
            [8.0, 80.0, 8.0],
            [0.0; 3],
            ItemEntity::new(item_stack(2)),
        );
        world.entities.remove(new);
        world.flush().unwrap();
        assert_eq!(read_chunk(&dir, 0, 0).unwrap().unwrap(), root);
    }
}
