//! Compare the advertised tree and command replies with Minecraft 26.1 on the network.

mod reference;

use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::time::Duration;

use harpas_java::{FrameReader, offline_uuid, write_frame};
use harpas_protocol_java::codec;
use harpas_protocol_java::v26_1::{PROTOCOL, configuration, handshaking, login, play};
use play::clientbound::{CommandSuggestionsResponse, Packet, SystemChatMessage};
use play::{CommandNodeKind, CommandTree};
use reference::{Folder, Reference, Server};

struct Client {
    stream: TcpStream,
    frames: FrameReader,
}

impl Client {
    fn join(port: u16, name: &str) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        let mut client = Self {
            stream,
            frames: FrameReader::default(),
        };
        let handshake =
            handshaking::serverbound::Packet::from(handshaking::serverbound::Handshake {
                protocol_version: PROTOCOL,
                server_address: "localhost".into(),
                server_port: port,
                intent: handshaking::Intent::Login,
            });
        let start = login::serverbound::Packet::from(login::serverbound::LoginStart {
            name: name.into(),
            player_uuid: 0,
        });
        client.send(|buf| handshake.encode(buf));
        client.send(|buf| start.encode(buf));
        let first = client.next();
        assert!(matches!(
            login::clientbound::Packet::decode(&first),
            Ok(login::clientbound::Packet::LoginSuccess(_))
        ));
        let ack = login::serverbound::Packet::from(login::serverbound::LoginAcknowledged);
        client.send(|buf| ack.encode(buf));
        client.send(|buf| {
            codec::write_varint(buf, 0); // Client Information
            codec::write_string(buf, "en_us");
            buf.extend_from_slice(&[2, 0, 1, 127, 1, 0, 1, 0]);
        });
        loop {
            let bytes = client.next();
            match codec::read_varint(&mut bytes.as_slice()).unwrap() {
                0x0e => {
                    let packs = configuration::serverbound::Packet::from(
                        configuration::serverbound::SelectKnownPacks {
                            packs: vec![configuration::KnownPack {
                                namespace: "minecraft".into(),
                                id: "core".into(),
                                version: "26.1".into(),
                            }],
                        },
                    );
                    client.send(|buf| packs.encode(buf));
                }
                3 => {
                    let finish = configuration::serverbound::Packet::from(
                        configuration::serverbound::AcknowledgeFinishConfiguration,
                    );
                    client.send(|buf| finish.encode(buf));
                    return client;
                }
                _ => {}
            }
        }
    }

    fn send(&mut self, encode: impl FnOnce(&mut Vec<u8>)) {
        let mut frame = Vec::new();
        write_frame(&mut frame, encode);
        self.stream.write_all(&frame).unwrap();
    }

    fn next(&mut self) -> Vec<u8> {
        loop {
            if let Some(frame) = self.frames.next_frame().unwrap() {
                return frame;
            }
            let mut buf = [0; 16 * 1024];
            let count = self
                .stream
                .read(&mut buf)
                .expect("waiting for server packet");
            assert!(count > 0, "server disconnected");
            self.frames.push(&buf[..count]);
        }
    }

    fn matching<T>(&mut self, name: &str, mut get: impl FnMut(Packet) -> Option<T>) -> T {
        for _ in 0..1000 {
            let bytes = self.next();
            match Packet::decode(&bytes) {
                Ok(packet) => {
                    if let Some(value) = get(packet) {
                        return value;
                    }
                }
                Err(error) if bytes[0] == 0x10 => panic!("cannot decode Commands: {error}"),
                Err(_) => {}
            }
        }
        panic!("server did not send {name}");
    }

    fn tree(&mut self) -> CommandTree {
        self.matching("Commands", |packet| match packet {
            Packet::Commands(commands) => Some(commands.tree),
            _ => None,
        })
    }

    fn request_suggestions(&mut self, id: i32, text: &str) {
        let request =
            play::serverbound::Packet::from(play::serverbound::CommandSuggestionsRequest {
                id,
                text: text.into(),
            });
        self.send(|buf| request.encode(buf));
    }

    fn next_suggestions(&mut self) -> CommandSuggestionsResponse {
        self.matching("Command Suggestions Response", |packet| match packet {
            Packet::CommandSuggestionsResponse(response) => Some(response),
            _ => None,
        })
    }

    fn suggestions(&mut self, text: &str) -> CommandSuggestionsResponse {
        self.request_suggestions(13, text);
        self.next_suggestions()
    }

    fn optional_suggestions(&mut self, text: &str) -> Option<CommandSuggestionsResponse> {
        // A known match lets us tell when the server skipped the request without waiting on a timeout.
        self.request_suggestions(12, text);
        let response = self.suggestions("/li");
        if response.id == 12 {
            assert_eq!(self.next_suggestions().id, 13);
            Some(response)
        } else {
            assert_eq!(response.id, 13);
            None
        }
    }

    fn command(&mut self, command: &str, replies: usize) -> Vec<SystemChatMessage> {
        let request = play::serverbound::Packet::from(play::serverbound::ChatCommand {
            command: command.into(),
        });
        self.send(|buf| request.encode(buf));
        (0..replies).map(|_| self.matching("command reply", |packet| match packet {
            Packet::SystemChatMessage(reply) if !matches!(&reply.content,
                codec::Nbt::Compound(text) if text.get("translate") == Some(&"multiplayer.player.joined".into())) => Some(reply),
            _ => None,
        })).collect()
    }
}

fn tree_nodes(tree: &CommandTree) -> Vec<(String, bool, bool)> {
    fn supported(
        tree: &CommandTree,
        parent: i32,
        prefix: &str,
        out: &mut Vec<(String, bool, bool)>,
    ) {
        for &id in &tree.nodes[parent as usize].children {
            let node = &tree.nodes[id as usize];
            if let CommandNodeKind::Literal(name) = &node.kind
                && (!prefix.is_empty() || matches!(name.as_str(), "list" | "seed"))
            {
                let path = format!("{prefix}{name}");
                out.push((path.clone(), node.executable, node.allows_restricted));
                supported(tree, id, &format!("{path} "), out);
            }
        }
    }
    let mut nodes = Vec::new();
    supported(tree, tree.root, "", &mut nodes);
    nodes.sort();
    nodes
}

fn compare(port: u16, other: u16) {
    let mut errors = Vec::new();
    let mut connected = Vec::new();
    for (name, can_seed) in [("Builder", false), ("LowOp", false), ("Op", true)] {
        let mut vanilla = Client::join(port, name);
        let expected = tree_nodes(&vanilla.tree());
        assert_eq!(expected.iter().any(|(name, _, _)| name == "seed"), can_seed);
        let mut harpas = Client::join(other, name);
        let actual = tree_nodes(&harpas.tree());
        if actual != expected {
            errors.push(format!(
                "{name}'s tree: vanilla {expected:?}, Harpas {actual:?}"
            ));
        }
        for text in ["/li", "/list u"]
            .into_iter()
            .chain(can_seed.then_some("/see"))
        {
            let expected = vanilla.suggestions(text);
            let actual = harpas.suggestions(text);
            if actual != expected {
                errors.push(format!(
                    "{name} {text}: vanilla {expected:?}, Harpas {actual:?}"
                ));
            }
        }
        if !can_seed {
            assert!(vanilla.optional_suggestions("/see").is_some_and(|reply| {
                reply
                    .suggestions
                    .iter()
                    .any(|suggestion| suggestion.text == "seed")
            }));
            assert!(harpas.optional_suggestions("/see").is_none());
        }
        for (command, replies) in [
            ("list", 1),
            ("list uuids", 1),
            ("seed", if can_seed { 1 } else { 2 }),
            ("list unexpected", 2),
            ("list ", 2),
        ] {
            let expected = vanilla.command(command, replies);
            let actual = harpas.command(command, replies);
            if actual != expected {
                errors.push(format!(
                    "{name} /{command}: vanilla {expected:?}, Harpas {actual:?}"
                ));
            }
        }
        connected.push((vanilla, harpas));
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn commands_match_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("commands");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let sockets = [(); 2].map(|()| TcpListener::bind(("127.0.0.1", 0)).unwrap());
    let [port, other] = sockets
        .each_ref()
        .map(|socket| socket.local_addr().unwrap().port());
    drop(sockets);
    let ops = serde_json::to_vec(&[("LowOp", 1), ("Op", 2)].map(|(name, level)| {
        serde_json::json!({"name": name, "uuid": uuid(name), "level": level, "bypassesPlayerLimit": false})
    })).unwrap();
    for (dir, port, seed) in [(&vanilla, port, -12345), (&harpas, other, 0)] {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("eula.txt"), &eula).unwrap();
        fs::write(dir.join("ops.json"), &ops).unwrap();
        fs::write(dir.join("server.properties"), format!(
            "server-ip=127.0.0.1\nserver-port={port}\nonline-mode=false\nnetwork-compression-threshold=-1\n\
             view-distance=2\nsimulation-distance=2\nlevel-type=minecraft:flat\nlevel-seed={seed}\n\
             generate-structures=false\nmax-players=12\nmax-tick-time=-1\n"
        )).unwrap();
    }
    let binary = env!("CARGO_BIN_EXE_harpas");
    let install = Command::new(binary)
        .current_dir(&harpas)
        .args(["vanilla-data", "install", "--from"])
        .arg(&jar)
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "could not install vanilla data: {}",
        String::from_utf8_lossy(&install.stderr)
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
    reference.wait_for("Done (");
    writeln!(reference.child.stdin.as_mut().unwrap(), "save-all flush").unwrap();
    reference.wait_for("Saved the game");
    // Use the saved flat world so Harpas does not need to generate terrain for this test.
    let settings = "world/data/minecraft/world_gen_settings.dat";
    fs::create_dir_all(harpas.join(settings).parent().unwrap()).unwrap();
    for path in ["world/level.dat", settings] {
        fs::copy(vanilla.join(path), harpas.join(path)).unwrap();
    }
    let mut server = Server::start(OsStr::new(binary), &[], &harpas);
    server.wait_for("Done (");
    for server in [&mut reference, &mut server] {
        let stdin = server.child.stdin.as_mut().unwrap();
        writeln!(stdin, "list\nseed").unwrap();
        server.wait_for("There are 0 of a max of 12 players online:");
        server.wait_for("Seed: [-12345]");
    }
    compare(port, other);
}

fn uuid(name: &str) -> String {
    let text = format!("{:032x}", offline_uuid(name));
    format!(
        "{}-{}-{}-{}-{}",
        &text[..8],
        &text[8..12],
        &text[12..16],
        &text[16..20],
        &text[20..]
    )
}
