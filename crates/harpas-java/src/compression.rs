//! Per-packet zlib compression, once Set Compression is sent
//!
//! A compressed frame holds the packet's uncompressed length as a VarInt, then the packet: zlib-compressed when
//! it is at least the threshold, as is with a length of 0 otherwise. zlib-rs at level 6, as measured in
//! `docs/protocol-codecs.md`.

use std::error::Error;
use std::fmt;

use flate2::{Compress, Decompress, FlushCompress, FlushDecompress, Status};
use harpas_protocol_java::codec;

/// Largest uncompressed packet a compressed frame may declare, as in vanilla: 8 MiB.
pub const MAX_UNCOMPRESSED_LEN: usize = 8 * 1024 * 1024;

/// Why a compressed frame from a client is invalid. Any of these closes the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompressionError {
    /// The frame ended before its uncompressed length.
    MissingLength,
    /// A compressed packet declared a length below the threshold; vanilla never compresses those.
    BelowThreshold {
        /// Declared uncompressed length.
        len: usize,
        /// The connection's threshold.
        threshold: usize,
    },
    /// A compressed packet declared more than [`MAX_UNCOMPRESSED_LEN`] bytes.
    TooLarge(usize),
    /// The zlib data was invalid, or did not inflate to the declared length.
    Invalid,
}

impl fmt::Display for CompressionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompressionError::MissingLength => f.write_str("compressed frame without its length"),
            CompressionError::BelowThreshold { len, threshold } => write!(
                f,
                "compressed packet of {len} bytes is below the threshold of {threshold}"
            ),
            CompressionError::TooLarge(len) => write!(
                f,
                "compressed packet declares {len} bytes, over the limit of {MAX_UNCOMPRESSED_LEN}"
            ),
            CompressionError::Invalid => f.write_str("invalid compressed packet"),
        }
    }
}

impl Error for CompressionError {}

/// The compression state of a connection.
pub(crate) struct Compression {
    threshold: usize,
    compressor: Compress,
    decompressor: Decompress,
}

impl fmt::Debug for Compression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Compression")
            .field("threshold", &self.threshold)
            .finish_non_exhaustive()
    }
}

impl Compression {
    pub(crate) fn new(threshold: usize) -> Self {
        Self {
            threshold,
            compressor: Compress::new(flate2::Compression::new(6), true),
            decompressor: Decompress::new(true),
        }
    }

    /// The body of a frame for `packet`: its uncompressed length and the packet, compressed when it is at least
    /// the threshold.
    pub(crate) fn compress(&mut self, packet: &[u8]) -> Vec<u8> {
        let mut body = Vec::with_capacity(packet.len() / 2 + 8);
        if packet.len() < self.threshold {
            codec::write_varint(&mut body, 0);
            body.extend_from_slice(packet);
            return body;
        }
        let len = i32::try_from(packet.len()).expect("packet fits in a VarInt length");
        codec::write_varint(&mut body, len);
        self.compressor.reset();
        let start = body.len();
        body.reserve(packet.len() / 2 + 64);
        loop {
            let consumed = self.compressor.total_in() as usize;
            let status = self
                .compressor
                .compress_vec(&packet[consumed..], &mut body, FlushCompress::Finish)
                .expect("zlib compression of a buffer cannot fail");
            if status == Status::StreamEnd {
                break;
            }
            body.reserve(body.capacity().max(64));
        }
        debug_assert!(body.len() > start);
        body
    }

    /// The packet in the body of a frame from the client.
    pub(crate) fn decompress(&mut self, body: &[u8]) -> Result<Vec<u8>, CompressionError> {
        let mut input = body;
        let len = codec::read_varint(&mut input).map_err(|_| CompressionError::MissingLength)?;
        let len = usize::try_from(len).map_err(|_| CompressionError::Invalid)?;
        if len == 0 {
            return Ok(input.to_vec());
        }
        if len < self.threshold {
            return Err(CompressionError::BelowThreshold {
                len,
                threshold: self.threshold,
            });
        }
        if len > MAX_UNCOMPRESSED_LEN {
            return Err(CompressionError::TooLarge(len));
        }
        // The output never grows past the declared length, so a small frame cannot inflate into more.
        let mut packet = Vec::with_capacity(len);
        self.decompressor.reset(true);
        let status = self
            .decompressor
            .decompress_vec(input, &mut packet, FlushDecompress::Finish)
            .map_err(|_| CompressionError::Invalid)?;
        let consumed = self.decompressor.total_in() as usize;
        if status != Status::StreamEnd || packet.len() != len || consumed != input.len() {
            return Err(CompressionError::Invalid);
        }
        Ok(packet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_packets_are_sent_as_they_are() {
        let mut compression = Compression::new(256);
        let body = compression.compress(&[1, 2, 3]);
        assert_eq!(body, [0, 1, 2, 3]);
        assert_eq!(compression.decompress(&body), Ok(vec![1, 2, 3]));
    }

    #[test]
    fn large_packets_round_trip_compressed() {
        let mut compression = Compression::new(256);
        let packet: Vec<u8> = (0..10_000).map(|i| (i % 7) as u8).collect();
        let body = compression.compress(&packet);
        assert!(body.len() < 200, "{} bytes", body.len());
        assert_eq!(body[..2], [0x90, 0x4e]);
        assert_eq!(compression.decompress(&body), Ok(packet.clone()));
        // The compressor is reused between packets.
        let again = compression.compress(&packet);
        assert_eq!(again, body);
        assert_eq!(compression.decompress(&again), Ok(packet));
    }

    #[test]
    fn hostile_frames_are_rejected() {
        let mut compression = Compression::new(256);
        // A compressed packet declaring 100 bytes, below the threshold.
        assert_eq!(
            compression.decompress(&[100, 0x78, 0x9c]),
            Err(CompressionError::BelowThreshold {
                len: 100,
                threshold: 256
            })
        );
        // 8 MiB + 1 declared.
        let mut body = Vec::new();
        codec::write_varint(&mut body, MAX_UNCOMPRESSED_LEN as i32 + 1);
        assert_eq!(
            compression.decompress(&body),
            Err(CompressionError::TooLarge(MAX_UNCOMPRESSED_LEN + 1))
        );
        // A bomb: 1 MiB of zeros declared as 300 bytes stops at 300 and is refused.
        let mut big = Compression::new(0);
        let bomb = big.compress(&vec![0; 1024 * 1024]);
        let mut lying = Vec::new();
        codec::write_varint(&mut lying, 300);
        let mut rest = &bomb[..];
        codec::read_varint(&mut rest).unwrap();
        lying.extend_from_slice(rest);
        assert_eq!(
            compression.decompress(&lying),
            Err(CompressionError::Invalid)
        );
        // Not zlib at all.
        let mut garbage = Vec::new();
        codec::write_varint(&mut garbage, 300);
        garbage.extend_from_slice(&[0xff; 20]);
        assert_eq!(
            compression.decompress(&garbage),
            Err(CompressionError::Invalid)
        );
    }
}
