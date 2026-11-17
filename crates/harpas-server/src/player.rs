//! Translate between the edition-neutral player engine and Java Play packets.

use std::io;

use harpas_engine::PlayerSession;
use harpas_engine::block::{BlockPos, DigAction};
use harpas_engine::player::{self, Input, Movement, Output, Position, Profile};
use harpas_java::{ChatEvent, Connection, PlayEvent};
use harpas_protocol_java::codec::{Holder, LpVec3, Nbt};
use harpas_protocol_java::v26_1::play::{self, clientbound, serverbound};

/// `minecraft:player` in minecraft-data's Java 26.1 entity type registry.
const PLAYER_ENTITY_TYPE: i32 = 155;

pub(crate) fn profile(player: &harpas_java::Player, mode: play::GameMode) -> Profile {
    Profile {
        uuid: player.uuid,
        name: player.name.clone(),
        properties: player
            .properties
            .iter()
            .map(|p| player::ProfileProperty {
                name: p.name.clone(),
                value: p.value.clone(),
                signature: p.signature.clone(),
            })
            .collect(),
        game_mode: match mode {
            play::GameMode::Survival => player::GameMode::Survival,
            play::GameMode::Creative => player::GameMode::Creative,
            play::GameMode::Adventure => player::GameMode::Adventure,
            play::GameMode::Spectator => player::GameMode::Spectator,
        },
    }
}

pub(crate) fn receive(connection: &mut Connection, session: &PlayerSession) -> io::Result<()> {
    use serverbound::Packet;
    for event in connection.take_play_events() {
        let input = match event {
            PlayEvent::Packet(packet) => match *packet {
                Packet::ConfirmTeleportation(p) => Input::ConfirmTeleport(p.teleport_id),
                Packet::ChunkBatchReceived(p) => Input::ChunkBatchReceived(p.chunks_per_tick),
                Packet::SetPlayerPosition(p) => Input::Movement(Movement {
                    position: Some([p.x, p.y, p.z]),
                    rotation: None,
                    on_ground: p.flags.on_ground,
                }),
                Packet::SetPlayerPositionAndRotation(p) => Input::Movement(Movement {
                    position: Some([p.x, p.y, p.z]),
                    rotation: Some([p.yaw, p.pitch]),
                    on_ground: p.flags.on_ground,
                }),
                Packet::SetPlayerRotation(p) => Input::Movement(Movement {
                    position: None,
                    rotation: Some([p.yaw, p.pitch]),
                    on_ground: p.flags.on_ground,
                }),
                Packet::SetPlayerMovementFlags(p) => Input::Movement(Movement {
                    position: None,
                    rotation: None,
                    on_ground: p.flags.on_ground,
                }),
                Packet::PlayerAbilities(p) => Input::Flying(p.flags.flying),
                Packet::PlayerAction(p) => {
                    let action = match p.status {
                        play::PlayerActionStatus::StartedDigging => DigAction::Start,
                        play::PlayerActionStatus::CancelledDigging => DigAction::Abort,
                        play::PlayerActionStatus::FinishedDigging => DigAction::Finish,
                        // Dropping, using and swapping items come with items.
                        _ => continue,
                    };
                    Input::Dig {
                        action,
                        position: BlockPos {
                            x: p.position.x,
                            y: p.position.y,
                            z: p.position.z,
                        },
                        sequence: p.sequence,
                    }
                }
                Packet::ClientInformation(p) => {
                    let distance = p.view_distance.clamp(2, 32) as u8;
                    Input::ViewDistance(distance)
                }
                _ => continue,
            },
            PlayEvent::Chat(ChatEvent::Session(session)) => {
                Input::ChatSession(player::ChatSession {
                    id: session.session_id,
                    expires_at: session.public_key.expires_at,
                    key: session.public_key.key,
                    key_signature: session.public_key.key_signature,
                })
            }
            PlayEvent::Chat(ChatEvent::SessionEnded) => Input::ChatSessionEnded,
            PlayEvent::Chat(ChatEvent::Message(message)) => {
                Input::ChatMessage(Box::new(player::ChatMessage {
                    index: message.index,
                    signature: message.signature,
                    message: message.message,
                    timestamp: message.timestamp,
                    salt: message.salt,
                    last_seen: message.last_seen,
                }))
            }
        };
        session.send(input).map_err(io::Error::other)?;
    }
    Ok(())
}

fn mode_id(mode: player::GameMode) -> i32 {
    match mode {
        player::GameMode::Survival => 0,
        player::GameMode::Creative => 1,
        player::GameMode::Adventure => 2,
        player::GameMode::Spectator => 3,
    }
}

fn angle(degrees: f32) -> i8 {
    ((f64::from(degrees).rem_euclid(360.0) * 256.0 / 360.0).floor() as u8) as i8
}

fn block_pos(position: BlockPos) -> play::BlockPos {
    play::BlockPos {
        x: position.x,
        y: position.y,
        z: position.z,
    }
}

/// One block as Update Section Blocks packs it: the state above 12 bits of position in the section, X, then Z,
/// then Y.
fn section_block(position: BlockPos, state: u32) -> i32 {
    ((state as i32) << 12) | (position.x & 15) << 8 | (position.z & 15) << 4 | (position.y & 15)
}

fn sync(position: Position, teleport_id: i32) -> clientbound::SynchronizePlayerPosition {
    clientbound::SynchronizePlayerPosition {
        teleport_id,
        x: position.x,
        y: position.y,
        z: position.z,
        velocity_x: 0.0,
        velocity_y: 0.0,
        velocity_z: 0.0,
        yaw: position.yaw,
        pitch: position.pitch,
        relatives: play::TeleportFlags::default(),
    }
}

/// Sends the logic thread's output for this player; returns the reason if the player was disconnected.
pub(crate) fn send(
    connection: &mut Connection,
    session: &PlayerSession,
    chunk_cache: &crate::chunks::Cache,
) -> Option<&'static str> {
    while let Some(output) = session.try_recv() {
        let packet = match output {
            Output::ChatSessionEnded { uuid } => clientbound::PlayerInfoUpdate {
                updates: play::PlayerInfoUpdates {
                    actions: play::PlayerInfoActions {
                        initialize_chat: true,
                        ..Default::default()
                    },
                    entries: vec![play::PlayerInfoEntry {
                        uuid,
                        chat_session: None,
                        ..Default::default()
                    }],
                },
            }
            .into(),
            Output::ChatSession { uuid, session } => clientbound::PlayerInfoUpdate {
                updates: play::PlayerInfoUpdates {
                    actions: play::PlayerInfoActions {
                        initialize_chat: true,
                        ..Default::default()
                    },
                    entries: vec![play::PlayerInfoEntry {
                        uuid,
                        chat_session: Some(play::ChatSession {
                            session_id: session.id,
                            public_key: play::PublicKeyData {
                                expires_at: session.expires_at,
                                key: session.key,
                                key_signature: session.key_signature,
                            },
                        }),
                        ..Default::default()
                    }],
                },
            }
            .into(),
            Output::ChatMessage { sender, message } => {
                connection.send_player_chat(clientbound::PlayerChatMessage {
                    global_index: 0, // Set separately for each receiving connection.
                    sender: sender.uuid,
                    index: message.index,
                    signature: message.signature,
                    message: message.message,
                    timestamp: message.timestamp,
                    salt: message.salt,
                    last_seen: message
                        .last_seen
                        .into_iter()
                        .map(play::PackedSignature::Full)
                        .collect(),
                    unsigned_content: None,
                    filter_mask: play::FilterMask::PassThrough,
                    // The synchronized 26.1 minecraft:chat_type registry starts with minecraft:chat.
                    chat_type: Holder::Reference(0),
                    sender_name: Nbt::from(sender.name.as_str()),
                    target_name: None,
                });
                continue;
            }
            Output::AddPlayer(profile) => clientbound::PlayerInfoUpdate {
                updates: play::PlayerInfoUpdates {
                    actions: play::PlayerInfoActions {
                        add_player: true,
                        update_game_mode: true,
                        update_listed: true,
                        update_latency: true,
                        update_list_order: true,
                        update_hat: true,
                        ..Default::default()
                    },
                    entries: vec![play::PlayerInfoEntry {
                        uuid: profile.uuid,
                        name: profile.name,
                        properties: profile
                            .properties
                            .into_iter()
                            .map(|p| play::ProfileProperty {
                                name: p.name,
                                value: p.value,
                                signature: p.signature,
                            })
                            .collect(),
                        game_mode: mode_id(profile.game_mode),
                        listed: true,
                        list_order: 2,
                        show_hat: true,
                        ..Default::default()
                    }],
                },
            }
            .into(),
            Output::RemovePlayer(uuid) => clientbound::PlayerInfoRemove {
                players: vec![uuid],
            }
            .into(),
            Output::Spawn(player) => clientbound::SpawnEntity {
                entity_id: player.entity_id,
                uuid: player.profile.uuid,
                entity_type: PLAYER_ENTITY_TYPE,
                x: player.position.x,
                y: player.position.y,
                z: player.position.z,
                velocity: LpVec3::default(),
                pitch: angle(player.position.pitch),
                yaw: angle(player.position.yaw),
                head_yaw: angle(player.position.yaw),
                data: 0,
            }
            .into(),
            Output::RemoveEntity(entity_id) => clientbound::RemoveEntities {
                entity_ids: vec![entity_id],
            }
            .into(),
            Output::MoveEntity {
                entity_id,
                position,
            } => {
                connection.send_play_packet(
                    clientbound::SetHeadRotation {
                        entity_id,
                        head_yaw: angle(position.yaw),
                    }
                    .into(),
                );
                clientbound::EntityPositionSync {
                    entity_id,
                    x: position.x,
                    y: position.y,
                    z: position.z,
                    velocity_x: 0.0,
                    velocity_y: 0.0,
                    velocity_z: 0.0,
                    yaw: position.yaw,
                    pitch: position.pitch,
                    on_ground: position.on_ground,
                }
                .into()
            }
            Output::OwnPosition(position) => {
                connection.update_view(position.x, position.z, None);
                continue;
            }
            Output::ViewDistance(distance) => {
                connection.update_view_distance(distance);
                continue;
            }
            Output::ChunkBatch(chunks) => {
                connection.send_play_packet(clientbound::ChunkBatchStart.into());
                let count = chunks.len() as i32;
                for chunk in chunks {
                    let packet = chunk_cache.prepare(chunk, connection);
                    connection.send_prepared_play_packet(&packet);
                }
                connection
                    .send_play_packet(clientbound::ChunkBatchFinished { batch_size: count }.into());
                continue;
            }
            Output::SectionBlocks(blocks) => match *blocks {
                [(position, state)] => clientbound::BlockUpdate {
                    position: block_pos(position),
                    block_state: state as i32,
                }
                .into(),
                _ => {
                    let Some(&(first, _)) = blocks.first() else {
                        continue;
                    };
                    clientbound::UpdateSectionBlocks {
                        section: play::SectionPos {
                            x: first.x >> 4,
                            y: first.y >> 4,
                            z: first.z >> 4,
                        },
                        blocks: blocks
                            .iter()
                            .map(|&(position, state)| section_block(position, state))
                            .collect(),
                    }
                    .into()
                }
            },
            Output::BlockUpdate { position, state } => clientbound::BlockUpdate {
                position: block_pos(position),
                block_state: state as i32,
            }
            .into(),
            Output::AcknowledgeBlockChanges(sequence) => {
                clientbound::AcknowledgeBlockChange { sequence }.into()
            }
            Output::LightUpdate { position, light } => clientbound::UpdateLight {
                chunk_x: position.x,
                chunk_z: position.z,
                light: crate::chunks::update_light(&light),
            }
            .into(),
            Output::BlockDestruction {
                entity_id,
                position,
                stage,
            } => clientbound::SetBlockDestroyStage {
                entity_id,
                position: block_pos(position),
                destroy_stage: stage,
            }
            .into(),
            Output::WorldEvent {
                event,
                position,
                data,
            } => clientbound::WorldEvent {
                event,
                position: block_pos(position),
                data,
                disable_relative_volume: false,
            }
            .into(),
            Output::Teleport {
                teleport_id,
                position,
            } => sync(position, teleport_id).into(),
            Output::Disconnect(reason) => {
                connection.disconnect_play(reason);
                return Some(reason);
            }
        };
        connection.send_play_packet(packet);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_blocks_pack_the_state_above_x_z_and_y() {
        let position = BlockPos {
            x: -1,
            y: -49,
            z: 18,
        };
        assert_eq!(
            section_block(position, 1),
            1 << 12 | 0xf << 8 | 2 << 4 | 0xf
        );
    }
}
