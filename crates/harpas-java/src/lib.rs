//! Java Edition connections, from the first byte to play
//!
//! [`Connection`] is the state machine of one connection. It does no I/O: the caller feeds it the bytes read
//! from the socket and writes back the bytes it produces, so the same code runs on any network runtime and
//! in tests. Packet codecs come from `harpas-protocol-java`.
//!
//! Design: `docs/java-connection.md`.

mod chat;
mod compression;
mod connection;
mod crypto;
mod frame;
mod legacy;
mod registries;
mod status;
mod void_world;

pub use chat::{ChatAuthority, ChatEvent, VerifiedMessage};
pub use compression::{CompressionError, MAX_UNCOMPRESSED_LEN};
pub use connection::{
    AuthOutcome, AuthRequest, Connection, ConnectionError, PlayEvent, Player, State, offline_uuid,
};
pub use crypto::{ServerKey, server_hash};
pub use frame::{FrameError, FrameReader, MAX_FRAME_LEN, write_frame};
pub use status::ServerInfo;
