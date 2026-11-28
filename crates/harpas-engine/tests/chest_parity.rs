//! Opens vanilla chests through the live engine, then has vanilla read the block entities from Harpas's save.

#[allow(dead_code)]
#[path = "../../harpas-worldgen/tests/reference/mod.rs"]
mod reference;

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use harpas_anvil::{Chunk, ChunkSection, DATA_VERSION, MapFiles, Region, dimension_dir};
use harpas_core::block::block_states;
use harpas_core::block_behavior::Face;
use harpas_core::chunk::{PalettedContainer, Section, Strategy};
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_engine::block::{BlockPos, DigAction};
use harpas_engine::inventory::ItemStack;
use harpas_engine::item_data::ItemCodec;
use harpas_engine::loot::LootTables;
use harpas_engine::player::{GameMode, Input, Movement, Output, Profile};
use harpas_engine::world::{ChunkPos, ChunkSource, ChunkStorage, Generated};
use harpas_engine::{GameData, Handle, PlayerSession};
use harpas_engine::{MapData, MapIconKind};
use harpas_worldgen::{StructureTemplates, WorldgenData};
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

fn join_and_load(engine: &Engine, positions: &[BlockPos], game_mode: GameMode) -> PlayerSession {
    let id = engine.0.next_entity_id().unwrap();
    let session = engine
        .0
        .player_joined(
            id,
            Profile {
                uuid: id as u128,
                name: "ChestViewer".into(),
                properties: Vec::new(),
                game_mode,
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

fn check_map_file(world: &Path, expected: Option<&Compound>) {
    let mut actual = MapFiles::new(world).read(0).unwrap();
    let mut expected = expected.cloned();
    let actual_colors = actual.as_mut().and_then(|data| data.remove("colors"));
    let expected_colors = expected.as_mut().and_then(|data| data.remove("colors"));
    assert_eq!(actual, expected, "saved map settings");
    match (actual_colors, expected_colors) {
        (Some(Nbt::ByteArray(actual)), Some(Nbt::ByteArray(expected))) => {
            assert_eq!(actual.len(), expected.len(), "map image size");
            for (pixel, (actual, expected)) in actual.iter().zip(expected).enumerate() {
                assert_eq!(
                    *actual,
                    expected,
                    "map pixel ({}, {})",
                    pixel % 128,
                    pixel / 128
                );
            }
        }
        (None, None) => {}
        _ => panic!("map image is missing or invalid"),
    }
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
            ("structures", Nbt::Compound(Compound::new())),
            (
                "block_entities",
                Nbt::List(List::Compound(vec![tag.clone()])),
            ),
        ]),
    };
    let mut region = Region::open(region_dir, cx >> 5, 0).unwrap();
    region.set_chunk(cx, 0, &chunk.to_nbt(BIOMES)).unwrap();
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

const LOOT_SEEDS: [i64; 8] = [0, 1, 2, 3, 17, -1, 123456789, -7046029254386353131];

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn dungeon_chest_loot_matches_vanilla_on_open_and_break() {
    check_chest_loot("simple_dungeon", &LOOT_SEEDS, &["minecraft:enchanted_book"]);
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn desert_pyramid_chest_loot_matches_vanilla_on_open_and_break() {
    check_chest_loot(
        "desert_pyramid",
        &LOOT_SEEDS,
        &[
            "minecraft:enchanted_book",
            "minecraft:dune_armor_trim_smithing_template",
        ],
    );
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn buried_treasure_chest_loot_matches_vanilla_on_open_and_break() {
    check_chest_loot(
        "buried_treasure",
        &LOOT_SEEDS,
        &["minecraft:heart_of_the_sea", "minecraft:potion"],
    );
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn shipwreck_treasure_chest_loot_matches_vanilla_on_open_and_break() {
    check_chest_loot(
        "shipwreck_treasure",
        &[0, 1, 2, 3, 6, -1, 123456789, -7046029254386353131],
        &[
            "minecraft:coast_armor_trim_smithing_template",
            "minecraft:iron_nautilus_armor",
        ],
    );
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn shipwreck_supply_chest_loot_matches_vanilla_on_open_and_break() {
    // Covers all six stew effects and all four leather armor pieces, including Thorns on boots.
    check_chest_loot(
        "shipwreck_supply",
        &[0, 1, 2, 3, 4, 9, 16, -1],
        &[
            "minecraft:suspicious_stew",
            "minecraft:leather_helmet",
            "minecraft:leather_chestplate",
            "minecraft:leather_leggings",
            "minecraft:leather_boots",
        ],
    );
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn shipwreck_map_chest_matches_vanilla_and_sends_the_image_after_restart() {
    check_chest_loot("shipwreck_map", &[0, -1], &["minecraft:filled_map"]);
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn shipwreck_map_chest_keeps_the_named_empty_map_when_there_is_no_target() {
    check_chest_loot("shipwreck_map_empty", &[0, -1], &["minecraft:map"]);
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn small_ocean_ruin_chest_loot_matches_vanilla_on_open_and_break() {
    // A fishing rod comes from the map's pool, so its seeds also check that no map is searched for.
    check_chest_loot(
        "underwater_ruin_small",
        &LOOT_SEEDS,
        &["minecraft:filled_map", "minecraft:fishing_rod"],
    );
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn big_ocean_ruin_chest_loot_matches_vanilla_on_open_and_break() {
    check_chest_loot(
        "underwater_ruin_big",
        &[0, 1, 2, 5, 8, 16, 23, -1],
        &[
            "minecraft:filled_map",
            "minecraft:enchanted_book",
            "minecraft:fishing_rod",
            "minecraft:golden_apple",
            "minecraft:diamond_nautilus_armor",
        ],
    );
}

/// Supply only the real structure and biome query. The test's playable chunk is the chest fixture.
struct MapFixtureSource(Arc<dyn ChunkSource>);

impl ChunkSource for MapFixtureSource {
    fn resumes(&self, _: &str) -> bool {
        false
    }

    fn generate(
        &self,
        pos: ChunkPos,
        _: Option<Compound>,
        _: &ChunkStorage,
    ) -> std::io::Result<Generated> {
        Ok(Generated {
            chunk: Chunk {
                sections: Vec::new(),
                rest: Compound::from_iter([
                    ("DataVersion", Nbt::Int(DATA_VERSION)),
                    ("xPos", Nbt::Int(pos.x)),
                    ("zPos", Nbt::Int(pos.z)),
                    ("Status", Nbt::from("minecraft:full")),
                    ("isLightOn", Nbt::Byte(1)),
                ]),
            },
            changed: false,
        })
    }

    fn treasure_map(
        &self,
        pos: BlockPos,
        storage: &ChunkStorage,
    ) -> std::io::Result<Option<(MapData, [i32; 2])>> {
        self.0.treasure_map(pos, storage)
    }

    fn flush(&self, _: &ChunkStorage) -> std::io::Result<()> {
        Ok(())
    }
}

fn check_chest_loot(table: &str, seeds: &[i64], required_items: &[&str]) {
    let reference = Reference::required();
    reference.compile(&[
        ("ChestProbe.java", include_str!("ChestProbe.java")),
        ("ChestLootProbe.java", include_str!("ChestLootProbe.java")),
    ]);
    let tags = reference.temp.0.join("loot.nbt");
    let components = reference.temp.0.join("components.txt");
    let enchantments = reference.temp.0.join("enchantments.txt");
    let potions = reference.temp.0.join("potions.txt");
    let effects = reference.temp.0.join("effects.txt");
    reference.java(
        "ChestLootProbe",
        [&tags, &components, &enchantments]
            .map(|path| path.display().to_string())
            .into_iter()
            .chain([table.to_owned()])
            .chain([&potions, &effects].map(|path| path.display().to_string()))
            .chain(seeds.iter().map(i64::to_string)),
    );
    let tags = read_tags(&tags);
    let (cases, _) = tags.as_chunks::<3>();
    assert_eq!(tags.len(), seeds.len() * 3);
    let names = fs::read_to_string(components).unwrap();
    let enchantments = fs::read_to_string(enchantments).unwrap();
    let potions = fs::read_to_string(potions).unwrap();
    let effects = fs::read_to_string(effects).unwrap();
    let stew_component = names
        .lines()
        .position(|name| name == "minecraft:suspicious_stew_effects")
        .unwrap() as i32;
    let codec = Arc::new(
        ItemCodec::new(&names.lines().map(str::to_owned).collect::<Vec<_>>())
            .with_enchantments(&enchantments.lines().collect::<Vec<_>>())
            .with_potions(&potions.lines().map(str::to_owned).collect::<Vec<_>>())
            .with_mob_effects(&effects.lines().map(str::to_owned).collect::<Vec<_>>()),
    );
    let loot = Arc::new(LootTables::from_zip_path(&reference.server_jar).unwrap());
    let searches_treasure = matches!(
        table,
        "shipwreck_map" | "underwater_ruin_small" | "underwater_ruin_big"
    );
    let map_source = searches_treasure.then(|| {
        let mut data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
        data.structure_sets
            .retain(|name, _| name == "minecraft:buried_treasures");
        let data = Arc::new(data);
        let templates = Arc::new(StructureTemplates::from_zip_path(&reference.server_jar).unwrap());
        let biomes: Vec<_> = data.biomes.keys().map(String::as_str).collect();
        let source =
            harpas_engine::worldgen::overworld(0, data.clone(), templates.clone(), true, &biomes)
                .unwrap()
                .decorate(0, data.clone(), templates)
                .unwrap();
        (Arc::new(MapFixtureSource(source)), data)
    });
    let start = |world: &Path, data: Option<Arc<LootTables>>| {
        let game = GameData {
            items: Some(codec.clone()),
            loot: data,
            ..GameData::default()
        };
        let (handle, thread) = if let Some((source, registries)) = &map_source {
            let biomes: Vec<_> = registries.biomes.keys().map(String::as_str).collect();
            if game.loot.is_some() {
                harpas_engine::start_with_world_source(world, &biomes, source.clone(), game)
            } else {
                harpas_engine::start_with_world(world, &biomes, game)
            }
        } else {
            harpas_engine::start_with_world(world, BIOMES, game)
        }
        .unwrap();
        Engine(handle, Some(thread))
    };
    let pos = BlockPos { x: 1, y: 65, z: 0 };
    let mut observed = HashSet::new();
    for (&seed, [unopened, expected, removal]) in seeds.iter().zip(cases) {
        let Some(Nbt::List(List::Compound(items))) = expected.get("Items") else {
            panic!("expected loot for seed {seed}");
        };
        let expected_map = match removal.get("MapData") {
            Some(Nbt::Compound(map)) => Some(map),
            None => None,
            _ => panic!("invalid vanilla map data"),
        };
        if table == "shipwreck_map" {
            assert!(expected_map.is_some(), "seed {seed} must find a treasure");
        } else if map_source.is_none() {
            assert!(expected_map.is_none());
        }
        let mut slots = vec![ItemStack::empty(); 27];
        for tag in items {
            let Some(Nbt::Byte(slot)) = tag.get("Slot") else {
                panic!("no slot")
            };
            let stack = codec.read(tag).unwrap();
            observed.extend(stack.name());
            slots[*slot as usize] = stack;
        }
        let Some(Nbt::Compound(stew_bytes)) = removal.get("StewBytes") else {
            panic!("expected stew component bytes for seed {seed}");
        };
        for (slot, value) in stew_bytes.iter() {
            let Nbt::ByteArray(bytes) = value else {
                panic!("invalid stew component at slot {slot}");
            };
            let stack = &slots[slot.parse::<usize>().unwrap()];
            let (_, actual) = stack
                .components
                .added
                .iter()
                .find(|(id, _)| *id == stew_component)
                .unwrap();
            let expected: Vec<_> = bytes.iter().map(|&byte| byte as u8).collect();
            assert_eq!(*actual, expected, "seed {seed}, slot {slot}");
        }
        let world = reference.temp.0.join(format!("open-{seed}"));
        let region = dimension_dir(&world, "minecraft:overworld").join("region");
        fs::create_dir_all(&region).unwrap();
        save_chest_fixture(&region, pos, "single", unopened);

        // Reopen with no loot data after shutdown. Only the saved items can supply this menu.
        for data in [Some(loot.clone()), None] {
            let engine = start(&world, data);
            let mode = if expected_map.is_some() {
                GameMode::Creative
            } else {
                GameMode::Survival
            };
            let session = join_and_load(&engine, &[pos], mode);
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
            if let Some(map) = expected_map {
                check_map_file(&world, Some(map));
                let stack = slots
                    .iter()
                    .find(|stack| stack.name() == Some("minecraft:filled_map"))
                    .unwrap();
                session.send(Input::CloseContainer(1)).unwrap();
                session
                    .send(Input::SetCreativeSlot {
                        slot: 36,
                        stack: stack.clone(),
                    })
                    .unwrap();
                let Output::MapUpdate {
                    id: 0,
                    scale: 1,
                    locked: false,
                    icons,
                    colors: Some(colors),
                } = receive(&session, |output| {
                    matches!(output, Output::MapUpdate { .. })
                })
                else {
                    panic!("expected first map update with its full image");
                };
                let Some(Nbt::ByteArray(expected_colors)) = map.get("colors") else {
                    unreachable!()
                };
                assert!(
                    colors
                        .iter()
                        .copied()
                        .eq(expected_colors.iter().map(|&color| color as u8))
                );
                assert_eq!(icons.len(), 2);
                assert_eq!(icons[1].kind, MapIconKind::RedX);
                assert_eq!(icons[1].direction, 8);
            } else if searches_treasure || table == "shipwreck_map_empty" {
                // A chest that does not roll the map must not search or take a map ID.
                check_map_file(&world, None);
            }
            drop(engine);
            assert_eq!(
                &saved_chest(&region, pos),
                expected,
                "saved chest for seed {seed}"
            );
        }

        let Some(Nbt::List(List::Compound(drops))) = removal.get("Drops") else {
            panic!("expected drops for seed {seed}");
        };
        let mut drops: Vec<_> = drops.iter().map(|tag| codec.read(tag).unwrap()).collect();
        let mode = if seed == 0 {
            drops.push(
                codec
                    .read(&Compound::from_iter([
                        ("id", Nbt::from("minecraft:chest")),
                        ("count", Nbt::Int(1)),
                    ]))
                    .unwrap(),
            );
            GameMode::Survival
        } else {
            GameMode::Creative
        };
        let world = reference.temp.0.join(format!("break-{seed}"));
        let region = dimension_dir(&world, "minecraft:overworld").join("region");
        fs::create_dir_all(&region).unwrap();
        save_chest_fixture(&region, pos, "single", unopened);
        let engine = start(&world, Some(loot.clone()));
        let session = join_and_load(&engine, &[pos], mode);
        if mode == GameMode::Survival {
            session
                .send(Input::Movement(Movement {
                    position: None,
                    rotation: None,
                    on_ground: true,
                }))
                .unwrap();
        }
        for (sequence, action) in [DigAction::Start, DigAction::Finish]
            .into_iter()
            .enumerate()
        {
            session
                .send(Input::Dig {
                    action,
                    position: pos,
                    sequence: sequence as i32 + 1,
                })
                .unwrap();
        }
        receive(
            &session,
            |output| matches!(output, Output::SectionBlocks(blocks) if blocks.iter().any(|&(p, state)| p == pos && block_states().is_air(state))),
        );
        for expected in drops {
            let Output::SpawnItem { stack, .. } = receive(&session, |output| {
                matches!(output, Output::SpawnItem { .. })
            }) else {
                unreachable!()
            };
            assert_eq!(stack, expected, "dropped stack for seed {seed}");
        }
        drop(engine);
        check_map_file(&world, expected_map);
        assert!(
            !std::iter::from_fn(|| session.try_recv())
                .any(|output| matches!(output, Output::SpawnItem { .. }))
        );
        let saved = Region::open(&region, 0, 0)
            .unwrap()
            .chunk(0, 0)
            .unwrap()
            .unwrap();
        assert!(matches!(
            saved.get("block_entities"),
            None | Some(Nbt::List(List::Empty))
        ));
    }
    for item in required_items {
        assert!(observed.contains(item), "sample must include {item}");
    }
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
    let session = join_and_load(&engine, &positions, GameMode::Survival);
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
    Nbt::List(List::Compound(saved))
        .write_network(&mut bytes)
        .unwrap();
    fs::write(&harpas_path, bytes).unwrap();
    reference.java(
        "ChestProbe",
        [&harpas_path, &reopened_path].map(|path| path.display().to_string()),
    );
    assert_eq!(read_tags(&reopened_path), expected);
}
