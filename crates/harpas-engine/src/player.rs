//! Players, movement checks, and what each player can see.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use harpas_anvil::Chunk;

use crate::light::Light;
use crate::world::ChunkPos;

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
    /// Broadcast an accepted message.
    ChatMessage(Box<ChatMessage>),
    /// The player moved or rotated.
    Movement(Movement),
    /// The client acknowledged a position synchronization.
    ConfirmTeleport(i32),
    /// The client started or stopped flying.
    Flying(bool),
    /// The client's requested view distance in chunks.
    ViewDistance(u8),
    /// The client acknowledged the last chunk batch and reported its desired chunks per tick.
    ChunkBatchReceived(f32),
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
    /// Publish a player's signing session before any messages using it.
    ChatSession {
        /// Player who owns the session.
        uuid: u128,
        /// Verified signing session.
        session: ChatSession,
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
    /// Apply an accepted client view distance in connection order.
    ViewDistance(u8),
    /// Send these ready chunks as one batch. At most one batch per player is in flight.
    ChunkBatch(Vec<VisibleChunk>),
    /// Correct the player's position and wait for the corresponding confirmation.
    Teleport {
        /// Teleport ID to be acknowledged by the client.
        teleport_id: i32,
        /// The last accepted position.
        position: Position,
    },
    /// Disconnect this player with a reason.
    Disconnect(&'static str),
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
}

/// The connection's end of a player's output.
#[derive(Debug)]
pub(crate) struct OutputReceiver {
    receiver: Receiver<Output>,
    queue: Arc<Queue>,
}

/// A channel for a player's output that holds at most [`MAX_QUEUED_OUTPUT`] messages.
pub(crate) fn output_channel() -> (OutputSender, OutputReceiver) {
    let (sender, receiver) = mpsc::channel();
    let queue = Arc::new(Queue::default());
    (
        OutputSender {
            sender,
            queue: Arc::clone(&queue),
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
            return;
        }
        // A dropped receiver means the connection has closed; its departure is queued separately.
        let _ = self.sender.send(output);
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

    /// Takes the messages that are ready.
    #[cfg(test)]
    pub(crate) fn try_iter(&self) -> impl Iterator<Item = Output> + '_ {
        std::iter::from_fn(|| self.try_recv().ok())
    }
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
    pub(crate) awaiting_chunk_batch: bool,
    pub(crate) chunks_per_tick: f32,
    pub(crate) chunk_credit: f32,
    pub(crate) awaiting_teleport: Option<i32>,
    pub(crate) next_teleport: i32,
    pub(crate) flying: bool,
    pub(crate) tick_start: Position,
    pub(crate) disconnecting: bool,
    /// Vanilla's chat spam counter: each message adds 20, and each tick takes one away.
    pub(crate) chat_spam: u32,
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
            awaiting_chunk_batch: false,
            chunks_per_tick: 8.0,
            chunk_credit: 8.0,
            // The Play Login sequence sends teleport 1 before accepting movement.
            awaiting_teleport: Some(1),
            next_teleport: 2,
            flying,
            tick_start: position,
            disconnecting: false,
            chat_spam: 0,
        }
    }

    pub(crate) fn send(&self, output: Output) {
        self.output.send(output);
    }

    pub(crate) fn accepts(&mut self, movement: Movement) -> bool {
        if self.disconnecting {
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
                let teleport_id = self.next_teleport;
                self.next_teleport = self.next_teleport.checked_add(1).unwrap_or(1);
                self.awaiting_teleport = Some(teleport_id);
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
            self.client_floating = !next.on_ground && next.y - self.player.position.y >= -0.03125;
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

    pub(crate) fn check_flight(&mut self) {
        let may_fly = matches!(
            self.player.profile.game_mode,
            GameMode::Creative | GameMode::Spectator
        );
        if self.disconnecting || self.awaiting_teleport.is_some() {
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

    pub(crate) fn can_see(&self, other: &Player) -> bool {
        if other.profile.game_mode == GameMode::Spectator
            && self.player.profile.game_mode != GameMode::Spectator
        {
            return false;
        }
        // A player is tracked within its observer's view distance, up to the player tracking range.
        let range = (f64::from(self.view_distance) * 16.0).min(128.0);
        (self.player.position.x - other.position.x).abs() <= range
            && (self.player.position.z - other.position.z).abs() <= range
    }
}
