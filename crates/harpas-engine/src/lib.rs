//! Harpas engine: vanilla tick order on one logic thread
//!
//! Design: `docs/engine-core.md`.

pub mod block;
mod block_menu;
mod chest;
mod container_storage;
pub mod entity;
pub mod entity_data;
pub(crate) mod fluid;
mod furnace;
pub mod inventory;
pub mod item_data;
pub mod light;
pub mod loot;
mod maps;
mod menu;
pub mod mining;
pub mod neighbor;
mod place;
pub mod player;
pub mod player_data;
pub mod recipe;
mod survival;
pub(crate) mod ticks;
mod tracking;
pub mod world;
pub mod worldgen;

pub use maps::{MapData, MapIcon, MapIconKind};

use std::collections::HashSet;
use std::fmt;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use harpas_worldgen::shape_update::ShapeUpdates;

const TICK_INTERVAL: Duration = Duration::from_millis(50);
const MAX_LAG: Duration = Duration::from_secs(2);
const AUTOSAVE_INTERVAL: u64 = 6_000;
const CHUNKS_PER_BATCH: usize = 8;

/// A handle for sending connection events to the logic thread.
#[derive(Clone, Debug)]
pub struct Handle {
    events: Sender<Event>,
    online_players: Arc<AtomicU32>,
    has_world: bool,
}

/// The runtime entity IDs of this server process, players' and other entities' alike, as vanilla's
/// `Entity.ENTITY_COUNTER`; they are never reused.
static ENTITY_COUNTER: AtomicI32 = AtomicI32::new(1);

/// Reserves a runtime entity ID, or none once they ran out.
pub(crate) fn next_entity_id() -> Option<i32> {
    ENTITY_COUNTER
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .ok()
}

impl Handle {
    /// Whether this engine supplies world chunks to connections.
    pub fn has_world(&self) -> bool {
        self.has_world
    }

    /// Reserves a runtime entity ID; IDs are never reused during this server process.
    pub fn next_entity_id(&self) -> Option<i32> {
        next_entity_id()
    }

    /// Registers a connection in Play. Dropping the returned session queues its departure.
    pub fn player_joined(
        &self,
        entity_id: i32,
        profile: player::Profile,
        view_distance: u8,
        allow_flight: bool,
    ) -> Result<PlayerSession, LogicStopped> {
        self.player_joined_with_view(
            entity_id,
            profile,
            view_distance,
            view_distance,
            allow_flight,
        )
    }

    /// Registers a player with its initial client view distance and the server's maximum.
    ///
    /// The logic thread unparks the calling thread for every message it queues for the player, so the thread
    /// can [park](thread::park_timeout) while it waits and still take the message at once.
    pub fn player_joined_with_view(
        &self,
        entity_id: i32,
        profile: player::Profile,
        max_view_distance: u8,
        initial_view_distance: u8,
        allow_flight: bool,
    ) -> Result<PlayerSession, LogicStopped> {
        self.queue_join(
            entity_id,
            profile,
            max_view_distance,
            initial_view_distance,
            allow_flight,
            None,
        )
    }

    /// Registers a player whose connection waits for the loaded game mode before sending Play Login.
    /// The first output is the initial mode or the reason the player could not join.
    pub fn player_joined_for_login(
        &self,
        entity_id: i32,
        profile: player::Profile,
        max_view_distance: u8,
        initial_view_distance: u8,
        allow_flight: bool,
        force_game_mode: bool,
    ) -> Result<PlayerSession, LogicStopped> {
        self.queue_join(
            entity_id,
            profile,
            max_view_distance,
            initial_view_distance,
            allow_flight,
            Some(force_game_mode),
        )
    }

    fn queue_join(
        &self,
        entity_id: i32,
        profile: player::Profile,
        max_view_distance: u8,
        initial_view_distance: u8,
        allow_flight: bool,
        force_game_mode_on_login: Option<bool>,
    ) -> Result<PlayerSession, LogicStopped> {
        let (output, receiver) = player::output_channel_waking(thread::current());
        self.events
            .send(Event::Joined {
                entity_id,
                profile,
                view_distance: max_view_distance,
                initial_view_distance,
                allow_flight,
                output,
                force_game_mode_on_login,
            })
            .map_err(|_| LogicStopped)?;
        Ok(PlayerSession {
            entity_id,
            events: self.events.clone(),
            output: receiver,
        })
    }

    /// The number of connections in Play as of the last completed packet phase.
    pub fn online_players(&self) -> u32 {
        self.online_players.load(Ordering::Relaxed)
    }

    /// The names of the players in Play, in join order, as of the next packet phase.
    pub fn player_names(&self) -> Result<Vec<String>, LogicStopped> {
        Ok(self.players()?.into_iter().map(|(_, name)| name).collect())
    }

    /// The UUIDs and names of the players in Play, in join order, as of the next packet phase.
    pub fn players(&self) -> Result<Vec<(u128, String)>, LogicStopped> {
        Ok(self
            .player_list()?
            .into_iter()
            .map(|player| (player.uuid, player.name))
            .collect())
    }

    /// The players in Play and their positions, in join order, as of the next connection phase.
    pub fn player_list(&self) -> Result<Vec<PlayerListEntry>, LogicStopped> {
        let (sender, receiver) = mpsc::channel();
        self.events
            .send(Event::ListPlayers(sender))
            .map_err(|_| LogicStopped)?;
        receiver.recv().map_err(|_| LogicStopped)
    }

    /// Changes connected players' game modes on the logic thread and returns the players that changed.
    pub fn set_game_mode(
        &self,
        targets: Vec<u128>,
        mode: player::GameMode,
        source: Option<u128>,
    ) -> Result<Vec<(u128, String)>, LogicStopped> {
        let (sender, receiver) = mpsc::channel();
        self.events
            .send(Event::SetGameMode {
                targets,
                mode,
                source,
                reply: sender,
            })
            .map_err(|_| LogicStopped)?;
        receiver.recv().map_err(|_| LogicStopped)
    }

    /// Disconnects players outside the allowed UUIDs at the next connection phase, using a translated reason.
    pub fn disconnect_players_except(
        &self,
        allowed: HashSet<u128>,
        key: &'static str,
        text: &'static str,
    ) -> Result<(), LogicStopped> {
        self.events
            .send(Event::DisconnectPlayers {
                uuids: allowed,
                except: true,
                key,
                text,
            })
            .map_err(|_| LogicStopped)
    }

    /// Disconnects the given UUIDs at the next connection phase, using a translated reason.
    pub fn disconnect_players(
        &self,
        uuids: HashSet<u128>,
        key: &'static str,
        text: &'static str,
    ) -> Result<(), LogicStopped> {
        self.events
            .send(Event::DisconnectPlayers {
                uuids,
                except: false,
                key,
                text,
            })
            .map_err(|_| LogicStopped)
    }

    /// Stops the logic thread at the next connection phase, disconnecting every player with `reason`.
    pub fn shutdown(&self, reason: &'static str) -> Result<(), LogicStopped> {
        self.events
            .send(Event::Shutdown(reason))
            .map_err(|_| LogicStopped)
    }
}

/// A connected player as seen by administrative commands.
pub struct PlayerListEntry {
    /// The authenticated or offline-mode UUID.
    pub uuid: u128,
    /// The player's login name.
    pub name: String,
    /// The position of the player's feet, in block coordinates.
    pub position: [f64; 3],
}

/// A connection in Play; dropping it tells the logic thread that the connection has ended.
#[derive(Debug)]
pub struct PlayerSession {
    entity_id: i32,
    events: Sender<Event>,
    output: player::OutputReceiver,
}

impl PlayerSession {
    /// Queues input from this player for the next packet phase.
    pub fn send(&self, input: player::Input) -> Result<(), LogicStopped> {
        self.events
            .send(Event::Input {
                entity_id: self.entity_id,
                input,
            })
            .map_err(|_| LogicStopped)
    }

    /// Takes the next message for this connection, if one is ready.
    pub fn try_recv(&self) -> Option<player::Output> {
        self.output.try_recv().ok()
    }

    /// Waits for a message from the logic thread, up to `timeout`.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<player::Output, RecvTimeoutError> {
        self.output.recv_timeout(timeout)
    }
}

impl Drop for PlayerSession {
    fn drop(&mut self) {
        // If the logic thread has stopped, there is no state left to update.
        let _ = self.events.send(Event::Left(self.entity_id));
    }
}

/// The logic thread is no longer receiving events.
#[derive(Debug)]
pub struct LogicStopped;

impl fmt::Display for LogicStopped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("logic thread stopped")
    }
}

impl std::error::Error for LogicStopped {}

/// Starts the single logic thread. It stops on [`Handle::shutdown`], or once all handles and player sessions
/// have been dropped. The thread's result reports any failure to finish saving the world.
pub fn start() -> io::Result<(Handle, JoinHandle<io::Result<()>>)> {
    start_game(None)
}

/// Starts the logic thread with an existing world's overworld and the server's ordered biome registry.
///
/// `data` holds the game's rules read from the vanilla data pack.
pub fn start_with_world(
    world_dir: &std::path::Path,
    biomes: &[&str],
    data: GameData,
) -> io::Result<(Handle, JoinHandle<io::Result<()>>)> {
    start_game(with_data(world::World::open(world_dir, biomes)?, data))
}

/// The game's rules read from the vanilla data pack. A world without them still runs, without what they drive.
#[derive(Debug, Clone, Default)]
pub struct GameData {
    /// The blocks' shape rules; without them, block changes leave their neighbors as they are.
    pub shapes: Option<Arc<ShapeUpdates>>,
    /// Loot tables for broken blocks and supported chests.
    pub loot: Option<Arc<loot::LootTables>>,
    /// The crafting recipes; without them, nothing is crafted.
    pub recipes: Option<Arc<recipe::Recipes>>,
    /// Mining speed and harvest rules for pickaxes, resolved from the installed block tags.
    pub mining: Option<Arc<mining::MiningRules>>,
    /// Membership in the climbable block tag, indexed by block state ID.
    pub climbable_blocks: Vec<bool>,
    /// Saved item component codecs, using the owner's generated component registry.
    pub items: Option<Arc<item_data::ItemCodec>>,
}

/// Starts the logic thread with a generator for chunks missing from an existing or newly initialized world.
/// The generator executes on chunk workers and its newly generated chunks are persisted on unload or shutdown.
pub fn start_with_world_generator(
    world_dir: &std::path::Path,
    biomes: &[&str],
    generator: Arc<world::ChunkGenerator>,
) -> io::Result<(Handle, JoinHandle<io::Result<()>>)> {
    start_game(Some(world::World::open_with_generator(
        world_dir, biomes, generator,
    )?))
}

/// Starts the logic thread with a source for chunks missing from an existing or newly initialized world, or
/// saved part way through generation. The source executes on chunk workers and the chunks it returns are
/// persisted on unload or shutdown. `data` is as for [`start_with_world`].
pub fn start_with_world_source(
    world_dir: &std::path::Path,
    biomes: &[&str],
    source: Arc<dyn world::ChunkSource>,
    data: GameData,
) -> io::Result<(Handle, JoinHandle<io::Result<()>>)> {
    start_game(with_data(
        world::World::open_with_source(world_dir, biomes, source)?,
        data,
    ))
}

fn with_data(mut world: world::World, data: GameData) -> Option<world::World> {
    if let Some(shapes) = data.shapes {
        world.set_shape_updates(shapes);
    }
    if let Some(loot) = data.loot {
        world.set_loot_tables(loot);
    }
    world.recipes = data.recipes;
    world.mining = data.mining;
    world.climbable_blocks = data.climbable_blocks;
    if let Some(items) = data.items {
        world.set_item_codec(items);
    }
    Some(world)
}

fn start_game(world: Option<world::World>) -> io::Result<(Handle, JoinHandle<io::Result<()>>)> {
    let (sender, receiver) = mpsc::channel();
    let online_players = Arc::new(AtomicU32::new(0));
    let handle = Handle {
        events: sender,
        online_players: Arc::clone(&online_players),
        has_world: world.is_some(),
    };
    let thread = thread::Builder::new()
        .name("harpas-logic".to_owned())
        .spawn(move || run(receiver, online_players, world))?;
    Ok((handle, thread))
}

#[derive(Debug)]
enum Event {
    Joined {
        entity_id: i32,
        profile: player::Profile,
        view_distance: u8,
        initial_view_distance: u8,
        allow_flight: bool,
        output: player::OutputSender,
        force_game_mode_on_login: Option<bool>,
    },
    Input {
        entity_id: i32,
        input: player::Input,
    },
    Left(i32),
    ListPlayers(Sender<Vec<PlayerListEntry>>),
    SetGameMode {
        targets: Vec<u128>,
        mode: player::GameMode,
        source: Option<u128>,
        reply: Sender<Vec<(u128, String)>>,
    },
    DisconnectPlayers {
        uuids: HashSet<u128>,
        except: bool,
        key: &'static str,
        text: &'static str,
    },
    Shutdown(&'static str),
    #[cfg(test)]
    Panic,
}

/// Whether `player` is closer than `range` to the corner of the block at `pos`, as vanilla measures who hears
/// and sees what happens to a block.
fn within(player: &player::PlayerState, pos: block::BlockPos, range: f64) -> bool {
    let p = &player.player.position;
    let (dx, dy, dz) = (
        f64::from(pos.x) - p.x,
        f64::from(pos.y) - p.y,
        f64::from(pos.z) - p.z,
    );
    dx * dx + dy * dy + dz * dz < range * range
}

/// Plays world events for every player within 64 blocks of each (`ServerLevel.levelEvent` with no source).
fn send_level_events(players: &[player::PlayerState], events: &[neighbor::LevelEvent]) {
    for event in events {
        for player in players {
            if !player.disconnecting && within(player, event.pos, 64.0) {
                player.send(player::Output::WorldEvent {
                    event: event.event,
                    position: event.pos,
                    data: event.data,
                });
            }
        }
    }
}

/// The next tick's target time. Short delays are caught up; a delay over two seconds resets the target.
struct Schedule {
    next: Instant,
}

impl Schedule {
    fn new(now: Instant) -> Self {
        Self {
            next: now + TICK_INTERVAL,
        }
    }

    fn take_due(&mut self, now: Instant) -> bool {
        if now < self.next {
            return false;
        }
        if now.duration_since(self.next) > MAX_LAG {
            self.next = now + TICK_INTERVAL;
        } else {
            self.next += TICK_INTERVAL;
        }
        true
    }
}

/// The phases that exist in vanilla's tick order.
trait TickWork {
    fn functions(&mut self) {}
    fn dimensions(&mut self) {}
    fn connections(&mut self);
    fn autosave(&mut self) {}
}

fn tick(work: &mut impl TickWork, number: u64) {
    work.functions();
    work.dimensions();
    work.connections();
    if number.is_multiple_of(AUTOSAVE_INTERVAL) {
        work.autosave();
    }
}

struct Game {
    pending: Vec<Event>,
    players: Vec<player::PlayerState>,
    online_players: Arc<AtomicU32>,
    world: Option<world::World>,
    /// The players whose views the world's wanted chunks were last computed from, in order.
    viewers: Vec<i32>,
    stopped: bool,
}

impl TickWork for Game {
    fn dimensions(&mut self) {
        let Some(world) = &mut self.world else {
            return;
        };
        // Most ticks no player enters another chunk, and the views and the chunks the world keeps loaded
        // stay as they are.
        let mut moved = false;
        for player in &mut self.players {
            let key = (!player.disconnecting).then(|| {
                let center =
                    world::ChunkPos::at(player.player.position.x, player.player.position.z);
                (center, player.view_distance)
            });
            if player.view.key == key {
                continue;
            }
            moved = true;
            player.view.key = key;
            player.view.chunks =
                key.map_or_else(Vec::new, |(center, distance)| center.view(distance));
            if let Some((center, distance)) = key {
                player
                    .sent_chunks
                    .retain(|pos| pos.within(center, distance));
            }
        }
        let viewers: Vec<_> = self.players.iter().map(|p| p.player.entity_id).collect();
        if moved || viewers != self.viewers {
            self.viewers = viewers;
            // A generated chunk's light reads one chunk around it, so a generating world keeps that border
            // loaded without sending it.
            let border = u8::from(world.generates());
            let load_views: Vec<_> = self
                .players
                .iter()
                .map(|p| match p.view.key {
                    Some((center, distance)) if border > 0 => center.view(distance + border),
                    _ => p.view.chunks.clone(),
                })
                .collect();
            // Interleave each player's nearest-first view so one large view cannot monopolize the work
            // queue.
            let longest = load_views.iter().map(Vec::len).max().unwrap_or(0);
            world.want((0..longest).flat_map(|index| {
                load_views
                    .iter()
                    .filter_map(move |view| view.get(index).copied())
            }));
        }
        if let Err(error) = world.advance() {
            self.fail_world(error);
            return;
        }
        // The game time advances and the scheduled fluid ticks run, before the tick's changes are sent.
        world.tick_scheduled();
        let events = world.take_level_events();
        send_level_events(&self.players, &events);
        // Sending light changes, only to the players at whose view's edge each chunk is: the others light the
        // changed blocks themselves (`ChunkHolder.broadcastChanges`).
        for (chunk, light) in world.take_light_updates() {
            for player in &self.players {
                if !player.disconnecting && player.on_tracked_border(chunk) {
                    player.send(player::Output::LightUpdate {
                        position: chunk,
                        light: Arc::clone(&light),
                    });
                }
            }
        }
        // Sending block changes, to the players who have each chunk (`ChunkHolder.broadcastChanges`).
        for (chunk, sections) in world.take_block_changes() {
            for blocks in sections {
                let blocks: Arc<[_]> = blocks.into();
                for player in &self.players {
                    if !player.disconnecting && player.sent_chunks.contains(&chunk) {
                        player.send(player::Output::SectionBlocks(Arc::clone(&blocks)));
                    }
                }
            }
        }
        for player in &mut self.players {
            if player.disconnecting || !player.data_ready || player.awaiting_chunk_batch {
                continue;
            }
            player.chunk_credit =
                (player.chunk_credit + player.chunks_per_tick).min(CHUNKS_PER_BATCH as f32);
            let limit = (player.chunk_credit as usize).min(CHUNKS_PER_BATCH);
            // Every chunk sent is in the view, so a player with as many has them all.
            if limit == 0 || player.sent_chunks.len() >= player.view.chunks.len() {
                continue;
            }
            let mut batch = Vec::with_capacity(limit);
            for &pos in &player.view.chunks {
                if player.sent_chunks.contains(&pos) {
                    continue;
                }
                let chunk = match world.chunk(pos) {
                    Some(world::ChunkState::Loaded(chunk)) => Some(Arc::clone(chunk)),
                    Some(world::ChunkState::Missing) => None,
                    _ => continue,
                };
                // A generated chunk waits for the workers to compute its light.
                let light = if world.needs_light(pos) {
                    match world.light(pos) {
                        Some(light) => light,
                        None => continue,
                    }
                } else {
                    None
                };
                player.sent_chunks.insert(pos);
                batch.push(player::VisibleChunk {
                    position: pos,
                    chunk,
                    light,
                });
                if batch.len() == limit {
                    break;
                }
            }
            if !batch.is_empty() {
                player.chunk_credit -= batch.len() as f32;
                player.awaiting_chunk_batch = true;
                player.send(player::Output::ChunkBatch(batch));
            }
        }
        // The entities tick, after the block changes are sent, and their viewers are sent their changes.
        let survival_rules = world.survival_rules;
        let removed = self
            .world
            .as_mut()
            .map_or_else(Vec::new, world::World::tick_entities);
        self.track_entities(&removed);
        // Players tick with the entities (`ServerPlayer.tick`), and pick up the items they touch (`aiStep`).
        self.tick_digging();
        self.tick_survival(survival_rules);
        self.retry_map_chests();
        self.pick_up_items();
        // Block entities tick after entities. A furnace's lit block state is broadcast on the next tick.
        let furnaces = self
            .world
            .as_mut()
            .map_or_else(Vec::new, world::World::tick_furnaces);
        for (position, furnace) in furnaces {
            self.synchronize_furnace(position, &furnace);
        }
        self.send_inventory_changes();
        self.send_held_maps();
    }

    fn connections(&mut self) {
        if self.stopped {
            return;
        }
        let joined = self.poll_player_data();
        if self.stopped {
            return;
        }
        // A player whose connection fell too far behind leaves; its connection closes on its own.
        let overflowed: Vec<_> = self
            .players
            .iter()
            .filter(|p| p.output.overflowed())
            .map(|p| p.player.entity_id)
            .collect();
        let mut tracking_changed = joined || !overflowed.is_empty();
        for entity_id in overflowed {
            self.leave(entity_id);
        }
        for state in &mut self.players {
            state.tick_start = state.player.position;
        }
        let mut moved = HashSet::new();
        for event in std::mem::take(&mut self.pending) {
            match event {
                Event::Joined {
                    entity_id,
                    profile,
                    view_distance,
                    initial_view_distance,
                    allow_flight,
                    output,
                    force_game_mode_on_login,
                } => {
                    self.join(
                        entity_id,
                        profile,
                        (view_distance, initial_view_distance),
                        allow_flight,
                        output,
                        force_game_mode_on_login,
                    );
                    tracking_changed = true;
                }
                Event::Left(entity_id) => self.leave(entity_id),
                Event::ListPlayers(reply) => {
                    let players = self
                        .players
                        .iter()
                        .filter(|p| p.data_ready && !p.disconnecting)
                        .map(|p| PlayerListEntry {
                            uuid: p.player.profile.uuid,
                            name: p.player.profile.name.clone(),
                            position: [
                                p.player.position.x,
                                p.player.position.y,
                                p.player.position.z,
                            ],
                        })
                        .collect();
                    let _ = reply.send(players);
                }
                Event::SetGameMode {
                    targets,
                    mode,
                    source,
                    reply,
                } => {
                    let mut changed = Vec::new();
                    for uuid in targets {
                        if let Some(index) = self.players.iter().position(|state| {
                            state.data_ready
                                && !state.disconnecting
                                && state.player.profile.uuid == uuid
                        }) && self.set_game_mode(index, mode, source != Some(uuid))
                        {
                            changed.push((uuid, self.players[index].player.profile.name.clone()));
                        }
                    }
                    tracking_changed |= !changed.is_empty();
                    let _ = reply.send(changed);
                }
                Event::DisconnectPlayers {
                    uuids,
                    except,
                    key,
                    text,
                } => {
                    for state in &mut self.players {
                        if !state.disconnecting
                            && uuids.contains(&state.player.profile.uuid) != except
                        {
                            state.disconnecting = true;
                            state.send(player::Output::DisconnectTranslated { key, text });
                        }
                    }
                }
                #[cfg(test)]
                Event::Panic => panic!("a test asked the logic thread to panic"),
                Event::Shutdown(reason) => {
                    for state in &mut self.players {
                        if !state.disconnecting {
                            state.disconnecting = true;
                            state.send(player::Output::Disconnect(reason));
                        }
                    }
                    self.stopped = true;
                }
                Event::Input { entity_id, input } => {
                    match input {
                        player::Input::ChatSessionEnded => {
                            if let Some(state) = self
                                .players
                                .iter_mut()
                                .find(|p| p.player.entity_id == entity_id && !p.disconnecting)
                            {
                                state.chat_session = None;
                                let uuid = state.player.profile.uuid;
                                for player in &self.players {
                                    player.send(player::Output::ChatSessionEnded { uuid });
                                }
                            }
                            continue;
                        }
                        player::Input::ChatSession(session) => {
                            if let Some(state) = self
                                .players
                                .iter_mut()
                                .find(|p| p.player.entity_id == entity_id && !p.disconnecting)
                            {
                                state.chat_session = Some(session.clone());
                                let uuid = state.player.profile.uuid;
                                for player in &self.players {
                                    player.send(player::Output::ChatSession {
                                        uuid,
                                        session: session.clone(),
                                    });
                                }
                            }
                            continue;
                        }
                        player::Input::Dig {
                            action,
                            position,
                            sequence,
                        } => {
                            if let Some(index) = self.active(entity_id) {
                                self.dig(index, action, position, sequence);
                            }
                            continue;
                        }
                        player::Input::Respawn => {
                            if let Some(index) = self.players.iter().position(|p| {
                                p.player.entity_id == entity_id && !p.disconnecting && p.is_dead()
                            }) {
                                self.respawn(index);
                                tracking_changed = true;
                            }
                            continue;
                        }
                        player::Input::Attack(target) => {
                            if let Some(index) = self.active(entity_id) {
                                self.attack_player(index, target);
                            }
                            continue;
                        }
                        player::Input::Movement(movement) => {
                            if let Some(index) = self.active(entity_id) {
                                let state = &mut self.players[index];
                                let before = state.player.position;
                                if state.accepts(movement, self.world.as_ref()) {
                                    let after = state.player.position;
                                    let center = world::ChunkPos::at(after.x, after.z);
                                    state
                                        .sent_chunks
                                        .retain(|pos| pos.within(center, state.view_distance));
                                    self.sprint_exhaustion(index, before, after);
                                    self.check_fall(index, before, after);
                                    moved.insert(entity_id);
                                    tracking_changed = true;
                                }
                            }
                            continue;
                        }
                        player::Input::SetCreativeSlot { slot, stack } => {
                            if let Some(index) = self.active(entity_id) {
                                self.set_creative_slot(index, slot, stack);
                            }
                            continue;
                        }
                        player::Input::SelectHotbarSlot(slot) => {
                            if let Some(index) = self.active(entity_id) {
                                if (0..9).contains(&slot) {
                                    let selected = slot as usize;
                                    let player = &self.players[index];
                                    if player.inventory.selected != selected
                                        && player
                                            .item_use
                                            .as_ref()
                                            .is_some_and(|using| !using.off_hand)
                                    {
                                        self.stop_using_item(index);
                                    }
                                    self.players[index].inventory.selected = selected;
                                } else {
                                    log::warn!(
                                        "{} tried to set an invalid carried item",
                                        self.players[index].player.profile.name
                                    );
                                }
                            }
                            continue;
                        }
                        player::Input::ClickContainer {
                            container_id,
                            state_id,
                            slot,
                            button,
                            mode,
                            changed,
                            carried,
                        } => {
                            if let Some(index) = self.active(entity_id) {
                                self.click_container(
                                    index,
                                    container_id,
                                    state_id,
                                    slot,
                                    button,
                                    mode,
                                    changed,
                                    carried,
                                );
                            }
                            continue;
                        }
                        player::Input::CloseContainer(container_id) => {
                            if let Some(index) = self.active(entity_id) {
                                self.close_container(index, container_id);
                            }
                            continue;
                        }
                        player::Input::DropSelected { all } => {
                            if let Some(index) = self.active(entity_id) {
                                self.drop_selected(index, all);
                            }
                            continue;
                        }
                        player::Input::Sprinting(sprinting) => {
                            if let Some(index) = self.active(entity_id) {
                                self.players[index].sprinting = sprinting;
                            }
                            continue;
                        }
                        player::Input::UseItem { off_hand, sequence } => {
                            if let Some(index) = self.active(entity_id) {
                                self.use_item(index, off_hand, sequence);
                            }
                            continue;
                        }
                        player::Input::StopUsingItem => {
                            if let Some(index) = self.active(entity_id) {
                                self.stop_using_item(index);
                            }
                            continue;
                        }
                        player::Input::UseItemOn {
                            off_hand,
                            position,
                            face,
                            cursor,
                            sequence,
                        } => {
                            if let Some(index) = self.active(entity_id) {
                                self.use_item_on(index, off_hand, position, face, cursor, sequence);
                            }
                            continue;
                        }
                        player::Input::ChatMessage(message) => {
                            if let Some(index) = self
                                .players
                                .iter()
                                .position(|p| p.player.entity_id == entity_id && !p.disconnecting)
                            {
                                let sender = self.players[index].player.profile.clone();
                                for player in &self.players {
                                    player.send(player::Output::ChatMessage {
                                        sender: sender.clone(),
                                        message: message.clone(),
                                    });
                                }
                                // After the message is delivered, as in vanilla.
                                self.players[index].count_chat_message();
                            }
                            continue;
                        }
                        _ => {}
                    }
                    if let Some(state) = self
                        .players
                        .iter_mut()
                        .find(|p| p.player.entity_id == entity_id)
                    {
                        match input {
                            player::Input::ChatSession(_)
                            | player::Input::ChatSessionEnded
                            | player::Input::ChatMessage(_)
                            | player::Input::Respawn
                            | player::Input::Attack(_)
                            | player::Input::Movement(_)
                            | player::Input::Dig { .. }
                            | player::Input::SetCreativeSlot { .. }
                            | player::Input::SelectHotbarSlot(_)
                            | player::Input::Sprinting(_)
                            | player::Input::UseItem { .. }
                            | player::Input::StopUsingItem
                            | player::Input::UseItemOn { .. }
                            | player::Input::DropSelected { .. }
                            | player::Input::ClickContainer { .. }
                            | player::Input::CloseContainer(_) => {
                                unreachable!()
                            }
                            player::Input::SecondaryUse(held) => state.secondary_use = held,
                            player::Input::ConfirmTeleport(id) => {
                                if state.awaiting_teleport == Some(id) {
                                    state.awaiting_teleport = None;
                                }
                            }
                            player::Input::Flying(flying) => {
                                if matches!(
                                    state.player.profile.game_mode,
                                    player::GameMode::Creative | player::GameMode::Spectator
                                ) {
                                    state.flying = flying;
                                }
                            }
                            player::Input::ViewDistance(distance) => {
                                let distance = distance.clamp(2, state.max_view_distance);
                                tracking_changed |= state.view_distance != distance;
                                state.view_distance = distance;
                                let center = world::ChunkPos::at(
                                    state.player.position.x,
                                    state.player.position.z,
                                );
                                state.sent_chunks.retain(|pos| pos.within(center, distance));
                                state.send(player::Output::ViewDistance(distance));
                            }
                            player::Input::ChunkBatchReceived(rate) => {
                                if state.awaiting_chunk_batch {
                                    state.awaiting_chunk_batch = false;
                                    state.chunks_per_tick = if rate.is_finite() {
                                        rate.clamp(0.01, CHUNKS_PER_BATCH as f32)
                                    } else {
                                        0.01
                                    };
                                    state.chunk_credit =
                                        state.chunk_credit.min(state.chunks_per_tick);
                                }
                            }
                        }
                    }
                }
            }
        }
        for state in &mut self.players {
            // At the start of the connection's tick, after its packets, as vanilla.
            if let Some(sequence) = state.acknowledge_block_changes.take() {
                state.send(player::Output::AcknowledgeBlockChanges(sequence));
            }
            if state.data_ready {
                state.check_flight();
            }
            state.tick_chat();
        }
        if tracking_changed {
            self.update_tracking(&moved);
        }
        self.online_players.store(
            u32::try_from(self.players.iter().filter(|p| p.data_ready).count()).unwrap_or(u32::MAX),
            Ordering::Relaxed,
        );
    }

    fn autosave(&mut self) {
        self.save_player_inventories();
        if let Some(Err(error)) = self.world.as_mut().map(world::World::autosave) {
            self.fail_world(error);
        }
    }
}

impl Game {
    fn fail_world(&mut self, error: io::Error) {
        log::error!("{error}");
        for state in &mut self.players {
            if !state.disconnecting {
                state.disconnecting = true;
                state.send(player::Output::Disconnect("Cannot access the world"));
            }
        }
        self.stopped = true;
    }

    fn join(
        &mut self,
        id: i32,
        profile: player::Profile,
        (view_distance, initial_view_distance): (u8, u8),
        allow_flight: bool,
        output: player::OutputSender,
        force_game_mode_on_login: Option<bool>,
    ) {
        if let Some(old) = self
            .players
            .iter()
            .position(|p| p.player.profile.uuid == profile.uuid)
        {
            self.players[old].send(player::Output::Disconnect(
                "You logged in from another location",
            ));
            let old_id = self.players[old].player.entity_id;
            self.leave(old_id);
        }
        let mut player = player::PlayerState::new(id, profile, view_distance, allow_flight, output);
        player.force_game_mode_on_login = force_game_mode_on_login;
        if let Some(world) = &self.world {
            let position = world.spawn_position();
            player.player.position = position;
            player.tick_start = position;
        }
        player.view_distance = initial_view_distance.clamp(2, player.max_view_distance);
        self.players.push(player);
        let index = self.players.len() - 1;
        let store = self
            .world
            .as_mut()
            .and_then(|world| world.player_store.as_mut());
        if let Some(store) = store {
            let player = &mut self.players[index];
            player.data_ready = false;
            match store.load(id, player.player.profile.uuid) {
                Ok(Some(data)) => self.finish_join(index, Some(data)),
                Ok(None) => {}
                Err(error) => {
                    log::error!("Cannot load player {}: {error}", player.player.profile.name);
                    player.disconnecting = true;
                    player.send(player::Output::Disconnect("Cannot load player data"));
                }
            }
        } else {
            self.finish_join(index, None);
        }
    }

    fn finish_join(&mut self, index: usize, data: Option<Arc<player_data::PlayerInventory>>) {
        let effect_ids = self
            .world
            .as_ref()
            .map_or_else(Default::default, world::World::effect_ids);
        let player = &mut self.players[index];
        if let Some(data) = data {
            Self::restore_inventory(player, data);
        }
        if player.force_game_mode_on_login.take().is_some() {
            player.send(player::Output::Login {
                mode: player.player.profile.game_mode,
                previous: player.previous_game_mode,
                flying: player.flying,
            });
            player.send(player.vitals.update());
            for (kind, effect_id) in player::EffectKind::ALL.into_iter().zip(effect_ids) {
                if player.effects[kind as usize].is_some() {
                    player.send_effect(kind, effect_id, false);
                }
            }
            if player.max_absorption() > 0.0 {
                player.send_max_absorption();
                player.send_absorption();
            }
            if player.is_burning() {
                player.send(player::Output::OnFire {
                    entity_id: player.player.entity_id,
                    burning: true,
                });
            }
            if player.is_dead() {
                player.send(player::Output::Death {
                    entity_id: player.player.entity_id,
                    uuid: player.player.profile.uuid,
                    name: player.player.profile.name.clone(),
                    kind: None,
                    attacker: None,
                });
            }
        }
        let contents = place::initial_contents(&mut player.inventory);
        let player = &self.players[index];
        for (i, existing) in self.players.iter().enumerate() {
            if i == index || !existing.data_ready || existing.disconnecting {
                continue;
            }
            player.send(player::Output::AddPlayer(existing.player.profile.clone()));
            if let Some(session) = &existing.chat_session {
                player.send(player::Output::ChatSession {
                    uuid: existing.player.profile.uuid,
                    session: session.clone(),
                });
            }
            existing.send(player::Output::AddPlayer(player.player.profile.clone()));
        }
        player.send(contents);
        if player.inventory.selected != 0 {
            player.send(player::Output::SelectedHotbarSlot(
                player.inventory.selected as i32,
            ));
        }
    }

    fn leave(&mut self, entity_id: i32) {
        let Some(index) = self
            .players
            .iter()
            .position(|p| p.player.entity_id == entity_id)
        else {
            return;
        };
        if self.players[index].block_menu.is_some() {
            let drops = self.players[index].with_menu(inventory::Inventory::take_transient_items);
            self.players[index].block_menu = None;
            for stack in drops {
                self.drop_item(index, stack);
            }
        }
        // The inventory menu's cursor and grid are also transient on departure.
        let drops = self.players[index].inventory.take_transient_items();
        for stack in drops {
            self.drop_item(index, stack);
        }
        self.save_player_inventory(index);
        let left = self.players.remove(index);
        for player in &mut self.players {
            if player.tracked.remove(&entity_id) {
                player.send(player::Output::RemoveEntity(entity_id));
            }
            if left.data_ready {
                player.send(player::Output::RemovePlayer(left.player.profile.uuid));
            }
        }
    }

    /// The index of a living player whose data has loaded and who is not leaving.
    fn active(&self, entity_id: i32) -> Option<usize> {
        self.players.iter().position(|p| {
            p.player.entity_id == entity_id && p.data_ready && !p.disconnecting && !p.is_dead()
        })
    }

    fn set_game_mode(&mut self, index: usize, mode: player::GameMode, notify: bool) -> bool {
        let state = &mut self.players[index];
        if state.player.profile.game_mode == mode {
            return false;
        }
        state.previous_game_mode = Some(state.player.profile.game_mode);
        state.player.profile.game_mode = mode;
        state.flying = mode == player::GameMode::Spectator;
        state.client_floating = false;
        state.floating_ticks = 0;
        state.fall_distance = 0.0;
        state.sprinting = false;
        let uuid = state.player.profile.uuid;
        self.stop_using_item(index);
        self.stop_digging(index);
        if mode == player::GameMode::Spectator && self.players[index].block_menu.is_some() {
            self.close_block_menu(index);
        }
        for player in &self.players {
            player.send(if player.player.profile.uuid == uuid {
                player::Output::GameModeChanged { uuid, mode, notify }
            } else {
                player::Output::PlayerGameMode { uuid, mode }
            });
        }
        true
    }

    fn stop_digging(&mut self, index: usize) {
        let digging = &mut self.players[index].digging;
        let active = std::mem::take(&mut digging.is_destroying).then_some(digging.destroy_pos);
        let delayed =
            std::mem::take(&mut digging.has_delayed_destroy).then_some(digging.delayed_destroy_pos);
        for position in active.into_iter().chain(delayed) {
            self.destroy_block_progress(index, position, -1);
        }
    }

    /// A player's Start, Abort or Finish of breaking a block, as vanilla's `handlePlayerAction` and
    /// `ServerPlayerGameMode.handleBlockBreakAction`.
    ///
    /// A player in Creative breaks the block at once; one in Survival breaks it once its destroy progress is
    /// far enough, as [`Game::tick_digging`] follows it. Spawn protection does not apply while the server has no
    /// operators, as in vanilla, and there are none until `ops.json` is read; the world border is not checked
    /// until it exists. Hitting a block (`BlockState.attack`, such as a note block playing) is not ported yet.
    fn dig(&mut self, index: usize, action: block::DigAction, pos: block::BlockPos, sequence: i32) {
        let state = &self.players[index];
        let range = state.block_interaction_range() + 1.0;
        let mode = state.player.profile.game_mode;
        if pos.distance_squared(state.eye_position()) >= range * range {
            // Too far: vanilla sends nothing back but the acknowledgement.
        } else if pos.y > block::MAX_Y {
            self.send_block(index, pos);
        } else {
            match action {
                block::DigAction::Start => self.start_digging(index, mode, pos),
                block::DigAction::Finish => self.finish_digging(index, pos),
                block::DigAction::Abort => self.abort_digging(index, pos),
            }
        }
        self.players[index].acknowledge_block_changes(sequence);
    }

    fn start_digging(&mut self, index: usize, mode: player::GameMode, pos: block::BlockPos) {
        if mode == player::GameMode::Creative {
            self.destroy_and_acknowledge(index, pos);
            return;
        }
        // Adventure and Spectator may not break blocks without a tool that allows it.
        if mode != player::GameMode::Survival {
            self.send_block(index, pos);
            return;
        }
        let state = self.block_at(pos);
        let air = harpas_core::block::block_states().is_air(state);
        let progress = if air {
            1.0
        } else {
            self.destroy_progress(index, state)
        };
        let digging = &mut self.players[index].digging;
        digging.destroy_progress_start = digging.game_ticks;
        if !air && progress >= 1.0 {
            self.destroy_and_acknowledge(index, pos);
            return;
        }
        if digging.is_destroying {
            let previous = digging.destroy_pos;
            self.send_block(index, previous);
        }
        let digging = &mut self.players[index].digging;
        digging.is_destroying = true;
        digging.destroy_pos = pos;
        let stage = (progress * 10.0) as i32;
        digging.last_sent_state = stage;
        self.destroy_block_progress(index, pos, stage);
    }

    fn finish_digging(&mut self, index: usize, pos: block::BlockPos) {
        let digging = &self.players[index].digging;
        if pos != digging.destroy_pos {
            return;
        }
        let ticks = digging
            .game_ticks
            .wrapping_sub(digging.destroy_progress_start);
        let state = self.block_at(pos);
        if harpas_core::block::block_states().is_air(state) {
            return;
        }
        let progress = self.destroy_progress(index, state) * (ticks.wrapping_add(1)) as f32;
        let digging = &mut self.players[index].digging;
        if progress >= 0.7 {
            digging.is_destroying = false;
            self.destroy_block_progress(index, pos, -1);
            self.destroy_and_acknowledge(index, pos);
            return;
        }
        if !digging.has_delayed_destroy {
            digging.is_destroying = false;
            digging.has_delayed_destroy = true;
            digging.delayed_destroy_pos = pos;
            digging.delayed_tick_start = digging.destroy_progress_start;
        }
    }

    fn abort_digging(&mut self, index: usize, pos: block::BlockPos) {
        let digging = &mut self.players[index].digging;
        digging.is_destroying = false;
        let destroy_pos = digging.destroy_pos;
        if destroy_pos != pos {
            log::warn!("Mismatch in destroy block pos: {destroy_pos:?} {pos:?}");
            self.destroy_block_progress(index, destroy_pos, -1);
        }
        self.destroy_block_progress(index, pos, -1);
    }

    /// Each player's part of `ServerPlayerGameMode.tick`: counts the player's ticks, and follows the block it is
    /// breaking, or the one it finished too early, until it breaks.
    fn tick_digging(&mut self) {
        let air = |state| harpas_core::block::block_states().is_air(state);
        for index in 0..self.players.len() {
            if self.players[index].disconnecting || !self.players[index].data_ready {
                continue;
            }
            let digging = &mut self.players[index].digging;
            digging.game_ticks = digging.game_ticks.wrapping_add(1);
            if digging.has_delayed_destroy {
                let pos = digging.delayed_destroy_pos;
                let start = digging.delayed_tick_start;
                let state = self.block_at(pos);
                if air(state) {
                    self.players[index].digging.has_delayed_destroy = false;
                } else if self.increment_destroy_progress(index, state, pos, start) >= 1.0 {
                    self.players[index].digging.has_delayed_destroy = false;
                    self.destroy_and_acknowledge(index, pos);
                }
            } else if digging.is_destroying {
                let pos = digging.destroy_pos;
                let start = digging.destroy_progress_start;
                let state = self.block_at(pos);
                if air(state) {
                    self.destroy_block_progress(index, pos, -1);
                    let digging = &mut self.players[index].digging;
                    digging.last_sent_state = -1;
                    digging.is_destroying = false;
                } else {
                    self.increment_destroy_progress(index, state, pos, start);
                }
            }
        }
    }

    fn increment_destroy_progress(
        &mut self,
        index: usize,
        state: u32,
        pos: block::BlockPos,
        start: i32,
    ) -> f32 {
        let ticks = self.players[index].digging.game_ticks.wrapping_sub(start);
        let progress = self.destroy_progress(index, state) * (ticks.wrapping_add(1)) as f32;
        let stage = (progress * 10.0) as i32;
        if stage != self.players[index].digging.last_sent_state {
            self.destroy_block_progress(index, pos, stage);
            self.players[index].digging.last_sent_state = stage;
        }
        progress
    }

    /// How much of `state` the player breaks each tick (`BlockState.getDestroyProgress`).
    fn destroy_progress(&self, index: usize, state: u32) -> f32 {
        let player = &self.players[index];
        let inventory = &player.inventory;
        let tool = &inventory.slots[inventory.hand_slot(false)];
        let mining = self.world.as_ref().map_or_else(
            || mining::Mining::by_hand(state),
            |world| world.mining(state, tool),
        );
        let eye_in_water = self
            .world
            .as_ref()
            .is_some_and(|world| world.eye_in_water(player.eye_position()));
        block::destroy_progress(
            state,
            player.destroy_speed(mining.speed, eye_in_water),
            mining.can_harvest,
        )
    }

    /// Shows the other players within 32 blocks the cracks of a block a player is breaking, or removes them for
    /// any `stage` outside 0 to 9 (`ServerLevel.destroyBlockProgress`).
    fn destroy_block_progress(&self, index: usize, position: block::BlockPos, stage: i32) {
        let breaker = self.players[index].player.entity_id;
        for player in &self.players {
            if player.player.entity_id != breaker
                && !player.disconnecting
                && within(player, position, 32.0)
            {
                player.send(player::Output::BlockDestruction {
                    entity_id: breaker,
                    position,
                    // Sent as a byte, as vanilla writes it.
                    stage: stage as i8,
                });
            }
        }
    }

    /// The block state at `pos`: void air outside the world's height, and where no chunk is loaded.
    fn block_at(&self, pos: block::BlockPos) -> u32 {
        self.world
            .as_ref()
            .and_then(|world| world.block_state(pos))
            .unwrap_or_else(block::void_air)
    }

    /// Sends a player the current state of a block, which overrides the client's prediction.
    fn send_block(&self, index: usize, position: block::BlockPos) {
        let state = self.block_at(position);
        self.players[index].send(player::Output::BlockUpdate { position, state });
    }

    fn destroy_and_acknowledge(&mut self, index: usize, pos: block::BlockPos) {
        if !self.destroy_block(index, pos) {
            self.send_block(index, pos);
        }
    }

    /// Breaks a block for a player, as vanilla's `ServerPlayerGameMode.destroyBlock`. Returns whether the
    /// player's prediction stands.
    fn destroy_block(&mut self, index: usize, pos: block::BlockPos) -> bool {
        let Some(world) = &mut self.world else {
            return true;
        };
        // Vanilla would load a chunk that is not loaded; a player cannot reach one.
        let Some(state) = world.block_state(pos) else {
            return true;
        };
        // Only operators may break these, and there are none yet.
        if block::is_game_master_block(state) {
            world.block_changed(pos);
            return false;
        }
        if let Err(error) = world.prepare_chest_maps([pos]) {
            if error.kind() == io::ErrorKind::WouldBlock {
                self.players[index].pending_map_chest = Some(maps::PendingChestAction::Break(pos));
            } else {
                log::debug!("Cannot break chest at {pos:?}: {error}");
            }
            return false;
        }
        if !world.set_block(
            pos,
            block::removed(state),
            neighbor::UPDATE_ALL,
            neighbor::UPDATE_LIMIT,
        ) {
            return false;
        }
        // `playerWillDestroy`: the particles and sound of the broken block, for the other players nearby.
        let breaker = self.players[index].player.entity_id;
        for player in &self.players {
            if player.player.entity_id != breaker
                && !player.disconnecting
                && within(player, pos, 64.0)
            {
                player.send(player::Output::WorldEvent {
                    event: neighbor::BLOCK_DESTROYED,
                    position: pos,
                    data: state as i32,
                });
            }
        }
        // A player in Survival who may harvest the block gets its drops (`playerDestroy`); in Creative, none.
        let mode = self.players[index].player.profile.game_mode;
        if mode != player::GameMode::Creative {
            let inventory = &mut self.players[index].inventory;
            let slot = inventory.hand_slot(false);
            let tool = inventory.slots[slot].clone();
            let mining = world.mining(state, &tool);
            if mining.can_harvest {
                world.drop_resources(state, pos, &tool);
            }
            if mode == player::GameMode::Survival {
                match mining.wear(world.block_item_codec(), &mut inventory.slots[slot]) {
                    Ok(true) => {
                        Self::send_item_broken(
                            &self.players,
                            breaker,
                            player::Output::MainHandItemBroken(breaker),
                        );
                    }
                    Ok(false) => {}
                    Err(error) => log::warn!("Cannot apply mining wear to item {slot}: {error}"),
                }
            }
        }
        // The particles and sound of the blocks that broke with it, for every player nearby.
        let events = world.take_level_events();
        send_level_events(&self.players, &events);
        for viewer in 0..self.players.len() {
            if self.players[viewer]
                .block_menu
                .as_ref()
                .is_some_and(|menu| menu.uses(pos))
            {
                self.close_block_menu(viewer);
            }
        }
        true
    }

    fn send_item_broken(players: &[player::PlayerState], entity_id: i32, output: player::Output) {
        for player in players {
            if !player.disconnecting
                && (player.player.entity_id == entity_id || player.tracked.contains(&entity_id))
            {
                player.send(output.clone());
            }
        }
    }

    fn update_tracking(&mut self, moved: &HashSet<i32>) {
        // What visibility depends on. A profile, with its skin, is copied only for a player that comes
        // into view.
        let targets: Vec<_> = self
            .players
            .iter()
            .map(|p| {
                (
                    p.player.entity_id,
                    p.player.profile.game_mode,
                    p.player.position,
                    p.data_ready,
                )
            })
            .collect();
        for viewer in 0..self.players.len() {
            for (target, &(entity_id, mode, position, ready)) in targets.iter().enumerate() {
                if viewer == target || !ready || !self.players[viewer].data_ready {
                    continue;
                }
                let visible = self.players[viewer].can_see(mode, position);
                let tracked = self.players[viewer].tracked.contains(&entity_id);
                match (visible, tracked) {
                    (true, false) => {
                        let target = &self.players[target];
                        let spawned = target.player.clone();
                        let using = target.item_use.as_ref().map(|using| using.off_hand);
                        let burning = target.is_burning();
                        let viewer = &mut self.players[viewer];
                        viewer.tracked.insert(entity_id);
                        viewer.send(player::Output::Spawn(spawned));
                        if burning {
                            viewer.send(player::Output::OnFire { entity_id, burning });
                        }
                        if using.is_some() {
                            viewer.send(player::Output::UsingItem {
                                entity_id,
                                off_hand: using,
                            });
                        }
                    }
                    (false, true) => {
                        let viewer = &mut self.players[viewer];
                        viewer.tracked.remove(&entity_id);
                        viewer.send(player::Output::RemoveEntity(entity_id));
                    }
                    (true, true) if moved.contains(&entity_id) => {
                        self.players[viewer].send(player::Output::MoveEntity {
                            entity_id,
                            position,
                        });
                    }
                    _ => {}
                }
            }
        }
    }
}

/// One step of the logic thread: the tick that is due at `now`, or else a wait for the next event or tick.
/// Returns whether the thread goes on.
fn advance(
    game: &mut Game,
    schedule: &mut Schedule,
    receiver: &Receiver<Event>,
    number: &mut u64,
    now: Instant,
) -> bool {
    if schedule.take_due(now) {
        // A tick that overruns makes the next one due at once. Take what arrived meanwhile first, or a server
        // that falls behind would stop hearing its players and the request to stop.
        loop {
            match receiver.try_recv() {
                Ok(event) => game.pending.push(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return false,
            }
        }
        *number += 1;
        tick(game, *number);
        !game.stopped
    } else {
        // Wake for incoming events, but apply them only at the tick's connection phase. Check the
        // deadline after each event so a busy network cannot prevent ticks from running.
        match receiver.recv_timeout(schedule.next.saturating_duration_since(now)) {
            Ok(event) => game.pending.push(event),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return false,
        }
        true
    }
}

fn run(
    receiver: Receiver<Event>,
    online_players: Arc<AtomicU32>,
    world: Option<world::World>,
) -> io::Result<()> {
    let mut game = Game {
        pending: Vec::new(),
        players: Vec::new(),
        online_players,
        world,
        viewers: Vec::new(),
        stopped: false,
    };
    let mut schedule = Schedule::new(Instant::now());
    let mut number = 0;
    // A panic in a tick must not lose the world: it is saved as it stands, as vanilla saves on a crash.
    let ticks = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        while advance(
            &mut game,
            &mut schedule,
            &receiver,
            &mut number,
            Instant::now(),
        ) {}
    }));
    // Final snapshots precede the I/O flush, including when the server stopped after a tick panic.
    while let Some(player) = game.players.last() {
        let id = player.player.entity_id;
        let index = game.players.len() - 1;
        let menu = game.players[index].container_id();
        // Close live menus before the final snapshot: their cursor and ingredients return to persistent
        // slots, rather than becoming new item entities just as the server is shutting down.
        game.close_container(index, menu);
        if menu != 0 {
            game.close_container(index, 0);
        }
        game.leave(id);
    }
    if let Some(world) = &mut game.world {
        world.flush()?;
    }
    ticks.map_err(|_| io::Error::other("The logic thread panicked; the world was saved"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use player::{GameMode, Input, Movement, Output, Profile};

    fn profile(name: &str, uuid: u128) -> Profile {
        Profile {
            uuid,
            name: name.into(),
            properties: vec![],
            game_mode: GameMode::Creative,
        }
    }

    fn game() -> Game {
        Game {
            pending: Vec::new(),
            players: Vec::new(),
            online_players: Arc::new(AtomicU32::new(0)),
            world: None,
            viewers: Vec::new(),
            stopped: false,
        }
    }

    fn join(game: &mut Game, id: i32, name: &str, distance: u8) -> player::OutputReceiver {
        let (output, receiver) = player::output_channel();
        game.pending.push(Event::Joined {
            entity_id: id,
            profile: profile(name, id as u128),
            view_distance: distance,
            initial_view_distance: distance,
            allow_flight: false,
            output,
            force_game_mode_on_login: None,
        });
        receiver
    }

    fn input(game: &mut Game, entity_id: i32, input: Input) {
        game.pending.push(Event::Input { entity_id, input });
    }

    fn movement(x: f64, yaw: f32) -> Input {
        Input::Movement(Movement {
            position: Some([x, 65.0, 0.5]),
            rotation: Some([yaw, 0.0]),
            on_ground: false,
        })
    }

    #[test]
    fn rapid_bare_hand_attacks_share_charge_across_targets() {
        use harpas_core::nbt::Compound;

        let mut game = game();
        let _attacker = join(&mut game, 1, "Attacker", 2);
        let _defender = join(&mut game, 2, "Defender", 2);
        let _other = join(&mut game, 3, "Other", 2);
        game.connections();
        for (offset, player) in game.players.iter_mut().enumerate() {
            player.player.profile.game_mode = GameMode::Survival;
            player.player.position.x += offset as f64;
            player.awaiting_teleport = None;
        }
        let game_rules = Compound::from_iter([("minecraft:natural_health_regeneration", 0_i8)]);
        let rules = survival::SurvivalRules::read(Some(&game_rules), None);
        for _ in 0..5 {
            game.tick_survival(rules);
        }

        input(&mut game, 1, Input::Attack(2));
        input(&mut game, 1, Input::Attack(2));
        input(&mut game, 1, Input::Attack(3));
        game.connections();
        assert_eq!(game.players[1].vitals.health, 19.0);
        assert_eq!(game.players[2].vitals.health, 20.0 - 0.208);

        game.players[2].player.profile.game_mode = GameMode::Creative;
        for _ in 0..10 {
            game.tick_survival(rules);
        }
        input(&mut game, 1, Input::Attack(3));
        input(&mut game, 1, Input::Attack(1));
        input(&mut game, 1, Input::Attack(2));
        game.connections();
        assert_eq!(game.players[0].vitals.health, 20.0);
        assert_eq!(game.players[1].vitals.health, 18.0);
        assert_eq!(game.players[2].vitals.health, 20.0 - 0.208);

        game.players[0].inventory.slots[inventory::HOTBAR] = inventory::ItemStack {
            item: harpas_core::item::items().id("minecraft:apple").unwrap(),
            count: 1,
            ..Default::default()
        };
        for _ in 0..10 {
            game.tick_survival(rules);
        }
        game.players[0].inventory.slots[inventory::HOTBAR] = inventory::ItemStack::empty();
        game.tick_survival(rules);
        input(&mut game, 1, Input::Attack(2));
        game.connections();
        assert_eq!(game.players[1].vitals.health, 18.0 - 0.208);
    }

    #[test]
    fn shield_blocks_after_five_ticks_until_release() {
        use harpas_core::nbt::Nbt;
        use inventory::{HOTBAR, ItemStack};
        use item_data::ItemCodec;

        let mut game = game();
        let _attacker = join(&mut game, 1, "Attacker", 2);
        let defender = join(&mut game, 2, "Defender", 2);
        game.connections();
        for player in &mut game.players {
            player.player.profile.game_mode = GameMode::Survival;
            player.awaiting_teleport = None;
        }
        game.players[0].player.position.z += 2.0;
        game.players[1].player.position.yaw = 0.0;
        game.players[1].inventory.slots[HOTBAR] = ItemStack {
            item: harpas_core::item::items().id("minecraft:shield").unwrap(),
            count: 1,
            ..ItemStack::empty()
        };
        let rules = survival::SurvivalRules::read(
            Some(&harpas_core::nbt::Compound::from_iter([(
                "minecraft:natural_health_regeneration",
                0_i8,
            )])),
            None,
        );
        game.tick_survival(rules);
        input(
            &mut game,
            2,
            Input::UseItem {
                off_hand: false,
                sequence: 1,
            },
        );
        game.connections();
        for _ in 0..4 {
            game.tick_survival(rules);
        }
        game.attack_player(0, 2);
        assert_eq!(game.players[1].vitals.health, 19.0);
        assert!(
            !defender
                .try_iter()
                .any(|output| matches!(output, Output::ShieldBlock { .. }))
        );
        game.tick_survival(rules);
        game.attack_player(0, 2);
        assert!(
            defender
                .try_iter()
                .any(|output| matches!(output, Output::ShieldBlock { .. }))
        );
        assert_eq!(game.players[1].vitals.health, 19.0);
        assert_eq!(game.players[0].attack_charge_ticks, 0);
        assert!(
            game.players[1].inventory.slots[HOTBAR]
                .components
                .added
                .is_empty()
        );

        for _ in 0..10 {
            game.tick_survival(rules);
        }
        game.players[0].attack_damage_modifier = 3.0;
        game.attack_player(0, 2);
        assert_eq!(game.players[1].vitals.health, 19.0);
        assert_eq!(
            ItemCodec::fallback()
                .saved_component(&game.players[1].inventory.slots[HOTBAR], "minecraft:damage"),
            Some(Nbt::Int(5))
        );
        game.tick_survival(rules);
        assert!(game.players[1].item_use.is_some());

        input(&mut game, 2, Input::StopUsingItem);
        game.connections();
        for _ in 0..10 {
            game.tick_survival(rules);
        }
        game.attack_player(0, 2);
        assert_eq!(game.players[1].vitals.health, 18.0);
    }

    #[test]
    fn eating_an_apple_takes_32_ticks_and_changing_slots_cancels_it() {
        use inventory::{HOTBAR, ItemStack};

        let mut game = game();
        let _received = join(&mut game, 1, "Eating", 2);
        game.connections();
        let player = &mut game.players[0];
        player.player.profile.game_mode = GameMode::Survival;
        player.awaiting_teleport = None;
        player.vitals.food = 16;
        player.inventory.slots[HOTBAR] = ItemStack {
            item: harpas_core::item::items().id("minecraft:apple").unwrap(),
            count: 2,
            ..ItemStack::empty()
        };

        input(
            &mut game,
            1,
            Input::UseItem {
                off_hand: false,
                sequence: 1,
            },
        );
        input(&mut game, 1, Input::SelectHotbarSlot(1));
        input(&mut game, 1, Input::SelectHotbarSlot(0));
        game.connections();
        assert!(game.players[0].item_use.is_none());

        input(
            &mut game,
            1,
            Input::UseItem {
                off_hand: false,
                sequence: 2,
            },
        );
        game.connections();
        for _ in 0..31 {
            game.tick_survival(survival::SurvivalRules::default());
        }
        assert_eq!(game.players[0].inventory.slots[HOTBAR].count, 2);
        assert_eq!(game.players[0].vitals.food, 16);
        game.tick_survival(survival::SurvivalRules::default());
        assert_eq!(game.players[0].inventory.slots[HOTBAR].count, 1);
        assert_eq!(game.players[0].vitals.food, 20);
        assert_eq!(game.players[0].vitals.saturation, 7.4);

        input(
            &mut game,
            1,
            Input::UseItem {
                off_hand: false,
                sequence: 3,
            },
        );
        game.connections();
        assert!(game.players[0].item_use.is_none());
    }

    #[test]
    fn hunger_rolls_only_when_food_is_finished_and_exhausts_until_expiry() {
        use harpas_core::random::LegacyRandom;
        use inventory::{HOTBAR, ItemStack};
        use item_data::{Consumable, ConsumeEffect, FoodEffect, FoodProperties, ItemCodec};
        use player::EffectKind::{Hunger, Nausea};

        let mut game = game();
        let received = join(&mut game, 1, "Eating", 2);
        game.connections();
        let flesh = harpas_core::item::items()
            .id("minecraft:rotten_flesh")
            .unwrap();
        let path = std::env::temp_dir().join(format!("harpas-food-hunger-{}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        let mut world = world::World::open(&path, &["minecraft:plains"]).unwrap();
        world.set_item_codec(Arc::new(
            ItemCodec::default()
                .with_mob_effects(&[Hunger.id().to_owned(), Nausea.id().to_owned()])
                .with_consumables(
                    [(
                        flesh,
                        Consumable {
                            food: Some(FoodProperties {
                                nutrition: 4,
                                saturation: 0.8,
                                can_always_eat: false,
                            }),
                            use_ticks: 2,
                            remainder: None,
                            effect: Some(ConsumeEffect::Apply {
                                effects: vec![
                                    FoodEffect {
                                        kind: Hunger,
                                        duration: 3,
                                        amplifier: 0,
                                    },
                                    FoodEffect {
                                        kind: Nausea,
                                        duration: 10,
                                        amplifier: 0,
                                    },
                                ],
                                probability: 0.8,
                            }),
                        },
                    )]
                    .into(),
                ),
        ));
        game.world = Some(world);
        let player = &mut game.players[0];
        player.player.profile.game_mode = GameMode::Survival;
        player.awaiting_teleport = None;
        player.vitals.food = 10;
        player.vitals.saturation = 0.0;
        player.vitals.exhaustion = 3.999;
        // This seed rolls below 0.8 on the first draw and above it on the second.
        player.random = LegacyRandom::new(0);
        player.inventory.slots[HOTBAR] = ItemStack {
            item: flesh,
            count: 2,
            ..ItemStack::empty()
        };
        let rules = survival::SurvivalRules::default();
        game.use_item(0, false, 1);
        game.tick_survival(rules);
        game.stop_using_item(0);
        assert_eq!(game.players[0].inventory.slots[HOTBAR].count, 2);
        assert!(game.players[0].effects[Hunger as usize].is_none());
        game.use_item(0, false, 2);
        for _ in 0..2 {
            game.tick_survival(rules);
        }
        assert_eq!(game.players[0].inventory.slots[HOTBAR].count, 1);
        assert_eq!(game.players[0].vitals.saturation, 0.8);
        assert!(game.players[0].effects[Hunger as usize].is_some());
        game.use_item(0, false, 3);
        game.tick_survival(rules);
        assert_eq!(game.players[0].vitals.saturation, 0.0);
        for _ in 0..2 {
            game.tick_survival(rules);
        }
        assert_eq!(game.players[0].inventory.slots[HOTBAR].count, 0);
        assert!(game.players[0].effects[Hunger as usize].is_none());
        assert!((game.players[0].vitals.exhaustion - 0.014).abs() < 0.000001);
        let updates: Vec<_> = std::iter::from_fn(|| received.try_recv().ok())
            .filter_map(|output| match output {
                Output::EntityEffect {
                    effect_id, effect, ..
                } => Some((effect_id, effect.map(|effect| effect.duration))),
                _ => None,
            })
            .collect();
        assert_eq!(updates, [(0, Some(3)), (1, Some(10)), (0, None)]);
        drop(game);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn poison_ticks_on_divisible_durations_and_stops_damaging_at_one_health() {
        use harpas_core::nbt::Compound;
        use player::{DamageKind, EffectKind::Poison};

        for (amplifier, damage_ticks) in [(0, [2, 27]), (1, [5, 17])] {
            let mut game = game();
            let received = join(&mut game, 1, "Poisoned", 2);
            game.connections();
            let player = &mut game.players[0];
            player.player.profile.game_mode = GameMode::Survival;
            player.effects[Poison as usize] = survival::ActiveEffect::read(&Compound::from_iter([
                ("duration", 76_i32),
                ("amplifier", amplifier),
            ]));
            player.vitals.health = 3.0;
            player.vitals.food = 10;
            for tick in 1..=76 {
                game.tick_survival(survival::SurvivalRules::default());
                let damage = received
                    .try_iter()
                    .filter(|output| {
                        matches!(
                            output,
                            Output::Damage {
                                kind: DamageKind::Magic,
                                ..
                            }
                        )
                    })
                    .count();
                assert_eq!(
                    damage,
                    usize::from(damage_ticks.contains(&tick)),
                    "level {}, tick {tick}",
                    amplifier + 1
                );
            }
            assert_eq!(game.players[0].vitals.health, 1.0);
            assert!(game.players[0].effects[Poison as usize].is_none());
        }
    }

    #[test]
    fn regeneration_heals_every_twenty_five_ticks_at_level_two() {
        use harpas_core::nbt::Compound;
        use player::EffectKind::Regeneration;

        let mut game = game();
        let received = join(&mut game, 1, "Healing", 2);
        game.connections();
        let player = &mut game.players[0];
        player.player.profile.game_mode = GameMode::Survival;
        player.vitals.health = 17.0;
        player.effects[Regeneration as usize] =
            survival::ActiveEffect::read(&Compound::from_iter([
                ("duration", 100_i32),
                ("amplifier", 1_i32),
            ]));
        let rules = Compound::from_iter([("minecraft:natural_health_regeneration", 0_i8)]);
        let rules = survival::SurvivalRules::read(Some(&rules), None);
        for tick in 1..=100 {
            game.tick_survival(rules);
            let updates: Vec<_> = received
                .try_iter()
                .filter_map(|output| match output {
                    Output::Health { health, .. } => Some(health),
                    _ => None,
                })
                .collect();
            match tick {
                1 => assert_eq!(updates, [18.0]),
                26 => assert_eq!(updates, [19.0]),
                51 => assert_eq!(updates, [20.0]),
                _ => assert!(
                    updates.is_empty(),
                    "unexpected health update on tick {tick}"
                ),
            }
        }
        assert_eq!(game.players[0].vitals.health, 20.0);
        assert!(game.players[0].effects[Regeneration as usize].is_none());
    }

    #[test]
    fn food_use_honors_the_configured_duration_and_cancels_on_item_replacement() {
        use inventory::{ItemStack, OFF_HAND};
        use item_data::{Consumable, FoodProperties, ItemCodec};

        let mut game = game();
        let _received = join(&mut game, 1, "Eating", 2);
        game.connections();
        let kelp = harpas_core::item::items()
            .id("minecraft:dried_kelp")
            .unwrap();
        let path = std::env::temp_dir().join(format!("harpas-food-use-{}", std::process::id()));
        let mut world = world::World::open(&path, &["minecraft:plains"]).unwrap();
        world.set_item_codec(Arc::new(ItemCodec::default().with_consumables(
            std::collections::HashMap::from([(
                kelp,
                Consumable {
                    food: Some(FoodProperties {
                        nutrition: 3,
                        saturation: 4.0,
                        can_always_eat: false,
                    }),
                    use_ticks: 16,
                    remainder: None,
                    effect: None,
                },
            )]),
        )));
        game.world = Some(world);

        let stack = ItemStack {
            item: kelp,
            count: 2,
            ..ItemStack::empty()
        };
        let player = &mut game.players[0];
        player.player.profile.game_mode = GameMode::Survival;
        player.awaiting_teleport = None;
        player.vitals.food = 19;
        player.vitals.saturation = 18.0;
        player.inventory.slots[OFF_HAND] = stack.clone();

        game.use_item(0, true, 1);
        game.players[0].inventory.slots[OFF_HAND] = ItemStack::empty();
        let rules = survival::SurvivalRules::default();
        game.tick_survival(rules);
        assert!(game.players[0].item_use.is_none());

        game.players[0].inventory.slots[OFF_HAND] = stack;
        game.use_item(0, true, 2);
        for _ in 0..15 {
            game.tick_survival(rules);
        }
        assert_eq!(game.players[0].inventory.slots[OFF_HAND].count, 2);
        assert_eq!(game.players[0].vitals.food, 19);
        game.tick_survival(rules);
        assert_eq!(game.players[0].inventory.slots[OFF_HAND].count, 1);
        assert_eq!(game.players[0].vitals.food, 20);
        assert_eq!(game.players[0].vitals.saturation, 20.0);
    }

    #[test]
    fn stacked_food_returns_its_container_to_the_inventory_or_drops_it() {
        use inventory::{HOTBAR, ItemStack, OFF_HAND};
        use item_data::{Consumable, FoodProperties, ItemCodec};

        let stack = |name, count| ItemStack {
            item: harpas_core::item::items().id(name).unwrap(),
            count,
            ..ItemStack::empty()
        };
        let mut stew = stack("minecraft:mushroom_stew", 2);
        Arc::make_mut(&mut stew.components).max_stack_size = Some(2);
        let bowl = stack("minecraft:bowl", 1);
        let path =
            std::env::temp_dir().join(format!("harpas-food-remainder-{}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        let mut world = world::World::open(&path, &["minecraft:plains"]).unwrap();
        world.set_item_codec(Arc::new(
            ItemCodec::default().with_consumables(
                [(
                    stew.item,
                    Consumable {
                        food: Some(FoodProperties {
                            nutrition: 6,
                            saturation: 7.2,
                            can_always_eat: false,
                        }),
                        use_ticks: 1,
                        remainder: Some(bowl.clone()),
                        effect: None,
                    },
                )]
                .into(),
            ),
        ));
        let mut game = game();
        let _received = join(&mut game, 1, "Eating", 2);
        game.connections();
        game.world = Some(world);
        let player = &mut game.players[0];
        player.player.profile.game_mode = GameMode::Survival;
        player.awaiting_teleport = None;
        player.vitals.food = 10;
        player.inventory.slots[OFF_HAND] = stew.clone();

        let rules = survival::SurvivalRules::default();
        game.use_item(0, true, 1);
        game.tick_survival(rules);
        assert_eq!(game.players[0].inventory.slots[OFF_HAND].count, 1);
        assert_eq!(game.players[0].inventory.slots[HOTBAR], bowl);

        game.players[0].inventory.slots[9..OFF_HAND].fill(stack("minecraft:stone", 64));
        game.players[0].inventory.slots[OFF_HAND] = stew;
        game.use_item(0, true, 2);
        game.tick_survival(rules);
        assert_eq!(game.players[0].inventory.slots[OFF_HAND].count, 1);
        let entities = &game.world.as_ref().unwrap().entities;
        assert_eq!(entities.order.len(), 1);
        assert_eq!(entities.item(entities.order[0]).unwrap().stack, bowl);
        drop(game);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn starvation_death_clears_fall_distance_on_respawn() {
        use harpas_core::nbt::Compound;

        let mut game = game();
        let _received = join(&mut game, 1, "Hungry", 2);
        game.connections();
        let player = &mut game.players[0];
        player.player.profile.game_mode = GameMode::Survival;
        player.fall_distance = 8.0;
        player.vitals.health = 1.0;
        player.vitals.food = 0;
        player.vitals.saturation = 0.0;
        player.vitals.food_timer = 79;
        let level = Compound::from_iter([(
            "difficulty_settings",
            Compound::from_iter([("difficulty", "hard")]),
        )]);
        game.tick_survival(survival::SurvivalRules::read(None, Some(&level)));
        assert!(game.players[0].is_dead());

        input(&mut game, 1, Input::Respawn);
        game.connections();
        assert_eq!(game.players[0].vitals.health, 20.0);
        assert_eq!(game.players[0].fall_distance, 0.0);
    }

    #[test]
    fn ticks_every_fifty_milliseconds_without_drifting() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start);
        assert!(!schedule.take_due(start + Duration::from_millis(49)));
        assert!(schedule.take_due(start + Duration::from_millis(50)));
        assert!(schedule.take_due(start + Duration::from_millis(125)));
        assert!(!schedule.take_due(start + Duration::from_millis(149)));
        assert!(schedule.take_due(start + Duration::from_millis(150)));
    }

    #[test]
    fn short_delays_catch_up_but_delays_over_two_seconds_reset_the_target() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start);
        let late = start + Duration::from_millis(200);
        for _ in 0..4 {
            assert!(schedule.take_due(late));
        }
        assert!(!schedule.take_due(late));

        let mut boundary = Schedule::new(start);
        assert!(boundary.take_due(start + Duration::from_millis(2_050)));
        assert!(boundary.take_due(start + Duration::from_millis(2_050)));

        let mut stalled = Schedule::new(start);
        let resumed = start + Duration::from_millis(2_051);
        assert!(stalled.take_due(resumed));
        assert!(!stalled.take_due(resumed));
        assert!(!stalled.take_due(resumed + Duration::from_millis(49)));
        assert!(stalled.take_due(resumed + Duration::from_millis(50)));
    }

    #[test]
    fn overrunning_ticks_still_apply_the_events_that_arrived() {
        let mut game = game();
        let (sender, receiver) = mpsc::channel();
        let start = Instant::now();
        let mut schedule = Schedule::new(start);
        let mut number = 0;
        let _output = {
            let (output, received) = player::output_channel();
            sender
                .send(Event::Joined {
                    entity_id: 1,
                    profile: profile("Steve", 1),
                    view_distance: 10,
                    initial_view_distance: 10,
                    allow_flight: false,
                    output,
                    force_game_mode_on_login: None,
                })
                .unwrap();
            received
        };
        // Every tick is late, as when each takes longer than 50 milliseconds: one is always due.
        let late = start + Duration::from_secs(1);
        assert!(advance(
            &mut game,
            &mut schedule,
            &receiver,
            &mut number,
            late
        ));
        assert_eq!(game.online_players.load(Ordering::Relaxed), 1);
        sender.send(Event::Shutdown("Server closed")).unwrap();
        assert!(!advance(
            &mut game,
            &mut schedule,
            &receiver,
            &mut number,
            late
        ));
        assert_eq!(number, 2);
    }

    #[test]
    fn tick_phases_follow_vanillas_order_and_autosave_interval() {
        #[derive(Default)]
        struct Recorder(Vec<&'static str>);
        impl TickWork for Recorder {
            fn functions(&mut self) {
                self.0.push("functions");
            }
            fn dimensions(&mut self) {
                self.0.push("dimensions");
            }
            fn connections(&mut self) {
                self.0.push("connections");
            }
            fn autosave(&mut self) {
                self.0.push("autosave");
            }
        }
        let mut recorder = Recorder::default();
        tick(&mut recorder, 5_999);
        assert_eq!(recorder.0, ["functions", "dimensions", "connections"]);
        recorder.0.clear();
        tick(&mut recorder, 6_000);
        assert_eq!(
            recorder.0,
            ["functions", "dimensions", "connections", "autosave"]
        );
    }

    #[test]
    fn connection_events_are_applied_in_order_at_the_packet_phase() {
        let mut game = game();
        let _first = join(&mut game, 1, "Steve", 10);
        game.pending.push(Event::Left(1));
        let _second = join(&mut game, 2, "Alex", 10);
        assert_eq!(game.online_players.load(Ordering::Relaxed), 0);
        tick(&mut game, 1);
        assert_eq!(game.online_players.load(Ordering::Relaxed), 1);
        game.pending.push(Event::Left(2));
        assert_eq!(game.online_players.load(Ordering::Relaxed), 1);
        tick(&mut game, 2);
        assert_eq!(game.online_players.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn returning_to_a_view_in_one_tick_resends_the_columns_the_client_unloaded() {
        let mut game = game();
        let _output = join(&mut game, 1, "Steve", 3);
        tick(&mut game, 1);
        let center = world::ChunkPos { x: 0, z: 0 };
        game.players[0].view_distance = 2;
        game.players[0].sent_chunks = center.view(2).into_iter().collect();
        input(&mut game, 1, Input::ConfirmTeleport(1));
        input(&mut game, 1, movement(-0.5, 0.0));
        input(&mut game, 1, movement(0.5, 0.0));
        tick(&mut game, 2);
        assert_eq!(game.players[0].sent_chunks.len(), 20);
        assert!(game.players[0].sent_chunks.iter().all(|pos| pos.x != 2));

        game.players[0].view_distance = 3;
        game.players[0].sent_chunks = center.view(3).into_iter().collect();
        input(&mut game, 1, Input::ViewDistance(2));
        input(&mut game, 1, Input::ViewDistance(3));
        tick(&mut game, 3);
        assert_eq!(game.players[0].view_distance, 3);
        assert_eq!(game.players[0].sent_chunks.len(), 25);
    }

    #[test]
    fn chat_sessions_precede_messages_and_are_sent_to_late_joiners() {
        let mut game = game();
        let steve = join(&mut game, 1, "Steve", 10);
        let alex = join(&mut game, 2, "Alex", 10);
        tick(&mut game, 1);
        steve.try_iter().for_each(drop);
        alex.try_iter().for_each(drop);
        let chat_session = player::ChatSession {
            id: 88,
            expires_at: 1000,
            key: vec![1],
            key_signature: vec![2],
        };
        let message = player::ChatMessage {
            index: 0,
            signature: Some([3; 256]),
            message: "hello".into(),
            timestamp: 42,
            salt: 5,
            last_seen: vec![],
        };
        input(&mut game, 1, Input::ChatSession(chat_session.clone()));
        input(&mut game, 1, Input::ChatMessage(Box::new(message.clone())));
        tick(&mut game, 2);
        for recipient in [&steve, &alex] {
            assert_eq!(
                recipient.try_recv(),
                Ok(Output::ChatSession {
                    uuid: 1,
                    session: chat_session.clone()
                })
            );
            assert!(
                matches!(recipient.try_recv(), Ok(Output::ChatMessage { sender, message: sent })
                if sender.uuid == 1 && *sent == message)
            );
        }
        let newcomer = join(&mut game, 3, "Newcomer", 10);
        tick(&mut game, 3);
        let initial: Vec<_> = newcomer.try_iter().collect();
        let profile = initial
            .iter()
            .position(|event| matches!(event, Output::AddPlayer(p) if p.uuid == 1))
            .unwrap();
        let chat = initial.iter().position(|event| matches!(event, Output::ChatSession { uuid: 1, session } if *session == chat_session)).unwrap();
        assert!(profile < chat);
    }

    #[test]
    fn chat_spam_disconnects_like_vanilla_after_the_tenth_quick_message() {
        let message = || {
            Input::ChatMessage(Box::new(player::ChatMessage {
                index: 0,
                signature: None,
                message: "hello".into(),
                timestamp: 42,
                salt: 5,
                last_seen: vec![],
            }))
        };
        let chats = |output: &player::OutputReceiver| {
            output
                .try_iter()
                .filter(|event| matches!(event, Output::ChatMessage { .. }))
                .count()
        };
        let mut game = game();
        let steve = join(&mut game, 1, "Steve", 10);
        let alex = join(&mut game, 2, "Alex", 10);
        tick(&mut game, 1);
        steve.try_iter().for_each(drop);

        // A message a second, every twentieth tick, never adds up.
        for number in 0..40 * 20 {
            if number % 20 == 0 {
                input(&mut game, 1, message());
            }
            tick(&mut game, 2 + number);
        }
        assert_eq!(chats(&alex), 40);
        assert!(
            !steve
                .try_iter()
                .any(|event| matches!(event, Output::Disconnect(_)))
        );

        // Ten in one tick are all delivered, and the tenth disconnects the sender.
        for _ in 0..12 {
            input(&mut game, 2, message());
        }
        tick(&mut game, 1000);
        assert_eq!(chats(&steve), 10);
        assert!(
            alex.try_iter()
                .any(|event| event == Output::Disconnect("Kicked for spamming"))
        );
    }

    #[test]
    fn players_get_profiles_spawns_movement_and_removal_in_order() {
        let mut game = game();
        let steve = join(&mut game, 1, "Steve", 10);
        let alex = join(&mut game, 2, "Alex", 10);
        tick(&mut game, 1);
        // Each is sent its inventory once, after the players already there, as vanilla's `initInventoryMenu`.
        let joined = |received: &player::OutputReceiver| -> Vec<_> {
            let (inventory, others): (Vec<_>, Vec<_>) = received
                .try_iter()
                .partition(|output| matches!(output, Output::InventoryContent { .. }));
            assert_eq!(inventory.len(), 1);
            others
        };
        let steve_join = joined(&steve);
        let alex_join = joined(&alex);
        assert!(
            matches!(&steve_join[..], [Output::AddPlayer(p), Output::Spawn(e)] if p.name == "Alex" && e.entity_id == 2)
        );
        assert!(
            matches!(&alex_join[..], [Output::AddPlayer(p), Output::Spawn(e)] if p.name == "Steve" && e.entity_id == 1)
        );

        input(&mut game, 1, Input::ConfirmTeleport(1));
        input(&mut game, 1, movement(2.5, 90.0));
        tick(&mut game, 2);
        assert!(matches!(steve.try_recv(), Ok(Output::OwnPosition(p)) if p.x == 2.5));
        assert!(
            matches!(alex.try_recv(), Ok(Output::MoveEntity { entity_id: 1, position }) if position.x == 2.5 && position.yaw == 90.0)
        );

        game.pending.push(Event::Left(1));
        tick(&mut game, 3);
        assert_eq!(alex.try_recv(), Ok(Output::RemoveEntity(1)));
        assert_eq!(alex.try_recv(), Ok(Output::RemovePlayer(1)));
        assert_eq!(game.online_players.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn players_stop_and_resume_tracking_when_they_move_out_of_range() {
        let mut game = game();
        let steve = join(&mut game, 1, "Steve", 2);
        let alex = join(&mut game, 2, "Alex", 2);
        tick(&mut game, 1);
        steve.try_iter().for_each(drop);
        alex.try_iter().for_each(drop);
        input(&mut game, 2, Input::ConfirmTeleport(1));
        for step in 1..=5 {
            input(&mut game, 2, movement(0.5 + 8.0 * f64::from(step), 0.0));
            tick(&mut game, step as u64 + 1);
        }
        assert!(
            steve
                .try_iter()
                .any(|event| event == Output::RemoveEntity(2))
        );
        assert!(
            alex.try_iter()
                .any(|event| event == Output::RemoveEntity(1))
        );
        input(&mut game, 2, movement(32.5, 0.0));
        tick(&mut game, 7);
        assert!(
            steve
                .try_iter()
                .any(|event| matches!(event, Output::Spawn(p) if p.entity_id == 2))
        );
        assert!(
            alex.try_iter()
                .any(|event| matches!(event, Output::Spawn(p) if p.entity_id == 1))
        );
    }

    #[test]
    fn invalid_or_excessive_movement_is_rejected_and_teleports_are_acknowledged() {
        let mut game = game();
        let output = join(&mut game, 1, "Steve", 10);
        tick(&mut game, 1);
        // A player who joins is sent its inventory.
        assert!(matches!(
            output.try_recv(),
            Ok(Output::InventoryContent { state_id: 1, .. })
        ));
        input(&mut game, 1, movement(1.5, 0.0));
        tick(&mut game, 2);
        assert!(
            output.try_recv().is_err(),
            "movement before the initial teleport confirmation"
        );

        input(&mut game, 1, Input::ConfirmTeleport(1));
        input(&mut game, 1, movement(1.5, 0.0));
        tick(&mut game, 3);
        assert!(matches!(output.try_recv(), Ok(Output::OwnPosition(p)) if p.x == 1.5));
        input(&mut game, 1, movement(100.0, 0.0));
        tick(&mut game, 4);
        assert!(
            matches!(output.try_recv(), Ok(Output::Teleport { teleport_id: 2, position }) if position.x == 1.5)
        );
        input(&mut game, 1, Input::ConfirmTeleport(3));
        input(&mut game, 1, movement(2.5, 0.0));
        tick(&mut game, 5);
        assert!(
            output.try_recv().is_err(),
            "wrong teleport confirmation must not release movement"
        );
        input(&mut game, 1, Input::ConfirmTeleport(2));
        input(&mut game, 1, movement(2.5, 0.0));
        tick(&mut game, 6);
        assert!(matches!(output.try_recv(), Ok(Output::OwnPosition(p)) if p.x == 2.5));
        input(&mut game, 1, movement(f64::NAN, 0.0));
        tick(&mut game, 7);
        assert_eq!(
            output.try_recv(),
            Ok(Output::Disconnect("Invalid player movement"))
        );
    }

    #[test]
    fn game_mode_change_revokes_flight_and_updates_other_players() {
        let mut game = game();
        let target = join(&mut game, 1, "Steve", 10);
        let observer = join(&mut game, 2, "Alex", 10);
        tick(&mut game, 1);
        while target.try_recv().is_ok() {}
        while observer.try_recv().is_ok() {}
        input(&mut game, 1, Input::Flying(true));
        tick(&mut game, 2);
        assert!(game.players[0].flying);

        let (reply, changed) = mpsc::channel();
        game.pending.push(Event::SetGameMode {
            targets: vec![1],
            mode: GameMode::Survival,
            source: None,
            reply,
        });
        input(&mut game, 1, Input::Flying(true));
        tick(&mut game, 3);
        assert_eq!(changed.try_recv().unwrap(), vec![(1, "Steve".to_owned())]);
        assert_eq!(game.players[0].player.profile.game_mode, GameMode::Survival);
        assert!(!game.players[0].flying);
        assert_eq!(
            target.try_recv(),
            Ok(Output::GameModeChanged {
                uuid: 1,
                mode: GameMode::Survival,
                notify: true,
            })
        );
        assert_eq!(
            observer.try_recv(),
            Ok(Output::PlayerGameMode {
                uuid: 1,
                mode: GameMode::Survival,
            })
        );
    }

    #[test]
    fn multiple_moves_in_one_tick_share_the_same_speed_budget() {
        let mut game = game();
        let output = join(&mut game, 1, "Steve", 10);
        input(&mut game, 1, Input::ConfirmTeleport(1));
        tick(&mut game, 1);
        input(&mut game, 1, movement(5.5, 0.0));
        input(&mut game, 1, movement(11.0, 0.0));
        tick(&mut game, 2);
        assert!(matches!(
            output.try_recv(),
            Ok(Output::InventoryContent { .. })
        ));
        assert!(matches!(output.try_recv(), Ok(Output::OwnPosition(p)) if p.x == 5.5));
        assert!(
            matches!(output.try_recv(), Ok(Output::Teleport { position, .. }) if position.x == 5.5)
        );
    }

    #[test]
    fn each_viewer_uses_its_own_distance_and_spectators_are_hidden_from_players() {
        let mut game = game();
        let first = join(&mut game, 1, "Steve", 2);
        let second = join(&mut game, 2, "Alex", 10);
        tick(&mut game, 1);
        first.try_iter().for_each(drop);
        second.try_iter().for_each(drop);
        input(&mut game, 2, Input::ConfirmTeleport(1));
        for step in 1..=5 {
            input(&mut game, 2, movement(0.5 + 8.0 * f64::from(step), 0.0));
            tick(&mut game, step as u64 + 1);
        }
        assert!(
            first
                .try_iter()
                .any(|event| event == Output::RemoveEntity(2))
        );
        assert!(
            !second
                .try_iter()
                .any(|event| event == Output::RemoveEntity(1))
        );

        let mut spectator = profile("Spectator", 3);
        spectator.game_mode = GameMode::Spectator;
        let (output, third) = player::output_channel();
        game.pending.push(Event::Joined {
            entity_id: 3,
            profile: spectator,
            view_distance: 10,
            initial_view_distance: 10,
            allow_flight: false,
            output,
            force_game_mode_on_login: None,
        });
        tick(&mut game, 7);
        assert!(
            first
                .try_iter()
                .any(|event| matches!(event, Output::AddPlayer(p) if p.name == "Spectator"))
        );
        assert!(
            !second
                .try_iter()
                .any(|event| matches!(event, Output::Spawn(p) if p.entity_id == 3))
        );
        assert!(
            third
                .try_iter()
                .any(|event| matches!(event, Output::Spawn(p) if p.entity_id == 1))
        );
    }

    #[test]
    fn finite_positions_are_clamped_to_the_world_limits() {
        let (output, _receiver) = player::output_channel();
        let mut state = player::PlayerState::new(1, profile("Steve", 1), 10, false, output);
        state.awaiting_teleport = None;
        state.player.position.x = 29_999_999.0;
        state.player.position.y = 19_999_999.0;
        state.tick_start = state.player.position;
        assert!(state.accepts(
            Movement {
                position: Some([30_000_001.0, 20_000_001.0, 0.5]),
                rotation: None,
                on_ground: false,
            },
            None
        ));
        assert_eq!(state.player.position.x, 30_000_000.0);
        assert_eq!(state.player.position.y, 20_000_000.0);
    }

    #[test]
    fn sustained_flight_is_checked_and_honors_server_settings_and_game_mode() {
        fn state(
            mode: GameMode,
            allow_flight: bool,
        ) -> (player::PlayerState, player::OutputReceiver) {
            let (sender, receiver) = player::output_channel();
            let mut profile = profile("Steve", 1);
            profile.game_mode = mode;
            let mut state = player::PlayerState::new(1, profile, 10, allow_flight, sender);
            state.awaiting_teleport = None;
            state.accepts(
                Movement {
                    position: Some([0.5, 65.0, 0.5]),
                    rotation: None,
                    on_ground: false,
                },
                None,
            );
            (state, receiver)
        }

        let (mut survival, output) = state(GameMode::Survival, false);
        for _ in 0..80 {
            survival.check_flight();
        }
        assert!(output.try_recv().is_err());
        survival.check_flight();
        assert_eq!(
            output.try_recv(),
            Ok(Output::Disconnect("Flying is not enabled on this server"))
        );

        for (mode, allowed) in [
            (GameMode::Creative, false),
            (GameMode::Spectator, false),
            (GameMode::Survival, true),
        ] {
            let (mut state, output) = state(mode, allowed);
            for _ in 0..100 {
                state.check_flight();
            }
            assert!(output.try_recv().is_err());
        }

        let (mut falling, output) = state(GameMode::Survival, false);
        for _ in 0..80 {
            falling.check_flight();
        }
        falling.accepts(
            Movement {
                position: Some([0.5, 64.0, 0.5]),
                rotation: None,
                on_ground: false,
            },
            None,
        );
        for _ in 0..100 {
            falling.check_flight();
        }
        assert!(
            !output
                .try_iter()
                .any(|message| matches!(message, Output::Disconnect(_)))
        );
    }

    #[test]
    fn tick_disconnects_a_hovering_survival_player_unless_flight_is_allowed() {
        for allow_flight in [false, true] {
            let mut game = game();
            let (output, receiver) = player::output_channel();
            let mut profile = profile("Steve", 1);
            profile.game_mode = GameMode::Survival;
            game.pending.push(Event::Joined {
                entity_id: 1,
                profile,
                view_distance: 10,
                initial_view_distance: 10,
                allow_flight,
                output,
                force_game_mode_on_login: None,
            });
            input(&mut game, 1, Input::ConfirmTeleport(1));
            input(&mut game, 1, movement(0.5, 0.0));
            for number in 1..=81 {
                tick(&mut game, number);
            }
            assert!(matches!(
                receiver.try_recv(),
                Ok(Output::InventoryContent { .. })
            ));
            if allow_flight {
                assert!(receiver.try_recv().is_err());
            } else {
                assert_eq!(
                    receiver.try_recv(),
                    Ok(Output::Disconnect("Flying is not enabled on this server"))
                );
            }
        }
    }

    #[test]
    fn duplicate_identity_disconnects_the_old_session_without_removing_the_new_one() {
        let mut game = game();
        let old = join(&mut game, 1, "Steve", 10);
        tick(&mut game, 1);
        old.try_iter().for_each(drop);
        let (output, new) = player::output_channel();
        game.pending.push(Event::Joined {
            entity_id: 2,
            profile: profile("Steve", 1),
            view_distance: 10,
            initial_view_distance: 10,
            allow_flight: false,
            output,
            force_game_mode_on_login: None,
        });
        tick(&mut game, 2);
        assert_eq!(
            old.try_recv(),
            Ok(Output::Disconnect("You logged in from another location"))
        );
        game.pending.push(Event::Left(1));
        tick(&mut game, 3);
        assert_eq!(game.online_players.load(Ordering::Relaxed), 1);
        assert!(matches!(
            new.try_recv(),
            Ok(Output::InventoryContent { .. })
        ));
        assert!(new.try_recv().is_err());
    }

    #[test]
    fn a_player_whose_connection_falls_too_far_behind_leaves() {
        let mut game = game();
        let slow = join(&mut game, 1, "Steve", 10);
        let alex = join(&mut game, 2, "Alex", 10);
        tick(&mut game, 1);
        alex.try_iter().for_each(drop);
        slow.try_iter().for_each(drop);

        // Steve's connection takes nothing while messages for it keep coming.
        let steve = &game.players[0];
        for _ in 0..player::MAX_QUEUED_OUTPUT {
            steve.send(Output::RemoveEntity(3));
        }
        assert!(!steve.output.overflowed());
        assert_eq!(slow.try_recv(), Ok(Output::RemoveEntity(3)));
        steve.send(Output::RemoveEntity(3));
        assert!(!steve.output.overflowed());
        steve.send(Output::RemoveEntity(3));
        assert!(steve.output.overflowed());

        // The connection is told to close before anything else that is queued, and the player leaves.
        assert_eq!(slow.try_recv(), Ok(Output::Disconnect(player::TOO_SLOW)));
        tick(&mut game, 2);
        assert_eq!(game.online_players.load(Ordering::Relaxed), 1);
        assert!(
            alex.try_iter()
                .any(|output| output == Output::RemovePlayer(1))
        );
        // Its connection's departure, which arrives later, changes nothing.
        game.pending.push(Event::Left(1));
        tick(&mut game, 3);
        assert_eq!(game.online_players.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn shutdown_disconnects_every_player_and_stops_the_logic_thread() {
        let (handle, logic) = start().unwrap();
        let session = handle
            .player_joined(
                handle.next_entity_id().unwrap(),
                profile("Steve", 1),
                10,
                false,
            )
            .unwrap();
        handle.shutdown("Server closed").unwrap();
        // The thread stops even though a handle and a session are still alive.
        logic.join().unwrap().unwrap();
        let mut outputs = std::iter::from_fn(|| session.try_recv());
        assert!(outputs.any(|output| output == Output::Disconnect("Server closed")));
        assert!(handle.shutdown("Server closed").is_err());
    }

    #[test]
    fn clicks_send_only_what_the_client_did_not_predict() {
        use crate::inventory::{HashedStack, ItemStack};
        use harpas_core::item::items;

        let mut game = game();
        let output = join(&mut game, 1, "Steve", 10);
        tick(&mut game, 1);
        output.try_iter().for_each(drop);
        game.players[0].player.profile.game_mode = GameMode::Survival;
        let stone = items().id("minecraft:stone").unwrap();
        game.players[0].inventory.set_by_client(
            9,
            ItemStack {
                item: stone,
                count: 8,
                components: Default::default(),
            },
        );
        let hashed = |count| {
            Some(HashedStack {
                item: stone,
                count,
                added: Vec::new(),
                removed: Vec::new(),
            })
        };
        let click = |game: &mut Game, state_id, carried| {
            game.pending.push(Event::Input {
                entity_id: 1,
                input: Input::ClickContainer {
                    container_id: 0,
                    state_id,
                    slot: 9,
                    button: 0,
                    mode: 0,
                    changed: vec![(9, None)],
                    carried,
                },
            });
        };
        // The client predicted the pickup: nothing to send.
        click(&mut game, 1, hashed(8));
        tick(&mut game, 2);
        assert!(output.try_recv().is_err());
        assert_eq!(game.players[0].inventory.carried.count, 8);
        // A stale state ID gets the whole inventory.
        click(&mut game, 0, None);
        tick(&mut game, 3);
        assert!(matches!(
            output.try_recv(),
            Ok(Output::InventoryContent { state_id: 2, .. })
        ));
        // A wrong prediction is corrected.
        click(&mut game, 2, hashed(3));
        tick(&mut game, 4);
        assert!(matches!(output.try_recv(), Ok(Output::SetCarried(stack)) if stack.count == 8));
    }

    #[test]
    fn output_wakes_the_thread_that_joined_the_player() {
        let (handle, logic) = start().unwrap();
        // The test's own thread joins the player, as a connection's thread does.
        let session = handle
            .player_joined(
                handle.next_entity_id().unwrap(),
                profile("Steve", 1),
                10,
                false,
            )
            .unwrap();
        handle.shutdown("Server closed").unwrap();
        let started = Instant::now();
        // Each park is far longer than the test may take, so it ends only when the logic thread unparks.
        // The messages already queued, such as the player's inventory, are taken before parking again.
        loop {
            match session.try_recv() {
                Some(Output::Disconnect("Server closed")) => break,
                Some(_) => {}
                None => {
                    assert!(started.elapsed() < Duration::from_secs(10));
                    thread::park_timeout(Duration::from_secs(60));
                }
            }
        }
        logic.join().unwrap().unwrap();
    }

    #[test]
    fn logic_thread_receives_joins_and_departures() {
        fn wait_for_count(handle: &Handle, expected: u32) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while handle.online_players() != expected {
                assert!(
                    Instant::now() < deadline,
                    "player count did not reach {expected}"
                );
                thread::sleep(Duration::from_millis(5));
            }
        }
        let (handle, logic) = start().unwrap();
        let first = handle
            .player_joined(
                handle.next_entity_id().unwrap(),
                profile("Steve", 1),
                10,
                false,
            )
            .unwrap();
        let second = handle
            .player_joined(
                handle.next_entity_id().unwrap(),
                profile("Alex", 2),
                10,
                false,
            )
            .unwrap();
        wait_for_count(&handle, 2);
        assert_eq!(handle.player_names().unwrap(), ["Steve", "Alex"]);
        drop(first);
        assert_eq!(handle.player_names().unwrap(), ["Alex"]);
        wait_for_count(&handle, 1);
        drop(second);
        wait_for_count(&handle, 0);
        drop(handle);
        logic.join().unwrap().unwrap();
    }
}
