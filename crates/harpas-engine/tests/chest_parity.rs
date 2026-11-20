//! Opens vanilla chests through the live engine, then has vanilla read the block entities from Harpas's save.

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
use harpas_engine::inventory::ItemStack;
use harpas_engine::item_data::ItemCodec;
use harpas_engine::loot::LootTables;
use harpas_engine::player::{GameMode, Input, Output, Profile};
use harpas_engine::{GameData, Handle, PlayerSession};
use reference::Reference;

const BIOMES: &[&str] = &["minecraft:plains"];

fn read_tags(path: &Path) -> Vec<Compound> {
    let bytes = fs::read(path).unwrap();
    let mut bytes = bytes.as_slice();
    let Nbt::List(List::Compound(tags)) = Nbt::read_network(&mut bytes).unwrap() else {
        panic!("expected a list of compounds");
    };
    assert!(bytes.is_empty());
    tags
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

fn join_and_load(engine: &Engine, positions: &[BlockPos]) -> PlayerSession {
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
    let mut pending_chunks: Vec<_> = positions.iter().map(|pos| pos.chunk()).collect();
    while !pending_chunks.is_empty() {
        let Output::ChunkBatch(batch) =
            receive(&session, |output| matches!(output, Output::ChunkBatch(_)))
        else {
            unreachable!()
        };
        pending_chunks.retain(|pos| {
            !batch
                .iter()
                .any(|chunk| chunk.position == *pos && chunk.chunk.is_some())
        });
        session.send(Input::ChunkBatchReceived(8.0)).unwrap();
    }
    session
}

fn use_chest(session: &PlayerSession, position: BlockPos, sequence: i32) {
    session
        .send(Input::UseItemOn {
            off_hand: false,
            position,
            face: Face::Up,
            cursor: [0.5; 3],
            sequence,
        })
        .unwrap();
}

fn saved_chest(region_dir: &Path, pos: BlockPos) -> Compound {
    let cx = pos.chunk().x;
    let chunk = Region::open(region_dir, cx >> 5, 0)
        .unwrap()
        .chunk(cx, 0)
        .unwrap()
        .unwrap();
    let Some(Nbt::List(List::Compound(entities))) = chunk.get("block_entities") else {
        panic!("chest missing from the saved chunk");
    };
    assert_eq!(entities.len(), 1);
    entities[0].clone()
}

fn save_chest_fixture(region_dir: &Path, position: BlockPos, kind: &str, tag: &Compound) {
    let states = block_states();
    let mut section = Section::new(
        PalettedContainer::filled(
            Strategy::block_states(states.global_bits()),
            states.default_state("minecraft:air").unwrap(),
        ),
        PalettedContainer::filled(Strategy::biomes(1), 0),
    );
    section.set_block_state(
        (position.x & 15) as usize,
        1,
        0,
        states.state("minecraft:chest", [("type", kind)]).unwrap().0,
    );
    let cx = position.chunk().x;
    let chunk = Chunk {
        sections: vec![ChunkSection {
            y: 4,
            section: Some(section),
            rest: Compound::new(),
        }],
        rest: Compound::from_iter([
            ("DataVersion", Nbt::Int(DATA_VERSION)),
            ("xPos", Nbt::Int(cx)),
            ("zPos", Nbt::Int(0)),
            ("Status", Nbt::from("minecraft:full")),
            ("isLightOn", Nbt::Byte(1)),
            (
                "block_entities",
                Nbt::List(List::Compound(vec![tag.clone()])),
            ),
        ]),
    };
    let mut region = Region::open(region_dir, cx >> 5, 0).unwrap();
    region.set_chunk(cx, 0, &chunk.to_nbt(BIOMES));
    region.save().unwrap();
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn simple_chest_shift_clicks_and_anvil_storage_match_vanilla() {
    check_chest(false);
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn double_chest_order_shift_clicks_and_anvil_storage_match_vanilla() {
    check_chest(true);
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn dungeon_chest_loot_matches_vanilla_and_survives_reload() {
    let reference = Reference::required();
    reference.compile(&[
        ("ChestProbe.java", include_str!("ChestProbe.java")),
        (
            "DungeonChestProbe.java",
            include_str!("DungeonChestProbe.java"),
        ),
    ]);
    let tags = reference.temp.0.join("dungeon.nbt");
    let components = reference.temp.0.join("components.txt");
    let enchantments = reference.temp.0.join("enchantments.txt");
    let seeds = [0_i64, 1, 2, 3, 17, -1, 123456789, -7046029254386353131];
    reference.java(
        "DungeonChestProbe",
        [&tags, &components, &enchantments]
            .map(|path| path.display().to_string())
            .into_iter()
            .chain(seeds.map(|seed| seed.to_string())),
    );
    let tags = read_tags(&tags);
    let (pairs, _) = tags.as_chunks::<2>();
    assert_eq!(tags.len(), seeds.len() * 2);
    let names = fs::read_to_string(components).unwrap();
    let enchantments = fs::read_to_string(enchantments).unwrap();
    let codec = Arc::new(
        ItemCodec::new(&names.lines().map(str::to_owned).collect::<Vec<_>>())
            .with_enchantments(&enchantments.lines().collect::<Vec<_>>()),
    );
    let loot = Arc::new(LootTables::from_zip_path(&reference.server_jar).unwrap());
    let pos = BlockPos { x: 1, y: 65, z: 0 };
    let mut enchanted_books = 0;
    for (seed, [unopened, expected]) in seeds.into_iter().zip(pairs) {
        let Some(Nbt::List(List::Compound(items))) = expected.get("Items") else {
            panic!("expected loot for seed {seed}");
        };
        let mut slots = vec![ItemStack::empty(); 27];
        for tag in items {
            let Some(Nbt::Byte(slot)) = tag.get("Slot") else {
                panic!("no slot")
            };
            let stack = codec.read(tag).unwrap();
            enchanted_books += usize::from(stack.name() == Some("minecraft:enchanted_book"));
            slots[*slot as usize] = stack;
        }
        let world = reference.temp.0.join(format!("dungeon-{seed}"));
        let region = dimension_dir(&world, "minecraft:overworld").join("region");
        fs::create_dir_all(&region).unwrap();
        save_chest_fixture(&region, pos, "single", unopened);

        // Reopen with no loot data after shutdown. Only the saved items can supply this menu.
        for data in [Some(loot.clone()), None] {
            let (handle, thread) = harpas_engine::start_with_world(
                &world,
                BIOMES,
                GameData {
                    items: Some(codec.clone()),
                    loot: data,
                    ..GameData::default()
                },
            )
            .unwrap();
            let engine = Engine(handle, Some(thread));
            let session = join_and_load(&engine, &[pos]);
            use_chest(&session, pos, 1);
            assert_eq!(
                receive(&session, |output| matches!(
                    output,
                    Output::OpenChest { .. }
                )),
                Output::OpenChest {
                    container_id: 1,
                    double: false,
                    title: expected.get("CustomName").cloned()
                },
                "seed {seed}"
            );
            let Output::InventoryContent { slots: actual, .. } = receive(&session, |output| {
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
            assert_eq!(&actual[..27], slots, "menu slots for seed {seed}");
            drop(engine);
            assert_eq!(
                &saved_chest(&region, pos),
                expected,
                "saved chest for seed {seed}"
            );
        }
    }
    assert!(enchanted_books > 0, "sample must include an enchanted book");
}

fn check_chest(double: bool) {
    let reference = Reference::required();
    reference.compile(&[("ChestProbe.java", include_str!("ChestProbe.java"))]);
    let original_path = reference.temp.0.join("original.nbt");
    let expected_path = reference.temp.0.join("expected.nbt");
    let registry_path = reference.temp.0.join("components.txt");
    reference.java(
        "ChestProbe",
        [&original_path, &expected_path, &registry_path]
            .map(|path| path.display().to_string())
            .into_iter()
            .chain([double.to_string()]),
    );
    let originals = read_tags(&original_path);
    let expected = read_tags(&expected_path);
    let registry = fs::read_to_string(registry_path).unwrap();
    let codec = ItemCodec::new(&registry.lines().map(str::to_owned).collect::<Vec<_>>());

    let world = reference.temp.0.join("harpas");
    let region_dir = dimension_dir(&world, "minecraft:overworld").join("region");
    fs::create_dir_all(&region_dir).unwrap();
    let halves: &[(i32, &str)] = if double {
        &[(0, "right"), (-1, "left")]
    } else {
        &[(1, "single")]
    };
    let positions: Vec<_> = halves
        .iter()
        .map(|&(x, _)| BlockPos { x, y: 65, z: 0 })
        .collect();
    for ((&pos, &(_, kind)), tag) in positions.iter().zip(halves).zip(&originals) {
        save_chest_fixture(&region_dir, pos, kind, tag);
    }

    let (handle, thread) = harpas_engine::start_with_world(
        &world,
        BIOMES,
        GameData {
            items: Some(Arc::new(codec)),
            ..GameData::default()
        },
    )
    .unwrap();
    let engine = Engine(handle, Some(thread));
    let session = join_and_load(&engine, &positions);
    let chest_slots = 27 * halves.len();
    let last_chest_slot = chest_slots - 1;
    let last_player_slot = chest_slots + 35;
    let contents = |id| {
        let Output::InventoryContent { slots, carried, .. } = receive(
            &session,
            |output| matches!(output, Output::InventoryContent { container_id, .. } if *container_id == id),
        ) else {
            unreachable!()
        };
        assert_eq!(slots.len(), last_player_slot + 1);
        assert!(carried.is_empty());
        slots
    };

    let mut before = None;
    for (index, &position) in positions.iter().enumerate() {
        let id = index as i32 + 1;
        use_chest(&session, position, id);
        assert_eq!(
            receive(&session, |output| matches!(
                output,
                Output::OpenChest { .. }
            )),
            Output::OpenChest {
                container_id: id,
                double,
                title: originals[0].get("CustomName").cloned()
            }
        );
        let slots = contents(id);
        assert_eq!(slots[last_chest_slot].count, 9);
        assert_eq!(slots[13].name(), Some("minecraft:diamond_pickaxe"));
        if let Some(previous) = before.replace(slots) {
            assert_eq!(before.as_ref(), Some(&previous));
        }
    }

    let id = positions.len() as i32;
    for slot in [last_chest_slot, last_player_slot] {
        session
            .send(Input::ClickContainer {
                container_id: id,
                state_id: -1, // Ask for authoritative contents after each click.
                slot: slot as i16,
                button: 0,
                mode: 1,
                changed: Vec::new(),
                carried: None,
            })
            .unwrap();
        let slots = contents(id);
        assert!(slots[last_chest_slot].is_empty());
        assert_eq!(slots[last_player_slot].is_empty(), slot == last_player_slot);
    }
    drop(engine);

    let saved: Vec<_> = positions
        .iter()
        .map(|&pos| saved_chest(&region_dir, pos))
        .collect();
    assert_eq!(saved, expected);
    let harpas_path = reference.temp.0.join("harpas.nbt");
    let reopened_path = reference.temp.0.join("reopened.nbt");
    let mut bytes = Vec::new();
    Nbt::List(List::Compound(saved)).write_network(&mut bytes);
    fs::write(&harpas_path, bytes).unwrap();
    reference.java(
        "ChestProbe",
        [&harpas_path, &reopened_path].map(|path| path.display().to_string()),
    );
    assert_eq!(read_tags(&reopened_path), expected);
}
