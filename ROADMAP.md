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
- [x] Add CI: build, `rustfmt`, `clippy`, tests, on Linux and Windows for every pull request and on macOS nightly.
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
- [x] World generation port: noise router, density functions, biomes, surface rules, aquifers, carvers.
  - [x] Vanilla-exact noise primitives (`ImprovedNoise`, `PerlinNoise`, `NormalNoise`, `BlendedNoise`,
        `SimplexNoise` and `PerlinSimplexNoise`) in `harpas-core`.
  - [x] Worldgen data loader, density functions, `NoiseChunk`, `NoiseRouter`, `MultiNoiseBiomeSource` and 3D
        biome zoom in `harpas-worldgen`.
  - [x] Base terrain block filling (`final_density`), `NoiseBasedAquifer`, `OreVeinifier` and `SurfaceSystem`
        (surface rules, conditions, badlands bands/pillars and frozen ocean icebergs).
  - [x] Cave and canyon carvers, chunk heightmaps, `level-seed` handling and worker-pool chunk generation in
        `harpas-engine`.
- [x] Features and structures.
  - [x] Feature data: `worldgen/configured_feature`, `worldgen/placed_feature` and each biome's `features`
        lists, with `minecraft:ore` configurations (size, discard chance, rule tests and target states) and the
        `count`, `in_square`, `height_range` and `biome` placement modifiers. Other types keep their JSON for
        later families.
  - [x] Ore feature placement: vanilla's vein geometry, rule tests, discard chance, height providers and random
        consumption, compared with vanilla's own `OreFeature` on the same decoration random source
        (`ore_feature_parity`). Ancient debris uses `scattered_ore` and follows with the Nether.
  - [x] The FEATURES stage on chunk workers. The schedule of one chunk (global feature order per step,
        decoration and feature seeds, the biomes of the 3x3 chunks around it, placement modifiers including
        `rarity_filter`) is compared with vanilla's `applyBiomeDecoration` for ores.
        Workers share proto-chunks, decorate each chunk once its neighbors reach `carvers` and hand out a
        chunk once its neighbors have decorated, saving proto-chunks at `carvers` or `features` and resuming
        them; final chunks match vanilla and reload unchanged (`feature_worker_parity`). New chunks always run
        it, as in vanilla; the structures of each step are tracked under "Structures through the complete
        FEATURES schedule".
  - [x] Placement modifiers: all 15 types with their integer and height providers, and the block predicates
        that need no block shapes, compared position by position with vanilla's own (`placement_parity`).
  - [x] Block shapes and survival rules for the `solid`, `replaceable`, `would_survive` and `has_sturdy_face`
        predicates, and the heightmaps vanilla keeps while a chunk decorates (`block_behavior_parity`).
  - [x] Simple features: `simple_block`, the three selectors, `block_column` and `disk`, with the block state
        providers and the survival rules of the blocks they place, compared block by block with vanilla's
        `applyBiomeDecoration` (`feature_blocks_parity`).
  - [x] Trees with straight and fancy trunks and blob, fancy, spruce and pine foliage, without decorators or
        roots: free height, leaf distances in vanilla's `HashSet` order and shape updates along the tree,
        compared with vanilla's own tree features (`tree_feature_parity`).
  - [x] Tree decorators on the ported trees: trunk and leaf vines, cocoa, bee nests with their bees and leaf
        litter, with the shape updates of vines and cocoa, compared with vanilla's own tree features including
        bee nest block entities (`tree_feature_parity`).
  - [x] Fallen trees with their stump and log decorators (`attached_to_logs`), and mushroom survival without
        light, compared with vanilla's own features (`tree_feature_parity`, `feature_blocks_parity`).
  - [x] Jungle, savanna and giant trees: forking, giant and mega jungle trunks, acacia, bush, jungle and mega pine
        foliage and the `alter_ground` decorator, compared with vanilla's own tree features
        (`tree_feature_parity`).
  - [x] Dark oak and cherry trees and huge mushrooms, compared with vanilla's own features loaded from its data
        pack (`tree_feature_parity`, `feature_blocks_parity`).
  - [x] Mangroves: upwards branching trunks, random spread foliage, mangrove roots and `attached_to_leaves`,
        compared with vanilla's own features (`tree_feature_parity`, `feature_blocks_parity`).
  - [x] Pale oaks: `pale_moss` and `creaking_heart`, `vegetation_patch`, pale moss carpets with the region's own
        random source, and the `DUMMY` placeholders of block entities placed by features, compared with vanilla's
        own features (`tree_feature_parity`, `feature_blocks_parity`).
  - [x] Azaleas: the bending trunk placer and the `root_system` feature that grows them above lush caves, with
        rooted dirt and hanging roots, compared with vanilla's own features (`tree_feature_parity`,
        `feature_blocks_parity`).
  - [x] Vegetation, lakes, springs and the remaining feature families. Every Overworld feature type is placed;
        `bonus_chest` is never generated by a dedicated server, and the Nether's and the End's follow with them.
    - [x] Seagrass, kelp and sea pickles, with the survival rules of the blocks they place, compared with
          vanilla's `applyBiomeDecoration` (`feature_blocks_parity`).
    - [x] Vines and glow lichen, with `MultifaceSpreader` spreading lichen from its first face, compared with
          vanilla's `applyBiomeDecoration` (`feature_blocks_parity`).
    - [x] Ice spikes, packed and blue ice icebergs and blue ice, compared with vanilla's `applyBiomeDecoration`
          (`feature_blocks_parity`).
    - [x] Water and lava springs and underground and surface lava lakes, without the ticks they schedule,
          compared with vanilla's `applyBiomeDecoration` (`feature_blocks_parity`).
    - [x] Bamboo stalks and podzol patches, with bamboo and bamboo shoot survival and shape updates,
          without the ticks they schedule, compared with vanilla's
          own feature and `applyBiomeDecoration`, including worker generation and Anvil reloads
          (`bamboo_feature_parity`, `feature_blocks_parity`, `feature_worker_parity`).
    - [x] Forest rocks (`block_blob`), with 26.1's configurable block and support predicate, compared with
          vanilla's own feature and `applyBiomeDecoration`, including worker generation and Anvil reloads
          (`block_blob_feature_parity`, `feature_blocks_parity`, `feature_worker_parity`).
    - [x] Desert wells, with sandstone foundations, water, slab roofs and buried suspicious sand with loot
          metadata, compared with vanilla's own feature and `applyBiomeDecoration`, including worker generation
          and Anvil reloads (`desert_well_feature_parity`, `feature_blocks_parity`, `feature_worker_parity`).
    - [x] Warm ocean coral reefs: trees, claws and mushrooms, with coral plants, fans and sea pickles, compared
          with vanilla's own features and `applyBiomeDecoration`, including worker generation and Anvil reloads
          (`coral_feature_parity`, `feature_blocks_parity`, `feature_worker_parity`).
    - [x] Underwater magma deposits: floor searches, directional occlusion and placement random consumption,
          compared with vanilla's own feature and `applyBiomeDecoration`, including worker generation,
          chunk edges and Anvil reloads (`underwater_magma_parity`, `feature_blocks_parity`, `feature_worker_parity`).
    - [x] Surface snow and ice (`freeze_top_layer`): biome temperatures, height adjustment, snow support and
          snowy ground, compared with vanilla's own feature in every biome, including world limits, repeated
          placement, worker generation and Anvil reloads (`freeze_top_layer_parity`, `feature_blocks_parity`,
          `feature_worker_parity`).
    - [x] Small stalactites and stalagmites (`pointed_dripstone`): base patches, direction selection, height
          and waterlogging, compared with vanilla's own feature and `applyBiomeDecoration`, including worker
          generation and Anvil reloads (`pointed_dripstone_parity`, `feature_blocks_parity`,
          `feature_worker_parity`).
    - [x] Large dripstone (`large_dripstone`): cave searches, embedded and shrinking bases, radial profiles,
          shared wind offsets and obstacle handling, compared with vanilla's own feature and
          `applyBiomeDecoration`, including worker generation and Anvil reloads (`large_dripstone_parity`,
          `feature_blocks_parity`, `feature_worker_parity`).
    - [x] Dripstone clusters (`dripstone_cluster`): independent floor and ceiling searches, wet pools, layered
          bases, biased column heights, merging tips and waterlogging, compared with vanilla's own feature and
          `applyBiomeDecoration`, including repeated placement, worker generation and Anvil reloads
          (`dripstone_cluster_parity`, `feature_blocks_parity`, `feature_worker_parity`).
    - [x] Monster rooms (`monster_room`): cave validation, cobblestone walls and mossy floors, chest orientation
          and loot seeds, and spawner species and metadata, compared with vanilla's own feature and
          `applyBiomeDecoration`, including overlapping rooms, world limits, worker generation, chunk edges
          and Anvil reloads (`monster_room_parity`, `feature_blocks_parity`, `feature_worker_parity`). Spawner
          ticking and opening loot follow in M4.
    - [x] Lush cave clay patches and pools (`waterlogged_vegetation_patch`), with small and big dripleaves,
          support rules, waterlogging and shape updates, compared with vanilla's own features and
          `applyBiomeDecoration`, including repeated placement, chunk edges, world limits, worker generation
          and Anvil reloads (`lush_vegetation_parity`, `feature_blocks_parity`, `feature_worker_parity`).
    - [x] Amethyst geodes (`geode`): world-seeded shape noise, layered shells, cracks, directional crystals and
          waterlogging, compared with vanilla's own feature and `applyBiomeDecoration`, including repeated
          placement, protected blocks, world limits, worker generation, chunk edges and Anvil reloads
          (`geode_parity`, `feature_blocks_parity`, `feature_worker_parity`). Fluid ticks follow in M4.
    - [x] Village block piles (`block_pile`): hay with random axes, melons, pumpkins, snow and weighted ice,
          with support checks and dirt-path coin flips, compared with vanilla's own features, including
          repeated placement, chunk edges and world limits (`block_pile_parity`), plus synthetic worker
          decoration and Anvil reload tests. Calling them from village pool elements follows with jigsaw blocks.
    - [x] Sculk patches (`sculk_patch`) and sculk veins (`multiface_growth`): the world generation sculk
          spreader with its charge cursors, vein spreading, sculk conversion, growths, catalysts and shriekers,
          and the general `MultifaceSpreader`, compared with vanilla's own features (`sculk_feature_parity`).
    - [x] Fossils (`fossil`): bone templates and ore overlays buried under the ocean floor, with their processor
          lists and empty-corner checks, compared with vanilla's own features (`fossil_feature_parity`).
  - [x] Jigsaw structure blocks: villages, pillager outposts, ancient cities, trail ruins and trial chambers.
    - [x] Processor lists and per-block `rule` substitutions: all six block predicates, distance and
          axis-distance tests, positional randomness and passthrough, clear and loot NBT modifiers,
          compared with vanilla's own processors (`rule_processor_parity`).
    - [x] Capped rule processors: whole-template limits, world-seeded candidate shuffles, unchanged and
          NBT-only results, integer limit providers and ordered finalizers, compared with vanilla's own
          processor pipeline (`capped_processor_parity`). Worker template placement follows below.
    - [x] Template block filters: `block_rot`, `protected_blocks`, `block_ignore` and `nop`, in an ordered
          pipeline with rules and capped finalizers. All 40 shipped processor lists, original versus current
          states, preserved NBT and placement-settings randomness are compared with vanilla's own pipeline
          (`filter_processor_parity`). Template placement on workers follows below.
    - [x] Template gravity processors: all six heightmaps, local heights, live-level heightmap substitutions
          and ordered filter/rule/finalizer interactions, compared with vanilla's own pipeline
          (`gravity_processor_parity`).
    - [x] Template material processors: `block_age`, `blackstone_replace` and `lava_submerged_block`, with
          tag membership, state properties, outline shapes and placement-settings randomness, compared with
          vanilla's own pipeline (`material_processor_parity`, `gravity_processor_parity`).
    - [x] Static rule NBT (`append_static`): recursive compound merges, JSON numeric and list types,
          existing-NBT mutation and capped limit accounting, compared with vanilla's own ordered pipeline
          (`static_processor_parity`).
    - [x] Jigsaw replacement, including strict command block states, discarded SNBT validation and
          structure-void filtering, compared with vanilla’s ordered pipeline (`jigsaw_processor_parity`).
    - [x] Template block state rotation and mirroring, including vanilla's stair, chest, anvil, fire and
          chorus plant exceptions, compared for every registry state (`block_transform_parity`).
    - [x] Single and legacy single pool element placement in one chunk: implied processors, state turns,
          chunk clipping, waterlogging, water filling, unchanged edge shapes and block entity loot seeds,
          compared for every single element of every pool (`pool_element_parity`).
    - [x] List and feature pool elements: lists in order with replaced block entities, and every village
          feature placed through its placement modifiers into the generation region (`pool_element_parity`),
          including the ancient city's sculk patches.
    - [x] Jigsaw pieces on chunk workers: decoration-seeded random, processor pivot, clipped templates and
          feature elements written into the region, compared in 384 chunks of nine real starts and their
          neighbors, sculk patches included (`jigsaw_chunk_parity`).
    - [x] Remaining capped delegates: every `capped` processor in vanilla's data has a `rule` delegate, which is
          ported.
  - [x] Structures through the complete FEATURES schedule and full resumption.
    - [x] Structures placed by each decoration step before its features, with their own seeds, for every start
          that reaches the chunk; jigsaw structures run there (`jigsaw_chunk_parity`, `feature_worker_parity`).
    - [x] Buried treasure in `underground_structures`, saving its moved piece with the chunk's start
          (`buried_treasure_parity`).
    - [x] Swamp huts in `surface_structures`, saving the ground height of a new hut with its start
          (`swamp_hut_parity`).
    - [x] Ocean monuments in `surface_structures`, with the decoration's own stream (`monument_parity`).
    - [x] Desert pyramids, jungle temples and igloos in `surface_structures`, reading their neighbors from the
          decoration region, with each new pyramid's shared height fixed once from the terrain before
          decoration and the container flags saved as vanilla saves them (`desert_pyramid_parity`,
          `jungle_temple_parity`, `igloo_parity`).
    - [x] Shipwrecks, ocean ruins and ruined portals in `surface_structures`, reading the terrain before
          decoration around the chunk, with shared shipwreck heights and portal placements prepared once
          (`shipwreck_parity`, `ocean_ruin_parity`, `ruined_portal_parity`).
    - [x] Mineshafts in `underground_structures` after buried treasure, and strongholds in `surface_structures`,
          leaving carvers to adapt the terrain only (`mineshaft_parity`, `stronghold_parity`).
    - [x] Full resumption from `features`: chunks vanilla saved at `initialize_light`, whose neighbors can still
          decorate, are resumed with the others, and chunks from `light` on are final, from a world the server
          itself generated and completed again (`world_edge_parity`).
  - [x] Where structures may start: structure sets, random spread and concentric ring placements, frequency
        reduction and exclusion zones, in `harpas-worldgen`.
  - [x] Structure starts and references for buried treasure, desert pyramids, jungle temples and swamp huts, with
        the noise heights and biome checks every structure uses.
  - [x] Stronghold starts: the stronghold piece layout, including the retries until it has a portal room.
  - [x] Structure templates and template pools: sizes, bounding boxes, palettes and jigsaw blocks in every
        rotation, and pool elements with their weights.
  - [x] Jigsaw structure starts for villages, pillager outposts, ancient cities, trail ruins and trial chambers,
        with their pieces' elements and junctions, pool aliases and random start heights.
  - [x] Igloo, shipwreck and ocean ruin starts, whose pieces are whole templates turned around a pivot.
  - [x] Ruined portal starts: setups by weight, mirrored templates and the height where three corners stand on
        terrain.
  - [x] Mineshaft starts of both kinds: rooms, corridors, crossings and stairs, below the sea level or up to the
        surface in badlands.
  - [x] Ocean monument starts: the surrounding biome volume, the saved building piece and its references.
        Room layouts and block placement are tracked separately below.
  - [x] Woodland mansion starts: the layout of corridors, rooms and the third floor, and its template pieces with
        their rotations and left-right and front-back mirrors, compared with vanilla (`mansion_layout_parity`,
        `structure_start_parity`, `block_transform_parity`).
  - [x] Woodland mansion block placement on chunk workers: templates with both mirrors, waterlogging, the shape
        updates of their edges and blocks, including connecting blocks and those reaching into neighboring
        chunks, chests with loot, mob markers and cobblestone foundations, compared chunk by chunk with vanilla
        (`mansion_parity`). Mob spawning follows in M4.
  - [x] Structure terrain adaptation (`Beardifier`): rigid pieces, jigsaw junctions and all five adjustment
        modes, connected to chunk generation and compared against vanilla.
  - [x] Structure terrain adaptation on the chunk workers, with a shared bounded cache of starts, saved-neighbor
        starts, Anvil starts/references and piece placement metadata, and worker terrain/persistence parity tests.
  - [x] Buried treasure block placement on chunk workers: foundation search, surrounding fill, chest orientation,
        loot metadata and final piece bounds, with vanilla placement/resumption and Anvil reload comparisons.
  - [x] Swamp hut block placement on chunk workers: ground height, rotated walls and roof, support columns and
        fence postprocessing, with vanilla placement/resumption, chunk-boundary and Anvil reload comparisons.
        Witch and cat spawning follows in M4; their saved flags remain unchanged.
  - [x] Desert pyramid block placement on chunk workers: shared lowest ground height, foundations, rotated
        blocks, four loot chests, TNT trap and archaeology cellar, with chunk-boundary, saved-neighbor, Anvil
        reload and vanilla placement/resumption comparisons. Chest flags stay deferred until FEATURES is complete.
  - [x] Jungle temple block placement on chunk workers: average ground height, rotated cobblestone/mossy
        blocks, tripwire traps, redstone puzzle, loot chests and dispensers, with vanilla placement/resumption,
        chunk-boundary, saved-neighbor and Anvil reload comparisons. Container flags stay deferred until
        FEATURES is complete; mechanism ticking follows in M4.
  - [x] Igloo block placement on chunk workers: rotated templates, entrance height, ladder shafts, basement
        blocks, block entity data and chest loot metadata, with vanilla placement/replay, chunk-boundary,
        saved-neighbor and Anvil reload comparisons. Entity spawning follows in M4; full FEATURES resumption
        remains pending.
  - [x] Shipwreck block placement on chunk workers: shared source-first ocean/beached heights, rotated
        templates and palettes, waterlogging, stair/fence/door shapes and chest loot metadata, with vanilla placement/replay,
        chunk-boundary, saved-neighbor and Anvil reload comparisons, including a chunk shared with buried
        treasure. Full FEATURES resumption remains pending.
  - [x] Cold and warm ocean ruin block placement on chunk workers: rotated templates, ocean-floor fitting,
        positional deterioration, capped archaeology, waterlogged chests and loot metadata, with vanilla
        placement/replay, chunk-boundary, saved-neighbor, reversed-request and Anvil reload comparisons.
        Drowned spawning follows in M4; full FEATURES resumption remains pending.
  - [x] Overworld ruined portal block placement on chunk workers: mirrored and rotated templates, positional
        decay, waterlogging, netherrack spread and drip columns, vines/leaves and chest loot metadata, with
        vanilla placement/replay, world-limit, saved-neighbor, reversed-request and Anvil reload comparisons.
        Complete FEATURES-stage resumption remains pending.
  - [x] Normal and badlands mineshaft block placement on chunk workers: rooms, crossings, stairs, corridors,
        floor supports and hanging chains, rails, cobwebs, torches and cave-spider spawner metadata, with
        vanilla placement/replay, partial floors, liquid/biome checks, world limits, shared decoration streams,
        saved neighbors, reversed requests and Anvil reload comparisons. Spawner flags remain deferred at
        `carvers`; chest minecart entities and spawner ticking follow in M4. Full FEATURES resumption remains pending.
  - [x] Stronghold block placement on chunk workers: corridors, prisons, crossings, stairs, libraries and portal
        rooms, rotated doors and decorations, chest loot metadata, portal eyes and silverfish spawner data,
        with vanilla placement/replay, chunk-boundary, world-limit, saved-neighbor, reversed-request and Anvil
        reload comparisons. Workers retain incoming chest/spawner flags at `carvers`; spawner ticking follows
        in M4. Full FEATURES resumption remains pending.
  - [x] Ocean monument internal room layouts: the connected cell graph, treasure core, fitted rooms, wings and
        penthouse, with all orientations, designs, random consumption and reconstruction from saved building
        records compared against vanilla. Block placement is tracked separately below.
  - [x] Ocean monument exterior placement API: outer wings, entrance, facade, roof, stepped walls, terrain
        clearing and foundations, with block and heightmap comparisons against vanilla, all orientations,
        chunk edges, world limits and saved-record pass replays.
  - [x] Ocean monument interior placement API: all eleven room types, treasure core, sponge rooms, both wing
        designs and penthouse, with individual rooms and complete monument passes compared against vanilla:
        blocks, six heightmaps, decoration random consumption, rotations, chunk edges, world limits and
        saved-record replays. The complete FEATURES stage remains pending; elder guardian spawning follows
        in M4.
  - [x] Ocean monument blocks generated on chunk workers: exterior and all internal rooms in every chunk
        intersecting the saved bounds, with the five-block clearing clipped to each, using vanilla's
        monument decoration stream (index 6, step 4) and keeping the single saved building record.
        Worker chunks, reversed request order and Anvil reloads are compared with the direct API, and
        complete passes block by block with vanilla. Elder guardians, scheduled fluid ticks and the
        complete FEATURES stage remain pending.
- [x] Light engine port.
  - [x] Sky and block light for generated chunks, with per-state emission and dampening from the block registry,
        one-chunk border propagation and vanilla's saved-light parity harness. Generated chunks are lit when
        sent; their carver records keep no light so vanilla can still run features and light.
  - [x] Run light on the worker pool: the workers light each chunk once its neighbors are loaded, and again when
        a chunk of its volume changes. Light updates from block changes follow with them in M4.
  - [x] Save light at the `light` stage: a final chunk that ran FEATURES is saved with the light the workers
        computed, `isLightOn` and vanilla's `light` status, so vanilla keeps the light and spawns its mobs, and
        the server's own light on the same chunks matches it (`world_edge_parity`).
- [x] Parity harness: block-by-block generation comparison against vanilla.
  - [x] Whole Overworld chunks through carvers, without structures, for every biome. Later stages join the
        comparison as they are ported.
  - [x] Whole chunks through carvers with terrain adaptation around villages, ancient cities, trail ruins and
        trial chambers, including neighboring chunks; their structure blocks are compared separately
        (`jigsaw_chunk_parity`).
- [x] Run the parity harnesses in CI: a reduced sample on pull requests and the full one on `main`
      ([docs/parity.md](docs/parity.md#running-the-comparisons)).
- [x] Make `parity against vanilla` a required check.
- [x] Shared Java chunk encoding cache: complete compressed columns shared between viewers, bounded by
      retained bytes and entry count, invalidated by chunk/light snapshots and compression settings, with
      concurrent-viewer, content-change, reload and cipher-stream tests.
- [x] Round-trip test: vanilla → Harpas → vanilla with no differences: every chunk of a vanilla world, at every
      status, rewritten whole by the server, holds what vanilla saved, and vanilla reopens it without chunk errors
      (`round_trip_parity`).
- [x] Older and newer world detection with clear errors.

## M4 — Vanilla gameplay (0.3.0)

**Done when:** survival basics (mining, building, crafting, mobs, combat, redstone basics) behave like vanilla,
as shown by the parity report.

- [ ] Block placement and breaking, block updates, scheduled and random ticks.
  - [x] Breaking blocks in Creative: reach and height checks, the fluid left behind, heightmaps, block
        entities, saving, block changes sent to the players who have the chunk, world events and
        acknowledgements.
  - [x] Breaking blocks in Survival, with each block's destroy speed and the destroy progress others see.
  - [x] Wooden and stone pickaxes use block tags for mining speed and harvest tier. Tool wear persists with
        the inventory. Stone, coal, iron and other representative blocks are compared with vanilla (`mining_parity`).
  - [x] Iron pickaxes harvest diamond, gold and redstone ore, with speed, harvest eligibility and wear
        compared with vanilla (`mining_parity`).
  - [x] Diamond pickaxes harvest obsidian. Mining speed, harvest eligibility and wear are compared with
        vanilla (`mining_parity`), and the Survival test checks that wear persists on reconnect.
  - [ ] Mining with the remaining tools, enchantments and player effects.
  - [x] Shape updates: the neighbors of a changed block take their new shapes in vanilla's order, and those
        that can no longer stand break, with the rules ported for world generation.
  - [x] Neighbor updates (`neighborChanged`) in vanilla's order, which liquids react to.
  - [ ] The other blocks that react to their neighbors, with redstone.
  - [x] Placing blocks from the Creative inventory and the hotbar (`BlockItem.place`), for blocks without
        properties, pillars, slabs, stairs, blocks that face the player and connecting blocks.
  - [ ] The placements of the other blocks, and clicks on blocks that react to being used.
  - [x] Scheduled ticks in vanilla's order (`LevelTicks`), saved with their chunks, and the fluid ticks of
        water and lava flowing, falling into holes and hardening into obsidian, cobblestone, stone and basalt.
  - [x] Redstone lamp ticks run before fluid ticks and keep their remaining delay across a chunk reload.
  - [ ] Run ticks for the remaining block types. Their saved tick entries are preserved.
  - [ ] Random ticks.
- [ ] Light updates from block changes, queued and applied at defined points as vanilla's light engine applies
      them, with their timing compared against vanilla ([docs/engine-core.md](docs/engine-core.md)).
  - [x] Light chunks again on the workers when a block changes its light within 15 blocks of them, write the new
        light into their sections, save it, and send it to the players at whose view's edge each chunk is. A
        chunk saved before it is lit again is saved without `isLightOn`, so that vanilla lights it.
  - [ ] Compare the timing of light updates against vanilla's light engine.
- [ ] Redstone components and block events, with tick-by-tick parity harnesses.
  - [x] A lever directly beside a lamp powers it immediately. After power is removed the lamp turns off
        four game ticks later, including after a save and reload (`lever_lamp_delay_survives_reload`).
- [ ] Items and data components, inventories and containers, crafting and smelting, loot tables.
  - [x] The item registry, item stacks, and the player's inventory menu in Creative, with its changes sent to
        the client.
  - [x] Player inventory persistence: main inventory, hotbar, armor, offhand and selected slot loaded and
        saved in vanilla player data on workers, on autosave, departure and shutdown; latest snapshots on
        rapid reconnects, preserved unrelated NBT and backup recovery. The supported item components and
        saved inventory/equipment format are compared with vanilla (`player_inventory_parity`).
  - [x] Block loot tables, evaluated as vanilla does for a broken block, drawing from the world's random
        sequences.
  - [x] Clicks in the inventory menu in Survival, every click type, and the 2x2 crafting grid with the shaped
        and shapeless recipes of the vanilla data pack.
  - [x] Crafting tables: opening the 3x3 menu, shaped and shapeless recipes, clicks and shift-crafting,
        synchronized inventory slots, and returning ingredients when the menu closes or the table is no
        longer reachable; native recipe and shift-click comparisons (`crafting_table_parity`).
  - [x] Simple chests: placement, opening a shared 27-slot menu, item drops on breaking, and vanilla-compatible
        contents saved in Anvil. Shift-click transfers and the saved chest block entity are checked against
        vanilla (`chest_parity`).
  - [x] Double chests: pairing by placement, a shared 54-slot menu, and separating on break with the other
        half's items intact. Slot order, shift-click transfers and Anvil storage are compared with vanilla
        across a chunk boundary (`chest_parity`).
  - [x] Dungeon chests roll their loot on first opening and save the resulting slots in Anvil. The item stacks,
        enchanted books and slot order are compared with vanilla for eight seeds (`chest_parity`).
  - [x] Breaking an unopened dungeon chest rolls and drops its contents. The dropped stacks are compared against
        vanilla with a saved loot seed and with the world's random sequence (`chest_parity`).
  - [x] Desert pyramid chests roll their loot when opened or broken unopened. Menu slots, saved contents and
        dropped stacks match vanilla, including enchanted books and Dune armor trim templates (`chest_parity`).
  - [x] Buried treasure chests roll their loot when opened or broken unopened, including hearts of the sea and
        potions of Water Breathing. Menu slots, saved contents and dropped stacks match vanilla (`chest_parity`).
  - [x] Shipwreck treasure chests roll their loot when opened or broken unopened. Menu slots, saved contents
        and dropped stacks match vanilla, including Coast armor trim templates and nautilus armor (`chest_parity`).
  - [x] Shipwreck supply chests roll their loot when opened or broken unopened. Menu slots, saved contents
        and dropped stacks match vanilla, including suspicious stew effects and enchanted leather armor (`chest_parity`).
  - [x] Shipwreck map chests roll their loot when opened or broken unopened. The buried treasure target, initial
        coastline preview, saved map data and named empty map fallback are compared with vanilla (`chest_parity`).
  - [x] Small and big ocean ruin chests roll their loot when opened or broken unopened. Only chests whose loot
        includes the treasure map search for buried treasure and take a map ID. Menu slots, saved contents,
        dropped stacks and map data match vanilla (`chest_parity`).
  - [x] Normal furnaces open a three-slot menu, smelt on the game tick, and save their slots and timers in Anvil.
        Fuel values, blocked output and resumed iron smelting are compared with vanilla (`furnace_parity`).
  - [ ] The other chest loot tables, containers and recipe types, furnace experience and recipe-book placement.
  - [ ] Data components, which loot functions copy into items and conditions read, and the other loot tables.
- [ ] The last 21 Java Play packets: item stacks and their 110 data components, entity metadata, particles and
      the command tree written by hand as the design says, and the debug subscription packets
      ([docs/protocol-codecs.md](docs/protocol-codecs.md#hand-written-exceptions)).
  - [x] Item stacks, with their component values kept as bytes, and Set Container Content, Set Container Slot
        and Set Creative Mode Slot.
  - [x] Set Entity Metadata, with the serializers item entities use.
  - [x] Click Container, with stacks hashed as the client predicts them, and Set Cursor Item.
  - [x] Saved-NBT/network conversion for 29 item component values, including damage, names, lore, custom
        data, enchantments, stored enchantments, potion contents without custom effects and suspicious stew
        effects, map decorations, and removed components. Unsupported values are refused without overwriting the player's data.
  - [x] The Commands packet encodes the permission-filtered Brigadier tree sent to Java players.
  - [x] Level Particles carries the type ID and its options as wire bytes. Sword sweep particles are compared
        with vanilla (`sweeping_attacks_match_vanilla`).
  - [ ] The remaining data component values, particle options, the other serializers, and the other 13 packets.
- [ ] Entity physics and collisions, ported one to one.
  - [x] Entity storage as the design describes, and item entities: gravity, collisions with blocks, friction,
        floating, burning in lava, merging and despawning, sent to the players who see them, and dropped by
        players.
  - [x] Block drops from the vanilla block loot tables, and players picking items up.
  - [x] Item entity persistence: stacks and supported components, UUIDs, movement, age and pickup delay
        survive autosave, chunk unload and shutdown in vanilla entity regions. Other saved entities keep
        their NBT. The format round trip is compared with vanilla (`item_entity_parity`).
  - [x] Saved adult pigs load from Overworld entity regions and appear to Java players tracking their chunk.
        UUID, position, rotation, health and appearance are compared with vanilla through a save and reopen
        (`saved_adult_pig_matches_vanilla`). Engine tests cover tracking changes and chunk unload and reload.
  - [x] Saved adult pigs fall and land on solid ground, with motion sent to Java viewers
        (`falling_adult_pig_matches_vanilla`). A pig that crosses a chunk boundary saves in its new chunk
        and reloads once. Pigs with `NoAI` remain suspended in air as in vanilla.
  - [x] A saved adult cow can be tracked and punched in Java Survival. Health persists across a restart,
        and a lethal hit rolls the cow loot table (`saved_cow_combat_and_drops_match_vanilla`).
  - [ ] The other entity types and their persistence.
- [ ] Mob AI and spawning, starting with common overworld mobs.
  - [x] Saved adult cows follow nearby players holding wheat on open, flat ground. A Java Survival client
        compares approach and stopping after a hotbar switch with vanilla (`saved_cow_follows_wheat_matches_vanilla`).
  - [x] A saved adult cow follows wheat around a two-block-high wall on flat ground and stops pursuing when
        the player hides the wheat, compared with vanilla (`saved_cow_walks_around_a_wall_matches_vanilla`).
  - [x] A saved adult cow jumps up a dry one-block step to follow wheat and stops pursuing when it is hidden.
        The jump, landing and saved position are compared with vanilla (`saved_cow_jumps_up_a_step_matches_vanilla`).
  - [x] A saved adult cow walks down a dry one-block step to follow wheat. The drop, landing, stopping when
        the wheat is hidden and position after restart are compared with vanilla (`saved_cow_walks_down_a_step_matches_vanilla`).
  - [x] A saved adult cow follows wheat across a dry one-block-deep trench between banks at the same height.
        The descent, jump back up, stopping when the wheat is hidden and position after restart are compared
        with vanilla (`saved_cow_crosses_a_dry_trench_matches_vanilla`).
  - [x] A saved adult cow walks down dry ledges two or three blocks high to follow wheat. The fall, landing,
        stopping when the wheat is hidden and position after restart are compared with vanilla
        (`saved_cow_walks_down_larger_steps_matches_vanilla`).
  - [ ] Navigation across other height changes and through liquids, broader obstacle parity and
        other animal goals.
  - [ ] Compare natural cow spawning on a fixed grassy platform under matching light, biome and mob-cap
        conditions.
- [ ] Combat, damage, health, hunger, effects.
  - [x] Java Survival fall damage on ordinary terrain, saved health on reconnect, and death and respawn at the
        Overworld spawn. Safe, damaging and lethal falls and Creative immunity are compared with vanilla
        (`fall_damage_and_respawn_match_vanilla`).
  - [x] Java Survival sprinting spends saturation and food, apples can be eaten from either hand, and natural
        regeneration uses the saved world rule. Hunger, saturation, exhaustion and the regeneration timer persist.
        Sprinting, cancelled and completed apple use, reconnects and both food-based regeneration rates are
        compared with vanilla (`food_and_natural_regeneration_match_vanilla`).
  - [x] Java Survival starvation uses the saved food timer and the world's difficulty. The first hit after a
        reconnect, the health thresholds, Peaceful food recovery and death and respawn on Hard are compared with
        vanilla (`starvation_matches_vanilla`).
  - [x] Basic foods use the locally generated item defaults for nutrition, saturation and use duration. Bread,
        steak and dried kelp are compared with vanilla, including the inventory on reconnect
        (`food_and_natural_regeneration_match_vanilla`).
  - [x] Mushroom stew, rabbit stew and beetroot soup return a bowl when eaten. Bowls from either hand,
        cancelling use and the inventory on reconnect are compared with vanilla
        (`food_and_natural_regeneration_match_vanilla`).
  - [x] Rotten flesh and raw chicken can apply Hunger from their default item components. Applying the
        effect, reconnecting, expiry and the resulting exhaustion are compared with vanilla
        (`food_hunger_effect_matches_vanilla`).
  - [x] Spider eyes and poisonous potatoes can apply Poison from their default item components. Effect
        application, magic damage, reconnecting, expiry and the health limit are compared with vanilla
        (`food_poison_matches_vanilla`).
  - [x] Pufferfish apply Poison II, Hunger III and Nausea together. Effect packets, remaining durations on
        reconnect and the Poison II damage interval are compared with vanilla (`food_pufferfish_matches_vanilla`).
  - [x] Normal golden apples can be eaten at full hunger and grant Regeneration II and Absorption. Healing,
        absorbed fall damage, eating again, reconnecting and expiry are compared with vanilla
        (`food_golden_apple_matches_vanilla`).
  - [x] Enchanted golden apples grant Regeneration II, Resistance, Fire Resistance and Absorption IV at full
        hunger. Fall damage, lava and the fire it leaves, expiry and reconnects are compared with vanilla
        (`food_enchanted_golden_apple_matches_vanilla`).
  - [x] Suspicious stew can be eaten at full hunger, returns a bowl and applies the supported effects stored on
        the stack. Regeneration and Poison are compared with vanilla, including both hands and reconnects
        (`food_suspicious_stew_matches_vanilla`).
  - [x] Milk can be drunk from either hand at full hunger. A finished drink returns an empty bucket and
        clears active status effects, including Absorption hearts. Cancellation, effect removal and
        reconnecting are compared with vanilla (`milk_bucket_matches_vanilla`).
  - [x] Honey bottles can be drunk from either hand at full hunger. A finished drink restores food and
        saturation, returns a glass bottle and removes Poison while keeping Absorption. Use duration,
        cancellation and reconnecting are compared with vanilla (`honey_bottle_matches_vanilla`).
  - [x] Normal, extended and stronger Regeneration potions read their variant from the held stack and return
        a glass bottle. Both hands, full hunger, cancellation, the healing interval and remaining effect
        duration on reconnect are compared with vanilla (`regeneration_potions_match_vanilla`).
  - [x] Java Survival players can punch each other with an empty main hand. Reach, damage, knockback, a
        rapid repeat attack and a player-attributed death are compared with vanilla
        (`bare_hand_combat_matches_vanilla`).
  - [x] Java Survival players can punch saved adult pigs. Range, damage, knockback, rapid repeat hits,
        death and health across restarts are compared with vanilla (`bare_hand_pig_combat_matches_vanilla`).
  - [x] Java Survival players can fight with swords. The locally generated item defaults supply attack damage,
        attack speed and durability. Wooden and iron swords are compared with vanilla for damage, recharge,
        switching the held item and saved wear (`sword_combat_matches_vanilla`).
  - [x] Unenchanted armor reduces Java Survival melee damage between players and wears on impact. Health,
        chestplate breakage, the armor bar and saved equipment are compared with vanilla for unarmored, iron
        and diamond defenders (`armored_combat_matches_vanilla`).
  - [x] Protection on equipped armor reduces melee damage between Java Survival players. The combined level,
        reduction limit, breakage on impact and interaction with Resistance and Absorption are compared with
        vanilla (`protection_matches_vanilla`).
  - [x] Sharpness on the main-hand sword increases Java Survival melee damage before armor mitigation.
        Levels I and V are compared with vanilla for full and partial charge against unarmored players and
        players wearing iron chestplates (`sharpness_matches_vanilla`).
  - [x] Charged punches and sword strikes can critically hit while falling in Java Survival. Damage and
        the critical animation are compared with vanilla, including sprinting, partial charge, disabled fall
        damage and Sharpness against armor (`critical_hits_match_vanilla`).
  - [x] Charged punches and sword strikes while sprinting add knockback in the attacker's facing direction
        and end the sprint in Java Survival. The defender's velocity, attack charge and sprint state are
        compared with vanilla (`sprint_knockback_matches_vanilla`).
  - [x] Knockback I and II on the main-hand sword add push at full and partial charge in Java Survival.
        Velocity, sprint cancellation, damage immunity and an enchanted offhand sword are compared with
        vanilla (`knockback_enchantment_matches_vanilla`).
  - [x] Fire Aspect on the main-hand sword ignites Java Survival players after a successful hit. Both
        levels, a hit rejected during damage immunity and Fire Resistance are compared with vanilla
        (`fire_aspect_matches_vanilla`).
  - [x] Shields raised in either hand block sword strikes from the front in Java Survival. Frontal blocks,
        rear hits, health, defender knockback, Fire Aspect, shield breakage and saved wear are compared with
        vanilla (`shield_blocking_matches_vanilla`).
  - [x] Axes use their own attack damage, speed and wear, and disable a raised shield for five seconds
        when it blocks an axe strike. Wooden and iron axes at full and partial charge, shields in both
        hands, cooldown expiry and saved wear are compared with vanilla (`axe_combat_matches_vanilla`).
  - [x] A charged grounded sword strike can sweep nearby players for one base damage point and extra knockback.
        Target bounds, reach, movement, partial charge, sprinting, critical hits and the sound and particles
        are compared with vanilla (`sweeping_attacks_match_vanilla`).
  - [x] Sweeping Edge I–III on the attacking sword add a share of the attack's charged base damage to the
        sweep. Health of both targets, Sharpness on the same sword, Sweeping Edge in the offhand and a hit
        during an equipment change are compared with vanilla (`sweeping_attacks_match_vanilla`). An engine
        test checks which level applies before and after the equipment tick.
  - [x] Adult pigs killed by a bare-hand attack drop raw porkchops from the locally installed vanilla loot
        table. The count, pickup, saved inventory, saved entities and `minecraft:mob_drops` are compared with
        vanilla (`bare_hand_pig_loot_matches_vanilla`). An engine test checks that only the lethal hit drops
        loot and that one stack reloads after a save during the death animation.
  - [x] Unenchanted swords can strike saved adult pigs in Java Survival. The parity test compares full and
        partial damage, rapid repeats, saved wear and the raw porkchops from a lethal hit using wooden and
        iron swords (`sword_pig_combat_matches_vanilla`).
  - [x] Looting I–III on the main-hand sword increase the raw porkchop count from adult pigs. The exact
        sequence of drops and their pickup are compared with vanilla for each level, an unenchanted sword,
        Looting in the offhand and `minecraft:mob_drops` disabled (`pig_looting_matches_vanilla`).
  - [ ] Loot from other mobs, experience, other weapons and enchantments against mobs, sword sweeps with
        mobs, and combat with other mob types.
- [ ] Vanilla commands on the single Brigadier tree, with operator levels ([docs/commands.md](docs/commands.md)).
  - [x] `list`, `list uuids` and `seed` use one tree for the console and Java clients. The command nodes,
        suggestions, replies and operator level required for `seed` are compared with vanilla (`commands_parity`).
  - [x] `gamemode` changes a connected player's mode from the console or Java. Level 2 permissions, player
        selectors, live abilities and player list updates are compared with vanilla (`commands_parity`).
  - [x] The current and previous game modes persist in vanilla player data across reconnects and restarts.
        Play Login, abilities and the player list reflect the saved mode or `force-gamemode`
        (`commands_parity`).
- [x] Whitelist, player and IP bans and operators from vanilla's `whitelist.json`, `banned-players.json`,
      `banned-ips.json` and `ops.json`: checked at login in vanilla's order, managed by their commands, and
      operators bypassing `max-players` as in vanilla ([docs/operations.md](docs/operations.md)).
  - [x] The four files loaded and written back at startup as vanilla writes them, and logins checked in vanilla's
        order with its messages: player bans, the whitelist that operators skip, IP bans, then `max-players`,
        which operators with `bypassesPlayerLimit` skip. Logins and files are compared with vanilla on the
        network (`access_list_parity`).
  - [x] `whitelist list`, `whitelist on` and `whitelist off` run from the console and Java clients with operator
        level 3. The setting persists across restarts and `enforce-whitelist` disconnects unlisted players
        when the whitelist is enabled (`commands_parity`).
  - [x] `whitelist add`, `whitelist remove` and `whitelist reload` manage `whitelist.json` from the console
        and Java clients. Profile lookups use `usercache.json`, Mojang and the offline UUID fallback.
        Permission-filtered arguments, suggestions, saved entries and enforcement are compared with vanilla
        (`commands_parity`).
  - [x] `ban` and `pardon` manage `banned-players.json` from the console and Java clients. Bans disconnect
        connected targets and survive restarts. Commands, saved entries and new logins are compared with
        vanilla (`commands_parity`, `access_list_parity`).
  - [x] `ban-ip` and `pardon-ip` manage `banned-ips.json` from the console and Java clients. IP bans
        disconnect players at the address and survive restarts. `banlist` shows player and IP bans together
        or by type. The tree, replies, saved entries and login results are compared with vanilla
        (`commands_parity`).
  - [x] `op` and `deop` manage `ops.json` from the console and Java clients. Connected targets receive their
        new operator level and command tree, and commands use the new permissions during the same session.
        Grants, revocations and whitelist enforcement after `deop` are compared with vanilla (`commands_parity`).
- [ ] Datapack loading on top of vanilla data.
  - [ ] Override one vanilla crafting recipe by ID in a world datapack. Compare the output and ingredient
        consumption with vanilla, then disable the pack and verify the original recipe after reload.
- [ ] Nether and End dimensions.
  - [ ] Enter the Nether by portal, place a block, save, restart and return through the portal. Compare
        arrival dimension and position, the block and the return destination with vanilla.
  - [ ] Enter the End by portal, place a block, save and rejoin there. Compare the arrival platform, saved
        block and player position, then leave through an active exit portal and compare the destination.
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
