//! Opens a vanilla chest through the live engine, then has vanilla read the block entity from Harpas's save.

#[allow(dead_code)]
#[path = "../../harpas-worldgen/tests/reference/mod.rs"]
mod reference;

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use harpas_anvil::{Chunk, ChunkSection, DATA_VERSION, Region, dimension_dir};
use harpas_core::block::block_states;
use harpas_core::block_behavior::Face;
use harpas_core::chunk::{PalettedContainer, Section, Strategy};
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_engine::block::BlockPos;
use harpas_engine::item_data::ItemCodec;
use harpas_engine::player::{GameMode, Input, Output, Profile};
use harpas_engine::{GameData, Handle, PlayerSession};
use reference::Reference;

fn read_tag(path: &Path) -> Compound {
    let bytes = fs::read(path).unwrap();
    let mut bytes = bytes.as_slice();
    let Nbt::Compound(tag) = Nbt::read_network(&mut bytes).unwrap() else {
        panic!("expected compound");
    };
    assert!(bytes.is_empty());
    tag
}

struct Engine(Handle, Option<JoinHandle<std::io::Result<()>>>);

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.0.shutdown("chest parity complete");
        let result = self.1.take().unwrap().join();
        if !thread::panicking() {
            result.unwrap().unwrap();
        }
    }
}

fn receive(session: &PlayerSession, predicate: impl Fn(&Output) -> bool) -> Output {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(output) = session.try_recv()
            && predicate(&output)
        {
            return output;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the engine"
        );
        thread::park_timeout(Duration::from_millis(1));
    }
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn simple_chest_shift_clicks_and_anvil_storage_match_vanilla() {
    let reference = Reference::required();
    reference.compile(&[(
        "SimpleChestProbe.java",
        include_str!("SimpleChestProbe.java"),
    )]);
    let original_path = reference.temp.0.join("original.nbt");
    let expected_path = reference.temp.0.join("expected.nbt");
    let registry_path = reference.temp.0.join("components.txt");
    reference.java(
        "SimpleChestProbe",
        [&original_path, &expected_path, &registry_path].map(|path| path.display().to_string()),
    );
    let original = read_tag(&original_path);
    let expected = read_tag(&expected_path);
    let registry = fs::read_to_string(registry_path).unwrap();
    let codec = ItemCodec::new(&registry.lines().map(str::to_owned).collect::<Vec<_>>());

    let world = reference.temp.0.join("harpas");
    let biomes = &["minecraft:plains"];
    let region_dir = dimension_dir(&world, "minecraft:overworld").join("region");
    fs::create_dir_all(&region_dir).unwrap();
    let states = block_states();
    let mut section = Section::new(
        PalettedContainer::filled(
            Strategy::block_states(states.global_bits()),
            states.default_state("minecraft:air").unwrap(),
        ),
        PalettedContainer::filled(Strategy::biomes(1), 0),
    );
    section.set_block_state(1, 1, 0, states.default_state("minecraft:chest").unwrap());
    let chunk = Chunk {
        sections: vec![ChunkSection {
            y: 4,
            section: Some(section),
            rest: Compound::new(),
        }],
        rest: Compound::from_iter([
            ("DataVersion", Nbt::Int(DATA_VERSION)),
            ("xPos", Nbt::Int(0)),
            ("zPos", Nbt::Int(0)),
            ("Status", Nbt::from("minecraft:full")),
            ("isLightOn", Nbt::Byte(1)),
            (
                "block_entities",
                Nbt::List(List::Compound(vec![original.clone()])),
            ),
        ]),
    };
    let mut region = Region::open(&region_dir, 0, 0).unwrap();
    region.set_chunk(0, 0, &chunk.to_nbt(biomes));
    region.save().unwrap();
    let (handle, thread) = harpas_engine::start_with_world(
        &world,
        biomes,
        GameData {
            items: Some(Arc::new(codec)),
            ..GameData::default()
        },
    )
    .unwrap();
    let engine = Engine(handle, Some(thread));
    let id = engine.0.next_entity_id().unwrap();
    let session = engine
        .0
        .player_joined(
            id,
            Profile {
                uuid: id as u128,
                name: "ChestViewer".into(),
                properties: Vec::new(),
                game_mode: GameMode::Survival,
            },
            2,
            true,
        )
        .unwrap();
    receive(&session, |output| {
        matches!(
            output,
            Output::InventoryContent {
                container_id: 0,
                ..
            }
        )
    });
    session.send(Input::ConfirmTeleport(1)).unwrap();
    receive(&session, |output| matches!(output, Output::ChunkBatch(_)));
    session
        .send(Input::UseItemOn {
            off_hand: false,
            position: BlockPos { x: 1, y: 65, z: 0 },
            face: Face::Up,
            cursor: [0.5; 3],
            sequence: 1,
        })
        .unwrap();
    assert_eq!(
        receive(&session, |output| matches!(
            output,
            Output::OpenChest { .. }
        )),
        Output::OpenChest {
            container_id: 1,
            title: original.get("CustomName").cloned()
        }
    );
    let contents = || {
        let Output::InventoryContent { slots, carried, .. } = receive(&session, |output| {
            matches!(
                output,
                Output::InventoryContent {
                    container_id: 1,
                    ..
                }
            )
        }) else {
            unreachable!()
        };
        assert_eq!(slots.len(), 63);
        assert!(carried.is_empty());
        slots
    };
    let before = contents();
    assert_eq!(before[26].count, 9);
    assert_eq!(before[13].name(), Some("minecraft:diamond_pickaxe"));
    for slot in [26, 62] {
        session
            .send(Input::ClickContainer {
                container_id: 1,
                state_id: -1, // Ask for authoritative contents after each click.
                slot,
                button: 0,
                mode: 1,
                changed: Vec::new(),
                carried: None,
            })
            .unwrap();
        let slots = contents();
        assert!(slots[26].is_empty());
        assert_eq!(slots[62].is_empty(), slot == 62);
    }
    drop(engine);

    let saved = Region::open(&region_dir, 0, 0)
        .unwrap()
        .chunk(0, 0)
        .unwrap()
        .unwrap();
    let Some(Nbt::List(List::Compound(entities))) = saved.get("block_entities") else {
        panic!("chest missing from the saved chunk");
    };
    assert_eq!(entities, std::slice::from_ref(&expected));
    let harpas_path = reference.temp.0.join("harpas.nbt");
    let reopened_path = reference.temp.0.join("reopened.nbt");
    let mut bytes = Vec::new();
    Nbt::Compound(entities[0].clone()).write_network(&mut bytes);
    fs::write(&harpas_path, bytes).unwrap();
    reference.java(
        "SimpleChestProbe",
        [&harpas_path, &reopened_path].map(|path| path.display().to_string()),
    );
    assert_eq!(read_tag(&reopened_path), expected);
}
