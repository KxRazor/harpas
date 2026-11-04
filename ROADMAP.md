# Harpas roadmap

Milestones in the order they are built. Each one has a clear outcome ("Done when") and is split into tasks that
can be finished and reviewed on their own. Milestones carry no dates: a milestone is finished when its outcome
is reached, not when a deadline passes.

The design behind every milestone is in [docs/](docs/README.md). Release numbering is in
[docs/releases.md](docs/releases.md).

| # | Milestone | Outcome | Release |
|---|---|---|---|
| M0 | Foundations | A repository where every later task has a place to land | — |
| M1 | Risk prototypes | The riskiest designs measured before they are built | — |
| M2 | First Java connection | A Java 26.1 client joins a void world, moves and chats | 0.1.0 |
| M3 | Worlds | Vanilla worlds open, generate, save and survive a round trip | 0.2.0 |
| M4 | Vanilla gameplay | Survival basics play like vanilla | 0.3.0 |
| M5 | Bukkit plugins by default | Popular Bukkit/Paper plugins run unmodified | 0.4.0 |
| M6 | Bedrock players | Bedrock players play alongside Java players | 0.5.0 |
| M7 | Native plugin API | WASM and Java native plugins on `harpas:plugin@0.1` | 0.6.0 |
| M8 | Operations and QoL | Ready for hosting providers | 0.7.0 |
| M9 | Networks and newer clients | Proxies, proxyless networks, Java 26.2 and 26.3 clients | 0.8.0 |
| M10 | Modded mode | Fabric/NeoForge mods with plugins, for Java and Bedrock players | 0.9.0 |

---

## M0 — Foundations

**Done when:** the workspace builds in CI, contributions follow written rules, and the project's legal files
are in place.

- [x] Create the Cargo workspace with the crate layout used by the design documents (`harpas-core`,
      `harpas-protocol-java`, `harpas-protocol-bedrock`, `harpas-nethernet`, `harpas-engine`, `harpas-jvm`,
      `harpas-server`, …), each declaring `MIT OR Apache-2.0`.
- [x] Add CI: build, `rustfmt`, `clippy`, tests, on Linux, Windows and macOS.
- [x] Write `CONTRIBUTING.md`: how contributions are licensed ([docs/licensing.md](docs/licensing.md#contributions)),
      pull request size, tests required for behavior changes.
- [x] Add `CHANGELOG.md` in the format described in [docs/releases.md](docs/releases.md#changelog).
- [x] Add automated license reporting for Rust dependencies, feeding `THIRD_PARTY_NOTICES.md`.
- [x] Set up the parity CI job skeleton: download Mojang's server for tests only, after EULA acceptance in CI
      ([docs/parity.md](docs/parity.md)).

## M1 — Risk prototypes

**Done when:** the *(measure)* questions that decide architecture have numbers, recorded in the design
documents.

- [x] JVM bridge benchmark ([docs/jvm.md](docs/jvm.md#validation)): synchronous event dispatch, FFM vs. JNI,
      shared-memory reads, 200 simulated players.
- [x] Temurin vs. GraalVM Community Edition comparison on the same benchmark.
- [x] Protocol codegen prototype: generate the Java 26.1 Status and Login codecs from a schema built from
      minecraft-data ([docs/protocol-codecs.md](docs/protocol-codecs.md)); decide on ProtoDef vs. our own format.
- [x] NetherNet prototype: HTTP signaling and WebRTC data channels, with a Bedrock 26.60 client reaching the
      login packet ([docs/nethernet.md](docs/nethernet.md)).
- [x] Shared encoding prototype: encode and compress a chunk section once and serve it to many simulated
      connections, measuring cost per viewer.
- [x] Record results and update the affected documents; close or reopen design questions accordingly.

## M2 — First Java connection (0.1.0)

**Done when:** a vanilla Java 26.1 client joins a void world in online mode, moves around, sees other players
and chats.

- [x] Generated Java 26.1 codecs for all states ([docs/protocol-codecs.md](docs/protocol-codecs.md)).
  - [x] Configuration, with network NBT and tagged unions in the schema format.
  - [x] Play: joining, moving, seeing other players and chatting.
  - [x] Play: the rest of the packets whose types the schema describes, 127 of them; the 21 that need item
        stacks, entity metadata, particles, the command tree or debug subscriptions move to M4.
- [x] Connection states, status ping and server list ([docs/java-connection.md](docs/java-connection.md)).
  - [x] Framing, Handshaking, Status and the server list, legacy ping included.
  - [x] Login, Configuration and Play states, up to a void world.
- [x] Online-mode authentication with Mojang's session server; offline mode.
- [x] Logins refused with vanilla's message when `max-players` players are in Play.
- [x] Encryption and compression following `server.properties`.
- [x] Configuration state with registry data for 26.1.
- [x] Abuse protection: connection limits, packet size limits, decompression limits, timeouts.
- [x] Logic thread with the vanilla tick loop skeleton ([docs/engine-core.md](docs/engine-core.md)).
- [x] Player entity, movement validation, player list, entity tracking between players in the void world;
      collision and vehicle movement follow with worlds and gameplay.
- [x] Chat with signing, as in vanilla.
- [x] `server.properties` and `harpas.toml` loading; console and `logs/latest.log`
      ([docs/operations.md](docs/operations.md)).
- [x] Graceful shutdown on SIGTERM/SIGINT.

## M3 — Worlds (0.2.0)

**Done when:** a vanilla 26.1 world opens, plays and saves, and a new world generates the same terrain as
vanilla for the same seed.

- [x] Mojang EULA acceptance flow ([docs/world.md](docs/world.md#mojang-eula)).
- [x] Vanilla data provisioning from Mojang's server jar ([docs/world.md](docs/world.md#vanilla-game-data)).
- [x] Paletted chunk sections with version counters ([docs/engine-core.md](docs/engine-core.md#6-chunks)).
- [x] Anvil reading and writing, including `.mcc` files; `level.dat` and player data.
  - [x] Region files with `.mcc` files, in the new `harpas-anvil` crate.
  - [x] Chunk NBT to and from sections, with the block state registry.
  - [x] `level.dat` and player data, in the 26.1 world folder layout.
- [x] Chunk loading, saving and sending on worker threads; non-blocking autosave.
  - [x] Load the chunk occupied by each player from an existing overworld on worker threads, and apply the
        result on the logic thread.
  - [x] Load and send existing chunks within each player's view distance.
    - [x] Send existing block states, biomes, heightmaps and saved light in bounded batches.
    - [x] Generate 26.1 block entity type IDs locally and send updates for types compared against vanilla.
          Provisionally, generation needs Java 25 on `PATH`; if neither Java nor the generated registry is
          present, the server starts without sending block entities.
    - [x] Verify all 49 block entity types and representative update data against vanilla 26.1.
  - [x] Save chunk snapshots on workers, including non-blocking autosave, unloading and shutdown.
- [x] Make saves crash-consistent across region files and external `.mcc` chunks, including recovery and tests
      for interruptions during each write stage.
- [x] Vanilla-exact random generators and Java math semantics.
- [ ] World generation port: noise router, density functions, biomes, surface rules, aquifers, carvers.
  - [x] Vanilla-exact noise primitives (`ImprovedNoise`, `PerlinNoise`, `NormalNoise`, `BlendedNoise`,
        `SimplexNoise` and `PerlinSimplexNoise`) in `harpas-core`.
  - [x] Worldgen data loader, density functions, `NoiseChunk`, `NoiseRouter`, `MultiNoiseBiomeSource` and 3D
        biome zoom in `harpas-worldgen`.
  - [x] Base terrain block filling (`final_density`), `NoiseBasedAquifer`, `OreVeinifier` and `SurfaceSystem`
        (surface rules, conditions, badlands bands/pillars and frozen ocean icebergs).
  - [x] Cave and canyon carvers, chunk heightmaps, `level-seed` handling and worker-pool chunk generation in
        `harpas-engine`.
- [ ] Features and structures.
  - [x] Where structures may start: structure sets, random spread and concentric ring placements, frequency
        reduction and exclusion zones, in `harpas-worldgen`.
  - [x] Structure starts and references for buried treasure, desert pyramids, jungle temples and swamp huts, with
        the noise heights and biome checks every structure uses.
  - [x] Stronghold starts: the stronghold piece layout, including the retries until it has a portal room.
  - [x] Structure templates and template pools: sizes, bounding boxes, palettes and jigsaw blocks in every
        rotation, and pool elements with their weights.
  - [x] Jigsaw structure starts for villages, pillager outposts, ancient cities, trail ruins and trial chambers,
        with their pieces' elements and junctions, pool aliases and random start heights.
- [ ] Light engine port.
- [x] Parity harness: block-by-block generation comparison against vanilla.
  - [x] Whole Overworld chunks through carvers, without structures, for every biome. Later stages join the
        comparison as they are ported.
- [ ] Run the parity harnesses in CI on pull requests, as a required check, after the maintainers accept the EULA
      for CI ([docs/parity.md](docs/parity.md#mojang-eula)).
- [ ] Shared Java chunk encoding cache.
- [ ] Round-trip test: vanilla → Harpas → vanilla with no differences.
- [x] Older and newer world detection with clear errors.

## M4 — Vanilla gameplay (0.3.0)

**Done when:** survival basics (mining, building, crafting, mobs, combat, redstone basics) behave like vanilla,
as shown by the parity report.

- [ ] Block placement and breaking, block updates, scheduled and random ticks.
- [ ] Redstone components and block events, with tick-by-tick parity harnesses.
- [ ] Items and data components, inventories and containers, crafting and smelting, loot tables.
- [ ] The last 21 Java Play packets: item stacks and their 110 data components, entity metadata, particles and
      the command tree written by hand as the design says, and the debug subscription packets
      ([docs/protocol-codecs.md](docs/protocol-codecs.md#hand-written-exceptions)).
- [ ] Entity physics and collisions, ported one to one.
- [ ] Mob AI and spawning, starting with common overworld mobs.
- [ ] Combat, damage, health, hunger, effects.
- [ ] Vanilla commands on the single Brigadier tree, with operator levels ([docs/commands.md](docs/commands.md)).
- [ ] Whitelist, player and IP bans and operators from vanilla's `whitelist.json`, `banned-players.json`,
      `banned-ips.json` and `ops.json`: checked at login in vanilla's order, managed by their commands, and
      operators bypassing `max-players` as in vanilla ([docs/operations.md](docs/operations.md)).
- [ ] Datapack loading on top of vanilla data.
- [ ] Nether and End dimensions.
- [ ] Parity report per subsystem, published with the release ([docs/parity.md](docs/parity.md)).

## M5 — Bukkit plugins by default (0.4.0)

**Done when:** a fixed suite of popular T1 plugins runs unmodified, with no setup beyond dropping the jars into
`plugins/`.

- [ ] JVM runtime provisioning: detection, pinned download, verification, atomic install, offline install
      ([docs/jvm.md](docs/jvm.md#runtime-provisioning)).
  - [ ] Reuse the provisioned runtime to generate block entity type IDs without a system Java installation,
        including installations that do not yet have the registry.
- [ ] JVM embedding: creation, logic thread attachment, signal handling.
- [ ] Shim core: plugin loading, per-plugin classloaders, `plugin.yml` and `paper-plugin.yml`.
- [ ] Synchronous event dispatch with interest masks; listener priorities; generated executors.
- [ ] Shared-memory entity views and immediate writes.
- [ ] Scheduler (sync and async), with Paper's main-thread rules.
- [ ] Commands and permissions on the shared tree and nodes ([docs/commands.md](docs/commands.md)).
- [ ] Paper APIs selected for T1 (Adventure, Brigadier commands): decide and implement.
- [ ] Folia schedulers on the logic thread.
- [ ] Floodgate API ([docs/jvm.md](docs/jvm.md#floodgate-api)).
- [ ] JVM flags: compact object headers, AOT cache, automatic GC and heap sizing.
- [ ] Plugin compatibility suite in CI, compared against Paper.
- [ ] Publish the shim as a separate GPL-3.0 artifact ([docs/licensing.md](docs/licensing.md)).

## M6 — Bedrock players (0.5.0)

**Done when:** Bedrock players from the supported window join without anything installed, play with Java
players, and use clearly less memory and CPU than a proxy setup.

- [ ] NetherNet transport with robustness limits, HTTPS signaling and LAN discovery ([docs/nethernet.md](docs/nethernet.md)).
- [ ] Generated Bedrock codecs for the version window ([docs/versions.md](docs/versions.md#bedrock-window)).
- [ ] Batching and shared compression; encryption is DTLS's ([docs/nethernet.md](docs/nethernet.md)).
- [ ] Xbox Live authentication and Floodgate-compatible identity ([docs/identity.md](docs/identity.md)).
- [ ] Hashed block network IDs and Java-to-Bedrock mappings.
- [ ] Shared chunk encoding, sub-chunk requests, blob cache ([docs/bedrock-interop.md](docs/bedrock-interop.md)).
- [ ] Entities, movement validation, inventories and containers.
- [ ] Chat (mixed model), commands list, forms.
- [ ] Skins in both directions, with a signing service for Java viewers.
- [ ] `[bedrock]` settings, zero cost when disabled.
- [ ] Benchmark against a proxy setup, recorded in the design document.

## M7 — Native plugin API (0.6.0)

**Done when:** plugins written in Rust, Go, TypeScript, Python and Java run on `harpas:plugin@0.1`, and a Kotlin
plugin runs experimentally.

- [ ] Define the scope of `harpas:plugin@0.1` in WIT ([docs/plugin-api.md](docs/plugin-api.md)), including the
      edition-aware API.
- [ ] Wasmtime host: sandbox, memory limits, epoch interruption, capabilities, hot reload.
- [ ] Synchronous base world; async world for I/O.
- [ ] Rust SDK and template, then Go, TypeScript and Python.
- [ ] WIT-to-Java generator and Java SDK on the embedded JVM.
- [ ] Kotlin/Wasm SDK with its Gradle plugin, marked experimental.
- [ ] Per-language hello-world and overhead benchmarks in CI.

## M8 — Operations and QoL (0.7.0)

**Done when:** a hosting provider can deploy, monitor and administer Harpas with its usual tools.

- [ ] RCON, Query and the Server Management Protocol with the `harpas:` namespace
      ([docs/operations.md](docs/operations.md)).
- [ ] Prometheus endpoint with names compatible with existing dashboards ([docs/qol.md](docs/qol.md)).
- [ ] Health report, lag-spike monitoring, merged sampling profiler, timings.
- [ ] World pregeneration with pause, resume and trim.
- [ ] Anti-xray ([docs/world.md](docs/world.md#anti-xray)).
- [ ] Paper configuration migration notice.
- [ ] Official container image, health endpoint and hosting panel templates.

## M9 — Networks and newer clients (0.8.0)

**Done when:** Harpas works behind Velocity and BungeeCord, forms proxyless networks for both editions, and
accepts Java 26.2 and 26.3 clients.

- [ ] Velocity modern, BungeeGuard and legacy forwarding ([docs/networks.md](docs/networks.md)).
- [ ] `BungeeCord` channel pass-through and Bedrock `Connect` translation.
- [ ] Proxyless network: network table, Ed25519 transfer cookies, Bedrock transfers.
- [ ] Java 26.2 and 26.3 schemas, contributed to minecraft-data ([docs/versions.md](docs/versions.md)).
- [ ] Translation tables for newer Java clients; shared encoding per protocol version.

## M10 — Modded mode (0.9.0)

**Done when:** a Fabric or NeoForge 26.x modpack runs with plugins, Java players need only the Harpas client mod,
and Bedrock players need nothing.

- [ ] Mod detection in `mods/`, required mod runtime, version resolution ([docs/mod-support.md](docs/mod-support.md)).
- [ ] Provisioning of Mojang's server and the loader.
- [ ] Companion mod: registries and assets, core API implementation, forwarding.
- [ ] Bedrock translation of mod content, with the translation cache.
- [ ] Harpas client mod for Fabric and NeoForge: handshake, content registration.
- [ ] Coverage suite of popular mods per category.

## Later

Not scheduled yet:

- what 1.0 means and when it comes ([docs/releases.md](docs/releases.md#10));
- community adapters and the Harpas client mod's data GUIs beyond the basics;
- account linking between editions;
- custom content for the Rust engine (`content.md`);
- older Minecraft versions;
- Harpas as a proxy for both editions.
