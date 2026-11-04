# Changelog

All notable changes to Harpas are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Harpas adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) as described in [docs/releases.md](docs/releases.md).

## [Unreleased]

### Added

- Mojang EULA acceptance: the server starts once `eula = true` is in `harpas.toml`, `HARPAS_EULA=true` is
  set or a vanilla `eula.txt` accepts it; an interactive console asks once and saves the answer
  (`harpas-server`).
- Vanilla game data: on the first start the Minecraft 26.1 server jar is downloaded from Mojang, verified
  and only its `data/` files are extracted into one zip, `.harpas/vanilla-data/26.1.zip`; without internet
  access, `harpas vanilla-data install --from <server.jar>` does the same with a jar the owner provides
  (`harpas-server`).
- Chunk sections with block states and biomes in paletted containers stored as vanilla stores them in memory,
  vanilla's compacted Anvil form, and a version that changes with every change (`harpas-core`).
- Region files: chunks read from and written to `r.x.z.mca` files, with external `c.x.z.mcc` files for large
  chunks, zlib writing, gzip and uncompressed reading, and atomic saves (`harpas-anvil`). NBT with a root name,
  as files store it (`harpas-core`).
- Chunk NBT read into sections and written back, keeping every other field, with vanilla's fallbacks for
  damaged sections (`harpas-anvil`), and the block state registry of Java 26.1 from minecraft-data
  (`harpas-core`).
- `level.dat` and player data read and written as gzip NBT with vanilla's `_old` backups, and the 26.1 world
  folder layout with dimensions under `dimensions/` and players under `players/data/` (`harpas-anvil`).
- World versions: worlds of 26.1 and its hotfixes 26.1.1 and 26.1.2 open, and the server does not start on a
  `level-name` world saved by any other version, explaining how to upgrade an older one with vanilla's
  `--forceUpgrade`; chunks of another version are refused as they are read (`harpas-anvil`, `harpas-server`).
- Existing overworld chunks occupied by players are read on a bounded worker pool and delivered to the logic
  thread. Missing and damaged chunks are distinguished, and an incompatible chunk stops the server with an
  error (`harpas-engine`, `harpas-server`).
- Existing overworld chunks within each player's view are loaded and sent to Java clients in bounded batches,
  including block states, biomes, heightmaps, stored light and verified block entity updates. Their type IDs
  are generated locally from the verified server jar when Java 25 is available; without them the server warns
  and omits block entities. Missing chunks remain void when generation is unavailable (`harpas-anvil`, `harpas-engine`,
  `harpas-java`, `harpas-server`).
- Changed overworld chunks are saved from immutable snapshots on a worker during autosave, unloading and
  shutdown. Edits made during a save stay pending, and shutdown reports a save failure (`harpas-engine`,
  `harpas-server`).
- Vanilla-exact deterministic random generators (`LegacyRandom`, `XoroshiroRandom`, Stafford `Mix13` and MD5
  positional seeding, `RandomSequence` and `WorldgenRandom`) and Java math helpers (`floorDiv`/`floorMod`,
  masked shifts, `Math.round`, deterministic `ln`, interpolation and the 65,536-entry `f32` sine table),
  checked bit for bit against Java 25 (`harpas-core`).
- Vanilla-exact gradient and simplex noise samplers (`ImprovedNoise`, `PerlinNoise`, `NormalNoise`,
  `BlendedNoise`, `SimplexNoise` and `PerlinSimplexNoise`), preserving modern positional and legacy sequential
  octave seeding, `2^25` coordinate wrapping and `(double) 1.0e-7f` vertical smearing, checked bit for bit
  against Java 25 (`harpas-core`).
- Worldgen datapack loader (`WorldgenData` from `.harpas/vanilla-data/26.1.zip` or loose JSON files), all 30
  density functions of 26.1 compiled into a deduplicated DAG with `RandomState` seeding and `f32` `CubicSpline`
  evaluation, `NoiseChunk` `4×8×4` trilinear cell interpolation and caching, 7D `Climate` R-tree with
  `OverworldBiomeBuilder`, `MultiNoiseBiomeSource` section biome filling and `BiomeManager` 3D Voronoi zoom,
  checked bit for bit against Java 25 (`harpas-worldgen`).
- Base terrain generation (`NoiseBasedChunkGenerator::fill_from_noise` and `Heightmaps`), localized 3D aquifers
  (`NoiseBasedAquifer` with 4-nearest cell lookup, deep dark suppression, barrier pressure and lava/water
  flooding), copper-granite and iron-tuff ore veins (`OreVeinifier`), and `SurfaceSystem` with all 4 surface
  rule types, all 11 condition types, eroded badlands pillars, 192-layer terracotta bands and frozen ocean
  icebergs. Sampled terrain and surface chunks, fluid updates, aquifers, ore veins and surface calculations
  are checked against Minecraft 26.1; parity for complete generated worlds remains to be verified
  (`harpas-worldgen`).
- Normal Overworld generation now applies cave and canyon carvers, heightmaps and fluid postprocessing on
  chunk workers. New worlds preserve `level-seed`, while existing worlds retain their saved seed; generated
  chunks are saved at `minecraft:carvers` so vanilla can continue later stages. Features, structures and
  lighting remain separate roadmap tasks (`harpas-worldgen`, `harpas-engine`, `harpas-anvil`, `harpas-server`).
- A parity harness that generates whole Overworld chunks with Mojang's server and compares every block,
  biome, carving mask, fluid update and heightmap, in 1,621 chunks of 5 seeds covering all 54 biomes, along with
  the climate parameter points (`harpas-worldgen`).
- Where structures may start: structure sets, structures and biome tags are loaded, with vanilla's random spread
  and concentric ring placements, frequency reduction and exclusion zones, checked against Minecraft 26.1. The
  generator does not place structures yet (`harpas-worldgen`).
- Structure starts and references for buried treasure, desert pyramids, jungle temples and swamp huts, with the
  terrain heights and biome checks that every structure uses, checked against Minecraft 26.1. The generator does
  not use them yet (`harpas-worldgen`).
- Stronghold starts with vanilla's piece layout, checked against Minecraft 26.1 at every ring position. The
  generator does not use them yet (`harpas-worldgen`).
- Structure templates and template pools are read from the vanilla data: template sizes, bounding boxes,
  palettes and jigsaw blocks in every rotation, and pool elements with their weights, checked against Minecraft
  26.1 (`harpas-worldgen`).
- Jigsaw structure starts for villages, pillager outposts, ancient cities, trail ruins and trial chambers, with
  their pieces, elements and junctions, checked against Minecraft 26.1. The generator does not use them yet
  (`harpas-worldgen`).
- Igloo, shipwreck and ocean ruin starts, whose pieces are whole templates. The generator does not use them yet
  (`harpas-worldgen`).

### Changed

### Fixed

- Saving a region no longer silently drops unreadable neighboring entries or overwrites a truncated header
  (`harpas-anvil`).
- A crash while saving a region no longer leaves a large chunk's `.mcc` file newer than the region that points
  to it: a region and its `.mcc` files are saved as one transaction through a journal, which the server
  completes or discards on start (`harpas-anvil`, `harpas-engine`).
- World generation differences from vanilla that the new parity harness found: pale gardens generated as dark
  forests in part of their climate range; ties between biomes resolved differently because the climate search
  tree ordered its branches differently; fluids placed by surface rules, such as swamp water, were not marked
  for postprocessing; and frozen ocean icebergs covered the wrong height range and melted at the wrong
  temperature, missing packed ice in caves under them (`harpas-worldgen`).
- Moving pistons in loaded chunks now send their moving block and animation data to Java clients
  (`harpas-server`).
- Truncated region headers and damaged world-generation backups are no longer treated as missing data, and
  chunks requiring old-terrain blending are not generated with an incompatible algorithm (`harpas-anvil`,
  `harpas-engine`).
- The sine table uses Minecraft 26.1's double-precision angle scaling, including near lookup boundaries
  (`harpas-core`).

### Removed

### Security

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
