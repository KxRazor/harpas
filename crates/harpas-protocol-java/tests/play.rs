//! Play packets against bytes written by hand from the documented wire format (minecraft.wiki, "Java
//! Edition protocol/Packets" and "Java Edition protocol/Data types"), not from the generator's output.

use harpas_protocol_java::codec::{DecodeError, Holder, LpVec3, Nbt};
use harpas_protocol_java::v26_1::play::{self, clientbound, serverbound};

const STEVE: u128 = 0x8667ba71_b85a_4004_af54_457a9734eed7;

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

fn f64s(values: &[f64]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_be_bytes()).collect()
}

#[test]
fn block_positions_match_the_documented_example() {
    // minecraft.wiki's example: x 18357644, y 831, z -20882616.
    let position = play::BlockPos {
        x: 18357644,
        y: 831,
        z: -20882616,
    };
    round_trip!(
        clientbound::Packet,
        concat(&[
            &[0x61, 0x13],
            b"minecraft:overworld",
            &0x4607_632c_15b4_833f_u64.to_be_bytes(),
            &90.0_f32.to_be_bytes(),
            &0.0_f32.to_be_bytes(),
        ]),
        clientbound::SetDefaultSpawnPosition {
            position: play::GlobalPos {
                dimension: "minecraft:overworld".into(),
                position,
            },
            yaw: 90.0,
            pitch: 0.0,
        }
    );
    // Negative heights are sign-extended from 12 bits.
    let low = play::BlockPos {
        x: -1,
        y: -64,
        z: 0,
    };
    let bits = (0x3ff_ffff_u64 << 38) | 0xfc0;
    let decoded = <play::BlockPos as harpas_protocol_java::codec::Decode>::decode(
        &mut &bits.to_be_bytes()[..],
    );
    assert_eq!(decoded, Ok(low));
}

#[test]
fn login() {
    let spawn = play::SpawnInfo {
        dimension_type: 0,
        dimension: "minecraft:overworld".into(),
        hashed_seed: 42,
        game_mode: play::GameMode::Creative,
        previous_game_mode: -1,
        is_debug: false,
        is_flat: true,
        death_location: None,
        portal_cooldown: 0,
        sea_level: 63,
    };
    round_trip!(
        clientbound::Packet,
        concat(&[
            &[0x31],
            &7_i32.to_be_bytes(),
            &[0x00, 0x01, 0x13],
            b"minecraft:overworld",
            // Max players 20, view distance 10, simulation distance 8, then three booleans.
            &[0x14, 0x0a, 0x08, 0x00, 0x01, 0x00],
            &[0x00, 0x13],
            b"minecraft:overworld",
            &42_i64.to_be_bytes(),
            // Creative, no previous game mode, not debug, flat, no death location, cooldown 0, sea level 63.
            &[0x01, 0xff, 0x00, 0x01, 0x00, 0x00, 0x3f],
            &[0x01],
        ]),
        clientbound::Login {
            entity_id: 7,
            is_hardcore: false,
            dimensions: vec!["minecraft:overworld".into()],
            max_players: 20,
            view_distance: 10,
            simulation_distance: 8,
            reduced_debug_info: false,
            enable_respawn_screen: true,
            do_limited_crafting: false,
            spawn,
            enforces_secure_chat: true,
        }
    );
}

#[test]
fn entities() {
    round_trip!(
        clientbound::Packet,
        concat(&[
            &[0x01, 0x07],
            &STEVE.to_be_bytes(),
            &[0x95, 0x01],
            &f64s(&[0.5, 64.0, -0.5]),
            // No velocity, then pitch, yaw and head yaw.
            &[0x00, 0x00, 0x40, 0x40, 0x00],
        ]),
        clientbound::SpawnEntity {
            entity_id: 7,
            uuid: STEVE,
            entity_type: 149,
            x: 0.5,
            y: 64.0,
            z: -0.5,
            velocity: LpVec3::default(),
            pitch: 0,
            yaw: 64,
            head_yaw: 64,
            data: 0,
        }
    );
    round_trip!(
        clientbound::Packet,
        concat(&[
            &[0x36, 0x07],
            &4096_i16.to_be_bytes(),
            &0_i16.to_be_bytes(),
            &(-2048_i16).to_be_bytes(),
            &[0x80, 0x00, 0x01],
        ]),
        clientbound::UpdateEntityPositionAndRotation {
            entity_id: 7,
            delta_x: 4096,
            delta_y: 0,
            delta_z: -2048,
            yaw: -128,
            pitch: 0,
            on_ground: true,
        }
    );
    round_trip!(
        clientbound::Packet,
        concat(&[&[0x4d, 0x02, 0x07, 0x80, 0x01]]),
        clientbound::RemoveEntities {
            entity_ids: vec![7, 128],
        }
    );
    round_trip!(
        clientbound::Packet,
        concat(&[
            &[0x48, 0x01],
            &f64s(&[0.0, 1.5, 0.0, 0.0, 0.0, 0.0]),
            &45.0_f32.to_be_bytes(),
            &0.0_f32.to_be_bytes(),
            // Yaw (bit 3) and rotate velocity (bit 8) are relative.
            &0x0108_u32.to_be_bytes(),
        ]),
        clientbound::SynchronizePlayerPosition {
            teleport_id: 1,
            x: 0.0,
            y: 1.5,
            z: 0.0,
            velocity_x: 0.0,
            velocity_y: 0.0,
            velocity_z: 0.0,
            yaw: 45.0,
            pitch: 0.0,
            relatives: play::TeleportFlags {
                yaw: true,
                rotate_velocity: true,
                ..Default::default()
            },
        }
    );
}

#[test]
fn chunks() {
    let block_entity_nbt = Nbt::Compound([("id", "minecraft:chest")].into_iter().collect());
    let mut nbt = Vec::new();
    block_entity_nbt.write_network(&mut nbt);
    round_trip!(
        clientbound::Packet,
        concat(&[
            &[0x2d],
            &1_i32.to_be_bytes(),
            &(-2_i32).to_be_bytes(),
            // One heightmap, motion blocking, with one long.
            &[0x01, 0x04, 0x01],
            &0_i64.to_be_bytes(),
            // Three bytes of section data.
            &[0x03, 0xaa, 0xbb, 0xcc],
            // A chest at x 3, z 12 of the column, y -60, and one with no data.
            &[0x02, 0x3c],
            &(-60_i16).to_be_bytes(),
            &[0x02],
            &nbt,
            &[0x00, 0x00, 0x00, 0x02, 0x00],
            // Sky light in the lowest section only, full; no block light.
            &[0x01],
            &1_i64.to_be_bytes(),
            &[0x00, 0x00, 0x00, 0x01, 0x80, 0x10],
            &[0xff; 2048],
            &[0x00],
        ]),
        clientbound::ChunkDataAndUpdateLight {
            x: 1,
            z: -2,
            heightmaps: vec![play::Heightmap {
                kind: play::HeightmapType::MotionBlocking,
                data: vec![0],
            }],
            data: vec![0xaa, 0xbb, 0xcc],
            block_entities: vec![
                play::ChunkBlockEntity {
                    position: play::PackedXZ { x: 3, z: 12 },
                    y: -60,
                    kind: 2,
                    data: Some(block_entity_nbt),
                },
                play::ChunkBlockEntity {
                    position: play::PackedXZ { x: 0, z: 0 },
                    y: 0,
                    kind: 2,
                    data: None,
                },
            ],
            light: play::LightData {
                sky_light_mask: vec![1],
                block_light_mask: vec![],
                empty_sky_light_mask: vec![],
                empty_block_light_mask: vec![],
                sky_light: vec![vec![0xff; 2048]],
                block_light: vec![],
            },
        }
    );
    // Light sections are exactly 2048 bytes; vanilla rejects longer ones.
    let long_light = concat(&[
        &[0x30, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x81, 0x10],
        &[0; 2049],
    ]);
    assert_eq!(
        clientbound::Packet::decode(&long_light),
        Err(DecodeError::TooLong {
            len: 2049,
            max: 2048
        })
    );
}

#[test]
fn chat() {
    let signature = [0x5a; 256];
    let text = Nbt::from("Steve");
    let mut name = Vec::new();
    text.write_network(&mut name);
    round_trip!(
        clientbound::Packet,
        concat(&[
            &[0x41, 0x00],
            &STEVE.to_be_bytes(),
            &[0x00, 0x01],
            &signature,
            &[0x02],
            b"hi",
            &1_i64.to_be_bytes(),
            &2_i64.to_be_bytes(),
            // Two seen messages: one in the cache at index 4 (sent as 5), one in full.
            &[0x02, 0x05, 0x00],
            &signature,
            // No unsigned content, partially filtered with one long, chat type 0 (sent as 1).
            &[0x00, 0x02, 0x01],
            &3_i64.to_be_bytes(),
            &[0x01],
            &name,
            &[0x00],
        ]),
        clientbound::PlayerChatMessage {
            global_index: 0,
            sender: STEVE,
            index: 0,
            signature: Some(signature),
            message: "hi".into(),
            timestamp: 1,
            salt: 2,
            last_seen: vec![
                play::PackedSignature::Cached(5),
                play::PackedSignature::Full(signature),
            ],
            unsigned_content: None,
            filter_mask: play::FilterMask::PartiallyFiltered(vec![3]),
            chat_type: Holder::Reference(0),
            sender_name: text.clone(),
            target_name: None,
        }
    );
    round_trip!(
        clientbound::Packet,
        vec![0x1f, 0x05],
        clientbound::DeleteMessage {
            signature: play::PackedSignature::Cached(5),
        }
    );
    round_trip!(
        serverbound::Packet,
        concat(&[
            &[0x09, 0x02],
            b"hi",
            &1_i64.to_be_bytes(),
            &2_i64.to_be_bytes(),
            &[0x00, 0x03, 0x07, 0x00, 0x00, 0x2a],
        ]),
        serverbound::ChatMessage {
            message: "hi".into(),
            timestamp: 1,
            salt: 2,
            signature: None,
            offset: 3,
            acknowledged: [0x07, 0x00, 0x00],
            checksum: 0x2a,
        }
    );
    // 257 characters, over vanilla's 256.
    let long = concat(&[&[0x09, 0x81, 0x02], &[b'a'; 257]]);
    assert_eq!(
        serverbound::Packet::decode(&long),
        Err(DecodeError::TooLong { len: 257, max: 256 })
    );
}

#[test]
fn player_info_update() {
    round_trip!(
        clientbound::Packet,
        concat(&[
            // Add player, update listed and update latency.
            &[0x46, 0x19, 0x01],
            &STEVE.to_be_bytes(),
            &[0x05],
            b"Steve",
            &[0x00, 0x01, 0x80, 0x01],
        ]),
        clientbound::PlayerInfoUpdate {
            updates: play::PlayerInfoUpdates {
                actions: play::PlayerInfoActions {
                    add_player: true,
                    update_listed: true,
                    update_latency: true,
                    ..Default::default()
                },
                entries: vec![play::PlayerInfoEntry {
                    uuid: STEVE,
                    name: "Steve".into(),
                    listed: true,
                    latency: 128,
                    ..Default::default()
                }],
            },
        }
    );
    round_trip!(
        clientbound::Packet,
        concat(&[
            // List order (bit 6) comes before the hat (bit 7).
            &[0x46, 0xc0, 0x01],
            &STEVE.to_be_bytes(),
            &[0x03, 0x01],
        ]),
        clientbound::PlayerInfoUpdate {
            updates: play::PlayerInfoUpdates {
                actions: play::PlayerInfoActions {
                    update_list_order: true,
                    update_hat: true,
                    ..Default::default()
                },
                entries: vec![play::PlayerInfoEntry {
                    uuid: STEVE,
                    list_order: 3,
                    show_hat: true,
                    ..Default::default()
                }],
            },
        }
    );
}

#[test]
fn movement() {
    round_trip!(
        serverbound::Packet,
        concat(&[&[0x1e], &f64s(&[1.0, 64.0, 2.0]), &[0x01]]),
        serverbound::SetPlayerPosition {
            x: 1.0,
            y: 64.0,
            z: 2.0,
            flags: play::MovementFlags {
                on_ground: true,
                horizontal_collision: false,
            },
        }
    );
    round_trip!(
        serverbound::Packet,
        vec![0x2b, 0x51],
        serverbound::PlayerInput {
            inputs: play::PlayerInputs {
                forward: true,
                jump: true,
                sprint: true,
                ..Default::default()
            },
        }
    );
    // Bits without a flag are ignored, as vanilla does.
    assert_eq!(
        serverbound::Packet::decode(&[0x21, 0xfe]),
        Ok(serverbound::SetPlayerMovementFlags {
            flags: play::MovementFlags {
                on_ground: false,
                horizontal_collision: true,
            },
        }
        .into())
    );
    round_trip!(
        serverbound::Packet,
        vec![0x3f, 0x01],
        serverbound::SwingArm {
            hand: play::Hand::OffHand,
        }
    );
}
