//! Translate between the edition-neutral player engine and Java Play packets.

use std::io;

use harpas_engine::PlayerSession;
use harpas_engine::player::{self, Input, Movement, Output, Position, Profile};
use harpas_java::Connection;
use harpas_protocol_java::codec::LpVec3;
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
    for packet in connection.take_play_input() {
        let input = match packet {
            Packet::ConfirmTeleportation(p) => Input::ConfirmTeleport(p.teleport_id),
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
            Packet::ClientInformation(p) => {
                let distance = p.view_distance.clamp(2, 32) as u8;
                connection.update_view_distance(distance);
                Input::ViewDistance(distance)
            }
            _ => continue,
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

pub(crate) fn send(connection: &mut Connection, session: &PlayerSession) {
    while let Some(output) = session.try_recv() {
        let packet = match output {
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
            Output::Teleport {
                teleport_id,
                position,
            } => sync(position, teleport_id).into(),
            Output::Disconnect(reason) => {
                connection.disconnect_play(reason);
                break;
            }
        };
        connection.send_play_packet(packet);
    }
}
