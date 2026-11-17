//! Player inventory persistence through the public engine API, including rapid reconnects, shutdown, failed
//! loads and backup recovery. No vanilla server is needed for these lifecycle regressions.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use harpas_anvil::{DATA_VERSION, read_player, write_player};
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_engine::inventory::ItemStack;
use harpas_engine::player::{GameMode, Input, Output, Profile};
use harpas_engine::{GameData, Handle, PlayerSession};

struct Folder(PathBuf);
impl Folder {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "harpas-player-inventory-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Folder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Engine {
    handle: Handle,
    thread: Option<std::thread::JoinHandle<std::io::Result<()>>>,
}
impl Engine {
    fn start(folder: &Folder, data: GameData) -> Self {
        let (handle, thread) =
            harpas_engine::start_with_world(&folder.0, &["minecraft:plains"], data).unwrap();
        Self {
            handle,
            thread: Some(thread),
        }
    }
    fn join(&self) -> PlayerSession {
        self.handle
            .player_joined(
                self.handle.next_entity_id().unwrap(),
                Profile {
                    uuid: 1,
                    name: "Crafter".to_owned(),
                    properties: Vec::new(),
                    game_mode: GameMode::Creative,
                },
                2,
                true,
            )
            .unwrap()
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.handle.shutdown("Test finished");
        self.thread.take().unwrap().join().unwrap().unwrap();
    }
}

fn wait(session: &PlayerSession, mut matches: impl FnMut(&Output) -> bool) -> Output {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(output) = session.try_recv() {
            if matches(&output) {
                return output;
            }
            if let Output::Disconnect(reason) = output {
                panic!("unexpected disconnect: {reason}");
            }
        }
        assert!(Instant::now() < deadline, "player output timed out");
        std::thread::park_timeout(Duration::from_millis(1));
    }
}
fn inventory(session: &PlayerSession) -> Vec<ItemStack> {
    let Output::InventoryContent {
        container_id: 0,
        slots,
        ..
    } = wait(session, |o| {
        matches!(
            o,
            Output::InventoryContent {
                container_id: 0,
                ..
            }
        )
    })
    else {
        unreachable!()
    };
    slots
}
fn refresh(session: &PlayerSession) -> Vec<ItemStack> {
    session
        .send(Input::ClickContainer {
            container_id: 0,
            state_id: -1,
            slot: -1,
            button: 0,
            mode: 0,
            changed: Vec::new(),
            carried: None,
        })
        .unwrap();
    inventory(session)
}
fn item(name: &str, count: i32) -> Compound {
    Compound::from_iter([("id", Nbt::from(name)), ("count", Nbt::Int(count))])
}
fn root() -> Compound {
    let mut hotbar = item("minecraft:stone", 16);
    hotbar.insert("Slot", Nbt::Byte(0));
    let mut main = item("minecraft:oak_planks", 12);
    main.insert("Slot", Nbt::Byte(9));
    Compound::from_iter([
        ("DataVersion", Nbt::Int(DATA_VERSION)),
        ("SelectedItemSlot", Nbt::Int(7)),
        ("Inventory", Nbt::List(List::Compound(vec![hotbar, main]))),
        (
            "equipment",
            Nbt::Compound(Compound::from_iter([
                ("head", Nbt::Compound(item("minecraft:iron_helmet", 1))),
                ("offhand", Nbt::Compound(item("minecraft:shield", 1))),
            ])),
        ),
        ("Health", Nbt::Float(13.0)),
        ("XpTotal", Nbt::Int(1237)),
        (
            "future_player_data",
            Nbt::Compound(Compound::from_iter([("keep", Nbt::Long(42))])),
        ),
    ])
}

#[test]
fn inventory_and_selected_slot_survive_reconnect_and_restart_without_touching_other_player_data() {
    let folder = Folder::new();
    let original = root();
    write_player(&folder.0, 1, &original).unwrap();
    let engine = Engine::start(&folder, GameData::default());
    let session = engine.join();
    let slots = inventory(&session);
    assert_eq!(slots[36].count, 16);
    assert_eq!(slots[9].count, 12);
    assert_eq!(slots[5].name(), Some("minecraft:iron_helmet"));
    assert_eq!(slots[45].name(), Some("minecraft:shield"));
    assert_eq!(
        wait(&session, |o| matches!(o, Output::SelectedHotbarSlot(_))),
        Output::SelectedHotbarSlot(7)
    );
    session
        .send(Input::ClickContainer {
            container_id: 0,
            state_id: -1,
            slot: 10,
            button: 0,
            mode: 2,
            changed: Vec::new(),
            carried: None,
        })
        .unwrap();
    let changed = inventory(&session);
    assert!(changed[36].is_empty());
    assert_eq!(changed[10].count, 16);
    session.send(Input::SelectHotbarSlot(3)).unwrap();
    refresh(&session);
    drop(session);
    // The new join follows departure immediately; it must get the newest snapshot even if the write is pending.
    let session = engine.join();
    assert_eq!(inventory(&session), changed);
    assert_eq!(
        wait(&session, |o| matches!(o, Output::SelectedHotbarSlot(_))),
        Output::SelectedHotbarSlot(3)
    );
    drop(engine); // An online player's shutdown snapshot must reach disk before the logic thread exits.
    drop(session);
    let saved = read_player(&folder.0, 1).unwrap().unwrap().0;
    for field in ["Health", "XpTotal", "future_player_data"] {
        assert_eq!(saved.get(field), original.get(field));
    }
    let engine = Engine::start(&folder, GameData::default());
    let session = engine.join();
    assert_eq!(inventory(&session), changed);
    assert_eq!(
        wait(&session, |o| matches!(o, Output::SelectedHotbarSlot(_))),
        Output::SelectedHotbarSlot(3)
    );
}

#[test]
fn an_unreadable_or_unsupported_inventory_is_refused_and_never_overwritten() {
    let folder = Folder::new();
    let mut original = root();
    let mut unsupported = item("minecraft:stone", 1);
    unsupported.insert("Slot", Nbt::Byte(0));
    unsupported.insert(
        "components",
        Compound::from_iter([("minecraft:future_component", Nbt::Int(99))]),
    );
    original.insert("Inventory", Nbt::List(List::Compound(vec![unsupported])));
    write_player(&folder.0, 1, &original).unwrap();
    let engine = Engine::start(
        &folder,
        GameData {
            items: Some(Arc::new(harpas_engine::item_data::ItemCodec::new(&[
                "minecraft:future_component".to_owned(),
            ]))),
            ..Default::default()
        },
    );
    let session = engine.join();
    assert_eq!(
        wait(&session, |o| matches!(o, Output::Disconnect(_))),
        Output::Disconnect("Cannot load player data")
    );
    drop(session);
    drop(engine);
    assert_eq!(read_player(&folder.0, 1).unwrap().unwrap().0, original);
}

#[test]
fn a_damaged_current_file_recovers_the_backup_instead_of_creating_an_empty_inventory() {
    let folder = Folder::new();
    let original = root();
    write_player(&folder.0, 1, &original).unwrap();
    write_player(&folder.0, 1, &original).unwrap();
    fs::write(
        folder
            .0
            .join("players/data/00000000-0000-0000-0000-000000000001.dat"),
        b"damaged gzip",
    )
    .unwrap();
    let engine = Engine::start(&folder, GameData::default());
    let session = engine.join();
    assert_eq!(inventory(&session)[36].count, 16);
    drop(session);
    drop(engine);
    assert_eq!(
        read_player(&folder.0, 1)
            .unwrap()
            .unwrap()
            .0
            .get("Inventory"),
        original.get("Inventory")
    );
}

#[test]
fn unsupported_creative_components_are_refused_before_changing_the_inventory() {
    let folder = Folder::new();
    let original = root();
    write_player(&folder.0, 1, &original).unwrap();
    let engine = Engine::start(
        &folder,
        GameData {
            items: Some(Arc::new(harpas_engine::item_data::ItemCodec::new(&[
                "minecraft:future_component".to_owned(),
            ]))),
            ..Default::default()
        },
    );
    let session = engine.join();
    inventory(&session);
    session
        .send(Input::SetCreativeSlot {
            slot: 36,
            stack: ItemStack {
                item: 1,
                count: 99,
                components: Arc::new(harpas_engine::inventory::ComponentPatch {
                    added: vec![(0, vec![1])],
                    ..Default::default()
                }),
            },
        })
        .unwrap();
    assert_eq!(
        wait(&session, |o| matches!(o, Output::Disconnect(_))),
        Output::Disconnect("Unsupported item data component")
    );
    drop(session);
    drop(engine);
    assert_eq!(
        read_player(&folder.0, 1)
            .unwrap()
            .unwrap()
            .0
            .get("Inventory"),
        original.get("Inventory")
    );
}

#[test]
fn shutdown_closes_the_inventory_menu_before_taking_its_final_snapshot() {
    let folder = Folder::new();
    write_player(&folder.0, 1, &root()).unwrap();
    let engine = Engine::start(&folder, GameData::default());
    let session = engine.join();
    let before = inventory(&session);
    session
        .send(Input::ClickContainer {
            container_id: 0,
            state_id: -1,
            slot: 36,
            button: 0,
            mode: 0,
            changed: Vec::new(),
            carried: None,
        })
        .unwrap();
    let while_carried = inventory(&session);
    assert!(while_carried[36].is_empty());
    drop(engine);
    drop(session);
    let engine = Engine::start(&folder, GameData::default());
    let session = engine.join();
    assert_eq!(inventory(&session), before);
}
