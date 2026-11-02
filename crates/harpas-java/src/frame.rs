//! Framing: each packet is a VarInt length, then the packet ID and its data

use std::error::Error;
use std::fmt;

use harpas_protocol_java::codec;

/// Longest frame accepted, as in vanilla: the largest length a 3-byte VarInt holds.
pub const MAX_FRAME_LEN: usize = (1 << 21) - 1;

/// Why the bytes from a client are not a valid frame. Either closes the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// The length prefix was longer than 3 bytes.
    LengthTooLong,
    /// The frame was longer than [`MAX_FRAME_LEN`].
    TooLarge(usize),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::LengthTooLong => f.write_str("frame length is wider than 21 bits"),
            FrameError::TooLarge(len) => {
                write!(
                    f,
                    "frame of {len} bytes is over the limit of {MAX_FRAME_LEN}"
                )
            }
        }
    }
}

impl Error for FrameError {}

/// Splits the bytes read from a socket into frames.
///
/// It never holds more than one frame and the bytes after it that were already read, since a frame's length
/// is checked before its bytes are waited for.
#[derive(Debug, Default)]
pub struct FrameReader {
    buf: Vec<u8>,
    start: usize,
}

impl FrameReader {
    /// Adds bytes read from the socket.
    pub fn push(&mut self, bytes: &[u8]) {
        if self.start > 0 && self.start == self.buf.len() {
            self.buf.clear();
            self.start = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// Bytes received and not yet returned in a frame.
    pub fn pending(&self) -> &[u8] {
        &self.buf[self.start..]
    }

    /// Takes the next complete frame, without its length prefix, or none until more bytes arrive.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        let pending = self.pending();
        let mut len = 0usize;
        let mut prefix = 0;
        loop {
            let Some(&byte) = pending.get(prefix) else {
                return Ok(None);
            };
            len |= usize::from(byte & 0x7f) << (7 * prefix);
            prefix += 1;
            if byte & 0x80 == 0 {
                break;
            }
            if prefix == 3 {
                return Err(FrameError::LengthTooLong);
            }
        }
        if len > MAX_FRAME_LEN {
            return Err(FrameError::TooLarge(len));
        }
        if pending.len() < prefix + len {
            return Ok(None);
        }
        let frame = pending[prefix..prefix + len].to_vec();
        self.start += prefix + len;
        // Reclaim the space of frames already taken once it is most of the buffer.
        if self.start > self.buf.len() / 2 {
            self.buf.drain(..self.start);
            self.start = 0;
        }
        Ok(Some(frame))
    }
}

/// Appends a frame to `out`: the length of what `encode` writes, then those bytes.
pub fn write_frame(out: &mut Vec<u8>, encode: impl FnOnce(&mut Vec<u8>)) {
    let mut packet = Vec::new();
    encode(&mut packet);
    let len = i32::try_from(packet.len()).expect("packet fits in a VarInt length");
    codec::write_varint(out, len);
    out.extend_from_slice(&packet);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_split_across_reads() {
        let mut reader = FrameReader::default();
        let mut bytes = Vec::new();
        write_frame(&mut bytes, |buf| buf.extend_from_slice(&[1, 2, 3]));
        write_frame(&mut bytes, |buf| buf.extend_from_slice(&[0xaa; 200]));
        for byte in &bytes[..4] {
            reader.push(&[*byte]);
        }
        assert_eq!(reader.next_frame(), Ok(Some(vec![1, 2, 3])));
        assert_eq!(reader.next_frame(), Ok(None));
        reader.push(&bytes[4..10]);
        assert_eq!(reader.next_frame(), Ok(None));
        reader.push(&bytes[10..]);
        assert_eq!(reader.next_frame(), Ok(Some(vec![0xaa; 200])));
        assert_eq!(reader.next_frame(), Ok(None));
        assert!(reader.pending().is_empty());
    }

    #[test]
    fn lengths_are_limited_before_waiting_for_the_bytes() {
        let mut reader = FrameReader::default();
        reader.push(&[0xff, 0xff, 0xff, 0x01]);
        assert_eq!(reader.next_frame(), Err(FrameError::LengthTooLong));

        // 2^21 in a 4-byte VarInt is rejected for its width; 2^21 - 1 is accepted and waited for.
        let mut reader = FrameReader::default();
        reader.push(&[0xff, 0xff, 0x7f]);
        assert_eq!(reader.next_frame(), Ok(None));
    }
}
