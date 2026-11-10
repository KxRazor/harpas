//! The Harpas server binary.
//!
//! Listens for Java Edition connections on `server-ip` and `server-port` from `server.properties`, answers the
//! server list, logs players in (online mode through Mojang's session server, or offline mode) and brings them
//! into the configured overworld. The engine runs on one logic thread; there is one thread per connection for now. The
//! network runtime comes with the protection against abusive connections (`docs/java-connection.md`). Settings
//! come from `server.properties`, `harpas.toml` and `HARPAS_*` environment variables; the log goes to the
//! console and `logs/latest.log`. The server starts only once the owner has accepted Mojang's EULA.
//!
//! Design: `docs/operations.md`.

mod block_entity_registry;
mod chunks;
mod console;
mod eula;
mod harpas_toml;
mod http;
mod limits;
mod logging;
mod player;
mod properties;
mod session;
mod shutdown;
mod vanilla_data;
mod world_generation;

use std::io::{self, Read, Write};
use std::net::{self, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{fs, thread};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use harpas_java::{AuthOutcome, Connection, ServerInfo, ServerKey, State};
use harpas_protocol_java::v26_1::play::GameMode;
use log::{error, info, warn};

use limits::{Limits, Permit};
use properties::Properties;
use session::{SessionServer, format_uuid};
use shutdown::Shutdown;

/// Vanilla closes a connection that sends nothing for 30 seconds.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Wake idle connections to send messages from the logic thread promptly.
const CONNECTION_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Revision of PrismarineJS/minecraft-data the Java codecs were built from, as in
/// `crates/harpas-protocol-java/schema/26.1/version.toml`.
const MINECRAFT_DATA_REVISION: &str = "bd8b5c6048e5af9694374b83f25f5b4aade619cc";

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Run,
    Version {
        verbose: bool,
    },
    /// Extract the vanilla data from a server jar the owner downloaded.
    InstallVanillaData {
        from: PathBuf,
    },
    /// Generate the block entity registry from an optional owner-provided jar.
    GenerateRegistry {
        from: Option<PathBuf>,
    },
}

fn parse_args(args: &[String]) -> Result<Command, String> {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        // Vanilla's `nogui` closes its window; Harpas has none, and panels pass it anyway.
        [] | ["nogui" | "--nogui"] => Ok(Command::Run),
        ["--version" | "-V"] => Ok(Command::Version { verbose: false }),
        ["--version" | "-V", "--verbose"] | ["--verbose", "--version" | "-V"] => {
            Ok(Command::Version { verbose: true })
        }
        ["vanilla-data", "install", "--from", jar] => Ok(Command::InstallVanillaData {
            from: PathBuf::from(jar),
        }),
        ["vanilla-data", "generate-registry"] => Ok(Command::GenerateRegistry { from: None }),
        ["vanilla-data", "generate-registry", "--from", jar] => Ok(Command::GenerateRegistry {
            from: Some(PathBuf::from(jar)),
        }),
        _ => Err("usage: harpas [nogui] | harpas --version [--verbose] | \
                  harpas vanilla-data install --from <server.jar> | \
                  harpas vanilla-data generate-registry [--from <server.jar>]"
            .to_owned()),
    }
}

/// The Harpas version with the Minecraft versions it supports as build metadata (`docs/releases.md`).
fn version() -> String {
    format!(
        "{}+{}",
        env!("CARGO_PKG_VERSION"),
        harpas_protocol_java::v26_1::MINECRAFT_VERSION
    )
}

/// What `--version --verbose` declares, as the release notes do (`docs/releases.md`).
fn verbose_version() -> String {
    use harpas_protocol_java::v26_1::{MINECRAFT_VERSION, PROTOCOL};
    format!(
        "harpas {}\n\
         Server version: Java {MINECRAFT_VERSION}\n\
         Java clients: {MINECRAFT_VERSION} (protocol {PROTOCOL})\n\
         Bedrock clients: not supported yet\n\
         Native plugin API: not supported yet\n\
         Protocol data: PrismarineJS/minecraft-data {MINECRAFT_DATA_REVISION}",
        version()
    )
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match parse_args(&args) {
        Ok(
            command @ (Command::Run
            | Command::InstallVanillaData { .. }
            | Command::GenerateRegistry { .. }),
        ) => command,
        // Before the log opens, so that asking for the version leaves no files behind.
        Ok(Command::Version { verbose: false }) => return println!("harpas {}", version()),
        Ok(Command::Version { verbose: true }) => return println!("{}", verbose_version()),
        Err(usage) => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
    };
    if let Err(e) = logging::init(Path::new("logs")) {
        eprintln!("Could not open logs/latest.log: {e}");
        std::process::exit(1);
    }
    let result = match command {
        Command::InstallVanillaData { from } => install_vanilla_data(&from),
        Command::GenerateRegistry { from } => generate_registry(from.as_deref()),
        _ => run(),
    };
    if let Err(e) = result {
        error!("{e}");
        log::logger().flush();
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    let started = Instant::now();
    info!(
        "Starting Harpas {} for Minecraft {}",
        version(),
        harpas_protocol_java::v26_1::MINECRAFT_VERSION
    );
    info!("Loading properties");
    let properties = load_properties(Path::new("server.properties"))?;
    let settings = harpas_toml::load(Path::new("harpas.toml"))?;
    for warning in harpas_toml::check(&settings) {
        warn!("{warning}");
    }
    // Every start needs Minecraft's game data, downloaded from Mojang (`docs/world.md`).
    eula::require(Path::new(""), &settings)?;
    let data_pack = vanilla_data::provision(Path::new(""))?;
    let block_entities = match block_entity_registry::load(Path::new("")) {
        Ok(Some(registry)) => registry,
        result => {
            if let Err(error) = result {
                warn!("Could not load block entity registry: {error}");
            }
            warn!(
                "Block entity updates are unavailable. Install Java 25 and run `harpas vanilla-data generate-registry`"
            );
            block_entity_registry::Registry::default()
        }
    };
    let world_dir = PathBuf::from(properties.string("level-name", "world"));
    check_world(&world_dir)?;
    let features = harpas_toml::generation_features(&settings)?;
    let generator = world_generation::prepare(&world_dir, &properties, &data_pack, features)?;
    let mut server_info = server_info(&properties);
    if let Some((level, _)) = harpas_anvil::read_level(&world_dir)?
        && let Some(spawn) = harpas_anvil::overworld_spawn(&level)
    {
        server_info.spawn_position = spawn;
    }
    if let Some((seed, _)) = harpas_anvil::read_world_seed(&world_dir)? {
        server_info.hashed_seed = harpas_worldgen::BiomeManager::obfuscate_seed(seed);
    }
    if generator.is_some() {
        server_info.is_flat = false;
    }
    let environment = Arc::new(ConnectionEnvironment {
        info: server_info,
        block_entities,
    });
    let info = &environment.info;
    info!("Default game type: {:?}", info.game_mode);
    info!("Generating keypair");
    let key = Arc::new(ServerKey::generate());
    let session = if info.online_mode {
        Some(Arc::new(SessionServer::new().map_err(io::Error::other)?))
    } else {
        warn!("**** SERVER IS RUNNING IN OFFLINE/INSECURE MODE!");
        warn!("The server will make no attempt to authenticate usernames. Beware.");
        None
    };
    let ip = properties.string("server-ip", "");
    let port: u16 = properties.parsed("server-port", 25565);
    let listeners = listen(&ip, port)?;
    // Poll the listeners so an asynchronous fatal world error also stops an otherwise idle server.
    for listener in &listeners {
        listener.set_nonblocking(true)?;
    }
    let (logic, logic_thread) = match generator {
        Some(source) => {
            harpas_engine::start_with_world_source(&world_dir, harpas_java::biome_names(), source)?
        }
        None => harpas_engine::start_with_world(&world_dir, harpas_java::biome_names())?,
    };
    let shutdown = Shutdown::new(logic.clone(), logic_thread);
    shutdown.handle_signals()?;
    for listener in &listeners {
        info!("Listening for Java Edition on {}", listener.local_addr()?);
    }
    console::start(console::Server {
        logic: logic.clone(),
        max_players: info.max_players,
        shutdown: Arc::clone(&shutdown),
    })?;
    info!(
        "Done ({:.3}s)! For help, type \"help\"",
        started.elapsed().as_secs_f64()
    );
    let limits = Arc::new(Limits::default());
    loop {
        // Once stopping, new connections are closed until the process exits.
        if shutdown.is_stopping() {
            thread::sleep(CONNECTION_POLL_INTERVAL);
            continue;
        }
        if shutdown.logic_failed() {
            return Err(io::Error::other("logic thread stopped"));
        }
        let Some(stream) = accept(&listeners) else {
            thread::sleep(CONNECTION_POLL_INTERVAL);
            continue;
        };
        if let Err(e) = stream.set_nonblocking(false) {
            warn!("Could not configure a connection: {e}");
            continue;
        }
        let Ok(peer) = stream.peer_addr() else {
            continue;
        };
        // Over a limit, the connection is closed before it costs a thread.
        let Some(mut permit) = limits.connect(peer.ip(), Instant::now()) else {
            continue;
        };
        let environment = Arc::clone(&environment);
        let key = Arc::clone(&key);
        let session = session.clone();
        let logic = logic.clone();
        let guard = shutdown.connection();
        let spawned = thread::Builder::new()
            .name(format!("Connection {peer}"))
            .spawn(move || {
                let session = session.as_deref();
                if let Err(e) = serve(stream, &environment, key, session, &logic, &mut permit) {
                    info!("{peer} disconnected: {e}");
                }
                drop(guard);
            });
        if let Err(e) = spawned {
            warn!("Could not start a thread for {peer}: {e}");
        }
    }
}

/// The listeners for `server-ip` and `server-port`. An empty `server-ip` listens on every address, as in vanilla.
fn listen(ip: &str, port: u16) -> io::Result<Vec<TcpListener>> {
    if !ip.is_empty() {
        let address = if ip.contains(':') && !ip.starts_with('[') {
            format!("[{ip}]:{port}")
        } else {
            format!("{ip}:{port}")
        };
        return Ok(vec![TcpListener::bind(address)?]);
    }
    // Systems without IPv6 still get every IPv4 address.
    let Ok(all) = TcpListener::bind(("::", port)) else {
        return Ok(vec![TcpListener::bind(("0.0.0.0", port))?]);
    };
    // On Linux the IPv6 listener takes IPv4 connections too, and the IPv4 port is then in use. On Windows it
    // takes only IPv6 ones, so IPv4 needs a listener of its own on the same port.
    let port = all.local_addr()?.port();
    let mut listeners = vec![all];
    if let Ok(ipv4) = TcpListener::bind(("0.0.0.0", port)) {
        listeners.push(ipv4);
    }
    Ok(listeners)
}

/// The next connection waiting on any of the non-blocking `listeners`, if there is one.
fn accept(listeners: &[TcpListener]) -> Option<TcpStream> {
    for listener in listeners {
        match listener.accept() {
            Ok((stream, _)) => return Some(stream),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => warn!("Could not accept a connection: {e}"),
        }
    }
    None
}

/// Refuses to start on a world another Minecraft version saved (`docs/world.md#supported-world-versions`). A
/// folder without `level.dat` has no world yet.
fn check_world(world: &Path) -> io::Result<()> {
    let Some((data, source)) = harpas_anvil::read_level(world)? else {
        return Ok(());
    };
    if source == harpas_anvil::Source::Backup {
        warn!(
            "{} is missing or damaged; using level.dat_old",
            world.join("level.dat").display()
        );
    }
    harpas_anvil::check_level(&data)
        .map_err(|e| io::Error::other(format!("Cannot open {}: {e}", world.display())))
}

/// `harpas vanilla-data install --from <jar>`: Mojang's files need the EULA even when the owner provides them.
fn install_vanilla_data(jar: &Path) -> io::Result<()> {
    let settings = harpas_toml::load(Path::new("harpas.toml"))?;
    eula::require(Path::new(""), &settings)?;
    vanilla_data::install_from(Path::new(""), jar)?;
    Ok(())
}

fn generate_registry(jar: Option<&Path>) -> io::Result<()> {
    let settings = harpas_toml::load(Path::new("harpas.toml"))?;
    eula::require(Path::new(""), &settings)?;
    vanilla_data::generate_registry(Path::new(""), jar)
}

/// Loads `server.properties`, completes and rewrites it as vanilla does, and applies `HARPAS_*` overrides.
fn load_properties(path: &Path) -> io::Result<Properties> {
    let mut properties = Properties::load(path)?;
    properties.complete();
    // Java's `Date.toString`, which vanilla writes as the second comment.
    let timestamp = chrono::Local::now().format("%a %b %d %H:%M:%S %Z %Y");
    if let Err(e) = properties.store(path, &timestamp.to_string()) {
        warn!("Failed to save {}: {e}", path.display());
    }
    for name in properties.apply_env(std::env::vars()) {
        warn!("{name} has an invalid value; it is ignored");
    }
    Ok(properties)
}

/// The server's settings and list entry, from `server.properties` and `server-icon.png`.
fn server_info(properties: &Properties) -> ServerInfo {
    let defaults = ServerInfo::default();
    let online_mode = properties.parsed("online-mode", defaults.online_mode);
    ServerInfo {
        motd: properties.string("motd", &defaults.motd),
        max_players: properties.parsed("max-players", defaults.max_players),
        online_players: 0,
        favicon: load_icon(Path::new("server-icon.png")),
        enforces_secure_chat: online_mode
            && properties.parsed("enforce-secure-profile", defaults.enforces_secure_chat),
        accepts_transfers: properties.parsed("accepts-transfers", defaults.accepts_transfers),
        online_mode,
        allow_flight: properties.parsed("allow-flight", defaults.allow_flight),
        view_distance: properties.parsed("view-distance", defaults.view_distance),
        simulation_distance: properties.parsed("simulation-distance", defaults.simulation_distance),
        game_mode: properties
            .get("gamemode")
            .and_then(parse_game_mode)
            .unwrap_or(defaults.game_mode),
        spawn_position: defaults.spawn_position,
        hashed_seed: defaults.hashed_seed,
        is_flat: defaults.is_flat,
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
        warn!(
            "{} must be a 64x64 PNG image; it is not shown",
            path.display()
        );
        return None;
    }
    Some(format!("data:image/png;base64,{}", STANDARD.encode(&bytes)))
}

/// Runs one connection until it is closed.
struct ConnectionEnvironment {
    info: ServerInfo,
    block_entities: block_entity_registry::Registry,
}

fn serve(
    mut stream: TcpStream,
    environment: &ConnectionEnvironment,
    key: Arc<ServerKey>,
    session: Option<&SessionServer>,
    logic: &harpas_engine::Handle,
    permit: &mut Permit,
) -> io::Result<()> {
    // Reads wake up to send the logic thread's output even when this player is idle.
    stream.set_read_timeout(Some(CONNECTION_POLL_INTERVAL))?;
    stream.set_nodelay(true)?;
    let result = drive(&mut stream, environment, key, session, logic, Some(permit));
    let _ = stream.shutdown(net::Shutdown::Both);
    result
}

/// Drives the I/O-free connection state machine over a stream. Once the player has joined, the end of the
/// connection is logged as vanilla logs it, errors included; errors before that are returned. With a `permit`,
/// login attempts count against the connection limits.
fn drive(
    stream: &mut (impl Read + Write),
    environment: &ConnectionEnvironment,
    key: Arc<ServerKey>,
    session: Option<&SessionServer>,
    logic: &harpas_engine::Handle,
    permit: Option<&mut Permit>,
) -> io::Result<()> {
    let mut departure = Departure::default();
    let result = play(
        stream,
        environment,
        key,
        session,
        logic,
        permit,
        &mut departure,
    );
    let Some(name) = departure.name else {
        return result;
    };
    info!(
        "{name} lost connection: {}",
        lost_connection_reason(departure.kick, result)
    );
    info!("{name} left the game");
    Ok(())
}

/// The reason vanilla gives when a player in Play loses connection.
fn lost_connection_reason(kick: Option<&str>, result: io::Result<()>) -> String {
    match (kick, result) {
        (Some(kick), _) => kick.to_owned(),
        (None, Ok(())) => "Disconnected".to_owned(),
        (None, Err(e)) if e.kind() == io::ErrorKind::TimedOut => "Timed out".to_owned(),
        (None, Err(e)) => e.to_string(),
    }
}

/// Who left the game, and why if the server disconnected them.
#[derive(Default)]
struct Departure {
    name: Option<String>,
    kick: Option<&'static str>,
}

fn play(
    stream: &mut (impl Read + Write),
    environment: &ConnectionEnvironment,
    key: Arc<ServerKey>,
    session: Option<&SessionServer>,
    logic: &harpas_engine::Handle,
    mut permit: Option<&mut Permit>,
    departure: &mut Departure,
) -> io::Result<()> {
    let entity_id = logic
        .next_entity_id()
        .ok_or_else(|| io::Error::other("entity IDs exhausted"))?;
    let mut connection = Connection::with_entity_id(key, entity_id);
    if logic.has_world() {
        connection.use_external_chunks();
    }
    if permit.is_some() {
        connection.require_admission();
    }
    if let Some(session) = session {
        connection.set_chat_authority(session.chat_authority());
    }
    let mut info = environment.info.clone();
    let mut buf = vec![0; 16 * 1024];
    let mut last_read = Instant::now();
    let mut player_session = None;
    let mut uuid_logged = false;
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
        let result = result.and_then(|()| {
            if !connection.awaits_admission() {
                return Ok(());
            }
            if permit.as_ref().is_some_and(|p| !p.login(Instant::now())) {
                connection.refuse(limits::THROTTLED);
                return Ok(());
            }
            connection.admit(&info)
        });
        stream.write_all(&connection.take_output())?;
        if let Some(request) = connection.take_auth_request() {
            let outcome = session.map_or(AuthOutcome::Unavailable, |s| s.has_joined(&request));
            connection.authenticated(outcome, &info);
            stream.write_all(&connection.take_output())?;
        }
        // In offline mode too, as vanilla logs it.
        if !uuid_logged && let Some(player) = connection.player() {
            info!(
                "UUID of player {} is {}",
                player.name,
                format_uuid(player.uuid)
            );
            uuid_logged = true;
        }
        result.map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if player_session.is_none() && connection.state() == State::Play {
            let player = connection
                .player()
                .ok_or_else(|| io::Error::other("Play without a player"))?;
            let max_view_distance = info.view_distance.clamp(2, 32) as u8;
            let session = logic
                .player_joined_with_view(
                    entity_id,
                    player::profile(player, info.game_mode),
                    max_view_distance,
                    connection
                        .client_view_distance()
                        .unwrap_or(max_view_distance),
                    info.allow_flight,
                )
                .map_err(io::Error::other)?;
            info!("{} joined the game", player.name);
            departure.name = Some(player.name.clone());
            if let Some(permit) = permit.as_deref_mut() {
                permit.entered_play();
            }
            player_session = Some(session);
        }
        if let Some(session) = &player_session {
            // The logic thread's output first: once it disconnects the player, there is nothing left to
            // forward, and forwarding to a stopped logic thread would hide its reason.
            departure.kick = departure.kick.or(player::send(
                &mut connection,
                session,
                &environment.block_entities,
            ));
            if !connection.is_closed() {
                player::receive(&mut connection, session)?;
            }
            stream.write_all(&connection.take_output())?;
        }
    }
    stream.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};

    use super::*;
    use harpas_java::{FrameReader, write_frame};
    use harpas_protocol_java::v26_1::{configuration, handshaking, login, play, status};

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

    fn login_packets(name: &str) -> VecDeque<Vec<u8>> {
        let start: login::serverbound::Packet = login::serverbound::LoginStart {
            name: name.into(),
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
        drive(
            &mut stream,
            &test_environment(info),
            Arc::clone(key),
            None,
            logic,
            None,
        )
        .unwrap();
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

    fn test_environment(info: &ServerInfo) -> ConnectionEnvironment {
        ConnectionEnvironment {
            info: info.clone(),
            block_entities: block_entity_registry::Registry::default(),
        }
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
                    input: login_packets("Steve"),
                    close: Some(receiver),
                    ..Stream::default()
                };
                drive(
                    &mut stream,
                    &test_environment(&client_info),
                    client_key,
                    None,
                    &client_logic,
                    None,
                )
            });
            wait_for_count(&logic, 1);
            assert_eq!(status_count(&info, &key, &logic), 1);
            close.send(error).unwrap();
            // The error is logged as the reason the player lost connection.
            connection.join().unwrap().unwrap();
            wait_for_count(&logic, 0);
            assert_eq!(status_count(&info, &key, &logic), 0);
        }
        drop(logic);
        logic_thread.join().unwrap().unwrap();
    }

    #[test]
    fn the_command_line_asks_for_the_server_or_its_version() {
        let args =
            |list: &[&str]| parse_args(&list.iter().map(|a| a.to_string()).collect::<Vec<_>>());
        assert_eq!(args(&[]), Ok(Command::Run));
        assert_eq!(
            args(&["--version"]),
            Ok(Command::Version { verbose: false })
        );
        assert_eq!(
            args(&["-V", "--verbose"]),
            Ok(Command::Version { verbose: true })
        );
        assert_eq!(
            args(&["--verbose", "--version"]),
            Ok(Command::Version { verbose: true })
        );
        assert!(args(&["--verbose"]).is_err());
        assert_eq!(args(&["nogui"]), Ok(Command::Run));
        assert!(args(&["--port", "25566"]).is_err());
        assert_eq!(
            args(&["vanilla-data", "install", "--from", "server.jar"]),
            Ok(Command::InstallVanillaData {
                from: PathBuf::from("server.jar")
            })
        );
        assert!(args(&["vanilla-data", "install"]).is_err());
        assert_eq!(
            args(&["vanilla-data", "generate-registry"]),
            Ok(Command::GenerateRegistry { from: None })
        );
        assert_eq!(
            args(&["vanilla-data", "generate-registry", "--from", "server.jar"]),
            Ok(Command::GenerateRegistry {
                from: Some(PathBuf::from("server.jar"))
            })
        );
        assert!(args(&["vanilla-data", "generate-registry", "--from"]).is_err());
        assert_eq!(version(), format!("{}+26.1", env!("CARGO_PKG_VERSION")));
        assert!(verbose_version().contains("Java clients: 26.1 (protocol 775)"));
    }

    #[test]
    fn worlds_of_other_versions_stop_the_server() {
        let world = std::env::temp_dir().join(format!("harpas-check-world-{}", std::process::id()));
        // No world yet.
        assert!(check_world(&world).is_ok());

        let mut data = harpas_core::nbt::Compound::new();
        data.insert("DataVersion", harpas_anvil::DATA_VERSION);
        harpas_anvil::write_level(&world, &data).unwrap();
        assert!(check_world(&world).is_ok());

        data.insert("DataVersion", 4440);
        harpas_anvil::write_level(&world, &data).unwrap();
        let error = check_world(&world).unwrap_err().to_string();
        let _ = fs::remove_dir_all(&world);
        assert!(error.starts_with(&format!(
            "Cannot open {}: this world was saved by",
            world.display()
        )));
        assert!(error.contains("--forceUpgrade"));
    }

    #[test]
    fn allow_flight_is_loaded_from_server_properties() {
        assert!(!server_info(&Properties::default()).allow_flight);
        assert!(server_info(&Properties::parse("allow-flight=true")).allow_flight);
    }

    #[test]
    fn secure_chat_status_requires_online_mode() {
        assert!(
            !server_info(&Properties::parse(
                "online-mode=false\nenforce-secure-profile=true"
            ))
            .enforces_secure_chat
        );
        assert!(
            server_info(&Properties::parse(
                "online-mode=true\nenforce-secure-profile=true"
            ))
            .enforces_secure_chat
        );
        assert!(
            !server_info(&Properties::parse(
                "online-mode=true\nenforce-secure-profile=false"
            ))
            .enforces_secure_chat
        );
    }

    #[test]
    fn an_empty_server_ip_accepts_ipv4_and_ipv6() {
        let listeners = listen("", 0).unwrap();
        for listener in &listeners {
            listener.set_nonblocking(true).unwrap();
        }
        let port = listeners[0].local_addr().unwrap().port();
        assert!(
            listeners
                .iter()
                .all(|l| l.local_addr().unwrap().port() == port)
        );
        let mut addresses = vec!["127.0.0.1"];
        // Machines without IPv6 have only the IPv4 listener.
        if listeners[0].local_addr().unwrap().is_ipv6() {
            addresses.push("::1");
        }
        for address in addresses {
            let _client = TcpStream::connect((address, port)).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while accept(&listeners).is_none() {
                assert!(Instant::now() < deadline, "{address} was not accepted");
                thread::sleep(Duration::from_millis(5));
            }
        }
        // A given address has one listener.
        assert_eq!(listen("127.0.0.1", 0).unwrap().len(), 1);
        assert_eq!(listen("::1", 0).map_or(1, |l| l.len()), 1);
    }

    #[test]
    fn lost_connection_reasons_follow_vanilla() {
        assert_eq!(
            lost_connection_reason(Some("Kicked"), Err(io::ErrorKind::BrokenPipe.into())),
            "Kicked"
        );
        assert_eq!(lost_connection_reason(None, Ok(())), "Disconnected");
        assert_eq!(
            lost_connection_reason(
                None,
                Err(io::Error::new(io::ErrorKind::TimedOut, "read timed out"))
            ),
            "Timed out"
        );
        assert_eq!(
            lost_connection_reason(None, Err(io::Error::other("connection reset"))),
            "connection reset"
        );
    }

    #[test]
    fn a_hovering_survival_player_is_kicked_with_the_reason() {
        let info = ServerInfo {
            online_mode: false,
            compression_threshold: -1,
            view_distance: 2,
            ..ServerInfo::default()
        };
        let key = Arc::new(ServerKey::generate());
        let (logic, logic_thread) = harpas_engine::start().unwrap();
        let (input, server_input) = mpsc::channel();
        let (server_output, _output) = mpsc::channel();
        let server = {
            let (info, key, logic) = (info.clone(), Arc::clone(&key), logic.clone());
            thread::spawn(move || {
                let mut stream = ChannelStream {
                    input: server_input,
                    output: server_output,
                };
                let mut departure = Departure::default();
                let result = play(
                    &mut stream,
                    &test_environment(&info),
                    key,
                    None,
                    &logic,
                    None,
                    &mut departure,
                );
                (result, departure)
            })
        };
        for packet in login_packets("Steve") {
            input.send(packet).unwrap();
        }
        let packets: [play::serverbound::Packet; 2] = [
            play::serverbound::ConfirmTeleportation { teleport_id: 1 }.into(),
            // In the air at spawn height, without falling.
            play::serverbound::SetPlayerPosition {
                x: 0.5,
                y: 65.0,
                z: 0.5,
                flags: play::MovementFlags::default(),
            }
            .into(),
        ];
        for packet in packets {
            input.send(frame(|buf| packet.encode(buf))).unwrap();
        }
        // Vanilla allows 80 ticks of floating, four seconds.
        let (result, departure) = server.join().unwrap();
        result.unwrap();
        assert_eq!(departure.name.as_deref(), Some("Steve"));
        assert_eq!(departure.kick, Some("Flying is not enabled on this server"));
        drop(input);
        drop(logic);
        logic_thread.join().unwrap().unwrap();
    }

    #[test]
    fn a_shutdown_disconnects_players_with_server_closed() {
        let info = ServerInfo {
            online_mode: false,
            compression_threshold: -1,
            view_distance: 2,
            ..ServerInfo::default()
        };
        let key = Arc::new(ServerKey::generate());
        let (logic, logic_thread) = harpas_engine::start().unwrap();
        let (input, server_input) = mpsc::channel();
        let (server_output, _output) = mpsc::channel();
        let server = {
            let (info, key, logic) = (info.clone(), Arc::clone(&key), logic.clone());
            thread::spawn(move || {
                let mut stream = ChannelStream {
                    input: server_input,
                    output: server_output,
                };
                let mut departure = Departure::default();
                let result = play(
                    &mut stream,
                    &test_environment(&info),
                    key,
                    None,
                    &logic,
                    None,
                    &mut departure,
                );
                (result, departure)
            })
        };
        for packet in login_packets("Steve") {
            input.send(packet).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while logic.online_players() != 1 {
            assert!(Instant::now() < deadline, "Steve did not join");
            thread::sleep(Duration::from_millis(5));
        }
        logic.shutdown("Server closed").unwrap();
        logic_thread.join().unwrap().unwrap();
        // A real client keeps sending; its packets must not hide the reason the logic thread gave. The
        // connection may already have closed, as it often has on Windows, and then there is nobody to send to.
        let packet: play::serverbound::Packet =
            play::serverbound::ConfirmTeleportation { teleport_id: 1 }.into();
        let _ = input.send(frame(|buf| packet.encode(buf)));
        let (result, departure) = server.join().unwrap();
        result.unwrap();
        assert_eq!(departure.kick, Some("Server closed"));
    }

    struct ChannelStream {
        input: Receiver<Vec<u8>>,
        output: Sender<Vec<u8>>,
    }

    impl Read for ChannelStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.input.recv_timeout(Duration::from_millis(10)) {
                Ok(bytes) => {
                    assert!(bytes.len() <= buf.len());
                    buf[..bytes.len()].copy_from_slice(&bytes);
                    Ok(bytes.len())
                }
                Err(RecvTimeoutError::Timeout) => Err(io::ErrorKind::WouldBlock.into()),
                Err(RecvTimeoutError::Disconnected) => Ok(0),
            }
        }
    }

    impl Write for ChannelStream {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.output
                .send(bytes.to_vec())
                .map_err(|_| io::ErrorKind::BrokenPipe)?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct TestClient {
        input: Sender<Vec<u8>>,
        output: Receiver<Vec<u8>>,
        frames: FrameReader,
        thread: thread::JoinHandle<io::Result<()>>,
    }

    impl TestClient {
        fn start(
            name: &str,
            info: &ServerInfo,
            key: &Arc<ServerKey>,
            logic: &harpas_engine::Handle,
        ) -> Self {
            let (input, server_input) = mpsc::channel();
            let (server_output, output) = mpsc::channel();
            let (info, key, logic) = (info.clone(), Arc::clone(key), logic.clone());
            let thread = thread::spawn(move || {
                let mut stream = ChannelStream {
                    input: server_input,
                    output: server_output,
                };
                drive(
                    &mut stream,
                    &test_environment(&info),
                    key,
                    None,
                    &logic,
                    None,
                )
            });
            for packet in login_packets(name) {
                input.send(packet).unwrap();
            }
            Self {
                input,
                output,
                frames: FrameReader::default(),
                thread,
            }
        }

        fn next_frame(&mut self) -> Vec<u8> {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(frame) = self.frames.next_frame().unwrap() {
                    return frame;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                let bytes = self
                    .output
                    .recv_timeout(remaining)
                    .expect("server output timed out");
                self.frames.push(&bytes);
            }
        }

        fn join(&mut self) -> i32 {
            self.join_with_chunks().0
        }

        fn join_with_chunks(&mut self) -> (i32, Vec<play::clientbound::ChunkDataAndUpdateLight>) {
            assert!(matches!(
                login::clientbound::Packet::decode(&self.next_frame()),
                Ok(login::clientbound::Packet::LoginSuccess(_))
            ));
            for _ in 0..33 {
                configuration::clientbound::Packet::decode(&self.next_frame()).unwrap();
            }
            let login = play::clientbound::Packet::decode(&self.next_frame()).unwrap();
            let play::clientbound::Packet::Login(login) = login else {
                panic!("expected Play Login");
            };
            let mut chunks = Vec::new();
            loop {
                match play::clientbound::Packet::decode(&self.next_frame()).unwrap() {
                    play::clientbound::Packet::ChunkDataAndUpdateLight(chunk) => chunks.push(chunk),
                    play::clientbound::Packet::ChunkBatchFinished(_) => {
                        self.acknowledge_chunks();
                        return (login.entity_id, chunks);
                    }
                    _ => {}
                }
            }
        }

        fn next_play(&mut self) -> play::clientbound::Packet {
            let packet = play::clientbound::Packet::decode(&self.next_frame()).unwrap();
            if matches!(packet, play::clientbound::Packet::ChunkBatchFinished(_)) {
                self.acknowledge_chunks();
            }
            packet
        }

        fn acknowledge_chunks(&self) {
            self.send(
                play::serverbound::ChunkBatchReceived {
                    chunks_per_tick: 8.0,
                }
                .into(),
            );
        }

        fn send(&self, packet: play::serverbound::Packet) {
            self.input.send(frame(|buf| packet.encode(buf))).unwrap();
        }

        fn close(self) {
            drop(self.input);
            self.thread.join().unwrap().unwrap();
        }
    }

    #[test]
    fn players_receive_existing_chunks_and_new_columns_after_moving() {
        use harpas_anvil::{Chunk, ChunkSection, DATA_VERSION, Region, dimension_dir, region_of};
        use harpas_core::block::block_states;
        use harpas_core::chunk::{PalettedContainer, Section, Strategy};
        use harpas_core::nbt::Compound;
        use harpas_protocol_java::codec;
        use std::collections::HashSet;

        struct Folder(PathBuf);
        impl Drop for Folder {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let folder =
            Folder(std::env::temp_dir().join(format!("harpas-view-test-{}", std::process::id())));
        fs::create_dir_all(&folder.0).unwrap();
        let dir = dimension_dir(&folder.0, "minecraft:overworld").join("region");
        fs::create_dir_all(&dir).unwrap();
        let states = block_states();
        let stone = states.default_state("minecraft:stone").unwrap();
        let diamond = states.default_state("minecraft:diamond_block").unwrap();
        for (x, state) in [(0, stone), (-3, diamond)] {
            let mut rest = Compound::new();
            rest.insert("DataVersion", DATA_VERSION);
            rest.insert("xPos", x);
            rest.insert("zPos", 0);
            let chunk = Chunk {
                sections: vec![ChunkSection {
                    y: -4,
                    section: Some(Section::new(
                        PalettedContainer::filled(
                            Strategy::block_states(states.global_bits()),
                            state,
                        ),
                        PalettedContainer::filled(Strategy::biomes(7), 0),
                    )),
                    rest: Compound::new(),
                }],
                rest,
            };
            let (rx, rz) = region_of(x, 0);
            let mut region = Region::open(&dir, rx, rz).unwrap();
            region.set_chunk(x, 0, &chunk.to_nbt(harpas_java::biome_names()));
            region.save().unwrap();
        }
        let info = ServerInfo {
            online_mode: false,
            compression_threshold: -1,
            view_distance: 2,
            ..ServerInfo::default()
        };
        let key = Arc::new(ServerKey::generate());
        let (logic, logic_thread) =
            harpas_engine::start_with_world(&folder.0, harpas_java::biome_names()).unwrap();
        let mut client = TestClient::start("Steve", &info, &key, &logic);
        let (_, first) = client.join_with_chunks();
        let mut received = HashSet::new();
        let mut origin = None;
        for chunk in first {
            received.insert((chunk.x, chunk.z));
            if (chunk.x, chunk.z) == (0, 0) {
                origin = Some(chunk);
            }
        }
        while received.len() < 25 {
            if let play::clientbound::Packet::ChunkDataAndUpdateLight(chunk) = client.next_play() {
                assert!(received.insert((chunk.x, chunk.z)), "chunk was sent twice");
                if (chunk.x, chunk.z) == (0, 0) {
                    origin = Some(chunk);
                }
            }
        }
        fn first_state(chunk: &play::clientbound::ChunkDataAndUpdateLight) -> i32 {
            let mut data = chunk.data.as_slice();
            assert_eq!(codec::read_i16(&mut data).unwrap(), 4096);
            assert_eq!(codec::read_i16(&mut data).unwrap(), 0);
            assert_eq!(codec::read_u8(&mut data).unwrap(), 0);
            codec::read_varint(&mut data).unwrap()
        }
        assert_eq!(first_state(&origin.unwrap()), stone as i32);
        client.send(play::serverbound::ConfirmTeleportation { teleport_id: 1 }.into());
        client.send(
            play::serverbound::SetPlayerPosition {
                x: -0.5,
                y: 65.0,
                z: 0.5,
                flags: play::MovementFlags::default(),
            }
            .into(),
        );
        let mut center = false;
        let mut unloaded = false;
        let mut entering = None;
        while !center || !unloaded || entering.is_none() {
            match client.next_play() {
                play::clientbound::Packet::SetCenterChunk(packet)
                    if (packet.x, packet.z) == (-1, 0) =>
                {
                    center = true
                }
                play::clientbound::Packet::UnloadChunk(packet)
                    if (packet.x, packet.z) == (2, 0) =>
                {
                    unloaded = true
                }
                play::clientbound::Packet::ChunkDataAndUpdateLight(packet)
                    if (packet.x, packet.z) == (-3, 0) =>
                {
                    entering = Some(packet)
                }
                _ => {}
            }
        }
        assert_eq!(first_state(&entering.unwrap()), diamond as i32);
        client.close();
        drop(logic);
        logic_thread.join().unwrap().unwrap();
    }

    #[test]
    fn two_clients_receive_player_lists_entities_movement_and_disconnects() {
        let info = ServerInfo {
            online_mode: false,
            compression_threshold: -1,
            view_distance: 2,
            ..ServerInfo::default()
        };
        let key = Arc::new(ServerKey::generate());
        let (logic, logic_thread) = harpas_engine::start().unwrap();
        let mut steve = TestClient::start("Steve", &info, &key, &logic);
        let steve_id = steve.join();
        let mut alex = TestClient::start("Alex", &info, &key, &logic);
        let alex_id = alex.join();
        assert_ne!(steve_id, alex_id);

        let steve_list = steve.next_play();
        assert!(
            matches!(steve_list, play::clientbound::Packet::PlayerInfoUpdate(p) if p.updates.entries[0].name == "Alex")
        );
        assert!(
            matches!(steve.next_play(), play::clientbound::Packet::SpawnEntity(p) if p.entity_id == alex_id && p.entity_type == 155)
        );
        let alex_list = alex.next_play();
        assert!(
            matches!(alex_list, play::clientbound::Packet::PlayerInfoUpdate(p) if p.updates.entries[0].name == "Steve")
        );
        assert!(
            matches!(alex.next_play(), play::clientbound::Packet::SpawnEntity(p) if p.entity_id == steve_id)
        );

        // Both clients see the original unsigned text, including their own messages.
        let now = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        for (index, text) in ["Hello, Alex!", "Hola, Steve!"].into_iter().enumerate() {
            let packet: play::serverbound::Packet = play::serverbound::ChatMessage {
                message: text.into(),
                timestamp: now,
                salt: 42,
                signature: None,
                offset: 0,
                acknowledged: [0; 3],
                checksum: 1,
            }
            .into();
            if index == 0 {
                steve.send(packet);
            } else {
                alex.send(packet);
            }
            for recipient in [&mut steve, &mut alex] {
                assert!(
                    matches!(recipient.next_play(), play::clientbound::Packet::PlayerChatMessage(p)
                    if p.message == text && p.signature.is_none())
                );
            }
        }

        alex.send(play::serverbound::ConfirmTeleportation { teleport_id: 1 }.into());
        alex.send(
            play::serverbound::SetPlayerPositionAndRotation {
                x: 3.5,
                y: 65.0,
                z: 0.5,
                yaw: 90.0,
                pitch: 15.0,
                flags: play::MovementFlags::default(),
            }
            .into(),
        );
        assert!(
            matches!(steve.next_play(), play::clientbound::Packet::SetHeadRotation(p) if p.entity_id == alex_id)
        );
        assert!(
            matches!(steve.next_play(), play::clientbound::Packet::EntityPositionSync(p) if p.entity_id == alex_id && p.x == 3.5 && p.yaw == 90.0)
        );

        alex.close();
        assert!(
            matches!(steve.next_play(), play::clientbound::Packet::RemoveEntities(p) if p.entity_ids == [alex_id])
        );
        assert!(
            matches!(steve.next_play(), play::clientbound::Packet::PlayerInfoRemove(p) if p.players == [harpas_java::offline_uuid("Alex")])
        );
        steve.close();
        drop(logic);
        logic_thread.join().unwrap().unwrap();
    }
}
