//! Java Edition packet codecs, generated from Harpas' protocol schema
//!
//! One module per protocol version, such as [`v26_1`], with one module per connection state and one per
//! direction inside it. Each packet is a struct with its `ID`; each direction has a `Packet` enum that encodes
//! and decodes any of them. Framing, compression and encryption belong to the connection, not to the codecs.
//!
//! Design: `docs/protocol-codecs.md`.

pub mod codec;

// Field docs are optional in the schema; the generator requires them for packets and types.
#[allow(missing_docs)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/codecs.rs"));
}

pub use generated::*;
