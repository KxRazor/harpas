//! Shared network client and flat-world fixtures for gameplay comparisons.
use super::reference::Server;
use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use harpas_java::{FrameReader, write_frame};
use harpas_protocol_java::codec;
use harpas_protocol_java::v26_1::{PROTOCOL, configuration, handshaking, login, play};
use play::clientbound::Packet;

pub(super) struct Client {
    pub(super) stream: TcpStream,
    frames: FrameReader,
}

impl Client {
    pub(super) fn login(port: u16, name: &str) -> (Self, login::clientbound::Packet) {
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
        let reply = login::clientbound::Packet::decode(&client.next("login reply")).unwrap();
        (client, reply)
    }

    pub(super) fn join(port: u16, name: &str) -> Self {
        Self::join_with_view(port, name, 2)
    }

    pub(super) fn join_with_view(port: u16, name: &str, view: u8) -> Self {
        let (mut client, reply) = Self::login(port, name);
        assert!(
            matches!(reply, login::clientbound::Packet::LoginSuccess(_)),
            "login refused for {name} on port {port}"
        );
        let ack = login::serverbound::Packet::from(login::serverbound::LoginAcknowledged);
        client.send(|buf| ack.encode(buf));
        client.send(|buf| {
            codec::write_varint(buf, 0); // Client Information
            codec::write_string(buf, "en_us");
            buf.extend_from_slice(&[view, 0, 1, 127, 1, 0, 1, 0]);
        });
        loop {
            let bytes = client.next("configuration packet");
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

    pub(super) fn send(&mut self, encode: impl FnOnce(&mut Vec<u8>)) {
        let mut frame = Vec::new();
        write_frame(&mut frame, encode);
        self.stream.write_all(&frame).unwrap();
    }

    pub(super) fn next(&mut self, expected: &str) -> Vec<u8> {
        loop {
            if let Some(frame) = self.frames.next_frame().unwrap() {
                return frame;
            }
            let mut buf = [0; 16 * 1024];
            let count = self
                .stream
                .read(&mut buf)
                .unwrap_or_else(|error| panic!("waiting for {expected}: {error}"));
            assert!(
                count > 0,
                "server disconnected while waiting for {expected}"
            );
            self.frames.push(&buf[..count]);
        }
    }

    pub(super) fn matching<T>(
        &mut self,
        name: &str,
        mut get: impl FnMut(Packet) -> Option<T>,
    ) -> T {
        for _ in 0..1000 {
            let bytes = self.next(name);
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

    pub(super) fn send_command(&mut self, command: &str) {
        let request = if command
            .strip_prefix("ban ")
            .or_else(|| command.strip_prefix("ban-ip "))
            .is_some_and(|args| args.contains(' '))
        {
            play::serverbound::SignedChatCommand {
                command: command.into(),
                timestamp: 0,
                salt: 0,
                argument_signatures: vec![],
                offset: 0,
                acknowledged: [0; 3],
                checksum: 0,
            }
            .into()
        } else {
            play::serverbound::Packet::from(play::serverbound::ChatCommand {
                command: command.into(),
            })
        };
        self.send(|buf| request.encode(buf));
    }
}

pub(super) fn stop(server: &mut Server) {
    writeln!(server.child.stdin.as_mut().unwrap(), "stop").unwrap();
    assert!(server.child.wait().unwrap().success());
}

pub(super) fn play_packet(client: &mut Client, packet: impl Into<play::serverbound::Packet>) {
    let packet = packet.into();
    client.send(|buf| packet.encode(buf));
}

pub(super) fn move_player(client: &mut Client, [x, y, z]: [f64; 3], on_ground: bool) {
    play_packet(
        client,
        play::serverbound::SetPlayerPosition {
            x,
            y,
            z,
            flags: play::MovementFlags {
                on_ground,
                ..Default::default()
            },
        },
    );
}

pub(super) fn assert_no_damage_until_list_reply(client: &mut Client, context: &str) {
    client.send_command("list");
    client.matching("list reply", |packet| match packet {
        Packet::SetHealth(health) => panic!("unexpected health {context}: {health:?}"),
        Packet::DamageEvent(event) => panic!("unexpected damage {context}: {event:?}"),
        Packet::SystemChatMessage(reply)
            if matches!(&reply.content, codec::Nbt::Compound(text)
                if text.get("translate") == Some(&codec::Nbt::from("commands.list.players"))) =>
        {
            Some(())
        }
        _ => None,
    });
}

pub(super) fn attack_speed(client: &mut Client, equipped: bool) -> play::AttributeProperty {
    client.matching("main-hand attack speed", |packet| match packet {
        Packet::UpdateAttributes(packet) => packet.attributes.into_iter().find(|property| {
            property.attribute == play::Attribute::AttackSpeed
                && property.modifiers.is_empty() != equipped
        }),
        _ => None,
    })
}

pub(super) fn flat_survival_servers(
    vanilla: &Path,
    harpas: &Path,
    eula: &str,
    difficulty: &str,
) -> [u16; 2] {
    let sockets = [(); 2].map(|()| TcpListener::bind(("127.0.0.1", 0)).unwrap());
    let [port, other] = sockets
        .each_ref()
        .map(|socket| socket.local_addr().unwrap().port());
    drop(sockets);
    for (dir, port) in [(&vanilla, port), (&harpas, other)] {
        fs::create_dir(dir).unwrap();
        fs::write(dir.join("eula.txt"), eula).unwrap();
        fs::write(
            dir.join("server.properties"),
            format!(
                "server-ip=127.0.0.1\nserver-port={port}\nonline-mode=false\nnetwork-compression-threshold=-1\n\
                 view-distance=2\nsimulation-distance=2\nlevel-type=minecraft:flat\nlevel-seed=42\n\
                 generate-structures=false\nmax-tick-time=-1\nallow-flight=true\ndifficulty={difficulty}\n"
            ),
        )
        .unwrap();
    }
    [port, other]
}

pub(super) fn start_flat_vanilla(jar: &Path, vanilla: &Path, natural_regeneration: bool) -> Server {
    let mut reference = Server::start(
        OsStr::new("java"),
        &[
            OsStr::new("-Xmx1G"),
            OsStr::new("-jar"),
            jar.as_os_str(),
            OsStr::new("nogui"),
        ],
        vanilla,
    );
    reference.wait_for("Done (");
    writeln!(
        reference.child.stdin.as_mut().unwrap(),
        "gamerule minecraft:respawn_radius 0\ngamerule minecraft:spawn_mobs false\n\
         gamerule minecraft:natural_health_regeneration {natural_regeneration}\nsetworldspawn 0 -60 0\nsave-all flush"
    )
    .unwrap();
    reference.wait_for("Saved the game");
    reference
}

pub(super) fn start_flat_harpas(jar: &Path, vanilla: &Path, harpas: &Path) -> Server {
    let binary = env!("CARGO_BIN_EXE_harpas");
    let install = Command::new(binary)
        .current_dir(harpas)
        .args(["vanilla-data", "install", "--from"])
        .arg(jar)
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let settings = "world/data/minecraft/world_gen_settings.dat";
    fs::create_dir_all(harpas.join(settings).parent().unwrap()).unwrap();
    for path in [
        "world/level.dat",
        "world/data/minecraft/game_rules.dat",
        settings,
    ] {
        fs::copy(vanilla.join(path), harpas.join(path)).unwrap();
    }
    let region = "world/dimensions/minecraft/overworld/region";
    fs::create_dir_all(harpas.join(region)).unwrap();
    for entry in fs::read_dir(vanilla.join(region)).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), harpas.join(region).join(entry.file_name())).unwrap();
    }
    let mut server = Server::start(OsStr::new(binary), &[], harpas);
    server.wait_for("Done (");
    server
}
