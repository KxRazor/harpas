//! Native recipe matching and CraftingMenu shift-clicks against the live engine's table menu. Preparation uses
//! the engine's player inputs; each comparison covers all 46 slots, the cursor and any dropped remainder.

#[allow(dead_code)]
#[path = "../../harpas-worldgen/tests/reference/mod.rs"]
mod reference;

use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

use harpas_anvil::{Chunk, ChunkSection, DATA_VERSION, Region, dimension_dir};
use harpas_core::block::block_states;
use harpas_core::block_behavior::Face;
use harpas_core::chunk::{PalettedContainer, Section, Strategy};
use harpas_core::item::items;
use harpas_core::nbt::{Compound, Nbt};
use harpas_engine::block::BlockPos;
use harpas_engine::inventory::ItemStack;
use harpas_engine::player::{GameMode, Input, Output, Profile};
use harpas_engine::{GameData, Handle, PlayerSession};
use reference::Reference;
use serde_json::{Value, json};

fn describe(stack: &ItemStack) -> Value {
    if stack.is_empty() {
        json!([0, 0])
    } else {
        json!([stack.item, stack.count])
    }
}

fn stack(value: &Value) -> ItemStack {
    ItemStack {
        item: items().id(value[0].as_str().unwrap()).unwrap(),
        count: value[1].as_i64().unwrap() as i32,
        components: Default::default(),
    }
}

fn contents(session: &PlayerSession, container_id: i32, drops: &mut Vec<Value>) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match session.try_recv() {
            Some(Output::InventoryContent {
                container_id: id,
                slots,
                carried,
                ..
            }) if id == container_id => {
                let mut result: Vec<_> = slots.iter().map(describe).collect();
                result.push(describe(&carried));
                return json!(result);
            }
            Some(Output::SpawnItem { stack, .. }) => drops.push(describe(&stack)),
            Some(Output::ChunkBatch(_)) => session.send(Input::ChunkBatchReceived(8.0)).unwrap(),
            Some(Output::Disconnect(reason)) => panic!("disconnected: {reason}"),
            _ => {}
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for menu {container_id}"
        );
        std::thread::park_timeout(Duration::from_millis(1));
    }
}

fn click(session: &PlayerSession, id: i32, slot: i16, mode: i32, drops: &mut Vec<Value>) -> Value {
    session
        .send(Input::ClickContainer {
            container_id: id,
            state_id: -1,
            slot,
            button: 0,
            mode,
            changed: Vec::new(),
            carried: None,
        })
        .unwrap();
    contents(session, id, drops)
}

struct Engine {
    handle: Handle,
    thread: Option<std::thread::JoinHandle<std::io::Result<()>>>,
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.handle.shutdown("Test finished");
        self.thread.take().unwrap().join().unwrap().unwrap();
    }
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn table_recipes_and_shift_clicks_match_vanilla() {
    let reference = Reference::required();
    reference.compile(&[
        (
            "WorldgenReferenceProbe.java",
            include_str!("../../harpas-worldgen/tests/WorldgenReferenceProbe.java"),
        ),
        (
            "ChunkParityProbe.java",
            include_str!("../../harpas-worldgen/tests/ChunkParityProbe.java"),
        ),
        (
            "PlacementProbe.java",
            include_str!("../../harpas-worldgen/tests/PlacementProbe.java"),
        ),
        (
            "CraftingTableProbe.java",
            include_str!("CraftingTableProbe.java"),
        ),
    ]);
    let plank = json!(["minecraft:oak_planks", 1]);
    let stick = json!(["minecraft:stick", 1]);
    let log = json!(["minecraft:oak_log", 1]);
    let cases = json!([
        {"recipe":"wooden_pickaxe", "grid":[plank,plank,plank,null,stick,null,null,stick,null], "move":0},
        {"recipe":"wooden_axe", "grid":[null,plank,plank,null,stick,plank,null,stick,null], "move":0},
        {"recipe":"crafting_table", "grid":[null,null,null,null,plank,plank,null,plank,plank], "move":0},
        {"recipe":"oak_stairs", "grid":[plank,null,null,plank,plank,null,plank,plank,plank], "move":0},
        {"recipe":"cake", "grid":[["minecraft:milk_bucket",1],["minecraft:milk_bucket",1],["minecraft:milk_bucket",1],
            ["minecraft:sugar",1],["minecraft:egg",1],["minecraft:sugar",1],
            ["minecraft:wheat",1],["minecraft:wheat",1],["minecraft:wheat",1]], "move":0},
        {"recipe":"oak_planks", "grid":[null,null,null,null,null,null,null,null,log], "move":0},
        {"recipe":"oak_planks", "grid":[log,null,null,null,null,null,null,null,null],
            "full":["minecraft:oak_planks",63], "move":0},
        {"recipe":"oak_planks", "grid":[log,null,null,null,null,null,null,null,null], "full":null, "move":0},
        {"recipe":"oak_planks", "grid":[null,null,null,null,null,null,null,null,log], "move":9}
    ]);
    let cases_path = reference.temp.0.join("cases.json");
    fs::write(&cases_path, cases.to_string()).unwrap();
    let output = reference.temp.0.join("crafting.json");
    reference.java(
        "CraftingTableProbe",
        [
            output.display().to_string(),
            reference.server_jar.display().to_string(),
            cases_path.display().to_string(),
        ],
    );
    let expected: Value = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    assert_eq!(
        expected["menu_type"], 12,
        "the Java connection's crafting menu type"
    );

    for (case, test) in cases.as_array().unwrap().iter().enumerate() {
        let world = reference.temp.0.join(format!("harpas-{case}"));
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
        section.set_block_state(
            1,
            1,
            0,
            states.default_state("minecraft:crafting_table").unwrap(),
        );
        let mut rest = Compound::new();
        rest.insert("DataVersion", DATA_VERSION);
        rest.insert("xPos", 0);
        rest.insert("zPos", 0);
        rest.insert("Status", "minecraft:full");
        rest.insert("isLightOn", Nbt::Byte(1));
        let chunk = Chunk {
            sections: vec![ChunkSection {
                y: 4,
                section: Some(section),
                rest: Compound::new(),
            }],
            rest,
        };
        let mut region = Region::open(&region_dir, 0, 0).unwrap();
        region.set_chunk(0, 0, &chunk.to_nbt(biomes)).unwrap();
        region.save().unwrap();
        let recipes = harpas_engine::recipe::Recipes::from_zip_path(&reference.server_jar).unwrap();
        let (handle, thread) = harpas_engine::start_with_world(
            &world,
            biomes,
            GameData {
                recipes: Some(Arc::new(recipes)),
                ..GameData::default()
            },
        )
        .unwrap();
        let engine = Engine {
            handle,
            thread: Some(thread),
        };
        let id = engine.handle.next_entity_id().unwrap();
        let session = engine
            .handle
            .player_joined(
                id,
                Profile {
                    uuid: id as u128,
                    name: format!("Crafter{case}"),
                    properties: Vec::new(),
                    game_mode: GameMode::Creative,
                },
                2,
                true,
            )
            .unwrap();
        let mut drops = Vec::new();
        contents(&session, 0, &mut drops);
        session.send(Input::ConfirmTeleport(1)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(Output::ChunkBatch(_)) = session.try_recv() {
                break;
            }
            assert!(Instant::now() < deadline, "chunk not loaded");
            std::thread::park_timeout(Duration::from_millis(1));
        }
        session
            .send(Input::UseItemOn {
                off_hand: false,
                position: BlockPos { x: 1, y: 65, z: 0 },
                face: Face::Up,
                cursor: [0.5; 3],
                sequence: 1,
            })
            .unwrap();
        contents(&session, 1, &mut drops);
        for (i, value) in test["grid"].as_array().unwrap().iter().enumerate() {
            if value.is_null() {
                continue;
            }
            session
                .send(Input::SetCreativeSlot {
                    slot: 36,
                    stack: stack(value),
                })
                .unwrap();
            click(&session, 1, 37, 0, &mut drops);
            click(&session, 1, i as i16 + 1, 0, &mut drops);
        }
        if let Some(full) = test.get("full") {
            for slot in 9..45 {
                let value = if slot == 44 && !full.is_null() {
                    full.clone()
                } else {
                    json!(["minecraft:stone", 64])
                };
                session
                    .send(Input::SetCreativeSlot {
                        slot,
                        stack: stack(&value),
                    })
                    .unwrap();
            }
        }
        let before = click(&session, 1, -1, 0, &mut drops);
        assert_eq!(
            before, expected["cases"][case]["before"],
            "case {case} before"
        );
        let after = click(
            &session,
            1,
            test["move"].as_i64().unwrap() as i16,
            1,
            &mut drops,
        );
        assert_eq!(after, expected["cases"][case]["after"], "case {case} after");
        // Item tracking runs after the input phase that sent the contents. A second snapshot waits for it.
        click(&session, 1, -1, 0, &mut drops);
        assert_eq!(
            json!(drops),
            expected["cases"][case]["drops"],
            "case {case} drops"
        );
    }
}
