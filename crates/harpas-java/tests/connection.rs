//! Whole connections, from the client's first byte, against what vanilla answers.

use harpas_java::{Connection, ConnectionError, FrameReader, ServerInfo, State, write_frame};
use harpas_protocol_java::codec::DecodeError;
use harpas_protocol_java::v26_1::{handshaking, login, status};

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
        username: name.into(),
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
    let mut connection = Connection::new();
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
    let mut connection = Connection::new();
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
    let mut connection = Connection::new();
    connection
        .receive(&handshake(777, handshaking::Intent::Login), &info)
        .unwrap();
    assert!(connection.is_closed());
    assert_eq!(
        login_disconnect_reason(&connection.take_output()),
        r#"{"translate":"multiplayer.disconnect.incompatible","with":["26.1"]}"#
    );

    let mut connection = Connection::new();
    connection
        .receive(&handshake(47, handshaking::Intent::Login), &info)
        .unwrap();
    assert_eq!(
        login_disconnect_reason(&connection.take_output()),
        r#"{"translate":"multiplayer.disconnect.outdated_client","with":["26.1"]}"#
    );

    // The server list still answers, and shows the server's version.
    let mut connection = Connection::new();
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
    let mut connection = Connection::new();
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
    let mut connection = Connection::new();
    connection
        .receive(&handshake(775, handshaking::Intent::Transfer), &info)
        .unwrap();
    assert_eq!(connection.state(), State::Login);
}

#[test]
fn login_is_turned_away_until_configuration_exists() {
    let info = ServerInfo::default();
    let mut connection = Connection::new();
    let bytes = [
        handshake(775, handshaking::Intent::Login),
        login_start("Steve"),
    ]
    .concat();
    connection.receive(&bytes, &info).unwrap();
    assert!(connection.is_closed());
    assert_eq!(
        login_disconnect_reason(&connection.take_output()),
        r#"{"text":"Hello, Steve. Harpas cannot accept players yet."}"#
    );
}

#[test]
fn legacy_ping_is_answered_and_closed() {
    let info = ServerInfo::default();
    let mut connection = Connection::new();
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
    let mut connection = Connection::new();
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

    let mut connection = Connection::new();
    assert!(matches!(
        connection.receive(&[0xff, 0xff, 0xff, 0x7f], &info),
        Err(ConnectionError::Frame(_))
    ));
}
