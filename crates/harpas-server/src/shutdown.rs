//! Stopping the server cleanly, from the console's `stop`, Ctrl-C, SIGINT, SIGTERM or SIGHUP
//!
//! Every player is disconnected with vanilla's "Server closed", the logic thread stops, connections get a moment
//! to send their last packets, the world is saved, and the log is flushed (`docs/operations.md`).

use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use log::{error, info, warn};

use crate::console;

/// How long connections have to close after the players are disconnected.
const CONNECTION_GRACE: Duration = Duration::from_secs(5);

/// What a shutdown has to stop.
pub struct Shutdown {
    stopping: AtomicBool,
    logic: harpas_engine::Handle,
    logic_thread: Mutex<Option<JoinHandle<io::Result<()>>>>,
    connections: AtomicUsize,
}

impl Shutdown {
    /// Takes over the logic thread, which the shutdown stops and waits for.
    pub fn new(
        logic: harpas_engine::Handle,
        logic_thread: JoinHandle<io::Result<()>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            stopping: AtomicBool::new(false),
            logic,
            logic_thread: Mutex::new(Some(logic_thread)),
            connections: AtomicUsize::new(0),
        })
    }

    /// Stops the server on SIGINT, SIGTERM and SIGHUP, and on Ctrl-C and closing the console on Windows.
    pub fn handle_signals(self: &Arc<Self>) -> io::Result<()> {
        let shutdown = Arc::clone(self);
        ctrlc::set_handler(move || shutdown.stop()).map_err(io::Error::other)
    }

    /// Whether a shutdown has started.
    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// Whether the logic thread has ended without a shutdown.
    pub fn logic_failed(&self) -> bool {
        !self.is_stopping()
            && self
                .logic_thread
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .is_some_and(JoinHandle::is_finished)
    }

    /// Why the logic thread ended without a shutdown. Call once [`logic_failed`](Self::logic_failed) is true:
    /// this waits for the thread.
    pub fn logic_failure(&self) -> io::Error {
        let logic_thread = self
            .logic_thread
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        match logic_thread.map(JoinHandle::join) {
            Some(Ok(Err(error))) => error,
            Some(Err(_)) => io::Error::other("The logic thread panicked"),
            Some(Ok(Ok(()))) | None => io::Error::other("The logic thread stopped"),
        }
    }

    /// Counts a connection until the returned guard is dropped, so that the shutdown can wait for it.
    pub fn connection(self: &Arc<Self>) -> ConnectionGuard {
        self.connections.fetch_add(1, Ordering::SeqCst);
        ConnectionGuard(Arc::clone(self))
    }

    /// Stops the server and exits the process. If a shutdown is already running, returns at once.
    pub fn stop(&self) {
        if self.stopping.swap(true, Ordering::SeqCst) {
            return;
        }
        info!("Stopping the server");
        // The logic thread may already have stopped; then there is nobody left to disconnect.
        let _ = self.logic.shutdown("Server closed");
        let logic_thread = self
            .logic_thread
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let exit_code = match logic_thread.map(JoinHandle::join) {
            Some(Ok(Ok(()))) | None => 0,
            Some(Ok(Err(error))) => {
                error!("Could not finish saving the world: {error}");
                1
            }
            Some(Err(_)) => {
                error!("The logic thread panicked during shutdown");
                1
            }
        };
        let open = self.wait_for_connections(CONNECTION_GRACE);
        if open > 0 {
            warn!("{open} connections did not close in time");
        }
        console::restore_terminal();
        log::logger().flush();
        std::process::exit(exit_code);
    }

    /// Waits until every connection has closed or `grace` has passed; returns how many are still open.
    fn wait_for_connections(&self, grace: Duration) -> usize {
        let deadline = Instant::now() + grace;
        loop {
            let open = self.connections.load(Ordering::SeqCst);
            if open == 0 || Instant::now() >= deadline {
                return open;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

/// A connection the shutdown waits for.
pub struct ConnectionGuard(Arc<Shutdown>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_for_connections_until_the_grace_period_ends() {
        let (logic, logic_thread) = harpas_engine::start().unwrap();
        let shutdown = Shutdown::new(logic, logic_thread);
        assert!(!shutdown.logic_failed());
        let first = shutdown.connection();
        let second = shutdown.connection();
        assert_eq!(shutdown.wait_for_connections(Duration::from_millis(20)), 2);
        drop(first);
        let closing = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            drop(second);
        });
        assert_eq!(shutdown.wait_for_connections(Duration::from_secs(5)), 0);
        closing.join().unwrap();
    }
}
