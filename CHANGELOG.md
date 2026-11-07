# Changelog

All notable changes to Harpas are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Harpas adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) as described in [docs/releases.md](docs/releases.md).

## [Unreleased]

### Added

- FEATURES stage on the chunk workers, behind `generation.features = true` in `harpas.toml`: workers share the
  proto-chunks of the area being generated, decorate each chunk once its eight neighbors have completed
  carvers, and hand a chunk to the logic thread only once its eight neighbors have decorated, so no decoration
  writes into a chunk players have. Proto-chunks are saved at `carvers` or `features` when memory runs short
  and on shutdown, and saved proto-chunks, from Harpas or vanilla, are resumed. `feature_worker_parity`
  compares final chunks with vanilla decorating their 3x3 chunks in the same order and reloads them unchanged.
  Off by default while only ores are placed, because a chunk saved after FEATURES never runs it again
  (`harpas-worldgen`, `harpas-engine`, `harpas-server`).

- FEATURES-stage schedule: each chunk decorates itself once, from its own decoration seed, with the features of
  every biome stored in the 3x3 chunks around it, run in the world's global order for each step and seeded by
  their index in that order. Every position a placed feature produces is tried, `rarity_filter` is applied, and
  vein ores cull spheres inside other spheres, bound the visited set as vanilla does and check air on all six
  sides. `RegionFeatureWorld` routes reads and writes across the nine chunks and can zoom their stored biomes.
  `feature_stage_parity` compares the order of all 11 steps and the ore blocks of 18 decorated chunks with
  vanilla's own `applyBiomeDecoration`. The structures of each step, worker integration and the other families
  follow (`harpas-worldgen`).

- Vein ore features and the FEATURES-stage scheduler: vanilla's ore vein geometry, rule tests, discard chance,
  height providers and decoration seeds, with the 26.1 ores compiled from the datapack. `ore_feature_parity`
  compares the feature with vanilla's own `OreFeature` on the same Xoroshiro decoration source, including the
  random state after the call. Scattered ore (ancient debris) follows with the Nether. The complete stage and
  worker integration follow (`harpas-worldgen`).

- Feature data: the datapack loader reads `configured_feature` and `placed_feature` definitions and each
  biome's `features` lists, with `minecraft:ore` configurations, rule tests, target states and the `count`,
  `in_square`, `height_range` and `biome` placement modifiers. Other types keep their JSON for later feature
  families, and the 26.1 pack's feature references are checked as part of `datapack_parity`
  (`harpas-worldgen`).

- Light engine: sky and block light for generated chunks, with per-state emission and dampening from the block
  registry, propagation across a one-chunk border, and vanilla's own saved chunks compared layer by layer
  (`light_parity`). Generated chunks are lit when sent and keep no light in their carver records, so vanilla can
  still run features and its own light (`harpas-core`, `harpas-worldgen`, `harpas-engine`, `harpas-server`).

- Ocean monument blocks on chunk workers: the exterior and all internal rooms are placed in every
  chunk intersecting the saved bounds, with the five-block clearing clipped to each, using vanilla's
  monument decoration stream (index 6 and step 4). Only the single building record is saved; elder
  guardians, scheduled fluid ticks and the complete FEATURES stage remain pending. Worker chunks,
  reversed request order and Anvil reloads are compared with the direct placement API, and complete
  passes are compared block by block with vanilla (`harpas-worldgen`, `harpas-engine`).

- Ocean monument interior placement API: all eleven room types, gold treasure core, sponge rooms,
  both wing designs and penthouse. Individual rooms and complete monument passes are compared against
  vanilla, including blocks, all six heightmaps, random consumption, rotations, chunk edges, world limits
  and reconstruction from saved records. Elder spawning follows in M4 (`harpas-worldgen`).

- Ocean monument exterior placement API: outer wings, entrance, facade, roof, stepped walls, terrain
  clearing and foundations, compared block by block against vanilla with all six heightmaps, rotations,
  chunk edges, world limits and replays after loading saved records (`harpas-worldgen`).

- Ocean monument internal room layouts, reconstructed from the world seed and saved building piece:
  connections, treasure core, fitted rooms, wings and penthouse. Room graphs, piece order, rotated bounds,
  designs and random consumption are compared against vanilla, including reconstruction after reload
  (`harpas-worldgen`).

- Stronghold blocks generated on chunk workers: corridors, doors, prisons, crossings, stairs, libraries
  and portal rooms, including chest loot metadata, silverfish spawner data and portal eyes. Placement and
  individual pass replays are compared against vanilla, including rotations, chunk boundaries, world
  limits, saved neighbors, reversed requests and Anvil reloads. Workers retain incoming container and
  spawner flags until the complete feature stage is implemented (`harpas-worldgen`, `harpas-engine`).

- Normal and badlands mineshaft blocks generated on chunk workers: rooms, crossings, stairs and corridors,
  supports and hanging chains, rails, cobwebs, torches and cave-spider spawner metadata. Placement shares
  vanilla's decoration stream across references and respects liquid shells and saved neighboring biomes.
  Blocks, heightmaps, records and replays are compared against vanilla, including world limits, partial
  floors, shared chunks, reversed worker requests and Anvil reloads. Chest minecart entities and spawner
  ticking follow in M4 (`harpas-worldgen`, `harpas-engine`).

- Overworld ruined portal blocks generated on chunk workers from the owner's templates, including
  rotations and mirrors, decay, netherrack spread and columns, waterlogging, vines/leaves and chest loot
  metadata. Whole portal results are shared across workers; saved portal templates are reconstructed on
  Anvil reload. Placement and replays are compared against vanilla, including chunk boundaries, world
  limits, saved neighbors and reversed requests (`harpas-worldgen`, `harpas-engine`).

- Cold and warm ocean ruin blocks generated on chunk workers from the owner's templates, with ocean-floor
  fitting, positional deterioration, up to five archaeology blocks per piece and chest loot metadata.
  Rotated pieces survive Anvil reloads, and placement/replay is compared against vanilla, including
  chunk boundaries, saved neighbors and reversed worker requests. Drowned spawning follows in M4
  (`harpas-worldgen`, `harpas-engine`).

- Ocean and beached shipwreck blocks generated on chunk workers from the owner's templates, with vanilla's
  height adjustment shared across workers with a source-first schedule, rotated palettes, waterlogging,
  block shapes and chest loot metadata. Saved adjusted
  positions survive Anvil reloads, and placement/replay is compared against vanilla, including chunk edges,
  saved neighbors and a chunk shared with buried treasure (`harpas-worldgen`, `harpas-engine`).

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
  chunks are saved at `minecraft:carvers` so vanilla can continue later stages. Features, other structure blocks and
  lighting remain separate roadmap tasks (`harpas-worldgen`, `harpas-engine`, `harpas-anvil`, `harpas-server`).
- A parity harness that generates whole Overworld chunks with Mojang's server and compares every block,
  biome, carving mask, fluid update and heightmap, in 1,621 chunks of 5 seeds covering all 54 biomes, along with
  the climate parameter points (`harpas-worldgen`).
- Where structures may start: structure sets, structures and biome tags are loaded, with vanilla's random spread
  and concentric ring placements, frequency reduction and exclusion zones, checked against Minecraft 26.1. The
  placement decisions are checked independently of structure block placement (`harpas-worldgen`).
- Structure starts and references for buried treasure, desert pyramids, jungle temples and swamp huts, with the
  terrain heights and biome checks that every structure uses, checked against Minecraft 26.1 (`harpas-worldgen`).
- Stronghold starts with vanilla's piece layout, checked against Minecraft 26.1 at every ring position
  (`harpas-worldgen`).
- Structure templates and template pools are read from the vanilla data: template sizes, bounding boxes,
  palettes and jigsaw blocks in every rotation, and pool elements with their weights, checked against Minecraft
  26.1 (`harpas-worldgen`).
- Jigsaw structure starts for villages, pillager outposts, ancient cities, trail ruins and trial chambers, with
  their pieces, elements and junctions, checked against Minecraft 26.1 (`harpas-worldgen`).
- Igloo, shipwreck and ocean ruin starts, whose pieces are whole templates (`harpas-worldgen`).
- Ruined portal starts in the Overworld, with their mirrored templates and heights (`harpas-worldgen`).
- Mineshaft starts of both kinds, with their rooms, corridors, crossings and stairs (`harpas-worldgen`).
- Ocean monument starts, with the surrounding biome volume, the saved building piece and its references, checked
  against Minecraft 26.1. Internal rooms and block placement remain pending (`harpas-worldgen`).
- Structure terrain adaptation (`Beardifier`), with all five modes, rigid pieces and jigsaw junctions added to
  interpolated terrain density before aquifers. Density samples and whole chunks around actual structures are
  compared with Minecraft 26.1 (`harpas-worldgen`).
- Chunk workers now adapt terrain around structures, reuse starts through a bounded shared cache and honor
  starts in saved neighbors. Generated proto-chunks keep their starts, references and piece placement metadata
  in Anvil; generation respects the world's saved `generate_structures` setting. Other structure blocks and features
  remain pending (`harpas-engine`, `harpas-worldgen`, `harpas-server`).
- Buried treasure blocks on chunk workers, including surrounding fill, chest orientation, loot table and seed,
  and final saved piece bounds. Placement, worker generation, Anvil reloads and vanilla resumption are compared
  against Minecraft 26.1; other structure blocks and features remain pending (`harpas-worldgen`, `harpas-engine`).
- Swamp hut blocks on chunk workers, including average ground height, all four orientations, roof corners,
  support columns and fence postprocessing. Placement, saved pieces across chunk boundaries, Anvil reloads and
  repeated placement are checked against Minecraft 26.1. Witch and cat spawning remains for M4; their saved
  flags are preserved so vanilla can spawn them later (`harpas-worldgen`, `harpas-engine`).
- Desert pyramid blocks on chunk workers, including shared ground height across chunks, foundations, rotated
  stairs and decorations, loot chests, the TNT trap and the archaeology cellar with suspicious sand. Placement,
  saved neighbors, Anvil reloads and vanilla replay of the pyramid pass are compared against Minecraft 26.1.
  Incoming chest flags are retained while chunks remain at `carvers`, so vanilla recreates the same chest loot
  on resumption; other features remain pending (`harpas-worldgen`, `harpas-engine`).
- Jungle temple blocks on chunk workers, including average ground height, all four orientations, randomized
  stone, tripwire traps, the redstone puzzle and chest/dispenser loot metadata. Placement, saved neighbors,
  Anvil reloads and vanilla replay are compared against Minecraft 26.1. Incoming container flags are retained
  at `carvers` for resumption; mechanism ticking remains for M4 (`harpas-worldgen`, `harpas-engine`).
- Igloo blocks on chunk workers, including rotated templates, entrance height, ladder shafts and basement,
  furnace/bed/sign/brewing stand data and chest loot metadata. Placement, chunk boundaries, saved neighbors,
  Anvil reloads and repeated placement are compared against Minecraft 26.1. Entity spawning remains for M4;
  repeating the igloo pass at `carvers` can change its height, as in vanilla, so full feature-stage resumption
  remains pending (`harpas-worldgen`, `harpas-engine`).

### Changed

- A generated chunk that arrives after nobody wants it any more is saved instead of discarded
  (`harpas-engine`).

### Fixed

- `WorldgenRandom` now sums the coordinate products in decoration seeds and uses vanilla's bit sampling with
  Xoroshiro sources, including its own Gaussian cache. Both source types are checked directly against 26.1
  (`harpas-core`, `harpas-worldgen`).

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
