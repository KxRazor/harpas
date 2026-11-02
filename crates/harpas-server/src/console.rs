//! The server console: commands typed into the terminal, with history and completion
//!
//! When standard input is not a terminal, as under hosting panels and service managers, commands are read line by
//! line instead. Command output goes to the log, as in vanilla (`docs/operations.md`).

use std::io::{self, BufRead, IsTerminal, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use log::info;
use rustyline::completion::Completer;
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{Config, Context, Editor, Helper};

use crate::logging;
use crate::shutdown::Shutdown;

/// The console commands, in the order `help` lists them.
const COMMANDS: &[&str] = &["help", "list", "stop"];

/// What the console needs from the rest of the server.
pub struct Server {
    /// The logic thread, for the player list.
    pub logic: harpas_engine::Handle,
    /// `max-players`, for the player list.
    pub max_players: u32,
    /// For `stop`.
    pub shutdown: Arc<Shutdown>,
}

/// Whether the console is waiting for a line, with the terminal in raw mode.
static READING: AtomicBool = AtomicBool::new(false);

/// The terminal's settings before the console changed them.
#[cfg(unix)]
static SAVED_TERMINAL: std::sync::Mutex<Option<nix::sys::termios::Termios>> =
    std::sync::Mutex::new(None);

/// Puts the terminal back as it was if the process exits while the console is reading, as on SIGTERM; the
/// console itself does this only when a line ends.
pub fn restore_terminal() {
    if !READING.load(Ordering::SeqCst) {
        return;
    }
    #[cfg(unix)]
    if let Some(saved) = SAVED_TERMINAL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        let _ =
            nix::sys::termios::tcsetattr(io::stdin(), nix::sys::termios::SetArg::TCSANOW, saved);
    }
    // Leave the prompt's line, so that the shell's prompt starts on its own.
    let _ = io::stdout().write_all(b"\n");
}

/// Starts the console thread.
pub fn start(server: Server) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("Server console handler".to_owned())
        .spawn(move || {
            if io::stdin().is_terminal() {
                interactive(&server);
            } else {
                for line in io::stdin().lock().lines() {
                    let Ok(line) = line else { break };
                    run(&server, &line);
                }
            }
        })
}

fn interactive(server: &Server) {
    #[cfg(unix)]
    if let Ok(terminal) = nix::sys::termios::tcgetattr(io::stdin()) {
        *SAVED_TERMINAL.lock().unwrap_or_else(|e| e.into_inner()) = Some(terminal);
    }
    let config = Config::builder().auto_add_history(true).build();
    let mut editor = match Editor::<CommandHelper, DefaultHistory>::with_config(config) {
        Ok(editor) => editor,
        Err(e) => {
            log::error!("The console could not start: {e}");
            return;
        }
    };
    editor.set_helper(Some(CommandHelper));
    if let Ok(printer) = editor.create_external_printer() {
        logging::set_printer(Box::new(printer));
    }
    loop {
        READING.store(true, Ordering::SeqCst);
        let line = editor.readline("> ");
        READING.store(false, Ordering::SeqCst);
        match line {
            Ok(line) => run(server, &line),
            // Ctrl-C stops the server, as it does without an interactive console.
            Err(ReadlineError::Interrupted) => run(server, "stop"),
            // Ctrl-D or a closed terminal: the server keeps running without a console, as vanilla's does.
            Err(_) => break,
        }
    }
}

/// Runs one console command.
fn run(server: &Server, line: &str) {
    let line = line.trim();
    let command = line.strip_prefix('/').unwrap_or(line);
    match command.split_whitespace().next() {
        None => {}
        Some("help") => {
            for name in COMMANDS {
                info!("/{name}");
            }
        }
        Some("list") => match server.logic.player_names() {
            Ok(names) => info!(
                "There are {} of a max of {} players online: {}",
                names.len(),
                server.max_players,
                names.join(", ")
            ),
            Err(e) => log::error!("Could not list the players: {e}"),
        },
        Some("stop") => server.shutdown.stop(),
        Some(_) => {
            info!("Unknown or incomplete command. See below for error");
            info!("{command}<--[HERE]");
        }
    }
}

/// Completes command names.
struct CommandHelper;

impl Completer for CommandHelper {
    type Candidate = String;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<String>)> {
        Ok(complete(&line[..pos]))
    }
}

/// The start of the word being completed and the commands that complete it.
fn complete(typed: &str) -> (usize, Vec<String>) {
    let start = usize::from(typed.starts_with('/'));
    let word = &typed[start..];
    if word.contains(char::is_whitespace) {
        return (start, Vec::new());
    }
    let candidates = COMMANDS
        .iter()
        .filter(|name| name.starts_with(word))
        .map(|&name| name.to_owned())
        .collect();
    (start, candidates)
}

impl Hinter for CommandHelper {
    type Hint = String;
}

impl Highlighter for CommandHelper {}

impl Validator for CommandHelper {}

impl Helper for CommandHelper {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_command_names() {
        assert_eq!(
            complete(""),
            (0, vec!["help".into(), "list".into(), "stop".into()])
        );
        assert_eq!(complete("st"), (0, vec!["stop".into()]));
        assert_eq!(complete("/l"), (1, vec!["list".into()]));
        assert_eq!(complete("list "), (0, Vec::new()));
        assert_eq!(complete("x"), (0, Vec::new()));
    }
}
