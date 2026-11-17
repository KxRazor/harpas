//! The persistent part of a player's inventory and its asynchronous, ordered `playerdata` I/O. The input
//! grids and cursor are transient menus, not additional saved inventory slots.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread::{self, JoinHandle};

use harpas_core::nbt::{Compound, List, Nbt};

use crate::inventory::{Inventory, ItemStack, SLOTS};
use crate::item_data::{ItemCodec, invalid};

const CAPACITY: usize = 64;
const EQUIPMENT: [(&str, usize); 5] = [
    ("head", 5),
    ("chest", 6),
    ("legs", 7),
    ("feet", 8),
    ("offhand", 45),
];

/// A vanilla player document with its playable inventory, preserving the unrelated player and item fields.
#[derive(Debug, Clone)]
pub struct PlayerInventory {
    /// The inventory menu's slots; only inventory, armor and offhand slots persist.
    pub slots: Vec<ItemStack>,
    /// The selected hotbar slot, from 0 to 8.
    pub selected: usize,
    root: Arc<Compound>,
}

impl PlayerInventory {
    /// Reads a vanilla 26.1 document. Unsupported or damaged items return an error instead of being removed.
    pub fn read(root: Compound, codec: &ItemCodec) -> io::Result<Self> {
        harpas_anvil::check_chunk(&root).map_err(invalid)?;
        let mut slots = vec![ItemStack::empty(); SLOTS];
        let entries = inventory_entries(&root)?;
        let mut used = HashSet::new();
        for entry in entries {
            let slot = entry_slot(entry)?;
            let Some(menu_slot) = saved_menu_slot(slot) else {
                continue;
            };
            if !used.insert(menu_slot) {
                return Err(invalid("duplicate inventory slot"));
            }
            slots[menu_slot] = codec.read(entry)?;
        }
        if let Some(equipment) = root.get("equipment") {
            let Nbt::Compound(equipment) = equipment else {
                return Err(invalid("equipment must be a compound"));
            };
            for (name, slot) in EQUIPMENT {
                if let Some(value) = equipment.get(name) {
                    let Nbt::Compound(tag) = value else {
                        return Err(invalid("equipment item must be a compound"));
                    };
                    if !used.insert(slot) {
                        return Err(invalid(
                            "equipment slot occurs in both equipment and Inventory",
                        ));
                    }
                    slots[slot] = codec.read(tag)?;
                }
            }
        }
        let selected = match root.get("SelectedItemSlot") {
            None => 0,
            Some(Nbt::Int(value @ 0..=8)) => *value as usize,
            _ => return Err(invalid("SelectedItemSlot must be an integer from 0 to 8")),
        };
        Ok(Self {
            slots,
            selected,
            root: Arc::new(root),
        })
    }

    /// Serializes the current inventory into its original document, leaving every unrelated field intact.
    pub fn to_nbt(&self, codec: &ItemCodec) -> io::Result<Compound> {
        if self.slots.len() != SLOTS || self.selected > 8 {
            return Err(invalid("invalid inventory snapshot"));
        }
        let mut root = (*self.root).clone();
        let mut entries = Vec::new();
        // Slots the engine does not model yet are retained, rather than erased by a save.
        for entry in inventory_entries(&root)? {
            if saved_menu_slot(entry_slot(entry)?).is_none() {
                entries.push(entry.clone());
            }
        }
        for index in 0..36 {
            let stack = &self.slots[Inventory::menu_slot(index)];
            if !stack.is_empty() {
                let mut tag = codec.write(stack)?;
                tag.insert("Slot", Nbt::Byte(index as i8));
                entries.push(tag);
            }
        }
        root.insert(
            "Inventory",
            Nbt::List(if entries.is_empty() {
                List::Empty
            } else {
                List::Compound(entries)
            }),
        );
        let mut equipment = match root.remove("equipment") {
            Some(Nbt::Compound(equipment)) => equipment,
            None => Compound::new(),
            _ => return Err(invalid("equipment must be a compound")),
        };
        for (name, slot) in EQUIPMENT {
            equipment.remove(name);
            if !self.slots[slot].is_empty() {
                equipment.insert(name, codec.write(&self.slots[slot])?);
            }
        }
        // Vanilla omits empty equipment.
        if !equipment.is_empty() {
            root.insert("equipment", equipment);
        }
        root.insert("SelectedItemSlot", self.selected as i32);
        Ok(root)
    }

    pub(crate) fn snapshot(&self, inventory: &Inventory) -> Self {
        Self {
            slots: inventory.slots.clone(),
            selected: inventory.selected,
            root: Arc::clone(&self.root),
        }
    }
}

fn inventory_entries(root: &Compound) -> io::Result<&[Compound]> {
    match root.get("Inventory") {
        None | Some(Nbt::List(List::Empty)) => Ok(&[]),
        Some(Nbt::List(List::Compound(entries))) => Ok(entries),
        _ => Err(invalid("Inventory must be a compound list")),
    }
}

fn entry_slot(entry: &Compound) -> io::Result<u8> {
    match entry.get("Slot") {
        Some(Nbt::Byte(slot)) => Ok(*slot as u8),
        _ => Err(invalid("inventory entry has no byte Slot")),
    }
}

fn saved_menu_slot(slot: u8) -> Option<usize> {
    match slot {
        0..=35 => Some(Inventory::menu_slot(slot as usize)),
        100..=103 => Some(108 - slot as usize),
        150 => Some(45),
        _ => None,
    }
}

pub(crate) struct Loaded {
    pub(crate) entity_id: i32,
    pub(crate) uuid: u128,
    pub(crate) result: io::Result<Arc<PlayerInventory>>,
}

enum Job {
    Load {
        entity_id: i32,
        uuid: u128,
        codec: Arc<ItemCodec>,
    },
    Save {
        uuid: u128,
        data: Arc<PlayerInventory>,
        codec: Arc<ItemCodec>,
    },
}

enum Completed {
    Loaded(Loaded),
    Saved {
        uuid: u128,
        data: Arc<PlayerInventory>,
        result: io::Result<()>,
    },
}

/// A bounded worker queue, with at most one outstanding write per UUID. Pending newer snapshots replace
/// older ones in memory; a reconnect uses the latest snapshot even when its write has not reached disk.
/// A single worker orders reads and writes, including the `_old` backup rotation.
pub(crate) struct PlayerStore {
    pub(crate) codec: Arc<ItemCodec>,
    requests: Option<SyncSender<Job>>,
    completed: Receiver<Completed>,
    pending: HashMap<u128, Arc<PlayerInventory>>,
    writing: HashSet<u128>,
    jobs: usize,
    thread: Option<JoinHandle<()>>,
}

impl PlayerStore {
    pub(crate) fn start(world: &Path, spawn: [i32; 3]) -> io::Result<Self> {
        let read_world = world.to_owned();
        let write_world = world.to_owned();
        Self::start_with_io(
            Arc::new(move |uuid| {
                let read = harpas_anvil::read_player(&read_world, uuid)?;
                if let Some((root, source)) = read {
                    if source == harpas_anvil::Source::Backup {
                        log::warn!("Loaded player data from its backup");
                    }
                    Ok(root)
                } else {
                    let mut root = new_player(uuid);
                    root.insert(
                        "Pos",
                        Nbt::List(List::Double(vec![
                            f64::from(spawn[0]) + 0.5,
                            f64::from(spawn[1]),
                            f64::from(spawn[2]) + 0.5,
                        ])),
                    );
                    Ok(root)
                }
            }),
            Arc::new(move |uuid, root| harpas_anvil::write_player(&write_world, uuid, root)),
        )
    }

    fn start_with_io(read: Arc<ReadPlayer>, write: Arc<WritePlayer>) -> io::Result<Self> {
        let (requests, receiver) = mpsc::sync_channel::<Job>(CAPACITY);
        let (sender, completed) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("harpas-player-data".to_owned())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    let completion = match job {
                        Job::Load {
                            entity_id,
                            uuid,
                            codec,
                        } => Completed::Loaded(Loaded {
                            entity_id,
                            uuid,
                            result: std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                read(uuid)
                                    .and_then(|root| PlayerInventory::read(root, &codec))
                                    .map(Arc::new)
                            }))
                            .unwrap_or_else(|_| {
                                Err(io::Error::other("player data read worker panicked"))
                            }),
                        }),
                        Job::Save { uuid, data, codec } => {
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    data.to_nbt(&codec).and_then(|root| write(uuid, &root))
                                }))
                                .unwrap_or_else(|_| {
                                    Err(io::Error::other("player data save worker panicked"))
                                });
                            Completed::Saved { uuid, data, result }
                        }
                    };
                    if sender.send(completion).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            codec: Arc::default(),
            requests: Some(requests),
            completed,
            pending: HashMap::new(),
            writing: HashSet::new(),
            jobs: 0,
            thread: Some(thread),
        })
    }

    pub(crate) fn load(
        &mut self,
        entity_id: i32,
        uuid: u128,
    ) -> io::Result<Option<Arc<PlayerInventory>>> {
        if let Some(latest) = self.pending.get(&uuid) {
            return Ok(Some(Arc::clone(latest)));
        }
        if self.pending.len() >= CAPACITY {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "player data saves are backlogged",
            ));
        }
        self.requests
            .as_ref()
            .expect("running worker")
            .try_send(Job::Load {
                entity_id,
                uuid,
                codec: Arc::clone(&self.codec),
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    io::Error::new(io::ErrorKind::WouldBlock, "player data queue is full")
                }
                TrySendError::Disconnected(_) => io::Error::other("player data worker stopped"),
            })?;
        self.jobs += 1;
        Ok(None)
    }

    pub(crate) fn save(&mut self, uuid: u128, data: PlayerInventory) {
        self.pending.insert(uuid, Arc::new(data));
    }

    pub(crate) fn poll(&mut self) -> io::Result<Vec<Loaded>> {
        let mut loaded = Vec::new();
        loop {
            match self.completed.try_recv() {
                Ok(completion) => self.finish(completion, &mut loaded)?,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err(io::Error::other("player data worker stopped"));
                }
            }
        }
        self.queue_saves()?;
        Ok(loaded)
    }

    fn finish(&mut self, completion: Completed, loaded: &mut Vec<Loaded>) -> io::Result<()> {
        self.jobs -= 1;
        match completion {
            Completed::Loaded(result) => loaded.push(result),
            Completed::Saved { uuid, data, result } => {
                self.writing.remove(&uuid);
                result.map_err(|error| {
                    io::Error::new(error.kind(), format!("saving player data: {error}"))
                })?;
                if self
                    .pending
                    .get(&uuid)
                    .is_some_and(|latest| Arc::ptr_eq(latest, &data))
                {
                    self.pending.remove(&uuid);
                }
            }
        }
        Ok(())
    }

    fn queue_saves(&mut self) -> io::Result<()> {
        for (&uuid, data) in &self.pending {
            if self.writing.contains(&uuid) {
                continue;
            }
            match self
                .requests
                .as_ref()
                .expect("running worker")
                .try_send(Job::Save {
                    uuid,
                    data: Arc::clone(data),
                    codec: Arc::clone(&self.codec),
                }) {
                Ok(()) => {
                    self.writing.insert(uuid);
                    self.jobs += 1;
                }
                Err(TrySendError::Full(_)) => break,
                Err(TrySendError::Disconnected(_)) => {
                    return Err(io::Error::other("player data worker stopped"));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn flush(&mut self) -> io::Result<()> {
        loop {
            self.poll()?;
            if self.jobs == 0 && self.pending.is_empty() {
                return Ok(());
            }
            let completion = self
                .completed
                .recv()
                .map_err(|_| io::Error::other("player data worker stopped"))?;
            self.finish(completion, &mut Vec::new())?;
        }
    }
}

impl Drop for PlayerStore {
    fn drop(&mut self) {
        self.requests.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

type ReadPlayer = dyn Fn(u128) -> io::Result<Compound> + Send + Sync;
type WritePlayer = dyn Fn(u128, &Compound) -> io::Result<()> + Send + Sync;

fn new_player(uuid: u128) -> Compound {
    Compound::from_iter([
        (
            "DataVersion".to_owned(),
            Nbt::Int(harpas_anvil::DATA_VERSION),
        ),
        (
            "UUID".to_owned(),
            Nbt::IntArray([96, 64, 32, 0].map(|shift| (uuid >> shift) as i32).to_vec()),
        ),
    ])
}

impl crate::Game {
    pub(crate) fn restore_inventory(
        player: &mut crate::player::PlayerState,
        data: Arc<PlayerInventory>,
    ) {
        player.inventory.slots = data.slots.clone();
        player.inventory.selected = data.selected;
        player.data_ready = true;
        player.saved_inventory = Some(data);
        let contents = crate::place::initial_contents(&mut player.inventory);
        player.send(contents);
        if player.inventory.selected != 0 {
            player.send(crate::player::Output::SelectedHotbarSlot(
                player.inventory.selected as i32,
            ));
        }
    }

    pub(crate) fn poll_player_data(&mut self) {
        let result = self
            .world
            .as_mut()
            .and_then(|world| world.player_store.as_mut())
            .map(PlayerStore::poll);
        match result {
            Some(Ok(loaded)) => {
                for loaded in loaded {
                    let Some(player) = self.players.iter_mut().find(|p| {
                        p.player.entity_id == loaded.entity_id
                            && p.player.profile.uuid == loaded.uuid
                            && !p.data_ready
                            && !p.disconnecting
                    }) else {
                        continue;
                    };
                    match loaded.result {
                        Ok(data) => Self::restore_inventory(player, data),
                        Err(error) => {
                            log::error!(
                                "Cannot load player {}: {error}",
                                player.player.profile.name
                            );
                            player.disconnecting = true;
                            player
                                .send(crate::player::Output::Disconnect("Cannot load player data"));
                        }
                    }
                }
            }
            Some(Err(error)) => self.fail_world(error),
            None => {}
        }
    }

    pub(crate) fn save_player_inventory(&mut self, index: usize) {
        let player = &self.players[index];
        if let Some(data) = &player.saved_inventory
            && let Some(store) = self
                .world
                .as_mut()
                .and_then(|world| world.player_store.as_mut())
        {
            store.save(player.player.profile.uuid, data.snapshot(&player.inventory));
        }
    }

    pub(crate) fn save_player_inventories(&mut self) {
        for index in 0..self.players.len() {
            self.save_player_inventory(index);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Condvar, Mutex};
    use std::time::{Duration, Instant};

    use super::*;
    use harpas_core::item::items;

    fn data(count: i32) -> PlayerInventory {
        let mut data = PlayerInventory::read(new_player(1), &ItemCodec::default()).unwrap();
        data.slots[36] = ItemStack {
            item: items().id("minecraft:stone").unwrap(),
            count,
            ..Default::default()
        };
        data
    }

    fn until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !condition() {
            assert!(Instant::now() < deadline, "worker timed out");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn inventory_mapping_preserves_unknown_player_item_and_equipment_fields() {
        let mut root = new_player(1);
        root.insert("Health", Nbt::Float(17.0));
        root.insert("SelectedItemSlot", 8);
        let entry = Compound::from_iter([
            ("Slot".to_owned(), Nbt::Byte(0)),
            ("id".to_owned(), Nbt::from("minecraft:stone")),
            ("count".to_owned(), Nbt::Int(3)),
            ("future_item_field".to_owned(), Nbt::from("keep")),
        ]);
        let reserved = Compound::from_iter([
            ("Slot".to_owned(), Nbt::Byte(90)),
            ("future".to_owned(), Nbt::Int(23)),
        ]);
        root.insert(
            "Inventory",
            Nbt::List(List::Compound(vec![entry, reserved.clone()])),
        );
        root.insert(
            "equipment",
            Compound::from_iter([
                (
                    "head".to_owned(),
                    Nbt::Compound(Compound::from_iter([
                        ("id".to_owned(), Nbt::from("minecraft:iron_helmet")),
                        ("count".to_owned(), Nbt::Int(1)),
                    ])),
                ),
                (
                    "saddle".to_owned(),
                    Nbt::Compound(Compound::from_iter([
                        ("id".to_owned(), Nbt::from("minecraft:saddle")),
                        ("count".to_owned(), Nbt::Int(1)),
                    ])),
                ),
            ]),
        );
        let codec = ItemCodec::default();
        let mut player = PlayerInventory::read(root.clone(), &codec).unwrap();
        assert_eq!(player.selected, 8);
        assert_eq!(player.slots[36].count, 3);
        assert_eq!(player.slots[5].name(), Some("minecraft:iron_helmet"));
        player.slots[9] = player.slots[36].clone();
        player.slots[9].count = 1;
        player.slots[36].count = 2;
        player.slots[0] = player.slots[9].clone();
        player.slots[1] = player.slots[9].clone();
        let saved = player.to_nbt(&codec).unwrap();
        assert_eq!(saved.get("Health"), root.get("Health"));
        let entries = inventory_entries(&saved).unwrap();
        assert_eq!(entries.len(), 3, "only persistent and reserved slots");
        assert_eq!(entries[0], reserved);
        assert_eq!(entries[1].get("count"), Some(&Nbt::Int(2)));
        assert_eq!(
            entries[2].get("future_item_field"),
            Some(&Nbt::from("keep"))
        );
        assert_eq!(saved.get("equipment"), root.get("equipment"));
    }

    #[test]
    fn a_reconnect_and_newer_save_do_not_wait_for_or_lose_to_an_older_write() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_gate = Arc::clone(&gate);
        let writes = Arc::new(Mutex::new(Vec::new()));
        let saved = Arc::clone(&writes);
        let mut store = PlayerStore::start_with_io(
            Arc::new(|_| panic!("pending snapshots must not be read from disk")),
            Arc::new(move |_, root| {
                let (lock, changed) = &*worker_gate;
                let _guard = changed
                    .wait_while(lock.lock().unwrap(), |open| !*open)
                    .unwrap();
                saved.lock().unwrap().push(root.clone());
                Ok(())
            }),
        )
        .unwrap();
        store.save(1, data(1));
        store.poll().unwrap();
        store.save(1, data(2));
        assert_eq!(store.load(10, 1).unwrap().unwrap().slots[36].count, 2);
        store.poll().unwrap();
        assert_eq!(store.jobs, 1, "one outstanding write per UUID");
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        store.flush().unwrap();
        let writes = writes.lock().unwrap();
        assert_eq!(writes.len(), 2);
        assert_eq!(
            PlayerInventory::read(writes[1].clone(), &ItemCodec::default())
                .unwrap()
                .slots[36]
                .count,
            2
        );
    }

    #[test]
    fn reads_do_not_block_the_logic_thread_and_corruption_is_not_an_empty_inventory() {
        let (release, waiting) = mpsc::channel();
        let waiting = Mutex::new(waiting);
        let mut store = PlayerStore::start_with_io(
            Arc::new(move |_| {
                waiting.lock().unwrap().recv().unwrap();
                Err(invalid("damaged player data"))
            }),
            Arc::new(|_, _| panic!("a failed read must not overwrite player data")),
        )
        .unwrap();
        assert!(store.load(11, 1).unwrap().is_none());
        assert!(store.poll().unwrap().is_empty());
        release.send(()).unwrap();
        let mut loaded = Vec::new();
        until(|| {
            loaded.extend(store.poll().unwrap());
            !loaded.is_empty()
        });
        assert_eq!(loaded[0].entity_id, 11);
        assert!(loaded[0].result.is_err());
        store.flush().unwrap();
    }

    #[test]
    fn queue_backpressure_and_save_failures_are_reported() {
        let mut store = PlayerStore::start_with_io(
            Arc::new(|_| Ok(new_player(1))),
            Arc::new(|_, _| Err(io::Error::other("disk full"))),
        )
        .unwrap();
        for uuid in 0..CAPACITY as u128 {
            store.save(uuid, data(1));
        }
        assert_eq!(
            store.load(7, 999).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let error = store.flush().unwrap_err().to_string();
        assert!(error.contains("disk full"));
        for uuid in 0..CAPACITY as u128 {
            assert!(!error.contains(&format!("{uuid:032x}")));
        }
        assert!(
            !store.pending.is_empty(),
            "failed writes retain their latest snapshot"
        );
    }
}

#[cfg(test)]
mod autosave_tests {
    use super::*;
    use crate::TickWork;
    use crate::player::{GameMode, PlayerState, Profile, output_channel};
    use crate::world::World;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicU32;

    #[test]
    fn autosave_captures_an_immutable_inventory_without_removing_live_menu_items() {
        let folder =
            std::env::temp_dir().join(format!("harpas-player-autosave-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let writes = Arc::new(Mutex::new(Vec::new()));
        let saved = Arc::clone(&writes);
        let mut world = World::open(&folder, &["minecraft:plains"]).unwrap();
        world.player_store = Some(
            PlayerStore::start_with_io(
                Arc::new(|_| Ok(new_player(1))),
                Arc::new(move |_, tag| {
                    saved.lock().unwrap().push(tag.clone());
                    Ok(())
                }),
            )
            .unwrap(),
        );
        let (output, _received) = output_channel();
        let mut player = PlayerState::new(
            1,
            Profile {
                uuid: 1,
                name: "Crafter".to_owned(),
                properties: Vec::new(),
                game_mode: GameMode::Creative,
            },
            2,
            true,
            output,
        );
        let document =
            Arc::new(PlayerInventory::read(new_player(1), &ItemCodec::default()).unwrap());
        crate::Game::restore_inventory(&mut player, document);
        player.inventory.slots[36] = ItemStack {
            item: 1,
            count: 16,
            ..Default::default()
        };
        player.inventory.slots[1] = ItemStack {
            item: 1,
            count: 3,
            ..Default::default()
        };
        player.inventory.carried = ItemStack {
            item: 1,
            count: 2,
            ..Default::default()
        };
        player.inventory.selected = 4;
        let mut game = crate::Game {
            pending: Vec::new(),
            players: vec![player],
            online_players: Arc::new(AtomicU32::new(1)),
            world: Some(world),
            viewers: Vec::new(),
            stopped: false,
        };
        game.autosave();
        game.players[0].inventory.slots[36].count = 15;
        game.world
            .as_mut()
            .unwrap()
            .player_store
            .as_mut()
            .unwrap()
            .flush()
            .unwrap();
        let snapshot = writes.lock().unwrap()[0].clone();
        let inventory = PlayerInventory::read(snapshot, &ItemCodec::default()).unwrap();
        assert_eq!(inventory.slots[36].count, 16);
        assert_eq!(inventory.selected, 4);
        assert!(inventory.slots[1].is_empty());
        assert_eq!(game.players[0].inventory.slots[1].count, 3);
        assert_eq!(game.players[0].inventory.carried.count, 2);
        drop(game);
        std::fs::remove_dir_all(folder).unwrap();
    }
}
