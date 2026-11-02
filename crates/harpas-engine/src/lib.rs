//! Harpas engine: vanilla tick order on one logic thread
//!
//! Design: `docs/engine-core.md`.

use std::fmt;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const TICK_INTERVAL: Duration = Duration::from_millis(50);
const MAX_LAG: Duration = Duration::from_secs(2);
const AUTOSAVE_INTERVAL: u64 = 6_000;

/// A handle for sending connection events to the logic thread.
#[derive(Clone, Debug)]
pub struct Handle {
    events: Sender<Event>,
    online_players: Arc<AtomicU32>,
}

impl Handle {
    /// Registers a connection that has reached Play. Dropping the returned token queues its departure.
    ///
    /// Both events are applied during a later packet phase; this token is not a player entity.
    pub fn player_joined(&self) -> Result<PlayerSession, LogicStopped> {
        self.events.send(Event::Joined).map_err(|_| LogicStopped)?;
        Ok(PlayerSession {
            events: self.events.clone(),
        })
    }

    /// The number of connections in Play as of the last completed packet phase.
    pub fn online_players(&self) -> u32 {
        self.online_players.load(Ordering::Relaxed)
    }
}

/// A connection in Play; dropping it tells the logic thread that the connection has ended.
#[derive(Debug)]
pub struct PlayerSession {
    events: Sender<Event>,
}

impl Drop for PlayerSession {
    fn drop(&mut self) {
        // If the logic thread has stopped, there is no state left to update.
        let _ = self.events.send(Event::Left);
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

/// Starts the single logic thread. It stops once all handles and player sessions have been dropped.
pub fn start() -> io::Result<(Handle, JoinHandle<()>)> {
    let (sender, receiver) = mpsc::channel();
    let online_players = Arc::new(AtomicU32::new(0));
    let handle = Handle {
        events: sender,
        online_players: Arc::clone(&online_players),
    };
    let thread = thread::Builder::new()
        .name("harpas-logic".to_owned())
        .spawn(move || run(receiver, online_players))?;
    Ok((handle, thread))
}

#[derive(Debug)]
enum Event {
    Joined,
    Left,
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

/// The phases that exist in vanilla's tick order. Only connection bookkeeping has work so far.
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
    players: u32,
    online_players: Arc<AtomicU32>,
}

impl TickWork for Game {
    fn connections(&mut self) {
        for event in self.pending.drain(..) {
            match event {
                Event::Joined => self.players += 1,
                Event::Left => self.players -= 1,
            }
        }
        self.online_players.store(self.players, Ordering::Relaxed);
    }
}

fn run(receiver: Receiver<Event>, online_players: Arc<AtomicU32>) {
    let mut game = Game {
        pending: Vec::new(),
        players: 0,
        online_players,
    };
    let mut schedule = Schedule::new(Instant::now());
    let mut number = 0;
    loop {
        let now = Instant::now();
        if schedule.take_due(now) {
            number += 1;
            tick(&mut game, number);
        } else {
            // Wake for incoming events, but apply them only at the tick's connection phase. Check the
            // deadline after each event so a busy network cannot prevent ticks from running.
            match receiver.recv_timeout(schedule.next.saturating_duration_since(now)) {
                Ok(event) => game.pending.push(event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let online_players = Arc::new(AtomicU32::new(0));
        let mut game = Game {
            pending: vec![Event::Joined, Event::Left, Event::Joined],
            players: 0,
            online_players: Arc::clone(&online_players),
        };
        assert_eq!(online_players.load(Ordering::Relaxed), 0);
        tick(&mut game, 1);
        assert_eq!(online_players.load(Ordering::Relaxed), 1);
        game.pending.push(Event::Left);
        assert_eq!(online_players.load(Ordering::Relaxed), 1);
        tick(&mut game, 2);
        assert_eq!(online_players.load(Ordering::Relaxed), 0);
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
        let first = handle.player_joined().unwrap();
        let second = handle.player_joined().unwrap();
        wait_for_count(&handle, 2);
        drop(first);
        wait_for_count(&handle, 1);
        drop(second);
        wait_for_count(&handle, 0);
        drop(handle);
        logic.join().unwrap();
    }
}
