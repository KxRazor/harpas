//! Compare the advertised tree and command replies with Minecraft 26.1 on the network.

mod reference;

use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use harpas_core::nbt::{Compound, List, Nbt};
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
    fn login(port: u16, name: &str) -> (Self, login::clientbound::Packet) {
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

    fn join(port: u16, name: &str) -> Self {
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
            buf.extend_from_slice(&[2, 0, 1, 127, 1, 0, 1, 0]);
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

    fn send(&mut self, encode: impl FnOnce(&mut Vec<u8>)) {
        let mut frame = Vec::new();
        write_frame(&mut frame, encode);
        self.stream.write_all(&frame).unwrap();
    }

    fn next(&mut self, expected: &str) -> Vec<u8> {
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

    fn matching<T>(&mut self, name: &str, mut get: impl FnMut(Packet) -> Option<T>) -> T {
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

    fn tree(&mut self) -> CommandTree {
        self.matching("Commands", |packet| match packet {
            Packet::Commands(commands) => Some(commands.tree),
            _ => None,
        })
    }

    fn initial_game_mode(&mut self, name: &str) -> (play::GameMode, i8, play::AbilityFlags, i32) {
        let uuid = offline_uuid(name);
        let (mode, previous) = self.matching("Play Login", |packet| match packet {
            Packet::Login(login) => Some((login.spawn.game_mode, login.spawn.previous_game_mode)),
            _ => None,
        });
        let mut abilities = None;
        let mut listed_mode = None;
        self.matching("initial abilities and player list entry", |packet| {
            match packet {
                Packet::PlayerAbilities(packet) => {
                    abilities.get_or_insert(packet.flags);
                }
                Packet::PlayerInfoUpdate(packet) if packet.updates.actions.add_player => {
                    assert!(packet.updates.actions.update_game_mode);
                    for entry in packet.updates.entries {
                        if entry.uuid == uuid {
                            listed_mode = Some(entry.game_mode);
                        }
                    }
                }
                Packet::GameEvent(event) if event.event == play::GameEventKind::ChangeGameMode => {
                    panic!("initial game mode was corrected after Play Login");
                }
                _ => {}
            }
            Some((mode, previous, abilities?, listed_mode?))
        })
    }

    fn permission_tree(&mut self, level: i8) -> CommandTree {
        self.matching("operator level", |packet| match packet {
            Packet::EntityEvent(event) if (24..=28).contains(&event.event) => {
                assert_eq!(event.event, 24 + level);
                Some(())
            }
            _ => None,
        });
        self.tree()
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
        self.send_command(command);
        self.replies(replies)
    }

    fn send_command(&mut self, command: &str) {
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

    fn replies(&mut self, replies: usize) -> Vec<SystemChatMessage> {
        (0..replies).map(|_| self.matching("command reply", |packet| match packet {
            Packet::SystemChatMessage(reply) if !matches!(&reply.content,
                codec::Nbt::Compound(text) if matches!(text.get("translate"), Some(codec::Nbt::String(key))
                    if matches!(key.as_str(), "multiplayer.player.joined" | "multiplayer.player.left" | "chat.type.admin"))) => Some(reply),
            Packet::Disconnect(packet) => panic!("disconnected while waiting for a command reply: {:?}", packet.reason),
            _ => None,
        })).collect()
    }

    fn disconnect(&mut self) -> codec::Nbt {
        self.matching("disconnect", |packet| match packet {
            Packet::Disconnect(disconnect) => Some(disconnect.reason),
            _ => None,
        })
    }

    fn whitelist_disconnect(&mut self) {
        assert_eq!(
            self.disconnect(),
            codec::Nbt::Compound(
                [("translate", "multiplayer.disconnect.not_whitelisted")]
                    .into_iter()
                    .collect()
            )
        );
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
            let name = match &node.kind {
                CommandNodeKind::Literal(name)
                    if !prefix.is_empty()
                        || matches!(
                            name.as_str(),
                            "ban"
                                | "ban-ip"
                                | "banlist"
                                | "deop"
                                | "gamemode"
                                | "list"
                                | "op"
                                | "pardon"
                                | "pardon-ip"
                                | "seed"
                                | "whitelist"
                        ) =>
                {
                    name.clone()
                }
                CommandNodeKind::Argument {
                    name,
                    parser,
                    properties,
                    suggestions,
                } => {
                    format!("<{name} {parser} {properties:?} {suggestions:?}>")
                }
                _ => continue,
            };
            let path = format!("{prefix}{name}");
            out.push((path.clone(), node.executable, node.allows_restricted));
            supported(tree, id, &format!("{path} "), out);
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
    for (name, level) in [("Builder", 0), ("LowOp", 1), ("Op", 2), ("Admin", 3)] {
        let can_seed = level >= 2;
        let can_whitelist = level >= 3;
        let mut vanilla = Client::join(port, name);
        let expected = tree_nodes(&vanilla.tree());
        for command in ["seed", "gamemode"] {
            assert_eq!(
                expected.iter().any(|(name, _, _)| name == command),
                can_seed
            );
        }
        assert_eq!(
            expected.iter().any(|(name, _, _)| name == "whitelist"),
            can_whitelist
        );
        for command in [
            "ban",
            "ban-ip",
            "banlist",
            "deop",
            "op",
            "pardon",
            "pardon-ip",
        ] {
            assert_eq!(
                expected.iter().any(|(name, _, _)| name == command),
                can_whitelist
            );
        }
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
            .chain(can_whitelist.then_some("/whitelist "))
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
            ("whitelist list", if can_whitelist { 1 } else { 2 }),
            ("whitelist", 2),
            ("whitelist add", 2),
            ("list unexpected", 2),
            ("list ", 2),
        ]
        .into_iter()
        .chain((!can_whitelist).then_some(("ban Builder", 2)))
        .chain((!can_seed).then_some(("gamemode creative", 2)))
        {
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

    let mut vanilla = Client::join(port, "IpGuest");
    let mut harpas = Client::join(other, "IpGuest");
    for command in ["ban-ip Builder", "pardon-ip 192.0.2.7", "banlist"] {
        assert_eq!(harpas.command(command, 2), vanilla.command(command, 2));
    }
}

fn assert_whitelist_file(dir: &Path, enabled: bool) {
    let properties = fs::read_to_string(dir.join("server.properties")).unwrap();
    let setting = format!("white-list={enabled}");
    assert!(
        properties.lines().any(|line| line == setting),
        "{} did not contain {setting}",
        dir.display()
    );
}

fn assert_login(port: u16, name: &str, allowed: bool) {
    use login::clientbound::Packet::*;
    match Client::login(port, name).1 {
        LoginSuccess(_) if allowed => {}
        LoginDisconnect(packet) if !allowed => assert_eq!(
            packet.reason,
            r#"{"translate":"multiplayer.disconnect.not_whitelisted"}"#
        ),
        packet => panic!("unexpected login reply for {name}: {packet:?}"),
    }
}

fn assert_banned(port: u16, name: &str, reason: Option<&str>) {
    assert_ban(port, name, "multiplayer.disconnect.banned.reason", reason);
}

fn assert_ban(port: u16, name: &str, translation: &str, reason: Option<&str>) {
    let login::clientbound::Packet::LoginDisconnect(packet) = Client::login(port, name).1 else {
        panic!("{name} was allowed to log in");
    };
    let reason = reason.map_or_else(
        || serde_json::json!({"translate": "multiplayer.disconnect.banned.reason.default"}),
        |reason| serde_json::json!(reason),
    );
    assert_eq!(
        packet.reason,
        serde_json::json!({
            "translate": translation, "with": [reason],
        })
        .to_string()
    );
}

fn exercise_ip_bans(server: &mut Server, dir: &Path, port: u16) -> (Bans, codec::Nbt) {
    let mut admin = Client::join(port, "Admin");
    let mut target = Client::join(port, "IpTarget");
    admin.tree();
    target.tree();
    let mut completions = vec![admin.suggestions("/banlist i")];
    completions.extend(admin.optional_suggestions("/ban-ip ipt"));
    let mut replies = admin.command("banlist", 1);
    for command in ["ban-ip ::1", "pardon-ip ::1", "ban-ip 192.0.2.9/24"] {
        replies.extend(admin.command(command, 2));
    }
    for command in [
        "banlist ips",
        "banlist players",
        "ban-ip MissingPlayer",
        "pardon-ip MissingPlayer",
        "pardon-ip 192.0.2.7",
        "ban-ip 192.0.2.7",
        "ban-ip 192.0.2.7",
        "ban-ip 192.0.2.9 Explicit reason",
        "ban IpTarget Player ban",
    ] {
        replies.extend(admin.command(command, 1));
    }
    target.disconnect();
    completions.push(admin.suggestions("/pardon-ip 192.0.2."));
    replies.extend(admin.command("banlist", 4));
    replies.extend(admin.command("banlist ips", 3));
    replies.extend(admin.command("banlist players", 2));
    for command in [
        "pardon IpTarget",
        "pardon-ip 192.0.2.7",
        "pardon-ip 192.0.2.9",
    ] {
        replies.extend(admin.command(command, 1));
    }
    let mut target = Client::join(port, "IpTarget");
    target.tree();
    let reason = "Shared address (by Admin)";
    replies.extend(admin.command("ban-ip iptarget Shared address (by @s)", 2));
    let disconnect = target.disconnect();
    let sender_disconnect = admin.disconnect();
    assert_ban(
        port,
        "NewGuest",
        "multiplayer.disconnect.banned_ip.reason",
        Some(reason),
    );

    let path = dir.join("banned-ips.json");
    let mut saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved[0]["ip"], "127.0.0.1");
    assert_eq!(saved[0]["reason"], reason);
    assert!(
        saved[0]
            .as_object_mut()
            .unwrap()
            .remove("created")
            .unwrap()
            .is_string()
    );
    writeln!(server.child.stdin.as_mut().unwrap(), "pardon-ip 127.0.0.1").unwrap();
    server.wait_for("Unbanned IP 127.0.0.1");
    assert_login(port, "NewGuest", true);
    assert_eq!(fs::read_to_string(path).unwrap(), "[]");
    (
        Bans {
            completions,
            replies,
            disconnect,
            saved,
        },
        sender_disconnect,
    )
}

#[derive(Debug, PartialEq)]
struct Bans {
    completions: Vec<CommandSuggestionsResponse>,
    replies: Vec<SystemChatMessage>,
    disconnect: codec::Nbt,
    saved: serde_json::Value,
}

fn exercise_bans(dir: &Path, port: u16) -> Bans {
    let mut admin = Client::join(port, "Admin");
    let mut target = Client::join(port, "BanTarget");
    admin.tree();
    target.tree();
    let mut completions = vec![admin.suggestions("/ban bant")];
    let reason = "Too many  blocks <&> (by Admin)";
    let mut replies = admin.command("ban @a[name=BanTarget] Too many  blocks <&> (by @s)", 1);
    let disconnect = target.disconnect();
    assert_banned(port, "BanTarget", Some(reason));
    replies.extend(admin.command("ban bantarget", 1));
    completions.push(admin.suggestions("/pardon bant"));
    let path = dir.join("banned-players.json");
    let mut saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved[0]["reason"], reason);
    assert!(
        saved[0]
            .as_object_mut()
            .unwrap()
            .remove("created")
            .unwrap()
            .is_string()
    );
    replies.extend(admin.command("pardon BANTARGET", 1));
    replies.extend(admin.command("pardon BanTarget", 1));
    assert_login(port, "BanTarget", true);
    replies.extend(admin.command("ban bantarget", 1));
    assert_banned(port, "BanTarget", None);
    let default: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(default[0].get("reason").is_none());
    replies.extend(admin.command("pardon bantarget", 1));
    assert_eq!(fs::read_to_string(path).unwrap(), "[]");
    Bans {
        completions,
        replies,
        disconnect,
        saved,
    }
}

fn exercise_whitelist(dir: &Path, port: u16, enforce: bool) -> Vec<SystemChatMessage> {
    let mut visitor = Client::join(port, "Visitor");
    let mut listed = Client::join(port, "Listed");
    let mut admin = Client::join(port, "Admin");
    let mut operator = enforce.then(|| Client::join(port, "LowOp"));
    for client in [&mut visitor, &mut listed, &mut admin] {
        client.tree();
    }
    if let Some(operator) = &mut operator {
        operator.tree();
    }
    let mut replies = admin.command("whitelist on", 1);
    assert_whitelist_file(dir, true);
    assert_login(port, "AnotherVisitor", false);
    assert_login(port, "OtherListed", true);
    if let Some(operator) = &mut operator {
        visitor.whitelist_disconnect();
        operator.whitelist_disconnect();
    } else {
        visitor.command("list", 1);
    }
    assert_login(port, "LowOp", true);
    listed.command("list", 1);
    for command in ["whitelist on", "whitelist off", "whitelist off"] {
        replies.extend(admin.command(command, 1));
    }
    assert_whitelist_file(dir, false);
    assert_login(port, "AnotherVisitor", true);
    if enforce {
        let mut unlisted_admin = Client::join(port, "UnlistedAdmin");
        unlisted_admin.tree();
        replies.extend(unlisted_admin.command("whitelist on", 1));
        unlisted_admin.whitelist_disconnect();
    } else {
        replies.extend(admin.command("whitelist on", 1));
    }
    assert_whitelist_file(dir, true);
    replies
}

fn console_whitelist(server: &mut Server, dir: &Path, enabled: bool) {
    let state = if enabled { "on" } else { "off" };
    writeln!(server.child.stdin.as_mut().unwrap(), "whitelist {state}").unwrap();
    server.wait_for(&format!("Whitelist is now turned {state}"));
    assert_whitelist_file(dir, enabled);
}

struct Membership {
    completions: Vec<CommandSuggestionsResponse>,
    replies: Vec<SystemChatMessage>,
    saved: Vec<String>,
}

fn exercise_membership(server: &mut Server, dir: &Path, port: u16) -> Membership {
    let path = dir.join("whitelist.json");
    let mut admin = Client::join(port, "Admin");
    admin.tree();
    let mut replies = admin.command("whitelist off", 1);
    assert_whitelist_file(dir, false);
    let mut first = Client::join(port, "MemberOne");
    let mut second = Client::join(port, "MemberTwo");
    for client in [&mut first, &mut second] {
        client.tree();
    }
    let mut completions = vec![admin.suggestions("/whitelist add mem")];
    replies.extend(admin.command("whitelist add memberone", 1));
    replies.extend(admin.command("whitelist add MEMBERONE", 1));
    completions.push(admin.suggestions("/whitelist add mem"));
    completions.push(admin.suggestions("/whitelist remove mem"));
    replies.extend(admin.command("whitelist remove membertwo", 1));
    replies.extend(admin.command("whitelist remove memberone", 1));
    first.command("list", 1); // The whitelist is off.
    replies.extend(admin.command("whitelist add @a", 2));
    let mut saved = vec![fs::read_to_string(&path).unwrap()];
    replies.extend(admin.command("whitelist on", 1));
    replies.extend(admin.command("whitelist remove memberone", 1));
    first.whitelist_disconnect();
    assert_login(port, "MemberOne", false);
    assert_login(port, "Listed", true);
    saved.push(fs::read_to_string(&path).unwrap());
    // A reload applies a manual edit, including disconnecting the operator who ran it.
    let edited = serde_json::to_vec(&[serde_json::json!({
        "name": "MemberOne", "uuid": uuid("MemberOne")
    })])
    .unwrap();
    fs::write(&path, &edited).unwrap();
    replies.extend(admin.command("whitelist reload", 1));
    admin.whitelist_disconnect();
    second.whitelist_disconnect();
    assert_login(port, "MemberOne", true);
    assert_login(port, "MemberTwo", false);
    assert_eq!(fs::read(path).unwrap(), edited);
    // The console can resolve a player who only joined before the restart.
    writeln!(
        server.child.stdin.as_mut().unwrap(),
        "whitelist add bUiLdEr"
    )
    .unwrap();
    server.wait_for("Added Builder to the whitelist");
    assert_login(port, "Builder", true);
    saved.push(fs::read_to_string(dir.join("whitelist.json")).unwrap());
    Membership {
        completions,
        replies,
        saved,
    }
}

fn stop(server: &mut Server) {
    writeln!(server.child.stdin.as_mut().unwrap(), "stop").unwrap();
    assert!(server.child.wait().unwrap().success());
}

fn mode_feedback(client: &mut Client, name: &str) -> Vec<Packet> {
    let uuid = offline_uuid(name);
    let mut packets = Vec::new();
    let reply = client.matching("game mode feedback", |packet| {
        match &packet {
            Packet::PlayerAbilities(_) => {}
            Packet::GameEvent(event) if event.event == play::GameEventKind::ChangeGameMode => {}
            Packet::PlayerInfoUpdate(update)
                if update.updates.actions.update_game_mode
                    && !update.updates.actions.add_player
                    && update.updates.entries.iter().any(|entry| entry.uuid == uuid) => {}
            Packet::SystemChatMessage(reply)
                if matches!(&reply.content, codec::Nbt::Compound(text)
                    if matches!(text.get("translate"), Some(codec::Nbt::String(key))
                        if key.starts_with("commands.gamemode.success.") || key == "gameMode.changed")) =>
            {
                return Some(packet);
            }
            _ => return None,
        }
        packets.push(packet);
        None
    });
    packets.push(reply);
    packets
}

fn exercise_game_modes(server: &mut Server, port: u16) -> Vec<Packet> {
    let mut op = Client::join(port, "Op");
    let mut target = Client::join(port, "ModeTarget");
    op.tree();
    target.tree();
    let mut packets = Vec::new();
    for query in [
        "/gamemode ",
        "/gamemode creative Mo",
        "/gamemode creative @",
    ] {
        packets.push(op.suggestions(query).into());
    }
    for (command, replies) in [
        ("gamemode Creative", 2),
        ("gamemode creative Nobody", 1),
        ("gamemode creative @e", 2),
    ] {
        packets.extend(op.command(command, replies).into_iter().map(Packet::from));
    }
    let uuid_target = format!("gamemode creative {}", uuid("ModeTarget"));
    packets.extend(op.command(&uuid_target, 2).into_iter().map(Packet::from));

    op.send_command("gamemode creative");
    packets.extend(mode_feedback(&mut op, "Op"));
    let fly = play::serverbound::Packet::from(play::serverbound::PlayerAbilities {
        flags: play::AbilityFlags {
            flying: true,
            ..Default::default()
        },
    });
    op.send(|buf| fly.encode(buf));
    op.send_command("gamemode survival");
    packets.extend(mode_feedback(&mut op, "Op"));
    op.send_command("gamemode survival");
    let [list] = op.command("list", 1).try_into().unwrap();
    assert!(matches!(&list.content, codec::Nbt::Compound(text)
        if matches!(text.get("translate"), Some(codec::Nbt::String(key)) if key == "commands.list.players")));

    for command in [
        "gamemode adventure @s",
        "gamemode spectator ModeTarget",
        "gamemode creative @a[name=ModeTarget]",
    ] {
        op.send_command(command);
        let self_target = command.ends_with("@s");
        let name = if self_target { "Op" } else { "ModeTarget" };
        packets.extend(mode_feedback(&mut op, name));
        if !self_target {
            packets.extend(mode_feedback(&mut target, name));
        }
    }
    writeln!(
        server.child.stdin.as_mut().unwrap(),
        "gamemode survival ModeTarget"
    )
    .unwrap();
    server.wait_for("Set ModeTarget's game mode to Survival Mode");
    packets.extend(mode_feedback(&mut target, "ModeTarget"));
    writeln!(server.child.stdin.as_mut().unwrap(), "gamemode creative").unwrap();
    server.wait_for("A player is required to run this command here");
    packets
}

fn reconnect_in_spectator(
    server: &mut Server,
    port: u16,
) -> (play::GameMode, i8, play::AbilityFlags, i32) {
    let mut client = Client::join(port, "ModeTarget");
    client.tree();
    writeln!(
        server.child.stdin.as_mut().unwrap(),
        "gamemode spectator ModeTarget"
    )
    .unwrap();
    server.wait_for("Set ModeTarget's game mode to Spectator Mode");
    mode_feedback(&mut client, "ModeTarget");
    client.stream.shutdown(Shutdown::Both).unwrap();
    Client::join(port, "ModeTarget").initial_game_mode("ModeTarget")
}

#[derive(Debug, PartialEq)]
struct Operators {
    completions: Vec<CommandSuggestionsResponse>,
    replies: Vec<SystemChatMessage>,
    trees: Vec<Vec<(String, bool, bool)>>,
    saved: Vec<String>,
}

fn exercise_operators(server: &mut Server, dir: &Path, port: u16) -> Operators {
    let path = dir.join("ops.json");
    let original = fs::read_to_string(&path).unwrap();
    let mut admin = Client::join(port, "Admin");
    let mut operator = Client::join(port, "NewOperator");
    admin.tree();
    let initial = tree_nodes(&operator.tree());
    let mut replies = operator.command("op NewOperator", 2);
    let mut completions = vec![admin.suggestions("/op New")];

    replies.extend(admin.command("op @a[name=NewOperator]", 1));
    let promoted = tree_nodes(&operator.permission_tree(3));
    assert!(promoted.iter().any(|(name, _, _)| name == "seed"));
    replies.extend(operator.command("seed", 1));
    let mut saved = vec![fs::read_to_string(&path).unwrap()];
    assert!(saved[0].contains("\"name\": \"NewOperator\""));
    completions.push(admin.suggestions("/deop New"));
    replies.extend(admin.command("op NewOperator", 1));

    operator.send_command("deop NewOperator");
    let demoted = tree_nodes(&operator.permission_tree(0));
    assert_eq!(demoted, initial);
    replies.extend(operator.replies(1));
    replies.extend(operator.command("seed", 2));
    replies.extend(admin.command("deop NewOperator", 1));
    assert_eq!(fs::read_to_string(&path).unwrap(), original);

    writeln!(server.child.stdin.as_mut().unwrap(), "op NewOperator").unwrap();
    server.wait_for("Made NewOperator a server operator");
    assert_eq!(tree_nodes(&operator.permission_tree(3)), promoted);
    saved.push(fs::read_to_string(&path).unwrap());
    Operators {
        completions,
        replies,
        trees: vec![promoted, demoted],
        saved,
    }
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
    let ops = serde_json::to_vec(&[("LowOp", 1), ("Op", 2), ("Admin", 3), ("UnlistedAdmin", 3)].map(|(name, level)| {
        serde_json::json!({"name": name, "uuid": uuid(name), "level": level, "bypassesPlayerLimit": false})
    })).unwrap();
    let whitelist = serde_json::to_vec(
        &["Listed", "OtherListed", "Admin"]
            .map(|name| serde_json::json!({"name": name, "uuid": uuid(name)})),
    )
    .unwrap();
    for (dir, port, seed) in [(&vanilla, port, -12345), (&harpas, other, 0)] {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("eula.txt"), &eula).unwrap();
        fs::write(dir.join("ops.json"), &ops).unwrap();
        fs::write(dir.join("whitelist.json"), &whitelist).unwrap();
        fs::write(dir.join("server.properties"), format!(
            "server-ip=127.0.0.1\nserver-port={port}\nonline-mode=false\nnetwork-compression-threshold=-1\n\
             view-distance=2\nsimulation-distance=2\nlevel-type=minecraft:flat\nlevel-seed={seed}\n\
             generate-structures=false\nmax-players=12\nmax-tick-time=-1\nallow-flight=true\nop-permission-level=3\n"
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
    let start_vanilla = || {
        Server::start(
            OsStr::new("java"),
            &[
                OsStr::new("-Xmx1G"),
                OsStr::new("-jar"),
                jar.as_os_str(),
                OsStr::new("nogui"),
            ],
            &vanilla,
        )
    };
    let start_harpas = || Server::start(OsStr::new(binary), &[], &harpas);
    let mut reference = start_vanilla();
    reference.wait_for("Done (");
    writeln!(reference.child.stdin.as_mut().unwrap(), "save-all flush").unwrap();
    reference.wait_for("Saved the game");
    // Use the saved flat world so Harpas does not need to generate terrain for this test.
    let settings = "world/data/minecraft/world_gen_settings.dat";
    fs::create_dir_all(harpas.join(settings).parent().unwrap()).unwrap();
    for path in ["world/level.dat", settings] {
        fs::copy(vanilla.join(path), harpas.join(path)).unwrap();
    }
    let mut server = start_harpas();
    server.wait_for("Done (");
    for server in [&mut reference, &mut server] {
        let stdin = server.child.stdin.as_mut().unwrap();
        writeln!(stdin, "list\nseed").unwrap();
        server.wait_for("There are 0 of a max of 12 players online:");
        server.wait_for("Seed: [-12345]");
        writeln!(server.child.stdin.as_mut().unwrap(), "whitelist list").unwrap();
        server.wait_for("There are 3 whitelisted player(s):");
    }
    compare(port, other);
    let expected_modes = exercise_game_modes(&mut reference, port);
    let actual_modes = exercise_game_modes(&mut server, other);
    assert_eq!(actual_modes.len(), expected_modes.len());
    for (index, (actual, expected)) in actual_modes.iter().zip(&expected_modes).enumerate() {
        assert_eq!(actual, expected, "game mode packet {index}");
    }
    let expected_rejoin = reconnect_in_spectator(&mut reference, port);
    let actual_rejoin = reconnect_in_spectator(&mut server, other);
    assert_eq!(expected_rejoin.0, play::GameMode::Spectator);
    assert_eq!(actual_rejoin, expected_rejoin);
    assert_eq!(
        exercise_operators(&mut reference, &vanilla, port),
        exercise_operators(&mut server, &harpas, other)
    );
    let expected = exercise_bans(&vanilla, port);
    let actual = exercise_bans(&harpas, other);
    assert_eq!(actual.completions, expected.completions);
    assert_eq!(actual.replies, expected.replies);
    assert_eq!(actual.disconnect, expected.disconnect);
    assert_eq!(actual.saved, expected.saved);

    let (expected, expected_sender_disconnect) = exercise_ip_bans(&mut reference, &vanilla, port);
    let (actual, actual_sender_disconnect) = exercise_ip_bans(&mut server, &harpas, other);
    assert_eq!(actual.completions, expected.completions);
    assert_eq!(actual.disconnect, expected.disconnect);
    assert_eq!(actual_sender_disconnect, expected_sender_disconnect);
    assert_eq!(actual.replies.len(), expected.replies.len());
    for (index, (actual, expected)) in actual.replies.iter().zip(&expected.replies).enumerate() {
        assert_eq!(actual, expected, "IP ban scenario reply {index}");
    }
    assert_eq!(actual.saved, expected.saved);

    for (server, port) in [(&mut reference, port), (&mut server, other)] {
        writeln!(server.child.stdin.as_mut().unwrap(), "ban persistentguest").unwrap();
        server.wait_for("Banned persistentguest: Banned by an operator.");
        assert_banned(port, "persistentguest", None);
    }
    assert_eq!(
        exercise_whitelist(&vanilla, port, false),
        exercise_whitelist(&harpas, other, false),
    );

    // Keep the whitelist enabled across the restart and exercise enforcement on connected players.
    for (server, dir) in [(&mut reference, &vanilla), (&mut server, &harpas)] {
        writeln!(server.child.stdin.as_mut().unwrap(), "ban-ip 127.0.0.1").unwrap();
        server.wait_for("Banned IP 127.0.0.1: Banned by an operator.");
        writeln!(server.child.stdin.as_mut().unwrap(), "banlist ips").unwrap();
        server.wait_for("127.0.0.1 was banned by Server: Banned by an operator.");
        stop(server);
        let path = dir.join("server.properties");
        let properties = fs::read_to_string(&path).unwrap();
        assert!(properties.contains("enforce-whitelist=false"));
        fs::write(
            path,
            properties
                .replace("enforce-whitelist=false", "enforce-whitelist=true")
                .replace("gamemode=survival", "gamemode=adventure"),
        )
        .unwrap();
        assert_eq!(fs::read(dir.join("whitelist.json")).unwrap(), whitelist);
    }
    let mut reference = start_vanilla();
    let mut server = start_harpas();
    for (server, dir, port) in [
        (&mut reference, &vanilla, port),
        (&mut server, &harpas, other),
    ] {
        server.wait_for("Done (");
        assert_ban(
            port,
            "Admin",
            "multiplayer.disconnect.banned_ip.reason",
            None,
        );
        writeln!(server.child.stdin.as_mut().unwrap(), "pardon-ip 127.0.0.1").unwrap();
        server.wait_for("Unbanned IP 127.0.0.1");
        let mut operator = Client::join(port, "NewOperator");
        let nodes = tree_nodes(&operator.tree());
        assert!(nodes.iter().any(|(name, _, _)| name == "deop"));
        writeln!(server.child.stdin.as_mut().unwrap(), "deop NewOperator").unwrap();
        server.wait_for("Made NewOperator no longer a server operator");
        operator.permission_tree(0);
        operator.whitelist_disconnect();
        assert_login(port, "NewOperator", false);
        assert_login(port, "Visitor", false);
        console_whitelist(server, dir, false);
        console_whitelist(server, dir, true);
        console_whitelist(server, dir, false);
        assert_banned(port, "persistentguest", None);
        writeln!(
            server.child.stdin.as_mut().unwrap(),
            "pardon persistentguest"
        )
        .unwrap();
        server.wait_for("Unbanned persistentguest");
        assert_login(port, "persistentguest", true);
    }
    assert_eq!(
        exercise_whitelist(&vanilla, port, true),
        exercise_whitelist(&harpas, other, true),
    );
    let expected = exercise_membership(&mut reference, &vanilla, port);
    let actual = exercise_membership(&mut server, &harpas, other);
    assert_eq!(actual.completions, expected.completions);
    assert_eq!(actual.replies, expected.replies);
    assert_eq!(actual.saved, expected.saved);

    for (server, dir) in [(&mut reference, &vanilla), (&mut server, &harpas)] {
        console_whitelist(server, dir, false);
    }
    let expected_restarted = Client::join(port, "ModeTarget").initial_game_mode("ModeTarget");
    let actual_restarted = Client::join(other, "ModeTarget").initial_game_mode("ModeTarget");
    assert_eq!(expected_restarted, expected_rejoin);
    assert_eq!(actual_restarted, expected_restarted);
    let first_join = "DefaultMode";
    let expected_default = Client::join(port, first_join).initial_game_mode(first_join);
    assert_eq!(expected_default.0, play::GameMode::Adventure);
    assert_eq!(
        Client::join(other, first_join).initial_game_mode(first_join),
        expected_default
    );

    for (server, dir) in [(&mut reference, &vanilla), (&mut server, &harpas)] {
        stop(server);
        let path = dir.join("server.properties");
        let properties = fs::read_to_string(&path).unwrap();
        assert!(properties.contains("force-gamemode=false"));
        fs::write(
            path,
            properties
                .replace("force-gamemode=false", "force-gamemode=true")
                .replace("gamemode=adventure", "gamemode=creative"),
        )
        .unwrap();
    }
    let saved_modes = |dir: &Path, name| {
        let (root, _) = harpas_anvil::read_player(&dir.join("world"), offline_uuid(name))
            .unwrap()
            .unwrap();
        ["playerGameType", "previousPlayerGameType"].map(|key| root.get(key).cloned())
    };
    assert_eq!(
        saved_modes(&harpas, "ModeTarget"),
        saved_modes(&vanilla, "ModeTarget")
    );

    let mut reference = start_vanilla();
    let mut server = start_harpas();
    reference.wait_for("Done (");
    server.wait_for("Done (");
    for name in ["ModeTarget", "FirstJoin"] {
        let expected = Client::join(port, name).initial_game_mode(name);
        assert_eq!(expected.0, play::GameMode::Creative);
        assert_eq!(Client::join(other, name).initial_game_mode(name), expected);
    }
    stop(&mut reference);
    stop(&mut server);
    for name in ["ModeTarget", "FirstJoin"] {
        assert_eq!(saved_modes(&harpas, name), saved_modes(&vanilla, name));
    }
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

fn play_packet(client: &mut Client, packet: impl Into<play::serverbound::Packet>) {
    let packet = packet.into();
    client.send(|buf| packet.encode(buf));
}

fn confirm_position(client: &mut Client) -> [f64; 3] {
    let position = client.matching("position synchronization", |packet| match packet {
        Packet::SynchronizePlayerPosition(position) => Some(position),
        _ => None,
    });
    play_packet(
        client,
        play::serverbound::ConfirmTeleportation {
            teleport_id: position.teleport_id,
        },
    );
    play_packet(client, play::serverbound::PlayerLoaded);
    [position.x, position.y, position.z]
}

fn move_player(client: &mut Client, [x, y, z]: [f64; 3], on_ground: bool) {
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

fn fall(client: &mut Client, [x, ground, z]: [f64; 3], height: i32) {
    for y in 1..=height {
        move_player(client, [x, ground + f64::from(y), z], false);
        std::thread::sleep(Duration::from_millis(50));
    }
    for y in (1..height).rev() {
        move_player(client, [x, ground + f64::from(y), z], false);
        std::thread::sleep(Duration::from_millis(50));
    }
    move_player(client, [x, ground, z], true);
    std::thread::sleep(Duration::from_millis(100));
}

fn fall_without_damage(client: &mut Client, ground: [f64; 3], height: i32) {
    fall(client, ground, height);
    assert_no_damage_until_list_reply(client, "after landing");
}

fn assert_no_damage_until_list_reply(client: &mut Client, context: &str) {
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

fn health(client: &mut Client) -> play::clientbound::SetHealth {
    client.matching("health", |packet| match packet {
        Packet::SetHealth(health) => Some(health),
        _ => None,
    })
}

fn exercise_falls(
    server: &mut Server,
    port: u16,
) -> (
    [play::clientbound::SetHealth; 4],
    play::clientbound::Respawn,
    harpas_protocol_java::codec::Nbt,
) {
    let mut client = Client::join(port, "Falling");
    client.stream.set_nodelay(true).unwrap();
    let spawn = confirm_position(&mut client);
    let initial = health(&mut client);
    fall_without_damage(&mut client, spawn, 3);
    fall(&mut client, spawn, 7);
    client.matching("fall damage source", |packet| match packet {
        Packet::DamageEvent(event) => {
            let types = harpas_java::registry_names("minecraft:damage_type").unwrap();
            assert_eq!(types[event.source_type as usize], "minecraft:fall");
            Some(())
        }
        _ => None,
    });
    let injured = health(&mut client);
    drop(client);
    server.wait_for("Falling left the game");
    let mut client = Client::join(port, "Falling");
    client.stream.set_nodelay(true).unwrap();
    assert_eq!(confirm_position(&mut client), spawn);
    assert_eq!(health(&mut client), injured);
    let [x, y, z] = spawn;
    let landing = [x + 2.0, y, z];
    move_player(&mut client, landing, true);
    fall(&mut client, landing, 28);
    let death = client.matching("CombatDeath", |packet| match packet {
        Packet::CombatDeath(death) => Some(death),
        _ => None,
    });
    let dead = health(&mut client);
    play_packet(
        &mut client,
        play::serverbound::ClientStatus {
            action: play::ClientStatusAction::PerformRespawn,
        },
    );
    let respawn = client.matching("Respawn", |packet| match packet {
        Packet::Respawn(respawn) => Some(respawn),
        _ => None,
    });
    assert_ne!(
        respawn.spawn.death_location.as_ref().unwrap().position.x as f64,
        x.floor()
    );
    assert_eq!(confirm_position(&mut client), spawn);
    let restored = health(&mut client);
    writeln!(
        server.child.stdin.as_mut().unwrap(),
        "gamemode creative Falling"
    )
    .unwrap();
    client.matching("Creative mode", |packet| match packet {
        Packet::GameEvent(event) if event.event == play::GameEventKind::ChangeGameMode => Some(()),
        _ => None,
    });
    fall_without_damage(&mut client, spawn, 28);
    ([initial, injured, dead, restored], respawn, death.message)
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn fall_damage_and_respawn_match_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("fall-damage");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    let expected = exercise_falls(&mut reference, port);
    assert_eq!(
        expected.0.each_ref().map(|health| health.health),
        [20.0, 16.0, 0.0, 20.0]
    );
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    assert_eq!(exercise_falls(&mut server, other), expected);
}

fn flat_survival_servers(vanilla: &Path, harpas: &Path, eula: &str, difficulty: &str) -> [u16; 2] {
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

fn start_flat_vanilla(jar: &Path, vanilla: &Path, natural_regeneration: bool) -> Server {
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

fn start_flat_harpas(jar: &Path, vanilla: &Path, harpas: &Path) -> Server {
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

fn use_food(client: &mut Client, hand: play::Hand, sequence: i32) {
    play_packet(
        client,
        play::serverbound::UseItem {
            hand,
            sequence,
            rotation: play::Vec2f { x: 0.0, y: 0.0 },
        },
    );
    client.matching("item use acknowledgement", |packet| match packet {
        Packet::AcknowledgeBlockChange(ack) if ack.sequence == sequence => Some(()),
        _ => None,
    });
}

fn cancel_food(client: &mut Client, entity_id: i32) {
    play_packet(
        client,
        play::serverbound::PlayerAction {
            status: play::PlayerActionStatus::FinishUsingItem,
            position: play::BlockPos { x: 0, y: 0, z: 0 },
            face: 0,
            sequence: 0,
        },
    );
    // Releasing use neither changes food nor finishes the action, even after a full use duration.
    std::thread::sleep(Duration::from_millis(1800));
    client.send_command("list");
    client.matching(
        "list after releasing the use button",
        |packet| match packet {
            Packet::SetHealth(health) => panic!("cancelled food use changed health: {health:?}"),
            Packet::EntityEvent(event) if event.entity_id == entity_id && event.event == 9 => {
                panic!("cancelled food use finished")
            }
            Packet::SystemChatMessage(reply)
                if matches!(&reply.content, codec::Nbt::Compound(text)
                if text.get("translate") == Some(&codec::Nbt::from("commands.list.players"))) =>
            {
                Some(())
            }
            _ => None,
        },
    );
}

fn exercise_food(server: &mut Server, port: u16) -> Vec<play::clientbound::SetHealth> {
    let mut client = Client::join(port, "Runner");
    client.stream.set_nodelay(true).unwrap();
    let entity_id = client.matching("Play Login", |packet| match packet {
        Packet::Login(login) => Some(login.entity_id),
        _ => None,
    });
    let [x, y, z] = confirm_position(&mut client);
    let initial = health(&mut client);
    assert_eq!(initial.food, 17);
    move_player(&mut client, [x + 2.0, y, z], true);
    play_packet(
        &mut client,
        play::serverbound::PlayerCommand {
            entity_id,
            action: play::PlayerCommandAction::StartSprinting,
            data: 0,
        },
    );
    move_player(&mut client, [x, y, z], true);
    let after_sprint = health(&mut client);
    assert_eq!(after_sprint.food, 16);
    play_packet(
        &mut client,
        play::serverbound::PlayerCommand {
            entity_id,
            action: play::PlayerCommandAction::StopSprinting,
            data: 0,
        },
    );
    use_food(&mut client, play::Hand::OffHand, 1);
    cancel_food(&mut client, entity_id);
    use_food(&mut client, play::Hand::OffHand, 2);
    let mut finished = false;
    let after_apple = client.matching(
        "completed apple use and food update",
        |packet| match packet {
            Packet::EntityEvent(event) if event.entity_id == entity_id && event.event == 9 => {
                finished = true;
                None
            }
            Packet::SetHealth(health) => {
                assert!(
                    finished,
                    "food changed before the client was told item use finished"
                );
                Some(health)
            }
            _ => None,
        },
    );
    assert_eq!(after_apple.food, 20);
    assert_eq!(after_apple.saturation, 2.4);
    drop(client);
    server.wait_for("Runner left the game");
    let mut client = Client::join(port, "Runner");
    confirm_position(&mut client);
    assert_eq!(health(&mut client), after_apple);
    drop(client);

    let mut fast = Client::join(port, "FastHealing");
    confirm_position(&mut fast);
    let fast_health = std::array::from_fn::<_, 3, _>(|_| health(&mut fast));
    assert_eq!(
        fast_health.each_ref().map(|health| health.health),
        [16.0, 17.0, 17.0 + 5.0 / 6.0]
    );
    drop(fast);
    let mut slow = Client::join(port, "SlowHealing");
    confirm_position(&mut slow);
    let slow_health = [health(&mut slow), health(&mut slow)];
    assert_eq!(
        slow_health.each_ref().map(|health| health.health),
        [19.0, 20.0]
    );
    [initial, after_sprint, after_apple]
        .into_iter()
        .chain(fast_health)
        .chain(slow_health)
        .collect()
}

struct FoodCase {
    name: &'static str,
    item: &'static str,
    hand: play::Hand,
    ticks: u32,
    remainder: Option<&'static str>,
}

const FOOD_CASES: [FoodCase; 6] = [
    FoodCase {
        name: "BreadEater",
        item: "minecraft:bread",
        hand: play::Hand::MainHand,
        ticks: 32,
        remainder: None,
    },
    FoodCase {
        name: "SteakEater",
        item: "minecraft:cooked_beef",
        hand: play::Hand::OffHand,
        ticks: 32,
        remainder: None,
    },
    FoodCase {
        name: "KelpEater",
        item: "minecraft:dried_kelp",
        hand: play::Hand::MainHand,
        ticks: 16,
        remainder: None,
    },
    FoodCase {
        name: "StewEater",
        item: "minecraft:mushroom_stew",
        hand: play::Hand::MainHand,
        ticks: 32,
        remainder: Some("minecraft:bowl"),
    },
    FoodCase {
        name: "SoupEater",
        item: "minecraft:beetroot_soup",
        hand: play::Hand::OffHand,
        ticks: 32,
        remainder: Some("minecraft:bowl"),
    },
    FoodCase {
        name: "RabbitEater",
        item: "minecraft:rabbit_stew",
        hand: play::Hand::MainHand,
        ticks: 32,
        remainder: Some("minecraft:bowl"),
    },
];

fn exercise_food_defaults(server: &mut Server, port: u16) -> Vec<play::clientbound::SetHealth> {
    FOOD_CASES
        .into_iter()
        .map(|case| {
            let FoodCase {
                name,
                item,
                hand,
                ticks,
                remainder,
            } = case;
            let mut client = Client::join(port, name);
            client.stream.set_nodelay(true).unwrap();
            let entity_id = client.matching("Play Login", |packet| match packet {
                Packet::Login(login) => Some(login.entity_id),
                _ => None,
            });
            confirm_position(&mut client);
            assert_eq!(health(&mut client).food, 16);
            let sequence = if remainder.is_some() && hand == play::Hand::OffHand {
                use_food(&mut client, hand, 1);
                cancel_food(&mut client, entity_id);
                2
            } else {
                1
            };
            let slot = if hand == play::Hand::MainHand { 36 } else { 45 };
            let expected_item = harpas_core::item::items()
                .id(remainder.unwrap_or(item))
                .unwrap();
            let (mut updated_health, mut delivered_remainder) = (None, remainder.is_none());
            let started = Instant::now();
            use_food(&mut client, hand, sequence);
            let after = client.matching("food update and any returned container", |packet| {
                match packet {
                    Packet::SetHealth(health) => updated_health = Some(health),
                    Packet::SetContainerSlot(change)
                        if change.window_id == 0
                            && change.slot == slot
                            && change.item.item as u32 == expected_item
                            && change.item.count == 1 =>
                    {
                        delivered_remainder = true;
                    }
                    _ => {}
                }
                if delivered_remainder {
                    updated_health.take()
                } else {
                    None
                }
            });
            let elapsed = started.elapsed();
            let expected = Duration::from_millis(u64::from(ticks) * 50);
            // The exact ticks are checked in the engine. Allow for network and scheduling delays here.
            assert!(
                elapsed.abs_diff(expected) < Duration::from_millis(400),
                "{item} took {elapsed:?}, expected about {expected:?}"
            );
            drop(client);
            server.wait_for(&format!("{name} left the game"));

            let (mut saved_health, mut saved_stack) = (None, None);
            Client::join(port, name).matching("food and inventory after reconnect", |packet| {
                match packet {
                    Packet::SetHealth(health) => saved_health = Some(health),
                    Packet::SetContainerContent(contents) if contents.window_id == 0 => {
                        saved_stack = Some(contents.slots[slot as usize].clone());
                    }
                    _ => {}
                }
                (saved_health.is_some() && saved_stack.is_some()).then_some(())
            });
            assert_eq!(saved_health.unwrap(), after);
            let stack = saved_stack.unwrap();
            assert_eq!(stack.item as u32, expected_item);
            assert_eq!(stack.count, 1, "inventory after eating {item}");
            after
        })
        .collect()
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn food_and_natural_regeneration_match_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("food");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, true);
    for dir in [&vanilla, &harpas] {
        let world = dir.join("world");
        for (name, health, food, saturation, exhaustion) in [
            ("Runner", 20.0_f32, 17, 0.0_f32, 3.9_f32),
            ("FastHealing", 16.0, 20, 6.0, 0.0),
            ("SlowHealing", 19.0, 18, 0.0, 0.0),
        ] {
            let mut data = Compound::new();
            data.insert("DataVersion", harpas_anvil::DATA_VERSION);
            data.insert("Health", health);
            data.insert("foodLevel", food);
            data.insert("foodSaturationLevel", saturation);
            data.insert("foodExhaustionLevel", exhaustion);
            data.insert("foodTickTimer", 0_i32);
            if name == "Runner" {
                let apple = Compound::from_iter([
                    ("id", Nbt::from("minecraft:apple")),
                    ("count", Nbt::Int(2)),
                ]);
                data.insert("equipment", Compound::from_iter([("offhand", apple)]));
            }
            harpas_anvil::write_player(&world, offline_uuid(name), &data).unwrap();
        }
        for FoodCase {
            name,
            item,
            hand,
            remainder,
            ..
        } in FOOD_CASES
        {
            let mut data = Compound::from_iter([
                ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
                ("Health", Nbt::Float(20.0)),
                ("foodLevel", Nbt::Int(16)),
                ("foodSaturationLevel", Nbt::Float(10.0)),
            ]);
            let count = if remainder.is_some() { 1 } else { 2 };
            let mut stack =
                Compound::from_iter([("id", Nbt::from(item)), ("count", Nbt::Int(count))]);
            if hand == play::Hand::MainHand {
                stack.insert("Slot", 0_i8);
                data.insert(
                    "Inventory",
                    Nbt::List(harpas_core::nbt::List::Compound(vec![stack])),
                );
            } else {
                data.insert("equipment", Compound::from_iter([("offhand", stack)]));
            }
            harpas_anvil::write_player(&world, offline_uuid(name), &data).unwrap();
        }
    }
    let expected = exercise_food(&mut reference, port);
    let expected_foods = exercise_food_defaults(&mut reference, port);
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    assert_eq!(exercise_food(&mut server, other), expected);
    assert_eq!(exercise_food_defaults(&mut server, other), expected_foods);
    stop(&mut server);
    let [expected, actual] = [&vanilla, &harpas].map(|dir| {
        harpas_anvil::read_player(&dir.join("world"), offline_uuid("Runner"))
            .unwrap()
            .unwrap()
            .0
    });
    for field in [
        "foodLevel",
        "foodSaturationLevel",
        "foodExhaustionLevel",
        "foodTickTimer",
    ] {
        assert_eq!(actual.get(field), expected.get(field), "{field}");
    }
    let apple_count = |data: &Compound| -> Nbt {
        let Some(Nbt::Compound(equipment)) = data.get("equipment") else {
            panic!("missing equipment")
        };
        let Some(Nbt::Compound(apple)) = equipment.get("offhand") else {
            panic!("missing offhand")
        };
        apple.get("count").cloned().unwrap_or(Nbt::Int(1))
    };
    assert_eq!(apple_count(&actual), apple_count(&expected));
    assert_eq!(apple_count(&actual), Nbt::Int(1));
}

fn hunger_packet(client: &mut Client) -> play::clientbound::EntityEffect {
    client.matching("Hunger effect", |packet| match packet {
        Packet::EntityEffect(effect) => Some(effect),
        _ => None,
    })
}

fn exercise_hunger(server: &mut Server, port: u16) -> play::clientbound::EntityEffect {
    for name in ["FleshEater", "FleshEater2", "FleshEater3"] {
        let mut client = Client::join(port, name);
        let entity_id = client.matching("Play Login", |packet| match packet {
            Packet::Login(login) => Some(login.entity_id),
            _ => None,
        });
        confirm_position(&mut client);
        assert_eq!(health(&mut client).food, 1);
        use_food(&mut client, play::Hand::OffHand, 1);
        cancel_food(&mut client, entity_id);
        for attempt in 1..=5 {
            let mut effect = None;
            use_food(&mut client, play::Hand::OffHand, attempt + 1);
            let fed = client.matching("eating rotten flesh", |packet| match packet {
                Packet::EntityEffect(applied) => {
                    effect = Some(applied);
                    None
                }
                Packet::SetHealth(health) if health.food == (1 + 4 * attempt).min(20) => {
                    Some(health)
                }
                _ => None,
            });
            if let Some(mut effect) = effect {
                assert_eq!(effect.entity_id, entity_id);
                assert_eq!(effect.duration, 600);
                assert_eq!(effect.amplifier, 0);
                assert!(effect.flags.show_icon);
                assert!(effect.flags.blend);
                let expected_saturation = 0.8 * attempt as f32;
                assert!(
                    (fed.saturation - expected_saturation).abs() < 0.000001,
                    "{fed:?}"
                );
                if attempt == 1 {
                    assert_eq!(health(&mut client).saturation, 0.0);
                } else {
                    // Leave enough time for the saved duration to advance after applying the effect.
                    std::thread::sleep(Duration::from_millis(150));
                }
                drop(client);
                server.wait_for(&format!("{name} left the game"));
                let mut client = Client::join(port, name);
                confirm_position(&mut client);
                let resumed = hunger_packet(&mut client);
                assert_eq!(resumed.effect, effect.effect);
                assert_eq!(
                    resumed.flags,
                    play::EffectFlags {
                        blend: false,
                        ..effect.flags
                    }
                );
                assert!((500..600).contains(&resumed.duration), "{resumed:?}");
                effect.entity_id = 0;
                return effect;
            }
        }
    }
    panic!("no Hunger effect in fifteen completed uses");
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn food_hunger_effect_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("food-hunger");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    let expiring = offline_uuid("Expiring");
    for dir in [&vanilla, &harpas] {
        let world = dir.join("world");
        let flesh = Compound::from_iter([
            ("id", Nbt::from("minecraft:rotten_flesh")),
            ("count", Nbt::Int(5)),
        ]);
        let data = Compound::from_iter([
            ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
            ("foodLevel", Nbt::Int(1)),
            ("foodSaturationLevel", Nbt::Float(0.0)),
            ("foodExhaustionLevel", Nbt::Float(3.999)),
            (
                "equipment",
                Compound::from_iter([("offhand", flesh)]).into(),
            ),
        ]);
        for name in ["FleshEater", "FleshEater2", "FleshEater3"] {
            harpas_anvil::write_player(&world, offline_uuid(name), &data).unwrap();
        }
        let effect = Compound::from_iter([
            ("id", Nbt::from("minecraft:hunger")),
            ("duration", Nbt::Int(20)),
        ]);
        let data = Compound::from_iter([
            ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
            (
                "active_effects",
                Nbt::List(harpas_core::nbt::List::Compound(vec![effect])),
            ),
        ]);
        harpas_anvil::write_player(&world, expiring, &data).unwrap();
    }
    let expires = |port, expected: &play::clientbound::EntityEffect| {
        let mut client = Client::join(port, "Expiring");
        confirm_position(&mut client);
        let effect = hunger_packet(&mut client);
        assert_eq!(effect.effect, expected.effect);
        assert!((1..=20).contains(&effect.duration), "{effect:?}");
        client.matching("Hunger expiry", |packet| match packet {
            Packet::RemoveEntityEffect(removed) => {
                assert_eq!(removed.entity_id, effect.entity_id);
                assert_eq!(removed.effect, effect.effect);
                Some(())
            }
            _ => None,
        });
    };
    let expected = exercise_hunger(&mut reference, port);
    expires(port, &expected);
    reference.wait_for("Expiring left the game");
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    assert_eq!(exercise_hunger(&mut server, other), expected);
    expires(other, &expected);
    server.wait_for("Expiring left the game");
    stop(&mut server);
    let exhaustion = (0..20).fold(0.0_f32, |sum, _| sum + 0.005);
    for dir in [&vanilla, &harpas] {
        let (saved, _) = harpas_anvil::read_player(&dir.join("world"), expiring)
            .unwrap()
            .unwrap();
        assert_eq!(
            saved.get("foodExhaustionLevel"),
            Some(&Nbt::Float(exhaustion))
        );
        assert!(saved.get("active_effects").is_none());
    }
}

fn poison_damage(client: &mut Client, effect: &play::clientbound::EntityEffect) {
    client.matching("Poison damage", |packet| match packet {
        Packet::DamageEvent(event) => {
            let types = harpas_java::registry_names("minecraft:damage_type").unwrap();
            assert_eq!(event.entity_id, effect.entity_id);
            assert_eq!(types[event.source_type as usize], "minecraft:magic");
            Some(())
        }
        _ => None,
    });
}

fn exercise_poison(server: &mut Server, port: u16) -> [play::clientbound::EntityEffect; 2] {
    let mut eye = Client::join(port, "SpiderEye");
    confirm_position(&mut eye);
    assert_eq!(health(&mut eye).health, 3.0);
    use_food(&mut eye, play::Hand::OffHand, 1);
    let mut effect = eye.matching("Poison from a spider eye", |packet| match packet {
        Packet::EntityEffect(effect) => Some(effect),
        _ => None,
    });
    assert_eq!((effect.amplifier, effect.duration), (0, 100));
    assert!(effect.flags.blend);
    poison_damage(&mut eye, &effect);
    let hurt = health(&mut eye);
    assert_eq!((hurt.health, hurt.food), (2.0, 3));
    drop(eye);
    server.wait_for("SpiderEye left the game");

    let mut eye = Client::join(port, "SpiderEye");
    confirm_position(&mut eye);
    let mut joined_health = None;
    let mut resumed = None;
    let (joined_health, resumed) = eye.matching("Poison and health on reconnect", |packet| {
        match packet {
            Packet::SetHealth(health) => {
                joined_health.get_or_insert(health.health);
            }
            Packet::EntityEffect(effect) => resumed = Some(effect),
            _ => {}
        }
        joined_health.and_then(|health| resumed.take().map(|effect| (health, effect)))
    });
    assert_eq!(joined_health, 2.0);
    assert_eq!((resumed.effect, resumed.amplifier), (effect.effect, 0));
    assert!((75..100).contains(&resumed.duration), "{resumed:?}");
    assert_eq!(
        resumed.flags,
        play::EffectFlags {
            blend: false,
            ..effect.flags
        }
    );
    poison_damage(&mut eye, &resumed);
    assert_eq!(health(&mut eye).health, 1.0);
    eye.matching("Poison expiry at one health", |packet| match packet {
        Packet::DamageEvent(event) => panic!("unexpected damage at one health: {event:?}"),
        Packet::CombatDeath(death) => panic!("Poison killed the player: {death:?}"),
        Packet::RemoveEntityEffect(removed) => {
            assert_eq!(removed.entity_id, resumed.entity_id);
            assert_eq!(removed.effect, resumed.effect);
            Some(())
        }
        _ => None,
    });
    drop(eye);
    server.wait_for("SpiderEye left the game");
    effect.entity_id = 0;

    for name in ["PoisonPotato", "PoisonPotato2", "PoisonPotato3"] {
        let mut potato = Client::join(port, name);
        confirm_position(&mut potato);
        assert_eq!(health(&mut potato).food, 1);
        for attempt in 1..=9 {
            use_food(&mut potato, play::Hand::OffHand, attempt);
            let mut applied = None;
            potato.matching("eating a poisonous potato", |packet| match packet {
                Packet::EntityEffect(effect) => {
                    applied = Some(effect);
                    None
                }
                Packet::SetHealth(health) if health.food == 1 + 2 * attempt => Some(()),
                _ => None,
            });
            if let Some(mut applied) = applied {
                applied.entity_id = 0;
                assert_eq!(applied.effect, effect.effect);
                return [effect, applied];
            }
        }
    }
    panic!("no Poison effect in 27 poisonous potatoes");
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn food_poison_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("food-poison");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    for dir in [&vanilla, &harpas] {
        for (name, item, count, health) in [
            ("SpiderEye", "minecraft:spider_eye", 1, 3.0),
            ("PoisonPotato", "minecraft:poisonous_potato", 9, 1.0),
            ("PoisonPotato2", "minecraft:poisonous_potato", 9, 1.0),
            ("PoisonPotato3", "minecraft:poisonous_potato", 9, 1.0),
        ] {
            let stack = Compound::from_iter([("id", Nbt::from(item)), ("count", Nbt::Int(count))]);
            let data = Compound::from_iter([
                ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
                ("Health", Nbt::Float(health)),
                ("foodLevel", Nbt::Int(1)),
                ("foodSaturationLevel", Nbt::Float(0.0)),
                (
                    "equipment",
                    Compound::from_iter([("offhand", stack)]).into(),
                ),
            ]);
            harpas_anvil::write_player(&dir.join("world"), offline_uuid(name), &data).unwrap();
        }
    }
    let expected = exercise_poison(&mut reference, port);
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    assert_eq!(exercise_poison(&mut server, other), expected);
    stop(&mut server);
    for dir in [&vanilla, &harpas] {
        let (saved, _) = harpas_anvil::read_player(&dir.join("world"), offline_uuid("SpiderEye"))
            .unwrap()
            .unwrap();
        assert_eq!(saved.get("Health"), Some(&Nbt::Float(1.0)));
        assert!(saved.get("active_effects").is_none());
    }
}

fn pufferfish_effect_packets(client: &mut Client) -> [play::clientbound::EntityEffect; 3] {
    let mut effects = std::array::from_fn(|_| {
        client.matching("pufferfish effect", |packet| match packet {
            Packet::EntityEffect(effect) => Some(effect),
            _ => None,
        })
    });
    effects.sort_unstable_by_key(|effect| effect.amplifier);
    effects
}

fn exercise_pufferfish(server: &mut Server, port: u16) -> [play::clientbound::EntityEffect; 3] {
    let mut client = Client::join(port, "FishEater");
    confirm_position(&mut client);
    assert_eq!(health(&mut client).food, 10);
    use_food(&mut client, play::Hand::OffHand, 1);
    let mut applied = pufferfish_effect_packets(&mut client);
    assert_eq!(applied.each_ref().map(|effect| effect.amplifier), [0, 1, 2]);
    assert_eq!(
        applied.each_ref().map(|effect| effect.duration),
        [300, 1200, 300]
    );
    assert!(applied.iter().all(|effect| effect.flags.blend));
    poison_damage(&mut client, &applied[1]);
    let hurt = health(&mut client);
    assert_eq!((hurt.health, hurt.food), (19.0, 11));
    drop(client);
    server.wait_for("FishEater left the game");

    let mut client = Client::join(port, "FishEater");
    confirm_position(&mut client);
    let resumed = pufferfish_effect_packets(&mut client);
    let elapsed = applied[1].duration - resumed[1].duration;
    assert!((1..120).contains(&elapsed), "{resumed:?}");
    for (before, after) in applied.iter_mut().zip(&resumed) {
        assert_eq!(
            (after.effect, after.amplifier),
            (before.effect, before.amplifier)
        );
        assert_eq!(after.duration, before.duration - elapsed);
        assert_eq!(
            after.flags,
            play::EffectFlags {
                blend: false,
                ..before.flags
            }
        );
        before.entity_id = 0;
    }
    for _ in 0..2 {
        poison_damage(&mut client, &resumed[1]);
    }
    drop(client);
    server.wait_for("FishEater left the game");
    applied
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn food_pufferfish_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("food-pufferfish");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    let fish = Compound::from_iter([
        ("id", Nbt::from("minecraft:pufferfish")),
        ("count", Nbt::Int(1)),
    ]);
    let data = Compound::from_iter([
        ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
        ("foodLevel", Nbt::Int(10)),
        ("foodSaturationLevel", Nbt::Float(0.0)),
        ("equipment", Compound::from_iter([("offhand", fish)]).into()),
    ]);
    let uuid = offline_uuid("FishEater");
    for dir in [&vanilla, &harpas] {
        harpas_anvil::write_player(&dir.join("world"), uuid, &data).unwrap();
    }
    let expected = exercise_pufferfish(&mut reference, port);
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    assert_eq!(exercise_pufferfish(&mut server, other), expected);
    stop(&mut server);

    for dir in [&vanilla, &harpas] {
        let (saved, _) = harpas_anvil::read_player(&dir.join("world"), uuid)
            .unwrap()
            .unwrap();
        let Some(Nbt::List(List::Compound(effects))) = saved.get("active_effects") else {
            panic!("missing saved pufferfish effects");
        };
        let poison = effects
            .iter()
            .find(|effect| effect.get("id") == Some(&Nbt::from("minecraft:poison")))
            .unwrap();
        let Some(Nbt::Int(remaining)) = poison.get("duration") else {
            panic!("missing saved Poison duration");
        };
        // Damage runs before the duration counts down, once per 12 ticks at level II.
        let hits = expected[1].duration / 12 - remaining / 12;
        assert!((3..19).contains(&hits), "{hits} ticks of Poison damage");
        assert_eq!(saved.get("Health"), Some(&Nbt::Float(20.0 - hits as f32)));
    }
}

fn absorption_metadata(bytes: &[u8], entity_id: i32) -> Option<f32> {
    let mut fields = bytes;
    if codec::read_varint(&mut fields).ok()? != 0x63
        || codec::read_varint(&mut fields).ok()? != entity_id
    {
        return None;
    }
    // Absorption is the last tracked field in these player packets. Vanilla may pack it
    // together with effect particles, whose serializer is still missing from the decoder.
    match fields {
        [.., 17, 3, a, b, c, d, 255] => Some(f32::from_be_bytes([*a, *b, *c, *d])),
        _ => None,
    }
}

fn absorption_packets(client: &mut Client, entity_id: i32, amount: f32, max: f64) {
    let mut amount_seen = false;
    let mut max_seen = false;
    for _ in 0..1000 {
        let bytes = client.next("absorption hearts and capacity");
        if let Some(value) = absorption_metadata(&bytes, entity_id) {
            assert_eq!(value, amount);
            amount_seen = true;
        }
        if let Ok(Packet::UpdateAttributes(packet)) = Packet::decode(&bytes)
            && packet.entity_id == entity_id
            && let Some(attribute) = packet
                .attributes
                .iter()
                .find(|attribute| attribute.attribute == play::Attribute::MaxAbsorption)
        {
            assert_eq!(attribute.base_value, 0.0);
            if max == 0.0 {
                assert!(attribute.modifiers.is_empty());
            } else {
                assert_eq!(
                    attribute.modifiers,
                    [play::AttributeModifier {
                        id: "minecraft:effect.absorption".into(),
                        amount: max,
                        operation: play::AttributeOperation::AddValue,
                    }]
                );
            }
            max_seen = true;
        }
        if amount_seen && max_seen {
            return;
        }
    }
    panic!("missing absorption amount or capacity");
}

fn golden_apple_use(
    client: &mut Client,
    sequence: i32,
    before: i32,
    saturation: f32,
) -> [play::clientbound::EntityEffect; 2] {
    use_food(client, play::Hand::OffHand, sequence);
    let mut effects = std::array::from_fn(|_| {
        client.matching("golden apple effect", |packet| match packet {
            Packet::EntityEffect(effect) => Some(effect),
            _ => None,
        })
    });
    let [regen, absorption] = &effects;
    assert_eq!((regen.amplifier, regen.duration), (1, 100));
    assert_eq!((absorption.amplifier, absorption.duration), (0, 2400));
    assert!(regen.flags.blend);
    assert_eq!(absorption.flags.blend, sequence == 1);
    absorption_packets(client, regen.entity_id, 4.0, 4.0);
    for points in (before + 1)..=(before + 4) {
        let update = client.matching("Regeneration healing", |packet| match packet {
            Packet::SetHealth(health) if health.health > before as f32 => Some(health),
            _ => None,
        });
        assert_eq!(
            (update.health, update.food, update.saturation),
            (points as f32, 20, saturation)
        );
    }
    client.matching("Regeneration expiry", |packet| match packet {
        Packet::RemoveEntityEffect(effect) => {
            assert_eq!(
                (effect.entity_id, effect.effect),
                (regen.entity_id, regen.effect)
            );
            Some(())
        }
        _ => None,
    });
    for effect in &mut effects {
        effect.entity_id = 0;
    }
    effects
}

fn exercise_golden_apple(
    server: &mut Server,
    port: u16,
) -> [[play::clientbound::EntityEffect; 2]; 2] {
    let mut client = Client::join(port, "AppleEater");
    let spawn = confirm_position(&mut client);
    let initial = health(&mut client);
    assert_eq!((initial.health, initial.food), (12.0, 20));
    let first = golden_apple_use(&mut client, 1, 12, 9.6);
    fall(&mut client, spawn, 5);
    client.matching("absorbed fall damage", |packet| match packet {
        Packet::DamageEvent(event) => {
            let types = harpas_java::registry_names("minecraft:damage_type").unwrap();
            assert_eq!(types[event.source_type as usize], "minecraft:fall");
            Some(())
        }
        _ => None,
    });
    client.matching("remaining absorption after a fall", |packet| match packet {
        Packet::SetEntityMetadata(packet)
            if packet
                .metadata
                .entries
                .contains(&(17, play::MetadataValue::Float(2.0))) =>
        {
            Some(())
        }
        Packet::SetHealth(update) => panic!("absorbed fall changed health: {update:?}"),
        _ => None,
    });
    assert_no_damage_until_list_reply(&mut client, "after the absorbed fall");
    drop(client);
    server.wait_for("AppleEater left the game");

    let mut client = Client::join(port, "AppleEater");
    let spawn = confirm_position(&mut client);
    let resumed = client.matching("Absorption on reconnect", |packet| match packet {
        Packet::EntityEffect(effect) => Some(effect),
        _ => None,
    });
    assert_eq!((resumed.effect, resumed.amplifier), (first[1].effect, 0));
    assert!(!resumed.flags.blend);
    assert!((2000..2300).contains(&resumed.duration), "{resumed:?}");
    absorption_packets(&mut client, resumed.entity_id, 2.0, 4.0);

    let second = golden_apple_use(&mut client, 2, 16, 19.2);
    fall(&mut client, spawn, 8);
    let mut hurt = false;
    let mut depleted = false;
    client.matching(
        "health and effect after spending the remaining absorption",
        |packet| {
            match packet {
                Packet::SetHealth(update) => {
                    assert_eq!(update.health, 19.0);
                    hurt = true;
                }
                Packet::RemoveEntityEffect(effect) => {
                    assert_eq!(
                        (effect.entity_id, effect.effect),
                        (resumed.entity_id, resumed.effect)
                    );
                    depleted = true;
                }
                _ => {}
            }
            (hurt && depleted).then_some(())
        },
    );
    drop(client);
    server.wait_for("AppleEater left the game");

    let mut expiring = Client::join(port, "ExpiringApple");
    confirm_position(&mut expiring);
    let removed = expiring.matching("Absorption expiry", |packet| match packet {
        Packet::RemoveEntityEffect(effect) => Some(effect),
        _ => None,
    });
    assert_eq!(removed.effect, first[1].effect);
    absorption_packets(&mut expiring, removed.entity_id, 0.0, 0.0);
    drop(expiring);
    server.wait_for("ExpiringApple left the game");
    [first, second]
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn food_golden_apple_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("food-golden-apple");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    let apple = Compound::from_iter([
        ("id", Nbt::from("minecraft:golden_apple")),
        ("count", Nbt::Int(2)),
    ]);
    let eater = Compound::from_iter([
        ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
        ("Health", Nbt::Float(12.0)),
        ("foodLevel", Nbt::Int(20)),
        ("foodSaturationLevel", Nbt::Float(0.0)),
        (
            "equipment",
            Compound::from_iter([("offhand", apple)]).into(),
        ),
    ]);
    let effect = Compound::from_iter([
        ("id", Nbt::from("minecraft:absorption")),
        ("duration", Nbt::Int(40)),
    ]);
    let modifier = Compound::from_iter([
        ("id", Nbt::from("minecraft:effect.absorption")),
        ("amount", Nbt::Double(4.0)),
        ("operation", Nbt::from("add_value")),
    ]);
    let attribute = Compound::from_iter([
        ("id", Nbt::from("minecraft:max_absorption")),
        ("base", Nbt::Double(0.0)),
        ("modifiers", Nbt::List(List::Compound(vec![modifier]))),
    ]);
    let expiring = Compound::from_iter([
        ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
        ("AbsorptionAmount", Nbt::Float(3.0)),
        (
            "attributes",
            Nbt::List(List::Compound(vec![attribute.clone()])),
        ),
        ("active_effects", Nbt::List(List::Compound(vec![effect]))),
    ]);
    for dir in [&vanilla, &harpas] {
        for (name, data) in [("AppleEater", &eater), ("ExpiringApple", &expiring)] {
            harpas_anvil::write_player(&dir.join("world"), offline_uuid(name), data).unwrap();
        }
    }
    let expected = exercise_golden_apple(&mut reference, port);
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    assert_eq!(exercise_golden_apple(&mut server, other), expected);
    stop(&mut server);

    for dir in [&vanilla, &harpas] {
        for name in ["AppleEater", "ExpiringApple"] {
            let (saved, _) = harpas_anvil::read_player(&dir.join("world"), offline_uuid(name))
                .unwrap()
                .unwrap();
            assert_eq!(saved.get("AbsorptionAmount"), Some(&Nbt::Float(0.0)));
            assert!(saved.get("active_effects").is_none(), "{dir:?}, {name}");
            if let Some(Nbt::List(List::Compound(attributes))) = saved.get("attributes")
                && let Some(max) = attributes
                    .iter()
                    .find(|tag| tag.get("id") == attribute.get("id"))
            {
                assert!(max.get("modifiers").is_none(), "{dir:?}, {name}: {max:?}");
            }
            if name == "AppleEater" {
                assert_eq!(saved.get("Health"), Some(&Nbt::Float(19.0)));
                assert!(saved.get("equipment").is_none());
            }
        }
    }
}

fn enter_lava(client: &mut Client, [_, ground, z]: [f64; 3]) {
    // Move to the opening at ground level before descending into the lava.
    move_player(client, [5.5, ground, z], false);
    std::thread::sleep(Duration::from_millis(100));
    move_player(client, [5.5, ground - 1.0, z], false);
    std::thread::sleep(Duration::from_millis(200));
}

fn leave_lava(client: &mut Client, spawn: [f64; 3]) {
    move_player(client, [5.5, spawn[1], spawn[2]], false);
    std::thread::sleep(Duration::from_millis(100));
    move_player(client, spawn, true);
}

fn exercise_enchanted_golden_apple(
    server: &mut Server,
    port: u16,
) -> [play::clientbound::EntityEffect; 4] {
    let mut client = Client::join(port, "EnchantedEater");
    let spawn = confirm_position(&mut client);
    let initial = health(&mut client);
    assert_eq!((initial.health, initial.food), (19.0, 20));
    use_food(&mut client, play::Hand::OffHand, 1);
    let mut applied = [(1, 400), (0, 6000), (0, 6000), (3, 2400)].map(|(amplifier, duration)| {
        client.matching("enchanted golden apple effect", |packet| match packet {
            Packet::EntityEffect(effect) => {
                assert_eq!((effect.amplifier, effect.duration), (amplifier, duration));
                assert!(effect.flags.blend);
                Some(effect)
            }
            _ => None,
        })
    });
    let entity = applied[0].entity_id;
    absorption_packets(&mut client, entity, 16.0, 16.0);
    client.matching("enchanted golden apple healing", |packet| match packet {
        Packet::SetHealth(update) if update.health == 20.0 => {
            assert_eq!((update.food, update.saturation), (20, 9.6));
            Some(())
        }
        _ => None,
    });

    // Resistance I reduces the five points of fall damage to four before absorption.
    let damage_types = harpas_java::registry_names("minecraft:damage_type").unwrap();
    fall(&mut client, spawn, 8);
    client.matching("resisted fall", |packet| match packet {
        Packet::DamageEvent(event) => {
            assert_eq!(damage_types[event.source_type as usize], "minecraft:fall");
            Some(())
        }
        _ => None,
    });
    let remaining = (0..1000).find_map(|_| {
        let bytes = client.next("remaining absorption after resisted fall");
        absorption_metadata(&bytes, entity)
    });
    assert_eq!(remaining, Some(12.0));
    enter_lava(&mut client, spawn);
    assert_no_damage_until_list_reply(&mut client, "with Fire Resistance in lava");
    leave_lava(&mut client, spawn);
    assert_no_damage_until_list_reply(&mut client, "after leaving lava");
    drop(client);
    server.wait_for("EnchantedEater left the game");

    let mut client = Client::join(port, "EnchantedEater");
    confirm_position(&mut client);
    let mut resumed = applied.each_ref().map(|_| {
        client.matching(
            "enchanted apple effect on reconnect",
            |packet| match packet {
                Packet::EntityEffect(effect) => Some(effect),
                _ => None,
            },
        )
    });
    absorption_packets(&mut client, resumed[0].entity_id, 12.0, 16.0);
    resumed.sort_by_key(|effect| effect.effect);
    applied.sort_by_key(|effect| effect.effect);
    for (current, initial) in resumed.iter().zip(&mut applied) {
        assert_eq!(current.effect, initial.effect);
        assert_eq!(current.amplifier, initial.amplifier);
        assert!(!current.flags.blend);
        assert!((1..initial.duration).contains(&current.duration));
        initial.entity_id = 0;
    }
    drop(client);
    server.wait_for("EnchantedEater left the game");

    let mut client = Client::join(port, "LavaProbe");
    let spawn = confirm_position(&mut client);
    assert_eq!(health(&mut client).health, 20.0);
    enter_lava(&mut client, spawn);
    assert_no_damage_until_list_reply(&mut client, "before Fire Resistance expired");
    client.matching("Fire Resistance expiry", |packet| match packet {
        Packet::RemoveEntityEffect(_) => Some(()),
        _ => None,
    });
    client.matching("lava damage", |packet| match packet {
        Packet::DamageEvent(event) => {
            assert_eq!(damage_types[event.source_type as usize], "minecraft:lava");
            Some(())
        }
        Packet::SynchronizePlayerPosition(position) => {
            panic!("unexpected teleport to {position:?}")
        }
        _ => None,
    });
    assert_eq!(health(&mut client).health, 16.0);
    leave_lava(&mut client, spawn);
    client.matching("burn damage after leaving lava", |packet| match packet {
        Packet::DamageEvent(event) => {
            assert_eq!(
                damage_types[event.source_type as usize],
                "minecraft:on_fire"
            );
            Some(())
        }
        _ => None,
    });
    assert_eq!(health(&mut client).health, 15.0);
    drop(client);
    server.wait_for("LavaProbe left the game");
    applied
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn food_enchanted_golden_apple_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("food-enchanted-golden-apple");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    let mut setup = Client::join(port, "TerrainSetup");
    confirm_position(&mut setup);
    setup.matching("chunk containing the lava fixture", |packet| match packet {
        Packet::ChunkDataAndUpdateLight(chunk) if (chunk.x, chunk.z) == (0, 0) => Some(()),
        _ => None,
    });
    writeln!(
        reference.child.stdin.as_mut().unwrap(),
        "setblock 5 -61 0 minecraft:lava\nsave-all flush"
    )
    .unwrap();
    reference.wait_for("Changed the block");
    reference.wait_for("Saved the game");
    drop(setup);
    reference.wait_for("TerrainSetup left the game");
    let apple = Compound::from_iter([
        ("id", Nbt::from("minecraft:enchanted_golden_apple")),
        ("count", Nbt::Int(1)),
    ]);
    let eater = Compound::from_iter([
        ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
        ("Health", Nbt::Float(19.0)),
        ("foodLevel", Nbt::Int(20)),
        ("foodSaturationLevel", Nbt::Float(0.0)),
        (
            "equipment",
            Compound::from_iter([("offhand", apple)]).into(),
        ),
    ]);
    let fire_resistance = Compound::from_iter([
        ("id", Nbt::from("minecraft:fire_resistance")),
        ("duration", Nbt::Int(80)),
    ]);
    let probe = Compound::from_iter([
        ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
        (
            "active_effects",
            Nbt::List(List::Compound(vec![fire_resistance])),
        ),
    ]);
    for dir in [&vanilla, &harpas] {
        for (name, data) in [("EnchantedEater", &eater), ("LavaProbe", &probe)] {
            harpas_anvil::write_player(&dir.join("world"), offline_uuid(name), data).unwrap();
        }
    }
    let expected = exercise_enchanted_golden_apple(&mut reference, port);
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    assert_eq!(
        exercise_enchanted_golden_apple(&mut server, other),
        expected
    );
    stop(&mut server);

    for dir in [&vanilla, &harpas] {
        let (saved, _) =
            harpas_anvil::read_player(&dir.join("world"), offline_uuid("EnchantedEater"))
                .unwrap()
                .unwrap();
        assert!(saved.get("equipment").is_none());
        assert_eq!(saved.get("AbsorptionAmount"), Some(&Nbt::Float(12.0)));
        let Some(Nbt::List(List::Compound(effects))) = saved.get("active_effects") else {
            panic!("missing saved enchanted golden apple effects in {dir:?}");
        };
        for id in [
            "regeneration",
            "resistance",
            "fire_resistance",
            "absorption",
        ] {
            assert!(
                effects
                    .iter()
                    .any(|effect| effect.get("id") == Some(&Nbt::from(format!("minecraft:{id}"))))
            );
        }
        let (burning, _) = harpas_anvil::read_player(&dir.join("world"), offline_uuid("LavaProbe"))
            .unwrap()
            .unwrap();
        assert!(matches!(burning.get("Fire"), Some(Nbt::Short(1..=300))));
    }
}

fn starvation_damage(client: &mut Client) {
    client.matching("starvation damage", |packet| match packet {
        Packet::DamageEvent(event) => {
            let types = harpas_java::registry_names("minecraft:damage_type").unwrap();
            assert_eq!(types[event.source_type as usize], "minecraft:starve");
            Some(())
        }
        _ => None,
    });
}

fn exercise_starvation(
    server: &mut Server,
    dir: &Path,
    port: u16,
    difficulty: &str,
    initial_health: f32,
    natural_regeneration: bool,
) -> Option<(play::clientbound::Respawn, Nbt)> {
    let name = "Hungry";
    let world = dir.join("world");
    let uuid = offline_uuid(name);
    let data = Compound::from_iter([
        ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
        ("Health", Nbt::Float(initial_health)),
        ("foodLevel", Nbt::Int(0)),
        ("foodSaturationLevel", Nbt::Float(0.0)),
        ("foodTickTimer", Nbt::Int(40)),
    ]);
    harpas_anvil::write_player(&world, uuid, &data).unwrap();
    let join = || {
        let mut client = Client::join(port, name);
        let spawn = confirm_position(&mut client);
        assert_eq!(
            health(&mut client),
            play::clientbound::SetHealth {
                health: initial_health,
                food: 0,
                saturation: 0.0,
            },
            "{difficulty} on login"
        );
        (client, spawn)
    };
    let (mut client, mut spawn) = join();
    if difficulty == "peaceful" && natural_regeneration {
        client.matching("food recovery on Peaceful", |packet| match packet {
            Packet::DamageEvent(event) => panic!("starved before recovering food: {event:?}"),
            Packet::SetHealth(health) => {
                assert_eq!((health.health, health.food), (20.0, 1));
                Some(())
            }
            _ => None,
        });
        return None;
    }
    if difficulty == "hard" {
        std::thread::sleep(Duration::from_millis(650));
        drop(client);
        server.wait_for("Hungry left the game");
        // Harpas writes on a worker. Wait for the saved timer to advance beyond the fixture.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let (saved, _) = harpas_anvil::read_player(&world, uuid).unwrap().unwrap();
            if let Some(Nbt::Int(timer)) = saved.get("foodTickTimer")
                && *timer > 40
            {
                assert!(*timer < 80, "{timer}");
                break;
            }
            assert!(Instant::now() < deadline, "starvation timer was not saved");
            std::thread::sleep(Duration::from_millis(20));
        }
        (client, spawn) = join();
    }

    let first_hit = Instant::now();
    starvation_damage(&mut client);
    assert!(
        first_hit.elapsed() < Duration::from_millis(3500),
        "{difficulty} restarted the saved timer"
    );
    if difficulty == "hard" {
        let death = client.matching("starvation death", |packet| match packet {
            Packet::CombatDeath(death) => Some(death.message),
            _ => None,
        });
        assert_eq!(health(&mut client).health, 0.0);
        play_packet(
            &mut client,
            play::serverbound::ClientStatus {
                action: play::ClientStatusAction::PerformRespawn,
            },
        );
        let respawn = client.matching("respawn after starvation", |packet| match packet {
            Packet::Respawn(respawn) => Some(respawn),
            _ => None,
        });
        assert_eq!(confirm_position(&mut client), spawn);
        let restored = health(&mut client);
        assert_eq!(
            (restored.health, restored.food, restored.saturation),
            (20.0, 20, 5.0)
        );
        assert_eq!(respawn.spawn.game_mode, play::GameMode::Survival);
        return Some((respawn, death));
    }

    assert_eq!(health(&mut client).health, initial_health - 1.0);
    std::thread::sleep(Duration::from_millis(4250));
    assert_no_damage_until_list_reply(&mut client, &format!("below the {difficulty} threshold"));
    None
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn starvation_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    for (difficulty, initial_health, natural_regeneration) in [
        ("peaceful", 11.0, false),
        ("peaceful", 20.0, true),
        ("easy", 11.0, false),
        ("normal", 2.0, false),
        ("hard", 1.0, false),
    ] {
        let folder = Folder::new(difficulty);
        let vanilla = folder.0.join("vanilla");
        let harpas = folder.0.join("harpas");
        let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, difficulty);
        let mut reference = start_flat_vanilla(&jar, &vanilla, natural_regeneration);
        let expected = exercise_starvation(
            &mut reference,
            &vanilla,
            port,
            difficulty,
            initial_health,
            natural_regeneration,
        );
        stop(&mut reference);
        let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
        assert_eq!(
            exercise_starvation(
                &mut server,
                &harpas,
                other,
                difficulty,
                initial_health,
                natural_regeneration
            ),
            expected
        );
        stop(&mut server);
    }
}
