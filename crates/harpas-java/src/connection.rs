//! The state machine of one connection

use std::error::Error;
use std::fmt;
use std::time::{Duration, Instant};

use harpas_protocol_java::codec::{self, DecodeError, Nbt};
use harpas_protocol_java::v26_1::{self, configuration, handshaking, login, play, status};
use md5::{Digest, Md5};

use crate::frame::{FrameError, FrameReader, write_frame};
use crate::status::{ServerInfo, json_string};
use crate::{legacy, registries, void_world};

/// Oldest protocol vanilla tells to update the client rather than calling it incompatible: Java 1.16.5.
const OUTDATED_CLIENT_BELOW: i32 = 754;
/// How often the server sends Keep Alive, and how long the client has to answer it, as in vanilla.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
/// The data pack whose registry data the client must have: Harpas sends registry entries without their data.
const CORE_PACK: (&str, &str, &str) = ("minecraft", "core", v26_1::MINECRAFT_VERSION);
/// Where players appear in the void world: above the middle of chunk (0, 0).
const SPAWN: (f64, f64, f64) = (0.5, 65.0, 0.5);
/// Entity ID of the player. There is one player per connection and no entities yet.
const PLAYER_ENTITY_ID: i32 = 1;

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
    /// The client did not answer a Keep Alive in time, or answered with another ID.
    TimedOut,
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
        }
    }
}

impl Error for ConnectionError {}

/// The player of a connection, once Login Start names them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Player {
    /// The player's name.
    pub name: String,
    /// The player's UUID: in offline mode, derived from the name as vanilla does.
    pub uuid: u128,
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
    frames: FrameReader,
    out: Vec<u8>,
    received_any: bool,
    status_sent: bool,
    player: Option<Player>,
    login_acknowledged: bool,
    next_keepalive: Option<Instant>,
    pending_keepalive: Option<PendingKeepAlive>,
    keepalive_counter: i64,
}

impl Default for Connection {
    fn default() -> Self {
        Self::new()
    }
}

impl Connection {
    /// A connection that has received nothing yet.
    pub fn new() -> Self {
        Self {
            state: State::Handshaking,
            frames: FrameReader::default(),
            out: Vec::new(),
            received_any: false,
            status_sent: false,
            player: None,
            login_acknowledged: false,
            next_keepalive: None,
            pending_keepalive: None,
            keepalive_counter: 0,
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

    /// The player, once Login Start names them.
    pub fn player(&self) -> Option<&Player> {
        self.player.as_ref()
    }

    /// Bytes to send to the client, in order. Taking them empties the output.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    /// Handles bytes read from the socket. `info` is the server's current settings and list entry.
    pub fn receive(&mut self, bytes: &[u8], info: &ServerInfo) -> Result<(), ConnectionError> {
        if self.is_closed() {
            return Ok(());
        }
        self.frames.push(bytes);
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

    /// Does the connection's timed work: in Configuration and Play, sends Keep Alive every 15 seconds and
    /// fails if the client takes longer than that to answer.
    pub fn tick(&mut self, now: Instant) -> Result<(), ConnectionError> {
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
        while !self.is_closed() {
            let Some(frame) = self.frames.next_frame().map_err(ConnectionError::Frame)? else {
                break;
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
                    Ok(packet) => self.play(packet)?,
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
                write_frame(&mut self.out, |buf| response.encode(buf));
            }
            status::serverbound::Packet::PingRequest(ping) => {
                let pong: status::clientbound::Packet = status::clientbound::PongResponse {
                    timestamp: ping.timestamp,
                }
                .into();
                write_frame(&mut self.out, |buf| pong.encode(buf));
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
        match packet {
            login::serverbound::Packet::LoginStart(start) if self.player.is_none() => {
                if !valid_player_name(&start.username) {
                    return Err(ConnectionError::InvalidUsername(start.username));
                }
                if info.online_mode {
                    // Authentication with Mojang comes with encryption, in a later task.
                    let text = "Harpas does not support online mode yet: set online-mode=false in \
                                server.properties.";
                    self.disconnect_login(&format!(r#"{{"text":{}}}"#, json_string(text)));
                    return Ok(());
                }
                let player = Player {
                    uuid: offline_uuid(&start.username),
                    name: start.username,
                };
                let success: login::clientbound::Packet = login::clientbound::LoginSuccess {
                    uuid: player.uuid,
                    username: player.name.clone(),
                    properties: Vec::new(),
                }
                .into();
                write_frame(&mut self.out, |buf| success.encode(buf));
                self.player = Some(player);
                Ok(())
            }
            login::serverbound::Packet::LoginAcknowledged(_)
                if self.player.is_some() && !self.login_acknowledged =>
            {
                self.login_acknowledged = true;
                self.state = State::Configuration;
                self.start_configuration();
                Ok(())
            }
            other => Err(unexpected(&other)),
        }
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
            // Settings, the client's brand and answers to things the server never sent are not used yet.
            Packet::ClientInformation(_)
            | Packet::PluginMessage(_)
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
        let dimension_type =
            registries::entry_id("minecraft:dimension_type", "minecraft:overworld")
                .expect("the overworld dimension type is synchronized");
        let biome = registries::entry_id("minecraft:worldgen/biome", "minecraft:plains")
            .expect("the plains biome is synchronized");
        let view_distance = info.view_distance.clamp(2, 32) as i32;
        let simulation_distance = info.simulation_distance.clamp(2, 32) as i32;

        self.send_play(
            play::clientbound::Login {
                entity_id: PLAYER_ENTITY_ID,
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

    fn play(&mut self, packet: play::serverbound::Packet) -> Result<(), ConnectionError> {
        if let play::serverbound::Packet::KeepAlive(keepalive) = packet {
            self.keepalive_answered(keepalive.id)?;
        }
        // Movement, chat and the rest come with the logic thread, in later tasks.
        Ok(())
    }

    fn send_configuration(&mut self, packet: configuration::clientbound::Packet) {
        write_frame(&mut self.out, |buf| packet.encode(buf));
    }

    fn send_play(&mut self, packet: play::clientbound::Packet) {
        write_frame(&mut self.out, |buf| packet.encode(buf));
    }

    /// Sends Login Disconnect with `reason`, a text component as JSON, and closes the connection.
    fn disconnect_login(&mut self, reason: &str) {
        let packet: login::clientbound::Packet = login::clientbound::LoginDisconnect {
            reason: reason.to_owned(),
        }
        .into();
        write_frame(&mut self.out, |buf| packet.encode(buf));
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
