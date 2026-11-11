# Changelog

All notable changes to Harpas are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Harpas adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) as described in [docs/releases.md](docs/releases.md).

## [Unreleased]

### Added

### Changed

### Fixed

### Removed

### Security

## [0.1.2+26.1] - 2026-11-11

Fixes to the network side of the server, from the same audit as 0.1.1.

### Fixed

- When Minecraft Services' chat keys cannot be fetched at startup, the server asks again every minute, and
  until they load it ignores the players' chat sessions, as vanilla does without the keys. Every online-mode
  player used to be disconnected on joining until the server was restarted (`harpas-java`, `harpas-server`).
- A player who sends chat messages too fast is disconnected with "Kicked for spamming", by vanilla's counter:
  each message adds 20, each tick takes one away, and 200 disconnects. Chat had no rate limit (`harpas-engine`).

### Security

- A player gets its entity ID when it enters Play. Every connection used to take one when it opened, server list
  pings included, and the IDs are not reused, so enough connections left none for new players until a restart
  (`harpas-java`, `harpas-server`).
- The connection limits count an IPv6 address as its /64 prefix, so one host can no longer avoid them by
  changing the rest of its address (`harpas-server`).
- A player whose connection stops taking the server's messages, or takes them too slowly, is disconnected
  once 60,000 are waiting, instead of the messages using more and more memory (`harpas-engine`).

## [0.1.1+26.1] - 2026-11-11

A security release: three ways for a client to exhaust or stall a server over the network
([GHSA-g382-rmhw-p4jg](https://github.com/KxRazor/harpas/security/advisories/GHSA-g382-rmhw-p4jg)).

### Fixed

- A server whose ticks take longer than 50 milliseconds keeps applying what its players send, and stops when asked
  to. It read connection events only while waiting for the next tick, so it never read them once it fell behind
  (`harpas-engine`).

### Security

- Network NBT is limited to 2 MiB of memory once read, as vanilla's quota for packets. A small compressed packet
  holding a list of empty compounds could make the server allocate hundreds of megabytes (`harpas-core`).
- A connection whose client stops reading is closed after 30 seconds. It used to wait forever, keeping its thread
  and the player online while the packets for it accumulated in memory (`harpas-server`).

## [0.1.0+26.1] - 2026-11-02

A Java Edition 26.1 client joins a void world in online or offline mode, moves around, sees other players and
chats (milestone M2).

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
- Java 26.1 Play codecs for 127 more packets: blocks, entities, the world border, titles, boss bars,
  scoreboards and teams, sounds, maps, waypoints, resource packs and the client's actions. The 21 packets that
  need item stacks, entity metadata, particles, the command tree or debug subscriptions remain, for M4. Union
  default variants may carry a value, and the minecraft-data check handles nested and optional switches
  (`harpas-protocol-java`, `harpas-protocol-codegen`).

- `harpas --version` prints the version with the supported Minecraft version as build metadata, and
  `--version --verbose` what the release declares; vanilla's `nogui` argument is accepted (`harpas-server`).
- Release binaries for Linux and Windows, with SHA-256 checksums, attached to each GitHub release (`ci`).

### Changed

- Login Start's player name field is `name`, as the protocol documentation calls it, instead of `username`;
  CodeQL read the old name as account credentials and flagged the vanilla log lines that show player names
  (`harpas-protocol-java`).

### Security

- Generate fresh operating-system random session keys for the shared-encoding benchmark instead of fixed
  byte patterns (`harpas-bench`).
