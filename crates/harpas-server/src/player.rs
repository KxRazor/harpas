//! Translate between the edition-neutral player engine and Java Play packets.

use std::io;

use harpas_core::block_behavior::Face;
use harpas_engine::PlayerSession;
use harpas_engine::block::{BlockPos, DigAction};
use harpas_engine::inventory::{ComponentPatch, HashedStack, ItemStack};
use harpas_engine::player::{self, Input, Movement, Output, Position, Profile};
use harpas_java::{ChatEvent, Connection, PlayEvent};
use harpas_protocol_java::codec::{Holder, LpVec3, Nbt};
use harpas_protocol_java::v26_1::play::{self, clientbound, serverbound};
use std::sync::Arc;

/// `minecraft:player` in minecraft-data's Java 26.1 entity type registry.
const PLAYER_ENTITY_TYPE: i32 = 155;
/// `minecraft:item` in minecraft-data's Java 26.1 entity type registry.
const ITEM_ENTITY_TYPE: i32 = 71;
/// The index of an item entity's stack in its tracked data (`ItemEntity.DATA_ITEM`), after the eight of every
/// entity.
const ITEM_DATA_INDEX: u8 = 8;

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
                Packet::SetCreativeModeSlot(p) => Input::SetCreativeSlot {
                    slot: p.slot,
                    stack: engine_stack(p.item.0),
                },
                Packet::SetHeldItem(p) => Input::SelectHotbarSlot(p.slot),
                Packet::PlayerInput(p) => Input::SecondaryUse(p.inputs.sneak),
                Packet::ClickContainer(p) => Input::ClickContainer {
                    container_id: p.container_id,
                    state_id: p.state_id,
                    slot: p.slot,
                    button: p.button,
                    mode: p.mode,
                    changed: p
                        .changed_slots
                        .into_iter()
                        .map(|changed| (changed.slot, changed.item.map(engine_hashed)))
                        .collect(),
                    carried: p.carried.map(engine_hashed),
                },
                Packet::CloseContainer(p) => Input::CloseContainer(p.container_id),
                Packet::UseItemOn(p) => Input::UseItemOn {
                    off_hand: p.hand == play::Hand::OffHand,
                    position: BlockPos {
                        x: p.position.x,
                        y: p.position.y,
                        z: p.position.z,
                    },
                    face: match p.face {
                        play::BlockFace::Bottom => Face::Down,
                        play::BlockFace::Top => Face::Up,
                        play::BlockFace::North => Face::North,
                        play::BlockFace::South => Face::South,
                        play::BlockFace::West => Face::West,
                        play::BlockFace::East => Face::East,
                    },
                    cursor: [p.cursor_x, p.cursor_y, p.cursor_z],
                    sequence: p.sequence,
                },
                Packet::PlayerAction(p) => {
                    let action = match p.status {
                        play::PlayerActionStatus::StartedDigging => DigAction::Start,
                        play::PlayerActionStatus::CancelledDigging => DigAction::Abort,
                        play::PlayerActionStatus::FinishedDigging => DigAction::Finish,
                        play::PlayerActionStatus::DropItemStack => {
                            session
                                .send(Input::DropSelected { all: true })
                                .map_err(io::Error::other)?;
                            continue;
                        }
                        play::PlayerActionStatus::DropItem => {
                            session
                                .send(Input::DropSelected { all: false })
                                .map_err(io::Error::other)?;
                            continue;
                        }
                        // Using and swapping items come later.
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

/// A stack as the engine keeps it. A negative item ID cannot be decoded, so it is never seen here.
fn engine_stack(stack: play::ItemStack) -> ItemStack {
    if stack.is_empty() {
        return ItemStack::empty();
    }
    ItemStack {
        item: stack.item as u32,
        count: stack.count,
        components: Arc::new(ComponentPatch {
            added: stack.components.added,
            removed: stack.components.removed,
            ..ComponentPatch::default()
        }),
    }
}

/// A predicted stack as the engine compares it.
fn engine_hashed(stack: play::HashedStack) -> HashedStack {
    HashedStack {
        item: stack.item as u32,
        count: stack.count,
        added: stack.added,
        removed: stack.removed,
    }
}

/// A stack as it is sent to a Java client.
fn packet_stack(stack: &ItemStack) -> play::ItemStack {
    if stack.is_empty() {
        return play::ItemStack::default();
    }
    play::ItemStack {
        count: stack.count,
        item: stack.item as i32,
        components: play::ComponentPatch {
            added: stack.components.added.clone(),
            removed: stack.components.removed.clone(),
        },
    }
}

fn item_metadata(stack: &ItemStack) -> play::EntityMetadata {
    play::EntityMetadata {
        entries: vec![(
            ITEM_DATA_INDEX,
            play::MetadataValue::ItemStack(packet_stack(stack)),
        )],
    }
}

fn velocity([x, y, z]: [f64; 3]) -> LpVec3 {
    LpVec3 { x, y, z }
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
            Output::SpawnItem {
                entity_id,
                uuid,
                position,
                velocity: motion,
                stack,
            } => {
                // An entity's spawn and its data, applied together (`sendPairingData` in a bundle).
                connection.send_play_packet(clientbound::BundleDelimiter.into());
                connection.send_play_packet(
                    clientbound::SpawnEntity {
                        entity_id,
                        uuid,
                        entity_type: ITEM_ENTITY_TYPE,
                        x: position[0],
                        y: position[1],
                        z: position[2],
                        velocity: velocity(motion),
                        pitch: 0,
                        yaw: 0,
                        head_yaw: 0,
                        data: 0,
                    }
                    .into(),
                );
                connection.send_play_packet(
                    clientbound::SetEntityMetadata {
                        entity_id,
                        metadata: item_metadata(&stack),
                    }
                    .into(),
                );
                clientbound::BundleDelimiter.into()
            }
            Output::EntitySync {
                entity_id,
                position,
                velocity: motion,
                on_ground,
            } => clientbound::EntityPositionSync {
                entity_id,
                x: position[0],
                y: position[1],
                z: position[2],
                velocity_x: motion[0],
                velocity_y: motion[1],
                velocity_z: motion[2],
                yaw: 0.0,
                pitch: 0.0,
                on_ground,
            }
            .into(),
            Output::EntityMove {
                entity_id,
                delta,
                on_ground,
            } => clientbound::UpdateEntityPosition {
                entity_id,
                delta_x: delta[0],
                delta_y: delta[1],
                delta_z: delta[2],
                on_ground,
            }
            .into(),
            Output::EntityVelocity {
                entity_id,
                velocity: motion,
            } => clientbound::SetEntityVelocity {
                entity_id,
                velocity: velocity(motion),
            }
            .into(),
            Output::ItemChanged { entity_id, stack } => clientbound::SetEntityMetadata {
                entity_id,
                metadata: item_metadata(&stack),
            }
            .into(),
            Output::PickupItem {
                collected,
                collector,
                count,
            } => clientbound::PickupItem {
                collected,
                collector,
                count,
            }
            .into(),
            Output::OpenCraftingTable(container_id) => clientbound::OpenScreen {
                container_id,
                // `minecraft:crafting` in the vanilla 26.1 menu registry.
                kind: 12,
                title: Nbt::Compound(harpas_core::nbt::Compound::from_iter([(
                    "translate".to_owned(),
                    Nbt::from("container.crafting"),
                )])),
            }
            .into(),
            Output::OpenChest {
                container_id,
                double,
                title,
            } => clientbound::OpenScreen {
                container_id,
                // `minecraft:generic_9x6` and `minecraft:generic_9x3` in the vanilla 26.1 menu registry.
                kind: if double { 5 } else { 2 },
                title: title.unwrap_or_else(|| {
                    Nbt::Compound(harpas_core::nbt::Compound::from_iter([(
                        "translate".to_owned(),
                        Nbt::from(if double {
                            "container.chestDouble"
                        } else {
                            "container.chest"
                        }),
                    )]))
                }),
            }
            .into(),
            Output::CloseContainer(container_id) => {
                clientbound::CloseContainer { container_id }.into()
            }
            Output::OpenFurnace {
                container_id,
                title,
            } => clientbound::OpenScreen {
                container_id,
                // `minecraft:furnace` in the vanilla 26.1 menu registry.
                kind: 14,
                title: title.unwrap_or_else(|| {
                    Nbt::Compound(harpas_core::nbt::Compound::from_iter([(
                        "translate",
                        Nbt::from("container.furnace"),
                    )]))
                }),
            }
            .into(),
            Output::ContainerProperty {
                container_id,
                property,
                value,
            } => clientbound::SetContainerProperty {
                container_id,
                property,
                value,
            }
            .into(),
            Output::InventoryContent {
                container_id,
                state_id,
                slots,
                carried,
            } => clientbound::SetContainerContent {
                window_id: container_id,
                state_id,
                slots: slots.iter().map(packet_stack).collect(),
                carried: packet_stack(&carried),
            }
            .into(),
            Output::SelectedHotbarSlot(slot) => clientbound::SetHeldItem { slot }.into(),
            Output::SetCarried(stack) => clientbound::SetCursorItem {
                item: packet_stack(&stack),
            }
            .into(),
            Output::MainHandItemBroken(entity_id) => clientbound::EntityEvent {
                entity_id,
                event: 47, // MAINHAND_BREAK in Java 26.1.
            }
            .into(),
            Output::SetSlot {
                container_id,
                state_id,
                slot,
                stack,
            } => clientbound::SetContainerSlot {
                window_id: container_id,
                state_id,
                slot,
                item: packet_stack(&stack),
            }
            .into(),
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
