//! Compare the advertised tree and command replies with Minecraft 26.1 on the network.

mod reference;

use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
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
        let reply = login::clientbound::Packet::decode(&client.next()).unwrap();
        (client, reply)
    }

    fn join(port: u16, name: &str) -> Self {
        let (mut client, reply) = Self::login(port, name);
        assert!(matches!(reply, login::clientbound::Packet::LoginSuccess(_)));
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
        self.replies(command, replies)
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

    fn replies(&mut self, command: &str, replies: usize) -> Vec<SystemChatMessage> {
        (0..replies).map(|_| self.matching("command reply", |packet| match packet {
            Packet::SystemChatMessage(reply) if !matches!(&reply.content,
                codec::Nbt::Compound(text) if matches!(text.get("translate"), Some(codec::Nbt::String(key))
                    if matches!(key.as_str(), "multiplayer.player.joined" | "multiplayer.player.left" | "chat.type.admin"))) => Some(reply),
            Packet::Disconnect(packet) => panic!("disconnected during /{command}: {:?}", packet.reason),
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
    replies.extend(operator.replies("deop NewOperator", 1));
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
            properties.replace("enforce-whitelist=false", "enforce-whitelist=true"),
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
