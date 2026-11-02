//! NetherNet transport for Bedrock connections
//!
//! Clients join in two steps: HTTP signaling ([`signaling`]), then a WebRTC connection whose reliable data
//! channel carries game batches split into segments ([`segment`]). [`Server`] runs the WebRTC side on one UDP
//! socket and does no I/O itself; the answer carries the server's [`identity::Identity`].
//!
//! Design: `docs/nethernet.md`.

pub mod identity;
pub mod segment;
pub mod server;
pub mod signaling;

pub use server::{Event, OfferError, PeerId, Server};
