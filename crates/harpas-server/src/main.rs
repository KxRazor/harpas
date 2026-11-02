//! The Harpas server binary.
//!
//! Listens for Java Edition connections on `server-ip` and `server-port` from `server.properties`, answers the
//! server list, and brings offline-mode players into a void world. One thread per connection for now; the
//! network runtime comes with the protection against abusive connections (`docs/java-connection.md`).
//!
//! Design: `docs/operations.md`.

mod properties;

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{fs, thread};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use harpas_java::{Connection, ServerInfo, State};
use harpas_protocol_java::v26_1::play::GameMode;

use properties::Properties;

/// Vanilla closes a connection that sends nothing for 30 seconds.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// How often a connection's timed work runs, such as Keep Alive.
const TICK: Duration = Duration::from_secs(1);

fn main() {
    println!("harpas {}", env!("CARGO_PKG_VERSION"));
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    let properties = Properties::load(Path::new("server.properties"))?;
    let info = Arc::new(server_info(&properties));
    let ip = properties.string("server-ip", "");
    let port: u16 = properties.parsed("server-port", 25565);
    // An empty `server-ip` listens on every address, as in vanilla.
    let address = if ip.is_empty() {
        format!("[::]:{port}")
    } else if ip.contains(':') && !ip.starts_with('[') {
        format!("[{ip}]:{port}")
    } else {
        format!("{ip}:{port}")
    };
    let listener = TcpListener::bind(&address).or_else(|e| {
        // Systems without IPv6 still get every IPv4 address.
        if ip.is_empty() {
            TcpListener::bind(("0.0.0.0", port))
        } else {
            Err(e)
        }
    })?;
    println!("Listening for Java Edition on {}", listener.local_addr()?);
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(e) => {
                eprintln!("could not accept a connection: {e}");
                continue;
            }
        };
        let info = Arc::clone(&info);
        thread::spawn(move || {
            let peer = stream
                .peer_addr()
                .map_or_else(|_| "unknown".to_owned(), |a| a.to_string());
            if let Err(e) = serve(stream, &info) {
                println!("{peer} disconnected: {e}");
            }
        });
    }
    Ok(())
}

/// The server's settings and list entry, from `server.properties` and `server-icon.png`.
fn server_info(properties: &Properties) -> ServerInfo {
    let defaults = ServerInfo::default();
    ServerInfo {
        motd: properties.string("motd", &defaults.motd),
        max_players: properties.parsed("max-players", defaults.max_players),
        online_players: 0,
        favicon: load_icon(Path::new("server-icon.png")),
        enforces_secure_chat: properties
            .parsed("enforce-secure-profile", defaults.enforces_secure_chat),
        accepts_transfers: properties.parsed("accepts-transfers", defaults.accepts_transfers),
        online_mode: properties.parsed("online-mode", defaults.online_mode),
        view_distance: properties.parsed("view-distance", defaults.view_distance),
        simulation_distance: properties.parsed("simulation-distance", defaults.simulation_distance),
        game_mode: properties
            .get("gamemode")
            .and_then(parse_game_mode)
            .unwrap_or(defaults.game_mode),
    }
}

/// A game mode by name or number, as vanilla accepts it in `gamemode`.
fn parse_game_mode(value: &str) -> Option<GameMode> {
    match value.trim() {
        "survival" | "0" => Some(GameMode::Survival),
        "creative" | "1" => Some(GameMode::Creative),
        "adventure" | "2" => Some(GameMode::Adventure),
        "spectator" | "3" => Some(GameMode::Spectator),
        _ => None,
    }
}

/// The server icon as a data URI. Like vanilla, it must be a 64 by 64 PNG; anything else is ignored.
fn load_icon(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    // A PNG starts with an 8-byte signature and the IHDR chunk, whose first fields are the width and height.
    let png = bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.get(12..16) == Some(b"IHDR");
    let size = |at: usize| {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_be_bytes(b.try_into().unwrap()))
    };
    if !png || size(16) != Some(64) || size(20) != Some(64) {
        eprintln!(
            "{} must be a 64x64 PNG image; it is not shown",
            path.display()
        );
        return None;
    }
    Some(format!("data:image/png;base64,{}", STANDARD.encode(&bytes)))
}

/// Runs one connection until it is closed.
fn serve(mut stream: TcpStream, info: &ServerInfo) -> io::Result<()> {
    // Reads wake up every second so that the connection can send its Keep Alive packets.
    stream.set_read_timeout(Some(TICK))?;
    stream.set_nodelay(true)?;
    let mut connection = Connection::new();
    let mut buf = vec![0; 16 * 1024];
    let mut last_read = Instant::now();
    let mut player_logged = false;
    while !connection.is_closed() {
        let result = match stream.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(read) => {
                last_read = Instant::now();
                connection.receive(&buf[..read], info)
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                if last_read.elapsed() >= READ_TIMEOUT {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "read timed out"));
                }
                Ok(())
            }
            Err(e) => return Err(e),
        };
        let result = result.and_then(|()| connection.tick(Instant::now()));
        stream.write_all(&connection.take_output())?;
        result.map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if !player_logged && connection.state() == State::Play {
            player_logged = true;
            if let Some(player) = connection.player() {
                println!("{} joined the void world", player.name);
            }
        }
    }
    stream.flush()?;
    let _ = stream.shutdown(Shutdown::Both);
    Ok(())
}
