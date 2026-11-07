//! Harpas engine: vanilla tick order on one logic thread
//!
//! Design: `docs/engine-core.md`.

pub mod player;
pub mod world;
pub mod worldgen;

use std::collections::HashSet;
use std::fmt;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const TICK_INTERVAL: Duration = Duration::from_millis(50);
const MAX_LAG: Duration = Duration::from_secs(2);
const AUTOSAVE_INTERVAL: u64 = 6_000;
const CHUNKS_PER_BATCH: usize = 8;

/// A handle for sending connection events to the logic thread.
#[derive(Clone, Debug)]
pub struct Handle {
    events: Sender<Event>,
    online_players: Arc<AtomicU32>,
    next_entity_id: Arc<AtomicI32>,
    has_world: bool,
}

impl Handle {
    /// Whether this engine supplies world chunks to connections.
    pub fn has_world(&self) -> bool {
        self.has_world
    }

    /// Reserves a runtime entity ID; IDs are never reused during this server process.
    pub fn next_entity_id(&self) -> Option<i32> {
        self.next_entity_id
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .ok()
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
    pub fn player_joined_with_view(
        &self,
        entity_id: i32,
        profile: player::Profile,
        max_view_distance: u8,
        initial_view_distance: u8,
        allow_flight: bool,
    ) -> Result<PlayerSession, LogicStopped> {
        let (output, receiver) = mpsc::channel();
        self.events
            .send(Event::Joined {
                entity_id,
                profile,
                view_distance: max_view_distance,
                initial_view_distance,
                allow_flight,
                output,
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
        let (sender, receiver) = mpsc::channel();
        self.events
            .send(Event::ListPlayers(sender))
            .map_err(|_| LogicStopped)?;
        receiver.recv().map_err(|_| LogicStopped)
    }

    /// Stops the logic thread at the next connection phase, disconnecting every player with `reason`.
    pub fn shutdown(&self, reason: &'static str) -> Result<(), LogicStopped> {
        self.events
            .send(Event::Shutdown(reason))
            .map_err(|_| LogicStopped)
    }
}

/// A connection in Play; dropping it tells the logic thread that the connection has ended.
#[derive(Debug)]
pub struct PlayerSession {
    entity_id: i32,
    events: Sender<Event>,
    output: Receiver<player::Output>,
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
pub fn start_with_world(
    world_dir: &std::path::Path,
    biomes: &[&str],
) -> io::Result<(Handle, JoinHandle<io::Result<()>>)> {
    start_game(Some(world::World::open(world_dir, biomes)?))
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
/// persisted on unload or shutdown.
pub fn start_with_world_source(
    world_dir: &std::path::Path,
    biomes: &[&str],
    source: Arc<dyn world::ChunkSource>,
) -> io::Result<(Handle, JoinHandle<io::Result<()>>)> {
    start_game(Some(world::World::open_with_source(
        world_dir, biomes, source,
    )?))
}

fn start_game(world: Option<world::World>) -> io::Result<(Handle, JoinHandle<io::Result<()>>)> {
    let (sender, receiver) = mpsc::channel();
    let online_players = Arc::new(AtomicU32::new(0));
    let handle = Handle {
        events: sender,
        online_players: Arc::clone(&online_players),
        next_entity_id: Arc::new(AtomicI32::new(1)),
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
        output: Sender<player::Output>,
    },
    Input {
        entity_id: i32,
        input: player::Input,
    },
    Left(i32),
    ListPlayers(Sender<Vec<String>>),
    Shutdown(&'static str),
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
    stopped: bool,
}

impl TickWork for Game {
    fn dimensions(&mut self) {
        let Some(world) = &mut self.world else {
            return;
        };
        let views: Vec<_> = self
            .players
            .iter()
            .map(|p| {
                if p.disconnecting {
                    Vec::new()
                } else {
                    world::ChunkPos::at(p.player.position.x, p.player.position.z)
                        .view(p.view_distance)
                }
            })
            .collect();
        // A generated chunk's light reads one chunk around it, so a generating world keeps that border
        // loaded without sending it.
        let load_views: Vec<_> = if world.generates() {
            self.players
                .iter()
                .map(|p| {
                    if p.disconnecting {
                        Vec::new()
                    } else {
                        world::ChunkPos::at(p.player.position.x, p.player.position.z)
                            .view(p.view_distance.saturating_add(1))
                    }
                })
                .collect()
        } else {
            views.clone()
        };
        // Interleave each player's nearest-first view so one large view cannot monopolize the work queue.
        let longest = load_views.iter().map(Vec::len).max().unwrap_or(0);
        let wanted = (0..longest).flat_map(|index| {
            load_views
                .iter()
                .filter_map(move |view| view.get(index).copied())
        });
        if let Err(error) = world.update(wanted) {
            self.fail_world(error);
            return;
        }
        for (player, view) in self.players.iter_mut().zip(views) {
            let center = world::ChunkPos::at(player.player.position.x, player.player.position.z);
            player
                .sent_chunks
                .retain(|pos| pos.within(center, player.view_distance));
            if player.disconnecting || player.awaiting_chunk_batch {
                continue;
            }
            player.chunk_credit =
                (player.chunk_credit + player.chunks_per_tick).min(CHUNKS_PER_BATCH as f32);
            let limit = (player.chunk_credit as usize).min(CHUNKS_PER_BATCH);
            if limit == 0 {
                continue;
            }
            let mut batch = Vec::with_capacity(limit);
            for pos in view {
                if player.sent_chunks.contains(&pos) {
                    continue;
                }
                let chunk = match world.chunk(pos) {
                    Some(world::ChunkState::Loaded(chunk)) => Some(Arc::clone(chunk)),
                    Some(world::ChunkState::Missing) => None,
                    _ => continue,
                };
                let neighbors = if world.needs_light(pos) {
                    match world.light_neighbors(pos) {
                        Some(neighbors) => neighbors,
                        None if world.generates() => continue,
                        None => std::array::from_fn(|_| None),
                    }
                } else {
                    std::array::from_fn(|_| None)
                };
                player.sent_chunks.insert(pos);
                batch.push(player::VisibleChunk {
                    position: pos,
                    chunk,
                    neighbors,
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
    }

    fn connections(&mut self) {
        if self.stopped {
            return;
        }
        for state in &mut self.players {
            state.tick_start = state.player.position;
        }
        let mut moved = HashSet::new();
        let mut tracking_changed = false;
        for event in std::mem::take(&mut self.pending) {
            match event {
                Event::Joined {
                    entity_id,
                    profile,
                    view_distance,
                    initial_view_distance,
                    allow_flight,
                    output,
                } => {
                    self.join(
                        entity_id,
                        profile,
                        view_distance,
                        initial_view_distance,
                        allow_flight,
                        output,
                    );
                    tracking_changed = true;
                }
                Event::Left(entity_id) => self.leave(entity_id),
                Event::ListPlayers(reply) => {
                    let names = self
                        .players
                        .iter()
                        .filter(|p| !p.disconnecting)
                        .map(|p| p.player.profile.name.clone())
                        .collect();
                    let _ = reply.send(names);
                }
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
                        player::Input::ChatMessage(message) => {
                            if let Some(sender) = self
                                .players
                                .iter()
                                .find(|p| p.player.entity_id == entity_id && !p.disconnecting)
                            {
                                let sender = sender.player.profile.clone();
                                for player in &self.players {
                                    player.send(player::Output::ChatMessage {
                                        sender: sender.clone(),
                                        message: message.clone(),
                                    });
                                }
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
                            player::Input::ChatSession(_) | player::Input::ChatMessage(_) => {
                                unreachable!()
                            }
                            player::Input::Movement(movement) => {
                                if state.accepts(movement) {
                                    let center = world::ChunkPos::at(
                                        state.player.position.x,
                                        state.player.position.z,
                                    );
                                    state
                                        .sent_chunks
                                        .retain(|pos| pos.within(center, state.view_distance));
                                    moved.insert(entity_id);
                                    tracking_changed = true;
                                }
                            }
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
            state.check_flight();
        }
        if tracking_changed {
            self.update_tracking(&moved);
        }
        self.online_players.store(
            u32::try_from(self.players.len()).unwrap_or(u32::MAX),
            Ordering::Relaxed,
        );
    }

    fn autosave(&mut self) {
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
        view_distance: u8,
        initial_view_distance: u8,
        allow_flight: bool,
        output: Sender<player::Output>,
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
        if let Some(world) = &self.world {
            let position = world.spawn_position();
            player.player.position = position;
            player.tick_start = position;
        }
        player.view_distance = initial_view_distance.clamp(2, player.max_view_distance);
        for existing in &self.players {
            player.send(player::Output::AddPlayer(existing.player.profile.clone()));
            if let Some(session) = &existing.chat_session {
                player.send(player::Output::ChatSession {
                    uuid: existing.player.profile.uuid,
                    session: session.clone(),
                });
            }
            existing.send(player::Output::AddPlayer(player.player.profile.clone()));
        }
        self.players.push(player);
    }

    fn leave(&mut self, entity_id: i32) {
        let Some(index) = self
            .players
            .iter()
            .position(|p| p.player.entity_id == entity_id)
        else {
            return;
        };
        let left = self.players.remove(index);
        for player in &mut self.players {
            if player.tracked.remove(&entity_id) {
                player.send(player::Output::RemoveEntity(entity_id));
            }
            player.send(player::Output::RemovePlayer(left.player.profile.uuid));
        }
    }

    fn update_tracking(&mut self, moved: &HashSet<i32>) {
        let players: Vec<_> = self.players.iter().map(|p| p.player.clone()).collect();
        for viewer in &mut self.players {
            for target in &players {
                if viewer.player.entity_id == target.entity_id {
                    continue;
                }
                let visible = viewer.can_see(target);
                let tracked = viewer.tracked.contains(&target.entity_id);
                match (visible, tracked) {
                    (true, false) => {
                        viewer.tracked.insert(target.entity_id);
                        viewer.send(player::Output::Spawn(target.clone()));
                    }
                    (false, true) => {
                        viewer.tracked.remove(&target.entity_id);
                        viewer.send(player::Output::RemoveEntity(target.entity_id));
                    }
                    (true, true) if moved.contains(&target.entity_id) => {
                        viewer.send(player::Output::MoveEntity {
                            entity_id: target.entity_id,
                            position: target.position,
                        });
                    }
                    _ => {}
                }
            }
        }
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
        stopped: false,
    };
    let mut schedule = Schedule::new(Instant::now());
    let mut number = 0;
    loop {
        let now = Instant::now();
        if schedule.take_due(now) {
            number += 1;
            tick(&mut game, number);
            if game.stopped {
                break;
            }
        } else {
            // Wake for incoming events, but apply them only at the tick's connection phase. Check the
            // deadline after each event so a busy network cannot prevent ticks from running.
            match receiver.recv_timeout(schedule.next.saturating_duration_since(now)) {
                Ok(event) => game.pending.push(event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    }
    if let Some(world) = &mut game.world {
        world.flush()?;
    }
    Ok(())
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
            stopped: false,
        }
    }

    fn join(game: &mut Game, id: i32, name: &str, distance: u8) -> Receiver<Output> {
        let (output, receiver) = mpsc::channel();
        game.pending.push(Event::Joined {
            entity_id: id,
            profile: profile(name, id as u128),
            view_distance: distance,
            initial_view_distance: distance,
            allow_flight: false,
            output,
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
    fn players_get_profiles_spawns_movement_and_removal_in_order() {
        let mut game = game();
        let steve = join(&mut game, 1, "Steve", 10);
        let alex = join(&mut game, 2, "Alex", 10);
        tick(&mut game, 1);
        let steve_join: Vec<_> = steve.try_iter().collect();
        let alex_join: Vec<_> = alex.try_iter().collect();
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
    fn multiple_moves_in_one_tick_share_the_same_speed_budget() {
        let mut game = game();
        let output = join(&mut game, 1, "Steve", 10);
        input(&mut game, 1, Input::ConfirmTeleport(1));
        tick(&mut game, 1);
        input(&mut game, 1, movement(5.5, 0.0));
        input(&mut game, 1, movement(11.0, 0.0));
        tick(&mut game, 2);
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
        let (output, third) = mpsc::channel();
        game.pending.push(Event::Joined {
            entity_id: 3,
            profile: spectator,
            view_distance: 10,
            initial_view_distance: 10,
            allow_flight: false,
            output,
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
        let (output, _receiver) = mpsc::channel();
        let mut state = player::PlayerState::new(1, profile("Steve", 1), 10, false, output);
        state.awaiting_teleport = None;
        state.player.position.x = 29_999_999.0;
        state.player.position.y = 19_999_999.0;
        state.tick_start = state.player.position;
        assert!(state.accepts(Movement {
            position: Some([30_000_001.0, 20_000_001.0, 0.5]),
            rotation: None,
            on_ground: false,
        }));
        assert_eq!(state.player.position.x, 30_000_000.0);
        assert_eq!(state.player.position.y, 20_000_000.0);
    }

    #[test]
    fn sustained_flight_is_checked_and_honors_server_settings_and_game_mode() {
        fn state(mode: GameMode, allow_flight: bool) -> (player::PlayerState, Receiver<Output>) {
            let (sender, receiver) = mpsc::channel();
            let mut profile = profile("Steve", 1);
            profile.game_mode = mode;
            let mut state = player::PlayerState::new(1, profile, 10, allow_flight, sender);
            state.awaiting_teleport = None;
            state.accepts(Movement {
                position: Some([0.5, 65.0, 0.5]),
                rotation: None,
                on_ground: false,
            });
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
        falling.accepts(Movement {
            position: Some([0.5, 64.0, 0.5]),
            rotation: None,
            on_ground: false,
        });
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
            let (output, receiver) = mpsc::channel();
            let mut profile = profile("Steve", 1);
            profile.game_mode = GameMode::Survival;
            game.pending.push(Event::Joined {
                entity_id: 1,
                profile,
                view_distance: 10,
                initial_view_distance: 10,
                allow_flight,
                output,
            });
            input(&mut game, 1, Input::ConfirmTeleport(1));
            input(&mut game, 1, movement(0.5, 0.0));
            for number in 1..=81 {
                tick(&mut game, number);
            }
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
        let (output, new) = mpsc::channel();
        game.pending.push(Event::Joined {
            entity_id: 2,
            profile: profile("Steve", 1),
            view_distance: 10,
            initial_view_distance: 10,
            allow_flight: false,
            output,
        });
        tick(&mut game, 2);
        assert_eq!(
            old.try_recv(),
            Ok(Output::Disconnect("You logged in from another location"))
        );
        game.pending.push(Event::Left(1));
        tick(&mut game, 3);
        assert_eq!(game.online_players.load(Ordering::Relaxed), 1);
        assert!(new.try_recv().is_err());
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
