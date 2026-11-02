//! Java Edition connections, from the first byte to play
//!
//! [`Connection`] is the state machine of one connection. It does no I/O: the caller feeds it the bytes read
//! from the socket and writes back the bytes it produces, so the same code runs on any network runtime and
//! in tests. Packet codecs come from `harpas-protocol-java`.
//!
//! Design: `docs/java-connection.md`.

mod connection;
mod frame;
mod legacy;
mod status;

pub use connection::{Connection, ConnectionError, State};
pub use frame::{FrameError, FrameReader, MAX_FRAME_LEN, write_frame};
pub use status::ServerInfo;
