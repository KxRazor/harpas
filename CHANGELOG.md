# Changelog

All notable changes to Harpas are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Harpas adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) as described in [docs/releases.md](docs/releases.md).

## [Unreleased]

### Added

- Embedded JDK 25 bridge benchmark with JNI/FFM calls, shared player views, synchronous events for 200
  simulated players, and behavioral checks for both dispatch paths (`harpas-bench`).
- Cold-start ticks and JIT compiler CPU time in the JVM bridge benchmark, to compare Temurin with GraalVM
  Community Edition (`harpas-bench`).
- Java 26.1 Handshaking, Status and Login packet codecs, generated from Harpas' own protocol schema
  (`harpas-protocol-java`), with a check of the schema against minecraft-data (`harpas-protocol-codegen`).
- Java 26.1 Configuration packet codecs, with network NBT, tagged unions and enum values checked against
  minecraft-data (`harpas-protocol-java`, `harpas-protocol-codegen`).
- NBT values with Java Edition's network encoding (`harpas-core`).
- Java Edition server list: the `harpas` binary listens on `server-ip` and `server-port` and answers the status
  ping with the MOTD, player counts and `server-icon.png` from `server.properties`, as well as the pre-1.7
  legacy ping. Other protocol versions and disabled transfers are refused at login with vanilla's messages
  (`harpas-java`, `harpas-server`).
- Offline-mode players join a void world: Login with vanilla's offline UUIDs, Configuration with the 26.1
  registries by name from the client's `minecraft:core` pack and the tags they need (empty for now), and Play
  with the player's abilities, player list entry, spawn, position, empty chunks and Keep Alive. `gamemode`,
  `view-distance`, `simulation-distance` and `online-mode` are read from `server.properties` (`harpas-java`,
  `harpas-server`).
- Online mode: Mojang authentication through the session server, AES/CFB8 encryption with a 1024-bit RSA key
  generated at startup, and zlib compression following `network-compression-threshold`, with vanilla's limits
  on compressed packets (`harpas-java`, `harpas-server`).
- Java 26.1 Play packet codecs for joining, moving, seeing other players and chatting, with fixed byte arrays,
  bit flags, bit fields, registry holders, low-precision vectors and hand-written types in the schema
  (`harpas-protocol-java`, `harpas-protocol-codegen`).
- A single logic thread with a 20 TPS tick skeleton and connection events, with player counts in the server
  list updated when players enter and leave Play (`harpas-engine`, `harpas-server`).
- Player entities in the void world: a shared player list, profile and skin properties, visibility within each
  viewer's tracking range, synchronized movement, movement and `allow-flight` checks, and chunk updates as
  players travel (`harpas-engine`, `harpas-java`, `harpas-server`).
- Java 26.1 player chat: unsigned messages in offline mode and verified signing sessions, message signatures
  and acknowledgements in online mode. Session and message fan-out includes the sender and late joiners;
  real-client validation of signed chat remains pending (`harpas-engine`, `harpas-java`, `harpas-server`).
- `server.properties` with every Java 26.1 key: completed with vanilla's defaults and written back on startup
  as vanilla does, with `HARPAS_*` environment variables overriding single keys. `harpas.toml` is checked, and
  settings it cannot use are reported (`harpas-server`).
- Server log in vanilla's format on the console and in `logs/latest.log`, archived into dated `.log.gz` files
  at startup and when the day changes, with vanilla's UUID, join and lost connection messages. An interactive
  console with history and completion runs `help`, `list` and `stop`; Ctrl-C stops the server
  (`harpas-engine`, `harpas-server`).
- Graceful shutdown on `stop`, SIGINT, SIGTERM and SIGHUP: new connections are closed, players are disconnected
  with "Server closed", the logic thread stops, connections get five seconds to close and the terminal is
  restored before exiting (`harpas-engine`, `harpas-server`).
- Protection against abusive Java connections: limits on connections not yet in Play (256 in total, 8 per
  address), on new connections (20 per address per 10 seconds) and on login attempts (5 per address per
  10 seconds, refused with "Connection throttled!"), a maximum packet size per state, and 30 seconds to log in
  with vanilla's `slow_login` message. Loopback connections skip the per-address limits (`harpas-java`,
  `harpas-server`).
- Logins are refused with vanilla's "The server is full!" when `max-players` players are in Play
  (`harpas-java`).

### Changed

### Fixed

### Removed

### Security

- Generate fresh operating-system random session keys for the shared-encoding benchmark instead of fixed
  byte patterns (`harpas-bench`).
