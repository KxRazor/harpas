//! The state machine of one connection

use std::error::Error;
use std::fmt;

use harpas_protocol_java::codec::DecodeError;
use harpas_protocol_java::v26_1::{self, handshaking, login, status};

use crate::frame::{FrameError, FrameReader, write_frame};
use crate::legacy;
use crate::status::{ServerInfo, json_string};

/// Oldest protocol vanilla tells to update the client rather than calling it incompatible: Java 1.16.5.
const OUTDATED_CLIENT_BELOW: i32 = 754;

/// The connection states, in the order a connection goes through them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// The first packet, which names the next state.
    Handshaking,
    /// The server list ping.
    Status,
    /// Authentication, encryption and compression.
    Login,
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
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectionError::Frame(e) => e.fmt(f),
            ConnectionError::Decode { state, error } => write!(f, "{state:?}: {error}"),
            ConnectionError::UnexpectedPacket { state, packet } => {
                write!(f, "{state:?}: unexpected {packet}")
            }
        }
    }
}

impl Error for ConnectionError {}

/// One Java Edition connection, without I/O.
///
/// The caller passes every byte read from the socket to [`receive`](Self::receive), writes what
/// [`take_output`](Self::take_output) returns, and closes the socket once the state is [`State::Closed`] and
/// the output is written. A connection whose `receive` fails is closed.
#[derive(Debug)]
pub struct Connection {
    state: State,
    frames: FrameReader,
    out: Vec<u8>,
    received_any: bool,
    status_sent: bool,
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

    /// Bytes to send to the client, in order. Taking them empties the output.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    /// Handles bytes read from the socket. `info` is what the server list shows at this moment.
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
                    self.login(packet)?;
                }
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

    fn login(&mut self, packet: login::serverbound::Packet) -> Result<(), ConnectionError> {
        match packet {
            login::serverbound::Packet::LoginStart(start) => {
                // Joining needs the Configuration state and registry data, which come next.
                let text = format!(
                    "Hello, {}. Harpas cannot accept players yet.",
                    start.username
                );
                self.disconnect_login(&format!(r#"{{"text":{}}}"#, json_string(&text)));
                Ok(())
            }
            other => Err(ConnectionError::UnexpectedPacket {
                state: State::Login,
                packet: other.name(),
            }),
        }
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
