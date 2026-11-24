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

fn exercise_punches(port: u16) -> ([f64; 3], codec::Nbt) {
    let mut attacker = Client::join(port, "Attacker");
    let mut defender = Client::join(port, "Defender");
    let join = |client: &mut Client| {
        client.stream.set_nodelay(true).unwrap();
        let id = client.matching("Play Login", |packet| match packet {
            Packet::Login(login) => Some(login.entity_id),
            _ => None,
        });
        (id, confirm_position(client))
    };
    let (attacker_id, _) = join(&mut attacker);
    let (defender_id, [x, y, z]) = join(&mut defender);
    assert_eq!(health(&mut defender).health, 2.0);
    move_player(&mut defender, [x, y, z], true);
    move_player(&mut attacker, [x + 3.2, y, z], true);
    std::thread::sleep(Duration::from_millis(400));

    let punch = play::serverbound::Attack {
        entity_id: defender_id,
    };
    play_packet(&mut attacker, punch.clone());
    play_packet(&mut attacker, punch.clone());
    let mut knockback = None;
    let mut damage = false;
    let mut injured = false;
    let motion = defender.matching("damage, knockback and injured health", |packet| {
        match packet {
            Packet::DamageEvent(event) => {
                let types = harpas_java::registry_names("minecraft:damage_type").unwrap();
                assert_eq!(types[event.source_type as usize], "minecraft:player_attack");
                assert_eq!(event.entity_id, defender_id);
                assert_eq!(event.source_cause, attacker_id + 1);
                assert_eq!(event.source_direct, attacker_id + 1);
                assert!(!damage, "the rapid punch dealt damage");
                damage = true;
            }
            Packet::SetEntityVelocity(packet) if packet.entity_id == defender_id => {
                let velocity = packet.velocity;
                assert!(velocity.x < 0.0 && velocity.y > 0.0);
                knockback = Some([velocity.x, velocity.y, velocity.z]);
            }
            Packet::SetHealth(packet) => {
                assert_eq!(packet.health, 1.0);
                injured = true;
            }
            _ => {}
        }
        if damage && injured { knockback } else { None }
    });
    attacker.command("list", 1);
    assert_no_damage_until_list_reply(&mut defender, "after the rapid punch");

    std::thread::sleep(Duration::from_millis(650));
    move_player(&mut attacker, [x + 3.35, y, z], true);
    play_packet(&mut attacker, punch.clone());
    attacker.command("list", 1);
    assert_no_damage_until_list_reply(&mut defender, "after the out-of-range punch");

    move_player(&mut attacker, [x + 2.0, y, z], true);
    play_packet(&mut attacker, punch);
    let message = defender.matching("CombatDeath", |packet| match packet {
        Packet::CombatDeath(death) => {
            assert_eq!(death.player_id, defender_id);
            Some(death.message)
        }
        _ => None,
    });
    assert_eq!(health(&mut defender).health, 0.0);
    (motion, message)
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn bare_hand_combat_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("bare-hand-combat");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    let data = Compound::from_iter([
        ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
        ("Health", Nbt::Float(2.0)),
    ]);
    for dir in [&vanilla, &harpas] {
        harpas_anvil::write_player(&dir.join("world"), offline_uuid("Defender"), &data).unwrap();
    }
    let expected = exercise_punches(port);
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    assert_eq!(exercise_punches(other), expected);
    stop(&mut server);
    for dir in [&vanilla, &harpas] {
        for name in ["Attacker", "Defender"] {
            let (saved, _) = harpas_anvil::read_player(&dir.join("world"), offline_uuid(name))
                .unwrap()
                .unwrap();
            assert_eq!(saved.get("foodExhaustionLevel"), Some(&Nbt::Float(0.2)));
        }
    }
}

fn exercise_knockback_attacks(port: u16, names: [&str; 3], damage: f32) -> [[f64; 3]; 6] {
    let join = |name| {
        let mut client = Client::join(port, name);
        client.stream.set_nodelay(true).unwrap();
        let id = client.matching("Play Login", |packet| match packet {
            Packet::Login(login) => Some(login.entity_id),
            _ => None,
        });
        let position = confirm_position(&mut client);
        assert_eq!(health(&mut client).health, 20.0);
        (client, id, position)
    };
    let [attacker, defender, reserve] = names;
    let (mut attacker, attacker_id, [x, y, z]) = join(attacker);
    let (mut defender, defender_id, _) = join(defender);
    let (mut reserve, reserve_id, _) = join(reserve);
    move_player(&mut attacker, [x, y, z], true);
    move_player(&mut defender, [x + 1.5, y, z], true);
    move_player(&mut reserve, [x - 1.5, y, z], true);

    let face = |attacker: &mut Client, yaw| {
        play_packet(
            attacker,
            play::serverbound::SetPlayerRotation {
                yaw,
                pitch: 0.0,
                flags: play::MovementFlags {
                    on_ground: true,
                    ..Default::default()
                },
            },
        );
    };
    let start = play::serverbound::PlayerCommand {
        entity_id: attacker_id,
        action: play::PlayerCommandAction::StartSprinting,
        data: 0,
    };
    let attack = play::serverbound::Attack {
        entity_id: defender_id,
    };
    let attack_reserve = play::serverbound::Attack {
        entity_id: reserve_id,
    };
    let recharge = || std::thread::sleep(Duration::from_millis(800));
    let velocity = |packet: &Packet, id| match packet {
        Packet::SetEntityVelocity(packet) if packet.entity_id == id => {
            let v = packet.velocity;
            Some([v.x, v.y, v.z])
        }
        _ => None,
    };
    let hit = |target: &mut Client, id| {
        let mut motion = None;
        let mut health = None;
        target.matching("health and velocity after a strike", |packet| {
            motion = velocity(&packet, id).or(motion);
            if let Packet::SetHealth(packet) = packet {
                health = Some(packet.health);
            }
            Some((motion?, health?))
        })
    };

    // Keep the target to the side to distinguish the attacker's facing from the line to the target.
    face(&mut attacker, 0.0);
    recharge();
    play_packet(&mut attacker, attack.clone());
    let (walking, health) = hit(&mut defender, defender_id);
    assert_eq!(health, 20.0 - damage);

    recharge();
    play_packet(&mut attacker, start.clone());
    play_packet(&mut attacker, attack.clone());
    let (sprinting, health) = hit(&mut defender, defender_id);
    assert_eq!(health, 20.0 - 2.0 * damage);

    recharge();
    // The previous strike should have stopped the sprint. Start again between these two attacks
    // so that the second strike tests the sprint with the charge just reset.
    let mut frames = Vec::new();
    for packet in [
        attack.clone().into(),
        start.clone().into(),
        attack_reserve.clone().into(),
    ] {
        write_frame(&mut frames, |buf| {
            play::serverbound::Packet::encode(&packet, buf);
        });
    }
    attacker.stream.write_all(&frames).unwrap();
    let (after_sprint, health) = hit(&mut defender, defender_id);
    assert_eq!(health, 20.0 - 3.0 * damage);
    let (partial, partial_health) = hit(&mut reserve, reserve_id);
    assert!(partial_health > 20.0 - damage && partial_health < 20.0);

    // A charged follow-up reveals whether the partial strike ended the sprint.
    face(&mut attacker, 100_000.0 * 360.0 + 132.0);
    recharge();
    play_packet(&mut attacker, attack_reserve.clone());
    let (after_partial, reserve_health) = hit(&mut reserve, reserve_id);
    assert_eq!(reserve_health, partial_health - damage);

    // Give the defender damage immunity with an uncharged punch, then deal more damage within it.
    face(&mut attacker, 0.0);
    recharge();
    frames.clear();
    for entity_id in [attacker_id, defender_id] {
        let packet = play::serverbound::Packet::from(play::serverbound::Attack { entity_id });
        write_frame(&mut frames, |buf| packet.encode(buf));
    }
    reserve.stream.write_all(&frames).unwrap();
    let (_, primed_health) = hit(&mut defender, defender_id);
    let previous = 20.0 - 3.0 * damage;
    assert!(primed_health > previous - 1.0 && primed_health < previous);
    play_packet(&mut attacker, start);
    play_packet(&mut attacker, attack);
    let check_velocity = |packet: &Packet| {
        assert_eq!(
            velocity(packet, defender_id),
            None,
            "knockback during damage immunity"
        );
    };
    let cooldown_health = defender.matching(
        "health after a stronger sprint attack during damage immunity",
        |packet| {
            check_velocity(&packet);
            match packet {
                Packet::SetHealth(packet) => Some(packet.health),
                _ => None,
            }
        },
    );
    assert!((cooldown_health - (previous - damage)).abs() < 0.0001);
    defender.send_command("list");
    defender.matching("list after a hit during damage immunity", |packet| {
        check_velocity(&packet);
        match packet {
            Packet::SystemChatMessage(reply)
                if matches!(&reply.content, codec::Nbt::Compound(text)
                    if text.get("translate") == Some(&codec::Nbt::from("commands.list.players"))) =>
            {
                Some(())
            }
            _ => None,
        }
    });
    recharge();
    play_packet(&mut attacker, attack_reserve);
    let (after_cooldown_hit, health) = hit(&mut reserve, reserve_id);
    assert_eq!(health, reserve_health - damage);
    [
        walking,
        sprinting,
        after_sprint,
        partial,
        after_partial,
        after_cooldown_hit,
    ]
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn sprint_knockback_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("sprint-knockback");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    let sword = Compound::from_iter([
        ("Slot", Nbt::Byte(0)),
        ("id", Nbt::from("minecraft:wooden_sword")),
    ]);
    let sword_player = Compound::from_iter([
        ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
        ("Inventory", List::Compound(vec![sword]).into()),
    ]);
    let sword_names = ["SwordFighter", "SwordTarget", "SwordReserve"];
    for dir in [&vanilla, &harpas] {
        harpas_anvil::write_player(
            &dir.join("world"),
            offline_uuid(sword_names[0]),
            &sword_player,
        )
        .unwrap();
    }
    let exercise = |port| {
        [
            exercise_knockback_attacks(port, ["FistFighter", "FistTarget", "FistReserve"], 1.0),
            exercise_knockback_attacks(port, sword_names, 4.0),
        ]
    };
    let expected = exercise(port);
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    assert_eq!(exercise(other), expected);
    stop(&mut server);
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn knockback_enchantment_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("knockback-enchantment");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    let cases = [
        (["KnockbackOne", "OneTarget", "OneReserve"], 1, false),
        (["KnockbackTwo", "TwoTarget", "TwoReserve"], 2, false),
        (
            ["OffhandFighter", "OffhandTarget", "OffhandReserve"],
            2,
            true,
        ),
    ];
    for (names, level, offhand) in cases {
        let sword = Compound::from_iter([
            ("id", Nbt::from("minecraft:wooden_sword")),
            (
                "components",
                Compound::from_iter([(
                    "minecraft:enchantments",
                    Compound::from_iter([("minecraft:knockback", level)]),
                )])
                .into(),
            ),
        ]);
        let mut player =
            Compound::from_iter([("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION))]);
        if offhand {
            player.insert("equipment", Compound::from_iter([("offhand", sword)]));
        } else {
            let mut sword = sword;
            sword.insert("Slot", 0_i8);
            player.insert("Inventory", List::Compound(vec![sword]));
        }
        for dir in [&vanilla, &harpas] {
            harpas_anvil::write_player(&dir.join("world"), offline_uuid(names[0]), &player)
                .unwrap();
        }
    }
    let exercise = |port| {
        cases.map(|(names, _, offhand)| {
            let damage = if offhand { 1.0 } else { 4.0 };
            exercise_knockback_attacks(port, names, damage)
        })
    };
    let expected = exercise(port);
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    assert_eq!(exercise(other), expected);
    stop(&mut server);
}

fn attack_speed(client: &mut Client, equipped: bool) -> play::AttributeProperty {
    client.matching("main-hand attack speed", |packet| match packet {
        Packet::UpdateAttributes(packet) => packet.attributes.into_iter().find(|property| {
            property.attribute == play::Attribute::AttackSpeed
                && property.modifiers.is_empty() != equipped
        }),
        _ => None,
    })
}

fn exercise_swords(port: u16) -> [play::AttributeProperty; 2] {
    let mut attacker = Client::join(port, "Attacker");
    let mut defender = Client::join(port, "Defender");
    attacker.stream.set_nodelay(true).unwrap();
    defender.stream.set_nodelay(true).unwrap();
    confirm_position(&mut attacker);
    let defender_id = defender.matching("Play Login", |packet| match packet {
        Packet::Login(login) => Some(login.entity_id),
        _ => None,
    });
    let [x, y, z] = confirm_position(&mut defender);
    assert_eq!(health(&mut defender).health, 20.0);
    move_player(&mut defender, [x, y, z], true);
    move_player(&mut attacker, [x + 2.0, y, z], true);

    let select =
        |client: &mut Client, slot| play_packet(client, play::serverbound::SetHeldItem { slot });
    let hit = play::serverbound::Attack {
        entity_id: defender_id,
    };
    let attack = |client: &mut Client| play_packet(client, hit.clone());
    select(&mut attacker, 1);
    let equipped = attack_speed(&mut attacker, true);
    std::thread::sleep(Duration::from_millis(800));
    attack(&mut attacker);
    attack(&mut attacker);
    assert_eq!(health(&mut defender).health, 16.0);
    attacker.command("list", 1);
    assert_no_damage_until_list_reply(&mut defender, "after the rapid sword attack");

    select(&mut attacker, 0);
    let unarmed = attack_speed(&mut attacker, false);
    std::thread::sleep(Duration::from_millis(800));
    select(&mut attacker, 2);
    assert_eq!(attack_speed(&mut attacker, true), equipped);
    attack(&mut attacker);
    let after_partial = health(&mut defender).health;
    let recharge_ticks = 20.0 / (equipped.base_value + equipped.modifiers[0].amount) as f32;
    let partial_hit = |before: f32, after: f32| {
        // The attack crosses the network, so it can arrive on any of these early charge ticks.
        (0..6).any(|ticks| {
            let charge = (ticks as f32 + 0.5) / recharge_ticks;
            let expected = before - 6.0 * (0.2 + charge * charge * 0.8);
            (after - expected).abs() < 0.0001
        })
    };
    assert!(
        partial_hit(16.0, after_partial),
        "unexpected partially charged hit: {after_partial}"
    );

    std::thread::sleep(Duration::from_millis(800));
    // The same item type in a new slot keeps its charge. This second iron sword breaks on hit.
    select(&mut attacker, 3);
    std::thread::sleep(Duration::from_millis(150));
    attack(&mut attacker);
    let after_break = health(&mut defender).health;
    assert_eq!(after_break, after_partial - 6.0);
    assert_eq!(attack_speed(&mut attacker, false), unarmed);

    std::thread::sleep(Duration::from_millis(800));
    // Before the equipment tick, the hit uses the old attributes and wears the newly selected sword.
    let mut frames = Vec::new();
    let select_iron: play::serverbound::Packet = play::serverbound::SetHeldItem { slot: 2 }.into();
    let hit: play::serverbound::Packet = hit.into();
    write_frame(&mut frames, |buf| select_iron.encode(buf));
    write_frame(&mut frames, |buf| hit.encode(buf));
    attacker.stream.write_all(&frames).unwrap();
    let after_swap = health(&mut defender).health;
    assert!(
        after_swap == after_break - 1.0 || partial_hit(after_break, after_swap),
        "unexpected hit during equipment change: {}",
        after_break - after_swap
    );
    [equipped, unarmed]
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn sword_combat_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("sword-combat");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    let sword =
        |slot: i8, id: &str| Compound::from_iter([("Slot", Nbt::Byte(slot)), ("id", id.into())]);
    let mut fragile = sword(3, "minecraft:iron_sword");
    fragile.insert(
        "components",
        Compound::from_iter([("minecraft:max_damage", 1)]),
    );
    let data = Compound::from_iter([
        ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
        (
            "Inventory",
            Nbt::List(List::Compound(vec![
                sword(1, "minecraft:wooden_sword"),
                sword(2, "minecraft:iron_sword"),
                fragile,
            ])),
        ),
    ]);
    for dir in [&vanilla, &harpas] {
        harpas_anvil::write_player(&dir.join("world"), offline_uuid("Attacker"), &data).unwrap();
    }
    let expected = exercise_swords(port);
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    assert_eq!(exercise_swords(other), expected);
    stop(&mut server);

    let [expected, actual] = [&vanilla, &harpas].map(|dir| {
        let (saved, _) = harpas_anvil::read_player(&dir.join("world"), offline_uuid("Attacker"))
            .unwrap()
            .unwrap();
        saved.get("Inventory").unwrap().clone()
    });
    assert_eq!(actual, expected);
    let Nbt::List(List::Compound(swords)) = actual else {
        panic!("missing saved sword stacks");
    };
    assert_eq!(swords.len(), 2);
    for (sword, damage) in swords.iter().zip([1, 2]) {
        assert_eq!(
            sword.get("components"),
            Some(&Nbt::from(Compound::from_iter([(
                "minecraft:damage",
                damage
            )])))
        );
    }
}

fn fire_flag(packet: &Packet, entity_id: i32) -> Option<bool> {
    let Packet::SetEntityMetadata(packet) = packet else {
        return None;
    };
    if packet.entity_id != entity_id {
        return None;
    }
    packet
        .metadata
        .entries
        .iter()
        .find_map(|entry| match entry {
            (0, play::MetadataValue::Byte(flags)) => Some(flags & 1 != 0),
            _ => None,
        })
}

fn exercise_fire_aspect(port: u16) -> [Vec<f32>; 3] {
    let mut attacker = Client::join(port, "FireAttacker");
    attacker.stream.set_nodelay(true).unwrap();
    confirm_position(&mut attacker);
    let list_reply = |packet: &Packet| {
        matches!(packet, Packet::SystemChatMessage(reply)
            if matches!(&reply.content, codec::Nbt::Compound(text)
                if text.get("translate") == Some(&codec::Nbt::from("commands.list.players"))))
    };

    [
        ("FireTargetOne", 1_u8, false),
        ("FireTargetTwo", 2, false),
        ("FireResistant", 1, true),
    ]
    .map(|(name, level, resistant)| {
        let mut defender = Client::join(port, name);
        defender.stream.set_nodelay(true).unwrap();
        let entity_id = defender.matching("Play Login", |packet| match packet {
            Packet::Login(login) => Some(login.entity_id),
            _ => None,
        });
        let [x, y, z] = confirm_position(&mut defender);
        assert_eq!(health(&mut defender).health, 20.0);
        move_player(&mut defender, [x, y, z], true);
        move_player(&mut attacker, [x + 2.0, y, z], true);
        let attack = play::serverbound::Attack { entity_id };
        let mut remaining = 20.0;

        if name == "FireTargetOne" {
            // A plain sword hit grants immunity before the weaker enchanted hit in the same batch.
            std::thread::sleep(Duration::from_millis(800));
            let mut frames = Vec::new();
            for packet in [
                attack.clone().into(),
                play::serverbound::SetHeldItem { slot: 2 }.into(),
                attack.clone().into(),
            ] {
                write_frame(&mut frames, |buf| {
                    play::serverbound::Packet::encode(&packet, buf)
                });
            }
            attacker.stream.write_all(&frames).unwrap();
            assert_eq!(health(&mut defender).health, 16.0);
            remaining = 16.0;
            attacker.command("list", 1);
        }

        play_packet(
            &mut attacker,
            play::serverbound::SetHeldItem {
                slot: i16::from(level),
            },
        );
        std::thread::sleep(Duration::from_millis(800));
        defender.send_command("list");
        defender.matching("list before the enchanted hit", |packet| {
            assert_ne!(fire_flag(&packet, entity_id), Some(true));
            match packet {
                Packet::SetHealth(update) => {
                    panic!("unexpected health before enchanted hit: {update:?}")
                }
                _ => list_reply(&packet).then_some(()),
            }
        });

        let strike = Instant::now();
        play_packet(&mut attacker, attack);
        attacker.command("list", 1);
        // Allow the defender's metadata to be sent before checking for ignition.
        std::thread::sleep(Duration::from_millis(200));
        defender.send_command("list");
        let damage_types = harpas_java::registry_names("minecraft:damage_type").unwrap();
        let mut sources = Vec::new();
        let mut health_values = Vec::new();
        let mut ignited = false;
        let elapsed = defender.matching("Fire Aspect ignition and expiry", |packet| {
            if list_reply(&packet) {
                assert!(ignited, "{name} was not ignited by Fire Aspect {level}");
            }
            match &packet {
                Packet::DamageEvent(event) if event.entity_id == entity_id => {
                    sources.push(damage_types[event.source_type as usize]);
                }
                Packet::SetHealth(update) => health_values.push(update.health),
                _ => {}
            }
            match fire_flag(&packet, entity_id) {
                Some(true) => {
                    ignited = true;
                    None
                }
                Some(false) if ignited => Some(strike.elapsed()),
                _ => None,
            }
        });
        assert_no_damage_until_list_reply(&mut defender, "after Fire Aspect expired");

        let duration = Duration::from_secs(4 * u64::from(level));
        assert!(
            elapsed >= duration - Duration::from_millis(500),
            "{name} burned for {elapsed:?}"
        );
        let burn_hits = if resistant {
            0
        } else {
            4 * usize::from(level) - 1
        };
        assert_eq!(sources.first(), Some(&"minecraft:player_attack"));
        assert_eq!(sources[1..], vec!["minecraft:on_fire"; burn_hits]);
        assert_eq!(
            health_values,
            (0..=burn_hits)
                .map(|hit| remaining - 4.0 - hit as f32)
                .collect::<Vec<_>>()
        );
        health_values
    })
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn fire_aspect_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("fire-aspect");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    let swords = (0..=2)
        .map(|slot| {
            let mut sword = Compound::from_iter([
                ("Slot", Nbt::Byte(slot)),
                ("id", Nbt::from("minecraft:wooden_sword")),
            ]);
            if slot > 0 {
                sword.insert(
                    "components",
                    Compound::from_iter([(
                        "minecraft:enchantments",
                        Compound::from_iter([("minecraft:fire_aspect", i32::from(slot))]),
                    )]),
                );
            }
            sword
        })
        .collect();
    let attacker = Compound::from_iter([
        ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
        ("Inventory", List::Compound(swords).into()),
    ]);
    let resistant = Compound::from_iter([
        ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
        (
            "active_effects",
            List::Compound(vec![Compound::from_iter([
                ("id", Nbt::from("minecraft:fire_resistance")),
                ("duration", Nbt::Int(-1)),
            ])])
            .into(),
        ),
    ]);
    for dir in [&vanilla, &harpas] {
        for (name, data) in [("FireAttacker", &attacker), ("FireResistant", &resistant)] {
            harpas_anvil::write_player(&dir.join("world"), offline_uuid(name), data).unwrap();
        }
    }
    let expected = exercise_fire_aspect(port);
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    assert_eq!(exercise_fire_aspect(other), expected);
    stop(&mut server);
}

fn exercise_shields(port: u16) {
    let mut attacker = Client::join(port, "ShieldAttacker");
    attacker.stream.set_nodelay(true).unwrap();
    confirm_position(&mut attacker);

    for (name, hand, blocks) in [
        ("ShieldMain", play::Hand::MainHand, 2),
        ("ShieldOffhand", play::Hand::OffHand, 1),
    ] {
        let mut defender = Client::join(port, name);
        defender.stream.set_nodelay(true).unwrap();
        let entity_id = defender.matching("Play Login", |packet| match packet {
            Packet::Login(login) => Some(login.entity_id),
            _ => None,
        });
        let [x, y, z] = confirm_position(&mut defender);
        assert_eq!(health(&mut defender).health, 20.0);
        move_player(&mut defender, [x, y, z], true);
        move_player(&mut attacker, [x, y, z + 2.0], true);
        play_packet(
            &mut defender,
            play::serverbound::SetPlayerRotation {
                yaw: 0.0,
                pitch: 0.0,
                flags: play::MovementFlags {
                    on_ground: true,
                    ..Default::default()
                },
            },
        );
        play_packet(&mut attacker, play::serverbound::SetHeldItem { slot: 0 });
        use_food(&mut defender, hand, 1);
        let attack = play::serverbound::Attack { entity_id };
        for _ in 0..blocks {
            std::thread::sleep(Duration::from_millis(800));
            play_packet(&mut attacker, attack.clone());
            attacker.command("list", 1);
            std::thread::sleep(Duration::from_millis(100));
            defender.send_command("list");
            let mut broke = false;
            defender.matching("packets following a shield block", |packet| {
                assert_ne!(fire_flag(&packet, entity_id), Some(true), "blocked Fire Aspect");
                match packet {
                    Packet::SetHealth(health) => panic!("blocked hit changed health: {health:?}"),
                    Packet::SetEntityVelocity(velocity) if velocity.entity_id == entity_id => {
                        panic!("blocked hit pushed the defender: {velocity:?}");
                    }
                    Packet::EntityEvent(event) if event.entity_id == entity_id && event.event == 48 => {
                        broke = true;
                    }
                    Packet::SystemChatMessage(reply)
                        if matches!(&reply.content, codec::Nbt::Compound(text)
                            if text.get("translate") == Some(&codec::Nbt::from("commands.list.players"))) =>
                    {
                        return Some(());
                    }
                    _ => {}
                }
                None
            });
            assert_eq!(broke, hand == play::Hand::OffHand);
        }
        if hand == play::Hand::MainHand {
            move_player(&mut attacker, [x, y, z - 2.0], true);
        }
        play_packet(&mut attacker, play::serverbound::SetHeldItem { slot: 1 });
        std::thread::sleep(Duration::from_millis(800));
        play_packet(&mut attacker, attack);
        assert_eq!(health(&mut defender).health, 16.0);
    }
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn shield_blocking_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("shield-blocking");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    let item = |slot, id| Compound::from_iter([("Slot", Nbt::Byte(slot)), ("id", Nbt::from(id))]);
    let mut sword = item(0, "minecraft:wooden_sword");
    sword.insert(
        "components",
        Compound::from_iter([(
            "minecraft:enchantments",
            Compound::from_iter([("minecraft:fire_aspect", 1), ("minecraft:knockback", 2)]),
        )]),
    );
    let mut fragile = Compound::from_iter([("id", "minecraft:shield")]);
    fragile.insert(
        "components",
        Compound::from_iter([("minecraft:max_damage", 5)]),
    );
    let players = [
        (
            "ShieldAttacker",
            "Inventory",
            Nbt::List(List::Compound(vec![
                sword,
                item(1, "minecraft:wooden_sword"),
            ])),
        ),
        (
            "ShieldMain",
            "Inventory",
            Nbt::List(List::Compound(vec![item(0, "minecraft:shield")])),
        ),
        (
            "ShieldOffhand",
            "equipment",
            Compound::from_iter([("offhand", fragile)]).into(),
        ),
    ];
    for dir in [&vanilla, &harpas] {
        for (name, key, value) in &players {
            let saved = Compound::from_iter([
                ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
                (*key, value.clone()),
            ]);
            harpas_anvil::write_player(&dir.join("world"), offline_uuid(name), &saved).unwrap();
        }
    }
    exercise_shields(port);
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    exercise_shields(other);
    stop(&mut server);

    let saved_items = |dir: &Path, name, key| {
        let (saved, _) = harpas_anvil::read_player(&dir.join("world"), offline_uuid(name))
            .unwrap()
            .unwrap();
        saved.get(key).cloned()
    };
    for (name, key, _) in players {
        assert_eq!(
            saved_items(&harpas, name, key),
            saved_items(&vanilla, name, key),
            "{name} {key}"
        );
    }
}

fn shield_cooldown(client: &mut Client) -> i32 {
    client.matching("shield cooldown", |packet| match packet {
        Packet::SetCooldown(cooldown) if cooldown.group == "minecraft:shield" => {
            Some(cooldown.ticks)
        }
        Packet::SetHealth(health) => {
            panic!("unexpected health waiting for shield cooldown: {health:?}")
        }
        _ => None,
    })
}

const SWEEP_SOUND_ID: i32 = 1302;

#[derive(Debug, Default, PartialEq)]
struct SweepEffects {
    particles: Vec<play::clientbound::LevelParticles>,
    sounds: Vec<play::clientbound::SoundEffect>,
    health: Option<f32>,
    velocity: Option<[f64; 3]>,
}

fn sweep_effects(client: &mut Client, id: i32) -> SweepEffects {
    let mut effects = SweepEffects::default();
    client.send_command("list");
    client.matching("packets after the sword strike", |packet| {
        match packet {
            Packet::LevelParticles(particles)
                if particles.particle.id == play::Particle::SWEEP_ATTACK =>
            {
                effects.particles.push(particles);
            }
            Packet::SoundEffect(mut sound)
                if sound.sound == codec::Holder::Reference(SWEEP_SOUND_ID) =>
            {
                sound.seed = 0;
                effects.sounds.push(sound);
            }
            Packet::SetHealth(health) => effects.health = Some(health.health),
            Packet::SetEntityVelocity(velocity) if velocity.entity_id == id => {
                let codec::LpVec3 { x, y, z } = velocity.velocity;
                effects.velocity = Some([x, y, z]);
            }
            Packet::SystemChatMessage(reply)
                if matches!(&reply.content, codec::Nbt::Compound(text)
                    if text.get("translate") == Some(&codec::Nbt::from("commands.list.players"))) =>
            {
                return Some(());
            }
            _ => {}
        }
        None
    });
    effects
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SweepCase {
    Near,
    OutsideBox,
    OutsideReach,
    Walking,
    FastMovement,
    Sprint,
    Critical,
    PartialCharge,
    SweepingEdge(i16),
    SweepingEdgeWithSharpness,
    EquipmentChange,
}

fn exercise_sweeps(
    port: u16,
    names: [&str; 3],
    cases: &[SweepCase],
) -> Vec<(SweepCase, [SweepEffects; 3])> {
    use SweepCase::*;
    let join = |name| {
        let mut client = Client::join(port, name);
        client.stream.set_nodelay(true).unwrap();
        let id = client.matching("Play Login", |packet| match packet {
            Packet::Login(login) => Some(login.entity_id),
            _ => None,
        });
        let position = confirm_position(&mut client);
        assert_eq!(health(&mut client).health, 20.0);
        (client, id, position)
    };
    let [
        (mut attacker, attacker_id, [x, y, z]),
        (mut primary, primary_id, _),
        (mut secondary, secondary_id, _),
    ] = names.map(join);
    play_packet(
        &mut attacker,
        play::serverbound::SetPlayerRotation {
            yaw: 0.0,
            pitch: 0.0,
            flags: play::MovementFlags {
                on_ground: true,
                ..Default::default()
            },
        },
    );
    let sprint = play::serverbound::PlayerCommand {
        entity_id: attacker_id,
        action: play::PlayerCommandAction::StartSprinting,
        data: 0,
    };
    let attack = |entity_id| play::serverbound::Attack { entity_id };
    cases
        .iter()
        .filter_map(|&case| {
            move_player(&mut attacker, [x, y, z], true);
            // A second position report records zero movement after returning to the starting position.
            move_player(&mut attacker, [x, y, z], true);
            move_player(&mut primary, [x, y, z + 1.5], true);
            let [neighbor_x, neighbor_z] = match case {
                OutsideBox => [2.0, 1.5],
                OutsideReach => [0.0, 3.0],
                _ => [0.8, 1.5],
            };
            move_player(&mut secondary, [x + neighbor_x, y, z + neighbor_z], true);
            let slot = match case {
                SweepingEdge(level) => level,
                SweepingEdgeWithSharpness => 4,
                _ => 0,
            };
            if slot != 0 {
                play_packet(&mut attacker, play::serverbound::SetHeldItem { slot });
            }
            std::thread::sleep(Duration::from_millis(800));
            match case {
                Sprint => play_packet(&mut attacker, sprint.clone()),
                Critical => {
                    for height in [0.42, 0.75, 0.60] {
                        move_player(&mut attacker, [x, y + height, z], false);
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
                Walking | FastMovement => {
                    let dx = if case == Walking { 0.24 } else { 0.26 };
                    move_player(&mut attacker, [x + dx, y, z], true);
                    std::thread::sleep(Duration::from_millis(150));
                }
                _ => {}
            }
            let before_attack = match case {
                // Spend the charge on a sprint strike before the immediate hit on the other player.
                PartialCharge => vec![sprint.clone().into(), attack(secondary_id).into()],
                EquipmentChange => vec![play::serverbound::SetHeldItem { slot: 4 }.into()],
                _ => Vec::new(),
            };
            let mut frames = Vec::new();
            for packet in before_attack.into_iter().chain([attack(primary_id).into()]) {
                write_frame(&mut frames, |buf| {
                    play::serverbound::Packet::encode(&packet, buf)
                });
            }
            attacker.stream.write_all(&frames).unwrap();
            // Let the attack run and vanilla flush its end-of-tick player velocity before the list barriers.
            std::thread::sleep(Duration::from_millis(200));
            let mut effects = [
                sweep_effects(&mut attacker, attacker_id),
                sweep_effects(&mut primary, primary_id),
                sweep_effects(&mut secondary, secondary_id),
            ];
            // A hit before the equipment tick uses the wooden sword's charged attributes and the held
            // iron sword's Sharpness. After that tick the different weapon is still recharging.
            let before_equipment_tick = case == EquipmentChange && effects[1].health == Some(3.0);
            let sweeps = before_equipment_tick
                || matches!(
                    case,
                    Near | OutsideBox
                        | OutsideReach
                        | Walking
                        | SweepingEdge(_)
                        | SweepingEdgeWithSharpness
                );
            for (name, effect) in names.into_iter().zip(&effects) {
                assert_eq!(
                    effect.particles.len(),
                    usize::from(sweeps),
                    "{case:?} particle for {name}"
                );
                assert_eq!(
                    effect.sounds.len(),
                    usize::from(sweeps),
                    "{case:?} sound for {name}"
                );
            }
            assert_eq!(effects[0].health, None, "{case:?} attacker health");
            assert!(effects[1].health.is_some(), "{case:?} primary health");
            match case {
                PartialCharge => {
                    // A tick can fall between the two attack packets.
                    assert!((1.0..1.2).contains(&effects[1].health.take().unwrap()));
                }
                Near | Walking => {
                    assert_eq!(effects[2].health, Some(19.0), "{case:?} secondary health");
                }
                SweepingEdge(level) => {
                    assert_eq!(effects[1].health, Some(20.0 - 4.0 * f32::from(level)));
                    assert!(effects[2].health.is_some(), "{case:?} secondary health");
                }
                SweepingEdgeWithSharpness => {
                    assert_eq!(effects[1].health, Some(1.0));
                    assert!(effects[2].health.is_some(), "{case:?} secondary health");
                }
                EquipmentChange => {
                    if before_equipment_tick {
                        assert_eq!(effects[2].health, Some(17.0));
                    } else {
                        assert!((6.0..7.0).contains(&effects[1].health.unwrap()));
                        assert_eq!(effects[2].health, None);
                    }
                    return None;
                }
                _ => assert_eq!(effects[2].health, None, "{case:?} secondary health"),
            }
            Some((case, effects))
        })
        .collect()
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn sweeping_attacks_match_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("sweeping-attacks");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    let sword = Compound::from_iter([
        ("Slot", Nbt::Byte(0)),
        ("id", Nbt::from("minecraft:wooden_sword")),
    ]);
    let enchanted = |slot: i8, material, enchantments: Compound| {
        Compound::from_iter([
            ("Slot", Nbt::Byte(slot)),
            ("id", Nbt::from(material)),
            (
                "components",
                Compound::from_iter([("minecraft:enchantments", enchantments)]).into(),
            ),
        ])
    };
    let mut enchanted_hotbar: Vec<_> = (1_i8..=3)
        .map(|level| {
            enchanted(
                level,
                "minecraft:wooden_sword",
                Compound::from_iter([("minecraft:sweeping_edge", i32::from(level))]),
            )
        })
        .collect();
    enchanted_hotbar.push(enchanted(
        4,
        "minecraft:iron_sword",
        Compound::from_iter([("minecraft:sweeping_edge", 3), ("minecraft:sharpness", 1)]),
    ));
    use SweepCase::*;
    let trials = [
        (
            ["Sweeper", "SweepTarget", "SweepNeighbor"],
            &[Near, OutsideBox, Sprint, Critical, PartialCharge][..],
        ),
        (
            ["WalkingSweeper", "WalkingTarget", "WalkingNeighbor"],
            &[Walking, FastMovement, OutsideReach, EquipmentChange][..],
        ),
        (
            ["EdgeSweeper", "EdgeTarget", "EdgeNeighbor"],
            &[
                SweepingEdge(1),
                SweepingEdge(2),
                SweepingEdge(3),
                SweepingEdgeWithSharpness,
            ][..],
        ),
    ];
    let mut offhand = enchanted_hotbar[2].clone();
    offhand.remove("Slot");
    let data = |hotbar| {
        Compound::from_iter([
            ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
            ("Inventory", List::Compound(hotbar).into()),
        ])
    };
    let mut walking = data(vec![sword.clone(), enchanted_hotbar[3].clone()]);
    walking.insert("equipment", Compound::from_iter([("offhand", offhand)]));
    let inventories = [data(vec![sword]), walking, data(enchanted_hotbar)];
    for dir in [&vanilla, &harpas] {
        for ((names, _), data) in trials.into_iter().zip(&inventories) {
            harpas_anvil::write_player(&dir.join("world"), offline_uuid(names[0]), data).unwrap();
        }
    }
    let expected = trials.map(|(names, cases)| exercise_sweeps(port, names, cases));
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    for ((names, cases), expected) in trials.into_iter().zip(expected) {
        let actual = exercise_sweeps(other, names, cases);
        for ((case, actual), (_, expected)) in actual.into_iter().zip(expected) {
            assert_eq!(actual, expected, "{case:?}");
        }
    }
    stop(&mut server);
}

fn exercise_axes(port: u16) -> [play::AttributeProperty; 2] {
    let mut attacker = Client::join(port, "AxeAttacker");
    attacker.stream.set_nodelay(true).unwrap();
    let attacker_id = attacker.matching("attacker Play Login", |packet| match packet {
        Packet::Login(login) => Some(login.entity_id),
        _ => None,
    });
    confirm_position(&mut attacker);
    let mut previous_speed = None;

    [
        ("AxeShieldMain", play::Hand::MainHand, 1, 7.0),
        ("AxeShieldOff", play::Hand::OffHand, 2, 9.0),
    ]
    .map(|(name, hand, slot, damage)| {
        let mut defender = Client::join(port, name);
        defender.stream.set_nodelay(true).unwrap();
        let entity_id = defender.matching("Play Login", |packet| match packet {
            Packet::Login(login) => Some(login.entity_id),
            _ => None,
        });
        let [x, y, z] = confirm_position(&mut defender);
        assert_eq!(health(&mut defender).health, 20.0);
        move_player(&mut defender, [x, y, z], true);
        move_player(&mut attacker, [x, y, z + 2.0], true);

        play_packet(
            &mut defender,
            play::serverbound::SetPlayerRotation {
                yaw: 0.0,
                pitch: 0.0,
                flags: play::MovementFlags {
                    on_ground: true,
                    ..Default::default()
                },
            },
        );
        use_food(&mut defender, hand, 1);
        std::thread::sleep(Duration::from_millis(400));
        play_packet(&mut attacker, play::serverbound::SetHeldItem { slot });
        let speed = attacker.matching("new axe attack speed", |packet| match packet {
            Packet::UpdateAttributes(update) if update.entity_id == attacker_id => {
                update.attributes.into_iter().find(|attribute| {
                    attribute.attribute == play::Attribute::AttackSpeed
                        && !attribute.modifiers.is_empty()
                        && previous_speed.as_ref() != Some(attribute)
                })
            }
            _ => None,
        });
        previous_speed = Some(speed.clone());
        if hand == play::Hand::MainHand {
            std::thread::sleep(Duration::from_millis(1500));
        }
        let attack =
            |client: &mut Client| play_packet(client, play::serverbound::Attack { entity_id });
        attack(&mut attacker);
        assert_eq!(shield_cooldown(&mut defender), 100);
        assert_no_damage_until_list_reply(&mut defender, "after the disabling hit");

        // Try to raise the shield during its cooldown, then strike again at full charge.
        use_food(&mut defender, hand, 2);
        std::thread::sleep(Duration::from_millis(1500));
        attack(&mut attacker);
        assert_eq!(health(&mut defender).health, 20.0 - damage);

        assert_eq!(shield_cooldown(&mut defender), 0);
        use_food(&mut defender, hand, 3);
        std::thread::sleep(Duration::from_millis(400));
        attack(&mut attacker);
        assert_eq!(shield_cooldown(&mut defender), 100);
        assert_no_damage_until_list_reply(&mut defender, "after blocking with a recovered shield");
        speed
    })
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn axe_combat_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("axe-combat");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    let item = |slot, id| Compound::from_iter([("Slot", Nbt::Byte(slot)), ("id", Nbt::from(id))]);
    let players: [(&str, &str, Nbt); 3] = [
        (
            "AxeAttacker",
            "Inventory",
            List::Compound(vec![
                item(1, "minecraft:wooden_axe"),
                item(2, "minecraft:iron_axe"),
            ])
            .into(),
        ),
        (
            "AxeShieldMain",
            "Inventory",
            List::Compound(vec![item(0, "minecraft:shield")]).into(),
        ),
        (
            "AxeShieldOff",
            "equipment",
            Compound::from_iter([("offhand", Compound::from_iter([("id", "minecraft:shield")]))])
                .into(),
        ),
    ];
    for dir in [&vanilla, &harpas] {
        for (name, key, value) in &players {
            let saved = Compound::from_iter([
                ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
                (*key, value.clone()),
            ]);
            harpas_anvil::write_player(&dir.join("world"), offline_uuid(name), &saved).unwrap();
        }
    }
    let expected = exercise_axes(port);
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    assert_eq!(exercise_axes(other), expected);
    stop(&mut server);

    for (name, key, _) in players {
        let [vanilla, harpas] = [&vanilla, &harpas].map(|dir| {
            let (saved, _) = harpas_anvil::read_player(&dir.join("world"), offline_uuid(name))
                .unwrap()
                .unwrap();
            saved.get(key).cloned()
        });
        assert_eq!(harpas, vanilla, "{name} {key}");
    }
}

fn exercise_armored_combat(server: &mut Server, port: u16) -> [[f32; 2]; 3] {
    let mut attacker = Client::join(port, "Attacker");
    attacker.stream.set_nodelay(true).unwrap();
    confirm_position(&mut attacker);
    ["Unarmored", "IronArmor", "DiamondArmor"].map(|name| {
        let mut defender = Client::join(port, name);
        defender.stream.set_nodelay(true).unwrap();
        let entity_id = defender.matching("Play Login", |packet| match packet {
            Packet::Login(login) => Some(login.entity_id),
            _ => None,
        });
        let [x, y, z] = confirm_position(&mut defender);
        assert_eq!(health(&mut defender).health, 20.0);
        move_player(&mut defender, [x, y, z], true);
        move_player(&mut attacker, [x + 2.0, y, z], true);
        let mut result = [0.0; 2];
        for (hit, remaining) in result.iter_mut().enumerate() {
            std::thread::sleep(Duration::from_millis(800));
            play_packet(&mut attacker, play::serverbound::Attack { entity_id });
            if name == "DiamondArmor" && hit == 0 {
                let (mut broken, mut empty, mut updated, mut health) = (false, false, false, None);
                *remaining =
                    defender.matching("broken chestplate, inventory, armor and health", |packet| {
                        match packet {
                            Packet::EntityEvent(event)
                                if event.entity_id == entity_id && event.event == 50 =>
                            {
                                broken = true
                            }
                            Packet::SetContainerSlot(change)
                                if change.window_id == 0 && change.slot == 6 =>
                            {
                                assert_eq!(change.item.count, 0);
                                empty = true;
                            }
                            Packet::UpdateAttributes(packet) if packet.entity_id == entity_id => {
                                updated |= packet.attributes.iter().any(|attr| {
                                    attr.attribute == play::Attribute::Armor
                                        && attr.base_value
                                            + attr.modifiers.iter().map(|m| m.amount).sum::<f64>()
                                            == 12.0
                                });
                            }
                            Packet::SetHealth(change) => health = Some(change.health),
                            _ => {}
                        }
                        if broken && empty && updated {
                            health
                        } else {
                            None
                        }
                    });
            } else {
                *remaining = health(&mut defender).health;
            }
        }
        drop(defender);
        server.wait_for(&format!("{name} left the game"));
        let mut defender = Client::join(port, name);
        confirm_position(&mut defender);
        assert_eq!(health(&mut defender).health, result[1]);
        drop(defender);
        server.wait_for(&format!("{name} left the game"));
        result
    })
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn armored_combat_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("armored-combat");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    let slots = [
        ("head", "helmet"),
        ("chest", "chestplate"),
        ("legs", "leggings"),
        ("feet", "boots"),
    ];
    for dir in [&vanilla, &harpas] {
        let write = |name, field, value| {
            let data = Compound::from_iter([
                ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
                (field, value),
            ]);
            harpas_anvil::write_player(&dir.join("world"), offline_uuid(name), &data).unwrap();
        };
        write(
            "Attacker",
            "Inventory",
            Nbt::List(List::Compound(vec![Compound::from_iter([
                ("Slot", Nbt::Byte(0)),
                ("id", "minecraft:netherite_sword".into()),
            ])])),
        );
        for (name, material) in [("IronArmor", "iron"), ("DiamondArmor", "diamond")] {
            let equipment = Compound::from_iter(slots.map(|(slot, piece)| {
                let mut stack =
                    Compound::from_iter([("id", format!("minecraft:{material}_{piece}"))]);
                if name == "DiamondArmor" && slot == "chest" {
                    stack.insert(
                        "components",
                        Compound::from_iter([("minecraft:max_damage", 2)]),
                    );
                }
                (slot, stack)
            }));
            write(name, "equipment", equipment.into());
        }
    }

    let expected = exercise_armored_combat(&mut reference, port);
    assert_eq!(expected[0], [12.0, 4.0]);
    assert!(expected[0][0] < expected[1][0] && expected[1][0] < expected[2][0]);
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    assert_eq!(exercise_armored_combat(&mut server, other), expected);
    stop(&mut server);
    for (name, piece_count) in [("IronArmor", 4), ("DiamondArmor", 3)] {
        let [expected, actual] = [&vanilla, &harpas].map(|dir| {
            let (saved, _) = harpas_anvil::read_player(&dir.join("world"), offline_uuid(name))
                .unwrap()
                .unwrap();
            let Some(Nbt::Compound(equipment)) = saved.get("equipment") else {
                panic!("missing saved armor for {name}");
            };
            equipment.clone()
        });
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), piece_count);
        for (_, stack) in actual.iter() {
            let Nbt::Compound(stack) = stack else {
                panic!("invalid saved armor for {name}");
            };
            assert_eq!(
                stack.get("components"),
                Some(&Nbt::from(Compound::from_iter([("minecraft:damage", 4)])))
            );
        }
    }
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn protection_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("protection");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    let cases = [
        ("Plain", [0; 4]),
        ("Single", [0, 4, 0, 0]),
        ("Full", [4; 4]),
        ("AtLimit", [5; 4]),
        ("OverLimit", [6; 4]),
        ("Offhand", [0; 4]),
        ("Fragile", [0, 4, 0, 0]),
        ("WithEffects", [4; 4]),
    ];
    let armor = |piece, level, fragile| {
        let mut stack = Compound::from_iter([("id", format!("minecraft:iron_{piece}"))]);
        let mut components = Compound::new();
        if level > 0 {
            components.insert(
                "minecraft:enchantments",
                Compound::from_iter([("minecraft:protection", level)]),
            );
        }
        if fragile {
            components.insert("minecraft:max_damage", 2);
        }
        if !components.is_empty() {
            stack.insert("components", components);
        }
        stack
    };
    let attacker = Compound::from_iter([
        ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
        (
            "Inventory",
            Nbt::List(List::Compound(vec![Compound::from_iter([
                ("Slot", Nbt::Byte(0)),
                ("id", "minecraft:netherite_sword".into()),
            ])])),
        ),
    ]);
    for dir in [&vanilla, &harpas] {
        let world = dir.join("world");
        harpas_anvil::write_player(&world, offline_uuid("Attacker"), &attacker).unwrap();
        for (name, levels) in cases {
            let mut equipment = Compound::from_iter(
                [
                    ("head", "helmet"),
                    ("chest", "chestplate"),
                    ("legs", "leggings"),
                    ("feet", "boots"),
                ]
                .into_iter()
                .zip(levels)
                .map(|((slot, piece), level)| {
                    (
                        slot,
                        armor(piece, level, name == "Fragile" && slot == "chest"),
                    )
                }),
            );
            if name == "Offhand" {
                equipment.insert("offhand", armor("helmet", 20, false));
            }
            let mut data = Compound::from_iter([
                ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
                ("equipment", equipment.into()),
            ]);
            if name == "WithEffects" {
                data.insert("AbsorptionAmount", 4.0_f32);
                data.insert(
                    "active_effects",
                    List::Compound(
                        ["minecraft:resistance", "minecraft:absorption"]
                            .map(|id| {
                                Compound::from_iter([
                                    ("id", Nbt::from(id)),
                                    ("duration", 1200.into()),
                                ])
                            })
                            .into(),
                    ),
                );
                data.insert(
                    "attributes",
                    List::Compound(vec![Compound::from_iter([
                        ("id", "minecraft:max_absorption".into()),
                        ("base", Nbt::Double(0.0)),
                        (
                            "modifiers",
                            List::Compound(vec![Compound::from_iter([
                                ("id", "minecraft:effect.absorption".into()),
                                ("amount", Nbt::Double(4.0)),
                                ("operation", "add_value".into()),
                            ])])
                            .into(),
                        ),
                    ])]),
                );
            }
            harpas_anvil::write_player(&world, offline_uuid(name), &data).unwrap();
        }
    }

    let exercise = |server: &mut Server, port| {
        let mut attacker = Client::join(port, "Attacker");
        attacker.stream.set_nodelay(true).unwrap();
        confirm_position(&mut attacker);
        for (name, _) in cases {
            let mut defender = Client::join(port, name);
            let entity_id = defender.matching("Play Login", |packet| match packet {
                Packet::Login(login) => Some(login.entity_id),
                _ => None,
            });
            let [x, y, z] = confirm_position(&mut defender);
            move_player(&mut defender, [x, y, z], true);
            move_player(&mut attacker, [x + 2.0, y, z], true);
            std::thread::sleep(Duration::from_millis(800));
            play_packet(&mut attacker, play::serverbound::Attack { entity_id });
            defender.matching("sword hit", |packet| match packet {
                Packet::DamageEvent(event) if event.entity_id == entity_id => Some(()),
                _ => None,
            });
            drop(defender);
            server.wait_for(&format!("{name} left the game"));
        }
    };
    exercise(&mut reference, port);
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    exercise(&mut server, other);
    stop(&mut server);

    let expected_health = cases.map(|(name, _)| {
        let [expected, actual] = [&vanilla, &harpas].map(|dir| {
            harpas_anvil::read_player(&dir.join("world"), offline_uuid(name))
                .unwrap()
                .unwrap()
                .0
        });
        for field in ["Health", "AbsorptionAmount", "equipment"] {
            assert_eq!(actual.get(field), expected.get(field), "{name} {field}");
        }
        if name == "WithEffects" {
            assert!(matches!(expected.get("AbsorptionAmount"), Some(Nbt::Float(left)) if *left > 0.0 && *left < 4.0));
        }
        let Some(Nbt::Float(health)) = expected.get("Health") else {
            panic!("missing saved health for {name}");
        };
        *health
    });
    let [
        plain,
        single,
        full,
        limit,
        over,
        offhand,
        fragile,
        with_effects,
    ] = expected_health;
    assert!(plain < single && single < full && full < limit);
    assert_eq!(limit, over);
    assert_eq!(plain, offhand);
    assert_eq!(plain, fragile);
    assert_eq!(with_effects, 20.0);
}

fn exercise_sharpness(port: u16) -> Vec<f32> {
    let mut attacker = Client::join(port, "Attacker");
    attacker.stream.set_nodelay(true).unwrap();
    let [x, y, z] = confirm_position(&mut attacker);
    move_player(&mut attacker, [x, y, z], true);
    let mut defenders = [("Unarmored", x - 1.0), ("IronArmor", x + 1.0)].map(|(name, x)| {
        let mut client = Client::join(port, name);
        client.stream.set_nodelay(true).unwrap();
        let entity_id = client.matching("Play Login", |packet| match packet {
            Packet::Login(login) => Some(login.entity_id),
            _ => None,
        });
        confirm_position(&mut client);
        assert_eq!(health(&mut client).health, 20.0);
        move_player(&mut client, [x, y, z], true);
        (client, entity_id)
    });
    let speed = attack_speed(&mut attacker, true);
    let recharge_ticks = 20.0 / (speed.base_value + speed.modifiers[0].amount) as f32;
    let mut remaining = [20.0; 2];
    let mut full_hits = Vec::new();
    for (slot, bonus) in [(0, 1.0), (1, 3.0)] {
        play_packet(&mut attacker, play::serverbound::SetHeldItem { slot });
        for full in 0..defenders.len() {
            std::thread::sleep(Duration::from_millis(800));
            let partial = 1 - full;
            // The first attack resets the charge. A different target lets the second take damage immediately.
            let mut frames = Vec::new();
            for index in [full, partial] {
                let packet = play::serverbound::Packet::from(play::serverbound::Attack {
                    entity_id: defenders[index].1,
                });
                write_frame(&mut frames, |buf| packet.encode(buf));
            }
            attacker.stream.write_all(&frames).unwrap();

            let after_full = health(&mut defenders[full].0).health;
            let damage = remaining[full] - after_full;
            if full == 0 {
                assert!((damage - (4.0 + bonus)).abs() < 0.0001);
            } else {
                assert!(damage < 4.0 + bonus);
            }
            full_hits.push(damage);
            remaining[full] = after_full;

            let after_partial = health(&mut defenders[partial].0).health;
            // Both frames are sent together, but a server tick may fall between their handlers.
            assert!(
                (0..3).any(|ticks| {
                    let charge = (ticks as f32 + 0.5) / recharge_ticks;
                    let raw = 4.0 * (0.2 + charge * charge * 0.8) + bonus * charge;
                    let damage = if partial == 1 {
                        // The iron chestplate gives six armor points and no toughness.
                        raw * (1.0 - (6.0 - raw / 2.0) / 25.0)
                    } else {
                        raw
                    };
                    (after_partial - (remaining[partial] - damage)).abs() < 0.0001
                }),
                "unexpected partial damage with Sharpness bonus {bonus} on defender {partial}: {}",
                remaining[partial] - after_partial
            );
            remaining[partial] = after_partial;
        }
    }
    full_hits
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn sharpness_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("sharpness");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    let sword = |level: i32| {
        Compound::from_iter([
            ("id", Nbt::from("minecraft:wooden_sword")),
            (
                "components",
                Compound::from_iter([(
                    "minecraft:enchantments",
                    Compound::from_iter([("minecraft:sharpness", level)]),
                )])
                .into(),
            ),
        ])
    };
    let hotbar = [(0_i8, 1), (1, 5)].map(|(slot, level)| {
        let mut stack = sword(level);
        stack.insert("Slot", slot);
        stack
    });
    let attacker = Compound::from_iter([
        ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
        ("Inventory", List::Compound(hotbar.into()).into()),
        (
            "equipment",
            Compound::from_iter([("offhand", sword(5))]).into(),
        ),
    ]);
    let armor = Compound::from_iter([
        ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
        (
            "equipment",
            Compound::from_iter([(
                "chest",
                Compound::from_iter([("id", "minecraft:iron_chestplate")]),
            )])
            .into(),
        ),
    ]);
    for dir in [&vanilla, &harpas] {
        let world = dir.join("world");
        harpas_anvil::write_player(&world, offline_uuid("Attacker"), &attacker).unwrap();
        harpas_anvil::write_player(&world, offline_uuid("IronArmor"), &armor).unwrap();
    }
    let expected = exercise_sharpness(port);
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    let actual = exercise_sharpness(other);
    stop(&mut server);
    for (actual, expected) in actual.into_iter().zip(expected) {
        assert!((actual - expected).abs() < 0.0001, "{actual} != {expected}");
    }
}

fn expect_critical_animation(attacker: &mut Client, expected: Option<i32>) {
    attacker.send_command("list");
    let mut critical = None;
    attacker.matching("list after the attack", |packet| match packet {
        Packet::EntityAnimation(packet) if packet.animation == play::Animation::CriticalHit => {
            assert!(
                critical.replace(packet.entity_id).is_none(),
                "extra critical hit"
            );
            None
        }
        Packet::SystemChatMessage(reply)
            if matches!(&reply.content, codec::Nbt::Compound(text)
                if text.get("translate") == Some(&codec::Nbt::from("commands.list.players"))) =>
        {
            Some(())
        }
        _ => None,
    });
    assert_eq!(critical, expected);
}

fn exercise_critical_hits(port: u16) -> Vec<f32> {
    let join = |name| {
        let mut client = Client::join(port, name);
        client.stream.set_nodelay(true).unwrap();
        let id = client.matching("Play Login", |packet| match packet {
            Packet::Login(login) => Some(login.entity_id),
            _ => None,
        });
        let position = confirm_position(&mut client);
        assert_eq!(health(&mut client).health, 20.0);
        (client, id, position)
    };
    let (mut attacker, attacker_id, [x, y, z]) = join("Attacker");
    let (mut defender, defender_id, _) = join("Defender");
    let (mut armored, armored_id, _) = join("Armored");
    move_player(&mut defender, [x - 1.5, y, z], true);
    move_player(&mut armored, [x + 1.5, y, z], true);
    let sprint = |attacker: &mut Client, action| {
        play_packet(
            attacker,
            play::serverbound::PlayerCommand {
                entity_id: attacker_id,
                action,
                data: 0,
            },
        );
    };
    let jump = |attacker: &mut Client, z, heights: &[f64]| {
        for &height in heights {
            move_player(attacker, [x, y + height, z], height == 0.0);
            std::thread::sleep(Duration::from_millis(50));
        }
    };

    let descent = &[0.42, 0.75, 0.60][..];
    let mut health_after = Vec::new();
    for (case, heights, offset, sprinting, damage) in [
        ("on the ground", &[][..], 0.0, false, 1.0),
        ("rising", &[0.42][..], 0.0, false, 1.0),
        ("descending", descent, 0.0, false, 1.5),
        ("sprinting", descent, 0.0, true, 1.0),
        (
            "after landing",
            &[0.42, 0.75, 0.60, 0.0][..],
            0.0,
            false,
            1.0,
        ),
        ("on a ladder", descent, 1.0, false, 1.0),
        ("on the trapdoor", &[0.5, 1.5, 1.25][..], 1.0, false, 1.0),
    ] {
        move_player(&mut attacker, [x, y, z], true);
        std::thread::sleep(Duration::from_millis(800));
        if sprinting {
            sprint(&mut attacker, play::PlayerCommandAction::StartSprinting);
        }
        jump(&mut attacker, z + offset, heights);
        play_packet(
            &mut attacker,
            play::serverbound::Attack {
                entity_id: defender_id,
            },
        );
        let before = health_after.last().copied().unwrap_or(20.0);
        let after = health(&mut defender).health;
        assert_eq!(after, before - damage, "punch {case}");
        health_after.push(after);
        expect_critical_animation(&mut attacker, (damage > 1.0).then_some(defender_id));
        if sprinting {
            sprint(&mut attacker, play::PlayerCommandAction::StopSprinting);
        }
    }

    let (mut blinded, _, _) = join("Blinded");
    std::thread::sleep(Duration::from_millis(800));
    jump(&mut blinded, z, descent);
    play_packet(
        &mut blinded,
        play::serverbound::Attack {
            entity_id: defender_id,
        },
    );
    let before = *health_after.last().unwrap();
    let after = health(&mut defender).health;
    assert_eq!(after, before - 1.0, "punch with Blindness");
    health_after.push(after);
    expect_critical_animation(&mut blinded, None);
    drop(blinded);

    move_player(&mut attacker, [x, y, z], true);
    play_packet(&mut attacker, play::serverbound::SetHeldItem { slot: 1 });
    let speed = attack_speed(&mut attacker, true);
    std::thread::sleep(Duration::from_millis(800));
    jump(&mut attacker, z, descent);
    let mut frames = Vec::new();
    // The charged hit resets the cooldown. A second target can take an immediate uncharged hit.
    for entity_id in [armored_id, defender_id] {
        let packet = play::serverbound::Packet::from(play::serverbound::Attack { entity_id });
        write_frame(&mut frames, |buf| packet.encode(buf));
    }
    attacker.stream.write_all(&frames).unwrap();
    let armored_health = health(&mut armored).health;
    let raw = 4.0 * 1.5 + 3.0;
    let expected = 20.0 - raw * (1.0 - (6.0 - raw / 2.0) / 25.0);
    assert!(
        (armored_health - expected).abs() < 0.0001,
        "armored critical hit left {armored_health} health, expected {expected}"
    );
    let partial = health(&mut defender).health;
    let recharge_ticks = 20.0 / (speed.base_value + speed.modifiers[0].amount) as f32;
    assert!(
        (0..3).any(|ticks| {
            let charge = (ticks as f32 + 0.5) / recharge_ticks;
            let damage = 4.0 * (0.2 + charge * charge * 0.8) + 3.0 * charge;
            (partial - (after - damage)).abs() < 0.0001
        }),
        "unexpected uncharged sword damage: {}",
        after - partial
    );
    expect_critical_animation(&mut attacker, Some(armored_id));
    health_after.push(armored_health);
    health_after
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn critical_hits_match_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("critical-hits");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    let mut setup = Client::join(port, "TerrainSetup");
    confirm_position(&mut setup);
    setup.matching(
        "chunk containing the ladder fixture",
        |packet| match packet {
            Packet::ChunkDataAndUpdateLight(chunk) if (chunk.x, chunk.z) == (0, 0) => Some(()),
            _ => None,
        },
    );
    writeln!(
        reference.child.stdin.as_mut().unwrap(),
        "gamerule minecraft:fall_damage false\n\
         setblock 0 -60 2 minecraft:stone\n\
         setblock 0 -60 1 minecraft:ladder[facing=north]\n\
         setblock 0 -59 1 minecraft:oak_trapdoor[facing=north,open=true]\n\
         save-all flush"
    )
    .unwrap();
    reference.wait_for("fall_damage is now set to: false");
    for _ in 0..3 {
        reference.wait_for("Changed the block");
    }
    reference.wait_for("Saved the game");
    drop(setup);
    reference.wait_for("TerrainSetup left the game");
    let sword = Compound::from_iter([
        ("Slot", Nbt::Byte(1)),
        ("id", Nbt::from("minecraft:wooden_sword")),
        (
            "components",
            Compound::from_iter([(
                "minecraft:enchantments",
                Compound::from_iter([("minecraft:sharpness", 5)]),
            )])
            .into(),
        ),
    ]);
    let attacker = Compound::from_iter([
        ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
        ("Inventory", List::Compound(vec![sword]).into()),
    ]);
    let armored = Compound::from_iter([
        ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
        (
            "equipment",
            Compound::from_iter([(
                "chest",
                Compound::from_iter([("id", "minecraft:iron_chestplate")]),
            )])
            .into(),
        ),
    ]);
    let blinded = Compound::from_iter([
        ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
        (
            "active_effects",
            List::Compound(vec![Compound::from_iter([
                ("id", Nbt::from("minecraft:blindness")),
                ("duration", Nbt::Int(-1)),
            ])])
            .into(),
        ),
    ]);
    for dir in [&vanilla, &harpas] {
        for (name, data) in [
            ("Attacker", &attacker),
            ("Armored", &armored),
            ("Blinded", &blinded),
        ] {
            harpas_anvil::write_player(&dir.join("world"), offline_uuid(name), data).unwrap();
        }
    }
    let expected = exercise_critical_hits(port);
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    assert_eq!(exercise_critical_hits(other), expected);
    stop(&mut server);
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
    std::thread::sleep(Duration::from_millis(2200));
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

fn exercise_suspicious_stew(
    server: &mut Server,
    port: u16,
) -> [play::clientbound::EntityEffect; 2] {
    let mut client = Client::join(port, "SuspiciousEater");
    confirm_position(&mut client);
    assert_eq!(health(&mut client).food, 14);
    let bowl = harpas_core::item::items().id("minecraft:bowl").unwrap() as i32;
    let mut applied = [
        (play::Hand::MainHand, 36, 1, 160),
        (play::Hand::OffHand, 45, 2, 210),
    ]
    .map(|(hand, slot, sequence, duration)| {
        use_food(&mut client, hand, sequence);
        let (mut effect, mut returned_bowl, mut full) = (None, false, sequence == 2);
        let effect = client.matching("stew effect and returned bowl", |packet| {
            match packet {
                Packet::EntityEffect(active) => assert!(effect.replace(active).is_none()),
                Packet::SetContainerSlot(change)
                    if change.window_id == 0 && change.slot == slot =>
                {
                    assert_eq!((change.item.item, change.item.count), (bowl, 1));
                    returned_bowl = true;
                }
                Packet::SetHealth(update) => {
                    assert_eq!(update.food, 20);
                    full = true;
                }
                _ => {}
            }
            if returned_bowl && full {
                effect.take()
            } else {
                None
            }
        });
        assert_eq!((effect.amplifier, effect.duration), (0, duration));
        effect
    });
    assert_ne!(applied[0].effect, applied[1].effect);
    poison_damage(&mut client, &applied[1]);
    drop(client);
    server.wait_for("SuspiciousEater left the game");

    let mut client = Client::join(port, "SuspiciousEater");
    let (mut resumed, mut inventory_synced) = ([false; 2], false);
    client.matching("stew effects and bowls after reconnect", |packet| {
        match packet {
            Packet::EntityEffect(current) => {
                let index = applied
                    .iter()
                    .position(|effect| effect.effect == current.effect)
                    .unwrap();
                let initial = &applied[index];
                assert_eq!(current.amplifier, initial.amplifier);
                assert!(
                    (1..initial.duration).contains(&current.duration),
                    "{current:?}"
                );
                assert_eq!(
                    current.flags,
                    play::EffectFlags {
                        blend: false,
                        ..initial.flags
                    }
                );
                resumed[index] = true;
            }
            Packet::SetContainerContent(contents) if contents.window_id == 0 => {
                for slot in [36, 45] {
                    let stack = &contents.slots[slot];
                    assert_eq!((stack.item, stack.count), (bowl, 1));
                }
                inventory_synced = true;
            }
            _ => {}
        }
        (resumed == [true; 2] && inventory_synced).then_some(())
    });
    drop(client);
    server.wait_for("SuspiciousEater left the game");
    for effect in &mut applied {
        effect.entity_id = 0;
    }
    applied
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn food_suspicious_stew_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("food-suspicious-stew");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    let stew = |effect| {
        Compound::from_iter([
            ("id", Nbt::from("minecraft:suspicious_stew")),
            ("count", Nbt::Int(1)),
            (
                "components",
                Compound::from_iter([(
                    "minecraft:suspicious_stew_effects",
                    List::Compound(vec![effect]),
                )])
                .into(),
            ),
        ])
    };
    let mut regeneration = stew(Compound::from_iter([("id", "minecraft:regeneration")]));
    regeneration.insert("Slot", 0_i8);
    let poison = stew(Compound::from_iter([
        ("id", Nbt::from("minecraft:poison")),
        ("duration", Nbt::Int(210)),
    ]));
    let data = Compound::from_iter([
        ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
        ("foodLevel", Nbt::Int(14)),
        ("foodSaturationLevel", Nbt::Float(0.0)),
        ("Inventory", List::Compound(vec![regeneration]).into()),
        (
            "equipment",
            Compound::from_iter([("offhand", poison)]).into(),
        ),
    ]);
    for dir in [&vanilla, &harpas] {
        harpas_anvil::write_player(&dir.join("world"), offline_uuid("SuspiciousEater"), &data)
            .unwrap();
    }
    let expected = exercise_suspicious_stew(&mut reference, port);
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    assert_eq!(exercise_suspicious_stew(&mut server, other), expected);
    stop(&mut server);
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

fn exercise_regeneration_potion(
    server: &mut Server,
    port: u16,
    name: &str,
    hand: play::Hand,
    cancel: bool,
) -> play::clientbound::EntityEffect {
    let mut client = Client::join(port, name);
    client.stream.set_nodelay(true).unwrap();
    let entity_id = client.matching("Play Login", |packet| match packet {
        Packet::Login(login) => Some(login.entity_id),
        _ => None,
    });
    confirm_position(&mut client);
    let initial = health(&mut client);
    assert_eq!(
        (initial.health, initial.food, initial.saturation),
        (10.0, 20, 3.0)
    );
    if cancel {
        use_food(&mut client, hand, 1);
        cancel_food(&mut client, entity_id);
    }

    let slot = if hand == play::Hand::OffHand { 45 } else { 36 };
    let bottle = harpas_core::item::items()
        .id("minecraft:glass_bottle")
        .unwrap() as i32;
    let started = Instant::now();
    use_food(&mut client, hand, 2);
    let (mut applied, mut returned_bottle, mut healed) = (None, false, 10.0);
    let mut applied = client.matching("potion effect, healing and returned bottle", |packet| {
        match packet {
            Packet::EntityEffect(effect) if effect.entity_id == entity_id => {
                assert!(applied.replace(effect).is_none());
            }
            Packet::SetContainerSlot(change) if change.window_id == 0 && change.slot == slot => {
                assert_eq!((change.item.item, change.item.count), (bottle, 1));
                assert!(
                    started.elapsed().abs_diff(Duration::from_millis(1600))
                        < Duration::from_millis(400)
                );
                returned_bottle = true;
            }
            Packet::SetHealth(update) => {
                healed += 1.0;
                assert_eq!(
                    update,
                    play::clientbound::SetHealth {
                        health: healed,
                        ..initial.clone()
                    }
                );
            }
            _ => {}
        }
        if returned_bottle && healed >= 12.0 {
            applied.take()
        } else {
            None
        }
    });
    drop(client);
    server.wait_for(&format!("{name} left the game"));

    let mut client = Client::join(port, name);
    confirm_position(&mut client);
    let (mut resumed, mut bottle_synced) = (false, false);
    client.matching("potion effect and bottle after reconnect", |packet| {
        match packet {
            Packet::EntityEffect(effect) => {
                assert_eq!(
                    (effect.effect, effect.amplifier),
                    (applied.effect, applied.amplifier)
                );
                assert!(
                    (1..applied.duration).contains(&effect.duration),
                    "{effect:?}"
                );
                assert_eq!(
                    effect.flags,
                    play::EffectFlags {
                        blend: false,
                        ..applied.flags
                    }
                );
                resumed = true;
            }
            Packet::SetContainerContent(contents) if contents.window_id == 0 => {
                let stack = &contents.slots[slot as usize];
                assert_eq!((stack.item, stack.count), (bottle, 1));
                bottle_synced = true;
            }
            _ => {}
        }
        (resumed && bottle_synced).then_some(())
    });
    drop(client);
    server.wait_for(&format!("{name} left the game"));
    applied.entity_id = 0;
    applied
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn regeneration_potions_match_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("regeneration-potions");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    let cases = [
        (
            "RegenDrinker",
            "minecraft:regeneration",
            play::Hand::OffHand,
            true,
        ),
        (
            "LongRegen",
            "minecraft:long_regeneration",
            play::Hand::MainHand,
            false,
        ),
        (
            "StrongRegen",
            "minecraft:strong_regeneration",
            play::Hand::MainHand,
            false,
        ),
    ];
    for (name, potion, hand, _) in cases {
        let mut drink = Compound::from_iter([
            ("id", Nbt::from("minecraft:potion")),
            ("count", Nbt::Int(1)),
            (
                "components",
                Compound::from_iter([(
                    "minecraft:potion_contents",
                    Compound::from_iter([("potion", potion)]),
                )])
                .into(),
            ),
        ]);
        let mut data = Compound::from_iter([
            ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
            ("Health", Nbt::Float(10.0)),
            ("foodLevel", Nbt::Int(20)),
            ("foodSaturationLevel", Nbt::Float(3.0)),
        ]);
        if hand == play::Hand::MainHand {
            drink.insert("Slot", 0_i8);
            data.insert("Inventory", List::Compound(vec![drink]));
        } else {
            data.insert("equipment", Compound::from_iter([("offhand", drink)]));
        }
        for dir in [&vanilla, &harpas] {
            harpas_anvil::write_player(&dir.join("world"), offline_uuid(name), &data).unwrap();
        }
    }
    let expected = cases.map(|(name, _, hand, cancel)| {
        exercise_regeneration_potion(&mut reference, port, name, hand, cancel)
    });
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    assert_eq!(
        cases.map(|(name, _, hand, cancel)| {
            exercise_regeneration_potion(&mut server, other, name, hand, cancel)
        }),
        expected
    );
    stop(&mut server);

    for ((name, _, _, _), effect) in cases.into_iter().zip(expected) {
        for dir in [&vanilla, &harpas] {
            let (saved, _) = harpas_anvil::read_player(&dir.join("world"), offline_uuid(name))
                .unwrap()
                .unwrap();
            let Some(Nbt::List(List::Compound(effects))) = saved.get("active_effects") else {
                panic!("missing saved potion effect for {name}");
            };
            assert_eq!(effects.len(), 1);
            assert_eq!(
                effects[0].get("id"),
                Some(&Nbt::from("minecraft:regeneration"))
            );
            let Some(Nbt::Int(remaining)) = effects[0].get("duration") else {
                panic!("missing saved potion effect duration for {name}");
            };
            // Regeneration heals at multiples of its interval before the duration counts down.
            let interval = 50 >> effect.amplifier;
            let heals = effect.duration / interval - remaining / interval;
            assert!(heals >= 2);
            assert_eq!(saved.get("Health"), Some(&Nbt::Float(10.0 + heals as f32)));
        }
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

fn exercise_milk_bucket(server: &mut Server, port: u16) -> Vec<i32> {
    let mut client = Client::join(port, "MilkDrinker");
    client.stream.set_nodelay(true).unwrap();
    let entity_id = client.matching("Play Login", |packet| match packet {
        Packet::Login(login) => Some(login.entity_id),
        _ => None,
    });
    confirm_position(&mut client);
    let initial = health(&mut client);
    assert_eq!(
        (initial.health, initial.food, initial.saturation),
        (1.0, 20, 3.0)
    );
    let bucket = harpas_core::item::items().id("minecraft:bucket").unwrap() as i32;

    use_food(&mut client, play::Hand::OffHand, 1);
    cancel_food(&mut client, entity_id);

    let started = Instant::now();
    use_food(&mut client, play::Hand::OffHand, 2);
    let mut removed = Vec::new();
    let (mut amount_cleared, mut capacity_cleared, mut returned_bucket) = (false, false, false);
    for _ in 0..1000 {
        let bytes = client.next("milk effects and offhand bucket");
        amount_cleared |= absorption_metadata(&bytes, entity_id) == Some(0.0);
        match Packet::decode(&bytes) {
            Ok(Packet::RemoveEntityEffect(effect)) if effect.entity_id == entity_id => {
                removed.push(effect.effect);
            }
            Ok(Packet::UpdateAttributes(packet)) if packet.entity_id == entity_id => {
                capacity_cleared |= packet.attributes.iter().any(|attribute| {
                    attribute.attribute == play::Attribute::MaxAbsorption
                        && attribute.base_value == 0.0
                        && attribute.modifiers.is_empty()
                });
            }
            Ok(Packet::SetContainerSlot(change)) if change.window_id == 0 && change.slot == 45 => {
                assert_eq!((change.item.item, change.item.count), (bucket, 1));
                returned_bucket = true;
                assert!(
                    started.elapsed().abs_diff(Duration::from_millis(1600))
                        < Duration::from_millis(400)
                );
            }
            _ => {}
        }
        if amount_cleared && capacity_cleared && returned_bucket && removed.len() == 2 {
            break;
        }
    }
    assert!(amount_cleared && capacity_cleared && returned_bucket);
    removed.sort_unstable();
    removed.dedup();
    assert_eq!(removed.len(), 2);

    use_food(&mut client, play::Hand::MainHand, 3);
    client.matching("main hand bucket", |packet| match packet {
        Packet::SetContainerSlot(change) if change.window_id == 0 && change.slot == 36 => {
            assert_eq!((change.item.item, change.item.count), (bucket, 1));
            Some(())
        }
        _ => None,
    });
    drop(client);
    server.wait_for("MilkDrinker left the game");

    let mut client = Client::join(port, "MilkDrinker");
    confirm_position(&mut client);
    client.send_command("list");
    let (mut synced_inventory, mut synced_health) = (false, false);
    client.matching("milk inventory and health after reconnect", |packet| {
        match packet {
            Packet::EntityEffect(effect) => {
                panic!("effect returned after drinking milk: {effect:?}")
            }
            Packet::SetContainerContent(contents) if contents.window_id == 0 => {
                for slot in [36, 45] {
                    let stack = &contents.slots[slot];
                    assert_eq!((stack.item, stack.count), (bucket, 1));
                }
                synced_inventory = true;
            }
            Packet::SetHealth(update) => {
                assert_eq!(update, initial);
                synced_health = true;
            }
            Packet::SystemChatMessage(reply)
                if matches!(&reply.content, codec::Nbt::Compound(text)
                if text.get("translate") == Some(&codec::Nbt::from("commands.list.players"))) =>
            {
                assert!(synced_inventory && synced_health);
                return Some(());
            }
            _ => {}
        }
        None
    });
    drop(client);
    server.wait_for("MilkDrinker left the game");
    removed
}

fn drinker_with_poison_and_absorption(item: &str) -> Compound {
    let drink = Compound::from_iter([("id", Nbt::from(item)), ("count", Nbt::Int(1))]);
    let mut main_hand = drink.clone();
    main_hand.insert("Slot", 0_i8);
    let mut poison = Compound::from_iter([("id", "minecraft:poison")]);
    poison.insert("duration", 1200_i32);
    poison.insert("amplifier", 1_i32);
    poison.insert(
        "hidden_effect",
        Compound::from_iter([("duration", 2400_i32)]),
    );
    let mut absorption = Compound::from_iter([("id", "minecraft:absorption")]);
    absorption.insert("duration", 1200_i32);
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
    Compound::from_iter([
        ("DataVersion", Nbt::Int(harpas_anvil::DATA_VERSION)),
        ("Health", Nbt::Float(1.0)),
        ("foodLevel", Nbt::Int(20)),
        ("foodSaturationLevel", Nbt::Float(3.0)),
        ("AbsorptionAmount", Nbt::Float(3.0)),
        ("Inventory", List::Compound(vec![main_hand]).into()),
        (
            "equipment",
            Compound::from_iter([("offhand", drink)]).into(),
        ),
        ("attributes", List::Compound(vec![attribute]).into()),
        (
            "active_effects",
            List::Compound(vec![poison, absorption]).into(),
        ),
    ])
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn milk_bucket_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("milk-bucket");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    let data = drinker_with_poison_and_absorption("minecraft:milk_bucket");
    let uuid = offline_uuid("MilkDrinker");
    for dir in [&vanilla, &harpas] {
        harpas_anvil::write_player(&dir.join("world"), uuid, &data).unwrap();
    }
    let expected = exercise_milk_bucket(&mut reference, port);
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    assert_eq!(exercise_milk_bucket(&mut server, other), expected);
    stop(&mut server);
    for dir in [&vanilla, &harpas] {
        let (saved, _) = harpas_anvil::read_player(&dir.join("world"), uuid)
            .unwrap()
            .unwrap();
        assert!(saved.get("active_effects").is_none(), "{dir:?}");
        assert_eq!(saved.get("AbsorptionAmount"), Some(&Nbt::Float(0.0)));
    }
}

fn exercise_honey_bottle(server: &mut Server, port: u16) -> (i32, i32) {
    let mut client = Client::join(port, "HoneyDrinker");
    client.stream.set_nodelay(true).unwrap();
    let entity_id = client.matching("Play Login", |packet| match packet {
        Packet::Login(login) => Some(login.entity_id),
        _ => None,
    });
    confirm_position(&mut client);
    let mut effects = [None; 2];
    let mut vitals = None;
    client.matching("initial health, Poison II and Absorption", |packet| {
        match packet {
            Packet::EntityEffect(effect) if effect.entity_id == entity_id => {
                effects[effect.amplifier as usize] = Some(effect.effect);
            }
            Packet::SetHealth(update) => vitals = Some(update),
            _ => {}
        }
        (effects.iter().all(Option::is_some) && vitals.is_some()).then_some(())
    });
    let [absorption, poison] = effects.map(Option::unwrap);
    let mut vitals = vitals.unwrap();
    assert_eq!(
        (vitals.health, vitals.food, vitals.saturation),
        (1.0, 14, 3.0)
    );
    let bottle = harpas_core::item::items()
        .id("minecraft:glass_bottle")
        .unwrap() as i32;

    use_food(&mut client, play::Hand::OffHand, 1);
    cancel_food(&mut client, entity_id);

    for (sequence, hand, slot) in [(2, play::Hand::OffHand, 45), (3, play::Hand::MainHand, 36)] {
        let first_drink = sequence == 2;
        let (mut removed_poison, mut returned_bottle) = (false, false);
        // The second drink changes only nonzero saturation, which is synced on reconnect.
        let mut updated_health = !first_drink;
        vitals.food = 20;
        vitals.saturation += 1.2;
        let started = Instant::now();
        use_food(&mut client, hand, sequence);
        client.matching("honey effect, food and returned bottle", |packet| {
            match packet {
                Packet::RemoveEntityEffect(effect) if effect.entity_id == entity_id => {
                    assert_eq!(effect.effect, poison);
                    assert!(first_drink);
                    assert!(!removed_poison);
                    removed_poison = true;
                }
                Packet::SetHealth(update) => {
                    assert_eq!(update, vitals);
                    updated_health = true;
                }
                Packet::SetContainerSlot(change)
                    if change.window_id == 0 && change.slot == slot =>
                {
                    assert_eq!((change.item.item, change.item.count), (bottle, 1));
                    returned_bottle = true;
                    assert!(
                        started.elapsed().abs_diff(Duration::from_secs(2))
                            < Duration::from_millis(400)
                    );
                }
                _ => {}
            }
            (returned_bottle && updated_health && (removed_poison || !first_drink)).then_some(())
        });
    }
    drop(client);
    server.wait_for("HoneyDrinker left the game");

    let mut client = Client::join(port, "HoneyDrinker");
    confirm_position(&mut client);
    client.send_command("list");
    let (mut synced_inventory, mut synced_health, mut synced_absorption) = (false, false, false);
    client.matching("honey inventory and effects after reconnect", |packet| {
        match packet {
            Packet::EntityEffect(effect) => {
                assert_eq!((effect.effect, effect.amplifier), (absorption, 0));
                assert!((1..1200).contains(&effect.duration));
                synced_absorption = true;
            }
            Packet::SetContainerContent(contents) if contents.window_id == 0 => {
                for slot in [36, 45] {
                    let stack = &contents.slots[slot];
                    assert_eq!((stack.item, stack.count), (bottle, 1));
                }
                synced_inventory = true;
            }
            Packet::SetHealth(update) => {
                assert_eq!(update, vitals);
                synced_health = true;
            }
            Packet::SystemChatMessage(reply)
                if matches!(&reply.content, codec::Nbt::Compound(text)
                if text.get("translate") == Some(&codec::Nbt::from("commands.list.players"))) =>
            {
                assert!(synced_inventory && synced_health && synced_absorption);
                return Some(());
            }
            _ => {}
        }
        None
    });
    drop(client);
    server.wait_for("HoneyDrinker left the game");
    (poison, absorption)
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn honey_bottle_matches_vanilla() {
    let Reference { jar, eula } = Reference::required();
    let folder = Folder::new("honey-bottle");
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let [port, other] = flat_survival_servers(&vanilla, &harpas, &eula, "normal");
    let mut reference = start_flat_vanilla(&jar, &vanilla, false);
    let mut data = drinker_with_poison_and_absorption("minecraft:honey_bottle");
    data.insert("foodLevel", 14_i32);
    let uuid = offline_uuid("HoneyDrinker");
    for dir in [&vanilla, &harpas] {
        harpas_anvil::write_player(&dir.join("world"), uuid, &data).unwrap();
    }
    let expected = exercise_honey_bottle(&mut reference, port);
    stop(&mut reference);
    let mut server = start_flat_harpas(&jar, &vanilla, &harpas);
    assert_eq!(exercise_honey_bottle(&mut server, other), expected);
    stop(&mut server);
    for dir in [&vanilla, &harpas] {
        let (saved, _) = harpas_anvil::read_player(&dir.join("world"), uuid)
            .unwrap()
            .unwrap();
        assert_eq!(saved.get("AbsorptionAmount"), Some(&Nbt::Float(3.0)));
        let Some(Nbt::List(List::Compound(effects))) = saved.get("active_effects") else {
            panic!("missing saved Absorption in {dir:?}");
        };
        assert_eq!(effects.len(), 1);
        assert_eq!(
            effects[0].get("id"),
            Some(&Nbt::from("minecraft:absorption"))
        );
    }
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
