//! Handshaking, Status and Login packets against bytes written by hand from the documented wire format
//! (minecraft.wiki, "Java Edition protocol/Packets"), not from the generator's output.

use harpas_protocol_java::codec::DecodeError;
use harpas_protocol_java::v26_1::{self, handshaking, login, status};

const NOTCH: u128 = 0x069a79f4_44e9_4726_a5be_fca90e38aaf5;

/// Decodes `bytes`, checks the packet, and encodes it back to the same bytes.
macro_rules! round_trip {
    ($packet:ty, $bytes:expr, $expected:expr) => {{
        let bytes: Vec<u8> = $bytes;
        let decoded = <$packet>::decode(&bytes).expect("valid packet");
        assert_eq!(decoded, $expected.into());
        let mut encoded = Vec::new();
        decoded.encode(&mut encoded);
        assert_eq!(encoded, bytes);
    }};
}

fn concat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

#[test]
fn version_constants() {
    assert_eq!(v26_1::PROTOCOL, 775);
    assert_eq!(v26_1::MINECRAFT_VERSION, "26.1");
}

#[test]
fn handshake() {
    round_trip!(
        handshaking::serverbound::Packet,
        // ID, protocol 775, "localhost", port 25565, intent 1 (Status).
        concat(&[&[0x00, 0x87, 0x06, 0x09], b"localhost", &[0x63, 0xdd, 0x01]]),
        handshaking::serverbound::Handshake {
            protocol_version: 775,
            server_address: "localhost".into(),
            server_port: 25565,
            intent: handshaking::Intent::Status,
        }
    );
    let transfer = concat(&[&[0x00, 0x87, 0x06, 0x00, 0x63, 0xdd, 0x03]]);
    let decoded = handshaking::serverbound::Packet::decode(&transfer).unwrap();
    let handshaking::serverbound::Packet::Handshake(handshake) = decoded;
    assert_eq!(handshake.intent, handshaking::Intent::Transfer);
}

#[test]
fn status() {
    round_trip!(
        status::serverbound::Packet,
        vec![0x00],
        status::serverbound::StatusRequest
    );
    round_trip!(
        status::serverbound::Packet,
        concat(&[&[0x01], &0x0123_4567_89ab_cdef_i64.to_be_bytes()]),
        status::serverbound::PingRequest {
            timestamp: 0x0123_4567_89ab_cdef
        }
    );
    round_trip!(
        status::clientbound::Packet,
        concat(&[&[0x00, 0x02], b"{}"]),
        status::clientbound::StatusResponse { json: "{}".into() }
    );
    round_trip!(
        status::clientbound::Packet,
        concat(&[&[0x01], &(-1_i64).to_be_bytes()]),
        status::clientbound::PongResponse { timestamp: -1 }
    );
}

#[test]
fn login_serverbound() {
    round_trip!(
        login::serverbound::Packet,
        concat(&[&[0x00, 0x05], b"Notch", &NOTCH.to_be_bytes()]),
        login::serverbound::LoginStart {
            username: "Notch".into(),
            player_uuid: NOTCH
        }
    );
    round_trip!(
        login::serverbound::Packet,
        concat(&[&[0x01, 0x03, 1, 2, 3, 0x02, 4, 5]]),
        login::serverbound::EncryptionResponse {
            shared_secret: vec![1, 2, 3],
            verify_token: vec![4, 5]
        }
    );
    round_trip!(
        login::serverbound::Packet,
        vec![0x02, 0x07, 0x00],
        login::serverbound::LoginPluginResponse {
            message_id: 7,
            data: None
        }
    );
    round_trip!(
        login::serverbound::Packet,
        vec![0x02, 0x07, 0x01, 0xaa, 0xbb],
        login::serverbound::LoginPluginResponse {
            message_id: 7,
            data: Some(vec![0xaa, 0xbb])
        }
    );
    round_trip!(
        login::serverbound::Packet,
        vec![0x03],
        login::serverbound::LoginAcknowledged
    );
    round_trip!(
        login::serverbound::Packet,
        concat(&[&[0x04, 0x05], b"a:key", &[0x01, 0x01, 0x2a]]),
        login::serverbound::CookieResponse {
            key: "a:key".into(),
            payload: Some(vec![42])
        }
    );
}

#[test]
fn login_clientbound() {
    round_trip!(
        login::clientbound::Packet,
        concat(&[&[0x00, 0x0d], br#"{"text":"No"}"#]),
        login::clientbound::LoginDisconnect {
            reason: r#"{"text":"No"}"#.into()
        }
    );
    round_trip!(
        login::clientbound::Packet,
        concat(&[&[0x01, 0x00, 0x02, 0x30, 0x31, 0x04, 9, 8, 7, 6, 0x01]]),
        login::clientbound::EncryptionRequest {
            server_id: String::new(),
            public_key: vec![0x30, 0x31],
            verify_token: vec![9, 8, 7, 6],
            should_authenticate: true,
        }
    );
    round_trip!(
        login::clientbound::Packet,
        concat(&[
            &[0x02],
            &NOTCH.to_be_bytes(),
            &[0x05],
            b"Notch",
            // Two properties: one signed, one not.
            &[0x02, 0x08],
            b"textures",
            &[0x01],
            b"v",
            &[0x01, 0x01],
            b"s",
            &[0x01],
            b"x",
            &[0x00],
            &[0x00],
        ]),
        login::clientbound::LoginSuccess {
            uuid: NOTCH,
            username: "Notch".into(),
            properties: vec![
                login::ProfileProperty {
                    name: "textures".into(),
                    value: "v".into(),
                    signature: Some("s".into())
                },
                login::ProfileProperty {
                    name: "x".into(),
                    value: String::new(),
                    signature: None
                },
            ],
        }
    );
    round_trip!(
        login::clientbound::Packet,
        vec![0x03, 0x80, 0x02],
        login::clientbound::SetCompression { threshold: 256 }
    );
    round_trip!(
        login::clientbound::Packet,
        concat(&[&[0x04, 0x01, 0x03], b"a:b", &[1, 2, 3]]),
        login::clientbound::LoginPluginRequest {
            message_id: 1,
            channel: "a:b".into(),
            data: vec![1, 2, 3]
        }
    );
    round_trip!(
        login::clientbound::Packet,
        concat(&[&[0x05, 0x03], b"a:b"]),
        login::clientbound::CookieRequest { key: "a:b".into() }
    );
}

#[test]
fn packet_metadata() {
    let packet: login::clientbound::Packet =
        login::clientbound::SetCompression { threshold: 0 }.into();
    assert_eq!(packet.id(), 0x03);
    assert_eq!(packet.name(), "SetCompression");
    assert_eq!(login::serverbound::LoginAcknowledged::ID, 0x03);
}

#[test]
fn invalid_packets_are_rejected() {
    use login::serverbound::Packet;

    let cases: Vec<(Vec<u8>, DecodeError)> = vec![
        (vec![], DecodeError::UnexpectedEnd),
        (vec![0x7f], DecodeError::UnknownPacket(0x7f)),
        // Login Acknowledged has no fields.
        (vec![0x03, 0x00], DecodeError::TrailingBytes(1)),
        // A 17-character username, over vanilla's 16.
        (
            concat(&[&[0x00, 0x11], b"abcdefghijklmnopq", &[0; 16]]),
            DecodeError::TooLong { len: 17, max: 16 },
        ),
        // A username cut short, then a missing UUID.
        (vec![0x00, 0x05, b'N'], DecodeError::UnexpectedEnd),
        (
            concat(&[&[0x00, 0x05], b"Notch", &[0; 15]]),
            DecodeError::UnexpectedEnd,
        ),
        (vec![0x00, 0x02, 0xc3, 0x28], DecodeError::InvalidUtf8),
        // Login Plugin Response with a presence byte of 2.
        (vec![0x02, 0x07, 0x02], DecodeError::InvalidBool(2)),
        // A cookie of 5121 bytes, over vanilla's 5120.
        (
            vec![0x04, 0x00, 0x01, 0x81, 0x28],
            DecodeError::TooLong {
                len: 5121,
                max: 5120,
            },
        ),
        // A shared secret claiming 2^31 - 1 bytes.
        (
            vec![0x01, 0xff, 0xff, 0xff, 0xff, 0x07],
            DecodeError::UnexpectedEnd,
        ),
    ];
    for (bytes, error) in cases {
        assert_eq!(Packet::decode(&bytes), Err(error), "{bytes:02x?}");
    }
    assert_eq!(
        handshaking::serverbound::Packet::decode(&[0x00, 0x87, 0x06, 0x00, 0x63, 0xdd, 0x04]),
        Err(DecodeError::UnknownEnumValue {
            name: "Intent",
            value: 4
        })
    );
    let too_many = concat(&[
        &[0x02],
        &NOTCH.to_be_bytes(),
        &[0x00, 0x11],
        &[0x00; 17 * 3],
    ]);
    assert_eq!(
        login::clientbound::Packet::decode(&too_many),
        Err(DecodeError::TooLong { len: 17, max: 16 })
    );
}
