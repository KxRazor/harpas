//! Splitting game batches into data channel messages, and joining them again
//!
//! Each message starts with the number of segments still to come; the batch is complete at 0.

use std::fmt;

/// Data bytes per segment, as go-nethernet splits batches.
pub const SEGMENT_SIZE: usize = 10_000;
/// Segments after the first, at most: one byte counts them.
const MAX_MORE_SEGMENTS: usize = u8::MAX as usize;
/// Largest batch accepted from a client.
pub const MAX_BATCH: usize = 4 * 1024 * 1024;

/// Why a client's message was refused. The connection is closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A message had no data after its segment count.
    Empty,
    /// The segment count did not go down by one.
    OutOfOrder {
        /// Count the previous segment promised.
        expected: u8,
        /// Count received.
        found: u8,
    },
    /// The batch grew past [`MAX_BATCH`].
    TooLarge,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Empty => f.write_str("empty segment"),
            Error::OutOfOrder { expected, found } => {
                write!(f, "segment count {found}, expected {expected}")
            }
            Error::TooLarge => write!(f, "batch larger than {MAX_BATCH} bytes"),
        }
    }
}

impl std::error::Error for Error {}

/// Splits a batch into messages; `None` if it needs more than 256 segments.
pub fn split(batch: &[u8]) -> Option<Vec<Vec<u8>>> {
    let count = batch.len().div_ceil(SEGMENT_SIZE).max(1);
    if count - 1 > MAX_MORE_SEGMENTS {
        return None;
    }
    let messages = batch
        .chunks(SEGMENT_SIZE)
        .enumerate()
        .map(|(i, chunk)| {
            let mut message = Vec::with_capacity(chunk.len() + 1);
            message.push((count - 1 - i) as u8);
            message.extend_from_slice(chunk);
            message
        })
        .collect();
    Some(if batch.is_empty() {
        vec![vec![0]]
    } else {
        messages
    })
}

/// Joins the messages of one data channel into batches.
#[derive(Default)]
pub struct Reassembly {
    remaining: Option<u8>,
    data: Vec<u8>,
}

impl Reassembly {
    /// Adds a message; returns the batch once its last segment arrives.
    pub fn push(&mut self, message: &[u8]) -> Result<Option<Vec<u8>>, Error> {
        let (&count, data) = message.split_first().ok_or(Error::Empty)?;
        if data.is_empty() {
            return Err(Error::Empty);
        }
        if let Some(remaining) = self.remaining
            && count != remaining - 1
        {
            return Err(Error::OutOfOrder {
                expected: remaining - 1,
                found: count,
            });
        }
        if self.data.len() + data.len() > MAX_BATCH {
            return Err(Error::TooLarge);
        }
        self.data.extend_from_slice(data);
        if count > 0 {
            self.remaining = Some(count);
            return Ok(None);
        }
        self.remaining = None;
        Ok(Some(std::mem::take(&mut self.data)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_round_trip() {
        for len in [1, SEGMENT_SIZE, SEGMENT_SIZE + 1, 3 * SEGMENT_SIZE + 7] {
            let batch: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let messages = split(&batch).unwrap();
            assert_eq!(messages.len(), len.div_ceil(SEGMENT_SIZE));
            assert_eq!(messages.last().unwrap()[0], 0);
            let mut reassembly = Reassembly::default();
            let (last, rest) = messages.split_last().unwrap();
            for message in rest {
                assert_eq!(reassembly.push(message), Ok(None));
            }
            assert_eq!(reassembly.push(last), Ok(Some(batch)));
        }
        assert!(split(&vec![0; 257 * SEGMENT_SIZE]).is_none());
    }

    #[test]
    fn refuses_broken_sequences() {
        let mut reassembly = Reassembly::default();
        assert_eq!(reassembly.push(&[]), Err(Error::Empty));
        assert_eq!(reassembly.push(&[0]), Err(Error::Empty));
        assert_eq!(reassembly.push(&[2, 1]), Ok(None));
        assert_eq!(
            reassembly.push(&[0, 1]),
            Err(Error::OutOfOrder {
                expected: 1,
                found: 0
            })
        );
        let mut big = Reassembly::default();
        // 256 segments of 20 kB pass the 4 MiB limit.
        let segment = vec![0u8; 20_001];
        let mut result = Ok(None);
        for count in (0..=255u8).rev() {
            let mut message = segment.clone();
            message[0] = count;
            result = big.push(&message);
            if result.is_err() {
                break;
            }
        }
        assert!(matches!(result, Err(Error::TooLarge)));
    }
}
