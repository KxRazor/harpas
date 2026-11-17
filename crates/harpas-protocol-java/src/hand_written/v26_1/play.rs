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

/// The changes to an item's default data components (`DataComponentPatch`): components added or replaced, each
/// with its type's numeric ID and its value as encoded, and components removed.
///
/// Component values are kept as their bytes, so a stack is sent back as it was received; decoding them waits
/// for each component's codec.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComponentPatch {
    /// Added or replaced components: the type's ID in `minecraft:data_component_type`, and the encoded value.
    pub added: Vec<(i32, Vec<u8>)>,
    /// Removed components, by type ID.
    pub removed: Vec<i32>,
}

impl ComponentPatch {
    /// Writes the patch; `delimited` prefixes each value with its length, as `DELIMITED_STREAM_CODEC` does.
    fn write(&self, buf: &mut Vec<u8>, delimited: bool) {
        let len = |n: usize| i32::try_from(n).expect("component count fits in a VarInt");
        codec::write_varint(buf, len(self.added.len()));
        codec::write_varint(buf, len(self.removed.len()));
        for (kind, value) in &self.added {
            codec::write_varint(buf, *kind);
            if delimited {
                codec::write_bytes(buf, value);
            } else {
                buf.extend_from_slice(value);
            }
        }
        for kind in &self.removed {
            codec::write_varint(buf, *kind);
        }
    }

    /// Reads a patch. Without the delimited form, only a patch that adds nothing can be read.
    fn read(input: &mut &[u8], delimited: bool) -> Result<Self, DecodeError> {
        let count = |input: &mut &[u8]| {
            let count = codec::read_varint(input)?;
            let count = usize::try_from(count).map_err(|_| DecodeError::NegativeLength(count))?;
            // Every entry takes at least one byte.
            if count > input.len() {
                return Err(DecodeError::UnexpectedEnd);
            }
            Ok(count)
        };
        let added = count(input)?;
        let removed = count(input)?;
        if added > 0 && !delimited {
            return Err(DecodeError::UndelimitedComponents);
        }
        let mut patch = Self::default();
        for _ in 0..added {
            let kind = codec::read_varint(input)?;
            patch
                .added
                .push((kind, codec::read_bytes(input, usize::MAX)?));
        }
        for _ in 0..removed {
            patch.removed.push(codec::read_varint(input)?);
        }
        Ok(patch)
    }
}

/// An item stack, or none (`ItemStack.OPTIONAL_STREAM_CODEC`): a count, then for a count above zero the item's
/// numeric ID in `minecraft:item` and the changes to its data components.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemStack {
    /// How many; zero or less is no stack.
    pub count: i32,
    /// The item's ID in `minecraft:item`.
    pub item: i32,
    /// The changes to the item's default components.
    pub components: ComponentPatch,
}

impl ItemStack {
    /// Whether this is no stack (`ItemStack.isEmpty`, as the codec sees it).
    pub fn is_empty(&self) -> bool {
        self.count <= 0
    }

    fn write(&self, buf: &mut Vec<u8>, delimited: bool) {
        if self.is_empty() {
            codec::write_varint(buf, 0);
            return;
        }
        codec::write_varint(buf, self.count);
        codec::write_varint(buf, self.item);
        self.components.write(buf, delimited);
    }

    fn read(input: &mut &[u8], delimited: bool) -> Result<Self, DecodeError> {
        let count = codec::read_varint(input)?;
        if count <= 0 {
            return Ok(Self::default());
        }
        let item = codec::read_varint(input)?;
        if item < 0 {
            return Err(DecodeError::InvalidRegistryId(item));
        }
        Ok(Self {
            count,
            item,
            components: ComponentPatch::read(input, delimited)?,
        })
    }
}

impl Encode for ItemStack {
    fn encode(&self, buf: &mut Vec<u8>) {
        self.write(buf, false);
    }
}

impl Decode for ItemStack {
    fn decode(input: &mut &[u8]) -> Result<Self, DecodeError> {
        Self::read(input, false)
    }
}

/// An item stack as a client sends it in Creative (`ItemStack.OPTIONAL_UNTRUSTED_STREAM_CODEC`): each added
/// component's value is prefixed with its length, so it can be read without the component's codec.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UntrustedItemStack(pub ItemStack);

impl Encode for UntrustedItemStack {
    fn encode(&self, buf: &mut Vec<u8>) {
        self.0.write(buf, true);
    }
}

impl Decode for UntrustedItemStack {
    fn decode(input: &mut &[u8]) -> Result<Self, DecodeError> {
        ItemStack::read(input, true).map(Self)
    }
}

/// One value of an entity's tracked data, of the serializers ported so far (`EntityDataSerializers`).
#[derive(Debug, Clone, PartialEq)]
pub enum MetadataValue {
    /// Serializer 0.
    Byte(i8),
    /// Serializer 1, a VarInt.
    Int(i32),
    /// Serializer 3.
    Float(f32),
    /// Serializer 7.
    ItemStack(ItemStack),
    /// Serializer 8.
    Boolean(bool),
}

impl MetadataValue {
    fn serializer(&self) -> i32 {
        match self {
            Self::Byte(_) => 0,
            Self::Int(_) => 1,
            Self::Float(_) => 3,
            Self::ItemStack(_) => 7,
            Self::Boolean(_) => 8,
        }
    }
}

/// Tracked data values of an entity, each with its index in the entity's data, ending with index 255.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EntityMetadata {
    /// The values, by index.
    pub entries: Vec<(u8, MetadataValue)>,
}

/// The index that ends the list.
const METADATA_END: u8 = 255;

impl Encode for EntityMetadata {
    fn encode(&self, buf: &mut Vec<u8>) {
        for (index, value) in &self.entries {
            codec::write_u8(buf, *index);
            codec::write_varint(buf, value.serializer());
            match value {
                MetadataValue::Byte(n) => codec::write_u8(buf, *n as u8),
                MetadataValue::Int(n) => codec::write_varint(buf, *n),
                MetadataValue::Float(n) => buf.extend_from_slice(&n.to_be_bytes()),
                MetadataValue::ItemStack(stack) => stack.encode(buf),
                MetadataValue::Boolean(b) => codec::write_bool(buf, *b),
            }
        }
        codec::write_u8(buf, METADATA_END);
    }
}

impl Decode for EntityMetadata {
    fn decode(input: &mut &[u8]) -> Result<Self, DecodeError> {
        let mut entries = Vec::new();
        loop {
            let index = codec::read_u8(input)?;
            if index == METADATA_END {
                return Ok(Self { entries });
            }
            let value = match codec::read_varint(input)? {
                0 => MetadataValue::Byte(codec::read_u8(input)? as i8),
                1 => MetadataValue::Int(codec::read_varint(input)?),
                3 => {
                    let bytes = input.get(..4).ok_or(DecodeError::UnexpectedEnd)?;
                    let value = f32::from_be_bytes(bytes.try_into().expect("four bytes"));
                    *input = &input[4..];
                    MetadataValue::Float(value)
                }
                7 => MetadataValue::ItemStack(ItemStack::decode(input)?),
                8 => MetadataValue::Boolean(codec::read_bool(input)?),
                other => {
                    return Err(DecodeError::UnknownEnumValue {
                        name: "EntityDataSerializer",
                        value: i64::from(other),
                    });
                }
            };
            entries.push((index, value));
        }
    }
}

#[cfg(test)]
mod item_tests {
    use super::*;

    #[test]
    fn stacks_keep_their_component_values_from_client_to_client() {
        let stack = ItemStack {
            count: 3,
            item: 1,
            components: ComponentPatch {
                added: vec![(5, vec![0x02, 0x07])],
                removed: vec![9],
            },
        };
        let mut untrusted = Vec::new();
        UntrustedItemStack(stack.clone()).encode(&mut untrusted);
        assert_eq!(untrusted, [3, 1, 1, 1, 5, 2, 0x02, 0x07, 9]);
        assert_eq!(
            UntrustedItemStack::decode(&mut &untrusted[..]),
            Ok(UntrustedItemStack(stack.clone()))
        );
        // Sent to a client, the value follows its type without a length.
        let mut sent = Vec::new();
        stack.encode(&mut sent);
        assert_eq!(sent, [3, 1, 1, 1, 5, 0x02, 0x07, 9]);
        assert_eq!(
            ItemStack::decode(&mut &sent[..]),
            Err(DecodeError::UndelimitedComponents)
        );
    }

    #[test]
    fn metadata_lists_end_with_index_255() {
        let metadata = EntityMetadata {
            entries: vec![(
                8,
                MetadataValue::ItemStack(ItemStack {
                    count: 2,
                    item: 1,
                    components: ComponentPatch::default(),
                }),
            )],
        };
        let mut buf = Vec::new();
        metadata.encode(&mut buf);
        assert_eq!(buf, [8, 7, 2, 1, 0, 0, 255]);
        assert_eq!(EntityMetadata::decode(&mut &buf[..]), Ok(metadata));
    }

    #[test]
    fn an_empty_stack_is_a_zero_count() {
        let mut buf = Vec::new();
        ItemStack::default().encode(&mut buf);
        assert_eq!(buf, [0]);
        assert_eq!(ItemStack::decode(&mut &[0][..]), Ok(ItemStack::default()));
        let plain = [64, 1, 0, 0];
        assert_eq!(
            ItemStack::decode(&mut &plain[..]),
            Ok(ItemStack {
                count: 64,
                item: 1,
                components: ComponentPatch::default(),
            })
        );
    }
}
