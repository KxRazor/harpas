//! Chat refusals and disconnections against vanilla.
//!
//! A vanilla server in online mode takes the keys it trusts and the profile of who joins from Mojang's
//! services. Here it is started against a stand-in for them on this machine, which serves a key made for the
//! test, so a client with no Mojang account can sign chat that vanilla accepts, and chat that it must not.
//! The same client then sends the same packets to a Harpas [`Connection`] that trusts the same key, and what
//! each server answers is compared. Design: `docs/parity.md`.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aes::Aes128;
use aes::cipher::array::Array;
use aes::cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyIvInit};
use flate2::read::ZlibDecoder;
use harpas_java::{
    AuthOutcome, ChatAuthority, ChatEvent, Connection, FrameReader, PlayEvent, Player, ServerInfo,
    ServerKey, write_frame,
};
use harpas_protocol_java::codec;
use harpas_protocol_java::v26_1::{configuration, handshaking, login, play};
use rsa::pkcs8::{DecodePublicKey, EncodePublicKey};
use rsa::{Pkcs1v15Encrypt, Pkcs1v15Sign, PrivateKey, PublicKey};
use sha1::Sha1;
use sha2::{Digest, Sha256};

const STEVE: u128 = 0x8667ba71_b85a_4004_af54_457a9734eed7;
const SESSION: u128 = 0x1234_5678_9abc_def0_1234_5678_9abc_def0;
const OTHER_SESSION: u128 = 0x0fed_cba9_8765_4321_0fed_cba9_8765_4321;

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
                Some(_) => {
                    let mut body = &frame[..];
                    let len = codec::read_varint(&mut body).unwrap() as usize;
                    if len == 0 {
                        body.to_vec()
                    } else {
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
                match login::clientbound::Packet::decode(&packet) {
                    Ok(login::clientbound::Packet::SetCompression(set)) => {
                        self.compression = usize::try_from(set.threshold).ok();
                    }
                    Ok(login::clientbound::Packet::LoginSuccess(_)) => self.in_login = false,
                    _ => {}
                }
            }
            packets.push(packet);
        }
        packets
    }
}

/// A server the client talks to: vanilla over a socket, or a Harpas connection in memory.
trait Peer {
    fn send(&mut self, bytes: &[u8]);
    /// What the server sent, waiting up to `wait` when nothing is ready. Empty when nothing came.
    fn receive(&mut self, wait: Duration) -> Vec<u8>;
    /// How long the server takes to answer what it was sent.
    fn settle(&self) -> Duration;
}

struct Vanilla(TcpStream);

impl Peer for Vanilla {
    fn send(&mut self, bytes: &[u8]) {
        // A server that has disconnected the client no longer reads.
        let _ = self.0.write_all(bytes);
    }

    fn receive(&mut self, wait: Duration) -> Vec<u8> {
        self.0.set_read_timeout(Some(wait)).unwrap();
        let mut buf = vec![0; 64 * 1024];
        match self.0.read(&mut buf) {
            Ok(read) => buf[..read].to_vec(),
            Err(_) => Vec::new(),
        }
    }

    fn settle(&self) -> Duration {
        Duration::from_millis(1_500)
    }
}

struct Harpas {
    connection: Connection,
    info: ServerInfo,
}

impl Peer for Harpas {
    fn send(&mut self, bytes: &[u8]) {
        let _ = self.connection.receive(bytes, &self.info);
        if self.connection.take_auth_request().is_some() {
            let steve = Player {
                name: "Steve".into(),
                uuid: STEVE,
                properties: Vec::new(),
            };
            self.connection
                .authenticated(AuthOutcome::Verified(steve), &self.info);
        }
    }

    fn receive(&mut self, _: Duration) -> Vec<u8> {
        // The engine sends a session to every player, its owner included, and an accepted message too.
        for event in self.connection.take_play_events() {
            let PlayEvent::Chat(event) = event else {
                continue;
            };
            let session = match event {
                ChatEvent::Session(session) => Some(session),
                ChatEvent::SessionEnded => None,
                ChatEvent::Message(message) => {
                    let chat = play::clientbound::PlayerChatMessage {
                        global_index: 0,
                        sender: STEVE,
                        index: message.index,
                        signature: message.signature,
                        message: message.message,
                        timestamp: message.timestamp,
                        salt: message.salt,
                        last_seen: vec![],
                        unsigned_content: None,
                        filter_mask: play::FilterMask::PassThrough,
                        chat_type: codec::Holder::Reference(0),
                        sender_name: codec::Nbt::from("Steve"),
                        target_name: None,
                    };
                    self.connection.send_player_chat(chat);
                    continue;
                }
            };
            let update = play::clientbound::PlayerInfoUpdate {
                updates: play::PlayerInfoUpdates {
                    actions: play::PlayerInfoActions {
                        initialize_chat: true,
                        ..Default::default()
                    },
                    entries: vec![play::PlayerInfoEntry {
                        uuid: STEVE,
                        chat_session: session,
                        ..Default::default()
                    }],
                },
            };
            self.connection.send_play_packet(update.into());
        }
        self.connection.take_output()
    }

    fn settle(&self) -> Duration {
        Duration::ZERO
    }
}

/// A client in Play on one of the servers.
struct Session<P> {
    peer: P,
    client: Client,
    /// Packets received and not looked at yet.
    waiting: Vec<Vec<u8>>,
}

impl<P: Peer> Session<P> {
    /// Logs in as Steve in online mode and goes through Configuration into Play.
    fn join(peer: P, port: u16) -> Self {
        let mut session = Self {
            peer,
            client: Client::new(),
            waiting: Vec::new(),
        };
        let handshake: handshaking::serverbound::Packet = handshaking::serverbound::Handshake {
            protocol_version: 775,
            server_address: "localhost".into(),
            server_port: port,
            intent: handshaking::Intent::Login,
        }
        .into();
        let start: login::serverbound::Packet = login::serverbound::LoginStart {
            name: "Steve".into(),
            player_uuid: STEVE,
        }
        .into();
        let bytes = [
            session.client.send(|buf| handshake.encode(buf)),
            session.client.send(|buf| start.encode(buf)),
        ]
        .concat();
        session.peer.send(&bytes);
        let request =
            session.wait_for(
                "Encryption Request",
                |packet| match login::clientbound::Packet::decode(packet) {
                    Ok(login::clientbound::Packet::EncryptionRequest(request)) => Some(request),
                    _ => None,
                },
            );
        let public = PublicKey::from_public_key_der(&request.public_key).unwrap();
        let mut rng = rsa::rand_core::UnwrapErr(getrandom::SysRng);
        let secret = [0x42; 16];
        let response: login::serverbound::Packet = login::serverbound::EncryptionResponse {
            shared_secret: public.encrypt(&mut rng, Pkcs1v15Encrypt, &secret).unwrap(),
            verify_token: public
                .encrypt(&mut rng, Pkcs1v15Encrypt, &request.verify_token)
                .unwrap(),
        }
        .into();
        let bytes = session.client.send(|buf| response.encode(buf));
        // From here on the client encrypts and decrypts with the secret as key and IV.
        let key = Array::from(secret);
        session.client.cipher = Some((
            cfb8::Encryptor::new(&key, &key),
            cfb8::Decryptor::new(&key, &key),
        ));
        session.peer.send(&bytes);
        session.wait_for("Login Success", |packet| {
            matches!(
                login::clientbound::Packet::decode(packet),
                Ok(login::clientbound::Packet::LoginSuccess(_))
            )
            .then_some(())
        });
        let acknowledged: login::serverbound::Packet = login::serverbound::LoginAcknowledged.into();
        session.send(|buf| acknowledged.encode(buf));
        session.send(|buf| {
            codec::write_varint(buf, 0); // Client Information
            codec::write_string(buf, "en_us");
            buf.extend_from_slice(&[2, 0, 1, 127, 1, 0, 1, 0]);
        });
        // Configuration: the server asks which packs the client knows, and later says it has finished.
        loop {
            let finished =
                session.wait_for(
                    "the end of Configuration",
                    |packet| match codec::read_varint(&mut &packet[..]).unwrap() {
                        0x0e => Some(false),
                        3 => Some(true),
                        _ => None,
                    },
                );
            if finished {
                let finished: configuration::serverbound::Packet =
                    configuration::serverbound::AcknowledgeFinishConfiguration.into();
                session.send(|buf| finished.encode(buf));
                break;
            }
            let packs: configuration::serverbound::Packet =
                configuration::serverbound::SelectKnownPacks {
                    packs: vec![configuration::KnownPack {
                        namespace: "minecraft".into(),
                        id: "core".into(),
                        version: "26.1".into(),
                    }],
                }
                .into();
            session.send(|buf| packs.encode(buf));
        }
        session
    }

    fn send(&mut self, encode: impl FnOnce(&mut Vec<u8>)) {
        let bytes = self.client.send(encode);
        self.peer.send(&bytes);
    }

    /// Reads packets until `find` gives a value. The packets after it in the same read are kept for the next
    /// call.
    fn wait_for<T>(&mut self, what: &str, find: impl Fn(&[u8]) -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            while !self.waiting.is_empty() {
                let packet = self.waiting.remove(0);
                if let Some(found) = find(&packet) {
                    return found;
                }
            }
            let bytes = self.peer.receive(Duration::from_millis(200));
            self.waiting = self.client.receive(bytes);
            assert!(
                !self.waiting.is_empty() || Instant::now() < deadline,
                "the server did not send {what}"
            );
        }
    }

    /// The Play packets the server sends in its settling time, with the answers a client owes it.
    fn play(&mut self) -> Vec<play::clientbound::Packet> {
        let deadline = Instant::now() + self.peer.settle();
        let mut packets = Vec::new();
        loop {
            let bytes = self.peer.receive(Duration::from_millis(100));
            let mut received = std::mem::take(&mut self.waiting);
            received.extend(self.client.receive(bytes));
            for packet in received {
                let Ok(packet) = play::clientbound::Packet::decode(&packet) else {
                    continue;
                };
                match &packet {
                    play::clientbound::Packet::KeepAlive(keep) => {
                        let answer: play::serverbound::Packet =
                            play::serverbound::KeepAlive { id: keep.id }.into();
                        self.send(|buf| answer.encode(buf));
                    }
                    play::clientbound::Packet::SynchronizePlayerPosition(position) => {
                        let answer: play::serverbound::Packet =
                            play::serverbound::ConfirmTeleportation {
                                teleport_id: position.teleport_id,
                            }
                            .into();
                        self.send(|buf| answer.encode(buf));
                    }
                    _ => {}
                }
                packets.push(packet);
            }
            if Instant::now() >= deadline {
                return packets;
            }
        }
    }

    /// What the server answers to chat: its disconnection and the chat it sends back, as text.
    fn answers(&mut self) -> Vec<String> {
        self.play()
            .into_iter()
            .filter_map(|packet| match packet {
                play::clientbound::Packet::Disconnect(p) => {
                    Some(format!("disconnect {}", text(&p.reason)))
                }
                play::clientbound::Packet::SystemChatMessage(p) => {
                    Some(format!("system {}", text(&p.content)))
                }
                play::clientbound::Packet::PlayerChatMessage(p) => Some(format!(
                    "player chat {:?}, signed: {}",
                    p.message,
                    p.signature.is_some()
                )),
                play::clientbound::Packet::PlayerInfoUpdate(p)
                    if p.updates.actions.initialize_chat =>
                {
                    let sessions = p.updates.entries.iter();
                    Some(format!(
                        "chat sessions {:?}",
                        sessions
                            .map(|entry| entry.chat_session.as_ref().map(|s| s.session_id))
                            .collect::<Vec<_>>()
                    ))
                }
                play::clientbound::Packet::DisguisedChatMessage(p) => {
                    Some(format!("disguised {:?}", p.message))
                }
                _ => None,
            })
            .collect()
    }
}

/// A text component with its fields in order, since their order means nothing.
fn text(component: &codec::Nbt) -> String {
    match component {
        codec::Nbt::Compound(fields) => {
            let mut fields: Vec<_> = fields
                .iter()
                .map(|(name, value)| format!("{name}: {}", text(value)))
                .collect();
            fields.sort();
            format!("{{{}}}", fields.join(", "))
        }
        other => format!("{other:?}"),
    }
}

/// The keys of a test: Mojang's stand-in, which certifies the player's, and the player's own.
struct Keys {
    authority: PrivateKey,
    signing: PrivateKey,
    /// The key the player signs with once the first is replaced.
    renewed: PrivateKey,
}

impl Keys {
    fn new() -> Self {
        let mut rng = rsa::rand_core::UnwrapErr(getrandom::SysRng);
        Self {
            authority: PrivateKey::new(&mut rng, 2048).unwrap(),
            signing: PrivateKey::new(&mut rng, 2048).unwrap(),
            renewed: PrivateKey::new(&mut rng, 2048).unwrap(),
        }
    }

    fn authority_der(&self) -> Vec<u8> {
        let key = self.authority.to_public_key();
        key.to_public_key_der().unwrap().as_bytes().to_vec()
    }

    /// The player's key, certified until `expires_at`, for the session `id`.
    fn session(&self, id: u128, expires_at: i64) -> play::serverbound::PlayerSession {
        self.session_with(&self.signing, id, expires_at)
    }

    fn session_with(
        &self,
        signing: &PrivateKey,
        id: u128,
        expires_at: i64,
    ) -> play::serverbound::PlayerSession {
        let key = signing.to_public_key();
        let key = key.to_public_key_der().unwrap().as_bytes().to_vec();
        let mut certificate = STEVE.to_be_bytes().to_vec();
        certificate.extend(expires_at.to_be_bytes());
        certificate.extend(&key);
        let key_signature = self
            .authority
            .sign(Pkcs1v15Sign::new::<Sha1>(), &Sha1::digest(certificate))
            .unwrap();
        play::serverbound::PlayerSession {
            session_id: id,
            public_key: play::PublicKeyData {
                expires_at,
                key,
                key_signature,
            },
        }
    }

    /// The message with index `index` of the session `session`, sent at `timestamp`, having seen no message.
    fn message_in(
        &self,
        session: u128,
        text: &str,
        index: i32,
        timestamp: i64,
    ) -> play::serverbound::ChatMessage {
        self.message_with(&self.signing, session, text, index, timestamp)
    }

    fn message_with(
        &self,
        signing: &PrivateKey,
        session: u128,
        text: &str,
        index: i32,
        timestamp: i64,
    ) -> play::serverbound::ChatMessage {
        let salt = 37i64;
        let mut payload = 1i32.to_be_bytes().to_vec();
        payload.extend(STEVE.to_be_bytes());
        payload.extend(session.to_be_bytes());
        payload.extend(index.to_be_bytes());
        payload.extend(salt.to_be_bytes());
        payload.extend((timestamp / 1000).to_be_bytes());
        payload.extend((text.len() as i32).to_be_bytes());
        payload.extend(text.as_bytes());
        payload.extend(0i32.to_be_bytes());
        let signature: [u8; 256] = signing
            .sign(Pkcs1v15Sign::new::<Sha256>(), &Sha256::digest(payload))
            .unwrap()
            .try_into()
            .unwrap();
        play::serverbound::ChatMessage {
            message: text.into(),
            timestamp,
            salt,
            signature: Some(signature),
            offset: 0,
            acknowledged: [0; 3],
            checksum: 0,
        }
    }
}

impl Keys {
    fn message(&self, text: &str, index: i32, timestamp: i64) -> play::serverbound::ChatMessage {
        self.message_in(SESSION, text, index, timestamp)
    }
}

fn now() -> i64 {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    i64::try_from(now.as_millis()).unwrap()
}

/// What a test sends once in Play, and how long it waits before each packet.
type Script = Vec<(Duration, play::serverbound::Packet)>;

/// Writes a script for a client with these keys at this time.
type Author = fn(&Keys, i64) -> Script;

/// The chat a client may send wrongly, each on a connection of its own. A script is written when it is run,
/// since its timestamps are measured from then.
fn scripts() -> Vec<(&'static str, Author)> {
    const NONE: Duration = Duration::ZERO;
    const HOUR: i64 = 3_600_000;
    fn session(keys: &Keys, now: i64) -> (Duration, play::serverbound::Packet) {
        (NONE, keys.session(SESSION, now + HOUR).into())
    }
    fn message(
        keys: &Keys,
        text: &str,
        index: i32,
        timestamp: i64,
    ) -> (Duration, play::serverbound::Packet) {
        (NONE, keys.message(text, index, timestamp).into())
    }
    fn unsigned(keys: &Keys, text: &str, now: i64) -> (Duration, play::serverbound::Packet) {
        let message = play::serverbound::ChatMessage {
            signature: None,
            ..keys.message(text, 0, now)
        };
        (NONE, message.into())
    }
    vec![
        ("a signed message", |keys, now| {
            vec![session(keys, now), message(keys, "hello", 0, now)]
        }),
        ("a message older than the one before", |keys, now| {
            vec![
                session(keys, now),
                message(keys, "first", 0, now),
                message(keys, "second", 1, now - 5_000),
                message(keys, "third", 1, now),
            ]
        }),
        ("an unsigned message in a signing session", |keys, now| {
            vec![
                session(keys, now),
                unsigned(keys, "unsigned", now),
                message(keys, "signed", 0, now),
            ]
        }),
        ("a wrong signature, then a right one", |keys, now| {
            let mut forged = keys.message("forged", 0, now);
            forged.signature.as_mut().unwrap()[0] ^= 1;
            vec![
                session(keys, now),
                (NONE, forged.into()),
                message(keys, "after", 0, now),
            ]
        }),
        ("a broken chain, then a new session", |keys, now| {
            let mut forged = keys.message("forged", 0, now);
            forged.signature.as_mut().unwrap()[0] ^= 1;
            let renewed = keys.message_in(OTHER_SESSION, "renewed", 0, now);
            vec![
                session(keys, now),
                (NONE, forged.into()),
                (NONE, keys.session(OTHER_SESSION, now + HOUR).into()),
                (NONE, renewed.into()),
            ]
        }),
        (
            "a message signed with a key that has expired",
            |keys, now| {
                vec![
                    (NONE, keys.session(SESSION, now + 4_000).into()),
                    (
                        Duration::from_secs(6),
                        keys.message("late", 0, now + 6_000).into(),
                    ),
                ]
            },
        ),
        ("a key that Mojang did not certify", |keys, now| {
            let mut wrong = keys.session(SESSION, now + HOUR);
            wrong.public_key.key_signature[0] ^= 1;
            vec![(NONE, wrong.into()), message(keys, "after", 0, now)]
        }),
        (
            "a key that expired a minute before it was sent",
            |keys, now| {
                vec![
                    (NONE, keys.session(SESSION, now - 60_000).into()),
                    message(keys, "expired", 0, now),
                ]
            },
        ),
        (
            "a key that expired a day before it was sent",
            |keys, now| {
                vec![
                    (NONE, keys.session(SESSION, now - 24 * HOUR).into()),
                    message(keys, "expired", 0, now),
                ]
            },
        ),
        (
            "a key that expired and Mojang did not certify",
            |keys, now| {
                let mut wrong = keys.session(SESSION, now - 60_000);
                wrong.public_key.key_signature[0] ^= 1;
                vec![(NONE, wrong.into()), message(keys, "after", 0, now)]
            },
        ),
        (
            "an unsigned message after a key that had expired",
            |keys, now| {
                vec![
                    (NONE, keys.session(SESSION, now - 60_000).into()),
                    unsigned(keys, "unsigned", now),
                ]
            },
        ),
        ("an expired key in a signing session", |keys, now| {
            let expired = keys.session_with(&keys.renewed, OTHER_SESSION, now - 60_000);
            vec![
                session(keys, now),
                message(keys, "one", 0, now),
                (NONE, expired.into()),
                message(keys, "two", 1, now),
            ]
        }),
        (
            "unsigned messages without a session, the second older",
            |keys, now| {
                let older = play::serverbound::ChatMessage {
                    signature: None,
                    ..keys.message("older", 0, now - 5_000)
                };
                vec![unsigned(keys, "first", now), (NONE, older.into())]
            },
        ),
        ("an unsigned message without a session", |keys, now| {
            vec![unsigned(keys, "no session", now)]
        }),
        ("a signed message without a session", |keys, now| {
            vec![message(keys, "no session", 0, now)]
        }),
        (
            "an acknowledgement of a message that was never sent",
            |keys, now| {
                let mut acknowledged = keys.message("acknowledged", 0, now);
                acknowledged.acknowledged = [1, 0, 0];
                vec![session(keys, now), (NONE, acknowledged.into())]
            },
        ),
        (
            "an acknowledgement offset past the messages sent",
            |keys, now| {
                let mut offset = keys.message("offset", 0, now);
                offset.offset = 5;
                vec![session(keys, now), (NONE, offset.into())]
            },
        ),
        ("the same session twice", |keys, now| {
            vec![
                session(keys, now),
                message(keys, "once", 0, now),
                session(keys, now),
                message(keys, "twice", 1, now),
            ]
        }),
        ("a second session", |keys, now| {
            let other = keys.message_in(OTHER_SESSION, "other", 0, now);
            vec![
                session(keys, now),
                message(keys, "one", 0, now),
                (NONE, keys.session(OTHER_SESSION, now + HOUR).into()),
                (NONE, other.into()),
                message(keys, "old session", 1, now),
            ]
        }),
        ("a second session with a new key", |keys, now| {
            let renewed = keys.session_with(&keys.renewed, OTHER_SESSION, now + HOUR);
            let other = keys.message_with(&keys.renewed, OTHER_SESSION, "other", 0, now);
            vec![
                session(keys, now),
                message(keys, "one", 0, now),
                (NONE, renewed.into()),
                (NONE, other.into()),
                message(keys, "old session", 1, now),
            ]
        }),
        ("a new key after a broken chain", |keys, now| {
            let mut forged = keys.message("forged", 0, now);
            forged.signature.as_mut().unwrap()[0] ^= 1;
            let renewed = keys.session_with(&keys.renewed, OTHER_SESSION, now + HOUR);
            let other = keys.message_with(&keys.renewed, OTHER_SESSION, "renewed", 0, now);
            vec![
                session(keys, now),
                (NONE, forged.into()),
                (NONE, renewed.into()),
                (NONE, other.into()),
            ]
        }),
        ("a message written ten minutes ago", |keys, now| {
            vec![session(keys, now), message(keys, "old", 0, now - 600_000)]
        }),
        ("a message written ten minutes from now", |keys, now| {
            vec![session(keys, now), message(keys, "new", 0, now + 600_000)]
        }),
    ]
}

/// Runs `script` on a server and returns what it answered to each packet.
fn run<P: Peer>(peer: P, port: u16, script: &Script) -> Vec<Vec<String>> {
    let mut session = Session::join(peer, port);
    // The messages of joining come before the script's.
    session.play();
    script
        .iter()
        .map(|(wait, packet)| {
            let waited = Instant::now();
            while waited.elapsed() < *wait {
                session.play();
                thread::sleep(Duration::from_millis(50));
            }
            session.send(|buf| packet.encode(buf));
            session.answers()
        })
        .collect()
}

/// A folder that is removed when dropped.
struct Folder(PathBuf);

impl Folder {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "harpas-chat-parity-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::new();
    for chunk in bytes.chunks(3) {
        let group = chunk
            .iter()
            .fold(0u32, |group, &byte| group << 8 | u32::from(byte))
            << (8 * (3 - chunk.len()));
        for index in 0..4 {
            text.push(if index <= chunk.len() {
                ALPHABET[(group >> (18 - 6 * index) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    text
}

/// Serves what a vanilla server asks Mojang's services: the keys it trusts, which here is `authority`, and
/// the profile of a player who joins, which is always Steve's. Returns the port.
fn serve_mojang(authority: &[u8]) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let key = base64(authority);
    thread::spawn(move || {
        for mut stream in listener.incoming().map_while(Result::ok) {
            let mut request = String::new();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            while reader.read_line(&mut request).is_ok_and(|read| read > 2) {}
            let body = if request.contains("/publickeys") {
                format!(
                    r#"{{"profilePropertyKeys":[{{"publicKey":"{key}"}}],"playerCertificateKeys":[{{"publicKey":"{key}"}}]}}"#
                )
            } else if request.contains("/hasJoined") {
                format!(r#"{{"id":"{STEVE:032x}","name":"Steve","properties":[]}}"#)
            } else {
                "{}".to_owned()
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    port
}

/// A vanilla server in online mode that trusts the services at `mojang`.
struct VanillaServer {
    child: Child,
    lines: Receiver<String>,
    log: String,
    port: u16,
    _folder: Folder,
}

impl VanillaServer {
    fn start(mojang: u16, enforce_secure_profile: bool) -> Self {
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
        fs::write(folder.0.join("eula.txt"), &eula).unwrap();
        let port = {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            listener.local_addr().unwrap().port()
        };
        fs::write(
            folder.0.join("server.properties"),
            format!(
                "server-ip=127.0.0.1\nserver-port={port}\nonline-mode=true\n\
                 enforce-secure-profile={enforce_secure_profile}\n\
                 network-compression-threshold=-1\nview-distance=2\nsimulation-distance=2\n\
                 level-type=minecraft:flat\ngenerate-structures=false\nmax-tick-time=-1\n\
                 difficulty=peaceful\ngamemode=creative\n"
            ),
        )
        .unwrap();
        let host = format!("http://127.0.0.1:{mojang}");
        let mut command = Command::new("java");
        for service in ["auth", "account", "session", "services", "profiles"] {
            command.arg(format!("-Dminecraft.api.{service}.host={host}"));
        }
        let mut child = command
            .arg("-jar")
            .arg(jar)
            .arg("nogui")
            .current_dir(&folder.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start java");
        let (sender, lines) = mpsc::channel();
        let output = child.stdout.take().unwrap();
        thread::spawn(move || {
            for line in BufReader::new(output).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut server = Self {
            child,
            lines,
            log: String::new(),
            port,
            _folder: folder,
        };
        server.wait_for("Done (");
        server
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
                "the reference server exited before {text}: {}",
                self.log
            );
            assert!(
                Instant::now() < deadline,
                "the reference server did not reach {text}: {}",
                self.log
            );
        }
    }

    /// Runs `script` as a client of this server, once the player of the script before has left.
    fn run(&mut self, script: &Script) -> Vec<Vec<String>> {
        let stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        let answers = run(Vanilla(stream), self.port, script);
        self.wait_for("Steve left the game");
        answers
    }
}

impl Drop for VanillaServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Runs `script` on a Harpas connection that trusts `keys`' authority.
fn harpas(keys: &Keys, enforce_secure_profile: bool, script: &Script) -> Vec<Vec<String>> {
    let info = ServerInfo {
        online_mode: true,
        enforces_secure_chat: enforce_secure_profile,
        compression_threshold: -1,
        view_distance: 2,
        ..ServerInfo::default()
    };
    let mut connection = Connection::new(Arc::new(ServerKey::generate()));
    connection.set_chat_authority(ChatAuthority::from_der(&[keys.authority_der()]).unwrap());
    run(Harpas { connection, info }, 25565, script)
}

fn compare(enforce_secure_profile: bool) {
    let keys = Keys::new();
    let mut vanilla =
        VanillaServer::start(serve_mojang(&keys.authority_der()), enforce_secure_profile);
    let mut differences = Vec::new();
    for (name, script) in scripts() {
        let expected = vanilla.run(&script(&keys, now()));
        let actual = harpas(&keys, enforce_secure_profile, &script(&keys, now()));
        if expected != actual {
            differences.push(format!(
                "{name}:\n  vanilla answers {expected:#?}\n  Harpas answers {actual:#?}"
            ));
        }
    }
    assert!(
        differences.is_empty(),
        "enforce-secure-profile={enforce_secure_profile}:\n{}",
        differences.join("\n")
    );
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn chat_answers_match_vanilla_with_secure_profiles_enforced() {
    compare(true);
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn chat_answers_match_vanilla_without_secure_profiles() {
    compare(false);
}
