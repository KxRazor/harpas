//! Mojang's session server, which says whether an online-mode player joined through it
//!
//! Requests run on the connection's own thread, never on the logic thread
//! (`docs/java-connection.md#authentication`). TLS is rustls with the system's root certificates.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use harpas_java::{AuthOutcome, AuthRequest, ChatAuthority, Player};
use harpas_protocol_java::v26_1::login::ProfileProperty;
use serde_json::Value;
use ureq::tls::{Certificate, RootCerts, TlsConfig, TlsProvider};

const HAS_JOINED: &str = "https://sessionserver.mojang.com/session/minecraft/hasJoined";
const PUBLIC_KEYS: &str = "https://api.minecraftservices.com/publickeys";
/// Longest wait for the session server, as a whole request.
const TIMEOUT: Duration = Duration::from_secs(10);

/// A client for the session server.
pub struct SessionServer {
    agent: ureq::Agent,
    chat_authority: ChatAuthority,
}

impl SessionServer {
    /// A client that trusts the system's root certificates.
    pub fn new() -> Result<Self, String> {
        let native = rustls_native_certs::load_native_certs();
        if native.certs.is_empty() {
            return Err(format!(
                "no root certificates found on this system to reach the session server: {:?}",
                native.errors
            ));
        }
        let roots: Vec<Certificate<'static>> = native
            .certs
            .iter()
            .map(|cert| Certificate::from_der(cert.as_ref()).to_owned())
            .collect();
        let tls = TlsConfig::builder()
            .provider(TlsProvider::Rustls)
            .root_certs(RootCerts::new_with_certs(&roots))
            .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .build();
        let agent = ureq::Agent::config_builder()
            .tls_config(tls)
            .timeout_global(Some(TIMEOUT))
            .http_status_as_error(false)
            .build()
            .new_agent();
        let chat_authority = match load_chat_authority(&agent) {
            Ok(authority) => authority,
            Err(error) => {
                eprintln!(
                    "could not load Minecraft Services chat keys; signed chat cannot be verified: {error}"
                );
                ChatAuthority::default()
            }
        };
        Ok(Self {
            agent,
            chat_authority,
        })
    }

    /// The trusted player-certificate keys fetched from Minecraft Services.
    pub fn chat_authority(&self) -> ChatAuthority {
        self.chat_authority.clone()
    }

    /// Asks whether `request.username` joined with `request.server_hash`.
    pub fn has_joined(&self, request: &AuthRequest) -> AuthOutcome {
        let response = self
            .agent
            .get(HAS_JOINED)
            .query("username", &request.username)
            .query("serverId", &request.server_hash)
            .call();
        let mut response = match response {
            Ok(response) => response,
            Err(e) => {
                eprintln!("session server unreachable: {e}");
                return AuthOutcome::Unavailable;
            }
        };
        match response.status().as_u16() {
            200 => {}
            // No content: the player did not join through the session server.
            204 => return AuthOutcome::NotVerified,
            status => {
                eprintln!("session server answered with status {status}");
                return AuthOutcome::Unavailable;
            }
        }
        match response
            .body_mut()
            .read_to_string()
            .ok()
            .and_then(|body| parse_profile(&body))
        {
            Some(player) => AuthOutcome::Verified(player),
            None => {
                eprintln!("session server answered with an invalid profile");
                AuthOutcome::Unavailable
            }
        }
    }
}

fn load_chat_authority(agent: &ureq::Agent) -> Result<ChatAuthority, String> {
    let mut response = agent.get(PUBLIC_KEYS).call().map_err(|e| e.to_string())?;
    if response.status().as_u16() != 200 {
        return Err(format!("Minecraft Services returned {}", response.status()));
    }
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    parse_chat_authority(&body)
}

fn parse_chat_authority(body: &str) -> Result<ChatAuthority, String> {
    let json: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let entries = json["playerCertificateKeys"]
        .as_array()
        .ok_or("missing player certificate keys")?;
    let keys: Vec<Vec<u8>> = entries
        .iter()
        .map(|entry| {
            let key = entry["publicKey"].as_str().ok_or("missing public key")?;
            STANDARD
                .decode(key)
                .map_err(|_| "invalid base64 public key")
        })
        .collect::<Result<_, _>>()
        .map_err(str::to_owned)?;
    ChatAuthority::from_der(&keys).map_err(str::to_owned)
}

/// Reads a profile: `{"id": "<32 hex digits>", "name": "...", "properties": [{"name", "value", "signature"}]}`.
fn parse_profile(json: &str) -> Option<Player> {
    let profile: Value = serde_json::from_str(json).ok()?;
    let id = profile["id"].as_str()?;
    if id.len() != 32 {
        return None;
    }
    let uuid = u128::from_str_radix(id, 16).ok()?;
    let name = profile["name"].as_str()?.to_owned();
    let properties = match profile.get("properties") {
        Some(list) => list
            .as_array()?
            .iter()
            .map(|p| {
                Some(ProfileProperty {
                    name: p["name"].as_str()?.to_owned(),
                    value: p["value"].as_str()?.to_owned(),
                    signature: p
                        .get("signature")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
            })
            .collect::<Option<Vec<_>>>()?,
        None => Vec::new(),
    };
    Some(Player {
        name,
        uuid,
        properties,
    })
}

/// A UUID with dashes, as vanilla logs it.
pub fn format_uuid(uuid: u128) -> String {
    let hex = format!("{uuid:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_session_server_profiles() {
        let json = r#"{"id":"069a79f444e94726a5befca90e38aaf5","name":"Notch",
            "properties":[{"name":"textures","value":"e30=","signature":"c2ln"}]}"#;
        let player = parse_profile(json).unwrap();
        assert_eq!(player.name, "Notch");
        assert_eq!(
            format_uuid(player.uuid),
            "069a79f4-44e9-4726-a5be-fca90e38aaf5"
        );
        assert_eq!(player.properties[0].signature.as_deref(), Some("c2ln"));
        assert!(parse_profile(r#"{"id":"short","name":"x"}"#).is_none());
        assert!(parse_profile("not json").is_none());
    }

    #[test]
    fn rejects_missing_or_invalid_chat_authority_responses() {
        assert!(parse_chat_authority("not json").is_err());
        assert!(parse_chat_authority(r#"{"profilePropertyKeys":[]}"#).is_err());
        assert!(parse_chat_authority(r#"{"playerCertificateKeys":[]}"#).is_err());
        assert!(parse_chat_authority(r#"{"playerCertificateKeys":[{}]}"#).is_err());
        assert!(
            parse_chat_authority(r#"{"playerCertificateKeys":[{"publicKey":"not-base64"}]}"#)
                .is_err()
        );
        assert!(
            parse_chat_authority(r#"{"playerCertificateKeys":[{"publicKey":"AQID"}]}"#).is_err()
        );
    }
}
