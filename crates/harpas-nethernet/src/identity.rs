//! The server identity carried in SDP answers
//!
//! The answer's `a=identity` attribute holds a JWT the server signs with its own P-384 key, whose `cpk` claim is
//! that key, and a detached ES384 signature over the answer's DTLS fingerprints. Clients remember the key the first
//! time they join, so it is kept across restarts (`docs/nethernet.md`).

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use p384::ecdsa::signature::Signer;
use p384::ecdsa::{Signature, SigningKey};
use p384::pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey, LineEnding};

/// How long a server token is valid, as go-nethernet issues them.
const TOKEN_LIFETIME_SECS: u64 = 60;

/// The server's signing key.
pub struct Identity {
    key: SigningKey,
}

impl Identity {
    /// Generates a new key from the system's random number generator.
    pub fn generate() -> Identity {
        loop {
            let mut bytes = [0u8; 48];
            getrandom::fill(&mut bytes).expect("the system random number generator works");
            // Fewer than one in 2^190 values is out of range; draw again then.
            if let Ok(key) = SigningKey::from_slice(&bytes) {
                return Identity { key };
            }
        }
    }

    /// Reads a key saved with [`Identity::to_pem`].
    pub fn from_pem(pem: &str) -> Option<Identity> {
        SigningKey::from_pkcs8_pem(pem)
            .ok()
            .map(|key| Identity { key })
    }

    /// The key as PKCS#8 PEM, to keep it across restarts.
    pub fn to_pem(&self) -> String {
        self.key
            .to_pkcs8_pem(LineEnding::LF)
            .expect("a P-384 key encodes")
            .to_string()
    }

    fn sign(&self, header: &str, payload: &str) -> String {
        let signing_input = format!(
            "{}.{}",
            encode(header.as_bytes()),
            encode(payload.as_bytes())
        );
        let signature: Signature = self.key.sign(signing_input.as_bytes());
        format!("{signing_input}.{}", encode(&signature.to_bytes()))
    }

    /// A self-signed JWT whose `cpk` claim is the public key, as a JWK.
    pub fn token(&self, now: u64) -> String {
        let point = self.key.verifying_key().to_sec1_point(false);
        let (x, y) = (
            point.x().expect("uncompressed point"),
            point.y().expect("uncompressed point"),
        );
        let spki = self
            .key
            .verifying_key()
            .to_public_key_der()
            .expect("a P-384 key encodes");
        let header = format!(
            r#"{{"alg":"ES384","x5u":"{}"}}"#,
            STANDARD.encode(spki.as_bytes())
        );
        let claims = format!(
            r#"{{"cpk":{{"kty":"EC","crv":"P-384","x":"{}","y":"{}"}},"exp":{},"iat":{now}}}"#,
            encode(x),
            encode(y),
            now + TOKEN_LIFETIME_SECS,
        );
        self.sign(&header, &claims)
    }

    /// The value of the `a=identity` attribute for an answer with these DTLS fingerprints, given as
    /// `(algorithm, digest)` pairs.
    pub fn attribute(&self, fingerprints: &[(&str, &str)], now: u64) -> String {
        let entries: Vec<String> = fingerprints
            .iter()
            .map(|(algorithm, digest)| {
                format!(
                    r#"{{"algorithm":{},"digest":{}}}"#,
                    json_string(algorithm),
                    json_string(digest)
                )
            })
            .collect();
        let payload = format!(r#"{{"fingerprint":[{}]}}"#, entries.join(","));
        // A detached JWS: the payload is left out between the two dots.
        let signed = self.sign(r#"{"alg":"ES384"}"#, &payload);
        let mut parts = signed.splitn(3, '.');
        let (header, _, signature) = (
            parts.next().unwrap(),
            parts.next().unwrap(),
            parts.next().unwrap(),
        );
        let assertion = format!(
            r#"{{"fingerprints":"{header}..{signature}","token":"{}"}}"#,
            self.token(now)
        );
        let identity = format!(
            r#"{{"assertion":{},"idp":{{"domain":"self","protocol":"default"}}}}"#,
            json_string(&assertion)
        );
        STANDARD.encode(identity)
    }

    /// Adds the `a=identity` attribute to a session description, at session level.
    pub fn add_to_sdp(&self, sdp: &str, now: u64) -> String {
        let fingerprints: Vec<(&str, &str)> = sdp
            .lines()
            .filter_map(|line| line.strip_prefix("a=fingerprint:"))
            .filter_map(|value| value.trim().split_once(' '))
            .collect();
        let attribute = format!("a=identity:{}\r\n", self.attribute(&fingerprints, now));
        match sdp.find("\nm=") {
            Some(media) => format!("{}{attribute}{}", &sdp[..=media], &sdp[media + 1..]),
            None => format!("{sdp}{attribute}"),
        }
    }
}

fn encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Quotes a string for JSON. The strings here are base64, identifiers and JSON produced above.
fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use p384::ecdsa::VerifyingKey;
    use p384::ecdsa::signature::Verifier;

    fn verify(jws: &str, key: &VerifyingKey, payload: Option<&str>) {
        let parts: Vec<&str> = jws.split('.').collect();
        let body = payload
            .map(|p| encode(p.as_bytes()))
            .unwrap_or_else(|| parts[1].to_owned());
        let signature = Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
        key.verify(format!("{}.{body}", parts[0]).as_bytes(), &signature)
            .unwrap();
    }

    #[test]
    fn token_is_signed_by_its_own_key() {
        let identity = Identity::generate();
        let token = identity.token(1_000);
        verify(&token, identity.key.verifying_key(), None);
        let claims = String::from_utf8(
            URL_SAFE_NO_PAD
                .decode(token.split('.').nth(1).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert!(
            claims.contains(r#""crv":"P-384""#) && claims.contains(r#""exp":1060"#),
            "{claims}"
        );
    }

    #[test]
    fn identity_attribute_signs_the_fingerprints() {
        let identity = Identity::generate();
        let sdp = "v=0\r\na=group:BUNDLE 0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=fingerprint:sha-256 AB:CD\r\n";
        let with_identity = identity.add_to_sdp(sdp, 1_000);
        let (before, after) = with_identity.split_once("m=application").unwrap();
        let value = before
            .lines()
            .find_map(|line| line.strip_prefix("a=identity:"))
            .unwrap();
        assert!(after.contains("a=fingerprint:sha-256 AB:CD"));

        let json = String::from_utf8(STANDARD.decode(value).unwrap()).unwrap();
        assert!(json.contains(r#""idp":{"domain":"self","protocol":"default"}"#));
        let start = json.find(r#"fingerprints\":\""#).unwrap() + r#"fingerprints\":\""#.len();
        let detached = &json[start..start + json[start..].find('\\').unwrap()];
        let payload = r#"{"fingerprint":[{"algorithm":"sha-256","digest":"AB:CD"}]}"#;
        verify(detached, identity.key.verifying_key(), Some(payload));
    }

    #[test]
    fn keys_survive_a_round_trip_through_pem() {
        let identity = Identity::generate();
        let restored = Identity::from_pem(&identity.to_pem()).unwrap();
        assert_eq!(restored.key.verifying_key(), identity.key.verifying_key());
    }
}
