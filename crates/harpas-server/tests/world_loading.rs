//! A real server stops on an incompatible chunk even if no new connection wakes its listener.

use std::fs;
use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use harpas_anvil::{DATA_VERSION, Region, dimension_dir, write_level};
use harpas_core::nbt::Compound;
use harpas_java::write_frame;
use harpas_protocol_java::v26_1::{configuration, handshaking, login};

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

#[test]
fn incompatible_chunk_stops_the_idle_server_after_a_player_joins() {
    match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => drop(listener),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("Skipping loopback test: local sockets are not permitted");
            return;
        }
        Err(error) => panic!("Cannot open a loopback listener: {error}"),
    }
    let folder = Folder::new();
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
    let mut old_chunk = Compound::new();
    old_chunk.insert("DataVersion", DATA_VERSION - 1);
    region.set_chunk(0, 0, &old_chunk);
    region.save().unwrap();

    let mut command = Command::new(env!("CARGO_BIN_EXE_harpas"));
    command
        .current_dir(&folder.0)
        .stdin(Stdio::null())
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
    let address: SocketAddr = loop {
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
