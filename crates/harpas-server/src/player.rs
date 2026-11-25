//! Translate between the edition-neutral player engine and Java Play packets.

use std::io;

use harpas_core::block_behavior::Face;
use harpas_core::nbt::List;
use harpas_engine::PlayerSession;
use harpas_engine::block::{BlockPos, DigAction};
use harpas_engine::inventory::{ArmorSlot, ComponentPatch, HashedStack, ItemStack};
use harpas_engine::player::{
    self, DamageKind, Input, Movement, Output, PigVariant, Position, Profile,
};
use harpas_java::{ChatEvent, Connection, PlayEvent};
use harpas_protocol_java::codec::{Holder, LpVec3, Nbt};
use harpas_protocol_java::v26_1::play::{self, clientbound, serverbound};
use std::sync::Arc;

use crate::commands::{self, Commands, ConnectedPeer, Source};

/// `minecraft:player` in minecraft-data's Java 26.1 entity type registry.
const PLAYER_ENTITY_TYPE: i32 = 155;
/// `minecraft:item` in minecraft-data's Java 26.1 entity type registry.
const ITEM_ENTITY_TYPE: i32 = 71;
/// `minecraft:pig` in the Java 26.1 entity type registry.
const PIG_ENTITY_TYPE: i32 = 100;
/// `minecraft:entity.player.attack.sweep` in the Java 26.1 sound event registry.
const SWEEP_SOUND_ID: i32 = 1302;
/// The index of an item entity's stack in its tracked data (`ItemEntity.DATA_ITEM`), after the eight of every
/// entity.
const ITEM_DATA_INDEX: u8 = 8;
/// The tracked data indices for living health, mob flags and pig appearance.
const LIVING_HEALTH_INDEX: u8 = 9;
const MOB_FLAGS_INDEX: u8 = 15;
const PIG_VARIANT_INDEX: u8 = 19;

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

pub(crate) fn receive(
    connection: &mut Connection,
    session: &PlayerSession,
    logic: &harpas_engine::Handle,
    commands: &Commands,
    peer: &ConnectedPeer<'_>,
    chunk_cache: &crate::chunks::Cache,
) -> io::Result<Option<&'static str>> {
    use serverbound::Packet;
    for event in connection.take_play_events() {
        if let PlayEvent::Packet(packet) = &event {
            let command = match packet.as_ref() {
                Packet::ChatCommand(packet) => Some(packet.command.as_str()),
                // The reason is administrative input, never relayed as signed player chat.
                Packet::SignedChatCommand(packet)
                    if packet.command.starts_with("ban ")
                        || packet.command.starts_with("ban-ip ") =>
                {
                    Some(packet.command.as_str())
                }
                _ => None,
            };
            if let Some(command) = command {
                let source = commands.player_source(peer.uuid());
                let (replies, disconnect) = commands.player_reply(logic, command, source)?;
                send_permission_updates(connection, peer);
                if disconnect.is_none()
                    && let Some(reason) = send(connection, session, chunk_cache)
                {
                    return Ok(Some(reason));
                }
                for reply in replies {
                    connection.send_play_packet(reply.into());
                }
                if let Some((key, text)) = disconnect {
                    connection.disconnect_play_translated(key);
                    return Ok(Some(text));
                }
                continue;
            }
        }
        let input = match event {
            PlayEvent::Packet(packet) => match *packet {
                Packet::CommandSuggestionsRequest(request) => {
                    let source = commands.player_source(peer.uuid());
                    if let Some(reply) = commands.suggestions(logic, request, source) {
                        connection.send_play_packet(reply.into());
                    }
                    continue;
                }
                Packet::ConfirmTeleportation(p) => Input::ConfirmTeleport(p.teleport_id),
                Packet::ClientStatus(p) if p.action == play::ClientStatusAction::PerformRespawn => {
                    Input::Respawn
                }
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
                Packet::Attack(p) => Input::Attack(p.entity_id),
                Packet::PlayerInput(p) => Input::SecondaryUse(p.inputs.sneak),
                Packet::PlayerCommand(p) => match p.action {
                    play::PlayerCommandAction::StartSprinting => Input::Sprinting(true),
                    play::PlayerCommandAction::StopSprinting => Input::Sprinting(false),
                    _ => continue,
                },
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
                Packet::UseItem(p) => Input::UseItem {
                    off_hand: p.hand == play::Hand::OffHand,
                    sequence: p.sequence,
                },
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
                        play::PlayerActionStatus::FinishUsingItem => {
                            session
                                .send(Input::StopUsingItem)
                                .map_err(io::Error::other)?;
                            continue;
                        }
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
    Ok(None)
}

pub(crate) fn send_permissions(connection: &mut Connection, uuid: u128, level: i32) {
    connection.send_play_packet(
        clientbound::EntityEvent {
            entity_id: connection.entity_id(),
            // Entity events 24 through 28 update the client's operator level.
            event: 24 + level as i8,
        }
        .into(),
    );
    connection.send_play_packet(commands::tree(Source::Player { uuid, level }).into());
}

pub(crate) fn send_permission_updates(connection: &mut Connection, peer: &ConnectedPeer<'_>) {
    for level in peer.permission_updates() {
        send_permissions(connection, peer.uuid(), level);
    }
}

fn mode_id(mode: player::GameMode) -> i32 {
    match mode {
        player::GameMode::Survival => 0,
        player::GameMode::Creative => 1,
        player::GameMode::Adventure => 2,
        player::GameMode::Spectator => 3,
    }
}

pub(crate) fn java_game_mode(mode: player::GameMode) -> play::GameMode {
    match mode {
        player::GameMode::Survival => play::GameMode::Survival,
        player::GameMode::Creative => play::GameMode::Creative,
        player::GameMode::Adventure => play::GameMode::Adventure,
        player::GameMode::Spectator => play::GameMode::Spectator,
    }
}

fn game_mode_update(uuid: u128, mode: player::GameMode) -> clientbound::PlayerInfoUpdate {
    clientbound::PlayerInfoUpdate {
        updates: play::PlayerInfoUpdates {
            actions: play::PlayerInfoActions {
                update_game_mode: true,
                ..Default::default()
            },
            entries: vec![play::PlayerInfoEntry {
                uuid,
                game_mode: mode_id(mode),
                ..Default::default()
            }],
        },
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

fn pig_metadata(health: f32, variant: PigVariant, no_ai: bool) -> play::EntityMetadata {
    let name = match variant {
        PigVariant::Cold => "minecraft:cold",
        PigVariant::Temperate => "minecraft:temperate",
        PigVariant::Warm => "minecraft:warm",
    };
    let variant_id = harpas_java::registry_names("minecraft:pig_variant")
        .and_then(|entries| entries.iter().position(|&entry| entry == name))
        .expect("vanilla pig variant");
    let mut entries = vec![(LIVING_HEALTH_INDEX, play::MetadataValue::Float(health))];
    if no_ai {
        entries.push((MOB_FLAGS_INDEX, play::MetadataValue::Byte(1)));
    }
    entries.push((
        PIG_VARIANT_INDEX,
        play::MetadataValue::PigVariant(variant_id as i32),
    ));
    play::EntityMetadata { entries }
}

/// Send an entity's spawn and initial data in one bundle.
fn spawn_entity(
    connection: &mut Connection,
    spawn: clientbound::SpawnEntity,
    metadata: play::EntityMetadata,
) -> clientbound::Packet {
    let entity_id = spawn.entity_id;
    connection.send_play_packet(clientbound::BundleDelimiter.into());
    connection.send_play_packet(spawn.into());
    connection.send_play_packet(
        clientbound::SetEntityMetadata {
            entity_id,
            metadata,
        }
        .into(),
    );
    clientbound::BundleDelimiter.into()
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
            Output::MapUpdate {
                id,
                scale,
                locked,
                icons,
                colors,
            } => clientbound::MapData {
                map_id: id,
                scale,
                locked,
                icons: Some(
                    icons
                        .into_iter()
                        .map(|icon| play::MapIcon {
                            kind: match icon.kind {
                                harpas_engine::MapIconKind::Player => 0,
                                harpas_engine::MapIconKind::PlayerOffMap => 6,
                                harpas_engine::MapIconKind::PlayerOffLimits => 7,
                                harpas_engine::MapIconKind::RedX => 26,
                            },
                            x: icon.x,
                            z: icon.z,
                            direction: icon.direction,
                            display_name: None,
                        })
                        .collect(),
                ),
                patch: colors.map_or(play::MapPatch::Unchanged, |colors| {
                    play::MapPatch::Patch(
                        128,
                        play::MapPatchData {
                            rows: 128,
                            x: 0,
                            y: 0,
                            colors: colors.to_vec(),
                        },
                    )
                }),
            }
            .into(),
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
            Output::Login { .. } => unreachable!("Play Login consumes the initial game mode"),
            Output::PlayerGameMode { uuid, mode } => game_mode_update(uuid, mode).into(),
            Output::GameModeChanged { uuid, mode, notify } => {
                let java_mode = java_game_mode(mode);
                // Vanilla sends the new abilities both before the list update and after the game event.
                connection.send_game_mode_abilities(java_mode);
                connection.send_play_packet(game_mode_update(uuid, mode).into());
                connection.send_play_packet(
                    clientbound::GameEvent {
                        event: play::GameEventKind::ChangeGameMode,
                        value: mode_id(mode) as f32,
                    }
                    .into(),
                );
                connection.send_game_mode_abilities(java_mode);
                if notify {
                    connection.send_play_packet(
                        clientbound::SystemChatMessage {
                            content: commands::game_mode_message("gameMode.changed", mode),
                            overlay: false,
                        }
                        .into(),
                    );
                }
                continue;
            }
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
            Output::Health {
                health,
                food,
                saturation,
            } => clientbound::SetHealth {
                health,
                food,
                saturation,
            }
            .into(),
            Output::Absorption { entity_id, amount } => clientbound::SetEntityMetadata {
                entity_id,
                metadata: play::EntityMetadata {
                    entries: vec![(17, play::MetadataValue::Float(amount))],
                },
            }
            .into(),
            Output::MaxAbsorption { entity_id, amount } => clientbound::UpdateAttributes {
                entity_id,
                attributes: vec![play::AttributeProperty {
                    attribute: play::Attribute::MaxAbsorption,
                    base_value: 0.0,
                    modifiers: (amount > 0.0)
                        .then(|| play::AttributeModifier {
                            id: "minecraft:effect.absorption".to_owned(),
                            amount: f64::from(amount),
                            operation: play::AttributeOperation::AddValue,
                        })
                        .into_iter()
                        .collect(),
                }],
            }
            .into(),
            Output::AttackSpeed {
                entity_id,
                base_speed,
                modifier,
            } => clientbound::UpdateAttributes {
                entity_id,
                attributes: vec![play::AttributeProperty {
                    attribute: play::Attribute::AttackSpeed,
                    base_value: base_speed,
                    modifiers: modifier
                        .map(|amount| play::AttributeModifier {
                            id: "minecraft:base_attack_speed".to_owned(),
                            amount,
                            operation: play::AttributeOperation::AddValue,
                        })
                        .into_iter()
                        .collect(),
                }],
            }
            .into(),
            Output::ArmorAttributes {
                entity_id,
                modifiers,
            } => clientbound::UpdateAttributes {
                entity_id,
                attributes: [play::Attribute::Armor, play::Attribute::ArmorToughness]
                    .into_iter()
                    .enumerate()
                    .map(|(index, attribute)| play::AttributeProperty {
                        attribute,
                        base_value: 0.0,
                        modifiers: ["helmet", "chestplate", "leggings", "boots"]
                            .into_iter()
                            .zip(modifiers)
                            .filter_map(|(piece, values)| {
                                Some(play::AttributeModifier {
                                    id: format!("minecraft:armor.{piece}"),
                                    amount: values?[index],
                                    operation: play::AttributeOperation::AddValue,
                                })
                            })
                            .collect(),
                    })
                    .collect(),
            }
            .into(),
            Output::OnFire { entity_id, burning } => clientbound::SetEntityMetadata {
                entity_id,
                metadata: play::EntityMetadata {
                    // The first shared entity flag indicates whether the entity is on fire.
                    entries: vec![(0, play::MetadataValue::Byte(i8::from(burning)))],
                },
            }
            .into(),
            Output::EntityEffect {
                entity_id,
                effect_id,
                effect,
                blend,
            } => match effect {
                Some(effect) => clientbound::EntityEffect {
                    entity_id,
                    effect: effect_id,
                    amplifier: i32::from(effect.amplifier),
                    duration: effect.duration,
                    flags: play::EffectFlags {
                        ambient: effect.ambient,
                        show_particles: effect.show_particles,
                        show_icon: effect.show_icon,
                        blend,
                    },
                }
                .into(),
                None => clientbound::RemoveEntityEffect {
                    entity_id,
                    effect: effect_id,
                }
                .into(),
            },
            Output::UsingItem {
                entity_id,
                off_hand,
            } => clientbound::SetEntityMetadata {
                entity_id,
                metadata: play::EntityMetadata {
                    // Living entity flags: bit 0 is using an item, bit 1 selects the off hand.
                    entries: vec![(
                        8,
                        play::MetadataValue::Byte(
                            off_hand.map_or(0, |off_hand| 1 + 2 * i8::from(off_hand)),
                        ),
                    )],
                },
            }
            .into(),
            Output::FinishUsingItem(entity_id) => clientbound::EntityEvent {
                entity_id,
                event: 9,
            }
            .into(),
            Output::ShieldSound {
                position,
                disabled,
                pitch,
                seed,
            } => clientbound::SoundEffect {
                sound: Holder::Direct(play::SoundEvent {
                    id: if disabled {
                        "minecraft:item.shield.break".into()
                    } else {
                        "minecraft:item.shield.block".into()
                    },
                    fixed_range: None,
                }),
                source: play::SoundSource::Player,
                x: (position.x * 8.0) as i32,
                y: (position.y * 8.0) as i32,
                z: (position.z * 8.0) as i32,
                volume: if disabled { 0.8 } else { 1.0 },
                pitch,
                seed,
            }
            .into(),
            Output::ShieldCooldown(ticks) => clientbound::SetCooldown {
                group: "minecraft:shield".into(),
                ticks,
            }
            .into(),
            Output::Damage { entity_id, kind } => {
                let source = match kind {
                    DamageKind::Fall => "minecraft:fall",
                    DamageKind::Starvation => "minecraft:starve",
                    DamageKind::Magic => "minecraft:magic",
                    DamageKind::Lava => "minecraft:lava",
                    DamageKind::OnFire => "minecraft:on_fire",
                    DamageKind::PlayerAttack(attacker) => {
                        connection.send_melee_damage(entity_id, attacker);
                        continue;
                    }
                };
                connection.send_environmental_damage(entity_id, source);
                continue;
            }
            Output::CriticalHit(entity_id) => clientbound::EntityAnimation {
                entity_id,
                animation: play::Animation::CriticalHit,
            }
            .into(),
            Output::SweepAttackSound { position, seed } => clientbound::SoundEffect {
                sound: Holder::Reference(SWEEP_SOUND_ID),
                source: play::SoundSource::Player,
                x: (position.x * 8.0) as i32,
                y: (position.y * 8.0) as i32,
                z: (position.z * 8.0) as i32,
                volume: 1.0,
                pitch: 1.0,
                seed,
            }
            .into(),
            Output::SweepAttackParticle {
                position: [x, y, z],
                direction: [offset_x, offset_z],
            } => clientbound::LevelParticles {
                override_limiter: false,
                always_show: false,
                x,
                y,
                z,
                offset_x,
                offset_y: 0.0,
                offset_z,
                speed: 0.0,
                count: 0,
                particle: play::Particle {
                    id: play::Particle::SWEEP_ATTACK,
                    options: Vec::new(),
                },
            }
            .into(),
            Output::Death {
                entity_id,
                uuid,
                name,
                kind,
                attacker,
            } => {
                let message = kind.map(|kind| {
                    let translation = match kind {
                        DamageKind::Fall => "death.fell.accident.generic",
                        DamageKind::Starvation => "death.attack.starve",
                        DamageKind::Magic => "death.attack.magic",
                        DamageKind::Lava => "death.attack.lava",
                        DamageKind::OnFire => "death.attack.onFire",
                        DamageKind::PlayerAttack(_) => "death.attack.player",
                    };
                    let mut names = vec![commands::player_name(uuid, name)];
                    if let Some((uuid, name)) = attacker {
                        names.push(commands::player_name(uuid, name));
                    }
                    Nbt::Compound(
                        [
                            ("translate", Nbt::from(translation)),
                            ("with", Nbt::List(List::Compound(names))),
                        ]
                        .into_iter()
                        .collect(),
                    )
                });
                if connection.entity_id() == entity_id {
                    clientbound::CombatDeath {
                        player_id: entity_id,
                        message: message.unwrap_or_else(|| Nbt::from("")),
                    }
                    .into()
                } else if let Some(content) = message {
                    clientbound::SystemChatMessage {
                        content,
                        overlay: false,
                    }
                    .into()
                } else {
                    continue;
                }
            }
            Output::PlayerDied(entity_id) | Output::PigDied(entity_id) => {
                clientbound::EntityEvent {
                    entity_id,
                    event: 3,
                }
                .into()
            }
            Output::Respawn {
                teleport_id,
                position,
                mode,
                previous,
                death_location,
            } => {
                connection.send_respawn(
                    java_game_mode(mode),
                    previous.map(java_game_mode),
                    death_location.map(block_pos),
                    sync(position, teleport_id),
                );
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
            } => spawn_entity(
                connection,
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
                },
                item_metadata(&stack),
            ),
            Output::SpawnPig {
                entity_id,
                uuid,
                position,
                velocity: motion,
                rotation: [yaw, pitch],
                health,
                variant,
                no_ai,
            } => spawn_entity(
                connection,
                clientbound::SpawnEntity {
                    entity_id,
                    uuid,
                    entity_type: PIG_ENTITY_TYPE,
                    x: position[0],
                    y: position[1],
                    z: position[2],
                    velocity: velocity(motion),
                    pitch: angle(pitch),
                    yaw: angle(yaw),
                    head_yaw: angle(yaw),
                    data: 0,
                },
                pig_metadata(health, variant, no_ai),
            ),
            Output::EntitySync {
                entity_id,
                position,
                velocity: motion,
                rotation: [yaw, pitch],
                on_ground,
            } => clientbound::EntityPositionSync {
                entity_id,
                x: position[0],
                y: position[1],
                z: position[2],
                velocity_x: motion[0],
                velocity_y: motion[1],
                velocity_z: motion[2],
                yaw,
                pitch,
                on_ground,
            }
            .into(),
            Output::PigHealth { entity_id, health } => clientbound::SetEntityMetadata {
                entity_id,
                metadata: play::EntityMetadata {
                    entries: vec![(LIVING_HEALTH_INDEX, play::MetadataValue::Float(health))],
                },
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
            Output::OffHandItemBroken(entity_id) => clientbound::EntityEvent {
                entity_id,
                event: 48,
            }
            .into(),
            Output::ArmorBroken { entity_id, slot } => clientbound::EntityEvent {
                entity_id,
                event: match slot {
                    ArmorSlot::Head => 49,
                    ArmorSlot::Chest => 50,
                    ArmorSlot::Legs => 51,
                    ArmorSlot::Feet => 52,
                },
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
            Output::DisconnectTranslated { key, text } => {
                connection.disconnect_play_translated(key);
                return Some(text);
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
