//! Compare Vanilla's normal furnace with a live Harpas menu and an Anvil restart mid-smelt.

#[allow(dead_code)]
#[path = "../../harpas-worldgen/tests/reference/mod.rs"]
mod reference;

use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use harpas_anvil::{Chunk, ChunkSection, DATA_VERSION, Region, dimension_dir, read_chunk};
use harpas_core::block::block_states;
use harpas_core::block_behavior::Face;
use harpas_core::chunk::{PalettedContainer, Section, Strategy};
use harpas_core::item::items;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_engine::block::BlockPos;
use harpas_engine::inventory::ItemStack;
use harpas_engine::item_data::ItemCodec;
use harpas_engine::player::{GameMode, Input, Output, Profile};
use harpas_engine::recipe::Recipes;
use harpas_engine::{GameData, Handle, PlayerSession};
use serde_json::{Value, json};

const BIOMES: &[&str] = &["minecraft:plains"];
const POS: BlockPos = BlockPos { x: 1, y: 65, z: 0 };
const TIMER_KEYS: [&str; 4] = [
    "lit_time_remaining",
    "lit_total_time",
    "cooking_time_spent",
    "cooking_total_time",
];

struct Engine(Handle, Option<JoinHandle<io::Result<()>>>);

impl Engine {
    fn start(world: &Path, recipes: &Arc<Recipes>) -> Self {
        let (handle, thread) = harpas_engine::start_with_world(
            world,
            BIOMES,
            GameData {
                recipes: Some(recipes.clone()),
                ..GameData::default()
            },
        )
        .unwrap();
        Self(handle, Some(thread))
    }

    fn menu(&self) -> PlayerSession {
        let id = self.0.next_entity_id().unwrap();
        let session = self
            .0
            .player_joined(
                id,
                Profile {
                    uuid: 1,
                    name: "Smelter".into(),
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
        receive(&session, |output| {
            matches!(output, Output::ChunkBatch(chunks)
            if chunks.iter().any(|c| c.position == POS.chunk()))
        });
        session
            .send(Input::UseItemOn {
                off_hand: false,
                position: POS,
                face: Face::Up,
                cursor: [0.5; 3],
                sequence: 1,
            })
            .unwrap();
        receive(&session, |output| {
            matches!(
                output,
                Output::OpenFurnace {
                    container_id: 1,
                    ..
                }
            )
        });
        session
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.0.shutdown("Test finished");
        self.1.take().unwrap().join().unwrap().unwrap();
    }
}

fn receive(session: &PlayerSession, mut ready: impl FnMut(&Output) -> bool) -> Output {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(output) = session.try_recv() {
            match &output {
                Output::ChunkBatch(_) => session.send(Input::ChunkBatchReceived(8.0)).unwrap(),
                Output::Disconnect(reason) => panic!("disconnected: {reason}"),
                _ => {}
            }
            if ready(&output) {
                return output;
            }
        } else {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the furnace"
            );
            std::thread::park_timeout(Duration::from_millis(1));
        }
    }
}

#[derive(Default)]
struct Menu {
    slots: [ItemStack; 3],
    properties: [i16; 4],
}

impl Menu {
    fn read(&mut self, output: &Output) {
        match output {
            Output::InventoryContent {
                container_id: 1,
                slots,
                ..
            } => {
                assert_eq!(slots.len(), 39);
                self.slots.clone_from_slice(&slots[..3]);
            }
            Output::SetSlot {
                container_id: 1,
                slot,
                stack,
                ..
            } if (0..3).contains(slot) => {
                self.slots[*slot as usize] = stack.clone();
            }
            Output::ContainerProperty {
                container_id: 1,
                property,
                value,
            } => {
                self.properties[*property as usize] = *value;
            }
            _ => {}
        }
    }

    fn compare(&self, expected: &Value) {
        assert_eq!(json!(self.properties), expected["properties"]);
        assert_eq!(
            json!(self.slots.each_ref().map(|stack| (stack.item, stack.count))),
            expected["slots"]
        );
    }
}

fn seed_world(region_dir: &Path) {
    fs::create_dir_all(region_dir).unwrap();
    let states = block_states();
    let mut section = Section::new(
        PalettedContainer::filled(
            Strategy::block_states(states.global_bits()),
            states.default_state("minecraft:air").unwrap(),
        ),
        PalettedContainer::filled(Strategy::biomes(1), 0),
    );
    section.set_block_state(1, 1, 0, states.default_state("minecraft:furnace").unwrap());
    let furnace = Compound::from_iter([
        ("id", Nbt::from("minecraft:furnace")),
        ("x", POS.x.into()),
        ("y", POS.y.into()),
        ("z", POS.z.into()),
        ("cooking_total_time", 200_i16.into()),
        (
            "Items",
            Nbt::from(List::Compound(
                [
                    ("minecraft:raw_iron", 2),
                    ("minecraft:coal", 1),
                    ("minecraft:iron_ingot", 64),
                ]
                .into_iter()
                .enumerate()
                .map(|(index, (item, count))| {
                    Compound::from_iter([
                        ("id", Nbt::from(item)),
                        ("Slot", (index as i8).into()),
                        ("count", count.into()),
                    ])
                })
                .collect(),
            )),
        ),
    ]);
    let chunk = Chunk {
        sections: vec![ChunkSection {
            y: 4,
            section: Some(section),
            rest: Compound::new(),
        }],
        rest: Compound::from_iter([
            ("DataVersion", DATA_VERSION.into()),
            ("xPos", 0.into()),
            ("zPos", 0.into()),
            ("Status", Nbt::from("minecraft:full")),
            ("isLightOn", 1_i8.into()),
            ("block_entities", Nbt::from(List::Compound(vec![furnace]))),
        ]),
    };
    let mut region = Region::open(region_dir, 0, 0).unwrap();
    region.set_chunk(0, 0, &chunk.to_nbt(BIOMES)).unwrap();
    region.save().unwrap();
}

fn saved_furnace(region: &Path) -> Menu {
    let chunk = read_chunk(region, 0, 0).unwrap().unwrap();
    let Some(Nbt::List(List::Compound(entities))) = chunk.get("block_entities") else {
        panic!("missing furnace tag")
    };
    let furnace = &entities[0];
    let Some(Nbt::List(List::Compound(items))) = furnace.get("Items") else {
        panic!("missing furnace items")
    };
    let mut menu = Menu::default();
    let codec = ItemCodec::default();
    for stack in items {
        let Some(Nbt::Byte(slot)) = stack.get("Slot") else {
            panic!("missing furnace slot")
        };
        menu.slots[*slot as usize] = codec.read(stack).unwrap();
    }
    menu.properties = TIMER_KEYS.map(|key| {
        let Some(Nbt::Short(value)) = furnace.get(key) else {
            panic!("missing furnace timer {key}")
        };
        *value
    });
    menu
}

fn compare(pack: &Path, world: &Path, observed: &Value) {
    assert_eq!(observed["menu"], 14);
    let recipes = Arc::new(Recipes::from_zip_path(pack).unwrap());
    for item in 1..items().len() {
        let name = items().name(item).unwrap();
        let stack = ItemStack {
            item,
            count: 1,
            ..ItemStack::default()
        };
        assert_eq!(
            i64::from(recipes.burn_duration(&stack)),
            observed["fuels"][name].as_i64().unwrap_or(0),
            "fuel {name}"
        );
    }
    let region = dimension_dir(world, "minecraft:overworld").join("region");
    seed_world(&region);
    let cooking = observed["cooking"].as_array().unwrap();
    {
        let engine = Engine::start(world, &recipes);
        let session = engine.menu();
        let mut menu = Menu::default();
        receive(&session, |output| {
            menu.read(output);
            matches!(
                output,
                Output::ContainerProperty {
                    container_id: 1,
                    property: 3,
                    ..
                }
            )
        });
        menu.compare(&observed["blocked"]);
        session
            .send(Input::ClickContainer {
                container_id: 1,
                state_id: -1,
                slot: 2,
                button: 0,
                mode: 1,
                changed: vec![],
                carried: None,
            })
            .unwrap();
        for tick in [2, 80] {
            receive(&session, |output| {
                menu.read(output);
                matches!(output, Output::ContainerProperty { container_id: 1, property: 2, value } if usize::try_from(*value) == Ok(tick))
            });
            menu.compare(&cooking[tick]);
        }
    }
    let saved = saved_furnace(&region);
    let progress = saved.properties[2] as usize;
    assert!((80..200).contains(&progress));
    saved.compare(&observed["saved_cooking"][progress]);
    {
        let engine = Engine::start(world, &recipes);
        let session = engine.menu();
        let mut menu = Menu::default();
        receive(&session, |output| {
            menu.read(output);
            matches!(output, Output::SetSlot { container_id: 1, slot: 2, stack, .. } if !stack.is_empty())
        });
        menu.compare(&cooking[200]);
    }
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn normal_furnace_matches_vanilla() {
    let reference = reference::Reference::required();
    reference.compile(&[("FurnaceProbe.java", include_str!("FurnaceProbe.java"))]);
    let report = reference.temp.0.join("furnace.json");
    reference.java("FurnaceProbe", [report.display().to_string()]);
    let observed: Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
    compare(
        &reference.server_jar,
        &reference.temp.0.join("harpas"),
        &observed,
    );
}
