//! Whole connections, from the client's first byte, against what vanilla answers.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use harpas_java::{
    Connection, ConnectionError, FrameReader, PlayEvent, ServerInfo, ServerKey, State,
    offline_uuid, write_frame,
};
use harpas_protocol_java::codec::DecodeError;
use harpas_protocol_java::v26_1::{configuration, handshaking, login, play, status};

/// A connection with a key shared by every test, since generating one takes a while.
fn new_connection() -> Connection {
    static KEY: OnceLock<Arc<ServerKey>> = OnceLock::new();
    Connection::new(Arc::clone(
        KEY.get_or_init(|| Arc::new(ServerKey::generate())),
    ))
}

fn handshake(protocol: i32, intent: handshaking::Intent) -> Vec<u8> {
    let packet: handshaking::serverbound::Packet = handshaking::serverbound::Handshake {
        protocol_version: protocol,
        server_address: "localhost".into(),
        server_port: 25565,
        intent,
    }
    .into();
    let mut out = Vec::new();
    write_frame(&mut out, |buf| packet.encode(buf));
    out
}

fn frame<P>(packet: P, encode: impl FnOnce(&P, &mut Vec<u8>)) -> Vec<u8> {
    let mut out = Vec::new();
    write_frame(&mut out, |buf| encode(&packet, buf));
    out
}

fn status_request() -> Vec<u8> {
    let packet: status::serverbound::Packet = status::serverbound::StatusRequest.into();
    frame(packet, |p, buf| p.encode(buf))
}

fn ping(timestamp: i64) -> Vec<u8> {
    let packet: status::serverbound::Packet = status::serverbound::PingRequest { timestamp }.into();
    frame(packet, |p, buf| p.encode(buf))
}

fn login_start(name: &str) -> Vec<u8> {
    let packet: login::serverbound::Packet = login::serverbound::LoginStart {
        name: name.into(),
        player_uuid: 0,
    }
    .into();
    frame(packet, |p, buf| p.encode(buf))
}

/// Splits the connection's output into frames.
fn frames(output: &[u8]) -> Vec<Vec<u8>> {
    let mut reader = FrameReader::default();
    reader.push(output);
    let mut frames = Vec::new();
    while let Some(frame) = reader.next_frame().unwrap() {
        frames.push(frame);
    }
    assert!(reader.pending().is_empty());
    frames
}

fn login_disconnect_reason(output: &[u8]) -> String {
    let frames = frames(output);
    assert_eq!(frames.len(), 1);
    match login::clientbound::Packet::decode(&frames[0]).unwrap() {
        login::clientbound::Packet::LoginDisconnect(packet) => packet.reason,
        other => panic!("expected Login Disconnect, got {other:?}"),
    }
}

#[test]
fn server_list_ping() {
    let info = ServerInfo::default();
    let mut connection = new_connection();
    let bytes = [
        handshake(775, handshaking::Intent::Status),
        status_request(),
    ]
    .concat();
    // One byte at a time, as a slow network could deliver them.
    for byte in &bytes {
        connection.receive(&[*byte], &info).unwrap();
    }
    assert_eq!(connection.state(), State::Status);
    let response = frames(&connection.take_output());
    assert_eq!(
        status::clientbound::Packet::decode(&response[0]),
        Ok(status::clientbound::StatusResponse {
            json: info.status_json()
        }
        .into())
    );

    connection.receive(&ping(1234), &info).unwrap();
    assert_eq!(
        status::clientbound::Packet::decode(&frames(&connection.take_output())[0]),
        Ok(status::clientbound::PongResponse { timestamp: 1234 }.into())
    );
    // Vanilla closes the connection after the pong.
    assert!(connection.is_closed());
}

#[test]
fn status_is_answered_once() {
    let info = ServerInfo::default();
    let mut connection = new_connection();
    let bytes = [
        handshake(775, handshaking::Intent::Status),
        status_request(),
        status_request(),
    ]
    .concat();
    assert_eq!(
        connection.receive(&bytes, &info),
        Err(ConnectionError::UnexpectedPacket {
            state: State::Status,
            packet: "StatusRequest"
        })
    );
    assert!(connection.is_closed());
    // The first answer is still sent.
    assert_eq!(frames(&connection.take_output()).len(), 1);
}

#[test]
fn other_protocol_versions_are_refused_at_login() {
    let info = ServerInfo::default();
    let mut connection = new_connection();
    connection
        .receive(&handshake(777, handshaking::Intent::Login), &info)
        .unwrap();
    assert!(connection.is_closed());
    assert_eq!(
        login_disconnect_reason(&connection.take_output()),
        r#"{"translate":"multiplayer.disconnect.incompatible","with":["26.1"]}"#
    );

    let mut connection = new_connection();
    connection
        .receive(&handshake(47, handshaking::Intent::Login), &info)
        .unwrap();
    assert_eq!(
        login_disconnect_reason(&connection.take_output()),
        r#"{"translate":"multiplayer.disconnect.outdated_client","with":["26.1"]}"#
    );

    // The server list still answers, and shows the server's version.
    let mut connection = new_connection();
    let bytes = [
        handshake(777, handshaking::Intent::Status),
        status_request(),
    ]
    .concat();
    connection.receive(&bytes, &info).unwrap();
    assert_eq!(frames(&connection.take_output()).len(), 1);
}

#[test]
fn transfers_follow_accepts_transfers() {
    let mut connection = new_connection();
    connection
        .receive(
            &handshake(775, handshaking::Intent::Transfer),
            &ServerInfo::default(),
        )
        .unwrap();
    assert_eq!(
        login_disconnect_reason(&connection.take_output()),
        r#"{"translate":"multiplayer.disconnect.transfers_disabled"}"#
    );

    let info = ServerInfo {
        accepts_transfers: true,
        ..ServerInfo::default()
    };
    let mut connection = new_connection();
    connection
        .receive(&handshake(775, handshaking::Intent::Transfer), &info)
        .unwrap();
    assert_eq!(connection.state(), State::Login);
}

/// Offline mode without compression, so that the output is plain frames; `tests/online.rs` covers both.
fn offline() -> ServerInfo {
    ServerInfo {
        online_mode: false,
        compression_threshold: -1,
        view_distance: 2,
        ..ServerInfo::default()
    }
}

/// Logs Steve in offline mode and acknowledges Login Success.
fn logged_in(info: &ServerInfo) -> Connection {
    logged_in_with(info, new_connection())
}

fn logged_in_with(info: &ServerInfo, mut connection: Connection) -> Connection {
    let bytes = [
        handshake(775, handshaking::Intent::Login),
        login_start("Steve"),
    ]
    .concat();
    connection.receive(&bytes, info).unwrap();
    let output = frames(&connection.take_output());
    assert_eq!(
        login::clientbound::Packet::decode(&output[0]),
        Ok(login::clientbound::LoginSuccess {
            uuid: offline_uuid("Steve"),
            username: "Steve".into(),
            properties: vec![],
        }
        .into())
    );
    let ack: login::serverbound::Packet = login::serverbound::LoginAcknowledged.into();
    connection
        .receive(&frame(ack, |p, buf| p.encode(buf)), info)
        .unwrap();
    assert_eq!(connection.state(), State::Configuration);
    connection
}

fn playing(info: &ServerInfo, connection: Connection) -> Connection {
    let mut connection = logged_in_with(info, connection);
    connection.take_output();
    connection
        .receive(&known_packs(&[("minecraft", "core", "26.1")]), info)
        .unwrap();
    connection.take_output();
    let ack: configuration::serverbound::Packet =
        configuration::serverbound::AcknowledgeFinishConfiguration.into();
    connection
        .receive(&frame(ack, |p, buf| p.encode(buf)), info)
        .unwrap();
    connection
}

fn configuration_packets(output: &[u8]) -> Vec<configuration::clientbound::Packet> {
    frames(output)
        .iter()
        .map(|f| configuration::clientbound::Packet::decode(f).unwrap())
        .collect()
}

fn known_packs(packs: &[(&str, &str, &str)]) -> Vec<u8> {
    let packet: configuration::serverbound::Packet = configuration::serverbound::SelectKnownPacks {
        packs: packs
            .iter()
            .map(|(namespace, id, version)| configuration::KnownPack {
                namespace: (*namespace).into(),
                id: (*id).into(),
                version: (*version).into(),
            })
            .collect(),
    }
    .into();
    frame(packet, |p, buf| p.encode(buf))
}

#[test]
fn offline_players_join_the_void_world() {
    let info = offline();
    let mut connection = logged_in(&info);

    // Configuration starts with the brand, the features and the known packs.
    let names: Vec<_> = configuration_packets(&connection.take_output())
        .iter()
        .map(|p| p.name())
        .collect();
    assert_eq!(names, ["PluginMessage", "FeatureFlags", "SelectKnownPacks"]);

    // The client has the core pack: every registry is sent by name only, then the tags, then configuration
    // ends.
    connection
        .receive(&known_packs(&[("minecraft", "core", "26.1")]), &info)
        .unwrap();
    let packets = configuration_packets(&connection.take_output());
    assert_eq!(packets.len(), 30);
    let mut entries = 0;
    for packet in &packets[..28] {
        let configuration::clientbound::Packet::RegistryData(data) = packet else {
            panic!("expected Registry Data, got {packet:?}");
        };
        assert!(data.entries.iter().all(|entry| entry.data.is_none()));
        entries += data.entries.len();
    }
    assert_eq!(entries, 382);
    assert_eq!(packets[28].name(), "UpdateTags");
    assert_eq!(packets[29].name(), "FinishConfiguration");

    let ack: configuration::serverbound::Packet =
        configuration::serverbound::AcknowledgeFinishConfiguration.into();
    connection
        .receive(&frame(ack, |p, buf| p.encode(buf)), &info)
        .unwrap();
    assert_eq!(connection.state(), State::Play);
    let packets: Vec<_> = frames(&connection.take_output())
        .iter()
        .map(|f| play::clientbound::Packet::decode(f).unwrap())
        .collect();
    let names: Vec<_> = packets.iter().map(|p| p.name()).collect();
    assert_eq!(
        names[..8],
        [
            "Login",
            "PlayerAbilities",
            "PlayerInfoUpdate",
            "SetDefaultSpawnPosition",
            "SynchronizePlayerPosition",
            "GameEvent",
            "SetCenterChunk",
            "ChunkBatchStart",
        ]
    );
    // A view distance of 2 is a 5 by 5 square of chunks.
    let chunks = names
        .iter()
        .filter(|n| **n == "ChunkDataAndUpdateLight")
        .count();
    assert_eq!(chunks, 25);
    assert_eq!(
        packets.last(),
        Some(&play::clientbound::ChunkBatchFinished { batch_size: 25 }.into())
    );

    // Play packets the schema does not have yet, such as Set Creative Mode Slot (0x38), are ignored.
    connection.receive(&[0x02, 0x38, 0x00], &info).unwrap();
    assert_eq!(connection.state(), State::Play);
}

#[test]
fn play_login_uses_the_world_seed_hash_and_saved_spawn() {
    let mut info = offline();
    info.spawn_position = [-32, 71, 8];
    info.hashed_seed = 123_456;
    info.is_flat = false;
    let mut connection = playing(&info, Connection::new(Arc::new(ServerKey::generate())));
    let packets: Vec<_> = frames(&connection.take_output())
        .iter()
        .map(|frame| play::clientbound::Packet::decode(frame).unwrap())
        .collect();
    assert!(packets.iter().any(|packet| matches!(packet,
        play::clientbound::Packet::Login(login) if login.spawn.hashed_seed == 123_456 && !login.spawn.is_flat)));
    assert!(packets.iter().any(|packet| matches!(packet,
        play::clientbound::Packet::SetDefaultSpawnPosition(spawn)
        if spawn.position.position.x == -32 && spawn.position.position.y == 71 && spawn.position.position.z == 8)));
    assert!(packets.iter().any(|packet| matches!(packet,
        play::clientbound::Packet::SynchronizePlayerPosition(pos)
        if pos.x == -31.5 && pos.y == 71.0 && pos.z == 8.5)));
}

#[test]
fn the_chunk_center_starts_at_the_spawn() {
    let mut info = offline();
    // Far from the origin and at negative coordinates: chunk 62, -32.
    info.spawn_position = [1000, 70, -500];
    for external in [false, true] {
        let mut connection = Connection::new(Arc::new(ServerKey::generate()));
        if external {
            connection.use_external_chunks();
        }
        let mut connection = playing(&info, connection);
        let packets: Vec<_> = frames(&connection.take_output())
            .iter()
            .map(|frame| play::clientbound::Packet::decode(frame).unwrap())
            .collect();
        let centers: Vec<_> = packets
            .iter()
            .filter_map(|packet| match packet {
                play::clientbound::Packet::SetCenterChunk(center) => Some((center.x, center.z)),
                _ => None,
            })
            .collect();
        assert_eq!(centers, [(62, -32)]);
        // The void world's columns surround the spawn, where the client keeps them.
        let chunks: Vec<_> = packets
            .iter()
            .filter_map(|packet| match packet {
                play::clientbound::Packet::ChunkDataAndUpdateLight(chunk) => {
                    Some((chunk.x, chunk.z))
                }
                _ => None,
            })
            .collect();
        assert_eq!(chunks.len(), if external { 0 } else { 25 });
        assert!(
            chunks
                .iter()
                .all(|(x, z)| (x - 62).abs() <= 2 && (z + 32).abs() <= 2)
        );
        // Standing at the spawn does not move the center again.
        connection.update_view(1000.5, -499.5, None);
        assert!(connection.take_output().is_empty());
    }
}

#[test]
fn play_uses_the_assigned_entity_id_and_forwards_movement() {
    let info = offline();
    let mut connection = playing(
        &info,
        Connection::with_entity_id(Arc::new(ServerKey::generate()), 42),
    );
    let output = frames(&connection.take_output());
    assert!(matches!(
        play::clientbound::Packet::decode(&output[0]),
        Ok(play::clientbound::Packet::Login(login)) if login.entity_id == 42
    ));
    let confirm: play::serverbound::Packet =
        play::serverbound::ConfirmTeleportation { teleport_id: 1 }.into();
    let movement: play::serverbound::Packet = play::serverbound::SetPlayerPositionAndRotation {
        x: 2.0,
        y: 65.0,
        z: -1.0,
        yaw: 90.0,
        pitch: 15.0,
        flags: play::MovementFlags::default(),
    }
    .into();
    let input = [
        frame(confirm.clone(), |p, buf| p.encode(buf)),
        frame(movement.clone(), |p, buf| p.encode(buf)),
    ]
    .concat();
    connection.receive(&input, &info).unwrap();
    assert_eq!(
        connection.take_play_events(),
        vec![
            PlayEvent::Packet(Box::new(confirm)),
            PlayEvent::Packet(Box::new(movement))
        ]
    );
    assert!(connection.take_play_events().is_empty());
    let update: play::clientbound::Packet =
        play::clientbound::PlayerInfoRemove { players: vec![5] }.into();
    connection.send_play_packet(update.clone());
    let output = frames(&connection.take_output());
    assert_eq!(play::clientbound::Packet::decode(&output[0]), Ok(update));
}

#[test]
fn chat_and_movement_reach_the_logic_thread_in_arrival_order() {
    let info = offline();
    let mut connection = playing(&info, new_connection());
    connection.take_output();
    let chat = |text: &str| -> play::serverbound::Packet {
        play::serverbound::ChatMessage {
            message: text.into(),
            timestamp: 1,
            salt: 2,
            signature: None,
            offset: 0,
            acknowledged: [0; 3],
            checksum: 1,
        }
        .into()
    };
    let first = chat("first");
    let movement: play::serverbound::Packet = play::serverbound::SetPlayerPosition {
        x: 1.0,
        y: 65.0,
        z: 0.0,
        flags: play::MovementFlags::default(),
    }
    .into();
    let second = chat("second");
    let input = [
        frame(first, |p, b| p.encode(b)),
        frame(movement, |p, b| p.encode(b)),
        frame(second, |p, b| p.encode(b)),
    ]
    .concat();
    connection.receive(&input, &info).unwrap();
    let events = connection.take_play_events();
    assert!(matches!(&events[..], [
        PlayEvent::Chat(harpas_java::ChatEvent::Message(first)),
        PlayEvent::Packet(movement),
        PlayEvent::Chat(harpas_java::ChatEvent::Message(second)),
    ] if first.message == "first" && matches!(&**movement, play::serverbound::Packet::SetPlayerPosition(_)) && second.message == "second"));
}

#[test]
fn chat_is_refused_with_vanillas_translated_messages() {
    use harpas_protocol_java::codec::Nbt;
    let info = offline();
    let mut connection = playing(&info, new_connection());
    connection.take_output();
    let chat = |text: &str, timestamp: i64| {
        let packet: play::serverbound::Packet = play::serverbound::ChatMessage {
            message: text.into(),
            timestamp,
            salt: 2,
            signature: None,
            offset: 0,
            acknowledged: [0; 3],
            checksum: 1,
        }
        .into();
        frame(packet, |p, b| p.encode(b))
    };
    let component =
        |fields: &[(&'static str, &'static str)]| Nbt::Compound(fields.iter().copied().collect());
    let answer = |connection: &mut Connection| {
        let output = frames(&connection.take_output());
        assert_eq!(output.len(), 1);
        play::clientbound::Packet::decode(&output[0]).unwrap()
    };

    // The C1 control characters are chat like any other.
    connection
        .receive(&chat("a\u{80}\u{9f}b", 1), &info)
        .unwrap();
    assert!(matches!(
        &connection.take_play_events()[..],
        [PlayEvent::Chat(harpas_java::ChatEvent::Message(message))] if message.message == "a\u{80}\u{9f}b"
    ));
    assert!(connection.take_output().is_empty());

    // With chat hidden in the client's settings, a message is answered with a red system message.
    let settings: play::serverbound::Packet = play::serverbound::ClientInformation {
        locale: "en_us".into(),
        view_distance: 10,
        chat_mode: play::ChatMode::Hidden,
        chat_colors: true,
        displayed_skin_parts: 0,
        main_hand: play::MainHand::Right,
        enable_text_filtering: false,
        allow_server_listings: true,
        particle_status: play::ParticleStatus::All,
    }
    .into();
    connection
        .receive(&frame(settings, |p, b| p.encode(b)), &info)
        .unwrap();
    connection.take_output();
    connection.take_play_events();
    connection.receive(&chat("hidden", 2), &info).unwrap();
    assert!(connection.take_play_events().is_empty());
    assert_eq!(
        answer(&mut connection),
        play::clientbound::SystemChatMessage {
            content: component(&[("translate", "chat.disabled.options"), ("color", "red")]),
            overlay: false,
        }
        .into()
    );

    // A character vanilla does not allow closes the connection.
    connection.receive(&chat("a\u{7f}b", 3), &info).unwrap();
    assert_eq!(
        answer(&mut connection),
        play::clientbound::Disconnect {
            reason: component(&[("translate", "multiplayer.disconnect.illegal_characters")]),
        }
        .into()
    );
    assert!(connection.is_closed());
}

#[test]
fn moving_to_an_adjacent_chunk_updates_only_the_entering_and_leaving_columns() {
    let info = offline();
    let mut connection = playing(&info, new_connection());
    connection.take_output();
    connection.update_view(16.5, 0.5, None);
    let packets: Vec<_> = frames(&connection.take_output())
        .iter()
        .map(|f| play::clientbound::Packet::decode(f).unwrap())
        .collect();
    assert_eq!(
        packets.first(),
        Some(&play::clientbound::SetCenterChunk { x: 1, z: 0 }.into())
    );
    let unloaded: Vec<_> = packets
        .iter()
        .filter_map(|p| match p {
            play::clientbound::Packet::UnloadChunk(p) => Some((p.x, p.z)),
            _ => None,
        })
        .collect();
    let loaded: Vec<_> = packets
        .iter()
        .filter_map(|p| match p {
            play::clientbound::Packet::ChunkDataAndUpdateLight(p) => Some((p.x, p.z)),
            _ => None,
        })
        .collect();
    assert_eq!(unloaded, (-2..=2).map(|z| (-2, z)).collect::<Vec<_>>());
    assert_eq!(loaded, (-2..=2).map(|z| (3, z)).collect::<Vec<_>>());
    assert_eq!(
        packets.last(),
        Some(&play::clientbound::ChunkBatchFinished { batch_size: 5 }.into())
    );
    connection.update_view(17.0, 0.5, None);
    assert!(connection.take_output().is_empty());
    connection.update_view(f64::NAN, 0.5, None);
    assert!(connection.take_output().is_empty());
    connection.update_view(f64::MAX, 0.5, None);
    let packets = frames(&connection.take_output());
    assert_eq!(
        play::clientbound::Packet::decode(&packets[0]),
        Ok(play::clientbound::SetCenterChunk { x: 1_875_000, z: 0 }.into())
    );
}

#[test]
fn external_chunks_are_not_replaced_with_void_during_join_or_movement() {
    let info = offline();
    let mut connection = new_connection();
    connection.use_external_chunks();
    let mut connection = playing(&info, connection);
    let joined: Vec<_> = frames(&connection.take_output())
        .iter()
        .map(|frame| play::clientbound::Packet::decode(frame).unwrap())
        .collect();
    assert!(
        joined
            .iter()
            .any(|packet| matches!(packet, play::clientbound::Packet::SetCenterChunk(_)))
    );
    assert!(!joined.iter().any(|packet| matches!(
        packet,
        play::clientbound::Packet::ChunkDataAndUpdateLight(_)
            | play::clientbound::Packet::ChunkBatchStart(_)
    )));
    connection.update_view(16.5, 0.5, None);
    let moved: Vec<_> = frames(&connection.take_output())
        .iter()
        .map(|frame| play::clientbound::Packet::decode(frame).unwrap())
        .collect();
    assert!(
        moved
            .iter()
            .any(|packet| matches!(packet, play::clientbound::Packet::UnloadChunk(_)))
    );
    assert!(!moved.iter().any(|packet| matches!(
        packet,
        play::clientbound::Packet::ChunkDataAndUpdateLight(_)
            | play::clientbound::Packet::ChunkBatchStart(_)
    )));
}

#[test]
fn clients_without_the_core_pack_are_turned_away() {
    let info = offline();
    let mut connection = logged_in(&info);
    connection.take_output();
    connection
        .receive(&known_packs(&[("minecraft", "core", "26.2")]), &info)
        .unwrap();
    assert!(connection.is_closed());
    let packets = configuration_packets(&connection.take_output());
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0].name(), "Disconnect");
}

#[test]
fn keep_alive_every_15_seconds() {
    let info = offline();
    let mut connection = logged_in(&info);
    connection.take_output();
    let start = Instant::now();
    connection.tick(start).unwrap();
    connection.tick(start + Duration::from_secs(14)).unwrap();
    assert!(connection.take_output().is_empty());
    connection.tick(start + Duration::from_secs(15)).unwrap();
    let packets = configuration_packets(&connection.take_output());
    let [configuration::clientbound::Packet::KeepAlive(keepalive)] = packets.as_slice() else {
        panic!("expected one Keep Alive, got {packets:?}");
    };
    let answer: configuration::serverbound::Packet =
        configuration::serverbound::KeepAlive { id: keepalive.id }.into();
    connection
        .receive(&frame(answer, |p, buf| p.encode(buf)), &info)
        .unwrap();

    // The next one goes unanswered for 15 seconds.
    connection.tick(start + Duration::from_secs(30)).unwrap();
    assert_eq!(configuration_packets(&connection.take_output()).len(), 1);
    assert_eq!(
        connection.tick(start + Duration::from_secs(45)),
        Err(ConnectionError::TimedOut)
    );
    assert!(connection.is_closed());
}

#[test]
fn invalid_player_names_are_refused() {
    let info = offline();
    let mut connection = new_connection();
    let bytes = [
        handshake(775, handshaking::Intent::Login),
        login_start("two words"),
    ]
    .concat();
    assert_eq!(
        connection.receive(&bytes, &info),
        Err(ConnectionError::InvalidUsername("two words".into()))
    );
}

#[test]
fn legacy_ping_is_answered_and_closed() {
    let info = ServerInfo::default();
    let mut connection = new_connection();
    connection.receive(&[0xfe, 0x01], &info).unwrap();
    assert!(connection.is_closed());
    let output = connection.take_output();
    assert_eq!(output[0], 0xff);
    // Anything after the answer is ignored.
    connection.receive(&[0x00], &info).unwrap();
    assert!(connection.take_output().is_empty());
}

#[test]
fn invalid_bytes_close_the_connection() {
    let info = ServerInfo::default();
    let mut connection = new_connection();
    // A Handshake with intent 4.
    let mut bytes = handshake(775, handshaking::Intent::Status);
    *bytes.last_mut().unwrap() = 0x04;
    assert_eq!(
        connection.receive(&bytes, &info),
        Err(ConnectionError::Decode {
            state: State::Handshaking,
            error: DecodeError::UnknownEnumValue {
                name: "Intent",
                value: 4
            }
        })
    );
    assert!(connection.is_closed());
    assert!(connection.take_output().is_empty());

    let mut connection = new_connection();
    assert!(matches!(
        connection.receive(&[0xff, 0xff, 0xff, 0x7f], &info),
        Err(ConnectionError::Frame(_))
    ));
}

/// The length prefix of a frame of `len` bytes, without the bytes.
fn frame_header(len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    harpas_protocol_java::codec::write_varint(&mut out, len as i32);
    out
}

#[test]
fn each_state_limits_its_packet_size_before_buffering() {
    let info = offline();
    let too_large = |len, limit| {
        Err(ConnectionError::Frame(harpas_java::FrameError::TooLarge {
            len,
            limit,
        }))
    };

    let mut connection = new_connection();
    assert_eq!(
        connection.receive(&frame_header(2000), &info),
        too_large(2000, 1024)
    );
    assert!(connection.is_closed());

    let mut connection = new_connection();
    let bytes = [
        handshake(775, handshaking::Intent::Status),
        frame_header(300),
    ]
    .concat();
    assert_eq!(connection.receive(&bytes, &info), too_large(300, 256));

    let mut connection = new_connection();
    let bytes = [
        handshake(775, handshaking::Intent::Login),
        frame_header(40_000),
    ]
    .concat();
    assert_eq!(
        connection.receive(&bytes, &info),
        too_large(40_000, 32 * 1024)
    );

    let mut connection = logged_in(&info);
    connection.take_output();
    assert_eq!(
        connection.receive(&frame_header(70_000), &info),
        too_large(70_000, 64 * 1024)
    );

    // Play keeps vanilla's limit.
    let mut connection = playing(&info, new_connection());
    assert_eq!(connection.receive(&frame_header(70_000), &info), Ok(()));
    assert!(!connection.is_closed());
}

#[test]
fn connections_have_30_seconds_to_log_in() {
    let info = offline();
    let start = Instant::now();

    // Vanilla's message in Login.
    let mut connection = new_connection();
    connection.tick(start).unwrap();
    connection
        .receive(&handshake(775, handshaking::Intent::Login), &info)
        .unwrap();
    connection.tick(start + Duration::from_secs(29)).unwrap();
    assert_eq!(
        connection.tick(start + Duration::from_secs(30)),
        Err(ConnectionError::TimedOut)
    );
    assert!(connection.is_closed());
    assert_eq!(
        login_disconnect_reason(&connection.take_output()),
        r#"{"translate":"multiplayer.disconnect.slow_login"}"#
    );

    // A handshake sent a byte at a time is closed all the same.
    let mut connection = new_connection();
    connection.tick(start).unwrap();
    for byte in handshake(775, handshaking::Intent::Status).iter().take(3) {
        connection.receive(&[*byte], &info).unwrap();
    }
    assert_eq!(
        connection.tick(start + Duration::from_secs(30)),
        Err(ConnectionError::TimedOut)
    );
    assert!(connection.take_output().is_empty());

    // Players who logged in are left to Keep Alive.
    let mut connection = new_connection();
    connection.tick(start).unwrap();
    let mut connection = logged_in_with(&info, connection);
    connection.take_output();
    assert_eq!(connection.tick(start + Duration::from_secs(31)), Ok(()));
    assert!(!connection.is_closed());
}

#[test]
fn logins_wait_for_admission_when_required() {
    let info = offline();
    let bytes = [
        handshake(775, handshaking::Intent::Login),
        login_start("Steve"),
    ]
    .concat();

    let mut connection = new_connection();
    connection.require_admission();
    connection.receive(&bytes, &info).unwrap();
    assert!(connection.awaits_admission());
    assert_eq!(connection.state(), State::Login);
    assert!(connection.take_output().is_empty());
    connection.admit(&info).unwrap();
    assert!(!connection.awaits_admission());
    assert!(matches!(
        login::clientbound::Packet::decode(&frames(&connection.take_output())[0]),
        Ok(login::clientbound::Packet::LoginSuccess(_))
    ));

    let mut connection = new_connection();
    connection.require_admission();
    connection.receive(&bytes, &info).unwrap();
    connection.refuse("Connection throttled! Please wait before reconnecting.");
    assert!(connection.is_closed());
    assert_eq!(
        login_disconnect_reason(&connection.take_output()),
        r#"{"text":"Connection throttled! Please wait before reconnecting."}"#
    );

    // Status pings are never held.
    let mut connection = new_connection();
    connection.require_admission();
    let bytes = [
        handshake(775, handshaking::Intent::Status),
        status_request(),
    ]
    .concat();
    connection.receive(&bytes, &info).unwrap();
    assert!(!connection.awaits_admission());
    assert!(!connection.take_output().is_empty());
}

#[test]
fn a_full_server_refuses_logins_as_vanilla() {
    let bytes = [
        handshake(775, handshaking::Intent::Login),
        login_start("Steve"),
    ]
    .concat();
    let full = ServerInfo {
        max_players: 1,
        online_players: 1,
        ..offline()
    };
    let mut connection = new_connection();
    connection.receive(&bytes, &full).unwrap();
    assert!(connection.is_closed());
    assert!(connection.player().is_none());
    assert_eq!(
        login_disconnect_reason(&connection.take_output()),
        r#"{"translate":"multiplayer.disconnect.server_full"}"#
    );

    let one_free = ServerInfo {
        online_players: 0,
        ..full
    };
    let mut connection = new_connection();
    connection.receive(&bytes, &one_free).unwrap();
    assert!(connection.player().is_some());
}
