//! Compares block entity chunk updates with an owner-provided, EULA-accepted Minecraft 26.1 server.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use harpas_core::nbt::{Compound, List, Nbt};
use harpas_java::{FrameReader, write_frame};
use harpas_protocol_java::{
    codec,
    v26_1::{configuration, handshaking, login, play},
};

struct Folder(PathBuf);

impl Folder {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "harpas-block-entity-parity-{}-{}",
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

/// A server whose output can be awaited while its standard input remains open.
struct Server {
    child: Child,
    lines: Receiver<String>,
    log: String,
}

impl Server {
    fn start(program: &OsStr, args: &[&OsStr], dir: &Path) -> Self {
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("HARPAS_") {
                command.env_remove(name);
            }
        }
        let mut child = command.spawn().unwrap();
        let (sender, lines) = mpsc::channel();
        for stream in [
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let sender = sender.clone();
            thread::spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            });
        }
        Self {
            child,
            lines,
            log: String::new(),
        }
    }

    fn wait_for(&mut self, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if let Ok(line) = self.lines.recv_timeout(Duration::from_millis(200)) {
                self.log.push_str(&line);
                self.log.push('\n');
                if line.contains(text) {
                    return;
                }
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "server exited before {text}: {}",
                self.log
            );
            assert!(
                Instant::now() < deadline,
                "server did not reach {text}: {}",
                self.log
            );
        }
    }

    fn command(&mut self, command: &str) {
        let input = self.child.stdin.as_mut().unwrap();
        writeln!(input, "{command}").unwrap();
        input.flush().unwrap();
    }

    fn wait_for_moving_piston(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            self.command(
                "execute if block 2 65 6 minecraft:moving_piston run say harpas-piston-ready",
            );
            let retry = Instant::now() + Duration::from_millis(200);
            while let Some(wait) = retry.checked_duration_since(Instant::now()) {
                match self.lines.recv_timeout(wait) {
                    Ok(line) => {
                        self.log.push_str(&line);
                        self.log.push('\n');
                        if line.contains("harpas-piston-ready") {
                            return;
                        }
                    }
                    Err(_) => break,
                }
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "reference server exited before moving the piston: {}",
                self.log
            );
        }
        panic!("reference piston did not move: {}", self.log);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() == "session.lock" {
            continue;
        }
        let destination = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).unwrap();
        }
    }
}

fn assert_variant_fixtures(world: &Path) {
    let region = world.join("dimensions/minecraft/overworld/region");
    let chunk = harpas_anvil::read_chunk(&region, 0, 0)
        .unwrap()
        .expect("fixture chunk was not saved");
    let Some(Nbt::List(List::Compound(entities))) = chunk.get("block_entities") else {
        panic!("fixture chunk has no block entities");
    };
    for (x, z, field) in [
        (1, 1, "front_text"),
        (2, 1, "Items"),
        (3, 1, "SpawnData"),
        (1, 2, "front_text"),
        (2, 2, "primary_effect"),
        (3, 2, "profile"),
        (3, 2, "note_block_sound"),
        (4, 2, "sherds"),
        (4, 2, "item"),
        (5, 2, "exit_portal"),
        (6, 2, "item"),
        (7, 2, "Items"),
        (8, 2, "components"),
        (1, 8, "RecordItem"),
        (2, 8, "Book"),
        (3, 8, "Command"),
        (4, 8, "server_data"),
        (5, 8, "Items"),
        (6, 8, "Items"),
        (7, 8, "bees"),
        (8, 8, "name"),
        (9, 8, "SpawnPotentials"),
        (10, 8, "back_text"),
        (11, 8, "Items"),
        (12, 8, "Items"),
        (13, 8, "Items"),
        (14, 8, "spawn_data"),
    ] {
        let entity = entities
            .iter()
            .find(|entity: &&Compound| {
                entity.get("x") == Some(&Nbt::Int(x))
                    && entity.get("y") == Some(&Nbt::Int(65))
                    && entity.get("z") == Some(&Nbt::Int(z))
            })
            .unwrap_or_else(|| panic!("missing fixture at {x}, 65, {z}"));
        let valid = match entity.get(field) {
            None | Some(Nbt::List(List::Empty)) => false,
            Some(Nbt::List(List::Compound(values))) => !values.is_empty(),
            Some(Nbt::Compound(value)) => !value.is_empty(),
            Some(Nbt::String(value)) => !value.is_empty(),
            Some(_) => true,
        };
        assert!(valid, "fixture at {x}, 65, {z} has no {field}");
    }
}

fn send(client: &mut TcpStream, encode: impl FnOnce(&mut Vec<u8>)) {
    let mut frame = Vec::new();
    write_frame(&mut frame, encode);
    client.write_all(&frame).unwrap();
}

fn next(client: &mut TcpStream, frames: &mut FrameReader) -> Vec<u8> {
    loop {
        if let Some(frame) = frames.next_frame().unwrap() {
            return frame;
        }
        let mut buf = [0; 16 * 1024];
        let count = client.read(&mut buf).expect("server output timed out");
        assert!(count > 0, "server disconnected before sending the chunk");
        frames.push(&buf[..count]);
    }
}

fn capture(port: u16, filtered: bool) -> Vec<play::ChunkBlockEntity> {
    let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut frames = FrameReader::default();
    let handshake: handshaking::serverbound::Packet = handshaking::serverbound::Handshake {
        protocol_version: 775,
        server_address: "localhost".into(),
        server_port: port,
        intent: handshaking::Intent::Login,
    }
    .into();
    let login: login::serverbound::Packet = login::serverbound::LoginStart {
        name: if filtered {
            "FilteredTest"
        } else {
            "ParityTest"
        }
        .into(),
        player_uuid: 0,
    }
    .into();
    send(&mut client, |b| handshake.encode(b));
    send(&mut client, |b| login.encode(b));
    let first = next(&mut client, &mut frames);
    assert!(matches!(
        login::clientbound::Packet::decode(&first),
        Ok(login::clientbound::Packet::LoginSuccess(_))
    ));
    let acknowledged: login::serverbound::Packet = login::serverbound::LoginAcknowledged.into();
    send(&mut client, |b| acknowledged.encode(b));
    send(&mut client, |b| {
        codec::write_varint(b, 0); // Client Information
        codec::write_string(b, "en_us");
        b.extend_from_slice(&[2, 0, 1, 127, 1, u8::from(filtered), 1, 0]);
    });
    loop {
        let frame = next(&mut client, &mut frames);
        let id = codec::read_varint(&mut frame.as_slice()).unwrap();
        if id == 0x0e {
            let packs: configuration::serverbound::Packet =
                configuration::serverbound::SelectKnownPacks {
                    packs: vec![configuration::KnownPack {
                        namespace: "minecraft".into(),
                        id: "core".into(),
                        version: "26.1".into(),
                    }],
                }
                .into();
            send(&mut client, |b| packs.encode(b));
        }
        if id == 3 {
            let finished: configuration::serverbound::Packet =
                configuration::serverbound::AcknowledgeFinishConfiguration.into();
            send(&mut client, |b| finished.encode(b));
            break;
        }
    }
    for _ in 0..500 {
        let frame = next(&mut client, &mut frames);
        let id = codec::read_varint(&mut frame.as_slice()).unwrap();
        if id == 0x0b {
            let ack: play::serverbound::Packet = play::serverbound::ChunkBatchReceived {
                chunks_per_tick: 64.0,
            }
            .into();
            send(&mut client, |b| ack.encode(b));
        }
        if id == 0x2d {
            match play::clientbound::Packet::decode(&frame) {
                Ok(play::clientbound::Packet::ChunkDataAndUpdateLight(chunk))
                    if (chunk.x, chunk.z) == (0, 0) =>
                {
                    return chunk.block_entities;
                }
                Err(error) => panic!("cannot decode chunk: {error:?}"),
                _ => {}
            }
        }
    }
    panic!("did not receive chunk 0, 0");
}

fn assert_updates_match(expected: &[play::ChunkBlockEntity], actual: &[play::ChunkBlockEntity]) {
    assert_eq!(
        expected.len(),
        actual.len(),
        "different numbers of block entities"
    );
    let mut differences = Vec::new();
    for entity in expected {
        let received = actual
            .iter()
            .find(|received| received.position == entity.position && received.y == entity.y)
            .expect("Harpas omitted a block entity");
        if received.kind != entity.kind || received.data != entity.data {
            differences.push(format!(
                "at {:?}: expected {entity:?}, received {received:?}",
                entity.position
            ));
        }
        if (entity.position.x, entity.position.z, entity.y) == (2, 1, 65) {
            assert!(
                received.data.is_none(),
                "a chest's inventory must not be sent"
            );
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

#[test]
fn saved_block_entities_match_vanilla_chunk_updates() {
    let Some(reference) = std::env::var_os("HARPAS_VANILLA_SERVER_DIR") else {
        eprintln!("Skipping parity test: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    let reference = PathBuf::from(reference);
    let eula = fs::read_to_string(reference.join("eula.txt")).expect("reference EULA");
    assert!(
        eula.lines().any(|line| line.trim() == "eula=true"),
        "the reference server's EULA must already be accepted"
    );
    let jar = fs::canonicalize(reference.join("server.jar")).expect("reference server.jar");
    let folder = Folder::new();
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    fs::create_dir_all(&vanilla).unwrap();
    fs::create_dir_all(&harpas).unwrap();
    fs::write(vanilla.join("eula.txt"), &eula).unwrap();
    fs::write(harpas.join("eula.txt"), &eula).unwrap();

    // Reserve two different ports while choosing them.
    let first = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let second = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let vanilla_port = first.local_addr().unwrap().port();
    let harpas_port = second.local_addr().unwrap().port();
    drop((first, second));
    for (dir, port) in [(&vanilla, vanilla_port), (&harpas, harpas_port)] {
        fs::write(
            dir.join("server.properties"),
            format!(
                "server-ip=127.0.0.1\nserver-port={port}\nonline-mode=false\n\
                 network-compression-threshold=-1\nview-distance=2\nsimulation-distance=2\n\
                 level-type=minecraft:flat\ngenerate-structures=false\nmax-tick-time=-1\n"
            ),
        )
        .unwrap();
    }

    // The existing installation verifies the jar, generates the IDs locally and removes its temporary reports.
    let install = Command::new(env!("CARGO_BIN_EXE_harpas"))
        .current_dir(&harpas)
        .args(["vanilla-data", "install", "--from"])
        .arg(&jar)
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "vanilla data installation failed: {}",
        String::from_utf8_lossy(&install.stderr)
    );
    let registry_path = harpas.join(".harpas/vanilla-data/26.1-block-entity-types.json");
    let registry: serde_json::Value = serde_json::from_slice(
        &fs::read(&registry_path).expect("Java did not generate the registry"),
    )
    .unwrap();

    let mut reference = Server::start(
        OsStr::new("java"),
        &[
            OsStr::new("-Xmx1G"),
            OsStr::new("-jar"),
            jar.as_os_str(),
            OsStr::new("nogui"),
        ],
        &vanilla,
    );
    reference.wait_for("Done (");
    for command in [
        "forceload add 0 0",
        "setworldspawn 1 65 1",
        "fill 0 64 0 15 64 15 minecraft:stone",
        r#"setblock 1 65 1 minecraft:oak_sign{front_text:{messages:[{text:"Hello"},{text:"world"},{text:""},{text:""}],filtered_messages:[{text:"Hi"},{text:"world"},{text:""},{text:""}],color:"red"},is_waxed:1b}"#,
        r#"setblock 2 65 1 minecraft:chest{Items:[{Slot:0b,id:"minecraft:diamond",count:1}]}"#,
        r#"setblock 3 65 1 minecraft:spawner{SpawnData:{entity:{id:"minecraft:pig"}}}"#,
        r#"setblock 1 65 2 minecraft:oak_hanging_sign{front_text:{messages:[{text:"Hanging"},{text:""},{text:""},{text:""}]}}"#,
        r#"setblock 2 65 2 minecraft:beacon{primary_effect:"minecraft:speed"}"#,
        r#"setblock 3 65 2 minecraft:player_head{profile:{name:"Notch"},note_block_sound:"minecraft:block.note_block.bell"}"#,
        r#"setblock 4 65 2 minecraft:decorated_pot{sherds:["minecraft:angler_pottery_sherd","minecraft:brick","minecraft:brick","minecraft:brick"],item:{id:"minecraft:diamond",count:1}}"#,
        r#"setblock 5 65 2 minecraft:end_gateway{exit_portal:[I;10,65,10],ExactTeleport:1b}"#,
        r#"setblock 6 65 2 minecraft:suspicious_sand{item:{id:"minecraft:diamond",count:1}}"#,
        r#"setblock 7 65 2 minecraft:oak_shelf{Items:[{Slot:0b,id:"minecraft:diamond",count:1}]}"#,
        r#"setblock 8 65 2 minecraft:white_banner{components:{"minecraft:banner_patterns":[{pattern:"minecraft:stripe_bottom",color:"blue"}]}}"#,
        "setblock 9 65 2 minecraft:campfire",
        "setblock 10 65 2 minecraft:jigsaw",
        "setblock 11 65 2 minecraft:structure_block",
        "setblock 12 65 2 minecraft:test_block",
        "setblock 13 65 2 minecraft:test_instance_block",
        "setblock 14 65 2 minecraft:trial_spawner",
        "setblock 1 65 3 minecraft:vault",
    ] {
        reference.command(command);
    }
    for (index, block) in [
        "barrel",
        "furnace",
        "blast_furnace",
        "smoker",
        "brewing_stand",
        "hopper",
        "dispenser",
        "dropper",
        "trapped_chest",
        "ender_chest",
        "shulker_box",
        "chiseled_bookshelf",
        "lectern",
        "jukebox",
        "beehive",
        "bell",
        "conduit",
        "enchanting_table",
        "sculk_sensor",
        "calibrated_sculk_sensor",
        "sculk_catalyst",
        "sculk_shrieker",
        "creaking_heart",
        "copper_golem_statue",
        "daylight_detector",
        "comparator",
        "crafter",
        "command_block",
        "red_bed",
        "end_portal",
    ]
    .into_iter()
    .enumerate()
    {
        let x = index % 14 + 2;
        let z = index / 14 + 3;
        reference.command(&format!("setblock {x} 65 {z} minecraft:{block}"));
    }
    // A moving piston entity must be created by a real piston movement, not setblock.
    reference.command("tick freeze");
    for command in [
        r#"setblock 1 65 8 minecraft:jukebox[has_record=true]{RecordItem:{id:"minecraft:music_disc_13",count:1}}"#,
        r#"setblock 2 65 8 minecraft:lectern[has_book=true]{Book:{id:"minecraft:writable_book",count:1,components:{"minecraft:writable_book_content":{pages:["private page"]}}}}"#,
        r#"setblock 3 65 8 minecraft:command_block{Command:"say SECRET_COMMAND",CustomName:"operator name"}"#,
        r#"setblock 4 65 8 minecraft:vault{shared_data:{display_item:{id:"minecraft:diamond",count:2},connected_particles_range:8.0d},server_data:{items_to_eject:[{id:"minecraft:netherite_ingot",count:1}],rewarded_players:[[I;1,2,3,4]]}}"#,
        r#"setblock 5 65 8 minecraft:campfire{Items:[{Slot:0b,id:"minecraft:salmon",count:1,components:{"minecraft:custom_data":{note:"visible item"}}}],CookingTimes:[I;23,0,0,0],CookingTotalTimes:[I;1000,0,0,0]}"#,
        r#"setblock 6 65 8 minecraft:chiseled_bookshelf[slot_0_occupied=true]{Items:[{Slot:0b,id:"minecraft:writable_book",count:1}]}"#,
        r#"setblock 7 65 8 minecraft:beehive{bees:[{entity_data:{id:"minecraft:bee"},ticks_in_hive:10,min_ticks_in_hive:200}]}"#,
        r#"setblock 8 65 8 minecraft:structure_block{name:"minecraft:parity_test",mode:"SAVE",posX:1,integrity:0.75f,seed:42L}"#,
        r#"setblock 9 65 8 minecraft:spawner{SpawnData:{entity:{id:"minecraft:pig"}},SpawnPotentials:[{weight:1,data:{entity:{id:"minecraft:pig"}}}],Delay:15s}"#,
        r#"setblock 10 65 8 minecraft:oak_sign{front_text:{messages:[{text:"front"},{text:""},{text:""},{text:""}]},back_text:{messages:[{text:"back"},{text:""},{text:""},{text:""}]}}"#,
        r#"setblock 11 65 8 minecraft:shulker_box{Items:[{Slot:0b,id:"minecraft:diamond",count:1}]}"#,
        r#"setblock 12 65 8 minecraft:brewing_stand{Items:[{Slot:0b,id:"minecraft:potion",count:1}]}"#,
        r#"setblock 13 65 8 minecraft:furnace{Items:[{Slot:0b,id:"minecraft:iron_ore",count:1}],cooking_time_spent:12s}"#,
        r#"setblock 14 65 8 minecraft:trial_spawner{spawn_data:{entity:{id:"minecraft:zombie"}}}"#,
    ] {
        reference.command(command);
    }
    reference.command("setblock 1 65 6 minecraft:piston[facing=east]");
    reference.command("setblock 2 65 6 minecraft:stone");
    reference.command("setblock 0 65 6 minecraft:redstone_block");
    reference.command("tick step 1");
    reference.wait_for("Stepping 1 tick(s)");
    reference.wait_for_moving_piston();
    reference.command("save-all flush");
    reference.wait_for("Saved the game");
    assert_variant_fixtures(&vanilla.join("world"));
    copy_tree(&vanilla.join("world"), &harpas.join("world"));

    let mut server = Server::start(OsStr::new(env!("CARGO_BIN_EXE_harpas")), &[], &harpas);
    server.wait_for("Done (");
    let expected = capture(vanilla_port, false);
    let actual = capture(harpas_port, false);
    let kinds: HashSet<_> = expected.iter().map(|entity| entity.kind).collect();
    let missing: Vec<_> = registry["entries"]
        .as_object()
        .unwrap()
        .iter()
        .filter_map(|(name, id)| {
            let id = i32::try_from(id.as_i64().unwrap()).unwrap();
            (!kinds.contains(&id)).then_some(name.as_str())
        })
        .collect();
    assert!(
        missing.is_empty(),
        "untested block entity types: {missing:?}"
    );
    println!(
        "Comparing {} block entities covering {} types",
        expected.len(),
        kinds.len()
    );
    let sign = expected
        .iter()
        .find(|entity| (entity.position.x, entity.position.z, entity.y) == (1, 1, 65))
        .expect("reference sign was not created");
    assert!(matches!(
        &sign.data,
        Some(Nbt::Compound(data)) if matches!(
            data.get("front_text"),
            Some(Nbt::Compound(text)) if text.get("filtered_messages").is_some()
        )
    ));
    let chest = expected
        .iter()
        .find(|entity| (entity.position.x, entity.position.z, entity.y) == (2, 1, 65))
        .expect("reference chest was not created");
    assert!(
        chest.data.is_none(),
        "vanilla must not send a chest's inventory"
    );
    for (x, description) in [
        (1, "jukebox record"),
        (2, "lectern book"),
        (3, "command block"),
        (6, "bookshelf contents"),
        (7, "beehive occupants"),
        (11, "shulker inventory"),
        (12, "brewing stand inventory"),
        (13, "furnace inventory"),
    ] {
        let entity = expected
            .iter()
            .find(|entity| (entity.position.x, entity.position.z, entity.y) == (x, 8, 65))
            .unwrap_or_else(|| panic!("missing reference {description}"));
        assert!(
            entity.data.is_none(),
            "vanilla sent {description} in a chunk"
        );
    }
    let vault = expected
        .iter()
        .find(|entity| (entity.position.x, entity.position.z, entity.y) == (4, 8, 65))
        .expect("missing reference vault");
    let Some(Nbt::Compound(vault)) = &vault.data else {
        panic!("reference vault did not send its visible data");
    };
    assert!(matches!(
        vault.get("shared_data"),
        Some(Nbt::Compound(shared)) if shared.get("display_item").is_some()
    ));
    assert!(vault.get("server_data").is_none());
    let spawner = expected
        .iter()
        .find(|entity| (entity.position.x, entity.position.z, entity.y) == (9, 8, 65))
        .expect("missing reference spawner");
    let Some(Nbt::Compound(spawner)) = &spawner.data else {
        panic!("reference spawner did not send its visible data");
    };
    assert!(spawner.get("SpawnData").is_some());
    assert!(spawner.get("SpawnPotentials").is_none());
    let sign = expected
        .iter()
        .find(|entity| (entity.position.x, entity.position.z, entity.y) == (10, 8, 65))
        .expect("missing reference sign");
    let Some(Nbt::Compound(sign)) = &sign.data else {
        panic!("reference sign did not send its text");
    };
    assert!(sign.get("front_text").is_some());
    assert!(sign.get("back_text").is_some());
    for x in [2, 3] {
        let piston = expected
            .iter()
            .find(|entity| (entity.position.x, entity.position.z, entity.y) == (x, 6, 65))
            .expect("missing reference moving piston");
        assert!(matches!(
            &piston.data,
            Some(Nbt::Compound(data)) if data.get("blockState").is_some()
        ));
    }
    assert_updates_match(&expected, &actual);
    let filtered_expected = capture(vanilla_port, true);
    let filtered_actual = capture(harpas_port, true);
    assert_updates_match(&filtered_expected, &filtered_actual);
}
