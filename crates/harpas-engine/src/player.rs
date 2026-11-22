//! Players, movement checks, and what each player can see.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, RandomState};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::Thread;
use std::time::Duration;

use harpas_anvil::Chunk;

use crate::block::{BlockPos, DigAction};
use crate::inventory::{HashedStack, Inventory, ItemStack};
use crate::light::Light;
use crate::world::{ChunkPos, World};
use harpas_core::block_behavior::Face;
use harpas_core::random::LegacyRandom;

/// A signed profile property, such as a Java player's skin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileProperty {
    /// Property name.
    pub name: String,
    /// Property value.
    pub value: String,
    /// Signature provided by the identity service, if present.
    pub signature: Option<String>,
}

/// A player's game mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameMode {
    /// Survival mode.
    Survival,
    /// Creative mode.
    Creative,
    /// Adventure mode.
    Adventure,
    /// Spectator mode.
    Spectator,
}

impl GameMode {
    pub(crate) fn from_id(id: i32) -> Option<Self> {
        match id {
            0 => Some(Self::Survival),
            1 => Some(Self::Creative),
            2 => Some(Self::Adventure),
            3 => Some(Self::Spectator),
            _ => None,
        }
    }

    pub(crate) fn id(self) -> i32 {
        match self {
            Self::Survival => 0,
            Self::Creative => 1,
            Self::Adventure => 2,
            Self::Spectator => 3,
        }
    }
}

/// A damage source the engine can apply to a player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DamageKind {
    /// Landing after a fall.
    Fall,
    /// Running out of food.
    Starvation,
}

/// The visible state of a player's status effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Effect {
    /// Remaining ticks, or -1 for an infinite effect.
    pub duration: i32,
    /// Level of the effect, starting at zero.
    pub amplifier: u8,
    /// Whether the effect is ambient.
    pub ambient: bool,
    /// Whether to display particles.
    pub show_particles: bool,
    /// Whether to display the effect icon.
    pub show_icon: bool,
}

/// A player's identity and game mode. The identity is verified in online mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// The player's persistent UUID.
    pub uuid: u128,
    /// The player's name.
    pub name: String,
    /// Signed identity properties.
    pub properties: Vec<ProfileProperty>,
    /// The player's game mode.
    pub game_mode: GameMode,
}

/// A player's verified chat session, carried to other connections without changing its key material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSession {
    /// Session identifier.
    pub id: u128,
    /// Key expiration in milliseconds since the Unix epoch.
    pub expires_at: i64,
    /// Encoded public key.
    pub key: Vec<u8>,
    /// Identity service's signature of the key.
    pub key_signature: Vec<u8>,
}

/// An accepted player message, including its original signing context when available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    /// Message index in the sender's session.
    pub index: i32,
    /// Original signature, if the connection verified it.
    pub signature: Option<[u8; 256]>,
    /// Original text.
    pub message: String,
    /// Timestamp in milliseconds since the Unix epoch.
    pub timestamp: i64,
    /// Signing salt.
    pub salt: i64,
    /// Previously seen message signatures, in order.
    pub last_seen: Vec<[u8; 256]>,
}

/// The player's position, rotation, and ground state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    /// East-west position, in blocks.
    pub x: f64,
    /// Height of the player's feet, in blocks.
    pub y: f64,
    /// North-south position, in blocks.
    pub z: f64,
    /// Horizontal rotation, in degrees.
    pub yaw: f32,
    /// Vertical rotation, in degrees.
    pub pitch: f32,
    /// Whether the client reports touching the ground.
    pub on_ground: bool,
}

impl Default for Position {
    fn default() -> Self {
        Self {
            x: 0.5,
            y: 65.0,
            z: 0.5,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
        }
    }
}

/// A movement update received from a client.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Movement {
    /// A new position, or none if only the rotation or ground state changed.
    pub position: Option<[f64; 3]>,
    /// New yaw and pitch, or none if the rotation did not change.
    pub rotation: Option<[f32; 2]>,
    /// Whether the client reports touching the ground.
    pub on_ground: bool,
}

/// Input from one player's connection, applied during the packet phase.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// Publish a verified signing session.
    ChatSession(ChatSession),
    /// Publish that the player no longer has a signing session.
    ChatSessionEnded,
    /// Broadcast an accepted message.
    ChatMessage(Box<ChatMessage>),
    /// The player moved or rotated.
    Movement(Movement),
    /// The client acknowledged a position synchronization.
    ConfirmTeleport(i32),
    /// The player requested a respawn from the death screen.
    Respawn,
    /// The client started or stopped flying.
    Flying(bool),
    /// The client's requested view distance in chunks.
    ViewDistance(u8),
    /// The client acknowledged the last chunk batch and reported its desired chunks per tick.
    ChunkBatchReceived(f32),
    /// In Creative, the client put a stack in a slot of the inventory menu, or dropped it with a negative slot.
    SetCreativeSlot {
        /// The slot, from 1 to 45.
        slot: i16,
        /// The stack, or none.
        stack: ItemStack,
    },
    /// The player selected a hotbar slot, 0 to 8.
    SelectHotbarSlot(i16),
    /// Whether the player is holding the secondary-use (sneak) input.
    SecondaryUse(bool),
    /// Whether the player is sprinting.
    Sprinting(bool),
    /// The player tried to use the item in a hand.
    UseItem {
        /// Whether the item is in the off hand.
        off_hand: bool,
        /// The client's sequence number for its prediction.
        sequence: i32,
    },
    /// The player released the use button.
    StopUsingItem,
    /// The player used the item in a hand on a block.
    UseItemOn {
        /// Whether the off hand's item was used.
        off_hand: bool,
        /// The block clicked.
        position: BlockPos,
        /// The face clicked.
        face: Face,
        /// Where on the block, each from 0 to 1.
        cursor: [f32; 3],
        /// The client's sequence number for its prediction.
        sequence: i32,
    },
    /// The player clicked a slot of an open container.
    ClickContainer {
        /// The container: 0 for the inventory.
        container_id: i32,
        /// The last state ID the client received.
        state_id: i32,
        /// The slot, -999 outside the window, -1 for none.
        slot: i16,
        /// The mouse button or hotbar key.
        button: i8,
        /// The click type, 0 to 6.
        mode: i32,
        /// The slots as the client predicted them.
        changed: Vec<(i16, Option<HashedStack>)>,
        /// The cursor's stack as the client predicted it.
        carried: Option<HashedStack>,
    },
    /// The player closed a container screen.
    CloseContainer(i32),
    /// The player dropped one item of the selected hotbar slot, or all of it.
    DropSelected {
        /// Whether the whole stack was dropped.
        all: bool,
    },
    /// The player started, stopped or finished breaking a block. The client predicted the result and waits
    /// for `sequence` to be acknowledged.
    Dig {
        /// What the player did.
        action: DigAction,
        /// The block.
        position: BlockPos,
        /// The client's sequence number for the prediction.
        sequence: i32,
    },
}

/// A chunk ready to send to a player; an absent chunk is the temporary void until generation exists.
#[derive(Debug, Clone, PartialEq)]
pub struct VisibleChunk {
    /// The chunk column's position.
    pub position: ChunkPos,
    /// Existing chunk data, or none when the world has no chunk at this position.
    pub chunk: Option<Arc<Chunk>>,
    /// The light computed for a chunk generated without it, or none for a chunk sent with its saved light.
    pub light: Option<Arc<Light>>,
}

/// The eight neighbours of a chunk, clockwise from the northwest.
pub const NEIGHBORS: [(i32, i32); 8] = [
    (-1, -1),
    (0, -1),
    (1, -1),
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
];

/// A player as seen by another player.
#[derive(Debug, Clone, PartialEq)]
pub struct Player {
    /// Runtime entity ID, unique for this server process.
    pub entity_id: i32,
    /// Identity, including any signed skin properties.
    pub profile: Profile,
    /// Most recent accepted position.
    pub position: Position,
}

/// A message for a player's connection to translate into edition-specific packets.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    /// The loaded game mode for the initial Play packets.
    Login {
        /// The mode to send in Play Login and the player list.
        mode: GameMode,
        /// The mode before the last change, if the player has one.
        previous: Option<GameMode>,
        /// Whether the player is already flying.
        flying: bool,
    },
    /// Update the image and decorations of a map in one of the player's hands.
    MapUpdate {
        /// The item's saved map ID.
        id: i32,
        /// The number of doublings of each pixel's width in blocks.
        scale: i8,
        /// Whether the map is locked in saved data.
        locked: bool,
        /// The current decorations.
        icons: Vec<crate::MapIcon>,
        /// The full 128 × 128 image on the first update.
        colors: Option<Arc<[u8]>>,
    },
    /// Publish a player's signing session before any messages using it.
    ChatSession {
        /// Player who owns the session.
        uuid: u128,
        /// Verified signing session.
        session: ChatSession,
    },
    /// Publish that a player no longer has a signing session.
    ChatSessionEnded {
        /// Player who owned the session.
        uuid: u128,
    },
    /// Deliver a player message, including to its sender.
    ChatMessage {
        /// Sender's identity.
        sender: Profile,
        /// Original message and signing context.
        message: Box<ChatMessage>,
    },
    /// Add a profile to the player list before spawning its entity.
    AddPlayer(Profile),
    /// Synchronize the local player's mode, abilities and player list entry.
    GameModeChanged {
        /// The local player's UUID.
        uuid: u128,
        /// The mode the player switched to.
        mode: GameMode,
        /// Whether the change came from another command source.
        notify: bool,
    },
    /// Update another player's game mode in the player list.
    PlayerGameMode {
        /// The other player's UUID.
        uuid: u128,
        /// The mode the other player switched to.
        mode: GameMode,
    },
    /// Remove a profile from the player list.
    RemovePlayer(u128),
    /// Spawn another player's entity.
    Spawn(Player),
    /// Stop tracking another player's entity.
    RemoveEntity(i32),
    /// Update a tracked entity's position and rotation.
    MoveEntity {
        /// Runtime entity ID.
        entity_id: i32,
        /// Its accepted position.
        position: Position,
    },
    /// The player's own accepted position changed; update the visible chunks if necessary.
    OwnPosition(Position),
    /// Update the local player's hearts and food display.
    Health {
        /// Health in half-hearts.
        health: f32,
        /// Food level.
        food: i32,
        /// Food saturation.
        saturation: f32,
    },
    /// Add, refresh or remove a status effect on the local player.
    EntityEffect {
        /// The player affected.
        entity_id: i32,
        /// Numeric ID in the mob effect registry.
        effect_id: i32,
        /// The new state, or none when it expires.
        effect: Option<Effect>,
        /// Whether the client should blend in the effect.
        blend: bool,
    },
    /// The entity started or stopped using an item.
    UsingItem {
        /// The entity using the item.
        entity_id: i32,
        /// The hand in use, or none when use stops.
        off_hand: Option<bool>,
    },
    /// The local player finished using an item.
    FinishUsingItem(i32),
    /// A player took damage.
    Damage {
        /// The player who was hurt.
        entity_id: i32,
        /// The source of the damage.
        kind: DamageKind,
    },
    /// A player died or joined while dead.
    Death {
        /// The player who died.
        entity_id: i32,
        /// The player's persistent identity, carried in the displayed name.
        uuid: u128,
        /// The name displayed in the death message.
        name: String,
        /// The cause for the death message, if shown.
        kind: Option<DamageKind>,
    },
    /// Play the death animation for a tracked player.
    PlayerDied(i32),
    /// Recreate the local player at the world's spawn after a death.
    Respawn {
        /// Teleport sequence ID.
        teleport_id: i32,
        /// Position and rotation at spawn.
        position: Position,
        /// Current game mode.
        mode: GameMode,
        /// Previous game mode, if the player switched modes.
        previous: Option<GameMode>,
        /// Position of the death in the Overworld.
        death_location: Option<BlockPos>,
    },
    /// Apply an accepted client view distance in connection order.
    ViewDistance(u8),
    /// Send these ready chunks as one batch. At most one batch per player is in flight.
    ChunkBatch(Vec<VisibleChunk>),
    /// Start seeing an item entity.
    SpawnItem {
        /// Runtime entity ID.
        entity_id: i32,
        /// The entity's UUID.
        uuid: u128,
        /// Its position, as last sent to the players who see it.
        position: [f64; 3],
        /// Its velocity, in blocks per tick, as last sent.
        velocity: [f64; 3],
        /// The stack it holds.
        stack: ItemStack,
    },
    /// An entity's full position, velocity and ground state.
    EntitySync {
        /// Runtime entity ID.
        entity_id: i32,
        /// Its position.
        position: [f64; 3],
        /// Its velocity, in blocks per tick.
        velocity: [f64; 3],
        /// Whether it is on the ground.
        on_ground: bool,
    },
    /// An entity moved from the position last sent, in 1/4096 of a block.
    EntityMove {
        /// Runtime entity ID.
        entity_id: i32,
        /// The move on each axis.
        delta: [i16; 3],
        /// Whether it is on the ground.
        on_ground: bool,
    },
    /// An entity's velocity changed.
    EntityVelocity {
        /// Runtime entity ID.
        entity_id: i32,
        /// Its velocity, in blocks per tick.
        velocity: [f64; 3],
    },
    /// The stack an item entity holds changed.
    ItemChanged {
        /// Runtime entity ID.
        entity_id: i32,
        /// The new stack.
        stack: ItemStack,
    },
    /// An entity picked up an item entity, which the client animates.
    PickupItem {
        /// The item entity's runtime ID.
        collected: i32,
        /// The runtime ID of the entity that picked it up.
        collector: i32,
        /// How many it held.
        count: i32,
    },
    /// Open a crafting table screen.
    OpenCraftingTable(i32),
    /// Open the three- or six-row screen for a chest.
    OpenChest {
        /// The player's active menu ID.
        container_id: i32,
        /// Whether the screen combines two chest block entities.
        double: bool,
        /// The chest's custom text component, if it has one.
        title: Option<harpas_core::nbt::Nbt>,
    },
    /// Open the normal furnace screen.
    OpenFurnace {
        /// The player's active menu ID.
        container_id: i32,
        /// The furnace's custom text component, if it has one.
        title: Option<harpas_core::nbt::Nbt>,
    },
    /// The value of a menu property, such as the furnace's progress.
    ContainerProperty {
        /// The player's active menu ID.
        container_id: i32,
        /// The property index.
        property: i16,
        /// The current value.
        value: i16,
    },
    /// Close the active container screen.
    CloseContainer(i32),
    /// Every slot of a menu, on opening or when the client's state is stale.
    InventoryContent {
        /// The menu: 0 for the player's inventory.
        container_id: i32,
        /// The menu's count of changes.
        state_id: i32,
        /// The menu's slots.
        slots: Vec<ItemStack>,
        /// The stack the cursor carries.
        carried: ItemStack,
    },
    /// Restore the selected hotbar slot after loading player data.
    SelectedHotbarSlot(i32),
    /// The stack the player's cursor carries changed.
    SetCarried(ItemStack),
    /// The entity's main-hand item broke.
    MainHandItemBroken(i32),
    /// One slot of a menu changed.
    SetSlot {
        /// The menu: 0 for the player's inventory.
        container_id: i32,
        /// The menu's count of changes.
        state_id: i32,
        /// The slot within that menu.
        slot: i16,
        /// Its stack.
        stack: ItemStack,
    },
    /// Changed blocks of one chunk section the player has, with their current states.
    SectionBlocks(Arc<[(BlockPos, u32)]>),
    /// Send the current state of a block, overriding the client's prediction.
    BlockUpdate {
        /// The block.
        position: BlockPos,
        /// Its current state.
        state: u32,
    },
    /// Confirm the client's block change predictions up to this sequence number.
    AcknowledgeBlockChanges(i32),
    /// New light for some sections of a chunk the player has; the sections left out keep theirs.
    LightUpdate {
        /// The chunk.
        position: ChunkPos,
        /// The sections whose light changed, with both their layers.
        light: Arc<Light>,
    },
    /// Show the cracks of a block another player is breaking.
    BlockDestruction {
        /// The player breaking it, which identifies the cracks.
        entity_id: i32,
        /// The block.
        position: BlockPos,
        /// 0 to 9; any other value removes the cracks.
        stage: i8,
    },
    /// Play a world event, such as the particles and sound of a broken block.
    WorldEvent {
        /// Vanilla's event type, such as 2001 for a broken block.
        event: i32,
        /// Where it happens.
        position: BlockPos,
        /// The event's data, such as the broken block's state.
        data: i32,
    },
    /// Correct the player's position and wait for the corresponding confirmation.
    Teleport {
        /// Teleport ID to be acknowledged by the client.
        teleport_id: i32,
        /// The last accepted position.
        position: Position,
    },
    /// Disconnect this player with a reason.
    Disconnect(&'static str),
    /// Disconnect this player with a translated reason.
    DisconnectTranslated {
        /// Translation key to send to the client.
        key: &'static str,
        /// Plain reason to log on the server.
        text: &'static str,
    },
}

/// The most messages that may wait for a player's connection to take them.
///
/// A connection takes its messages every tick, so only one that has stopped writing to its client, or writes
/// more slowly than the server produces, gets this far behind. At 20 ticks a second it is 30 seconds of 100
/// messages a tick.
pub(crate) const MAX_QUEUED_OUTPUT: usize = 60_000;

/// The reason a player whose connection fell [`MAX_QUEUED_OUTPUT`] messages behind is disconnected with, as a
/// connection that stops answering is in vanilla.
pub(crate) const TOO_SLOW: &str = "Timed out";

#[derive(Debug, Default)]
struct Queue {
    len: AtomicUsize,
    overflowed: AtomicBool,
}

/// The logic thread's end of a player's output.
#[derive(Debug)]
pub(crate) struct OutputSender {
    sender: Sender<Output>,
    queue: Arc<Queue>,
    /// The connection's thread, which parks while it has nothing to do.
    connection: Option<Thread>,
}

/// The connection's end of a player's output.
#[derive(Debug)]
pub(crate) struct OutputReceiver {
    receiver: Receiver<Output>,
    queue: Arc<Queue>,
}

/// A channel for a player's output that holds at most [`MAX_QUEUED_OUTPUT`] messages.
#[cfg(test)]
pub(crate) fn output_channel() -> (OutputSender, OutputReceiver) {
    channel(None)
}

/// A channel like [`output_channel`]'s that also unparks `connection` for every message, so that a connection
/// thread parked while its player is idle sends the message at once.
pub(crate) fn output_channel_waking(connection: Thread) -> (OutputSender, OutputReceiver) {
    channel(Some(connection))
}

fn channel(connection: Option<Thread>) -> (OutputSender, OutputReceiver) {
    let (sender, receiver) = mpsc::channel();
    let queue = Arc::new(Queue::default());
    (
        OutputSender {
            sender,
            queue: Arc::clone(&queue),
            connection,
        },
        OutputReceiver { receiver, queue },
    )
}

impl OutputSender {
    /// Queues `output`. A message over the limit is dropped and the queue overflows for good: the receiver
    /// then gives only a disconnection.
    fn send(&self, output: Output) {
        if self.overflowed() {
            return;
        }
        if self.queue.len.fetch_add(1, Ordering::Relaxed) >= MAX_QUEUED_OUTPUT {
            self.queue.overflowed.store(true, Ordering::Relaxed);
        } else {
            // A dropped receiver means the connection has closed; its departure is queued separately.
            let _ = self.sender.send(output);
        }
        // After the message is queued, so the connection finds it. Unparking a running thread costs no
        // system call.
        if let Some(connection) = &self.connection {
            connection.unpark();
        }
    }

    /// Whether the connection fell too far behind, so that the player must leave.
    pub(crate) fn overflowed(&self) -> bool {
        self.queue.overflowed.load(Ordering::Relaxed)
    }
}

impl OutputReceiver {
    /// Takes the next message, if one is ready. After an overflow that is a disconnection, whatever else is
    /// queued.
    pub(crate) fn try_recv(&self) -> Result<Output, TryRecvError> {
        if self.queue.overflowed.load(Ordering::Relaxed) {
            return Ok(Output::Disconnect(TOO_SLOW));
        }
        let output = self.receiver.try_recv()?;
        self.queue.len.fetch_sub(1, Ordering::Relaxed);
        Ok(output)
    }

    pub(crate) fn recv_timeout(&self, timeout: Duration) -> Result<Output, mpsc::RecvTimeoutError> {
        if self.queue.overflowed.load(Ordering::Relaxed) {
            return Ok(Output::Disconnect(TOO_SLOW));
        }
        let output = self.receiver.recv_timeout(timeout)?;
        self.queue.len.fetch_sub(1, Ordering::Relaxed);
        Ok(output)
    }

    /// Takes the messages that are ready.
    #[cfg(test)]
    pub(crate) fn try_iter(&self) -> impl Iterator<Item = Output> + '_ {
        std::iter::from_fn(|| self.try_recv().ok())
    }
}

/// The chunks a player sees, kept from tick to tick while the player stays in its chunk.
#[derive(Debug, Default)]
pub(crate) struct View {
    /// The chunk the player is in and its view distance, or none for a player that sees nothing: one that
    /// is leaving, or whose view has not been computed yet.
    pub(crate) key: Option<(ChunkPos, u8)>,
    /// The chunks of the view, nearest first.
    pub(crate) chunks: Vec<ChunkPos>,
}

pub(crate) struct PlayerState {
    pub(crate) player: Player,
    pub(crate) chat_session: Option<ChatSession>,
    pub(crate) output: OutputSender,
    pub(crate) view_distance: u8,
    pub(crate) max_view_distance: u8,
    pub(crate) allow_flight: bool,
    pub(crate) client_floating: bool,
    pub(crate) floating_ticks: u32,
    pub(crate) tracked: HashSet<i32>,
    pub(crate) sent_chunks: HashSet<ChunkPos>,
    pub(crate) view: View,
    pub(crate) awaiting_chunk_batch: bool,
    pub(crate) chunks_per_tick: f32,
    pub(crate) chunk_credit: f32,
    pub(crate) awaiting_teleport: Option<i32>,
    pub(crate) next_teleport: i32,
    pub(crate) flying: bool,
    pub(crate) previous_game_mode: Option<GameMode>,
    pub(crate) vitals: crate::survival::Vitals,
    pub(crate) peaceful_food_ticks: u8,
    pub(crate) sprinting: bool,
    pub(crate) food_use: Option<crate::survival::FoodUse>,
    pub(crate) hunger_effect: Option<crate::survival::HungerEffect>,
    pub(crate) random: LegacyRandom,
    pub(crate) fall_distance: f64,
    pub(crate) last_damage: f32,
    pub(crate) hurt_cooldown: u8,
    pub(crate) death_location: Option<BlockPos>,
    pub(crate) force_game_mode_on_login: Option<bool>,
    pub(crate) tick_start: Position,
    pub(crate) disconnecting: bool,
    /// Vanilla's chat spam counter: each message adds 20, and each tick takes one away.
    pub(crate) chat_spam: u32,
    /// The highest block change sequence to acknowledge in this tick (`ackBlockChangesUpTo`).
    pub(crate) acknowledge_block_changes: Option<i32>,
    pub(crate) digging: Digging,
    pub(crate) pending_map_chest: Option<crate::maps::PendingChestAction>,
    pub(crate) sent_maps: HashMap<i32, Vec<crate::MapIcon>>,
    pub(crate) inventory: Inventory,
    pub(crate) data_ready: bool,
    pub(crate) saved_inventory: Option<Arc<crate::player_data::PlayerInventory>>,
    pub(crate) block_menu: Option<crate::block_menu::BlockMenu>,
    pub(crate) next_container_id: i32,
    pub(crate) secondary_use: bool,
}

/// The block a player is breaking, with the fields of vanilla's `ServerPlayerGameMode`.
#[derive(Debug)]
pub(crate) struct Digging {
    /// The player's ticks (`gameTicks`).
    pub(crate) game_ticks: i32,
    /// The tick the player started breaking the block (`destroyProgressStart`).
    pub(crate) destroy_progress_start: i32,
    pub(crate) is_destroying: bool,
    /// The block last started, kept after it stops being broken.
    pub(crate) destroy_pos: BlockPos,
    /// Whether the client finished breaking a block before the server's progress reached it, so that the
    /// server breaks it once its own progress does.
    pub(crate) has_delayed_destroy: bool,
    pub(crate) delayed_destroy_pos: BlockPos,
    pub(crate) delayed_tick_start: i32,
    /// The stage of cracks last shown to the other players.
    pub(crate) last_sent_state: i32,
}

impl Default for Digging {
    fn default() -> Self {
        let origin = BlockPos { x: 0, y: 0, z: 0 };
        Self {
            game_ticks: 0,
            destroy_progress_start: 0,
            is_destroying: false,
            destroy_pos: origin,
            has_delayed_destroy: false,
            delayed_destroy_pos: origin,
            delayed_tick_start: 0,
            last_sent_state: -1,
        }
    }
}

const CHAT_SPAM_PER_MESSAGE: u32 = 20;
const CHAT_SPAM_LIMIT: u32 = 200;

impl PlayerState {
    pub(crate) fn new(
        entity_id: i32,
        profile: Profile,
        view_distance: u8,
        allow_flight: bool,
        output: OutputSender,
    ) -> Self {
        let position = Position::default();
        let flying = profile.game_mode == GameMode::Spectator;
        let seed = RandomState::new().hash_one((profile.uuid, entity_id)) as i64;
        Self {
            player: Player {
                entity_id,
                profile,
                position,
            },
            chat_session: None,
            output,
            view_distance: view_distance.clamp(2, 32),
            max_view_distance: view_distance.clamp(2, 32),
            allow_flight,
            client_floating: false,
            floating_ticks: 0,
            tracked: HashSet::new(),
            sent_chunks: HashSet::new(),
            view: View::default(),
            awaiting_chunk_batch: false,
            chunks_per_tick: 8.0,
            chunk_credit: 8.0,
            // The Play Login sequence sends teleport 1 before accepting movement.
            awaiting_teleport: Some(1),
            next_teleport: 2,
            flying,
            previous_game_mode: None,
            vitals: crate::survival::Vitals::default(),
            peaceful_food_ticks: 0,
            sprinting: false,
            food_use: None,
            hunger_effect: None,
            random: LegacyRandom::new(seed),
            fall_distance: 0.0,
            last_damage: 0.0,
            hurt_cooldown: 0,
            death_location: None,
            force_game_mode_on_login: None,
            tick_start: position,
            disconnecting: false,
            chat_spam: 0,
            acknowledge_block_changes: None,
            digging: Digging::default(),
            pending_map_chest: None,
            sent_maps: HashMap::new(),
            inventory: Inventory::default(),
            data_ready: true,
            saved_inventory: None,
            block_menu: None,
            next_container_id: 0,
            secondary_use: false,
        }
    }

    pub(crate) fn send(&self, output: Output) {
        if self.data_ready
            || matches!(
                output,
                Output::Disconnect(_) | Output::DisconnectTranslated { .. }
            )
        {
            self.output.send(output);
        }
    }

    /// Applies a movement packet and returns whether the player moved. `world` tells whether the player is
    /// next to a block, which is not floating; without a world there are no blocks.
    pub(crate) fn accepts(&mut self, movement: Movement, world: Option<&World>) -> bool {
        if self.disconnecting || self.is_dead() {
            return false;
        }
        if movement
            .position
            .is_some_and(|p| p.iter().any(|n| !n.is_finite()))
            || movement
                .rotation
                .is_some_and(|r| r.iter().any(|n| !n.is_finite()))
        {
            self.disconnecting = true;
            self.send(Output::Disconnect("Invalid player movement"));
            return false;
        }
        if self.awaiting_teleport.is_some() {
            return false;
        }

        let mut next = self.player.position;
        if let Some([x, y, z]) = movement.position {
            next.x = x.clamp(-30_000_000.0, 30_000_000.0);
            next.y = y.clamp(-20_000_000.0, 20_000_000.0);
            next.z = z.clamp(-30_000_000.0, 30_000_000.0);
            let dx = next.x - self.tick_start.x;
            let dy = next.y - self.tick_start.y;
            let dz = next.z - self.tick_start.z;
            if dx * dx + dy * dy + dz * dz > 100.0 {
                let teleport_id = self.begin_teleport();
                self.send(Output::Teleport {
                    teleport_id,
                    position: self.player.position,
                });
                return false;
            }
        }
        if let Some([yaw, pitch]) = movement.rotation {
            next.yaw = yaw;
            next.pitch = pitch;
        }
        next.on_ground = movement.on_ground;
        if movement.position.is_some() {
            // Vanilla's `clientIsFloating`: not falling, off the ground and touching nothing but air, so that
            // swimming, climbing and standing on a block the client reports as not ground do not count.
            self.client_floating = !next.on_ground
                && next.y - self.player.position.y >= -0.03125
                && world.is_none_or(|world| world.no_blocks_around(&next));
        } else if next.on_ground {
            self.client_floating = false;
        }
        if next == self.player.position {
            return false;
        }
        self.player.position = next;
        self.send(Output::OwnPosition(next));
        true
    }

    pub(crate) fn acknowledge_block_changes(&mut self, sequence: i32) -> bool {
        if sequence < 0 {
            // Vanilla's ackBlockChangesUpTo throws on a negative sequence.
            self.disconnecting = true;
            self.send(Output::Disconnect("Network Protocol Error"));
            return false;
        }
        self.acknowledge_block_changes = Some(
            self.acknowledge_block_changes
                .map_or(sequence, |acknowledged| acknowledged.max(sequence)),
        );
        true
    }

    pub(crate) fn begin_teleport(&mut self) -> i32 {
        let id = self.next_teleport;
        self.next_teleport = id.checked_add(1).unwrap_or(1);
        self.awaiting_teleport = Some(id);
        self.fall_distance = 0.0;
        id
    }

    pub(crate) fn check_flight(&mut self) {
        let may_fly = matches!(
            self.player.profile.game_mode,
            GameMode::Creative | GameMode::Spectator
        );
        if self.disconnecting || self.is_dead() || self.awaiting_teleport.is_some() {
            return;
        }
        if self.allow_flight || may_fly || !self.client_floating {
            self.floating_ticks = 0;
            return;
        }
        self.floating_ticks += 1;
        if self.floating_ticks > 80 {
            self.disconnecting = true;
            self.send(Output::Disconnect("Flying is not enabled on this server"));
        }
    }

    /// Counts a chat message that was just delivered, and disconnects a player who sends them too fast, as
    /// vanilla's `detectRateSpam`: ten messages at once, or more than one a second for long enough.
    pub(crate) fn count_chat_message(&mut self) {
        self.chat_spam += CHAT_SPAM_PER_MESSAGE;
        if self.chat_spam >= CHAT_SPAM_LIMIT && !self.disconnecting {
            self.disconnecting = true;
            self.send(Output::Disconnect("Kicked for spamming"));
        }
    }

    /// Lets the chat spam counter go down by one each tick.
    pub(crate) fn tick_chat(&mut self) {
        self.chat_spam = self.chat_spam.saturating_sub(1);
    }

    /// The position of the player's eyes: a standing player's, until sneaking and swimming poses exist.
    pub(crate) fn eye_position(&self) -> [f64; 3] {
        let position = &self.player.position;
        [position.x, position.y + 1.62, position.z]
    }

    /// How far the player reaches blocks: the `block_interaction_range` attribute, which is 4.5 and has 0.5
    /// more in Creative.
    pub(crate) fn block_interaction_range(&self) -> f64 {
        if self.player.profile.game_mode == GameMode::Creative {
            5.0
        } else {
            4.5
        }
    }

    /// Applies the player's default mining speed modifiers to the held item's speed.
    pub(crate) fn destroy_speed(&self, mut speed: f32, eye_in_water: bool) -> f32 {
        if eye_in_water {
            speed *= 0.2;
        }
        if !self.player.position.on_ground {
            speed /= 5.0;
        }
        speed
    }

    /// Whether the player has the chunk but not all eight chunks around it (`ChunkMap.isChunkOnTrackedBorder`).
    pub(crate) fn on_tracked_border(&self, chunk: ChunkPos) -> bool {
        self.sent_chunks.contains(&chunk)
            && NEIGHBORS.iter().any(|(dx, dz)| {
                !self.sent_chunks.contains(&ChunkPos {
                    x: chunk.x + dx,
                    z: chunk.z + dz,
                })
            })
    }

    /// Whether this player sees a player in `mode` at `position`.
    pub(crate) fn can_see(&self, mode: GameMode, position: Position) -> bool {
        if mode == GameMode::Spectator && self.player.profile.game_mode != GameMode::Spectator {
            return false;
        }
        // A player is tracked within its observer's view distance, up to the player tracking range.
        let range = (f64::from(self.view_distance) * 16.0).min(128.0);
        (self.player.position.x - position.x).abs() <= range
            && (self.player.position.z - position.z).abs() <= range
    }
}
