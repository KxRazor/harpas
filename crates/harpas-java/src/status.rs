//! What the server list shows: the Status Response and the legacy ping answer

use std::fmt::Write;

use harpas_protocol_java::v26_1;

/// The server's entry in the server list, and the settings connections need before Login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    /// Message of the day, from `motd` in `server.properties`. Legacy `§` formatting codes are kept.
    pub motd: String,
    /// Player limit shown in the list.
    pub max_players: u32,
    /// Players online.
    pub online_players: u32,
    /// The server icon as a `data:image/png;base64,` URI, from `server-icon.png`.
    pub favicon: Option<String>,
    /// Whether the server requires signed chat, from `enforce-secure-profile`.
    pub enforces_secure_chat: bool,
    /// Whether connections transferred from another server are accepted, from `accepts-transfers`.
    pub accepts_transfers: bool,
}

impl Default for ServerInfo {
    /// Vanilla's defaults.
    fn default() -> Self {
        Self {
            motd: "A Minecraft Server".to_owned(),
            max_players: 20,
            online_players: 0,
            favicon: None,
            enforces_secure_chat: true,
            accepts_transfers: false,
        }
    }
}

impl ServerInfo {
    /// The JSON of the Status Response.
    pub fn status_json(&self) -> String {
        let mut json = format!(
            r#"{{"version":{{"name":{},"protocol":{}}},"players":{{"max":{},"online":{}}},"description":{{"text":{}}}"#,
            json_string(v26_1::MINECRAFT_VERSION),
            v26_1::PROTOCOL,
            self.max_players,
            self.online_players,
            json_string(&self.motd),
        );
        if let Some(favicon) = &self.favicon {
            let _ = write!(json, r#","favicon":{}"#, json_string(favicon));
        }
        let _ = write!(
            json,
            r#","enforcesSecureChat":{}}}"#,
            self.enforces_secure_chat
        );
        json
    }
}

/// `text` as a JSON string, with its quotes.
pub(crate) fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_json_has_vanilla_fields() {
        let info = ServerInfo {
            motd: "§aHello \"you\"\n".to_owned(),
            favicon: Some("data:image/png;base64,AA==".to_owned()),
            online_players: 3,
            ..ServerInfo::default()
        };
        assert_eq!(
            info.status_json(),
            r#"{"version":{"name":"26.1","protocol":775},"players":{"max":20,"online":3},"description":{"text":"§aHello \"you\"\n"},"favicon":"data:image/png;base64,AA==","enforcesSecureChat":true}"#
        );
    }

    #[test]
    fn control_characters_are_escaped() {
        assert_eq!(json_string("a\u{1}b\\"), r#""a\u0001b\\""#);
    }
}
