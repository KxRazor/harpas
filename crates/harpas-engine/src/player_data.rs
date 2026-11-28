//! Player inventory and game mode snapshots and their asynchronous, ordered `playerdata` I/O.
//! The input grids and cursor are transient menus, not additional saved inventory slots.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread::{self, JoinHandle};

use harpas_core::nbt::{Compound, List, Nbt};

use crate::inventory::{Inventory, ItemStack, SLOTS};
use crate::item_data::{ItemCodec, invalid};
use crate::player::{EffectKind, Experience, GameMode, PlayerState};
use crate::survival::{ActiveEffect, ActiveEffects, Vitals};

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
    mode: Option<SavedGameMode>,
    vitals: Option<Vitals>,
    experience: Experience,
    effects: ActiveEffects,
}

#[derive(Debug, Clone, Copy)]
struct SavedGameMode {
    current: GameMode,
    previous: Option<GameMode>,
    flying: bool,
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
        let xp_int = |key| match root.get(key) {
            Some(Nbt::Int(value)) => (*value).max(0),
            _ => 0,
        };
        let experience = Experience {
            progress: match root.get("XpP") {
                Some(Nbt::Float(value)) if (0.0..1.0).contains(value) => *value,
                _ => 0.0,
            },
            level: xp_int("XpLevel"),
            total: xp_int("XpTotal"),
        };
        Ok(Self {
            slots,
            selected,
            root: Arc::new(root),
            mode: None,
            vitals: None,
            experience,
            effects: Default::default(),
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
        if self.experience != Experience::default() {
            root.insert("XpP", self.experience.progress);
            root.insert("XpLevel", self.experience.level);
            root.insert("XpTotal", self.experience.total);
        }
        if let Some(mode) = self.mode {
            root.insert("playerGameType", mode.current.id());
            if let Some(previous) = mode.previous {
                root.insert("previousPlayerGameType", previous.id());
            } else {
                root.remove("previousPlayerGameType");
            }
            if mode.flying || root.get("abilities").is_some() {
                let mut abilities = match root.remove("abilities") {
                    Some(Nbt::Compound(abilities)) => abilities,
                    _ => Compound::new(),
                };
                abilities.insert("flying", Nbt::Byte(i8::from(mode.flying)));
                root.insert("abilities", abilities);
            }
        }
        if let Some(vitals) = self.vitals {
            root.insert("Health", Nbt::Float(vitals.health));
            root.insert("Fire", Nbt::Short(vitals.fire_ticks));
            root.insert("AbsorptionAmount", Nbt::Float(vitals.absorption));
            save_absorption_modifier(
                &mut root,
                self.effects[EffectKind::Absorption as usize].as_ref(),
            );
            root.insert("foodLevel", Nbt::Int(vitals.food));
            root.insert("foodSaturationLevel", Nbt::Float(vitals.saturation));
            root.insert("foodExhaustionLevel", Nbt::Float(vitals.exhaustion));
            root.insert("foodTickTimer", Nbt::Int(vitals.food_timer));
            let mut effects = active_effects(&root).to_vec();
            for (kind, active) in EffectKind::ALL.into_iter().zip(&self.effects) {
                effects.retain(|tag| !is_effect(tag, kind));
                if let Some(effect) = active {
                    let mut tag = effect.write();
                    tag.insert("id", kind.id());
                    effects.push(tag);
                }
            }
            if effects.is_empty() {
                root.remove("active_effects");
            } else {
                root.insert("active_effects", Nbt::List(List::Compound(effects)));
            }
        }
        Ok(root)
    }

    fn snapshot(&self, player: &PlayerState) -> Self {
        Self {
            slots: player.inventory.slots.clone(),
            selected: player.inventory.selected,
            root: Arc::clone(&self.root),
            mode: Some(SavedGameMode {
                current: player.player.profile.game_mode,
                previous: player.previous_game_mode,
                flying: player.flying,
            }),
            vitals: Some(player.vitals),
            experience: player.experience,
            effects: player.effects.clone(),
        }
    }

    fn restore_game_mode(&self, player: &mut PlayerState) {
        let read_mode = |key| match self.root.get(key) {
            Some(Nbt::Int(id)) => GameMode::from_id(*id),
            _ => None,
        };
        let (current, previous, flying) = self.mode.map_or_else(
            || {
                let flying = matches!(self.root.get("abilities"), Some(Nbt::Compound(abilities))
                    if matches!(abilities.get("flying"), Some(Nbt::Byte(value)) if *value != 0));
                (
                    read_mode("playerGameType"),
                    read_mode("previousPlayerGameType"),
                    flying,
                )
            },
            |mode| (Some(mode.current), mode.previous, mode.flying),
        );
        if player.force_game_mode_on_login != Some(true)
            && let Some(current) = current
        {
            player.player.profile.game_mode = current;
        }
        player.previous_game_mode = previous;
        player.flying = match player.player.profile.game_mode {
            GameMode::Spectator => true,
            GameMode::Creative => flying,
            _ => false,
        };
    }

    fn restore_vitals(&self, player: &mut PlayerState) {
        if let Some(vitals) = self.vitals {
            player.vitals = vitals;
            player.effects.clone_from(&self.effects);
            return;
        }
        if let Some(Nbt::Float(health)) = self.root.get("Health")
            && health.is_finite()
        {
            player.vitals.health = health.clamp(0.0, 20.0);
        }
        if let Some(Nbt::Short(fire)) = self.root.get("Fire") {
            player.vitals.fire_ticks = (*fire).max(0);
        }
        if let Some(Nbt::Float(absorption)) = self.root.get("AbsorptionAmount")
            && absorption.is_finite()
        {
            player.vitals.absorption = absorption.max(0.0);
        }
        if let Some(Nbt::Int(food)) = self.root.get("foodLevel") {
            player.vitals.food = (*food).clamp(0, 20);
        }
        if let Some(Nbt::Float(saturation)) = self.root.get("foodSaturationLevel")
            && saturation.is_finite()
        {
            player.vitals.saturation = *saturation;
        }
        if let Some(Nbt::Float(exhaustion)) = self.root.get("foodExhaustionLevel")
            && exhaustion.is_finite()
        {
            player.vitals.exhaustion = *exhaustion;
        }
        if let Some(Nbt::Int(timer)) = self.root.get("foodTickTimer") {
            player.vitals.food_timer = *timer;
        }
        let effects = active_effects(&self.root);
        player.effects = EffectKind::ALL.map(|kind| {
            effects
                .iter()
                .find(|tag| is_effect(tag, kind))
                .and_then(ActiveEffect::read)
        });
    }
}

fn save_absorption_modifier(root: &mut Compound, effect: Option<&ActiveEffect>) {
    const ATTRIBUTE: &str = "minecraft:max_absorption";
    const MODIFIER: &str = "minecraft:effect.absorption";
    let mut attributes = match root.get("attributes") {
        Some(Nbt::List(List::Compound(attributes))) => attributes.clone(),
        None | Some(Nbt::List(List::Empty)) if effect.is_some() => Vec::new(),
        _ => return,
    };
    let has_id =
        |tag: &Compound, name: &str| matches!(tag.get("id"), Some(Nbt::String(id)) if id == name);
    let index = match attributes.iter().position(|tag| has_id(tag, ATTRIBUTE)) {
        Some(index) => index,
        None if effect.is_some() => {
            attributes.push(Compound::from_iter([
                ("id", Nbt::from(ATTRIBUTE)),
                ("base", Nbt::Double(0.0)),
            ]));
            attributes.len() - 1
        }
        None => return,
    };
    let attribute = &mut attributes[index];
    let mut modifiers = match attribute.get("modifiers") {
        Some(Nbt::List(List::Compound(modifiers))) => modifiers.clone(),
        None | Some(Nbt::List(List::Empty)) if effect.is_some() => Vec::new(),
        _ => return,
    };
    modifiers.retain(|tag| !has_id(tag, MODIFIER));
    if let Some(effect) = effect {
        modifiers.push(Compound::from_iter([
            ("id", Nbt::from(MODIFIER)),
            ("amount", Nbt::Double(f64::from(effect.absorption_limit()))),
            ("operation", Nbt::from("add_value")),
        ]));
    }
    if modifiers.is_empty() {
        attribute.remove("modifiers");
    } else {
        attribute.insert("modifiers", Nbt::List(List::Compound(modifiers)));
    }
    root.insert("attributes", Nbt::List(List::Compound(attributes)));
}

fn active_effects(root: &Compound) -> &[Compound] {
    match root.get("active_effects") {
        Some(Nbt::List(List::Compound(effects))) => effects,
        _ => &[],
    }
}

fn is_effect(tag: &Compound, kind: EffectKind) -> bool {
    matches!(tag.get("id"), Some(Nbt::String(id)) if id == kind.id())
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

struct FailedSave {
    uuid: u128,
    data: Arc<PlayerInventory>,
    error: io::Error,
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
                Ok(completion) => self
                    .finish(completion, &mut loaded)
                    .map_err(|save| save.error)?,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err(io::Error::other("player data worker stopped"));
                }
            }
        }
        self.queue_saves(&HashSet::new())?;
        Ok(loaded)
    }

    fn finish(
        &mut self,
        completion: Completed,
        loaded: &mut Vec<Loaded>,
    ) -> Result<(), FailedSave> {
        self.jobs -= 1;
        match completion {
            Completed::Loaded(result) => loaded.push(result),
            Completed::Saved { uuid, data, result } => {
                self.writing.remove(&uuid);
                result.map_err(|error| FailedSave {
                    uuid,
                    data: Arc::clone(&data),
                    error: io::Error::new(error.kind(), format!("saving player data: {error}")),
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

    fn queue_saves(&mut self, failed: &HashSet<u128>) -> io::Result<()> {
        for (&uuid, data) in &self.pending {
            if self.writing.contains(&uuid) || failed.contains(&uuid) {
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
        let mut failed = HashSet::new();
        let mut error = None;
        loop {
            self.queue_saves(&failed)?;
            if self.jobs == 0 {
                return error.map_or(Ok(()), Err);
            }
            let completion = self
                .completed
                .recv()
                .map_err(|_| io::Error::other("player data worker stopped"))?;
            if let Err(save) = self.finish(completion, &mut Vec::new()) {
                // A failed older write must not prevent the latest snapshot from being attempted.
                if self
                    .pending
                    .get(&save.uuid)
                    .is_some_and(|latest| Arc::ptr_eq(latest, &save.data))
                {
                    failed.insert(save.uuid);
                }
                error.get_or_insert(save.error);
            }
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
        data.restore_game_mode(player);
        data.restore_vitals(player);
        player.experience = data.experience;
        player.inventory.slots = data.slots.clone();
        player.inventory.selected = data.selected;
        player.data_ready = true;
        player.saved_inventory = Some(data);
    }

    pub(crate) fn poll_player_data(&mut self) -> bool {
        let mut joined = false;
        let result = self
            .world
            .as_mut()
            .and_then(|world| world.player_store.as_mut())
            .map(PlayerStore::poll);
        match result {
            Some(Ok(loaded)) => {
                for loaded in loaded {
                    let Some(index) = self.players.iter().position(|p| {
                        p.player.entity_id == loaded.entity_id
                            && p.player.profile.uuid == loaded.uuid
                            && !p.data_ready
                            && !p.disconnecting
                    }) else {
                        continue;
                    };
                    match loaded.result {
                        Ok(data) => {
                            self.finish_join(index, Some(data));
                            joined = true;
                        }
                        Err(error) => {
                            let player = &mut self.players[index];
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
        joined
    }

    pub(crate) fn save_player_inventory(&mut self, index: usize) {
        let player = &self.players[index];
        if let Some(data) = &player.saved_inventory
            && let Some(store) = self
                .world
                .as_mut()
                .and_then(|world| world.player_store.as_mut())
        {
            store.save(player.player.profile.uuid, data.snapshot(player));
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
    fn shutdown_saves_other_players_and_newer_snapshots_after_a_write_fails() {
        let writes = Arc::new(Mutex::new(HashSet::new()));
        let saved = Arc::clone(&writes);
        let mut store = PlayerStore::start_with_io(
            Arc::new(|_| panic!("no reads expected")),
            Arc::new(move |uuid, root| {
                let items = PlayerInventory::read(root.clone(), &ItemCodec::default())?;
                let count = items.slots[36].count;
                if (uuid, count) == (1, 1) {
                    return Err(io::Error::other("old snapshot failed"));
                }
                saved.lock().unwrap().insert((uuid, count));
                Ok(())
            }),
        )
        .unwrap();
        store.save(1, data(1));
        store.poll().unwrap();
        store.save(1, data(2));
        for uuid in 2..=CAPACITY as u128 + 1 {
            store.save(uuid, data(1));
        }
        assert!(
            store
                .flush()
                .unwrap_err()
                .to_string()
                .contains("old snapshot failed")
        );
        let writes = writes.lock().unwrap();
        assert!(writes.contains(&(1, 2)));
        assert_eq!(writes.len(), CAPACITY + 1);
        assert!(store.pending.is_empty());
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
        let mut root = new_player(1);
        let hunger = Compound::from_iter([
            ("id", Nbt::from(EffectKind::Hunger.id())),
            ("duration", Nbt::Int(15)),
        ]);
        let absorption = Compound::from_iter([
            ("id", Nbt::from(EffectKind::Absorption.id())),
            ("duration", Nbt::Int(30)),
        ]);
        let other = Compound::from_iter([
            ("id", Nbt::from("minecraft:glowing")),
            ("duration", Nbt::Int(100)),
        ]);
        root.insert(
            "active_effects",
            Nbt::List(List::Compound(vec![hunger, absorption, other.clone()])),
        );
        root.insert("AbsorptionAmount", Nbt::Float(2.0));
        let document = Arc::new(PlayerInventory::read(root, &ItemCodec::default()).unwrap());
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
        player.vitals.health = 16.0;
        player.vitals.exhaustion = 3.5;
        player.vitals.food_timer = 9;
        let mut game = crate::Game {
            pending: Vec::new(),
            players: vec![player],
            online_players: Arc::new(AtomicU32::new(1)),
            world: Some(world),
            viewers: Vec::new(),
            stop: None,
        };
        game.autosave();
        game.players[0].inventory.slots[36].count = 15;
        game.players[0].vitals.health = 9.0;
        game.players[0].vitals.absorption = 0.0;
        game.players[0].vitals.exhaustion = 0.0;
        game.players[0].vitals.food_timer = 0;
        game.players[0].effects[EffectKind::Hunger as usize] = None;
        game.world
            .as_mut()
            .unwrap()
            .player_store
            .as_mut()
            .unwrap()
            .flush()
            .unwrap();
        let snapshot = writes.lock().unwrap()[0].clone();
        assert_eq!(snapshot.get("Health"), Some(&Nbt::Float(16.0)));
        assert_eq!(snapshot.get("AbsorptionAmount"), Some(&Nbt::Float(2.0)));
        let Some(Nbt::List(List::Compound(attributes))) = snapshot.get("attributes") else {
            panic!("missing absorption attribute");
        };
        assert_eq!(
            attributes[0].get("id"),
            Some(&Nbt::from("minecraft:max_absorption"))
        );
        assert_eq!(
            attributes[0].get("modifiers"),
            Some(&Nbt::List(List::Compound(vec![Compound::from_iter([
                ("id", Nbt::from("minecraft:effect.absorption")),
                ("amount", Nbt::Double(4.0)),
                ("operation", Nbt::from("add_value")),
            ]),])))
        );
        assert_eq!(active_effects(&snapshot)[0], other);
        let inventory = PlayerInventory::read(snapshot, &ItemCodec::default()).unwrap();
        inventory.restore_vitals(&mut game.players[0]);
        assert_eq!(game.players[0].vitals.absorption, 2.0);
        assert_eq!(game.players[0].vitals.exhaustion, 3.5);
        assert_eq!(game.players[0].vitals.food_timer, 9);
        assert_eq!(
            game.players[0].effects[EffectKind::Hunger as usize]
                .as_ref()
                .unwrap()
                .write()
                .get("duration"),
            Some(&Nbt::Int(15)),
        );
        assert_eq!(inventory.slots[36].count, 16);
        assert_eq!(inventory.selected, 4);
        assert!(inventory.slots[1].is_empty());
        assert_eq!(game.players[0].inventory.slots[1].count, 3);
        assert_eq!(game.players[0].inventory.carried.count, 2);
        drop(game);
        std::fs::remove_dir_all(folder).unwrap();
    }
}
