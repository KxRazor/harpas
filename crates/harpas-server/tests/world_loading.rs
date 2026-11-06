//! A real server loads chunks and handles incompatible worlds over a loopback connection.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use harpas_anvil::{DATA_VERSION, Region, dimension_dir, write_level};
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_java::{FrameReader, write_frame};
use harpas_protocol_java::codec;
use harpas_protocol_java::v26_1::{configuration, handshaking, login, play};

struct Folder(PathBuf);

impl Folder {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "harpas-server-world-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn frame(encode: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_frame(&mut bytes, encode);
    bytes
}

fn loopback_available() -> bool {
    match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => {
            drop(listener);
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("Skipping loopback test: local sockets are not permitted");
            false
        }
        Err(error) => panic!("Cannot open a loopback listener: {error}"),
    }
}

fn prepare(folder: &Folder, chunk: &Compound) {
    fs::write(folder.0.join("harpas.toml"), "eula = true\n").unwrap();
    fs::write(
        folder.0.join("server.properties"),
        "server-ip=127.0.0.1\nserver-port=0\nonline-mode=false\nnetwork-compression-threshold=-1\nview-distance=2\n",
    )
    .unwrap();
    let pack = folder.0.join(".harpas/vanilla-data/26.1.zip");
    fs::create_dir_all(pack.parent().unwrap()).unwrap();
    // Provisioning checks for the pack's presence; generation does not use it in this test.
    fs::write(pack, []).unwrap();

    let world = folder.0.join("world");
    let mut level = Compound::new();
    level.insert("DataVersion", DATA_VERSION);
    write_level(&world, &level).unwrap();
    let region_dir = dimension_dir(&world, "minecraft:overworld").join("region");
    fs::create_dir_all(&region_dir).unwrap();
    let mut region = Region::open(&region_dir, 0, 0).unwrap();
    region.set_chunk(0, 0, chunk);
    region.save().unwrap();
}

fn start(folder: &Folder) -> (Server, SocketAddr) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_harpas"));
    command
        .current_dir(&folder.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("HARPAS_") {
            command.env_remove(name);
        }
    }
    let mut server = Server(command.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(15);
    let log = folder.0.join("logs/latest.log");
    let address = loop {
        let lines = fs::read_to_string(&log).unwrap_or_default();
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "server exited before listening: {lines}"
        );
        if let Some(address) = lines.lines().find_map(|line| {
            line.split_once("Listening for Java Edition on ")
                .and_then(|(_, address)| address.parse().ok())
        }) {
            break address;
        }
        assert!(Instant::now() < deadline, "server did not listen: {lines}");
        thread::sleep(Duration::from_millis(10));
    };
    (server, address)
}

fn stop(server: &mut Server) {
    server
        .0
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"stop\n")
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = server.0.try_wait().unwrap() {
            assert!(status.success(), "server failed during shutdown: {status}");
            return;
        }
        assert!(Instant::now() < deadline, "server did not stop and save");
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_log(server: &mut Server, log: &std::path::Path, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let contents = fs::read_to_string(log).unwrap_or_default();
        if contents.contains(text) {
            return;
        }
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "server exited before {text}: {contents}"
        );
        assert!(
            Instant::now() < deadline,
            "server did not reach {text}: {contents}"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

fn login(address: SocketAddr) -> TcpStream {
    let handshake: handshaking::serverbound::Packet = handshaking::serverbound::Handshake {
        protocol_version: 775,
        server_address: "localhost".into(),
        server_port: address.port(),
        intent: handshaking::Intent::Login,
    }
    .into();
    let start: login::serverbound::Packet = login::serverbound::LoginStart {
        name: "Steve".into(),
        player_uuid: 0,
    }
    .into();
    let acknowledged: login::serverbound::Packet = login::serverbound::LoginAcknowledged.into();
    let packs: configuration::serverbound::Packet = configuration::serverbound::SelectKnownPacks {
        packs: vec![configuration::KnownPack {
            namespace: "minecraft".into(),
            id: "core".into(),
            version: "26.1".into(),
        }],
    }
    .into();
    let finish: configuration::serverbound::Packet =
        configuration::serverbound::AcknowledgeFinishConfiguration.into();
    let mut client = TcpStream::connect(address).unwrap();
    for bytes in [
        frame(|buf| handshake.encode(buf)),
        frame(|buf| start.encode(buf)),
        frame(|buf| acknowledged.encode(buf)),
        frame(|buf| packs.encode(buf)),
        frame(|buf| finish.encode(buf)),
    ] {
        client.write_all(&bytes).unwrap();
    }
    client
}

fn next_frame(client: &mut TcpStream, frames: &mut FrameReader) -> Vec<u8> {
    loop {
        if let Some(frame) = frames.next_frame().unwrap() {
            return frame;
        }
        let mut buffer = [0; 16 * 1024];
        let count = client.read(&mut buffer).expect("server output timed out");
        assert_ne!(count, 0, "server closed before sending a chunk");
        frames.push(&buffer[..count]);
    }
}

#[test]
fn incompatible_chunk_stops_the_idle_server_after_a_player_joins() {
    if !loopback_available() {
        return;
    }
    let folder = Folder::new();
    let mut old_chunk = Compound::new();
    old_chunk.insert("DataVersion", DATA_VERSION - 1);
    prepare(&folder, &old_chunk);
    let (mut server, address) = start(&folder);
    let _client = login(address);
    let log = folder.0.join("logs/latest.log");
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = server.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "server did not stop: {}",
            fs::read_to_string(&log).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.code(), Some(1));
    let lines = fs::read_to_string(log).unwrap();
    assert!(lines.contains("Steve joined the game"), "{lines}");
    assert!(lines.contains("Cannot load chunk 0, 0"), "{lines}");
    assert!(lines.contains("--forceUpgrade"), "{lines}");
}

#[test]
fn server_sends_saved_block_states_over_a_real_connection() {
    if !loopback_available() {
        return;
    }
    let folder = Folder::new();
    let mut stone = Compound::new();
    stone.insert("Name", "minecraft:stone");
    let mut blocks = Compound::new();
    blocks.insert("palette", Nbt::List(List::Compound(vec![stone])));
    let mut biomes = Compound::new();
    biomes.insert(
        "palette",
        Nbt::List(List::String(vec!["minecraft:plains".into()])),
    );
    let mut section = Compound::new();
    section.insert("Y", -4_i8);
    section.insert("block_states", blocks);
    section.insert("biomes", biomes);
    let mut chunk = Compound::new();
    chunk.insert("DataVersion", DATA_VERSION);
    chunk.insert("xPos", 0);
    chunk.insert("zPos", 0);
    chunk.insert("Status", "minecraft:full");
    chunk.insert("sections", Nbt::List(List::Compound(vec![section])));
    prepare(&folder, &chunk);
    let (_server, address) = start(&folder);
    let mut client = login(address);
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut frames = FrameReader::default();
    assert!(matches!(
        login::clientbound::Packet::decode(&next_frame(&mut client, &mut frames)),
        Ok(login::clientbound::Packet::LoginSuccess(_))
    ));
    for _ in 0..33 {
        configuration::clientbound::Packet::decode(&next_frame(&mut client, &mut frames)).unwrap();
    }
    assert!(matches!(
        play::clientbound::Packet::decode(&next_frame(&mut client, &mut frames)),
        Ok(play::clientbound::Packet::Login(_))
    ));
    loop {
        match play::clientbound::Packet::decode(&next_frame(&mut client, &mut frames)).unwrap() {
            play::clientbound::Packet::ChunkDataAndUpdateLight(chunk)
                if (chunk.x, chunk.z) == (0, 0) =>
            {
                let mut data = chunk.data.as_slice();
                assert_eq!(codec::read_i16(&mut data).unwrap(), 4096);
                assert_eq!(codec::read_i16(&mut data).unwrap(), 0);
                assert_eq!(codec::read_u8(&mut data).unwrap(), 0);
                assert_eq!(codec::read_varint(&mut data).unwrap(), 1); // minecraft:stone
                break;
            }
            play::clientbound::Packet::ChunkBatchFinished(_) => {
                let ack: play::serverbound::Packet = play::serverbound::ChunkBatchReceived {
                    chunks_per_tick: 8.0,
                }
                .into();
                client.write_all(&frame(|buf| ack.encode(buf))).unwrap();
            }
            _ => {}
        }
    }
}

/// Gives the server in `folder` vanilla's data: a local test server's pack if there is one, which CI does not have,
/// or else the data of the reference server. Returns false if there is neither.
fn install_vanilla_data(folder: &Folder, reference: Option<&PathBuf>) -> bool {
    let pack = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test-environment/.harpas/vanilla-data/26.1.zip");
    let installed = folder.0.join(".harpas/vanilla-data/26.1.zip");
    fs::create_dir_all(installed.parent().unwrap()).unwrap();
    if pack.is_file() {
        fs::copy(pack, installed).unwrap();
        return true;
    }
    let Some(reference) = reference else {
        return false;
    };
    let eula = fs::read_to_string(reference.join("eula.txt")).expect("reference EULA");
    assert!(eula.lines().any(|line| line.trim() == "eula=true"));
    let output = Command::new(env!("CARGO_BIN_EXE_harpas"))
        .args(["vanilla-data", "install", "--from"])
        .arg(reference.join("server.jar"))
        .current_dir(&folder.0)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "could not install test data: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

#[test]
fn a_text_seed_starts_a_normal_world_that_keeps_it_when_reopened() {
    if !loopback_available() {
        return;
    }
    let reference = std::env::var_os("HARPAS_VANILLA_SERVER_DIR").map(PathBuf::from);
    let folder = Folder::new();
    fs::write(folder.0.join("harpas.toml"), "eula = true\n").unwrap();
    if !install_vanilla_data(&folder, reference.as_ref()) {
        eprintln!("Skipping a new world: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    }
    let properties = "server-ip=127.0.0.1\nserver-port=0\nonline-mode=false\nlevel-seed=hello\n";
    fs::write(folder.0.join("server.properties"), properties).unwrap();
    let log = folder.0.join("logs/latest.log");
    let prepared = "Prepared Overworld terrain generation for seed 99162322";
    let world = folder.0.join("world");

    let (mut server, _) = start(&folder);
    let lines = fs::read_to_string(&log).unwrap();
    assert!(lines.contains(prepared), "{lines}");
    assert_eq!(
        harpas_anvil::read_world_seed(&world).unwrap().unwrap().0,
        99_162_322
    );
    assert!(harpas_anvil::read_level(&world).unwrap().is_some());
    stop(&mut server);

    fs::write(
        folder.0.join("server.properties"),
        properties.replace("level-seed=hello", "level-seed=999"),
    )
    .unwrap();
    let (mut restarted, _) = start(&folder);
    let lines = fs::read_to_string(&log).unwrap();
    assert!(lines.contains(prepared), "{lines}");
    assert_eq!(
        harpas_anvil::read_world_seed(&world).unwrap().unwrap().0,
        99_162_322
    );
    stop(&mut restarted);
}

#[test]
fn a_new_world_generates_saves_and_reuses_its_seed_when_data_is_available() {
    if !loopback_available() {
        return;
    }
    let reference = std::env::var_os("HARPAS_VANILLA_SERVER_DIR").map(PathBuf::from);
    let folder = Folder::new();
    fs::write(folder.0.join("harpas.toml"), "eula = true\n").unwrap();
    if !install_vanilla_data(&folder, reference.as_ref()) {
        eprintln!("Skipping a new world: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    }
    let properties = "server-ip=127.0.0.1\nserver-port=0\nonline-mode=false\nnetwork-compression-threshold=-1\nview-distance=2\nlevel-seed=42\n";
    fs::write(folder.0.join("server.properties"), properties).unwrap();
    let (mut server, address) = start(&folder);
    let world = folder.0.join("world");
    assert_eq!(
        harpas_anvil::read_world_seed(&world).unwrap().unwrap().0,
        42
    );
    let spawn =
        harpas_anvil::overworld_spawn(&harpas_anvil::read_level(&world).unwrap().unwrap().0)
            .unwrap();
    let mut client = login(address);
    client
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let mut frames = FrameReader::default();
    login::clientbound::Packet::decode(&next_frame(&mut client, &mut frames)).unwrap();
    for _ in 0..33 {
        configuration::clientbound::Packet::decode(&next_frame(&mut client, &mut frames)).unwrap();
    }
    let play::clientbound::Packet::Login(login) =
        play::clientbound::Packet::decode(&next_frame(&mut client, &mut frames)).unwrap()
    else {
        panic!("expected play login")
    };
    assert!(!login.spawn.is_flat);
    assert_eq!(
        login.spawn.hashed_seed,
        harpas_worldgen::BiomeManager::obfuscate_seed(42)
    );
    loop {
        match play::clientbound::Packet::decode(&next_frame(&mut client, &mut frames)).unwrap() {
            play::clientbound::Packet::ChunkDataAndUpdateLight(chunk)
                if (chunk.x, chunk.z) == (0, 0) =>
            {
                let mut data = chunk.data.as_slice();
                assert!(
                    codec::read_i16(&mut data).unwrap() > 0,
                    "the lowest section should contain terrain"
                );
                assert!(
                    !chunk.light.sky_light_mask.is_empty() && !chunk.light.sky_light.is_empty(),
                    "a generated chunk should carry computed sky light"
                );
                break;
            }
            play::clientbound::Packet::ChunkBatchFinished(_) => {
                let ack: play::serverbound::Packet = play::serverbound::ChunkBatchReceived {
                    chunks_per_tick: 8.0,
                }
                .into();
                client.write_all(&frame(|buf| ack.encode(buf))).unwrap();
            }
            _ => {}
        }
    }
    drop(client);
    stop(&mut server);
    let region = dimension_dir(&world, "minecraft:overworld").join("region");
    let chunk = harpas_anvil::read_chunk(&region, 0, 0).unwrap().unwrap();
    assert_eq!(chunk.get("Status"), Some(&Nbt::from("minecraft:carvers")));
    fs::write(
        folder.0.join("server.properties"),
        properties.replace("level-seed=42", "level-seed=99"),
    )
    .unwrap();
    let (mut restarted, _) = start(&folder);
    assert_eq!(
        harpas_anvil::read_world_seed(&world).unwrap().unwrap().0,
        42
    );
    assert_eq!(
        harpas_anvil::overworld_spawn(&harpas_anvil::read_level(&world).unwrap().unwrap().0),
        Some(spawn)
    );
    stop(&mut restarted);

    if let Some(reference) = reference {
        let eula = fs::read_to_string(reference.join("eula.txt")).expect("reference EULA");
        assert!(eula.lines().any(|line| line.trim() == "eula=true"));
        fs::write(folder.0.join("eula.txt"), "eula=true\n").unwrap();
        let log = folder.0.join("vanilla-output.log");
        let output = fs::File::create(&log).unwrap();
        let mut vanilla = Server(
            Command::new("java")
                .args(["-Xms256M", "-Xmx1G", "-jar"])
                .arg(reference.join("server.jar"))
                .arg("nogui")
                .current_dir(&folder.0)
                .stdin(Stdio::piped())
                .stdout(Stdio::from(output.try_clone().unwrap()))
                .stderr(Stdio::from(output))
                .spawn()
                .unwrap(),
        );
        wait_for_log(&mut vanilla, &log, "Done (");
        vanilla
            .0
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"forceload add 0 0\n")
            .unwrap();
        wait_for_log(&mut vanilla, &log, "Marked chunk [0, 0]");
        vanilla
            .0
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"save-all flush\n")
            .unwrap();
        wait_for_log(&mut vanilla, &log, "Saved the game");
        stop(&mut vanilla);
        let log_contents = fs::read_to_string(&log).unwrap();
        assert!(
            !log_contents.contains("Failed to load chunk"),
            "{log_contents}"
        );
        let chunk = harpas_anvil::read_chunk(&region, 0, 0).unwrap().unwrap();
        assert_eq!(chunk.get("Status"), Some(&Nbt::from("minecraft:full")));
        assert_eq!(
            harpas_anvil::read_world_seed(&world).unwrap().unwrap().0,
            42
        );
    }
}
