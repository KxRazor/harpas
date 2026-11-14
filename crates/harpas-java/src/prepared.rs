//! Shareable Play frames, compressed before each connection encrypts its own stream.

/// An encoded and framed Java 26.1 Play packet, ready for connections with the same compression threshold.
///
/// Created by [`Connection::prepare_play_packet`](crate::Connection::prepare_play_packet). It contains no
/// encryption state and can be shared between threads and sent repeatedly.
#[derive(Debug)]
pub struct PreparedPlayPacket {
    pub(crate) frame: Vec<u8>,
    pub(crate) compression_threshold: Option<usize>,
}

impl PreparedPlayPacket {
    /// Bytes retained by the frame's buffer, for bounding an encoding cache's memory use.
    pub fn retained_bytes(&self) -> usize {
        self.frame.capacity()
    }
}
