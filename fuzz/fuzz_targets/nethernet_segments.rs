//! NetherNet's segment framing. The input is a sequence of messages, each with a one-byte length, given to
//! one reassembly; and a batch that splits into segments must reassemble into itself. An empty batch is left
//! out: `split` gives it one segment without data, which `Reassembly` refuses.
#![no_main]

use harpas_nethernet::segment::{Reassembly, split};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut reassembly = Reassembly::default();
    let mut rest = data;
    while let Some((length, tail)) = rest.split_first() {
        let (message, tail) = tail.split_at(usize::from(*length).min(tail.len()));
        if reassembly.push(message).is_err() {
            break;
        }
        rest = tail;
    }

    if let Some(segments) = split(data).filter(|_| !data.is_empty()) {
        let mut reassembly = Reassembly::default();
        let mut batch = None;
        for segment in &segments {
            assert!(batch.is_none(), "a batch ends at its last segment");
            batch = reassembly.push(segment).expect("split segments reassemble");
        }
        assert_eq!(batch.as_deref(), Some(data));
    }
});
