//! Configuration packets against bytes written by hand from the documented wire format (minecraft.wiki,
//! "Java Edition protocol/Packets" and "NBT format"), not from the generator's output.

use harpas_core::nbt::{Compound, NbtError};
use harpas_protocol_java::codec::{DecodeError, Nbt};
use harpas_protocol_java::v26_1::configuration::{self, clientbound, serverbound};

const PACK: u128 = 0x0123_4567_89ab_cdef_0123_4567_89ab_cdef;

/// Decodes `bytes`, checks the packet, and encodes it back to the same bytes.
macro_rules! round_trip {
    ($packet:ty, $bytes:expr, $expected:expr) => {{
        let bytes: Vec<u8> = $bytes;
        let decoded = <$packet>::decode(&bytes).expect("valid packet");
        assert_eq!(decoded, $expected.into());
        let mut encoded = Vec::new();
        decoded.encode(&mut encoded);
        assert_eq!(encoded, bytes);
    }};
}

fn concat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

/// `{"text": "Hi"}` as network NBT.
const TEXT_NBT: &[u8] = &[
    0x0a, 0x08, 0x00, 0x04, b't', b'e', b'x', b't', 0x00, 0x02, b'H', b'i', 0x00,
];

fn text() -> Nbt {
    Nbt::Compound([("text", "Hi")].into_iter().collect())
}

#[test]
fn clientbound_without_nbt() {
    round_trip!(
        clientbound::Packet,
        concat(&[&[0x01, 0x0f], b"minecraft:brand", &[0x07], b"harpas"]),
        clientbound::PluginMessage {
            channel: "minecraft:brand".into(),
            data: concat(&[&[0x07], b"harpas"]),
        }
    );
    round_trip!(
        clientbound::Packet,
        vec![0x03],
        clientbound::FinishConfiguration
    );
    round_trip!(
        clientbound::Packet,
        concat(&[&[0x04], &7_i64.to_be_bytes()]),
        clientbound::KeepAlive { id: 7 }
    );
    round_trip!(
        clientbound::Packet,
        concat(&[&[0x08, 0x01], &PACK.to_be_bytes()]),
        clientbound::RemoveResourcePack { uuid: Some(PACK) }
    );
    round_trip!(
        clientbound::Packet,
        concat(&[&[0x0b, 0x09], b"localhost", &[0xdd, 0xc7, 0x01]]),
        clientbound::Transfer {
            host: "localhost".into(),
            port: 25565,
        }
    );
    round_trip!(
        clientbound::Packet,
        concat(&[&[0x0c, 0x01, 0x11], b"minecraft:vanilla"]),
        clientbound::FeatureFlags {
            features: vec!["minecraft:vanilla".into()],
        }
    );
    round_trip!(
        clientbound::Packet,
        concat(&[
            &[0x0d, 0x01, 0x0f],
            b"minecraft:block",
            &[0x01, 0x0e],
            b"minecraft:logs",
            &[0x03, 0x2a, 0x2b, 0xac, 0x02],
        ]),
        clientbound::UpdateTags {
            registries: vec![configuration::RegistryTags {
                registry: "minecraft:block".into(),
                tags: vec![configuration::Tag {
                    name: "minecraft:logs".into(),
                    entries: vec![42, 43, 300],
                }],
            }],
        }
    );
    round_trip!(
        clientbound::Packet,
        concat(&[
            &[0x0e, 0x01, 0x09],
            b"minecraft",
            &[0x04],
            b"core",
            &[0x04],
            b"26.1"
        ]),
        clientbound::SelectKnownPacks {
            packs: vec![configuration::KnownPack {
                namespace: "minecraft".into(),
                id: "core".into(),
                version: "26.1".into(),
            }],
        }
    );
}

#[test]
fn clientbound_with_nbt() {
    round_trip!(
        clientbound::Packet,
        concat(&[&[0x02], TEXT_NBT]),
        clientbound::Disconnect { reason: text() }
    );
    // A dimension type sent in full, and a biome the client has from the core pack.
    let mut dimension = Compound::new();
    dimension.insert("height", 384_i32);
    round_trip!(
        clientbound::Packet,
        concat(&[
            &[0x07, 0x18],
            b"minecraft:dimension_type",
            &[0x02, 0x13],
            b"minecraft:overworld",
            &[0x01, 0x0a, 0x03, 0x00, 0x06],
            b"height",
            &384_i32.to_be_bytes(),
            &[0x00, 0x0f],
            b"minecraft:plain",
            &[0x00],
        ]),
        clientbound::RegistryData {
            registry: "minecraft:dimension_type".into(),
            entries: vec![
                configuration::RegistryEntry {
                    id: "minecraft:overworld".into(),
                    data: Some(Nbt::Compound(dimension)),
                },
                configuration::RegistryEntry {
                    id: "minecraft:plain".into(),
                    data: None,
                },
            ],
        }
    );
    round_trip!(
        clientbound::Packet,
        concat(&[
            &[0x09],
            &PACK.to_be_bytes(),
            &[0x03],
            b"u:x",
            &[0x00, 0x01, 0x01],
            TEXT_NBT,
        ]),
        clientbound::AddResourcePack {
            uuid: PACK,
            url: "u:x".into(),
            hash: String::new(),
            forced: true,
            prompt: Some(text()),
        }
    );
}

#[test]
fn server_links_are_built_in_or_custom() {
    round_trip!(
        clientbound::Packet,
        concat(&[
            &[0x10, 0x02],
            // Built-in label 6 (website).
            &[0x01, 0x06, 0x03],
            b"u:a",
            // Custom label as a string tag.
            &[0x00, 0x08, 0x00, 0x04],
            b"Wiki",
            &[0x03],
            b"u:b",
        ]),
        clientbound::ServerLinks {
            links: vec![
                configuration::ServerLink {
                    label: configuration::ServerLinkLabel::BuiltIn(
                        configuration::ServerLinkType::Website
                    ),
                    url: "u:a".into(),
                },
                configuration::ServerLink {
                    label: configuration::ServerLinkLabel::Custom(Nbt::from("Wiki")),
                    url: "u:b".into(),
                },
            ],
        }
    );
}

#[test]
fn serverbound() {
    round_trip!(
        serverbound::Packet,
        concat(&[
            &[0x00, 0x05],
            b"en_us",
            &[0x0c, 0x00, 0x01, 0x7f, 0x01, 0x00, 0x01, 0x02],
        ]),
        serverbound::ClientInformation {
            locale: "en_us".into(),
            view_distance: 12,
            chat_mode: configuration::ChatMode::Enabled,
            chat_colors: true,
            displayed_skin_parts: 0x7f,
            main_hand: configuration::MainHand::Right,
            enable_text_filtering: false,
            allow_server_listings: true,
            particle_status: configuration::ParticleStatus::Minimal,
        }
    );
    round_trip!(
        serverbound::Packet,
        vec![0x03],
        serverbound::AcknowledgeFinishConfiguration
    );
    round_trip!(
        serverbound::Packet,
        concat(&[&[0x06], &PACK.to_be_bytes(), &[0x03]]),
        serverbound::ResourcePackResponse {
            uuid: PACK,
            result: configuration::ResourcePackResult::Accepted,
        }
    );
    round_trip!(
        serverbound::Packet,
        vec![0x07, 0x00],
        serverbound::SelectKnownPacks { packs: vec![] }
    );
    round_trip!(
        serverbound::Packet,
        concat(&[&[0x08, 0x05], b"a:act", &[0x01], TEXT_NBT]),
        serverbound::CustomClickAction {
            id: "a:act".into(),
            payload: Some(text()),
        }
    );
    round_trip!(
        serverbound::Packet,
        vec![0x09],
        serverbound::AcceptCodeOfConduct
    );
}

#[test]
fn invalid_packets_are_rejected() {
    let cases: Vec<(Vec<u8>, DecodeError)> = vec![
        // Chat mode 3 does not exist.
        (
            concat(&[&[0x00, 0x05], b"en_us", &[0x0c, 0x03]]),
            DecodeError::UnknownEnumValue {
                name: "ChatMode",
                value: 3,
            },
        ),
        // 65 known packs, over vanilla's 64.
        (vec![0x07, 0x41], DecodeError::TooLong { len: 65, max: 64 }),
        // A click action payload whose root is TAG_End.
        (
            concat(&[&[0x08, 0x01], b"a", &[0x01, 0x00]]),
            DecodeError::Nbt(NbtError::EndRoot),
        ),
        // A click action payload cut short inside a compound.
        (
            concat(&[&[0x08, 0x01], b"a", &[0x01, 0x0a, 0x01, 0x00]]),
            DecodeError::Nbt(NbtError::UnexpectedEnd),
        ),
    ];
    for (bytes, error) in cases {
        assert_eq!(
            serverbound::Packet::decode(&bytes),
            Err(error),
            "{bytes:02x?}"
        );
    }
}
