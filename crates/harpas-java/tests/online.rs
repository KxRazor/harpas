//! Login with encryption, compression and Mojang authentication, against a client written from the documented
//! protocol (minecraft.wiki, "Java Edition protocol/Encryption" and "Java Edition protocol/Packets").

use std::io::Read;
use std::sync::{Arc, OnceLock};

use aes::Aes128;
use aes::cipher::array::Array;
use aes::cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyIvInit};
use flate2::read::ZlibDecoder;
use harpas_java::{
    AuthOutcome, AuthRequest, Connection, ConnectionError, FrameReader, Player, ServerInfo,
    ServerKey, State, server_hash, write_frame,
};
use harpas_protocol_java::codec;
use harpas_protocol_java::v26_1::{configuration, handshaking, login, play};
use rsa::pkcs8::DecodePublicKey;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey};

const STEVE: u128 = 0x8667ba71_b85a_4004_af54_457a9734eed7;

fn key() -> Arc<ServerKey> {
    static KEY: OnceLock<Arc<ServerKey>> = OnceLock::new();
    Arc::clone(KEY.get_or_init(|| Arc::new(ServerKey::generate())))
}

/// The client's side of a connection: framing, compression and encryption as a vanilla client does them.
struct Client {
    reader: FrameReader,
    in_login: bool,
    compression: Option<usize>,
    cipher: Option<(cfb8::Encryptor<Aes128>, cfb8::Decryptor<Aes128>)>,
}

impl Client {
    fn new() -> Self {
        Self {
            reader: FrameReader::default(),
            in_login: true,
            compression: None,
            cipher: None,
        }
    }

    /// The bytes of a packet sent to the server.
    fn send(&mut self, encode: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
        let mut packet = Vec::new();
        encode(&mut packet);
        let mut frame = Vec::new();
        match self.compression {
            // Packets below the threshold go uncompressed, which is all these tests send.
            Some(threshold) => {
                assert!(packet.len() < threshold);
                write_frame(&mut frame, |buf| {
                    codec::write_varint(buf, 0);
                    buf.extend_from_slice(&packet);
                });
            }
            None => write_frame(&mut frame, |buf| buf.extend_from_slice(&packet)),
        }
        if let Some((encryptor, _)) = &mut self.cipher {
            let (blocks, _) = Array::slice_as_chunks_mut(&mut frame);
            encryptor.encrypt_blocks(blocks);
        }
        frame
    }

    /// The packets in bytes from the server.
    fn receive(&mut self, mut bytes: Vec<u8>) -> Vec<Vec<u8>> {
        if let Some((_, decryptor)) = &mut self.cipher {
            let (blocks, _) = Array::slice_as_chunks_mut(&mut bytes);
            decryptor.decrypt_blocks(blocks);
        }
        self.reader.push(&bytes);
        let mut packets = Vec::new();
        while let Some(frame) = self.reader.next_frame().unwrap() {
            let packet = match self.compression {
                Some(threshold) => {
                    let mut body = &frame[..];
                    let len = codec::read_varint(&mut body).unwrap() as usize;
                    if len == 0 {
                        body.to_vec()
                    } else {
                        assert!(len >= threshold);
                        let mut packet = Vec::new();
                        ZlibDecoder::new(body).read_to_end(&mut packet).unwrap();
                        assert_eq!(packet.len(), len);
                        packet
                    }
                }
                None => frame,
            };
            // In Login, Set Compression turns compression on for every later frame, and Login Success ends
            // the state.
            if self.in_login {
                match decode_login(&packet) {
                    login::clientbound::Packet::SetCompression(set) => {
                        self.compression = usize::try_from(set.threshold).ok();
                    }
                    login::clientbound::Packet::LoginSuccess(_) => self.in_login = false,
                    _ => {}
                }
            }
            packets.push(packet);
        }
        packets
    }
}

fn handshake(client: &mut Client) -> Vec<u8> {
    let packet: handshaking::serverbound::Packet = handshaking::serverbound::Handshake {
        protocol_version: 775,
        server_address: "localhost".into(),
        server_port: 25565,
        intent: handshaking::Intent::Login,
    }
    .into();
    client.send(|buf| packet.encode(buf))
}

fn login_packet(client: &mut Client, packet: login::serverbound::Packet) -> Vec<u8> {
    client.send(|buf| packet.encode(buf))
}

fn decode_login(packet: &[u8]) -> login::clientbound::Packet {
    login::clientbound::Packet::decode(packet).unwrap()
}

/// Starts an online login as Steve and answers the Encryption Request; returns the shared secret and the
/// server's public key.
fn encrypted_login(
    connection: &mut Connection,
    client: &mut Client,
    info: &ServerInfo,
    tamper_token: bool,
) -> ([u8; 16], Vec<u8>) {
    let start = login::serverbound::LoginStart {
        username: "Steve".into(),
        player_uuid: 0,
    };
    let bytes = [handshake(client), login_packet(client, start.into())].concat();
    connection.receive(&bytes, info).unwrap();
    let packets = client.receive(connection.take_output());
    let login::clientbound::Packet::EncryptionRequest(request) = decode_login(&packets[0]) else {
        panic!("expected Encryption Request");
    };
    assert_eq!(request.server_id, "");
    assert_eq!(request.verify_token.len(), 4);
    assert!(request.should_authenticate);

    let public = RsaPublicKey::from_public_key_der(&request.public_key).unwrap();
    let mut rng = rsa::rand_core::UnwrapErr(getrandom::SysRng);
    let secret = [0x42; 16];
    let mut token = request.verify_token.clone();
    if tamper_token {
        token[0] ^= 1;
    }
    let response = login::serverbound::EncryptionResponse {
        shared_secret: public.encrypt(&mut rng, Pkcs1v15Encrypt, &secret).unwrap(),
        verify_token: public.encrypt(&mut rng, Pkcs1v15Encrypt, &token).unwrap(),
    };
    let bytes = login_packet(client, response.into());
    // From here on the client encrypts and decrypts with the secret as key and IV.
    let key = Array::from(secret);
    client.cipher = Some((
        cfb8::Encryptor::new(&key, &key),
        cfb8::Decryptor::new(&key, &key),
    ));
    match connection.receive(&bytes, info) {
        Ok(()) => {}
        Err(e) => {
            assert!(tamper_token, "{e}");
            assert_eq!(e, ConnectionError::EncryptionFailed);
        }
    }
    (secret, request.public_key)
}

fn steve() -> Player {
    Player {
        name: "Steve".into(),
        uuid: STEVE,
        properties: vec![login::ProfileProperty {
            name: "textures".into(),
            value: "e30=".into(),
            signature: Some("c2ln".into()),
        }],
    }
}

#[test]
fn online_login_encrypts_compresses_and_uses_the_mojang_profile() {
    let info = ServerInfo {
        online_mode: true,
        ..ServerInfo::default()
    };
    let mut connection = Connection::new(key());
    let mut client = Client::new();
    let (secret, public_key) = encrypted_login(&mut connection, &mut client, &info, false);
    assert!(connection.is_encrypted());
    assert_eq!(
        connection.take_auth_request(),
        Some(AuthRequest {
            username: "Steve".into(),
            server_hash: server_hash("", &secret, &public_key),
        })
    );
    assert!(connection.take_output().is_empty());

    connection.authenticated(AuthOutcome::Verified(steve()), &info);
    let packets = client.receive(connection.take_output());
    // Set Compression goes out uncompressed; the frames after it are compressed.
    assert_eq!(
        decode_login(&packets[0]),
        login::clientbound::SetCompression { threshold: 256 }.into()
    );
    assert_eq!(client.compression, Some(256));
    assert_eq!(
        decode_login(&packets[1]),
        login::clientbound::LoginSuccess {
            uuid: STEVE,
            username: "Steve".into(),
            properties: steve().properties,
        }
        .into()
    );

    // The rest of the connection is encrypted and compressed in both directions.
    let ack = login_packet(&mut client, login::serverbound::LoginAcknowledged.into());
    connection.receive(&ack, &info).unwrap();
    assert_eq!(connection.state(), State::Configuration);
    let packets = client.receive(connection.take_output());
    let names: Vec<_> = packets
        .iter()
        .map(|p| {
            configuration::clientbound::Packet::decode(p)
                .unwrap()
                .name()
        })
        .collect();
    assert_eq!(names, ["PluginMessage", "FeatureFlags", "SelectKnownPacks"]);

    let known: configuration::serverbound::Packet = configuration::serverbound::SelectKnownPacks {
        packs: vec![configuration::KnownPack {
            namespace: "minecraft".into(),
            id: "core".into(),
            version: "26.1".into(),
        }],
    }
    .into();
    connection
        .receive(&client.send(|buf| known.encode(buf)), &info)
        .unwrap();
    assert_eq!(client.receive(connection.take_output()).len(), 30);
    let finish: configuration::serverbound::Packet =
        configuration::serverbound::AcknowledgeFinishConfiguration.into();
    connection
        .receive(&client.send(|buf| finish.encode(buf)), &info)
        .unwrap();
    let packets = client.receive(connection.take_output());
    assert!(matches!(
        play::clientbound::Packet::decode(&packets[0]),
        Ok(play::clientbound::Packet::Login(_))
    ));

    // Engine output and movement inputs use the same continuous encryption and compression streams.
    let removal: play::clientbound::Packet = play::clientbound::PlayerInfoRemove {
        players: vec![STEVE],
    }
    .into();
    connection.send_play_packet(removal.clone());
    let packets = client.receive(connection.take_output());
    assert_eq!(play::clientbound::Packet::decode(&packets[0]), Ok(removal));
    let profile: play::clientbound::Packet = play::clientbound::PlayerInfoUpdate {
        updates: play::PlayerInfoUpdates {
            actions: play::PlayerInfoActions {
                add_player: true,
                update_listed: true,
                ..Default::default()
            },
            entries: vec![play::PlayerInfoEntry {
                uuid: STEVE,
                name: "Steve".into(),
                properties: vec![play::ProfileProperty {
                    name: "textures".into(),
                    value: "x".repeat(300),
                    signature: Some("signature".into()),
                }],
                listed: true,
                ..Default::default()
            }],
        },
    }
    .into();
    connection.send_play_packet(profile.clone());
    let packets = client.receive(connection.take_output());
    assert_eq!(play::clientbound::Packet::decode(&packets[0]), Ok(profile));
    let movement: play::serverbound::Packet = play::serverbound::SetPlayerPosition {
        x: 1.5,
        y: 65.0,
        z: 0.5,
        flags: play::MovementFlags::default(),
    }
    .into();
    connection
        .receive(&client.send(|buf| movement.encode(buf)), &info)
        .unwrap();
    assert_eq!(connection.take_play_input(), vec![movement]);
}

#[test]
fn a_wrong_verify_token_closes_the_connection() {
    let info = ServerInfo::default();
    let mut connection = Connection::new(key());
    let mut client = Client::new();
    encrypted_login(&mut connection, &mut client, &info, true);
    assert!(connection.is_closed());
    assert_eq!(connection.take_auth_request(), None);
}

#[test]
fn players_mojang_does_not_know_are_refused() {
    let info = ServerInfo::default();
    for (outcome, key_name) in [
        (
            AuthOutcome::NotVerified,
            "multiplayer.disconnect.unverified_username",
        ),
        (
            AuthOutcome::Unavailable,
            "multiplayer.disconnect.authservers_down",
        ),
    ] {
        let mut connection = Connection::new(key());
        let mut client = Client::new();
        encrypted_login(&mut connection, &mut client, &info, false);
        connection.take_auth_request().unwrap();
        connection.authenticated(outcome, &info);
        assert!(connection.is_closed());
        let packets = client.receive(connection.take_output());
        assert_eq!(
            decode_login(&packets[0]),
            login::clientbound::LoginDisconnect {
                reason: format!(r#"{{"translate":"{key_name}"}}"#)
            }
            .into()
        );
    }
}

#[test]
fn offline_mode_compresses_large_packets() {
    let info = ServerInfo {
        online_mode: false,
        compression_threshold: 64,
        ..ServerInfo::default()
    };
    let mut connection = Connection::new(key());
    let mut client = Client::new();
    let start = login::serverbound::LoginStart {
        username: "Steve".into(),
        player_uuid: 0,
    };
    let bytes = [
        handshake(&mut client),
        login_packet(&mut client, start.into()),
    ]
    .concat();
    connection.receive(&bytes, &info).unwrap();
    assert!(!connection.is_encrypted());
    let packets = client.receive(connection.take_output());
    assert_eq!(
        decode_login(&packets[0]),
        login::clientbound::SetCompression { threshold: 64 }.into()
    );
    assert_eq!(client.compression, Some(64));
    let ack = login_packet(&mut client, login::serverbound::LoginAcknowledged.into());
    connection.receive(&ack, &info).unwrap();

    // The client has the core pack; the registry data that follows is mostly above the threshold.
    let known = configuration::serverbound::SelectKnownPacks {
        packs: vec![configuration::KnownPack {
            namespace: "minecraft".into(),
            id: "core".into(),
            version: "26.1".into(),
        }],
    };
    let packet: configuration::serverbound::Packet = known.into();
    connection.take_output();
    let bytes = client.send(|buf| packet.encode(buf));
    connection.receive(&bytes, &info).unwrap();
    let packets = client.receive(connection.take_output());
    assert_eq!(packets.len(), 30);
    for packet in &packets {
        configuration::clientbound::Packet::decode(packet).unwrap();
    }
}
