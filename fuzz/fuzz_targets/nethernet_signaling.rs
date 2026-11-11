//! The HTTP requests of the NetherNet signaling endpoint.
#![no_main]

use harpas_nethernet::signaling;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Err(error) = signaling::parse(data) {
        signaling::error_response(&error);
    }
});
