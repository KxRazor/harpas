//! A whole connection in offline mode, from its first byte: the legacy ping, framing, the connection states
//! and compression. The first byte sets how the input is cut into reads.
#![no_main]

use std::sync::{Arc, LazyLock};

use harpas_java::{Connection, ServerInfo, ServerKey};
use libfuzzer_sys::fuzz_target;

static KEY: LazyLock<Arc<ServerKey>> = LazyLock::new(|| Arc::new(ServerKey::generate()));

fuzz_target!(|data: &[u8]| {
    let Some((cut, bytes)) = data.split_first() else {
        return;
    };
    let info = ServerInfo {
        online_mode: false,
        ..ServerInfo::default()
    };
    let mut connection = Connection::new(Arc::clone(&KEY));
    for read in bytes.chunks(usize::from(*cut) + 1) {
        if connection.receive(read, &info).is_err() || connection.is_closed() {
            break;
        }
        connection.take_output();
        connection.take_play_events();
    }
});
