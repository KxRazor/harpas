//! Compares the tags of the Configuration state's Update Tags packet with an owner-provided, EULA-accepted
//! Minecraft 26.1 server.
//!
//! Both servers are started on loopback, and the same client goes through Login and Configuration with each
//! until it receives Update Tags. Every registry, every tag and every entry of each tag, in order, must match.
//! Vanilla's order of registries and of tags changes from one run of its server to the next, so those are compared
//! by name. Design: `docs/java-connection.md#tags`, `docs/parity.md`.

use std::collections::BTreeMap;
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

use harpas_java::{FrameReader, write_frame};
use harpas_protocol_java::v26_1::{PROTOCOL, configuration, handshaking, login};

/// Registry, then tag, then the numeric IDs of the tag's entries in the order they were sent.
type AllTags = BTreeMap<String, BTreeMap<String, Vec<i32>>>;

struct Folder(PathBuf);

impl Folder {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "harpas-tags-parity-{}-{}",
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

/// A server whose output can be awaited.
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
        let streams: [Option<Box<dyn Read + Send>>; 2] = [
            child.stdout.take().map(|s| Box::new(s) as _),
            child.stderr.take().map(|s| Box::new(s) as _),
        ];
        for stream in streams.into_iter().flatten() {
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
        let deadline = Instant::now() + Duration::from_secs(180);
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
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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
        let mut buf = [0; 64 * 1024];
        let count = client.read(&mut buf).expect("server output timed out");
        assert!(count > 0, "server disconnected before sending its tags");
        frames.push(&buf[..count]);
    }
}

/// Joins the server on `port`, with compression off, and returns the tags it sends in Configuration.
fn capture(port: u16) -> AllTags {
    let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut frames = FrameReader::default();
    let handshake: handshaking::serverbound::Packet = handshaking::serverbound::Handshake {
        protocol_version: PROTOCOL,
        server_address: "localhost".into(),
        server_port: port,
        intent: handshaking::Intent::Login,
    }
    .into();
    let start: login::serverbound::Packet = login::serverbound::LoginStart {
        name: "TagsParity".into(),
        player_uuid: 0,
    }
    .into();
    send(&mut client, |b| handshake.encode(b));
    send(&mut client, |b| start.encode(b));
    let first = next(&mut client, &mut frames);
    assert!(
        matches!(
            login::clientbound::Packet::decode(&first),
            Ok(login::clientbound::Packet::LoginSuccess(_))
        ),
        "expected Login Success with compression off"
    );
    let acknowledged: login::serverbound::Packet = login::serverbound::LoginAcknowledged.into();
    send(&mut client, |b| acknowledged.encode(b));
    loop {
        let frame = next(&mut client, &mut frames);
        match configuration::clientbound::Packet::decode(&frame) {
            Ok(configuration::clientbound::Packet::SelectKnownPacks(packs)) => {
                let answer: configuration::serverbound::Packet =
                    configuration::serverbound::SelectKnownPacks { packs: packs.packs }.into();
                send(&mut client, |b| answer.encode(b));
            }
            Ok(configuration::clientbound::Packet::UpdateTags(update)) => {
                let mut all = AllTags::new();
                for registry in update.registries {
                    let tags = all.entry(registry.registry.clone()).or_default();
                    for tag in registry.tags {
                        let name = tag.name.clone();
                        assert!(
                            tags.insert(tag.name, tag.entries).is_none(),
                            "{} sent twice in {}",
                            name,
                            registry.registry
                        );
                    }
                }
                return all;
            }
            Ok(configuration::clientbound::Packet::FinishConfiguration(_)) => {
                panic!("configuration finished without Update Tags")
            }
            Ok(_) => {}
            Err(error) => panic!("cannot decode a Configuration packet: {error:?}"),
        }
    }
}

/// Every difference between the tags vanilla sends and Harpas sends.
fn differences(vanilla: &AllTags, harpas: &AllTags) -> Vec<String> {
    let mut differences = Vec::new();
    let registries: std::collections::BTreeSet<_> = vanilla.keys().chain(harpas.keys()).collect();
    let empty = BTreeMap::new();
    for registry in registries {
        let expected = vanilla.get(registry).unwrap_or(&empty);
        let actual = harpas.get(registry).unwrap_or(&empty);
        if !vanilla.contains_key(registry) {
            differences.push(format!("{registry}: sent by Harpas only"));
        }
        if !harpas.contains_key(registry) {
            differences.push(format!("{registry}: missing from Harpas"));
            continue;
        }
        let tags: std::collections::BTreeSet<_> = expected.keys().chain(actual.keys()).collect();
        for tag in tags {
            match (expected.get(tag), actual.get(tag)) {
                (Some(expected), Some(actual)) if expected != actual => differences.push(format!(
                    "{registry} #{tag}: vanilla {expected:?}, Harpas {actual:?}"
                )),
                (Some(_), None) => differences.push(format!("{registry} #{tag}: missing")),
                (None, Some(_)) => {
                    differences.push(format!("{registry} #{tag}: sent by Harpas only"))
                }
                _ => {}
            }
        }
    }
    differences
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn update_tags_match_vanilla() {
    let reference = PathBuf::from(
        std::env::var_os("HARPAS_VANILLA_SERVER_DIR")
            .expect("HARPAS_VANILLA_SERVER_DIR is not set (docs/parity.md#locally)"),
    );
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
    // The world does not change the tags. Vanilla gets a flat one, which starts sooner; Harpas generates only
    // normal worlds.
    for (dir, port, level_type) in [
        (&vanilla, vanilla_port, "flat"),
        (&harpas, harpas_port, "normal"),
    ] {
        fs::write(
            dir.join("server.properties"),
            format!(
                "server-ip=127.0.0.1\nserver-port={port}\nonline-mode=false\n\
                 network-compression-threshold=-1\nview-distance=2\nsimulation-distance=2\n\
                 level-type=minecraft:{level_type}\ngenerate-structures=false\n"
            ),
        )
        .unwrap();
    }

    // Harpas takes the tags from the data pack and the IDs of built-in registries from the data generator, which
    // the installation runs with Java.
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
    assert!(
        harpas
            .join(".harpas/vanilla-data/26.1-registries.json")
            .is_file(),
        "Java did not generate the built-in registries"
    );

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
    let mut server = Server::start(OsStr::new(env!("CARGO_BIN_EXE_harpas")), &[], &harpas);
    reference.wait_for("Done (");
    server.wait_for("Done (");
    let expected = capture(vanilla_port);
    let actual = capture(harpas_port);

    // Guard against a vanilla capture that would make an empty Harpas pass: water must be water.
    let water = &expected["minecraft:fluid"]["minecraft:water"];
    assert!(!water.is_empty(), "vanilla sent an empty water tag");
    let count: usize = expected.values().map(BTreeMap::len).sum();
    println!(
        "Comparing {count} tags of {} registries: {:?}",
        expected.len(),
        expected.keys().collect::<Vec<_>>()
    );

    // The comparison notices a single broken tag: a missing entry, entries out of order, a missing tag.
    let mut broken = expected.clone();
    broken
        .get_mut("minecraft:fluid")
        .unwrap()
        .get_mut("minecraft:water")
        .unwrap()
        .pop();
    let mut reordered = expected.clone();
    reordered
        .get_mut("minecraft:fluid")
        .unwrap()
        .get_mut("minecraft:water")
        .unwrap()
        .reverse();
    let mut missing = expected.clone();
    missing
        .get_mut("minecraft:block")
        .unwrap()
        .remove("minecraft:logs");
    for harpas in [broken, reordered, missing] {
        assert_eq!(differences(&expected, &harpas).len(), 1);
    }

    let differences = differences(&expected, &actual);
    assert!(
        differences.is_empty(),
        "{} differences from vanilla:\n{}",
        differences.len(),
        differences.join("\n")
    );
}
