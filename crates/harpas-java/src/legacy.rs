//! The legacy server list ping of clients before 1.7, answered as vanilla does
//!
//! These clients send `0xFE` before any framing. Beta 1.8 to 1.3 send it alone; 1.4 and later follow it with
//! `0x01`, and 1.6 with a plugin message after that. The answer is a legacy Disconnect packet: `0xFF`, a
//! length in UTF-16 code units, and the text in UTF-16 big endian.

use harpas_protocol_java::v26_1;

use crate::status::ServerInfo;

/// First byte of a legacy ping. No framed packet starts with it in practice: as a VarInt length it would
/// announce a Handshake of at least 254 bytes.
pub(crate) const LEGACY_PING: u8 = 0xfe;

/// Protocol number vanilla answers legacy pings with, which no legacy client has, so they show the server as
/// incompatible while still listing it.
const LEGACY_PROTOCOL: u32 = 127;

/// The answer to a legacy ping whose first bytes are `bytes`, or none if it is not one.
pub(crate) fn answer(bytes: &[u8], info: &ServerInfo) -> Option<Vec<u8>> {
    let text = match bytes {
        [LEGACY_PING] => format!("{}§{}§{}", info.motd, info.online_players, info.max_players),
        [LEGACY_PING, 0x01, ..] => format!(
            "§1\0{LEGACY_PROTOCOL}\0{}\0{}\0{}\0{}",
            v26_1::MINECRAFT_VERSION,
            info.motd,
            info.online_players,
            info.max_players
        ),
        _ => return None,
    };
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut out = Vec::with_capacity(3 + units.len() * 2);
    out.push(0xff);
    // The motd is at most a few hundred characters; a longer one is cut, as a Java `short` would wrap.
    let len = u16::try_from(units.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    for unit in &units[..usize::from(len)] {
        out.extend_from_slice(&unit.to_be_bytes());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_be_bytes).collect()
    }

    #[test]
    fn beta_ping() {
        let info = ServerInfo::default();
        let text = "A Minecraft Server§0§20";
        let mut expected = vec![0xff, 0x00, text.encode_utf16().count() as u8];
        expected.extend(utf16(text));
        assert_eq!(answer(&[0xfe], &info), Some(expected));
    }

    #[test]
    fn ping_since_1_4() {
        let info = ServerInfo {
            online_players: 2,
            ..ServerInfo::default()
        };
        let text = "§1\u{0}127\u{0}26.1\u{0}A Minecraft Server\u{0}2\u{0}20";
        let mut expected = vec![0xff, 0x00, text.encode_utf16().count() as u8];
        expected.extend(utf16(text));
        assert_eq!(answer(&[0xfe, 0x01], &info), Some(expected.clone()));
        // 1.6 adds a plugin message; the answer is the same.
        assert_eq!(
            answer(&[0xfe, 0x01, 0xfa, 0x00, 0x0b], &info),
            Some(expected)
        );
        assert_eq!(answer(&[0x10, 0x00], &info), None);
    }
}
