//! Verification of Java chat sessions, signatures and per-recipient acknowledgements.
//!
//! The protocol's signing layout is documented by the community packet reference for 26.1:
//! <https://github.com/xKumorio/Minecraft-Packet-ID-Reference/blob/main/versions/26.1.md>.

use std::collections::{HashSet, VecDeque};

use harpas_protocol_java::v26_1::play::{self, serverbound};
use rsa::pkcs8::DecodePublicKey;
use rsa::traits::PublicKeyParts;
use rsa::{Pkcs1v15Sign, RsaPublicKey};
use sha1::Sha1;
use sha2::{Digest, Sha256};

const WINDOW: usize = 20;
const MAX_PENDING: usize = 4096;

/// The certificate authorities obtained from Minecraft Services over HTTPS.
///
/// An empty set, the default, means the keys could not be fetched: no player key can be verified, and chat
/// sessions are ignored, as vanilla ignores them without the service's key. Tests can construct a separate
/// authority from generated keys.
#[derive(Debug, Clone, Default)]
pub struct ChatAuthority {
    keys: Vec<RsaPublicKey>,
}

impl ChatAuthority {
    /// Parses the service's X.509 SubjectPublicKeyInfo keys. An invalid or empty list is rejected.
    pub fn from_der(keys: &[Vec<u8>]) -> Result<Self, &'static str> {
        let keys: Vec<_> = keys
            .iter()
            .map(|key| RsaPublicKey::from_public_key_der(key).map_err(|_| "invalid authority key"))
            .collect::<Result<_, _>>()?;
        if keys.is_empty() {
            return Err("no chat authority keys");
        }
        Ok(Self { keys })
    }

    /// Whether the service's keys are missing.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    fn verify(&self, player: u128, data: &play::PublicKeyData, now: i64) -> Option<RsaPublicKey> {
        if data.expires_at <= now {
            return None;
        }
        let key = RsaPublicKey::from_public_key_der(&data.key).ok()?;
        if key.size() != 256 {
            return None;
        }
        let mut payload = Vec::with_capacity(24 + data.key.len());
        payload.extend_from_slice(&player.to_be_bytes());
        payload.extend_from_slice(&data.expires_at.to_be_bytes());
        payload.extend_from_slice(&data.key);
        let hash = Sha1::digest(&payload);
        self.keys
            .iter()
            .any(|root| {
                root.verify(Pkcs1v15Sign::new::<Sha1>(), &hash, &data.key_signature)
                    .is_ok()
            })
            .then_some(key)
    }
}

/// A message accepted from a connection; its signature, when present, has been verified.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedMessage {
    /// Index in the sender's session.
    pub index: i32,
    /// Original signature, absent for unsigned chat.
    pub signature: Option<[u8; 256]>,
    /// Original, unmodified message.
    pub message: String,
    /// Timestamp in milliseconds.
    pub timestamp: i64,
    /// Salt covered by the signature.
    pub salt: i64,
    /// Signatures of the messages the sender acknowledged.
    pub last_seen: Vec<[u8; 256]>,
}

/// A session or message to deliver in order to the game thread.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatEvent {
    /// A verified player session, to publish before its messages.
    Session(play::ChatSession),
    /// An accepted message.
    Message(Box<VerifiedMessage>),
}

#[derive(Debug)]
struct Session {
    id: u128,
    key: RsaPublicKey,
    expires_at: i64,
    index: i32,
    broken: bool,
}

/// Some invalid chat input closes the connection; a failed signing chain instead refuses messages
/// until a new session is received.
#[derive(Debug)]
pub(crate) enum MessageError {
    Fatal(&'static str),
    Refused(&'static str),
}

#[derive(Debug, Clone)]
struct Seen {
    signature: [u8; 256],
    pending: bool,
}

/// Connection-local tracking. The first 20 slots are the client's last-seen window; further entries
/// are signed messages sent since its last offset update.
#[derive(Debug)]
pub(crate) struct ChatState {
    authority: ChatAuthority,
    session: Option<Session>,
    sessions: HashSet<u128>,
    seen: VecDeque<Option<Seen>>,
    last_timestamp: Option<i64>,
    global_index: i32,
    enabled: bool,
    events: Vec<ChatEvent>,
}

impl Default for ChatState {
    fn default() -> Self {
        Self {
            authority: ChatAuthority::default(),
            session: None,
            sessions: HashSet::new(),
            seen: std::iter::repeat_with(|| None).take(WINDOW).collect(),
            last_timestamp: None,
            global_index: 0,
            enabled: true,
            events: Vec::new(),
        }
    }
}

impl ChatState {
    pub(crate) fn authority(&mut self, authority: ChatAuthority) {
        self.authority = authority;
    }

    pub(crate) fn enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub(crate) fn take_events(&mut self) -> Vec<ChatEvent> {
        std::mem::take(&mut self.events)
    }

    pub(crate) fn session(
        &mut self,
        player: u128,
        packet: serverbound::PlayerSession,
        now: i64,
    ) -> Result<(), &'static str> {
        // Without the service's keys the session cannot be checked. Vanilla ignores it and leaves the player
        // without a session, so the player chats unsigned where the server allows it.
        if self.authority.is_empty() {
            return Ok(());
        }
        if self.sessions.len() >= 128 || self.sessions.contains(&packet.session_id) {
            return Err("Invalid chat session");
        }
        let key = self
            .authority
            .verify(player, &packet.public_key, now)
            .ok_or("Invalid or expired chat public key")?;
        self.sessions.insert(packet.session_id);
        self.session = Some(Session {
            id: packet.session_id,
            key,
            expires_at: packet.public_key.expires_at,
            index: 0,
            broken: false,
        });
        self.events.push(ChatEvent::Session(play::ChatSession {
            session_id: packet.session_id,
            public_key: packet.public_key,
        }));
        Ok(())
    }

    pub(crate) fn offset(&mut self, offset: i32) -> Result<(), &'static str> {
        let offset = usize::try_from(offset).map_err(|_| "Invalid chat acknowledgement")?;
        if offset > self.seen.len() - WINDOW {
            return Err("Invalid chat acknowledgement");
        }
        self.seen.drain(..offset);
        Ok(())
    }

    /// Commands are not executed yet, but their acknowledgement and signature indices still affect
    /// the next chat message. Their unverified arguments are discarded rather than used as input.
    pub(crate) fn ignored_command(
        &mut self,
        packet: serverbound::SignedChatCommand,
    ) -> Result<(), &'static str> {
        self.check_timestamp(packet.timestamp)?;
        self.acknowledge(packet.offset, packet.acknowledged, packet.checksum)?;
        if !packet.argument_signatures.is_empty() {
            let session = self.session.as_mut().ok_or("Missing chat session")?;
            session.index = session
                .index
                .checked_add(packet.argument_signatures.len() as i32)
                .ok_or("Chat session exhausted")?;
        }
        self.last_timestamp = Some(packet.timestamp);
        Ok(())
    }

    fn check_timestamp(&self, timestamp: i64) -> Result<(), &'static str> {
        if self.last_timestamp.is_some_and(|last| timestamp < last) {
            return Err("Chat message is out of order");
        }
        Ok(())
    }

    fn acknowledge(
        &mut self,
        offset: i32,
        bits: [u8; 3],
        checksum: u8,
    ) -> Result<Vec<[u8; 256]>, &'static str> {
        if bits[2] & 0xf0 != 0 {
            return Err("Invalid chat acknowledgement");
        }
        self.offset(offset)?;
        let mut last_seen = Vec::new();
        for (index, slot) in self.seen.iter_mut().take(WINDOW).enumerate() {
            if bits[index / 8] & (1 << (index % 8)) != 0 {
                let entry = slot.as_mut().ok_or("Invalid chat acknowledgement")?;
                entry.pending = false;
                last_seen.push(entry.signature);
            } else if slot.as_ref().is_some_and(|entry| !entry.pending) {
                return Err("Invalid chat acknowledgement");
            } else {
                *slot = None;
            }
        }
        if checksum != 0 && checksum != chat_checksum(&last_seen) {
            return Err("Invalid chat checksum");
        }
        Ok(last_seen)
    }

    pub(crate) fn message(
        &mut self,
        player: u128,
        mut packet: serverbound::ChatMessage,
        online: bool,
        secure_required: bool,
        now: i64,
    ) -> Result<(), MessageError> {
        let last_seen = self
            .acknowledge(packet.offset, packet.acknowledged, packet.checksum)
            .map_err(MessageError::Fatal)?;
        if packet
            .message
            .chars()
            .any(|c| c == '\u{a7}' || c.is_control())
        {
            return Err(MessageError::Fatal("Invalid chat message"));
        }
        if !self.enabled {
            return Err(MessageError::Refused(
                "Chat is disabled in your client settings",
            ));
        }
        if self.check_timestamp(packet.timestamp).is_err() {
            if let Some(session) = &mut self.session {
                session.broken = true;
            }
            return Err(MessageError::Refused("Chat message is out of order"));
        }
        if !online {
            // Offline UUIDs are not authenticated identities. Never relay their signatures as trusted.
            packet.signature = None;
        }
        let index = match &mut self.session {
            Some(session) if online => {
                let signature = packet
                    .signature
                    .as_ref()
                    .ok_or(MessageError::Refused("Missing chat signature"))?;
                if session.expires_at <= now {
                    return Err(MessageError::Refused("Expired chat public key"));
                }
                if session.broken {
                    return Err(MessageError::Refused("Chat signing chain is broken"));
                }
                let payload =
                    signed_payload(player, session.id, session.index, &packet, &last_seen);
                let hash = Sha256::digest(&payload);
                if session
                    .key
                    .verify(Pkcs1v15Sign::new::<Sha256>(), &hash, signature)
                    .is_err()
                {
                    session.broken = true;
                    return Err(MessageError::Refused("Invalid chat signature"));
                }
                let index = session.index;
                session.index = session
                    .index
                    .checked_add(1)
                    .ok_or(MessageError::Refused("Chat session exhausted"))?;
                index
            }
            _ if secure_required || packet.signature.is_some() => {
                return Err(MessageError::Refused("Missing chat session"));
            }
            _ => 0,
        };
        self.last_timestamp = Some(packet.timestamp);
        self.events
            .push(ChatEvent::Message(Box::new(VerifiedMessage {
                index,
                signature: packet.signature,
                message: packet.message,
                timestamp: packet.timestamp,
                salt: packet.salt,
                last_seen,
            })));
        Ok(())
    }

    /// Records a player-chat packet only if this client is configured to receive player chat.
    pub(crate) fn outgoing(
        &mut self,
        signature: Option<[u8; 256]>,
    ) -> Result<Option<i32>, &'static str> {
        if !self.enabled {
            return Ok(None);
        }
        if let Some(signature) = signature {
            if self.seen.len() - WINDOW >= MAX_PENDING {
                return Err("Too many pending chat messages");
            }
            self.seen.push_back(Some(Seen {
                signature,
                pending: true,
            }));
        }
        let index = self.global_index;
        self.global_index = self
            .global_index
            .checked_add(1)
            .ok_or("Chat index exhausted")?;
        Ok(Some(index))
    }
}

fn signed_payload(
    player: u128,
    session: u128,
    index: i32,
    packet: &serverbound::ChatMessage,
    seen: &[[u8; 256]],
) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&1i32.to_be_bytes());
    data.extend_from_slice(&player.to_be_bytes());
    data.extend_from_slice(&session.to_be_bytes());
    data.extend_from_slice(&index.to_be_bytes());
    data.extend_from_slice(&packet.salt.to_be_bytes());
    data.extend_from_slice(&(packet.timestamp / 1000).to_be_bytes());
    data.extend_from_slice(&(packet.message.len() as i32).to_be_bytes());
    data.extend_from_slice(packet.message.as_bytes());
    data.extend_from_slice(&(seen.len() as i32).to_be_bytes());
    for signature in seen {
        data.extend_from_slice(signature);
    }
    data
}

fn chat_checksum(seen: &[[u8; 256]]) -> u8 {
    let sum = seen.iter().fold(1i32, |sum, signature| {
        let hash = signature.iter().fold(1i32, |hash, byte| {
            hash.wrapping_mul(31).wrapping_add(i32::from(*byte as i8))
        });
        sum.wrapping_mul(31).wrapping_add(hash)
    }) as u8;
    if sum == 0 { 1 } else { sum }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use super::*;
    use rsa::RsaPrivateKey;
    use rsa::pkcs8::EncodePublicKey;
    use rsa::rand_core::UnwrapErr;

    const PLAYER: u128 = 0x0123456789abcdef0123456789abcdef;
    const SESSION: u128 = 0xfedcba9876543210fedcba9876543210;
    const NOW: i64 = 1_800_000_000_000;

    struct Keys {
        authority: RsaPrivateKey,
        player: RsaPrivateKey,
    }

    fn keys() -> &'static Keys {
        static KEYS: OnceLock<Keys> = OnceLock::new();
        KEYS.get_or_init(|| {
            let mut rng = UnwrapErr(getrandom::SysRng);
            Keys {
                authority: RsaPrivateKey::new(&mut rng, 2048).unwrap(),
                player: RsaPrivateKey::new(&mut rng, 2048).unwrap(),
            }
        })
    }

    fn authority() -> ChatAuthority {
        let der = keys()
            .authority
            .to_public_key()
            .to_public_key_der()
            .unwrap();
        ChatAuthority::from_der(&[der.as_bytes().to_vec()]).unwrap()
    }

    fn session(expires_at: i64) -> serverbound::PlayerSession {
        let key = keys()
            .player
            .to_public_key()
            .to_public_key_der()
            .unwrap()
            .as_bytes()
            .to_vec();
        let mut certificate = PLAYER.to_be_bytes().to_vec();
        certificate.extend(expires_at.to_be_bytes());
        certificate.extend(&key);
        let signature = keys()
            .authority
            .sign(Pkcs1v15Sign::new::<Sha1>(), &Sha1::digest(certificate))
            .unwrap();
        serverbound::PlayerSession {
            session_id: SESSION,
            public_key: play::PublicKeyData {
                expires_at,
                key,
                key_signature: signature,
            },
        }
    }

    fn packet(message: &str) -> serverbound::ChatMessage {
        serverbound::ChatMessage {
            message: message.into(),
            timestamp: NOW,
            salt: 0x102030405060708,
            signature: None,
            offset: 0,
            acknowledged: [0; 3],
            checksum: 1,
        }
    }

    // This fixture builds the signable bytes independently of the server's payload function.
    fn sign(
        packet: serverbound::ChatMessage,
        index: i32,
        previous: &[[u8; 256]],
    ) -> serverbound::ChatMessage {
        sign_in(packet, index, previous, SESSION)
    }

    fn sign_in(
        mut packet: serverbound::ChatMessage,
        index: i32,
        previous: &[[u8; 256]],
        session: u128,
    ) -> serverbound::ChatMessage {
        let mut bytes = vec![0, 0, 0, 1];
        bytes.extend(PLAYER.to_be_bytes());
        bytes.extend(session.to_be_bytes());
        bytes.extend(index.to_be_bytes());
        bytes.extend(packet.salt.to_be_bytes());
        bytes.extend((packet.timestamp / 1000).to_be_bytes());
        bytes.extend((packet.message.len() as i32).to_be_bytes());
        bytes.extend(packet.message.as_bytes());
        bytes.extend((previous.len() as i32).to_be_bytes());
        for item in previous {
            bytes.extend(item);
        }
        packet.signature = Some(
            keys()
                .player
                .sign(Pkcs1v15Sign::new::<Sha256>(), &Sha256::digest(bytes))
                .unwrap()
                .try_into()
                .unwrap(),
        );
        packet
    }

    fn connected() -> ChatState {
        let mut state = ChatState::default();
        state.authority(authority());
        state.session(PLAYER, session(NOW + 60_000), NOW).unwrap();
        assert!(matches!(&state.take_events()[..], [ChatEvent::Session(_)]));
        state
    }

    #[test]
    fn certificates_bind_player_key_expiry_and_authority() {
        let authority = authority();
        let good = session(NOW + 60_000);
        assert!(authority.verify(PLAYER, &good.public_key, NOW).is_some());
        assert!(
            authority
                .verify(PLAYER + 1, &good.public_key, NOW)
                .is_none()
        );
        assert!(
            authority
                .verify(PLAYER, &good.public_key, NOW + 60_000)
                .is_none()
        );
        let mut changed = good.public_key.clone();
        changed.expires_at += 1;
        assert!(authority.verify(PLAYER, &changed, NOW).is_none());
        assert!(
            ChatAuthority::default()
                .verify(PLAYER, &good.public_key, NOW)
                .is_none()
        );
        assert!(ChatAuthority::from_der(&[]).is_err());
        assert!(ChatAuthority::from_der(&[vec![1, 2, 3]]).is_err());
    }

    #[test]
    fn a_session_is_ignored_while_the_service_keys_are_missing() {
        let mut state = ChatState::default();
        state.session(PLAYER, session(NOW + 60_000), NOW).unwrap();
        assert!(state.take_events().is_empty());
        // With the keys, the same session is verified and published.
        state.authority(authority());
        state.session(PLAYER, session(NOW + 60_000), NOW).unwrap();
        assert!(matches!(&state.take_events()[..], [ChatEvent::Session(_)]));
    }

    #[test]
    fn verified_signatures_are_bound_to_session_order_and_message() {
        let mut state = connected();
        let first = sign(packet("hello ñ"), 0, &[]);
        state
            .message(PLAYER, first.clone(), true, true, NOW)
            .unwrap();
        assert!(
            matches!(&state.take_events()[..], [ChatEvent::Message(message)] if message.index == 0 && message.signature.is_some())
        );
        // A replay uses the previous index and therefore fails even with a valid signature.
        assert!(state.message(PLAYER, first, true, true, NOW).is_err());
        let mut fresh = connected();
        let mut modified = sign(packet("message"), 0, &[]);
        modified.message.push('!');
        assert!(matches!(
            fresh.message(PLAYER, modified, true, true, NOW),
            Err(MessageError::Refused("Invalid chat signature"))
        ));
        assert!(matches!(
            fresh.message(PLAYER, sign(packet("valid"), 0, &[]), true, true, NOW),
            Err(MessageError::Refused("Chat signing chain is broken"))
        ));
        let mut replacement = session(NOW + 60_000);
        replacement.session_id = SESSION + 1;
        fresh.session(PLAYER, replacement, NOW).unwrap();
        fresh
            .message(
                PLAYER,
                sign_in(packet("new session"), 0, &[], SESSION + 1),
                true,
                true,
                NOW,
            )
            .unwrap();
        assert!(
            state
                .message(PLAYER, packet("unsigned"), true, false, NOW)
                .is_err()
        );
        assert!(state.session(PLAYER, session(NOW + 60_000), NOW).is_err());
    }

    #[test]
    fn signed_messages_include_acknowledged_messages_and_reject_omissions() {
        let mut state = connected();
        let previous = [0xabu8; 256];
        assert_eq!(state.outgoing(Some(previous)).unwrap(), Some(0));
        let mut second = packet("answer");
        second.offset = 1;
        second.acknowledged = [0, 0, 8]; // slot 19, the newest message
        second.checksum = chat_checksum(&[previous]);
        let second = sign(second, 0, &[previous]);
        state.message(PLAYER, second, true, true, NOW).unwrap();
        assert!(
            matches!(&state.take_events()[..], [ChatEvent::Message(message)] if message.last_seen == vec![previous])
        );
        let mut omission = packet("omit");
        omission.checksum = 0;
        assert!(
            state
                .message(PLAYER, sign(omission, 1, &[]), true, true, NOW)
                .is_err()
        );
    }

    #[test]
    fn acknowledgements_check_offsets_bits_checksums_and_pending_limit() {
        let mut state = ChatState::default();
        assert!(state.offset(-1).is_err());
        assert!(state.offset(1).is_err());
        assert!(state.acknowledge(0, [0, 0, 0x10], 0).is_err());
        assert!(state.acknowledge(0, [1, 0, 0], 0).is_err());
        state.outgoing(Some([3; 256])).unwrap();
        assert!(state.acknowledge(1, [0, 0, 8], 123).is_err());
        let mut pending = ChatState::default();
        for _ in 0..MAX_PENDING {
            pending.outgoing(Some([7; 256])).unwrap();
        }
        assert!(pending.outgoing(Some([7; 256])).is_err());
        pending.offset(21).unwrap();
        assert!(pending.outgoing(Some([7; 256])).is_ok());
    }

    #[test]
    fn acknowledgement_window_rolls_over_and_accepts_periodic_offsets() {
        let mut state = ChatState::default();
        for value in 0..25u8 {
            state.outgoing(Some([value; 256])).unwrap();
        }
        state.offset(10).unwrap();
        let seen = state.acknowledge(15, [0xff, 0xff, 0x0f], 0).unwrap();
        assert_eq!(
            seen,
            (5..25u8).map(|value| [value; 256]).collect::<Vec<_>>()
        );
        state.outgoing(Some([25; 256])).unwrap();
        let seen = state.acknowledge(1, [0xff, 0xff, 0x0f], 0).unwrap();
        assert_eq!(
            seen,
            (6..26u8).map(|value| [value; 256]).collect::<Vec<_>>()
        );
    }

    #[test]
    fn checksum_matches_independent_byte_hash_vectors() {
        let mut sequence = [0; 256];
        for (index, byte) in sequence.iter_mut().enumerate() {
            *byte = index as u8;
        }
        assert_eq!(chat_checksum(&[]), 1);
        assert_eq!(chat_checksum(&[[0; 256]]), 32);
        assert_eq!(chat_checksum(&[sequence]), 160);
        assert_eq!(chat_checksum(&[[0; 256], sequence]), 97);
    }

    #[test]
    fn an_expired_session_cannot_sign_even_if_it_was_valid_at_join() {
        let mut state = connected();
        assert!(
            state
                .message(
                    PLAYER,
                    sign(packet("late"), 0, &[]),
                    true,
                    true,
                    NOW + 60_000
                )
                .is_err()
        );
    }

    #[test]
    fn unsigned_chat_is_honest_and_secure_mode_fails_closed() {
        let mut offline = ChatState::default();
        let mut message = packet("offline");
        message.signature = Some([4; 256]);
        offline.message(PLAYER, message, false, false, NOW).unwrap();
        assert!(
            matches!(&offline.take_events()[..], [ChatEvent::Message(message)] if message.signature.is_none())
        );
        let mut online = ChatState::default();
        assert!(
            online
                .message(PLAYER, packet("unsigned"), true, true, NOW)
                .is_err()
        );
        assert!(
            online
                .message(PLAYER, packet("unsigned"), true, false, NOW)
                .is_ok()
        );
        let mut old = packet("old");
        old.timestamp -= 1;
        assert!(online.message(PLAYER, old, true, false, NOW).is_err());
        assert!(
            online
                .message(PLAYER, packet("bad\ntext"), true, false, NOW)
                .is_err()
        );
        online.enabled(false);
        assert_eq!(online.outgoing(None).unwrap(), None);
        assert!(
            online
                .message(PLAYER, packet("hidden"), true, false, NOW)
                .is_err()
        );
    }

    #[test]
    fn ignored_signed_commands_advance_the_acknowledgement_and_signing_index() {
        let mut state = connected();
        let previous = [0x42; 256];
        state.outgoing(Some(previous)).unwrap();
        state
            .ignored_command(serverbound::SignedChatCommand {
                command: "unsupported message".into(),
                timestamp: NOW,
                salt: 5,
                argument_signatures: vec![play::ArgumentSignature {
                    name: "message".into(),
                    signature: [0; 256],
                }],
                offset: 1,
                acknowledged: [0, 0, 8],
                checksum: 0,
            })
            .unwrap();
        let mut message = packet("after command");
        message.acknowledged = [0, 0, 8];
        message.checksum = 0;
        state
            .message(PLAYER, sign(message, 1, &[previous]), true, true, NOW)
            .unwrap();
        assert!(
            matches!(&state.take_events()[..], [ChatEvent::Message(message)] if message.index == 1)
        );
    }
}
