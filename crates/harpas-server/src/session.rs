//! Mojang's session server, which says whether an online-mode player joined through it
//!
//! Requests run on the connection's own thread, never on the logic thread
//! (`docs/java-connection.md#authentication`). TLS is rustls with the system's root certificates.

use std::sync::{Arc, Mutex};
use std::thread;
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

/// How long to wait before asking Minecraft Services for the chat keys again after a failure.
const CHAT_KEYS_RETRY: Duration = Duration::from_secs(60);

/// A client for the session server.
pub struct SessionServer {
    agent: ureq::Agent,
    /// Empty until Minecraft Services has answered.
    chat_authority: Arc<Mutex<ChatAuthority>>,
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
        let chat_authority = Arc::new(Mutex::new(ChatAuthority::default()));
        match load_chat_authority(&agent) {
            Ok(authority) => *chat_authority.lock().unwrap_or_else(|e| e.into_inner()) = authority,
            Err(error) => {
                log::warn!(
                    "Could not load Minecraft Services chat keys; chat is unsigned until they load: {error}"
                );
                // The service may only be down for now, so keep asking instead of waiting for a restart.
                let (agent, shared) = (agent.clone(), Arc::clone(&chat_authority));
                thread::Builder::new()
                    .name("harpas-chat-keys".to_owned())
                    .spawn(move || {
                        retry_chat_authority(&shared, CHAT_KEYS_RETRY, || {
                            load_chat_authority(&agent)
                        });
                        log::info!("Loaded Minecraft Services chat keys");
                    })
                    .map_err(|e| format!("cannot start the chat keys thread: {e}"))?;
            }
        }
        Ok(Self {
            agent,
            chat_authority,
        })
    }

    /// The trusted player-certificate keys fetched from Minecraft Services, empty while they could not be
    /// fetched.
    pub fn chat_authority(&self) -> ChatAuthority {
        self.chat_authority
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
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
                log::warn!("Session server unreachable: {e}");
                return AuthOutcome::Unavailable;
            }
        };
        match response.status().as_u16() {
            200 => {}
            // No content: the player did not join through the session server.
            204 => return AuthOutcome::NotVerified,
            status => {
                log::warn!("Session server answered with status {status}");
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
                log::warn!("Session server answered with an invalid profile");
                AuthOutcome::Unavailable
            }
        }
    }
}

/// Asks for the chat keys every `interval` until `load` succeeds, then stores them in `shared`.
fn retry_chat_authority(
    shared: &Mutex<ChatAuthority>,
    interval: Duration,
    load: impl Fn() -> Result<ChatAuthority, String>,
) {
    loop {
        thread::sleep(interval);
        if let Ok(authority) = load() {
            *shared.lock().unwrap_or_else(|e| e.into_inner()) = authority;
            return;
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
    fn chat_keys_are_asked_for_again_until_they_load() {
        let der = harpas_java::ServerKey::generate().public_der().to_vec();
        let shared = Mutex::new(ChatAuthority::default());
        let attempts = std::cell::Cell::new(0);
        retry_chat_authority(&shared, Duration::from_millis(1), || {
            attempts.set(attempts.get() + 1);
            if attempts.get() < 3 {
                Err("unavailable".to_owned())
            } else {
                ChatAuthority::from_der(std::slice::from_ref(&der)).map_err(str::to_owned)
            }
        });
        assert_eq!(attempts.get(), 3);
        assert!(!shared.lock().unwrap().is_empty());
    }

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
