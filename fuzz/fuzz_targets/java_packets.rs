//! The serverbound packets of every connection state. The first byte chooses the state; the rest is one
//! frame, starting with the packet ID.
#![no_main]

use harpas_protocol_java::v26_1::{configuration, handshaking, login, play, status};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((state, frame)) = data.split_first() else {
        return;
    };
    match state % 5 {
        0 => drop(handshaking::serverbound::Packet::decode(frame)),
        1 => drop(status::serverbound::Packet::decode(frame)),
        2 => drop(login::serverbound::Packet::decode(frame)),
        3 => drop(configuration::serverbound::Packet::decode(frame)),
        _ => drop(play::serverbound::Packet::decode(frame)),
    }
});
