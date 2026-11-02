//! The Harpas server binary.
//!
//! Listens for Java Edition connections on `server-ip` and `server-port` from `server.properties`, answers the
//! server list, logs players in (online mode through Mojang's session server, or offline mode) and brings them
//! into a void world. The engine runs on one logic thread; there is one thread per connection for now. The
//! network runtime comes with the protection against abusive connections (`docs/java-connection.md`).
//!
//! Design: `docs/operations.md`.

mod properties;
mod session;

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{fs, thread};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use harpas_java::{AuthOutcome, Connection, ServerInfo, ServerKey, State};
use harpas_protocol_java::v26_1::play::GameMode;

use properties::Properties;
use session::{SessionServer, format_uuid};

/// Vanilla closes a connection that sends nothing for 30 seconds.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// How often a connection's timed work runs, such as Keep Alive.
const CONNECTION_POLL_INTERVAL: Duration = Duration::from_secs(1);

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
    println!("Generating keypair");
    let key = Arc::new(ServerKey::generate());
    let session = if info.online_mode {
        Some(Arc::new(SessionServer::new().map_err(io::Error::other)?))
    } else {
        println!("**** SERVER IS RUNNING IN OFFLINE/INSECURE MODE!");
        println!("The server will make no attempt to authenticate usernames. Beware.");
        None
    };
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
    let (logic, logic_thread) = harpas_engine::start()?;
    println!("Listening for Java Edition on {}", listener.local_addr()?);
    for stream in listener.incoming() {
        if logic_thread.is_finished() {
            return Err(io::Error::other("logic thread stopped"));
        }
        let stream = match stream {
            Ok(stream) => stream,
            Err(e) => {
                eprintln!("could not accept a connection: {e}");
                continue;
            }
        };
        let info = Arc::clone(&info);
        let key = Arc::clone(&key);
        let session = session.clone();
        let logic = logic.clone();
        thread::spawn(move || {
            let peer = stream
                .peer_addr()
                .map_or_else(|_| "unknown".to_owned(), |a| a.to_string());
            if let Err(e) = serve(stream, &info, key, session.as_deref(), &logic) {
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
        compression_threshold: properties.parsed(
            "network-compression-threshold",
            defaults.compression_threshold,
        ),
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
fn serve(
    mut stream: TcpStream,
    info: &ServerInfo,
    key: Arc<ServerKey>,
    session: Option<&SessionServer>,
    logic: &harpas_engine::Handle,
) -> io::Result<()> {
    // Reads wake up every second so that the connection can send its Keep Alive packets.
    stream.set_read_timeout(Some(CONNECTION_POLL_INTERVAL))?;
    stream.set_nodelay(true)?;
    let result = drive(&mut stream, info, key, session, logic);
    let _ = stream.shutdown(Shutdown::Both);
    result
}

/// Drives the I/O-free connection state machine over a stream.
fn drive(
    stream: &mut (impl Read + Write),
    info: &ServerInfo,
    key: Arc<ServerKey>,
    session: Option<&SessionServer>,
    logic: &harpas_engine::Handle,
) -> io::Result<()> {
    let mut connection = Connection::new(key);
    let mut info = info.clone();
    let mut buf = vec![0; 16 * 1024];
    let mut last_read = Instant::now();
    let mut player_session = None;
    while !connection.is_closed() {
        let result = match stream.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(read) => {
                last_read = Instant::now();
                info.online_players = logic.online_players();
                connection.receive(&buf[..read], &info)
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
        if let Some(request) = connection.take_auth_request() {
            let outcome = session.map_or(AuthOutcome::Unavailable, |s| s.has_joined(&request));
            if let AuthOutcome::Verified(player) = &outcome {
                println!(
                    "UUID of player {} is {}",
                    player.name,
                    format_uuid(player.uuid)
                );
            }
            connection.authenticated(outcome, &info);
            stream.write_all(&connection.take_output())?;
        }
        result.map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if player_session.is_none() && connection.state() == State::Play {
            player_session = Some(logic.player_joined().map_err(io::Error::other)?);
            if let Some(player) = connection.player() {
                println!("{} joined the void world", player.name);
            }
        }
    }
    stream.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::mpsc::{self, Receiver};

    use super::*;
    use harpas_java::{FrameReader, write_frame};
    use harpas_protocol_java::v26_1::{configuration, handshaking, login, status};

    #[derive(Default)]
    struct Stream {
        input: VecDeque<Vec<u8>>,
        output: Vec<u8>,
        close: Option<Receiver<bool>>,
    }

    impl Read for Stream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if let Some(bytes) = self.input.pop_front() {
                buf[..bytes.len()].copy_from_slice(&bytes);
                return Ok(bytes.len());
            }
            if let Some(close) = self.close.take()
                && close.recv().unwrap_or(false)
            {
                return Err(io::Error::other("connection failed"));
            }
            Ok(0)
        }
    }

    impl Write for Stream {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn frame(encode: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, encode);
        bytes
    }

    fn handshake(intent: handshaking::Intent) -> Vec<u8> {
        let packet: handshaking::serverbound::Packet = handshaking::serverbound::Handshake {
            protocol_version: 775,
            server_address: "localhost".into(),
            server_port: 25565,
            intent,
        }
        .into();
        frame(|buf| packet.encode(buf))
    }

    fn login_packets() -> VecDeque<Vec<u8>> {
        let start: login::serverbound::Packet = login::serverbound::LoginStart {
            username: "Steve".into(),
            player_uuid: 0,
        }
        .into();
        let acknowledged: login::serverbound::Packet = login::serverbound::LoginAcknowledged.into();
        let packs: configuration::serverbound::Packet =
            configuration::serverbound::SelectKnownPacks {
                packs: vec![configuration::KnownPack {
                    namespace: "minecraft".into(),
                    id: "core".into(),
                    version: "26.1".into(),
                }],
            }
            .into();
        let finish: configuration::serverbound::Packet =
            configuration::serverbound::AcknowledgeFinishConfiguration.into();
        VecDeque::from([
            handshake(handshaking::Intent::Login),
            frame(|buf| start.encode(buf)),
            frame(|buf| acknowledged.encode(buf)),
            frame(|buf| packs.encode(buf)),
            frame(|buf| finish.encode(buf)),
        ])
    }

    fn status_count(info: &ServerInfo, key: &Arc<ServerKey>, logic: &harpas_engine::Handle) -> u64 {
        let request: status::serverbound::Packet = status::serverbound::StatusRequest.into();
        let ping: status::serverbound::Packet =
            status::serverbound::PingRequest { timestamp: 1 }.into();
        let mut stream = Stream {
            input: VecDeque::from([
                handshake(handshaking::Intent::Status),
                frame(|buf| request.encode(buf)),
                frame(|buf| ping.encode(buf)),
            ]),
            ..Stream::default()
        };
        drive(&mut stream, info, Arc::clone(key), None, logic).unwrap();
        let mut frames = FrameReader::default();
        frames.push(&stream.output);
        let packet =
            status::clientbound::Packet::decode(&frames.next_frame().unwrap().unwrap()).unwrap();
        let status::clientbound::Packet::StatusResponse(response) = packet else {
            panic!("expected a Status Response");
        };
        let json: serde_json::Value = serde_json::from_str(&response.json).unwrap();
        json["players"]["online"].as_u64().unwrap()
    }

    #[test]
    fn status_tracks_players_through_join_and_disconnect() {
        fn wait_for_count(logic: &harpas_engine::Handle, expected: u32) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while logic.online_players() != expected {
                assert!(
                    Instant::now() < deadline,
                    "player count did not reach {expected}"
                );
                thread::sleep(Duration::from_millis(5));
            }
        }

        let info = ServerInfo {
            online_mode: false,
            compression_threshold: -1,
            view_distance: 2,
            ..ServerInfo::default()
        };
        let key = Arc::new(ServerKey::generate());
        let (logic, logic_thread) = harpas_engine::start().unwrap();
        assert_eq!(status_count(&info, &key, &logic), 0);

        // Both a clean EOF and an I/O error must remove the player, including early returns from drive.
        for error in [false, true] {
            let (close, receiver) = mpsc::channel();
            let (client_info, client_key, client_logic) =
                (info.clone(), Arc::clone(&key), logic.clone());
            let connection = thread::spawn(move || {
                let mut stream = Stream {
                    input: login_packets(),
                    close: Some(receiver),
                    ..Stream::default()
                };
                drive(&mut stream, &client_info, client_key, None, &client_logic)
            });
            wait_for_count(&logic, 1);
            assert_eq!(status_count(&info, &key, &logic), 1);
            close.send(error).unwrap();
            assert_eq!(connection.join().unwrap().is_err(), error);
            wait_for_count(&logic, 0);
            assert_eq!(status_count(&info, &key, &logic), 0);
        }
        drop(logic);
        logic_thread.join().unwrap();
    }
}
