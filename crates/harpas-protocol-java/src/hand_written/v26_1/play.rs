//! Hand-written Play types of Java Edition 26.1.

use crate::codec::{self, Decode, DecodeError, Encode, Nbt};
use crate::v26_1::play::{ProfileProperty, PublicKeyData};

/// The body of a Player Info Update: which actions it carries, and one entry per player.
///
/// The actions are a bit set, and every entry has the fields of the actions that are set, in the order of
/// the bits. Fields of the actions that are not set are left at their default when decoding and are not
/// sent when encoding.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlayerInfoUpdates {
    /// The actions every entry carries.
    pub actions: PlayerInfoActions,
    /// One entry per player.
    pub entries: Vec<PlayerInfoEntry>,
}

/// The actions of a Player Info Update, sent as one byte.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct PlayerInfoActions {
    /// Bit 0: adds the player, with their name and profile properties.
    pub add_player: bool,
    /// Bit 1: sets or clears the player's chat session.
    pub initialize_chat: bool,
    /// Bit 2: sets the player's game mode.
    pub update_game_mode: bool,
    /// Bit 3: shows or hides the player in the player list.
    pub update_listed: bool,
    /// Bit 4: sets the player's latency.
    pub update_latency: bool,
    /// Bit 5: sets or clears the player's display name.
    pub update_display_name: bool,
    /// Bit 6: sets the player's position in the player list.
    pub update_list_order: bool,
    /// Bit 7: shows or hides the hat layer of the player's skin in the player list.
    pub update_hat: bool,
}

impl PlayerInfoActions {
    fn bits(self) -> [bool; 8] {
        [
            self.add_player,
            self.initialize_chat,
            self.update_game_mode,
            self.update_listed,
            self.update_latency,
            self.update_display_name,
            self.update_list_order,
            self.update_hat,
        ]
    }
}

/// A player's entry in a Player Info Update.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlayerInfoEntry {
    /// The player's UUID, always sent.
    pub uuid: u128,
    /// With `add_player`: the player's name.
    pub name: String,
    /// With `add_player`: the player's profile properties, such as the skin.
    pub properties: Vec<ProfileProperty>,
    /// With `initialize_chat`: the player's chat session, or none to clear it.
    pub chat_session: Option<ChatSession>,
    /// With `update_game_mode`: the player's game mode, as in [`GameMode`](crate::v26_1::play::GameMode).
    pub game_mode: i32,
    /// With `update_listed`: whether the player is shown in the player list.
    pub listed: bool,
    /// With `update_latency`: the player's latency in milliseconds.
    pub latency: i32,
    /// With `update_display_name`: the name shown in the player list, a text component, or none for the
    /// player's name.
    pub display_name: Option<Nbt>,
    /// With `update_list_order`: the player's position in the player list; higher comes first.
    pub list_order: i32,
    /// With `update_hat`: whether the hat layer of the skin is shown in the player list.
    pub show_hat: bool,
}

/// A player's chat session.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatSession {
    /// ID of the session.
    pub session_id: u128,
    /// The key the player signs messages with.
    pub public_key: PublicKeyData,
}

impl Encode for ChatSession {
    fn encode(&self, buf: &mut Vec<u8>) {
        codec::write_uuid(buf, self.session_id);
        self.public_key.encode(buf);
    }
}

impl Decode for ChatSession {
    fn decode(input: &mut &[u8]) -> Result<Self, DecodeError> {
        Ok(Self {
            session_id: codec::read_uuid(input)?,
            public_key: PublicKeyData::decode(input)?,
        })
    }
}

impl Encode for PlayerInfoUpdates {
    fn encode(&self, buf: &mut Vec<u8>) {
        let bits = self.actions.bits();
        let byte = bits
            .iter()
            .enumerate()
            .fold(0u8, |byte, (bit, set)| byte | (u8::from(*set) << bit));
        codec::write_u8(buf, byte);
        codec::write_array(buf, &self.entries, |buf, entry| {
            codec::write_uuid(buf, entry.uuid);
            let [
                add_player,
                initialize_chat,
                update_game_mode,
                update_listed,
                update_latency,
                update_display_name,
                update_list_order,
                update_hat,
            ] = bits;
            if add_player {
                codec::write_string(buf, &entry.name);
                codec::write_array(buf, &entry.properties, |buf, p| p.encode(buf));
            }
            if initialize_chat {
                codec::write_option(buf, &entry.chat_session, |buf, s| s.encode(buf));
            }
            if update_game_mode {
                codec::write_varint(buf, entry.game_mode);
            }
            if update_listed {
                codec::write_bool(buf, entry.listed);
            }
            if update_latency {
                codec::write_varint(buf, entry.latency);
            }
            if update_display_name {
                codec::write_option(buf, &entry.display_name, codec::write_nbt);
            }
            if update_list_order {
                codec::write_varint(buf, entry.list_order);
            }
            if update_hat {
                codec::write_bool(buf, entry.show_hat);
            }
        });
    }
}

impl Decode for PlayerInfoUpdates {
    fn decode(input: &mut &[u8]) -> Result<Self, DecodeError> {
        let byte = codec::read_u8(input)?;
        let bit = |n: u32| byte & (1 << n) != 0;
        let actions = PlayerInfoActions {
            add_player: bit(0),
            initialize_chat: bit(1),
            update_game_mode: bit(2),
            update_listed: bit(3),
            update_latency: bit(4),
            update_display_name: bit(5),
            update_list_order: bit(6),
            update_hat: bit(7),
        };
        let entries = codec::read_array(input, usize::MAX, |input| {
            let mut entry = PlayerInfoEntry {
                uuid: codec::read_uuid(input)?,
                ..PlayerInfoEntry::default()
            };
            if actions.add_player {
                entry.name = codec::read_string(input, 16)?;
                entry.properties = codec::read_array(input, 16, ProfileProperty::decode)?;
            }
            if actions.initialize_chat {
                entry.chat_session = codec::read_option(input, ChatSession::decode)?;
            }
            if actions.update_game_mode {
                entry.game_mode = codec::read_varint(input)?;
            }
            if actions.update_listed {
                entry.listed = codec::read_bool(input)?;
            }
            if actions.update_latency {
                entry.latency = codec::read_varint(input)?;
            }
            if actions.update_display_name {
                entry.display_name = codec::read_option(input, codec::read_nbt)?;
            }
            if actions.update_list_order {
                entry.list_order = codec::read_varint(input)?;
            }
            if actions.update_hat {
                entry.show_hat = codec::read_bool(input)?;
            }
            Ok(entry)
        })?;
        Ok(Self { actions, entries })
    }
}
