//! Loading existing overworld chunks without doing disk I/O on the logic thread.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use harpas_anvil::{Chunk, Region, VersionError, dimension_dir, region_of};
use log::{error, warn};

/// Keep disk requests bounded even when players move faster than the disk can respond.
const QUEUE_CAPACITY: usize = 64;
const MAX_WORKERS: usize = 4;

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
}

/// The result of requesting an existing chunk.
#[derive(Debug)]
pub enum ChunkState {
    /// Waiting for a worker or for a place in the bounded work queue.
    Loading,
    /// The chunk was loaded and is owned by the logic thread.
    Loaded(Chunk),
    /// No chunk is stored at these coordinates. Generation is not implemented yet.
    Missing,
    /// The chunk exists or its region was accessed, but it could not be read.
    Failed(io::Error),
}

type Loaded = io::Result<Option<(Chunk, Vec<String>)>>;
type Load = dyn Fn(ChunkPos) -> Loaded + Send + Sync;

struct Completion {
    pos: ChunkPos,
    result: Loaded,
}

/// The chunks currently wanted by the logic thread and their asynchronous loader.
pub struct World {
    region_dir: PathBuf,
    chunks: HashMap<ChunkPos, ChunkState>,
    in_flight: HashSet<ChunkPos>,
    loader: Loader,
}

impl World {
    /// Opens the overworld in `world_dir`. `biomes` must be the server's biome registry in numeric ID order.
    /// Reading a region happens only after a chunk is requested by [`World::update`].
    pub fn open(world_dir: &Path, biomes: &[&str]) -> io::Result<Self> {
        if !biomes.contains(&"minecraft:plains") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the biome registry must include minecraft:plains",
            ));
        }
        let region_dir = dimension_dir(world_dir, "minecraft:overworld").join("region");
        let source_dir = region_dir.clone();
        let biomes: Vec<String> = biomes.iter().map(|name| (*name).to_owned()).collect();
        let load = Arc::new(move |pos: ChunkPos| {
            let (x, z) = region_of(pos.x, pos.z);
            let region = Region::open(&source_dir, x, z)?;
            let Some(nbt) = region.chunk(pos.x, pos.z)? else {
                return Ok(None);
            };
            let biomes: Vec<&str> = biomes.iter().map(String::as_str).collect();
            Chunk::read(nbt, &biomes).map(Some)
        });
        let workers = thread::available_parallelism()
            .map_or(1, usize::from)
            .min(MAX_WORKERS);
        Ok(Self {
            region_dir,
            chunks: HashMap::new(),
            in_flight: HashSet::new(),
            loader: Loader::start(workers, load)?,
        })
    }

    /// The current state of a wanted chunk, or `None` if it is no longer wanted.
    pub fn chunk(&self, pos: ChunkPos) -> Option<&ChunkState> {
        self.chunks.get(&pos)
    }

    /// Applies finished reads, drops unwanted chunks and queues missing requests without waiting for disk.
    /// An incompatible chunk or an unavailable worker pool is fatal and returned to the caller, even if the
    /// incompatible chunk is no longer wanted when its read finishes.
    pub fn update(&mut self, wanted: impl IntoIterator<Item = ChunkPos>) -> io::Result<()> {
        let wanted: HashSet<_> = wanted.into_iter().collect();
        self.chunks.retain(|pos, _| wanted.contains(pos));
        loop {
            match self.loader.completed.try_recv() {
                Ok(completion) => {
                    let pos = completion.pos;
                    self.in_flight.remove(&pos);
                    let prefix = format!(
                        "chunk {}, {} in {}",
                        pos.x,
                        pos.z,
                        self.region_dir.display()
                    );
                    let state = match completion.result {
                        Ok(Some((chunk, warnings))) => {
                            for warning in warnings {
                                warn!("{prefix}: {warning}");
                            }
                            ChunkState::Loaded(chunk)
                        }
                        Ok(None) => ChunkState::Missing,
                        Err(error) => {
                            if error
                                .get_ref()
                                .is_some_and(|source| source.is::<VersionError>())
                            {
                                return Err(io::Error::other(format!(
                                    "Cannot load {prefix}: {error}"
                                )));
                            }
                            error!("Cannot load {prefix}: {error}");
                            ChunkState::Failed(error)
                        }
                    };
                    if wanted.contains(&pos) {
                        self.chunks.insert(pos, state);
                    }
                }
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
    use std::time::{Duration, Instant};

    use harpas_anvil::{ChunkSection, DATA_VERSION};
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
        World {
            region_dir: PathBuf::from("test"),
            chunks: HashMap::new(),
            in_flight: HashSet::new(),
            loader: Loader::start(workers, load).unwrap(),
        }
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
                Ok(Some((chunk(pos, DATA_VERSION), Vec::new())))
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
    fn game_loads_the_occupied_chunk_on_the_dimension_tick_and_releases_it_on_movement() {
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
        assert!(game.world.as_ref().unwrap().chunk(origin).is_none());
        game.pending.push(Event::Left(1));
        tick(&mut game, 5);
        tick(&mut game, 6);
        assert!(game.world.as_ref().unwrap().chunk(west).is_none());
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
            Some(Output::Disconnect("Cannot load the world"))
        ));
        logic.join().unwrap();
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
