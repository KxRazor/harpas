//! The state machine of one connection

use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use harpas_protocol_java::codec::{self, DecodeError, Nbt};
use harpas_protocol_java::v26_1::{self, configuration, handshaking, login, play, status};
use md5::{Digest, Md5};

use crate::chat::{ChatAuthority, ChatEvent, ChatState, MessageError};
use crate::compression::{Compression, CompressionError};
use crate::crypto::{Encryption, ServerKey, ct_equal, server_hash};
use crate::frame::{FrameError, FrameReader, write_frame};
use crate::status::{ServerInfo, json_string};
use crate::{legacy, registries, void_world};

/// Oldest protocol vanilla tells to update the client rather than calling it incompatible: Java 1.16.5.
const OUTDATED_CLIENT_BELOW: i32 = 754;
/// How often the server sends Keep Alive, and how long the client has to answer it, as in vanilla.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
/// Time a connection has to reach Configuration, as vanilla's 600-tick login timeout. It also ends status pings
/// and handshakes sent a byte at a time.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(30);
/// The data pack whose registry data the client must have: Harpas sends registry entries without their data.
const CORE_PACK: (&str, &str, &str) = ("minecraft", "core", v26_1::MINECRAFT_VERSION);
/// Where players appear in the void world: above the middle of chunk (0, 0).
const SPAWN: (f64, f64, f64) = (0.5, 65.0, 0.5);

fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|time| i64::try_from(time.as_millis()).ok())
        .unwrap_or(0)
}

/// The connection states, in the order a connection goes through them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// The first packet, which names the next state.
    Handshaking,
    /// The server list ping.
    Status,
    /// Authentication, encryption and compression.
    Login,
    /// Registry data and the client's settings, before playing.
    Configuration,
    /// The game.
    Play,
    /// The connection is over; the caller sends what is left of the output and closes the socket.
    Closed,
}

/// Largest packet accepted in `state`, before and after decompression; the frame and decompression limits
/// still cap it. Each is well above what a vanilla client sends in that state.
fn packet_limit(state: State) -> usize {
    match state {
        // A handshake's address is at most 255 characters.
        State::Handshaking => 1024,
        State::Status => 256,
        // Login Plugin and Cookie Responses, as from proxies, are the largest.
        State::Login => 32 * 1024,
        // Plugin messages are at most 32,767 bytes in vanilla.
        State::Configuration => 64 * 1024,
        State::Play => usize::MAX,
        State::Closed => 0,
    }
}

/// Why a connection was closed without a message to the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionError {
    /// The bytes were not a valid frame.
    Frame(FrameError),
    /// A packet could not be decoded in the state the connection was in.
    Decode {
        /// The connection's state.
        state: State,
        /// Why the packet is invalid.
        error: DecodeError,
    },
    /// A valid packet arrived when the server does not accept it, such as a second Login Start.
    UnexpectedPacket {
        /// The connection's state.
        state: State,
        /// Name of the packet's type.
        packet: &'static str,
    },
    /// A player name with characters vanilla does not allow.
    InvalidUsername(String),
    /// The client did not answer a Keep Alive in time, answered with another ID, or did not log in in time.
    TimedOut,
    /// A compressed frame was invalid.
    Compression(CompressionError),
    /// The Encryption Response did not decrypt to a shared secret and the verify token the server sent.
    EncryptionFailed,
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectionError::Frame(e) => e.fmt(f),
            ConnectionError::Decode { state, error } => write!(f, "{state:?}: {error}"),
            ConnectionError::UnexpectedPacket { state, packet } => {
                write!(f, "{state:?}: unexpected {packet}")
            }
            ConnectionError::InvalidUsername(name) => write!(f, "invalid player name {name:?}"),
            ConnectionError::TimedOut => f.write_str("timed out"),
            ConnectionError::Compression(e) => e.fmt(f),
            ConnectionError::EncryptionFailed => f.write_str("invalid encryption response"),
        }
    }
}

impl Error for ConnectionError {}

/// The player of a connection, once Login succeeds.
#[derive(Debug, Clone, PartialEq)]
pub struct Player {
    /// The player's name.
    pub name: String,
    /// The player's UUID: Mojang's in online mode, derived from the name as vanilla does in offline mode.
    pub uuid: u128,
    /// Signed profile properties, such as the skin; none in offline mode.
    pub properties: Vec<login::ProfileProperty>,
}

/// Play input to apply on the logic thread, in the order it arrived on this connection.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayEvent {
    /// A movement or settings packet.
    Packet(Box<play::serverbound::Packet>),
    /// An accepted chat session or message.
    Chat(ChatEvent),
}

/// A request to Mojang's session server that the caller makes for the connection, off the logic thread:
/// `GET https://sessionserver.mojang.com/session/minecraft/hasJoined?username=<username>&serverId=<server_hash>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthRequest {
    /// The name the client logged in with.
    pub username: String,
    /// The login hash, which the client also sent to the session server when it joined.
    pub server_hash: String,
}

/// The answer to an [`AuthRequest`].
#[derive(Debug, Clone, PartialEq)]
pub enum AuthOutcome {
    /// The session server knows the join: the player's profile.
    Verified(Player),
    /// The session server answered that the player did not join through it.
    NotVerified,
    /// The session server could not be reached or gave no usable answer.
    Unavailable,
}

/// Where a connection is in the Login state.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LoginStep {
    /// Waiting for Login Start.
    Start,
    /// Encryption Request sent; waiting for the response.
    Encryption { username: String, token: [u8; 4] },
    /// Waiting for the caller to authenticate the player with Mojang.
    Authenticating,
    /// Login Success sent; waiting for Login Acknowledged.
    Success,
    /// Login Acknowledged; the connection is past Login.
    Done,
}

/// A Keep Alive the client has not answered yet.
#[derive(Debug, Clone, Copy)]
struct PendingKeepAlive {
    id: i64,
    sent: Instant,
}

/// One Java Edition connection, without I/O.
///
/// The caller passes every byte read from the socket to [`receive`](Self::receive), calls
/// [`tick`](Self::tick) regularly, writes what [`take_output`](Self::take_output) returns, and closes the socket
/// once the state is [`State::Closed`] and the output is written. A connection whose `receive` or `tick` fails
/// is closed.
#[derive(Debug)]
pub struct Connection {
    state: State,
    key: Arc<ServerKey>,
    frames: FrameReader,
    out: Vec<u8>,
    encryption: Option<Encryption>,
    compression: Option<Compression>,
    received_any: bool,
    opened: Option<Instant>,
    requires_admission: bool,
    awaiting_admission: bool,
    status_sent: bool,
    login: LoginStep,
    auth_request: Option<AuthRequest>,
    player: Option<Player>,
    next_keepalive: Option<Instant>,
    pending_keepalive: Option<PendingKeepAlive>,
    keepalive_counter: i64,
    entity_id: i32,
    play_input: Vec<PlayEvent>,
    chat: ChatState,
    client_view_distance: Option<u8>,
    server_view_distance: i32,
    view_distance: i32,
    center_chunk: (i32, i32),
    biome: i32,
}

impl Connection {
    /// A connection that has received nothing yet. `key` is the server's RSA key, shared by every
    /// connection.
    pub fn new(key: Arc<ServerKey>) -> Self {
        Self::with_entity_id(key, 1)
    }

    /// A connection with the runtime entity ID assigned to its player.
    pub fn with_entity_id(key: Arc<ServerKey>, entity_id: i32) -> Self {
        Self {
            state: State::Handshaking,
            key,
            frames: FrameReader::default(),
            out: Vec::new(),
            encryption: None,
            compression: None,
            received_any: false,
            opened: None,
            requires_admission: false,
            awaiting_admission: false,
            status_sent: false,
            login: LoginStep::Start,
            auth_request: None,
            player: None,
            next_keepalive: None,
            pending_keepalive: None,
            keepalive_counter: 0,
            entity_id,
            play_input: Vec::new(),
            chat: ChatState::default(),
            client_view_distance: None,
            server_view_distance: 2,
            view_distance: 2,
            center_chunk: (0, 0),
            biome: 0,
        }
    }

    /// The connection's state.
    pub fn state(&self) -> State {
        self.state
    }

    /// Whether the connection is over.
    pub fn is_closed(&self) -> bool {
        self.state == State::Closed
    }

    /// The player, once Login succeeds.
    pub fn player(&self) -> Option<&Player> {
        self.player.as_ref()
    }

    /// Whether the connection is encrypted.
    pub fn is_encrypted(&self) -> bool {
        self.encryption.is_some()
    }

    /// The session server request the connection is waiting for, in online mode. The caller makes it and
    /// passes the answer to [`authenticated`](Self::authenticated).
    pub fn take_auth_request(&mut self) -> Option<AuthRequest> {
        self.auth_request.take()
    }

    /// Finishes Login with the session server's answer to the [`AuthRequest`].
    pub fn authenticated(&mut self, outcome: AuthOutcome, info: &ServerInfo) {
        if self.login != LoginStep::Authenticating || self.is_closed() {
            return;
        }
        match outcome {
            AuthOutcome::Verified(player) => self.finish_login(player, info),
            AuthOutcome::NotVerified => self.disconnect_login(&translate(
                "multiplayer.disconnect.unverified_username",
                &[],
            )),
            AuthOutcome::Unavailable => {
                self.disconnect_login(&translate("multiplayer.disconnect.authservers_down", &[]))
            }
        }
    }

    /// Bytes to send to the client, in order. Taking them empties the output.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    /// Sets the certificate authorities used to verify online chat sessions.
    pub fn set_chat_authority(&mut self, authority: ChatAuthority) {
        self.chat.authority(authority);
    }

    /// Takes all player inputs in arrival order, including verified chat events.
    pub fn take_play_events(&mut self) -> Vec<PlayEvent> {
        std::mem::take(&mut self.play_input)
    }

    /// Sends a player message and records its signature for this client's acknowledgement window.
    pub fn send_player_chat(&mut self, mut packet: play::clientbound::PlayerChatMessage) {
        if self.state != State::Play {
            return;
        }
        match self.chat.outgoing(packet.signature) {
            Ok(Some(index)) => {
                packet.global_index = index;
                self.send_play(packet.into());
            }
            Ok(None) => {}
            Err(reason) => self.disconnect_play(reason),
        }
    }

    /// The client's requested view distance, if it sent its settings.
    pub fn client_view_distance(&self) -> Option<u8> {
        self.client_view_distance
    }

    /// Sends an engine-produced Play packet through this connection's compression and encryption.
    pub fn send_play_packet(&mut self, packet: play::clientbound::Packet) {
        if self.state == State::Play {
            self.send_play(packet);
        }
    }

    /// Disconnects a player in Play with a text reason.
    pub fn disconnect_play(&mut self, reason: &str) {
        if self.state == State::Play {
            self.send_play(
                play::clientbound::Disconnect {
                    reason: Nbt::from(reason),
                }
                .into(),
            );
            self.state = State::Closed;
        }
    }

    /// Updates the void chunks sent to the client after an accepted movement or settings change.
    pub fn update_view(&mut self, x: f64, z: f64, client_distance: Option<u8>) {
        if self.state != State::Play || !x.is_finite() || !z.is_finite() {
            return;
        }
        if let Some(distance) = client_distance {
            self.client_view_distance = Some(distance.clamp(2, 32));
        }
        let distance = self
            .client_view_distance
            .map_or(self.server_view_distance, |distance| {
                self.server_view_distance.min(i32::from(distance))
            });
        let center = (
            (x.clamp(-30_000_000.0, 30_000_000.0) / 16.0).floor() as i32,
            (z.clamp(-30_000_000.0, 30_000_000.0) / 16.0).floor() as i32,
        );
        if center == self.center_chunk && distance == self.view_distance {
            return;
        }
        let (old_x, old_z) = self.center_chunk;
        let old_distance = self.view_distance;
        self.center_chunk = center;
        self.view_distance = distance;
        self.send_play(
            play::clientbound::SetCenterChunk {
                x: center.0,
                z: center.1,
            }
            .into(),
        );
        for chunk_x in old_x - old_distance..=old_x + old_distance {
            for chunk_z in old_z - old_distance..=old_z + old_distance {
                if (chunk_x - center.0).abs() > distance || (chunk_z - center.1).abs() > distance {
                    self.send_play(
                        play::clientbound::UnloadChunk {
                            x: chunk_x,
                            z: chunk_z,
                        }
                        .into(),
                    );
                }
            }
        }
        let mut added = Vec::new();
        for chunk_x in center.0 - distance..=center.0 + distance {
            for chunk_z in center.1 - distance..=center.1 + distance {
                if (chunk_x - old_x).abs() > old_distance || (chunk_z - old_z).abs() > old_distance
                {
                    added.push((chunk_x, chunk_z));
                }
            }
        }
        if !added.is_empty() {
            self.send_play(play::clientbound::ChunkBatchStart.into());
            for &(chunk_x, chunk_z) in &added {
                self.send_play(void_world::empty_chunk(chunk_x, chunk_z, self.biome).into());
            }
            self.send_play(
                play::clientbound::ChunkBatchFinished {
                    batch_size: added.len() as i32,
                }
                .into(),
            );
        }
    }

    /// Applies a new client view distance at the current chunk center.
    pub fn update_view_distance(&mut self, distance: u8) {
        let (x, z) = self.center_chunk;
        self.update_view(f64::from(x) * 16.0, f64::from(z) * 16.0, Some(distance));
    }

    /// Makes a handshake that asks to log in wait for [`admit`](Self::admit) or [`refuse`](Self::refuse), so that
    /// the caller can limit login attempts before Login Start is read.
    pub fn require_admission(&mut self) {
        self.requires_admission = true;
    }

    /// Whether the connection waits for [`admit`](Self::admit) or [`refuse`](Self::refuse).
    pub fn awaits_admission(&self) -> bool {
        self.awaiting_admission
    }

    /// Lets a connection waiting for admission log in, handling the bytes it already sent.
    pub fn admit(&mut self, info: &ServerInfo) -> Result<(), ConnectionError> {
        if !self.awaiting_admission {
            return Ok(());
        }
        self.awaiting_admission = false;
        let result = self.handle_frames(info);
        if result.is_err() {
            self.state = State::Closed;
        }
        result
    }

    /// Refuses a connection waiting for admission with Login Disconnect and `text`, and closes it.
    pub fn refuse(&mut self, text: &str) {
        if self.awaiting_admission {
            self.awaiting_admission = false;
            self.disconnect_login(&format!(r#"{{"text":{}}}"#, json_string(text)));
        }
    }

    /// Handles bytes read from the socket. `info` is the server's current settings and list entry.
    pub fn receive(&mut self, bytes: &[u8], info: &ServerInfo) -> Result<(), ConnectionError> {
        if self.is_closed() {
            return Ok(());
        }
        match &mut self.encryption {
            Some(encryption) => {
                let mut bytes = bytes.to_vec();
                encryption.decrypt(&mut bytes);
                self.frames.push(&bytes);
            }
            None => self.frames.push(bytes),
        }
        if !self.received_any && !bytes.is_empty() {
            self.received_any = true;
            // The legacy ping can only be the first thing a connection sends, and comes in one read.
            if let Some(answer) = legacy::answer(self.frames.pending(), info) {
                self.out.extend_from_slice(&answer);
                self.state = State::Closed;
                return Ok(());
            }
        }
        let result = self.handle_frames(info);
        if result.is_err() {
            self.state = State::Closed;
        }
        result
    }

    /// Does the connection's timed work. Until Configuration, the connection fails 30 seconds after the first
    /// call, with vanilla's message in Login. In Configuration and Play, it sends Keep Alive every 15 seconds and
    /// fails if the client takes longer than that to answer.
    pub fn tick(&mut self, now: Instant) -> Result<(), ConnectionError> {
        let opened = *self.opened.get_or_insert(now);
        if matches!(
            self.state,
            State::Handshaking | State::Status | State::Login
        ) {
            if now.duration_since(opened) < LOGIN_TIMEOUT {
                return Ok(());
            }
            if self.state == State::Login {
                self.awaiting_admission = false;
                self.disconnect_login(&translate("multiplayer.disconnect.slow_login", &[]));
            }
            self.state = State::Closed;
            return Err(ConnectionError::TimedOut);
        }
        if !matches!(self.state, State::Configuration | State::Play) {
            return Ok(());
        }
        if let Some(pending) = self.pending_keepalive {
            if now.duration_since(pending.sent) >= KEEPALIVE_INTERVAL {
                self.state = State::Closed;
                return Err(ConnectionError::TimedOut);
            }
            return Ok(());
        }
        let due = *self.next_keepalive.get_or_insert(now + KEEPALIVE_INTERVAL);
        if now >= due {
            self.keepalive_counter += 1;
            let id = self.keepalive_counter;
            if self.state == State::Configuration {
                self.send_configuration(configuration::clientbound::KeepAlive { id }.into());
            } else {
                self.send_play(play::clientbound::KeepAlive { id }.into());
            }
            self.pending_keepalive = Some(PendingKeepAlive { id, sent: now });
            self.next_keepalive = Some(now + KEEPALIVE_INTERVAL);
        }
        Ok(())
    }

    fn keepalive_answered(&mut self, id: i64) -> Result<(), ConnectionError> {
        match self.pending_keepalive {
            Some(pending) if pending.id == id => {
                self.pending_keepalive = None;
                Ok(())
            }
            // Vanilla treats an answer to no Keep Alive, or to another one, as a timeout.
            _ => Err(ConnectionError::TimedOut),
        }
    }

    fn handle_frames(&mut self, info: &ServerInfo) -> Result<(), ConnectionError> {
        while !self.is_closed() && !self.awaiting_admission {
            let limit = packet_limit(self.state);
            let Some(frame) = self
                .frames
                .next_frame_within(limit)
                .map_err(ConnectionError::Frame)?
            else {
                break;
            };
            let frame = match &mut self.compression {
                Some(compression) => compression
                    .decompress(&frame, limit)
                    .map_err(ConnectionError::Compression)?,
                None => frame,
            };
            let decode_error = |state, error| ConnectionError::Decode { state, error };
            match self.state {
                State::Handshaking => {
                    let packet = handshaking::serverbound::Packet::decode(&frame)
                        .map_err(|e| decode_error(State::Handshaking, e))?;
                    self.handshake(packet, info);
                }
                State::Status => {
                    let packet = status::serverbound::Packet::decode(&frame)
                        .map_err(|e| decode_error(State::Status, e))?;
                    self.status(packet, info)?;
                }
                State::Login => {
                    let packet = login::serverbound::Packet::decode(&frame)
                        .map_err(|e| decode_error(State::Login, e))?;
                    self.login(packet, info)?;
                }
                State::Configuration => {
                    let packet = configuration::serverbound::Packet::decode(&frame)
                        .map_err(|e| decode_error(State::Configuration, e))?;
                    self.configuration(packet, info)?;
                }
                State::Play => match play::serverbound::Packet::decode(&frame) {
                    Ok(packet) => self.play(packet, info)?,
                    // Play packets the schema does not have yet are ignored rather than fatal.
                    Err(DecodeError::UnknownPacket(_)) => {}
                    Err(e) => return Err(decode_error(State::Play, e)),
                },
                State::Closed => break,
            }
        }
        Ok(())
    }

    fn handshake(&mut self, packet: handshaking::serverbound::Packet, info: &ServerInfo) {
        let handshaking::serverbound::Packet::Handshake(handshake) = packet;
        match handshake.intent {
            handshaking::Intent::Status => self.state = State::Status,
            handshaking::Intent::Login | handshaking::Intent::Transfer => {
                self.state = State::Login;
                let transfer = handshake.intent == handshaking::Intent::Transfer;
                if transfer && !info.accepts_transfers {
                    self.disconnect_login(&translate(
                        "multiplayer.disconnect.transfers_disabled",
                        &[],
                    ));
                } else if handshake.protocol_version != v26_1::PROTOCOL {
                    let key = if handshake.protocol_version < OUTDATED_CLIENT_BELOW {
                        "multiplayer.disconnect.outdated_client"
                    } else {
                        "multiplayer.disconnect.incompatible"
                    };
                    self.disconnect_login(&translate(key, &[v26_1::MINECRAFT_VERSION]));
                } else if self.requires_admission {
                    self.awaiting_admission = true;
                }
            }
        }
    }

    fn status(
        &mut self,
        packet: status::serverbound::Packet,
        info: &ServerInfo,
    ) -> Result<(), ConnectionError> {
        match packet {
            status::serverbound::Packet::StatusRequest(_) if self.status_sent => {
                // Vanilla answers a status request only once per connection.
                return Err(ConnectionError::UnexpectedPacket {
                    state: State::Status,
                    packet: "StatusRequest",
                });
            }
            status::serverbound::Packet::StatusRequest(_) => {
                self.status_sent = true;
                let response: status::clientbound::Packet = status::clientbound::StatusResponse {
                    json: info.status_json(),
                }
                .into();
                self.send(|buf| response.encode(buf));
            }
            status::serverbound::Packet::PingRequest(ping) => {
                let pong: status::clientbound::Packet = status::clientbound::PongResponse {
                    timestamp: ping.timestamp,
                }
                .into();
                self.send(|buf| pong.encode(buf));
                self.state = State::Closed;
            }
        }
        Ok(())
    }

    fn login(
        &mut self,
        packet: login::serverbound::Packet,
        info: &ServerInfo,
    ) -> Result<(), ConnectionError> {
        let unexpected = |packet: &login::serverbound::Packet| ConnectionError::UnexpectedPacket {
            state: State::Login,
            packet: packet.name(),
        };
        match (packet, &self.login) {
            (login::serverbound::Packet::LoginStart(start), LoginStep::Start) => {
                if !valid_player_name(&start.username) {
                    return Err(ConnectionError::InvalidUsername(start.username));
                }
                if !info.online_mode {
                    let player = Player {
                        uuid: offline_uuid(&start.username),
                        name: start.username,
                        properties: Vec::new(),
                    };
                    self.finish_login(player, info);
                    return Ok(());
                }
                let mut token = [0; 4];
                getrandom::fill(&mut token).expect("the system random number generator works");
                let request: login::clientbound::Packet = login::clientbound::EncryptionRequest {
                    server_id: String::new(),
                    public_key: self.key.public_der().to_vec(),
                    verify_token: token.to_vec(),
                    should_authenticate: true,
                }
                .into();
                self.send(|buf| request.encode(buf));
                self.login = LoginStep::Encryption {
                    username: start.username,
                    token,
                };
                Ok(())
            }
            (
                login::serverbound::Packet::EncryptionResponse(response),
                LoginStep::Encryption { username, token },
            ) => {
                // Both are always decrypted, and a bad padding gives random bytes, so that every invalid
                // response fails here in the same way.
                let secret = self.key.decrypt::<16>(&response.shared_secret);
                let echoed = self.key.decrypt::<4>(&response.verify_token);
                let (Some(secret), Some(echoed)) = (secret, echoed) else {
                    return Err(ConnectionError::EncryptionFailed);
                };
                if !ct_equal(&echoed, token) {
                    return Err(ConnectionError::EncryptionFailed);
                }
                let username = username.clone();
                let mut encryption = Encryption::new(&secret);
                // Bytes the client sent after the response are already encrypted.
                encryption.decrypt(self.frames.pending_mut());
                self.encryption = Some(encryption);
                self.auth_request = Some(AuthRequest {
                    username,
                    server_hash: server_hash("", &secret, self.key.public_der()),
                });
                self.login = LoginStep::Authenticating;
                Ok(())
            }
            (login::serverbound::Packet::LoginAcknowledged(_), LoginStep::Success) => {
                self.login = LoginStep::Done;
                self.state = State::Configuration;
                self.start_configuration();
                Ok(())
            }
            (other, _) => Err(unexpected(&other)),
        }
    }

    /// Sends Set Compression when compression is on, then Login Success.
    fn finish_login(&mut self, player: Player, info: &ServerInfo) {
        if let Ok(threshold) = usize::try_from(info.compression_threshold) {
            let packet: login::clientbound::Packet = login::clientbound::SetCompression {
                threshold: info.compression_threshold,
            }
            .into();
            self.send(|buf| packet.encode(buf));
            self.compression = Some(Compression::new(threshold));
        }
        let success: login::clientbound::Packet = login::clientbound::LoginSuccess {
            uuid: player.uuid,
            username: player.name.clone(),
            properties: player.properties.clone(),
        }
        .into();
        self.send(|buf| success.encode(buf));
        self.player = Some(player);
        self.login = LoginStep::Success;
    }

    /// What vanilla sends when Configuration starts: the server's brand, the enabled features and the data
    /// packs it knows, which the client answers with the packs it has.
    fn start_configuration(&mut self) {
        let mut brand = Vec::new();
        codec::write_string(&mut brand, "Harpas");
        self.send_configuration(
            configuration::clientbound::PluginMessage {
                channel: "minecraft:brand".to_owned(),
                data: brand,
            }
            .into(),
        );
        self.send_configuration(
            configuration::clientbound::FeatureFlags {
                features: vec!["minecraft:vanilla".to_owned()],
            }
            .into(),
        );
        let (namespace, id, version) = CORE_PACK;
        self.send_configuration(
            configuration::clientbound::SelectKnownPacks {
                packs: vec![configuration::KnownPack {
                    namespace: namespace.to_owned(),
                    id: id.to_owned(),
                    version: version.to_owned(),
                }],
            }
            .into(),
        );
    }

    fn configuration(
        &mut self,
        packet: configuration::serverbound::Packet,
        info: &ServerInfo,
    ) -> Result<(), ConnectionError> {
        use configuration::serverbound::Packet;
        match packet {
            Packet::SelectKnownPacks(answer) => {
                let (namespace, id, version) = CORE_PACK;
                let has_core = answer
                    .packs
                    .iter()
                    .any(|p| p.namespace == namespace && p.id == id && p.version == version);
                if !has_core {
                    // Without the core pack, every registry entry would have to be sent with its data.
                    self.disconnect_configuration(&format!(
                        "Harpas needs the client's built-in {namespace}:{id} {version} data."
                    ));
                    return Ok(());
                }
                for registry in registries::registries() {
                    let entries = registry
                        .entries
                        .iter()
                        .map(|entry| configuration::RegistryEntry {
                            id: (*entry).to_owned(),
                            data: None,
                        })
                        .collect();
                    self.send_configuration(
                        configuration::clientbound::RegistryData {
                            registry: registry.id.to_owned(),
                            entries,
                        }
                        .into(),
                    );
                }
                // Tags the registry data refers to, empty until Harpas has vanilla's data.
                let registries = registries::required_tags()
                    .iter()
                    .map(|registry| configuration::RegistryTags {
                        registry: registry.id.to_owned(),
                        tags: registry
                            .entries
                            .iter()
                            .map(|tag| configuration::Tag {
                                name: (*tag).to_owned(),
                                entries: Vec::new(),
                            })
                            .collect(),
                    })
                    .collect();
                self.send_configuration(
                    configuration::clientbound::UpdateTags { registries }.into(),
                );
                self.send_configuration(configuration::clientbound::FinishConfiguration.into());
            }
            Packet::AcknowledgeFinishConfiguration(_) => {
                self.state = State::Play;
                self.join(info);
            }
            Packet::KeepAlive(keepalive) => self.keepalive_answered(keepalive.id)?,
            Packet::ClientInformation(settings) => {
                self.client_view_distance = Some(settings.view_distance.clamp(2, 32) as u8);
                self.chat.enabled(matches!(
                    settings.chat_mode,
                    configuration::ChatMode::Enabled
                ));
            }
            // The client's brand and answers to things the server never sent are not used yet.
            Packet::PluginMessage(_)
            | Packet::Pong(_)
            | Packet::CookieResponse(_)
            | Packet::ResourcePackResponse(_)
            | Packet::CustomClickAction(_)
            | Packet::AcceptCodeOfConduct(_) => {}
        }
        Ok(())
    }

    /// Brings the player into the void world.
    fn join(&mut self, info: &ServerInfo) {
        let player = self.player.clone().expect("Login set the player");
        debug_assert_eq!(self.login, LoginStep::Done);
        let dimension_type =
            registries::entry_id("minecraft:dimension_type", "minecraft:overworld")
                .expect("the overworld dimension type is synchronized");
        let biome = registries::entry_id("minecraft:worldgen/biome", "minecraft:plains")
            .expect("the plains biome is synchronized");
        self.biome = biome;
        self.server_view_distance = info.view_distance.clamp(2, 32) as i32;
        let view_distance = self
            .client_view_distance
            .map_or(self.server_view_distance, |distance| {
                self.server_view_distance.min(i32::from(distance))
            });
        self.view_distance = view_distance;
        let simulation_distance = info.simulation_distance.clamp(2, 32) as i32;

        self.send_play(
            play::clientbound::Login {
                entity_id: self.entity_id,
                is_hardcore: false,
                dimensions: vec!["minecraft:overworld".to_owned()],
                max_players: info.max_players as i32,
                view_distance,
                simulation_distance,
                reduced_debug_info: false,
                enable_respawn_screen: true,
                do_limited_crafting: false,
                spawn: play::SpawnInfo {
                    dimension_type,
                    dimension: "minecraft:overworld".to_owned(),
                    hashed_seed: 0,
                    game_mode: info.game_mode,
                    previous_game_mode: -1,
                    is_debug: false,
                    is_flat: true,
                    death_location: None,
                    portal_cooldown: 0,
                    sea_level: 63,
                },
                enforces_secure_chat: info.enforces_secure_chat,
            }
            .into(),
        );
        self.send_play(
            play::clientbound::PlayerAbilities {
                flags: abilities(info.game_mode),
                flying_speed: 0.05,
                field_of_view_modifier: 0.1,
            }
            .into(),
        );
        self.send_play(
            play::clientbound::PlayerInfoUpdate {
                updates: play::PlayerInfoUpdates {
                    actions: play::PlayerInfoActions {
                        add_player: true,
                        update_game_mode: true,
                        update_listed: true,
                        update_latency: true,
                        update_list_order: true,
                        update_hat: true,
                        ..Default::default()
                    },
                    entries: vec![play::PlayerInfoEntry {
                        uuid: player.uuid,
                        name: player.name,
                        properties: player
                            .properties
                            .into_iter()
                            .map(|p| play::ProfileProperty {
                                name: p.name,
                                value: p.value,
                                signature: p.signature,
                            })
                            .collect(),
                        game_mode: game_mode_id(info.game_mode),
                        listed: true,
                        // With list order 2 before the hat, a client reading the two in the other order
                        // would find 2 where a boolean goes and disconnect: joining confirms the bit order.
                        list_order: 2,
                        show_hat: true,
                        ..Default::default()
                    }],
                },
            }
            .into(),
        );
        self.send_play(
            play::clientbound::SetDefaultSpawnPosition {
                position: play::GlobalPos {
                    dimension: "minecraft:overworld".to_owned(),
                    position: play::BlockPos { x: 0, y: 64, z: 0 },
                },
                yaw: 0.0,
                pitch: 0.0,
            }
            .into(),
        );
        let (x, y, z) = SPAWN;
        self.send_play(
            play::clientbound::SynchronizePlayerPosition {
                teleport_id: 1,
                x,
                y,
                z,
                velocity_x: 0.0,
                velocity_y: 0.0,
                velocity_z: 0.0,
                yaw: 0.0,
                pitch: 0.0,
                relatives: play::TeleportFlags::default(),
            }
            .into(),
        );
        self.send_play(
            play::clientbound::GameEvent {
                event: play::GameEventKind::StartWaitingForChunks,
                value: 0.0,
            }
            .into(),
        );
        self.send_play(play::clientbound::SetCenterChunk { x: 0, z: 0 }.into());
        self.send_play(play::clientbound::ChunkBatchStart.into());
        let mut count = 0;
        for chunk_x in -view_distance..=view_distance {
            for chunk_z in -view_distance..=view_distance {
                self.send_play(void_world::empty_chunk(chunk_x, chunk_z, biome).into());
                count += 1;
            }
        }
        self.send_play(play::clientbound::ChunkBatchFinished { batch_size: count }.into());
    }

    fn play(
        &mut self,
        packet: play::serverbound::Packet,
        info: &ServerInfo,
    ) -> Result<(), ConnectionError> {
        use play::serverbound::Packet;
        match packet {
            Packet::KeepAlive(keepalive) => self.keepalive_answered(keepalive.id)?,
            Packet::PlayerSession(session) if info.online_mode => {
                let uuid = self.player.as_ref().expect("Play has a player").uuid;
                if let Err(reason) = self.chat.session(uuid, session, unix_millis()) {
                    self.disconnect_play(reason);
                }
            }
            Packet::ChatMessage(message) => {
                let uuid = self.player.as_ref().expect("Play has a player").uuid;
                if let Err(error) = self.chat.message(
                    uuid,
                    message,
                    info.online_mode,
                    info.online_mode && info.enforces_secure_chat,
                    unix_millis(),
                ) {
                    match error {
                        MessageError::Fatal(reason) => self.disconnect_play(reason),
                        MessageError::Refused(reason) => self.send_play(
                            play::clientbound::SystemChatMessage {
                                content: Nbt::Compound(
                                    [("text", reason), ("color", "red")].into_iter().collect(),
                                ),
                                overlay: false,
                            }
                            .into(),
                        ),
                    }
                }
            }
            Packet::MessageAcknowledgment(ack) => {
                if let Err(reason) = self.chat.offset(ack.offset) {
                    self.disconnect_play(reason);
                }
            }
            Packet::SignedChatCommand(command) => {
                if let Err(reason) = self.chat.ignored_command(command) {
                    self.disconnect_play(reason);
                }
            }
            Packet::ClientInformation(ref settings) => {
                self.chat
                    .enabled(matches!(settings.chat_mode, play::ChatMode::Enabled));
                self.play_input.push(PlayEvent::Packet(Box::new(packet)));
            }
            Packet::ConfirmTeleportation(_)
            | Packet::SetPlayerPosition(_)
            | Packet::SetPlayerPositionAndRotation(_)
            | Packet::SetPlayerRotation(_)
            | Packet::SetPlayerMovementFlags(_)
            | Packet::PlayerAbilities(_) => {
                self.play_input.push(PlayEvent::Packet(Box::new(packet)))
            }
            _ => {}
        }
        self.play_input
            .extend(self.chat.take_events().into_iter().map(PlayEvent::Chat));
        Ok(())
    }

    /// Frames the packet `encode` writes, compressing and encrypting it when they are on, and queues it.
    fn send(&mut self, encode: impl FnOnce(&mut Vec<u8>)) {
        let mut frame = Vec::new();
        match &mut self.compression {
            Some(compression) => {
                let mut packet = Vec::new();
                encode(&mut packet);
                let body = compression.compress(&packet);
                write_frame(&mut frame, |buf| buf.extend_from_slice(&body));
            }
            None => write_frame(&mut frame, encode),
        }
        if let Some(encryption) = &mut self.encryption {
            encryption.encrypt(&mut frame);
        }
        self.out.extend_from_slice(&frame);
    }

    fn send_configuration(&mut self, packet: configuration::clientbound::Packet) {
        self.send(|buf| packet.encode(buf));
    }

    fn send_play(&mut self, packet: play::clientbound::Packet) {
        self.send(|buf| packet.encode(buf));
    }

    /// Sends Login Disconnect with `reason`, a text component as JSON, and closes the connection.
    fn disconnect_login(&mut self, reason: &str) {
        let packet: login::clientbound::Packet = login::clientbound::LoginDisconnect {
            reason: reason.to_owned(),
        }
        .into();
        self.send(|buf| packet.encode(buf));
        self.state = State::Closed;
    }

    /// Sends the Configuration Disconnect with `text` and closes the connection.
    fn disconnect_configuration(&mut self, text: &str) {
        self.send_configuration(
            configuration::clientbound::Disconnect {
                reason: Nbt::from(text),
            }
            .into(),
        );
        self.state = State::Closed;
    }
}

/// Whether vanilla accepts `name`: no spaces or control characters, and nothing past ASCII.
fn valid_player_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c > ' ' && c < '\u{7f}')
}

/// The UUID vanilla gives a player in offline mode: a version 3 UUID of `OfflinePlayer:<name>`.
pub fn offline_uuid(name: &str) -> u128 {
    let mut bytes: [u8; 16] = Md5::digest(format!("OfflinePlayer:{name}")).into();
    bytes[6] = (bytes[6] & 0x0f) | 0x30;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    u128::from_be_bytes(bytes)
}

fn abilities(game_mode: play::GameMode) -> play::AbilityFlags {
    use play::GameMode::*;
    play::AbilityFlags {
        invulnerable: matches!(game_mode, Creative | Spectator),
        flying: game_mode == Spectator,
        may_fly: matches!(game_mode, Creative | Spectator),
        instant_build: game_mode == Creative,
    }
}

fn game_mode_id(game_mode: play::GameMode) -> i32 {
    match game_mode {
        play::GameMode::Survival => 0,
        play::GameMode::Creative => 1,
        play::GameMode::Adventure => 2,
        play::GameMode::Spectator => 3,
    }
}

/// A translatable text component as JSON.
fn translate(key: &str, with: &[&str]) -> String {
    if with.is_empty() {
        return format!(r#"{{"translate":{}}}"#, json_string(key));
    }
    let with: Vec<_> = with.iter().map(|arg| json_string(arg)).collect();
    format!(
        r#"{{"translate":{},"with":[{}]}}"#,
        json_string(key),
        with.join(",")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_uuids_match_vanilla() {
        // Offline UUIDs as vanilla and Paper derive them.
        assert_eq!(
            offline_uuid("Notch"),
            0xb50ad385_829d_3141_a216_7e7d7539ba7f
        );
    }

    #[test]
    fn player_names_follow_vanilla() {
        assert!(valid_player_name("Steve_01"));
        assert!(!valid_player_name("two words"));
        assert!(!valid_player_name("ñandú"));
        assert!(!valid_player_name(""));
    }
}
