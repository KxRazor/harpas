//! Compare saved adult creatures, combat and loot with Minecraft 26.1.
mod gameplay;
mod reference;

use gameplay::*;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_java::{offline_uuid, write_frame};
use harpas_protocol_java::v26_1::play;
use play::clientbound::Packet;
use reference::{Folder, Reference, Server};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::ops::Range;
use std::path::Path;
use std::time::{Duration, Instant};

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn saved_adult_pig_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("saved-pig");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [vanilla_port, harpas_port] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    writeln!(
        reference.child.stdin.as_mut().unwrap(),
        "forceload add 0 0\nsave-all flush"
    )
    .unwrap();
    reference.wait_for("Saved the game");
    writeln!(
        reference.child.stdin.as_mut().unwrap(),
        "summon minecraft:pig 5.5 -58 4.5 {{UUID:[I;1,2,3,4],Health:7.5f,\
         Rotation:[90.0f,-22.5f],variant:\"minecraft:cold\",NoAI:1b}}\n\
         summon minecraft:item 8.5 -60 4.5 {{Item:{{id:\"minecraft:stone\",count:1}},PickupDelay:32767s}}\n\
         save-all flush"
    )
    .unwrap();
    reference.wait_for("Saved the game");
    let entities = "world/dimensions/minecraft/overworld/entities";
    let original = harpas_anvil::read_chunk(&vanilla.join(entities), 0, 0)
        .unwrap()
        .unwrap();
    let expected = observe_saved_pig(vanilla_port, "Vanilla's original world");
    assert_eq!(expected.0.uuid, 0x0000_0001_0000_0002_0000_0003_0000_0004);
    assert_eq!(
        expected.1.entries,
        [
            (9, play::MetadataValue::Float(7.5)),
            (15, play::MetadataValue::Byte(1)),
            (19, play::MetadataValue::PigVariant(0)),
        ],
    );
    stop(&mut reference);
    let (rx, rz) = harpas_anvil::region_of(0, 0);
    harpas_anvil::write_chunks(&harpas.join(entities), rx, rz, &[((0, 0), &original)]).unwrap();
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    assert_eq!(observe_saved_pig(harpas_port, "Harpas"), expected);
    stop(&mut server);

    let saved = harpas_anvil::read_chunk(&harpas.join(entities), 0, 0)
        .unwrap()
        .unwrap();
    let Some(Nbt::Short(original_age)) = saved_entity(&original, "minecraft:item").get("Age")
    else {
        panic!("expected the vanilla item's age");
    };
    assert!(
        matches!(saved_entity(&saved, "minecraft:item").get("Age"), Some(Nbt::Short(age)) if age > original_age),
        "the item should have ticked and been saved"
    );
    assert_eq!(
        saved_entity(&saved, "minecraft:pig"),
        saved_entity(&original, "minecraft:pig")
    );
    harpas_anvil::write_chunks(&vanilla.join(entities), rx, rz, &[((0, 0), &saved)]).unwrap();
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    assert_eq!(
        observe_saved_pig(vanilla_port, "Vanilla after loading Harpas's save"),
        expected
    );
    stop(&mut reference);
}

fn saved_entity<'a>(root: &'a Compound, id: &str) -> &'a Compound {
    let Some(Nbt::List(List::Compound(entities))) = root.get("Entities") else {
        panic!("expected saved entities");
    };
    entities
        .iter()
        .find(|entity| matches!(entity.get("id"), Some(Nbt::String(saved)) if saved == id))
        .unwrap_or_else(|| panic!("expected saved {id}"))
}

fn observe_saved_pig(
    port: u16,
    source: &str,
) -> (play::clientbound::SpawnEntity, play::EntityMetadata) {
    let mut client = Client::join(port, "PigViewer");
    let mut spawn = track_animals(
        &mut client,
        100,
        &format!("saved adult pig in {source}"),
        1,
        |_| true,
    )
    .pop()
    .unwrap();
    let metadata = client.matching("pig metadata", |packet| match packet {
        Packet::SetEntityMetadata(packet) if packet.entity_id == spawn.entity_id => {
            Some(packet.metadata)
        }
        _ => None,
    });
    spawn.entity_id = 0;
    (spawn, metadata)
}

fn track_saved_pig(client: &mut Client) -> play::clientbound::SpawnEntity {
    track_animals(client, 100, "saved adult pig", 1, |_| true)
        .pop()
        .unwrap()
}

fn track_animals(
    client: &mut Client,
    entity_type: i32,
    expected: &str,
    count: usize,
    belongs: impl Fn(&play::clientbound::SpawnEntity) -> bool,
) -> Vec<play::clientbound::SpawnEntity> {
    track_animals_after_join(client, entity_type, expected, count, belongs, || {})
}

fn track_animals_after_join(
    client: &mut Client,
    entity_type: i32,
    expected: &str,
    count: usize,
    belongs: impl Fn(&play::clientbound::SpawnEntity) -> bool,
    on_ready: impl FnOnce(),
) -> Vec<play::clientbound::SpawnEntity> {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut chunks = HashSet::new();
    let mut on_ready = Some(on_ready);
    let mut pending_position = None;
    let mut animals = Vec::with_capacity(count);
    while Instant::now() < deadline {
        let frame = client.next(expected);
        match Packet::decode(&frame) {
            Ok(Packet::SynchronizePlayerPosition(position)) => {
                play_packet(
                    client,
                    play::serverbound::ConfirmTeleportation {
                        teleport_id: position.teleport_id,
                    },
                );
                play_packet(client, play::serverbound::PlayerLoaded);
                pending_position = Some([position.x, position.y, position.z]);
            }
            Ok(Packet::KeepAlive(keep_alive)) => {
                play_packet(client, play::serverbound::KeepAlive { id: keep_alive.id })
            }
            Ok(Packet::ChunkBatchFinished(_)) => play_packet(
                client,
                play::serverbound::ChunkBatchReceived {
                    chunks_per_tick: 25.0,
                },
            ),
            Ok(Packet::ChunkDataAndUpdateLight(chunk)) => {
                chunks.insert((chunk.x, chunk.z));
            }
            Ok(Packet::SpawnEntity(spawn))
                if spawn.entity_type == entity_type && belongs(&spawn) =>
            {
                let pos = ((spawn.x.floor() as i32) >> 4, (spawn.z.floor() as i32) >> 4);
                assert!(
                    chunks.contains(&pos),
                    "received the animal before its chunk"
                );
                animals.push(spawn);
                if animals.len() == count {
                    return animals;
                }
            }
            _ => {}
        }
        // Vanilla refreshes entity tracking on player movement, but skips a viewer whose chunk
        // is still queued to send. Wait for the chunk before sending our only idle movement.
        if chunks.contains(&(0, 0))
            && let Some(position) = pending_position.take()
        {
            move_player(client, position, true);
            if let Some(on_ready) = on_ready.take() {
                on_ready();
            }
        }
    }
    panic!(
        "received {} of {count} spawns while waiting for {expected} ({} chunks received)",
        animals.len(),
        chunks.len()
    );
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn natural_cows_spawn_on_grass_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("natural-cows");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [vanilla_port, harpas_port] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    for dir in [&vanilla, &harpas] {
        let path = dir.join("server.properties");
        let properties = fs::read_to_string(&path)
            .unwrap()
            .replace("view-distance=2", "view-distance=7")
            .replace("simulation-distance=2", "simulation-distance=7");
        fs::write(
            &path,
            format!(
                "{properties}generator-settings={{\"biome\":\"minecraft:plains\",\"layers\":[\
             {{\"block\":\"minecraft:bedrock\",\"height\":1}},\
             {{\"block\":\"minecraft:stone\",\"height\":3}}],\"features\":false,\"lakes\":false}}\n"
            ),
        )
        .unwrap();
    }
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    writeln!(
        reference.child.stdin.as_mut().unwrap(),
        "forceload add -112 -96 111 95"
    )
    .unwrap();
    reference.wait_for("force loaded");
    writeln!(reference.child.stdin.as_mut().unwrap(), "save-all flush").unwrap();
    reference.wait_for("Saved the game");
    writeln!(
        reference.child.stdin.as_mut().unwrap(),
        "fill -96 -61 -80 -33 -61 79 minecraft:grass_block\n\
         fill 32 -61 -80 95 -61 79 minecraft:grass_block\n\
         fill -112 -57 -96 -17 -57 95 minecraft:stone\n\
         fill 16 -57 -96 111 -57 95 minecraft:stone\n\
         gamerule minecraft:advance_time false\ngamerule minecraft:advance_weather false\n\
         gamerule minecraft:random_tick_speed 0\n\
         gamerule minecraft:spawn_monsters false\ntime set 6000\nweather clear\n\
         gamerule minecraft:spawn_mobs true"
    )
    .unwrap();
    reference.wait_for("Gamerule spawn_mobs is now set to: true");

    let expected = std::thread::scope(|scope| {
        let (ready, waiting) = std::sync::mpsc::channel();
        let peer = scope.spawn(move || {
            let mut client = Client::join_with_view(vanilla_port, "CowViewer", 7);
            track_animals_after_join(
                &mut client,
                30,
                "naturally spawned vanilla cow",
                1,
                |_| true,
                || ready.send(()).unwrap(),
            )
            .pop()
            .unwrap()
        });
        waiting.recv_timeout(Duration::from_secs(30)).unwrap();
        let command = |server: &mut Server, text: &str| {
            writeln!(server.child.stdin.as_mut().unwrap(), "{text}").unwrap();
        };
        command(&mut reference, "tick sprint 400t");
        reference.wait_for("Sprint completed");
        let assert_no_cows = |server: &mut Server, case| {
            command(server, "execute unless entity @e[type=minecraft:cow]");
            assert_eq!(
                server.wait_for_any(&["Test passed", "Test failed"]),
                0,
                "{case}"
            );
        };
        assert_no_cows(&mut reference, "the covered platforms should be too dark");
        command(&mut reference, "fill -112 -57 -96 -17 -57 95 minecraft:air");
        command(&mut reference, "fill 16 -57 -96 111 -57 95 minecraft:air");
        for _ in 0..10 {
            command(
                &mut reference,
                "summon minecraft:sheep 7 -60 7 {NoAI:1b,PersistenceRequired:0b}",
            );
        }
        command(&mut reference, "tick sprint 400t");
        reference.wait_for("Sprint completed");
        assert_no_cows(&mut reference, "the creature cap should be full");
        command(&mut reference, "kill @e[type=minecraft:sheep]");
        for _ in 0..12 {
            command(&mut reference, "tick sprint 400t");
            reference.wait_for("Sprint completed");
            if peer.is_finished() {
                break;
            }
            // Other species may fill the vanilla cap before cows are selected.
            command(
                &mut reference,
                "kill @e[type=!minecraft:player,type=!minecraft:cow]",
            );
        }
        peer.join().unwrap()
    });
    stop(&mut reference);

    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    let mut client = Client::join_with_view(harpas_port, "CowViewer", 7);
    client
        .stream
        .set_read_timeout(Some(Duration::from_secs(25)))
        .unwrap();
    let actual = track_animals(&mut client, 30, "naturally spawned Harpas cow", 1, |_| true)
        .pop()
        .unwrap();
    stop(&mut server);
    for spawn in [&expected, &actual] {
        assert!(
            ((-96.0..-32.0).contains(&spawn.x) || (32.0..96.0).contains(&spawn.x))
                && (-80.0..80.0).contains(&spawn.z),
            "cow spawned outside the grass platforms: {spawn:?}"
        );
        assert_eq!(spawn.y, -60.0);
    }
    let saved = harpas_anvil::read_entity_chunk(
        &harpas_anvil::dimension_dir(&harpas.join("world"), "minecraft:overworld").join("entities"),
        (actual.x.floor() as i32) >> 4,
        (actual.z.floor() as i32) >> 4,
    )
    .unwrap()
    .unwrap();
    let Some(Nbt::List(List::Compound(entities))) = saved.get("Entities") else {
        panic!("expected saved cows");
    };
    let uuid = Nbt::IntArray(
        actual
            .uuid
            .to_be_bytes()
            .as_chunks::<4>()
            .0
            .iter()
            .copied()
            .map(i32::from_be_bytes)
            .collect(),
    );
    assert!(entities.iter().any(|cow| {
        cow.get("id") == Some(&Nbt::from("minecraft:cow")) && cow.get("UUID") == Some(&uuid)
    }));
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn falling_adult_pig_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("falling-pig");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [vanilla_port, harpas_port] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    writeln!(
        reference.child.stdin.as_mut().unwrap(),
        "forceload add 0 0\n\
         tick freeze\n\
         summon minecraft:pig 3.5 -57 1.5 \
         {{attributes:[{{id:\"minecraft:movement_speed\",base:0d}}]}}\n\
         save-all flush"
    )
    .unwrap();
    reference.wait_for("Saved the game");
    let entities = "world/dimensions/minecraft/overworld/entities";
    let initial = harpas_anvil::read_chunk(&vanilla.join(entities), 0, 0)
        .unwrap()
        .unwrap();
    harpas_anvil::write_chunks(&harpas.join(entities), 0, 0, &[((0, 0), &initial)]).unwrap();
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);

    let mut viewer = Client::join(vanilla_port, "PigViewer");
    let pig = track_saved_pig(&mut viewer);
    assert_eq!(pig.y, -57.0);
    writeln!(reference.child.stdin.as_mut().unwrap(), "tick unfreeze").unwrap();
    let expected = observe_pig_landing(&mut viewer, &pig);
    assert_eq!([expected.x, expected.y, expected.z], [3.5, -60.0, 1.5]);
    let mut viewer = Client::join(harpas_port, "PigViewer");
    let pig = track_saved_pig(&mut viewer);
    assert!(pig.y > -60.0);
    let actual = observe_pig_landing(&mut viewer, &pig);
    assert_eq!(
        [actual.x, actual.y, actual.z],
        [expected.x, expected.y, expected.z]
    );
    assert_eq!(actual.velocity_y, expected.velocity_y);
    stop(&mut server);
    stop(&mut reference);
    for dir in [&vanilla, &harpas] {
        let saved = harpas_anvil::read_chunk(&dir.join(entities), 0, 0)
            .unwrap()
            .unwrap();
        let pig = saved_entity(&saved, "minecraft:pig");
        assert_eq!(
            pig.get("Pos"),
            Some(&List::Double(vec![3.5, -60.0, 1.5]).into())
        );
        assert_eq!(pig.get("OnGround"), Some(&Nbt::Byte(1)));
    }
}

fn observe_pig_landing(
    client: &mut Client,
    pig: &play::clientbound::SpawnEntity,
) -> play::clientbound::EntityPositionSync {
    let mut fell = false;
    let landing = client.matching("pig landing", |packet| match packet {
        Packet::EntityPositionSync(sync) if sync.entity_id == pig.entity_id && sync.y < pig.y => {
            fell |= !sync.on_ground;
            sync.on_ground.then_some(sync)
        }
        Packet::UpdateEntityPosition(movement)
            if movement.entity_id == pig.entity_id && !movement.on_ground =>
        {
            fell |= movement.delta_y < 0;
            None
        }
        Packet::UpdateEntityPositionAndRotation(movement)
            if movement.entity_id == pig.entity_id && !movement.on_ground =>
        {
            fell |= movement.delta_y < 0;
            None
        }
        _ => None,
    });
    assert!(
        fell,
        "the viewer should see the pig falling before it lands"
    );
    landing
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn bare_hand_pig_combat_matches_vanilla() {
    saved_animal_combat("minecraft:pig", "Pig", 100, "minecraft:porkchop");
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn saved_cow_combat_and_drops_match_vanilla() {
    saved_animal_combat("minecraft:cow", "Cow", 30, "minecraft:beef");
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn saved_cow_follows_wheat_matches_vanilla() {
    let [
        ((vanilla_switch, vanilla_speed), vanilla_pos),
        ((harpas_switch, harpas_speed), harpas_pos),
    ] = observe_saved_cow_following(CowCourse::Flat, lead_cow);
    // The position and velocity packets can sample different ticks of the approach.
    assert!(vanilla_speed > 0.06);
    assert!(
        (harpas_speed - vanilla_speed).abs() < 0.01,
        "cow's westward velocity: Harpas {harpas_speed}, vanilla {vanilla_speed}"
    );
    for (server, switched_at, pos) in [
        ("vanilla", vanilla_switch, vanilla_pos),
        ("Harpas", harpas_switch, harpas_pos),
    ] {
        // Allow a few ticks' movement in flight and some coasting, not continued pursuit.
        assert!(
            (-0.25..=1.0).contains(&(switched_at - pos[0])),
            "cow did not stop in {server}: switched at {switched_at}, saved at {pos:?}"
        );
    }
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn saved_cow_walks_around_a_wall_matches_vanilla() {
    let [vanilla, harpas] = observe_saved_cow_following(CowCourse::Wall, lead_cow_around_wall);
    for (server, (at, saved)) in [("vanilla", vanilla), ("Harpas", harpas)] {
        assert_eq!(saved[1], HERDER_POSITION[1]);
        assert!(
            saved[0] < 5.5,
            "cow returned behind the wall in {server}: {saved:?}"
        );
        assert!(
            (saved[0] - at[0]).hypot(saved[2] - at[2]) < 0.9,
            "cow did not stop in {server}: switched at {at:?}, saved at {saved:?}"
        );
    }
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn saved_cow_jumps_up_a_step_matches_vanilla() {
    let [vanilla, harpas] = observe_saved_cow_following(CowCourse::StepUp, lead_cow_up_step);
    for (server, (at, saved)) in [("vanilla", vanilla), ("Harpas", harpas)] {
        assert_eq!(saved[1], UPPER_HERDER_POSITION[1]);
        assert!(
            saved[0] < 5.8,
            "cow left the platform in {server}: {saved:?}"
        );
        assert!(
            (saved[0] - at[0]).hypot(saved[2] - at[2]) < 1.0,
            "cow did not stop in {server}: switched at {at:?}, saved at {saved:?}"
        );
    }
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn saved_cow_walks_down_a_step_matches_vanilla() {
    compare_cow_descent(1, 5.4..6.55);
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn saved_cow_walks_down_larger_steps_matches_vanilla() {
    for height in [2, 3] {
        compare_cow_descent(height, 4.9..6.55);
    }
}

fn compare_cow_descent(height: i32, landing_x: Range<f64>) {
    let [vanilla, harpas] =
        observe_saved_cow_following(CowCourse::StepDown(height), |port, reference| {
            lead_cow_down_step(port, reference, height, &landing_x)
        });
    for (server, (at, saved)) in [("vanilla", vanilla), ("Harpas", harpas)] {
        assert_eq!(saved[1], HERDER_POSITION[1]);
        assert!(
            landing_x.contains(&saved[0]),
            "cow was not stopped below the {height}-block ledge in {server}: {saved:?}"
        );
        assert!(
            (saved[0] - at[0]).hypot(saved[2] - at[2]) < 1.0,
            "cow did not stop in {server}: switched at {at:?}, saved at {saved:?}"
        );
    }
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn saved_cow_crosses_a_dry_trench_matches_vanilla() {
    let [vanilla, harpas] = observe_saved_cow_following(CowCourse::Trench, lead_cow_across_trench);
    for (server, (at, saved)) in [("vanilla", vanilla), ("Harpas", harpas)] {
        assert_eq!(saved[1], UPPER_HERDER_POSITION[1]);
        assert!(
            (0.0..6.0).contains(&saved[0]) && (0.0..5.0).contains(&saved[2]),
            "cow did not stay on the far bank in {server}: {saved:?}"
        );
        assert!(
            (saved[0] - at[0]).hypot(saved[2] - at[2]) < 1.0,
            "cow did not stop in {server}: switched at {at:?}, saved at {saved:?}"
        );
    }
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn saved_cow_swims_across_a_shallow_trench_matches_vanilla() {
    for dry_detour in [true, false] {
        let [vanilla, harpas] = observe_saved_cow_following(
            CowCourse::WaterTrench { dry_detour },
            lead_cow_through_water,
        );
        for (server, (at, saved)) in [("vanilla", vanilla), ("Harpas", harpas)] {
            assert_eq!(saved[1], UPPER_HERDER_POSITION[1]);
            assert!(
                (0.0..6.0).contains(&saved[0]) && (0.0..5.0).contains(&saved[2]),
                "cow did not stay on the far bank in {server} (dry detour {dry_detour}): {saved:?}"
            );
            assert!(
                (saved[0] - at[0]).hypot(saved[2] - at[2]) < 1.0,
                "cow did not stop in {server}: switched at {at:?}, saved at {saved:?}"
            );
        }
    }
}

const HERDER_POSITION: [f64; 3] = [2.5, -60.0, 1.5];
const UPPER_HERDER_POSITION: [f64; 3] = [1.5, -59.0, 1.5];

enum CowCourse {
    Flat,
    Wall,
    StepUp,
    StepDown(i32),
    Trench,
    WaterTrench { dry_detour: bool },
}

fn observe_saved_cow_following<T>(
    course: CowCourse,
    lead: impl Fn(u16, Option<&mut Server>) -> T,
) -> [(T, [f64; 3]); 2] {
    let Reference { jar, eula } = Reference::required();
    let check_reload = matches!(
        course,
        CowCourse::StepDown(_) | CowCourse::Trench | CowCourse::WaterTrench { .. }
    );
    let (name, north_edge, terrain, cow_y) = match course {
        CowCourse::Flat => ("cow-temptation", 0, String::new(), -60),
        // It is much shorter to go around the south end than the north end.
        CowCourse::Wall => (
            "cow-navigation",
            -4,
            "fill 6 -60 -4 6 -59 2 minecraft:stone\n".into(),
            -60,
        ),
        CowCourse::StepUp => (
            "cow-step",
            0,
            "fill 0 -60 0 6 -60 4 minecraft:stone\n".into(),
            -60,
        ),
        CowCourse::StepDown(height) => (
            "cow-step-descent",
            0,
            format!("fill 7 -60 0 15 {} 4 minecraft:stone\n", -61 + height),
            -60 + height,
        ),
        CowCourse::Trench => (
            "cow-uneven-terrain",
            0,
            "fill 0 -60 0 5 -60 4 minecraft:stone\nfill 9 -60 0 15 -60 4 minecraft:stone\n".into(),
            -59,
        ),
        CowCourse::WaterTrench { dry_detour } => (
            "cow-water-trench",
            -1,
            format!(
                "fill 0 -60 0 5 -60 4 minecraft:stone\n\
                 fill 9 -60 0 15 -60 4 minecraft:stone\n\
                 fill 0 -60 -1 15 -57 -1 minecraft:stone\n\
                 fill 0 -60 5 15 -57 5 minecraft:stone\n\
                 fill 6 -60 0 8 -60 4 minecraft:water\n{}",
                if dry_detour {
                    "fill 6 -60 4 8 -60 4 minecraft:stone\n"
                } else {
                    ""
                }
            ),
            -59,
        ),
    };
    let folder = Folder::new(name);
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [vanilla_port, harpas_port] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    let console = reference.child.stdin.as_mut().unwrap();
    writeln!(
        console,
        "forceload add 0 {north_edge} 0 0\ntick freeze\n{terrain}\
         summon minecraft:cow 10.5 {cow_y} 1.5\nsave-all flush"
    )
    .unwrap();
    reference.wait_for("Saved the game");
    let entities = "world/dimensions/minecraft/overworld/entities";
    let read_entities = |dir: &Path| {
        harpas_anvil::read_chunk(&dir.join(entities), 0, 0)
            .unwrap()
            .unwrap()
    };
    let initial = read_entities(&vanilla);
    harpas_anvil::write_chunks(&harpas.join(entities), 0, 0, &[((0, 0), &initial)]).unwrap();
    let player = Compound::from_iter([
        ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
        (
            "Inventory",
            List::Compound(vec![Compound::from_iter([
                ("Slot", Nbt::Byte(1)),
                ("id", Nbt::from("minecraft:wheat")),
                ("count", Nbt::Int(1)),
            ])])
            .into(),
        ),
    ]);
    for dir in [&vanilla, &harpas] {
        harpas_anvil::write_player(&dir.join("world"), offline_uuid("CowHerder"), &player).unwrap();
    }
    let expected = lead(vanilla_port, Some(&mut reference));
    if check_reload {
        // Leave the cow's chunk unloaded at startup so it cannot wander before we freeze ticks.
        writeln!(
            reference.child.stdin.as_mut().unwrap(),
            "tick freeze\nforceload remove all"
        )
        .unwrap();
    }
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    let actual = lead(harpas_port, None);
    stop(&mut server);
    [
        (&vanilla, vanilla_port, expected),
        (&harpas, harpas_port, actual),
    ]
    .map(|(dir, port, observed)| {
        let saved = read_entities(dir);
        let cow = saved_entity(&saved, "minecraft:cow");
        let Some(Nbt::List(List::Double(pos))) = cow.get("Pos") else {
            panic!("cow has no saved position in {dir:?}");
        };
        let pos: [f64; 3] = pos.as_slice().try_into().unwrap();
        if check_reload {
            assert_eq!(cow.get("OnGround"), Some(&Nbt::Byte(1)));
            let mut server = if dir == &vanilla {
                let mut reference = start_flat_vanilla(&jar, &vanilla, false);
                writeln!(
                    reference.child.stdin.as_mut().unwrap(),
                    "tick freeze\nforceload add 0 0\nsave-all flush"
                )
                .unwrap();
                reference.wait_for("Saved the game");
                reference
            } else {
                let mut server =
                    Server::start(OsStr::new(env!("CARGO_BIN_EXE_harpas")), &[], &harpas);
                server.wait_for("Done (");
                server
            };
            let mut client = Client::join(port, "CowHerder");
            let cows = track_animals(&mut client, 30, "reloaded adult cow", 1, |_| true);
            let reloaded = &cows[0];
            assert_eq!(reloaded.y, pos[1]);
            assert!(
                (reloaded.x - pos[0]).hypot(reloaded.z - pos[2]) < 0.25,
                "cow reloaded away from {pos:?} in {dir:?}: {reloaded:?}"
            );
            stop(&mut server);
        }
        (observed, pos)
    })
}

fn hold_wheat(
    port: u16,
    reference: Option<&mut Server>,
    herder: [f64; 3],
) -> (Client, play::clientbound::SpawnEntity) {
    let mut client = Client::join(port, "CowHerder");
    client.stream.set_nodelay(true).unwrap();
    let cow = track_animals(&mut client, 30, "saved adult cow", 1, |_| true)
        .pop()
        .unwrap();
    move_player(&mut client, herder, true);
    play_packet(&mut client, play::serverbound::SetHeldItem { slot: 1 });
    if let Some(reference) = reference {
        writeln!(reference.child.stdin.as_mut().unwrap(), "tick unfreeze").unwrap();
    }
    (client, cow)
}

fn lead_cow(port: u16, reference: Option<&mut Server>) -> (f64, f64) {
    let (mut client, cow) = hold_wheat(port, reference, HERDER_POSITION);
    let mut x = cow.x;
    let mut head_yaw = cow.head_yaw;
    let mut westward_speed = 0.0_f64;
    client.matching("cow approaches the wheat", |packet| {
        match packet {
            Packet::EntityPositionSync(sync) if sync.entity_id == cow.entity_id => x = sync.x,
            Packet::UpdateEntityPosition(movement) if movement.entity_id == cow.entity_id => {
                x += f64::from(movement.delta_x) / 4096.0;
            }
            Packet::UpdateEntityPositionAndRotation(movement)
                if movement.entity_id == cow.entity_id =>
            {
                x += f64::from(movement.delta_x) / 4096.0;
            }
            Packet::SetHeadRotation(rotation) if rotation.entity_id == cow.entity_id => {
                head_yaw = rotation.head_yaw;
            }
            Packet::SetEntityVelocity(motion) if motion.entity_id == cow.entity_id => {
                westward_speed = westward_speed.max(-motion.velocity.x);
            }
            _ => return None,
        }
        // West is 90 degrees, encoded as 64 in the protocol.
        (x < cow.x - 1.25 && (48..=80).contains(&head_yaw)).then_some(())
    });
    play_packet(&mut client, play::serverbound::SetHeldItem { slot: 0 });
    std::thread::sleep(Duration::from_millis(650));
    (x, westward_speed)
}

fn lead_cow_around_wall(port: u16, reference: Option<&mut Server>) -> [f64; 3] {
    let (mut client, cow) = hold_wheat(port, reference, HERDER_POSITION);
    let mut rounded_end = false;
    follow_cow_until(&mut client, &cow, "cow rounds the wall", |[x, y, z]| {
        assert!(
            (y - cow.y).abs() < 0.001,
            "cow left the ground at {x}, {y}, {z}"
        );
        if (5.56..7.44).contains(&x) {
            assert!(z >= 3.44, "cow crossed the wall at {x}, {y}, {z}");
            rounded_end = true;
        }
        rounded_end && x < 5.4 && (x - HERDER_POSITION[0]).hypot(z - HERDER_POSITION[2]) > 3.0
    })
}

fn lead_cow_up_step(port: u16, reference: Option<&mut Server>) -> [f64; 3] {
    let (mut client, cow) = hold_wheat(port, reference, UPPER_HERDER_POSITION);
    let mut jumped = false;
    follow_cow_until(
        &mut client,
        &cow,
        "cow jumps onto the platform",
        |[x, y, z]| {
            jumped |= y > UPPER_HERDER_POSITION[1] + 0.05;
            jumped
                && (y - UPPER_HERDER_POSITION[1]).abs() < 0.001
                && (4.6..5.8).contains(&x)
                && (0.5..4.5).contains(&z)
        },
    )
}

fn lead_cow_down_step(
    port: u16,
    reference: Option<&mut Server>,
    height: i32,
    landing_x: &Range<f64>,
) -> [f64; 3] {
    let server = if reference.is_some() {
        "vanilla"
    } else {
        "Harpas"
    };
    let (mut client, cow) = hold_wheat(port, reference, HERDER_POSITION);
    let floor = HERDER_POSITION[1];
    assert_eq!(cow.y, floor + f64::from(height));
    let mut fell = false;
    follow_cow_until(
        &mut client,
        &cow,
        &format!("cow walks down the {height}-block ledge in {server}"),
        |[x, y, z]| {
            assert!(y <= cow.y + 0.001, "cow jumped at {x}, {y}, {z}");
            fell |= (floor + 0.01..cow.y - 0.01).contains(&y);
            fell && (y - floor).abs() < 0.001 && landing_x.contains(&x)
        },
    )
}

fn lead_cow_across_trench(port: u16, reference: Option<&mut Server>) -> [f64; 3] {
    let (mut client, cow) = hold_wheat(port, reference, UPPER_HERDER_POSITION);
    let bank = UPPER_HERDER_POSITION[1];
    assert_eq!(cow.y, bank);
    let mut reached_bottom = false;
    let mut jumped = false;
    follow_cow_until(
        &mut client,
        &cow,
        "cow descends into the trench and jumps onto the far bank",
        |[x, y, z]| {
            reached_bottom |= (y - (bank - 1.0)).abs() < 0.001 && (6.44..8.56).contains(&x);
            jumped |= reached_bottom && y > bank + 0.05;
            jumped && (y - bank).abs() < 0.001 && (4.6..5.3).contains(&x) && (0.5..4.5).contains(&z)
        },
    )
}

fn lead_cow_through_water(port: u16, reference: Option<&mut Server>) -> [f64; 3] {
    let server = if reference.is_some() {
        "vanilla"
    } else {
        "Harpas"
    };
    let (mut client, cow) = hold_wheat(port, reference, UPPER_HERDER_POSITION);
    let bank = UPPER_HERDER_POSITION[1];
    assert_eq!(cow.y, bank);
    let mut swam = false;
    let at = follow_cow_until(
        &mut client,
        &cow,
        &format!("cow swims across the trench and reaches the far bank in {server}"),
        |[x, y, z]| {
            // Feet above the bottom and below the water's surface, away from either bank.
            swam |= (7.0..8.0).contains(&x)
                && (bank - 0.9..bank - 0.15).contains(&y)
                && (0.5..3.5).contains(&z);
            // Let the whole cow reach the bank before it can resume its idle movement.
            (y - bank).abs() < 0.001 && (4.6..5.1).contains(&x)
        },
    );
    assert!(swam, "cow did not swim across in {server}: {at:?}");
    at
}

fn follow_cow_until(
    client: &mut Client,
    cow: &play::clientbound::SpawnEntity,
    expectation: &str,
    mut reached: impl FnMut([f64; 3]) -> bool,
) -> [f64; 3] {
    let mut at = [cow.x, cow.y, cow.z];
    client.matching(expectation, |packet| {
        let delta = match packet {
            Packet::EntityPositionSync(pos) if pos.entity_id == cow.entity_id => {
                at = [pos.x, pos.y, pos.z];
                [0; 3]
            }
            Packet::UpdateEntityPosition(pos) if pos.entity_id == cow.entity_id => {
                [pos.delta_x, pos.delta_y, pos.delta_z]
            }
            Packet::UpdateEntityPositionAndRotation(pos) if pos.entity_id == cow.entity_id => {
                [pos.delta_x, pos.delta_y, pos.delta_z]
            }
            _ => return None,
        };
        for (coord, change) in at.iter_mut().zip(delta) {
            *coord += f64::from(change) / 4096.0;
        }
        reached(at).then_some(())
    });
    play_packet(client, play::serverbound::SetHeldItem { slot: 0 });
    std::thread::sleep(Duration::from_millis(650));
    at
}

fn saved_animal_combat(kind: &str, name: &str, entity_type: i32, meat: &str) {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("saved-animal-combat");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [vanilla_port, harpas_port] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    writeln!(
        reference.child.stdin.as_mut().unwrap(),
        "forceload add 0 0\nsummon {kind} 3.5 -60 1.5 {{Health:1.5f,attributes:[{{id:\"minecraft:movement_speed\",base:0d}}]}}"
    )
    .unwrap();
    reference.wait_for(&format!("Summoned new {name}"));
    std::thread::sleep(Duration::from_millis(250));
    writeln!(reference.child.stdin.as_mut().unwrap(), "save-all flush").unwrap();
    reference.wait_for("Saved the game");
    let entities = "world/dimensions/minecraft/overworld/entities";
    let read_entities = |dir: &Path| harpas_anvil::read_chunk(&dir.join(entities), 0, 0).unwrap();
    let initial = read_entities(&vanilla).unwrap();
    harpas_anvil::write_chunks(&harpas.join(entities), 0, 0, &[((0, 0), &initial)]).unwrap();

    let expected = punch_saved_animal(entity_type, vanilla_port, 1.5);
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    let actual = punch_saved_animal(entity_type, harpas_port, 1.5);
    assert_eq!(actual, expected);
    stop(&mut server);
    for dir in [&vanilla, &harpas] {
        let saved = read_entities(dir).unwrap();
        assert_eq!(
            saved_entity(&saved, kind).get("Health"),
            Some(&Nbt::Float(0.5))
        );
    }
    let saved = read_entities(&harpas).unwrap();
    harpas_anvil::write_chunks(&vanilla.join(entities), 0, 0, &[((0, 0), &saved)]).unwrap();
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    punch_saved_animal(entity_type, vanilla_port, 0.5);
    stop(&mut reference);
    let mut server = Server::start(OsStr::new(env!("CARGO_BIN_EXE_harpas")), &[], &harpas);
    server.wait_for("Done (");
    punch_saved_animal(entity_type, harpas_port, 0.5);
    stop(&mut server);
    let drops = [&vanilla, &harpas].map(|dir| {
        let mut stacks = std::collections::BTreeMap::new();
        if let Some(saved) = read_entities(dir)
            && let Some(Nbt::List(List::Compound(entities))) = saved.get("Entities")
        {
            for entity in entities {
                assert_ne!(entity.get("id"), Some(&Nbt::from(kind)));
                if let Some(Nbt::Compound(stack)) = entity.get("Item") {
                    let Some(Nbt::String(item)) = stack.get("id") else {
                        panic!("missing drop ID")
                    };
                    let Some(Nbt::Int(count)) = stack.get("count") else {
                        panic!("missing drop count")
                    };
                    *stacks.entry(item.clone()).or_insert(0) += count;
                }
            }
        }
        stacks
    });
    assert!(drops[0].contains_key(meat));
    assert_eq!(drops[0], drops[1]);
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn bare_hand_pig_loot_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let entities = "world/dimensions/minecraft/overworld/entities";
    let read_entities = |dir: &Path| harpas_anvil::read_chunk(&dir.join(entities), 0, 0).unwrap();
    for drops in [true, false] {
        let folder = Folder::new("pig-loot");
        let vanilla = folder.0.join("vanilla");
        let harpas = folder.0.join("harpas");
        let [vanilla_port, harpas_port] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
        let mut reference = start_flat_vanilla(&jar, &vanilla, false);
        writeln!(
            reference.child.stdin.as_mut().unwrap(),
            "gamerule minecraft:mob_drops {drops}\nforceload add 0 0\nsave-all flush"
        )
        .unwrap();
        reference.wait_for("Saved the game");
        writeln!(
            reference.child.stdin.as_mut().unwrap(),
            "summon minecraft:pig 3.5 -60 1.5 {{Health:0.5f,NoAI:1b}}\nsave-all flush"
        )
        .unwrap();
        reference.wait_for("Summoned new Pig");
        reference.wait_for("Saved the game");
        let initial = read_entities(&vanilla).unwrap();
        harpas_anvil::write_chunks(&harpas.join(entities), 0, 0, &[((0, 0), &initial)]).unwrap();

        let expected = punch_and_collect_pig_loot(vanilla_port, drops);
        stop(&mut reference);
        let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
        assert_eq!(punch_and_collect_pig_loot(harpas_port, drops), expected);
        stop(&mut server);
        for dir in [&vanilla, &harpas] {
            let (data, _) =
                harpas_anvil::read_player(&dir.join("world"), offline_uuid("PigHunter"))
                    .unwrap()
                    .unwrap();
            let porkchops = match data.get("Inventory") {
                Some(Nbt::List(List::Compound(items))) => items
                    .iter()
                    .filter(|item| item.get("id") == Some(&Nbt::from("minecraft:porkchop")))
                    .map(|item| match item.get("count") {
                        Some(Nbt::Int(count)) => *count,
                        other => panic!("invalid porkchop count: {other:?}"),
                    })
                    .sum::<i32>(),
                _ => 0,
            };
            assert_eq!(porkchops, expected.unwrap_or(0));
            if let Some(saved) = read_entities(dir)
                && let Some(Nbt::List(List::Compound(entities))) = saved.get("Entities")
            {
                assert!(!entities.iter().any(|entity| {
                    matches!(entity.get("Item"), Some(Nbt::Compound(stack))
                        if stack.get("id") == Some(&Nbt::from("minecraft:porkchop")))
                }));
            }
        }
    }
}

fn punch_and_collect_pig_loot(port: u16, drops: bool) -> Option<i32> {
    let mut attacker = Client::join(port, "PigHunter");
    attacker.stream.set_nodelay(true).unwrap();
    let pig = track_saved_pig(&mut attacker);
    move_player(&mut attacker, [pig.x - 2.0, pig.y, pig.z], true);
    std::thread::sleep(Duration::from_millis(300));
    play_packet(
        &mut attacker,
        play::serverbound::Attack {
            entity_id: pig.entity_id,
        },
    );
    collect_pig_loot(&mut attacker, pig, drops.then_some(3))
}

fn collect_pig_loot(
    attacker: &mut Client,
    pig: play::clientbound::SpawnEntity,
    max_count: Option<i32>,
) -> Option<i32> {
    let porkchop = harpas_core::item::items().id("minecraft:porkchop").unwrap();
    let porkchop_drop = |packet: Packet| {
        let Packet::SetEntityMetadata(packet) = packet else {
            return None;
        };
        packet.metadata.entries.into_iter().find_map(|entry| {
            let (8, play::MetadataValue::ItemStack(stack)) = entry else {
                return None;
            };
            (stack.item == porkchop as i32 && stack.count > 0)
                .then_some((packet.entity_id, stack.count))
        })
    };
    let loot = max_count.map(|max_count| {
        let loot = attacker.matching("a stack of raw porkchops", porkchop_drop);
        assert!((1..=max_count).contains(&loot.1));
        move_player(attacker, [pig.x, pig.y, pig.z], true);
        loot
    });
    let mut picked_up = false;
    let mut removed = false;
    attacker.matching("pig removal and expected loot", |packet| {
        match packet {
            Packet::PickupItem(packet) if loot.is_some_and(|(id, _)| packet.collected == id) => {
                assert_eq!(Some(packet.count), loot.map(|(_, count)| count));
                picked_up = true;
            }
            Packet::RemoveEntities(packet) if packet.entity_ids.contains(&pig.entity_id) => {
                removed = true;
            }
            packet => {
                if let Some(observed) = porkchop_drop(packet) {
                    assert_eq!(Some(observed), loot, "unexpected stack of raw porkchops");
                }
            }
        }
        (removed && (picked_up || loot.is_none())).then_some(())
    });
    loot.map(|(_, count)| count)
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn sword_pig_combat_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("sword-pig-combat");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    writeln!(
        reference.child.stdin.as_mut().unwrap(),
        "forceload add 0 0\nsummon minecraft:pig 3.5 -60 1.5 {{NoAI:1b}}\nsave-all flush"
    )
    .unwrap();
    reference.wait_for("Summoned new Pig");
    reference.wait_for("Saved the game");
    let entities = "world/dimensions/minecraft/overworld/entities";
    let initial = harpas_anvil::read_chunk(&vanilla.join(entities), 0, 0)
        .unwrap()
        .unwrap();
    harpas_anvil::write_chunks(&harpas.join(entities), 0, 0, &[((0, 0), &initial)]).unwrap();

    let sword =
        |slot, name: &str| Compound::from_iter([("Slot", Nbt::Byte(slot)), ("id", name.into())]);
    let data = Compound::from_iter([
        ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
        (
            "Inventory",
            Nbt::List(List::Compound(vec![
                sword(1, "minecraft:wooden_sword"),
                sword(2, "minecraft:iron_sword"),
            ])),
        ),
    ]);
    let hunter = offline_uuid("PigHunter");
    for dir in [&vanilla, &harpas] {
        harpas_anvil::write_player(&dir.join("world"), hunter, &data).unwrap();
    }
    let expected = strike_pig_with_swords(port);
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    assert_eq!(strike_pig_with_swords(other), expected);
    stop(&mut server);

    let [expected_inventory, actual_inventory] = [&vanilla, &harpas].map(|dir| {
        let (saved, _) = harpas_anvil::read_player(&dir.join("world"), hunter)
            .unwrap()
            .unwrap();
        saved.get("Inventory").unwrap().clone()
    });
    assert_eq!(actual_inventory, expected_inventory);
    let Nbt::List(List::Compound(items)) = actual_inventory else {
        panic!("missing the hunter's inventory");
    };
    assert_eq!(items.len(), 3);
    for (name, damage) in [("minecraft:wooden_sword", 1), ("minecraft:iron_sword", 2)] {
        let sword = items
            .iter()
            .find(|item| item.get("id") == Some(&Nbt::from(name)))
            .unwrap();
        assert_eq!(
            sword.get("components"),
            Some(&Nbt::from(Compound::from_iter([(
                "minecraft:damage",
                damage
            )])))
        );
    }
    let porkchop = items
        .iter()
        .find(|item| item.get("id") == Some(&Nbt::from("minecraft:porkchop")))
        .unwrap();
    assert_eq!(porkchop.get("count"), Some(&Nbt::Int(expected)));
}

fn strike_pig_with_swords(port: u16) -> i32 {
    let mut attacker = Client::join(port, "PigHunter");
    attacker.stream.set_nodelay(true).unwrap();
    let pig = track_saved_pig(&mut attacker);
    let pig_health = |packet| animal_health_update(packet, pig.entity_id);
    assert_eq!(attacker.matching("initial pig health", pig_health), 10.0);
    move_player(&mut attacker, [pig.x - 2.0, pig.y, pig.z], true);
    let select =
        |client: &mut Client, slot| play_packet(client, play::serverbound::SetHeldItem { slot });
    let attack: play::serverbound::Packet = play::serverbound::Attack {
        entity_id: pig.entity_id,
    }
    .into();
    let strike = |client: &mut Client| play_packet(client, attack.clone());

    select(&mut attacker, 1);
    let equipped = attack_speed(&mut attacker, true);
    std::thread::sleep(Duration::from_millis(800));
    let mut frames = Vec::new();
    for _ in 0..2 {
        write_frame(&mut frames, |buf| attack.encode(buf));
    }
    attacker.stream.write_all(&frames).unwrap();
    assert_eq!(
        attacker.matching("pig health after the charged wooden sword hit", pig_health),
        6.0
    );
    assert_no_damage_until_list_reply(&mut attacker, "after the rapid sword attack on the pig");

    select(&mut attacker, 0);
    attack_speed(&mut attacker, false);
    std::thread::sleep(Duration::from_millis(800));
    select(&mut attacker, 2);
    assert_eq!(attack_speed(&mut attacker, true), equipped);
    strike(&mut attacker);
    let after_partial = attacker.matching(
        "pig health after the partially charged iron sword hit",
        pig_health,
    );
    let recharge_ticks = 20.0 / (equipped.base_value + equipped.modifiers[0].amount) as f32;
    assert!(
        (0..6).any(|ticks| {
            // Network timing can place the hit on any of these early recharge ticks.
            let charge = (ticks as f32 + 0.5) / recharge_ticks;
            (after_partial - (6.0 - 6.0 * (0.2 + charge * charge * 0.8))).abs() < 0.0001
        }),
        "unexpected pig health after the partially charged hit: {after_partial}"
    );

    std::thread::sleep(Duration::from_millis(800));
    attacker.stream.write_all(&frames).unwrap();
    collect_pig_loot(&mut attacker, pig, Some(3)).unwrap()
}

const PIG_LOOTING_ROWS: [f64; 4] = [1.5, 4.5, 7.5, 10.5];

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn pig_looting_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let main_hand = [(6.5, 0), (9.5, 1), (12.5, 2), (15.5, 3)];
    let offhand = [(3.5, 0)];
    let no_drops = [(3.5, 3)];
    let sword = |slot, level| {
        let mut sword = Compound::from_iter([
            ("Slot", Nbt::Byte(slot)),
            ("id", Nbt::from("minecraft:iron_sword")),
        ]);
        if level > 0 {
            sword.insert(
                "components",
                Compound::from_iter([(
                    "minecraft:enchantments",
                    Compound::from_iter([("minecraft:looting", level)]),
                )]),
            );
        }
        sword
    };
    let hunters = [
        (
            "LootingHunter",
            (0..=3)
                .map(|level| sword(level, i32::from(level)))
                .collect(),
        ),
        ("OffhandHunter", vec![sword(0, 0), sword(-106, 3)]),
    ];
    for drops in [true, false] {
        let folder = Folder::new("pig-looting");
        let vanilla = folder.0.join("vanilla");
        let harpas = folder.0.join("harpas");
        let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
        let mut reference = start_flat_vanilla(&jar, &vanilla, false);
        writeln!(
            reference.child.stdin.as_mut().unwrap(),
            "gamerule minecraft:mob_drops {drops}\nforceload add 0 0\nsave-all flush"
        )
        .unwrap();
        reference.wait_for("Saved the game");
        let columns = if drops {
            [&main_hand[..], &offhand]
        } else {
            [&no_drops, &[][..]]
        };
        let mut positions = Vec::new();
        for &(x, _) in columns.into_iter().flatten() {
            for z in PIG_LOOTING_ROWS {
                writeln!(
                    reference.child.stdin.as_mut().unwrap(),
                    "summon minecraft:pig {x} -60 {z} {{Health:0.5f,NoAI:1b}}"
                )
                .unwrap();
                reference.wait_for("Summoned new Pig");
                positions.push(List::Double(vec![x, -60.0, z]).into());
            }
        }
        writeln!(reference.child.stdin.as_mut().unwrap(), "save-all flush").unwrap();
        reference.wait_for("Saved the game");
        let entities = "world/dimensions/minecraft/overworld/entities";
        let initial = harpas_anvil::read_chunk(&vanilla.join(entities), 0, 0)
            .unwrap()
            .unwrap();
        let Some(Nbt::List(List::Compound(saved))) = initial.get("Entities") else {
            panic!("missing the saved Looting test pigs");
        };
        for position in positions {
            assert!(
                saved
                    .iter()
                    .any(|tag| tag.get("id") == Some(&Nbt::from("minecraft:pig"))
                        && tag.get("Pos") == Some(&position)),
                "missing saved pig at {position:?}"
            );
        }
        harpas_anvil::write_chunks(&harpas.join(entities), 0, 0, &[((0, 0), &initial)]).unwrap();
        for (name, inventory) in &hunters {
            let data = Compound::from_iter([
                ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
                ("Inventory", List::Compound(inventory.clone()).into()),
            ]);
            for dir in [&vanilla, &harpas] {
                harpas_anvil::write_player(&dir.join("world"), offline_uuid(name), &data).unwrap();
            }
        }

        let hunt = |port, server| {
            let mut main = Client::join(port, hunters[0].0);
            let mut counts = hunt_pigs_for_looting(&mut main, server, columns[0], drops);
            if drops {
                // Keep the first hunter connected to hold the fixture chunk in view across logins.
                let mut offhand = Client::join(port, hunters[1].0);
                counts.extend(hunt_pigs_for_looting(
                    &mut offhand,
                    server,
                    columns[1],
                    true,
                ));
            }
            counts
        };
        let expected = hunt(port, "vanilla");
        if drops {
            assert!(expected.iter().flatten().any(|&count| count > 3));
        }
        stop(&mut reference);
        let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
        assert_eq!(hunt(other, "Harpas"), expected, "mob_drops={drops}");
        stop(&mut server);
    }
}

fn hunt_pigs_for_looting(
    attacker: &mut Client,
    server: &str,
    columns: &[(f64, i16)],
    drops: bool,
) -> Vec<Option<i32>> {
    attacker.stream.set_nodelay(true).unwrap();
    let mut pigs = track_animals(
        attacker,
        100,
        &format!("Looting pigs on {server}, mob_drops={drops}, columns={columns:?}"),
        columns.len() * PIG_LOOTING_ROWS.len(),
        |pig| columns.iter().any(|&(x, _)| pig.x == x) && PIG_LOOTING_ROWS.contains(&pig.z),
    );
    pigs.sort_unstable_by_key(|pig| (pig.x as i32, pig.z as i32));

    let mut counts = Vec::new();
    let mut pigs = pigs.into_iter();
    for &(_, slot) in columns {
        play_packet(attacker, play::serverbound::SetHeldItem { slot });
        std::thread::sleep(Duration::from_millis(150));
        for pig in pigs.by_ref().take(PIG_LOOTING_ROWS.len()) {
            move_player(attacker, [pig.x - 2.0, pig.y, pig.z], true);
            play_packet(
                attacker,
                play::serverbound::Attack {
                    entity_id: pig.entity_id,
                },
            );
            counts.push(collect_pig_loot(
                attacker,
                pig,
                drops.then_some(3 + i32::from(slot)),
            ));
        }
    }
    counts
}

fn animal_health_update(packet: Packet, entity_id: i32) -> Option<f32> {
    match packet {
        Packet::SetEntityMetadata(packet) if packet.entity_id == entity_id => packet
            .metadata
            .entries
            .into_iter()
            .find_map(|(index, value)| match (index, value) {
                (9, play::MetadataValue::Float(health)) => Some(health),
                _ => None,
            }),
        _ => None,
    }
}

fn punch_saved_animal(entity_type: i32, port: u16, initial_health: f32) -> Option<[f64; 3]> {
    let mut attacker = Client::join(port, "PigHunter");
    attacker.stream.set_nodelay(true).unwrap();
    let attacker_id = attacker.matching("Play Login", |packet| match packet {
        Packet::Login(login) => Some(login.entity_id),
        _ => None,
    });
    let animal = track_animals(&mut attacker, entity_type, "saved animal", 1, |_| true)
        .pop()
        .unwrap();
    let health_update = |packet| animal_health_update(packet, animal.entity_id);
    assert_eq!(
        attacker.matching("initial animal health", health_update),
        initial_health
    );
    let punch = play::serverbound::Attack {
        entity_id: animal.entity_id,
    };
    let lethal = initial_health <= 1.0;
    if !lethal {
        move_player(&mut attacker, [animal.x - 7.0, -60.0, animal.z], true);
        play_packet(&mut attacker, punch.clone());
        attacker.send_command("list");
        attacker.matching("list after the out-of-range punch", |packet| match packet {
            Packet::DamageEvent(event) if event.entity_id == animal.entity_id => {
                panic!("out-of-range attack damaged the animal")
            }
            Packet::SystemChatMessage(_) => Some(()),
            _ => {
                assert_eq!(health_update(packet), None);
                None
            }
        });
    }
    let distance = if lethal { 4.0 } else { 5.5 };
    move_player(&mut attacker, [animal.x - distance, -60.0, animal.z], true);
    std::thread::sleep(Duration::from_millis(300));
    let mut frames = Vec::new();
    for _ in 0..2 {
        let packet = play::serverbound::Packet::from(punch.clone());
        write_frame(&mut frames, |buf| packet.encode(buf));
    }
    attacker.stream.write_all(&frames).unwrap();

    let mut damaged = false;
    let mut updated_health = None;
    let mut velocity = None;
    let mut died = false;
    let mut removed = false;
    attacker.matching("animal damage, health and motion", |packet| {
        match packet {
            Packet::DamageEvent(event) if event.entity_id == animal.entity_id => {
                assert!(!damaged, "rapid repeat should not damage the animal");
                let types = harpas_java::registry_names("minecraft:damage_type").unwrap();
                assert_eq!(types[event.source_type as usize], "minecraft:player_attack");
                assert_eq!(event.source_cause, attacker_id + 1);
                assert_eq!(event.source_direct, attacker_id + 1);
                damaged = true;
            }
            Packet::SetEntityVelocity(packet)
                if packet.entity_id == animal.entity_id && packet.velocity.x > 0.0 =>
            {
                let motion = packet.velocity;
                velocity.get_or_insert([motion.x, motion.y, motion.z]);
            }
            Packet::EntityEvent(event)
                if event.entity_id == animal.entity_id && event.event == 3 =>
            {
                died = true;
            }
            Packet::RemoveEntities(packet) if packet.entity_ids.contains(&animal.entity_id) => {
                removed = true;
            }
            _ => {
                if let Some(health) = health_update(packet) {
                    assert!(updated_health.is_none());
                    updated_health = Some(health);
                }
            }
        }
        if lethal {
            removed.then_some(())
        } else {
            (damaged && updated_health.is_some() && velocity.is_some()).then_some(())
        }
    });
    assert!(damaged);
    assert_eq!(died, lethal);
    assert_eq!(updated_health, (!lethal).then_some(initial_health - 1.0));
    velocity
}
