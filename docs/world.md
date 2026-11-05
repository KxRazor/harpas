# World

Scope: how Harpas stores worlds, where it gets vanilla's game data (world generation, recipes, loot tables,
tags, structures), and how it generates and saves chunks. Parity with vanilla is covered in
[parity.md](parity.md). The tick loop and in-memory storage are covered in [engine-core.md](engine-core.md).

Status: in progress. The [Mojang EULA](#mojang-eula) acceptance and [vanilla game data](#vanilla-game-data)
provisioning are implemented; both happen on every start, since every start needs the vanilla game data.
Region files, chunk sections, `level.dat` and player data are read and written. Existing overworld chunks are
loaded and sent within each player's view, with block states, biomes, heightmaps, saved light and block entity
data checked against vanilla. Changed chunks can be saved asynchronously; generation, live block changes and
saving other world and player data during play remain in progress.
Facts about vanilla cite their source in [References](#references); everything else is a design decision of this
project.

---

## Goals

In priority order. When two goals conflict, the higher one wins.

| # | Goal | Meaning |
|---|---|---|
| 1 | **World compatibility** | A vanilla or Paper world of the supported version opens in Harpas as it is, and a Harpas world opens in vanilla |
| 2 | **Vanilla parity** | The same seed generates the same world; blocks, redstone and entities behave the same |
| 3 | **Performance** | Saving never stalls the tick; generation, loading and lighting run in parallel |

---

## Save format: Anvil

Harpas reads and writes vanilla's region format [1], in the `harpas-anvil` crate. The crate knows nothing of
the engine: it takes and gives NBT, so the engine, its worker threads and the parity tools share it.

- Region files `r.x.z.mca`, each holding 32×32 chunks, built from 4 KiB sectors, with a location table and a
  timestamp table as the first two sectors.
- Three folders of each dimension use region files: `region/` (chunks), `entities/` and `poi/`. Since 26.1
  every dimension, the overworld included, lives in `dimensions/<namespace>/<path>/`, such as
  `dimensions/minecraft/overworld/`, and player data in `players/data/` [7].
- Each chunk's data starts with its length and a compression byte. Harpas writes **zlib**, as vanilla does.
  Other types vanilla supports, including LZ4 since 24w04a, are read [1]. Whether to write LZ4 is decided by a
  benchmark, and only if it wins clearly. Gzip and uncompressed chunks are read today; an LZ4 chunk is
  reported with how to convert the world, until LZ4 reading lands with a test file written by vanilla.
- Chunks larger than 255 sectors are stored in external `c.x.z.mcc` files, as vanilla does [1].
- A chunk's NBT is turned into sections and back (`harpas_anvil::Chunk`): palettes of block and biome names
  become global IDs, and every other field of the chunk and its sections is kept as it was, so a chunk saved
  again differs from the one read only where it changed. As in vanilla, a damaged section is not fatal: an
  unknown block becomes air, an unknown biome plains, and a container that cannot be read its default, each
  with a warning.
- `level.dat` and player data (`players/data/<uuid>.dat`) are gzip NBT files, saved as vanilla saves them
  [7]: the new file is written first (`level.dat_new` for `level.dat`), the current one becomes `<name>_old`,
  and the new one takes its place. Reading falls back to the `_old` backup when the file is missing or
  damaged, so a crash at any point leaves a readable file.
- The rest of the world folder (statistics, advancements, saved data such as maps, raids and scoreboards)
  uses vanilla's files and formats.

### Supported world versions

- Harpas opens worlds of the server version ([versions.md](versions.md#normal-mode-rust-engine)) and its
  hotfixes, currently 26.1, 26.1.1 and 26.1.2 (data versions 4786, 4788 and 4790, each `world_version` in the
  release's server jar). The hotfixes changed nothing in the save format: they fix chat reporting, a UI detail
  and a spectator exploit, and share 26.1's protocol (775) and data pack format (101.1) [6]. They are listed
  one by one, so a later hotfix is accepted only once it has been checked the same way. `level.dat` and every
  chunk record the data version that saved them; a world saved before 1.9 records none and counts as older.
- **Older world:** the server refuses it, and the error explains how to upgrade it: start the vanilla
  server of the supported version once with `--forceUpgrade`. Vanilla upgrades chunks only when it loads them,
  so a world merely opened in vanilla can still hold old chunks; `--forceUpgrade` upgrades every chunk at once.
  Converters for older versions may come later.
- **Newer world,** or any data version not listed: the server refuses it, to avoid damaging data it does not
  understand.
- A chunk keeps the data version it was read with when it is saved again, so a world from 26.1.2 still opens
  in vanilla 26.1.2 as its own.
- `level.dat` is checked before the server starts. Chunks are checked one by one as they are read, since a
  world can mix versions; a chunk of another version stops the running server with an error when it is read.
  It is never treated as a missing chunk to generate again.

### Loading

The server requests existing overworld chunks within each player's effective view distance, prioritizing nearby
chunks and sharing reads between viewers. Workers read only the requested region entries and decode the chunks;
the logic thread receives results in its dimension phase. Each connection encodes and sends bounded batches off
the logic thread. A missing chunk is represented by the temporary void until generation exists; read errors
are logged separately, and an incompatible data version stops the server even if the player left while its
read was in progress. Chunks outside every view are released and other stale results discarded. When the local
block entity registry is available, their positions and types are sent with the client update fields checked
against 26.1: for example, sign text is visible while a chest's saved inventory is not sent in the chunk.
The differential fixture covers all 49 registered block entity types, including moving pistons and examples
with non-default data, for clients with and without text filtering. It does not cover every possible saved
value. If the registry is missing, the server warns and omits block entities until it is generated; later live
block changes are part of gameplay work.

### Saving

- Saves happen when vanilla saves: the periodic autosave every 6,000 ticks [2], when chunks unload, and on
  shutdown.
- At a save point the logic thread shares immutable **snapshots** of changed chunks with a bounded save queue,
  taking the remaining snapshots as space becomes available. A worker serializes, compresses and writes them;
  the tick does not wait for disk. Changes made while a snapshot is being written remain dirty. An unloaded
  chunk stays resident until its latest changes are saved, and shutdown waits for pending saves. A save error
  stops the server and is reported.
- A region and the `.mcc` files of its large chunks are saved as one transaction, so a crash never leaves a
  half-written region or a chunk newer than the region that points to it. Every new file is written whole and
  synced under a staged name (`<name>.tmp`); then a journal, `r.x.z.mca.journal`, listing the files to install
  and the `.mcc` files to remove, is synced and renamed into place, which commits the save; then the staged
  files are renamed over the current ones, stale `.mcc` files removed and the journal removed. The folder is
  synced between these steps, on Unix, so they reach the disk in this order.
- On start, before any chunk is read, the server completes the committed saves it finds and removes the files
  of saves interrupted before their commit, with a warning for each. Opening a region completes its own
  committed save, and reading one chunk of a region with a committed save still pending is an error instead
  of a mix of old and new data. Vanilla ignores the journal, so a world should be started once with Harpas
  after a crash before it is opened with vanilla. Tests interrupt a save before each change it makes to the
  disk and check that the region reads entirely as before or entirely as after.
- A chunk whose region entry cannot be read is reported when it is read, without hiding the region's other
  chunks. A save refuses to overwrite a region with an unreadable entry unless that entry is explicitly
  replaced or removed.

---

## Vanilla game data

World generation, recipes, loot tables, tags, structures and other built-in data are **data files that ship
inside Mojang's game jar**: since 22w42a, for example, world generation data lives under
`data/minecraft/worldgen` [3]. Harpas needs them for parity and does not redistribute them.

They are provisioned like the JVM runtime ([jvm.md](jvm.md#runtime-provisioning)):

- Before downloading anything from Mojang, the owner must have accepted Mojang's EULA (see
  [Mojang EULA](#mojang-eula)).
- On first start, Harpas downloads the official `server.jar` of the server version from Mojang's download
  endpoints and verifies it against the hash in Mojang's version manifest [4]. Each Harpas release also pins
  that hash and size in its binary: a manifest naming a different jar is refused, and a jar provided offline
  is checked as strictly as a download.
- It **extracts the data files** into one zip, `.harpas/vanilla-data/<version>.zip`: the `data/`
  directory and `version.json` of the game jar, which the server jar bundles under `META-INF/versions/` since
  1.18. The zip is written under a temporary name and
  renamed into place, so an interrupted start leaves nothing behind and keeps the data already installed.
- If Java 25 is available on `PATH`, Harpas attempts to run the verified jar's data generator once to obtain its
  block entity type registry [8]. Harpas stores only those IDs in `.harpas/vanilla-data/<version>-block-entity-types.json`;
  the other generated reports, unpacked executable files and downloaded jar are removed afterwards. Neither
  the jar nor the generated data is committed or redistributed.
- Java is not required to start Harpas. If it is unavailable during installation, or for installations made
  before the registry was added, the server warns that block entities are unavailable. Install Java 25 and run
  `harpas vanilla-data generate-registry`; to avoid downloading the jar again, use
  `harpas vanilla-data generate-registry --from <server.jar>`. Both commands check the EULA and the jar hash.

The data stays in a zip, as vanilla reads it from inside its own jar, rather than being unpacked into a folder:

- **It is a datapack like any other.** The zip is laid out like a zipped datapack, and datapacks in
  `world/datapacks/` can be zips, so the vanilla data and the owner's datapacks go through the same loader.
- **Size on disk.** The 26.1 data is about 8,300 small JSON files: 8.6 MB of content that takes about 36 MB
  as a folder, because every file fills at least one file system block, and 5 MB as a zip. The files keep
  Mojang's compression; they are copied without being inflated again. Installing and removing one file is also
  much faster than thousands, on Windows above all.
- **Reading speed does not matter.** The data is read once at startup and parsed into the engine's registries.
- **Nobody edits it.** Vanilla data is changed with datapacks, not by hand, so a folder would offer nothing
  the owner needs.

Projects that ship the data with the server instead, generated from the game by an extractor and committed or
compiled in, redistribute Mojang's data, which Harpas does not ([licensing.md](licensing.md)).
- Without internet access, `harpas vanilla-data install --from <server.jar>` extracts the data from a jar the
  owner provides, with the same verification.
- If the data cannot be provisioned, the server does not start and explains how to fix it.

Datapacks use the same formats and override or extend this data, so datapack support follows from the same
loader.

### Mojang EULA

Mojang's files are subject to the Minecraft EULA, and vanilla's own server does not start until the owner
accepts it (`eula=true` in `eula.txt`). Harpas applies the same rule before it downloads anything from Mojang,
and the owner **accepts it once**:

- On the first start that needs Mojang's files, if the EULA has not been accepted:
  - **Interactive console:** Harpas shows the link to the EULA and asks for acceptance. Accepting writes
    `eula = true` to `harpas.toml`, and the question is never asked again.
  - **Non-interactive start** (containers, hosting panels): the server does not start, and the error shows the
    link to the EULA and the one line to add, either `eula = true` in `harpas.toml` or the `HARPAS_EULA=true`
    environment variable.
- A server folder that already has `eula=true` in a vanilla `eula.txt` counts as accepted, so worlds migrated
  from vanilla or Paper need nothing new.
- One acceptance covers every download from Mojang: the vanilla data here and the Minecraft server in modded
  mode ([mod-support.md](mod-support.md#provisioning)).
- The acceptance stays with the server folder: copying or moving the folder keeps it.

---

## World generation

- **One-to-one port of vanilla's generator**, driven by the extracted data: noise router and density
  functions, noise settings, biome sources, surface rules, aquifers, carvers, features and structures.
  - `harpas-worldgen` loads `worldgen/noise`, `worldgen/density_function`, `worldgen/noise_settings`,
    `worldgen/biome`, `worldgen/multi_noise_biome_source_parameter_list`, `worldgen/world_preset`,
    `worldgen/configured_carver` and block tags from `.harpas/vanilla-data/26.1.zip` (or an extracted datapack
    directory). Configured carvers compile their distributions and recursively resolve replaceable block tags;
    unsupported provider types or debug carving are reported as errors.
  - All 30 density function types of 26.1 are compiled into a deduplicated DAG (`CompiledDensityGraph`) with
    `RandomState` seeding (`XoroshiroRandom::new(world_seed).fork_positional()` or `LegacyRandom` when
    `legacy_random_source` is set) and 32-bit `f32` `CubicSpline` evaluation.
  - `NoiseChunk` evaluates the DAG per chunk with `FlatCache` (`5×5` quart grid at `y = 0`), `Cache2D`,
    `CacheOnce`, `CacheAllInCell` and `NoiseInterpolator` (`4×8×4` trilinear interpolation in Y → X → Z order).
  - `MultiNoiseBiomeSource` queries a 7D `Climate` R-tree (`CHILDREN_PER_NODE = 6` and
    `OverworldBiomeBuilder`'s 7,593 parameter boxes) in vanilla's `x → y → z` section order, and `BiomeManager`
    applies SHA-256 little-endian seed obfuscation and 3D Voronoi zoom.
  - `NoiseBasedChunkGenerator` fills base terrain from `final_density` in `NoiseChunk` cell order, evaluating
    `NoiseBasedAquifer` (`16×12×16` grid with 4-nearest cell lookup, 13-offset preliminary surface sampling,
    deep dark suppression and 3-pair barrier pressure) for `density <= 0.0` and `OreVeinifier` (`vein_toggle`,
    `vein_ridged`, `vein_gap`) for solid blocks and recording fluid positions that need postprocessing. It tracks
    six heightmaps during generation. Their predicates for terrain and carver blocks are compared with vanilla;
    classification of arbitrary gameplay blocks remains provisional until gameplay block updates are implemented.
  - `SurfaceSystem` applies all 4 surface rule types (`block`, `sequence`, `condition`, `bandlands`) and all 11
    condition types (`biome`, `noise_threshold`, `vertical_gradient`, `y_above`, `water`, `temperature`, `steep`,
    `not`, `hole`, `above_preliminary_surface`, `stone_depth` for both `floor` and `ceiling`), plus eroded
    badlands hoodoo pillars, the 192-layer terracotta band table (`minecraft:clay_bands`) and frozen ocean
    icebergs.
  - Two vanilla parity harnesses check this. `vanilla_parity` compares intermediate results (aquifer samples,
    ore veins, surface depth and bands, carver decisions and paths) across several chunks. `chunk_parity` runs
    vanilla's own generator and compares the finished result of 1,621 chunks in 5 seeds, including the far edges
    of the world and all 54 Overworld biomes: every block and biome, the carving mask, fluid postprocessing and
    the six heightmaps. It also compares the 7,593 climate parameter points. Vanilla's chunks have no structures,
    as with `generate-structures=false`, and stop after carvers, so structures' effect on terrain, features and
    light are not compared yet. This does not establish parity for every seed.
  - A climate point can be equally close to several parameter points, and vanilla then keeps the thread's last
    result, which on a real server depends on what that worker generated before. Harpas fills each chunk's biomes
    starting without a last result, and the parity harness makes vanilla do the same, so the comparison is
    repeatable. Such a tie at the start of a chunk may resolve differently in a vanilla world.
  - `StructureTemplates` reads the structure templates under `data/<namespace>/structure/` of the vanilla data,
    which must be at data version 4786 since Harpas has no data fixers: their size, palettes, bounding box and
    jigsaw blocks in each rotation, in vanilla's block order, and the palette vanilla picks for a position. The
    `worldgen/template_pool` definitions are loaded with their weighted elements. The `template_parity` harness
    compares all 1,202 templates in every rotation at two positions, and all 188 pools with a shuffled order,
    with vanilla's own `StructureTemplate` and template pool registry.
  - Jigsaw structures grow from a start pool by connecting templates at their jigsaw blocks, as vanilla's
    `JigsawPlacement` does: highest placement priority first, the expansion hack for villages, and the
    free space a piece may still use, which is a box with the placed pieces taken out. A location with no
    template, which one ancient city pool names, is an empty template as in vanilla. Each piece keeps its
    junctions, which terrain adaptation will need. All jigsaw structures of the
    Overworld are laid out, including trial chambers, which rename pools for each start (`PoolAliasLookup`) and
    start at a random height.
  - The normal Overworld runs cave and canyon carvers from the surrounding 17×17 source chunks, using the
    saved seed, the biome's configured carvers, the same aquifer as the noise fill, and surface restoration.
    Nether caves, debug carvers, non-vanilla data packs and custom Overworld generators are not supported by
    this pipeline. Generation is refused near saved chunks that require blending with older terrain.
  - The generation settings and seed live in `data/minecraft/world_gen_settings.dat`, separately from
    `level.dat`. A new world's seed comes from `level-seed` or a random value and is saved before generation;
    existing settings and unknown fields are preserved. If an existing world lacks generation settings,
    uses another Overworld generator or enables unsupported data packs, its existing chunks can still load but
    missing chunks are not generated. A new world's initial spawn is the surface at the origin, pending the
    vanilla spawn-selection work.
  - `StructureState` loads `worldgen/structure_set`, `worldgen/structure` and biome tags, keeps the structure sets
    that the biome source allows, and decides where a set's structures may start: `random_spread` and
    `concentric_rings` placements, the four frequency reduction methods and exclusion zones. The
    `structure_placement_parity` harness compares this with vanilla's own `ChunkGeneratorStructureState`, loaded
    with real registries and tags, for 5 seeds in 2,048×2,048 chunks at the origin and smaller areas far from it.
    `StructureState::starts_in_chunk` then decides which structures start in a chunk: it picks a set's structure by
    weight, finds the generation point from the terrain height of the noise
    (`NoiseBasedChunkGenerator::base_height`, one noise column as vanilla's `iterateNoiseColumn` reads it), checks
    the biome there and makes the pieces. `references_in_chunk` lists the starts within 8 chunks whose bounding
    boxes reach a chunk. Buried treasure, desert pyramids, jungle temples, swamp huts, igloos, shipwrecks, ocean ruins,
    ruined portals, mineshafts, ocean monuments, strongholds and the jigsaw structures above are made so far; sets with
    other structure types are skipped. Ocean monuments check the full 3D biome volume within 29 blocks of a point
    9 blocks into the start chunk at sea level, against `required_ocean_monument_surrounding`. The biome at the
    generation point on the ocean floor is checked separately against the structure's allowed biomes. Their one
    saved building piece is laid out; its internal rooms are reconstructed as described below. Vanilla
    places shipwrecks wider or taller than 32 blocks by another rule, which none of its templates reaches, so Harpas
    refuses them. The `structure_start_parity`
    harness compares base heights at 1,764 columns, the starts, pieces and references of 4 seeds in 512×512 chunks,
    and the strongholds at all 128 ring positions of each seed, with vanilla's own `createStructures` and
    `createReferences`. It also compares the full saved NBT of up to eight starts of each structure per seed,
    including every piece's placement state, and compares reading those records with vanilla's own loader.
    Jigsaw pieces reconstruct their template bounds on reload, as vanilla does, dropping any extra height reserved
    by the expansion hack. Ring positions and the biome checks of starts search biomes starting without a last
    result, whereas vanilla's depends on its worker threads, so a tie may resolve differently in a vanilla world.
  - `MonumentLayout` builds ocean monuments' 46 grid cells and three special connectors, closes passages
    while retaining access to the entrance, reserves the eight-cell treasure core and fits the remaining
    cells into rooms in vanilla's order. It records every internal piece's bounds and the simple-room and
    wing designs, including wings and the penthouse outside the grid. `from_start` reconstructs this layout
    from the world seed and the single saved building piece, retaining its origin and orientation; internal
    pieces are not added to Anvil's `Children`. `monument_layout_parity` compares 3,237 layouts with vanilla:
    graphs, piece order, all eleven piece types, four orientations, negative and world-limit coordinates,
    designs, the following random draw and reconstruction from saved records (including a missing orientation).
    Block placement and worker integration remain pending; this comparison does not establish block parity.
  - `Beardifier::for_starts` collects the rigid pieces and jigsaw junctions of supplied referenced starts, in
    their supplied order, with vanilla's chunk-distance checks. It supports `none`, `bury`, `beard_thin`,
    `beard_box` and `encapsulate`, including jigsaw ground-level offsets and junctions on terrain-matching pieces.
    Its `24×24×24` float kernel is calculated once from a formula, without storing extracted game data.
    `NoiseChunk` adds this density after noise interpolation and before the aquifer picks a block;
    `generate_chunk_carved_with_beardifier` exposes the complete terrain/surface/carvers pipeline with it.
    `beardifier_parity` checks 4,160,772 density samples bit for bit against vanilla, including chunk boundaries,
    ground-level offsets, all adjustment modes and overlapping structures. `chunk_parity` additionally compares
    terrain around villages, ancient cities, trail ruins and trial chambers, through carvers, in two seeds.
    The engine's workers use a shared cache of up to 4,096 source chunks, including empty starts. A per-entry lock
    shares each calculation without blocking unrelated starts. Saved neighbors are read under the region locks
    used by loading and saving; their starts, including empty ones, take precedence over seed-derived layouts.
    References reach at most eight chunks in each direction. Starts, piece placement metadata and references are
    written in the proto-chunk's `structures` compound. The world's saved `generate_structures` switch disables
    both start discovery and terrain adaptation. `generated_chunk_parity` compares the actual workers' terrain,
    biomes, carving masks, fluid postprocessing, heightmaps and structure records against vanilla around all four
    adapting modes, then saves and reloads the chunks.
  - After carvers, chunk workers place buried treasure: search down from `OCEAN_FLOOR_WG` for its foundation,
    fill exposed neighbors, orient the chest and save its loot table and decoration-seeded loot seed without
    rolling any items. Final piece bounds are saved with the start; shared cached starts stay immutable.
    The treasure's one-block shell fits in its source chunk. `buried_treasure_parity` compares every block,
    heightmaps, chest NBT and saved starts in 38 synthetic cases and four actual chunks per seed in two seeds
    (one seed on pull requests), checks worker saves and Anvil reloads, and has vanilla reload and place the
    treasure again to check resumption.
  - Chunk workers also place swamp hut blocks: average `MOTION_BLOCKING_NO_LEAVES` ground height saved as
    `HPos`, oriented walls and roof corners, support columns through replaceable blocks and fence positions
    marked for postprocessing. Unwritten interior blocks and the saved `Witch`/`Cat` flags stay unchanged;
    spawning those entities belongs to M4. New huts fit inside their source chunk. A saved, height-adjusted
    piece crossing a boundary also places its intersection in neighboring chunks without copying its start.
    `swamp_hut_parity` compares every block, all six heightmaps, saved starts and postprocessing in 92 synthetic
    cases covering all orientations, terrain, supports and four-chunk pieces. It also compares two actual
    worker chunks per seed in two seeds (one seed on pull requests), checks Anvil reloads and a saved neighbor,
    and compares repeated placement after vanilla reloads the records. Below zero, vanilla's negative `HPos`
    sentinel intentionally causes another ground adjustment on each pass; these cases compare both passes.
    The vanilla probe suppresses entity spawning while observing blocks and restores the original flags.
  - Desert pyramid blocks also run on chunk workers: the lowest `MOTION_BLOCKING_NO_LEAVES` height over the
    entire footprint, the random burial offset, foundation columns, rotated sandstone blocks and decorations,
    four chests with decoration-seeded loot, the pressure plate/TNT trap and the archaeology cellar. The cellar
    uses the region's positional random stream for its stairs and collapsed roof; suspicious sand selection is
    sorted and shuffled with a separate positional legacy stream and stores loot seeds from block positions.
    Workers read saved neighbor terrain where available and otherwise generate it through carvers with the same
    structure adaptation. For new pieces, Harpas fixes the shared height as if vanilla processed the source chunk
    first, independent of worker order; already saved heights take precedence. The probe uses that same schedule
    for worker comparisons. A vanilla world that processes another intersecting chunk first may choose a different
    burial offset. Cached starts remain immutable, and neighbor starts are not copied into the target's records.
    `desert_pyramid_parity` compares every block, all six heightmaps, chest/brushable NBT and saved starts in 160
    synthetic chunks across two seeds, including all rotations, slopes, water, negative coordinates, boundary
    chests, saved flags and the bottom of the world, then compares replay after loading Harpas's records in vanilla.
    It also compares eight natural worker chunks across two seeds, their Anvil reloads and pyramid-pass replay,
    and checks a saved source with altered blocks in the neighboring terrain. Pull requests use one seed.
  - Jungle temple blocks run on chunk workers with average `MOTION_BLOCKING_NO_LEAVES` ground height,
    saved `HPos`, randomized cobblestone/mossy cobblestone, rotated stairs and mechanisms, tripwire traps,
    vines and the piston/lever puzzle. Two chests and two dispensers save their loot tables and decoration-seeded
    loot without rolling items. New temples fit in their source chunk; saved pieces can place intersections
    across boundaries, reading saved neighboring terrain when orienting chests. Mechanism ticking belongs to M4.
    `jungle_temple_parity` compares every block, all six heightmaps, block entity NBT, saved starts and
    postprocessing in 208 synthetic chunks across two seeds, then compares their replays after vanilla loads
    Harpas records. Cases cover every rotation and container flag, slopes, water, negative coordinates,
    four-chunk pieces, negative `HPos` resumption and both vertical world limits. Two natural worker chunks
    across two seeds, worker-pass replay, Anvil reloads and a saved neighbor with altered terrain are also
    checked. Pull requests use one seed.
  - Igloo blocks run on chunk workers using the owner's template palettes, blocks and block entity NBT.
    Each piece samples the top's rotated entrance at `WORLD_SURFACE_WG`, temporarily shifts its template
    position, and saves the adjusted bounds while retaining its original template position. Reloading rebuilds
    those original bounds, as vanilla does. The top seals its ladder hole with snow only over non-air,
    non-ladder terrain. Furnace, bed, sign and brewing stand data are retained; the chest marker saves its
    loot table with the second decoration-seeded draw, without rolling items. Template placement updates
    snowy ground before the top's separate snow seal. Template edge connections read
    saved neighboring terrain when present; workers clip writes to their own chunk, including the snow seal
    that vanilla may write outside the target bounds. These comparisons use pre-placement neighboring terrain;
    feature-stage scheduling and general block shape updates remain for later stages.
    `igloo_parity` compares every block, all six heightmaps, saved records and block entity NBT in 126 synthetic
    chunks across two seeds and their replays after vanilla reloads Harpas records. Cases cover all rotations,
    igloos with and without basements, both shaft-depth extremes, slopes, water, negative coordinates, chunk
    boundaries and both vertical world limits. Four natural igloos across two seeds also run through workers,
    Anvil reloads and repeated placement. A saved neighbor with an altered entrance column checks that saved
    terrain controls height. Pull requests use one seed. Entity spawning remains for M4; other structure types
    remain unplaced.
  - Ocean and beached shipwreck blocks run on chunk workers using the owner's 20 templates and their
    palettes. Height sampling uses the unrotated footprint at the template origin, even when the hull turns
    around its pivot. Ocean pieces use the average `OCEAN_FLOOR_WG` height; beached pieces use the lowest
    `WORLD_SURFACE_WG` height, minus half the template height and a bounded burial draw. The adjusted origin
    and `height_adjusted` flag are saved, so a reloaded piece does not choose its height again. A separate
    bounded cache shares adjusted starts for up to 4,096 source chunks. Workers choose the height with
    the source chunk's random stream before placing any intersection, retaining the burial draw in that
    source chunk's loot stream. Neighboring workers reuse the chosen height and skip that draw, as vanilla
    does after its first piece pass. This fixes a source-first schedule; a vanilla world that first places
    another intersecting chunk can choose a different burial offset and loot seeds.
    Placement retains water and fills liquid containers from source water above or beside them, recomputes
    stair, fence and door shapes (including existing blocks beside the template), and assigns chest loot tables without rolling
    items. Workers read saved neighboring terrain before regenerating missing columns, and clip writes to
    the target chunk. Edge shape comparisons use pre-placement neighbors; general block updates and the
    complete feature-stage schedule remain for later stages.
    `shipwreck_parity` compares every block, all six heightmaps, chest NBT, saved records and postprocessing
    in 656 synthetic chunks across two seeds, plus their replays after vanilla reloads Harpas records.
    Cases cover every template and rotation, multiple palettes, slopes, water, negative coordinates, chunk
    boundaries and both vertical world limits. Five natural shipwrecks across two seeds also run through
    workers, reversed worker request order, Anvil reloads and repeated placement, including a chunk shared
    with buried treasure. Two saved
    neighbor cases check altered ocean-floor columns and an already adjusted origin. Pull requests use one
    seed; the treasure overlap is included in the full sample.
  - Cold and warm ocean ruin blocks run on chunk workers using the owner's 48 templates. Each intersecting
    piece samples `OCEAN_FLOOR_WG` at its origin and checks support across its rotated footprint before
    placement. The fitted origin and piece bounds are saved; a repeated pass fits the piece again, as vanilla
    does. Cold ruins overlay their brick, cracked and mossy pieces in order, removing block entities when a
    later layer replaces their blocks.
    Deterioration uses a position-seeded legacy random stream. Archaeology shuffles the surviving blocks of
    the whole piece with a world-seeded positional stream and converts up to five sand or gravel blocks before
    clipping to the target chunk. Their loot tables and seeds are retained without rolling items. Chest
    markers run independently of deterioration, retaining water and using the chunk's structure decoration
    stream for loot seeds. Drowned markers remain deferred to M4.
    `ocean_ruin_parity` compares every block, all six heightmaps, block entity NBT, saved records and
    postprocessing in 2,320 placements and replays across two seeds. It covers all 48 templates and rotations,
    slopes, coasts, ice, waterlogged terrain, negative coordinates, chunk boundaries and vertical world
    limits, including repeated placement after
    vanilla reloads Harpas records. Additional kelp cases compare fitting and metadata, allowing only the
    deferred conversion of neighboring kelp plants to randomized tips; general plant shape updates belong
    to gameplay. Natural cold and warm ruins also run through workers, reversed request order, Anvil reloads
    and repeated placement. Saved-neighbor cases verify fitting against saved terrain and piece records.
    Workers resolve missing neighboring terrain through carvers and prefer saved blocks;
    comparisons use those pre-placement neighbors, not the complete feature-stage schedule.
  - Overworld ruined portal blocks run on chunk workers using the owner's 13 templates. Rotations and
    front/back mirrors preserve each template's pivot. The saved `Properties` and `VerticalPlacement`
    rebuild the positional processors: gold removal, cold/ocean lava replacement, magma substitutions,
    block ageing and mossiness, protected blocks, lava submersion and optional blackstone replacements.
    Waterlogging and template edge shapes are resolved before jigsaw final states and terrain decoration.
    The chunk containing the piece center supplies its decoration stream, including chest loot seeds,
    netherrack spread, capped drip columns, vines and persistent jungle leaves. A center outside the
    world's vertical bounds skips the pass, as vanilla does. Replays also update existing vines whose
    supporting blocks were cleared.
    A portal's owning pass writes the entire piece and surrounding
    terrain, including positions outside the saved piece bounds. Workers prepare this complete sparse
    result against carved or saved terrain and share up to 256 source chunks through a bounded cache,
    applying only their target chunk's writes. The normal structure references retain vanilla's piece
    bounds; spread-only chunks receive blocks without adding a reference to the portal. Saved neighbors
    restore the template, origin, pivot, rotation, mirror and processor properties before placement.
    `ruined_portal_parity` compares every block, all six heightmaps, chest NBT, saved records and
    postprocessing across the 13 templates, all rotations and both mirrors, water and lava, cold and
    overgrown properties, slopes, burial, negative coordinates, chunk boundaries and vertical limits.
    Representative small and giant templates are repeated after vanilla reloads Harpas records. Natural
    portals across two seeds also run through workers, reversed requests, Anvil reloads and pass replays;
    saved-neighbor cases read altered water, lava and protected blocks. Pull requests use one seed.
    These comparisons use the same pre-placement neighbors for each complete portal pass; they do not
    establish the full feature-stage schedule or resumption. Ruined portals in the Nether follow with
    that dimension.
  - Normal and badlands mineshaft blocks run on chunk workers, retaining the saved piece layout, entrances,
    orientation, depth and reference count. Rooms carve their upper dome; crossings include upper floors
    and supported posts; stairs excavate their rotated slope. Corridors place oak or dark oak supports,
    floating floors, hanging fences/chains, rails, torches and cobwebs. Pieces check the liquid shell clipped
    to the target chunk and the zoomed `mineshaft_blocking` biome at its clipped center. Biome samples use
    saved neighboring sections when available, otherwise the noise biome source, without generating extra
    terrain. Floor support and solid-render checks differ for slabs, stairs, leaves and glass.
    Each structure type shares one decoration-seeded stream across all its starts, in a new chunk's native
    reference-set iteration order. Buried treasure runs before the mineshafts. Spider corridors write cave-spider
    spawner NBT; workers retain incoming `hps` flags at `carvers` so a later vanilla pass can recreate a
    spawner which its excavation clears. Chest minecart entities and loot remain with M4; their placement
    draws and rail blocks are already preserved. The light engine and spawner ticking are separate tasks.
    `mineshaft_parity` compares every block, all six heightmaps, postprocessing, saved records and spawner
    data in 558 synthetic chunks and their replays, including both types, all rotations, multiple references
    and reference-set growth, partial floors, liquids, blocked biomes, chunk edges and vertical limits.
    Natural worker chunks across three seeds include a chunk shared with buried treasure, reversed requests
    and Anvil reloads. A saved-neighbor case checks piece records and a changed biome across a chunk edge.
    These are individual placement-pass comparisons; the full FEATURES schedule and resumption remain pending.
  - Stronghold blocks run on chunk workers using the existing saved piece layout, in piece order. Corridors,
    prisons, three- and five-way crossings, both stair types, filler corridors and short/tall libraries retain
    their doors, child openings and room variants. Masonry draws only on box boundaries; boxes which skip air
    also skip draws outside the target chunk, while portal-room masonry and library cobweb draws include
    clipped positions. All pieces of a start share the stronghold decoration stream (index 19, step 4 in 26.1).
    Chests retain loot table names and seeds without rolling items. Chest facing excludes transparent and
    partial blocks; workers resolve only the neighboring positions needed for chests on a chunk edge,
    preferring saved terrain. Portal rooms place lava, stairs, bars and all twelve independently drawn eyes,
    including an active portal when every frame has an eye, and save silverfish spawner data. Spawner ticking
    follows in M4. Workers retain incoming `Chest` and `Mob` flags at `carvers` so vanilla can recreate containers
    which its next structure pass clears; the direct placement API updates them as vanilla does.
    `stronghold_parity` compares blocks, six final-block heightmaps, block entities, postprocessing and saved
    records in 5,472 placements and replays across two seeds: every piece type and rotation, all door types,
    crossing/library variants, air, liquids, caves, chunk boundaries and vertical limits, including individual
    pass replays after loading the saved records. Eighteen natural worker chunks across two seeds cover
    libraries, portal rooms, chests and crossings, reversed
    requests and Anvil reloads. A saved-neighbor test changes a chest's neighboring block to glass and checks
    the retained source record and target chunk against vanilla. Complete FEATURES scheduling and resumption
    remain separate work.
- **Deterministic:** the same seed and data produce the same chunks, using the vanilla-exact random generators
  of [engine-core.md](engine-core.md#7-determinism).
- **Parallel:** missing chunks are generated on the bounded worker pool and saved on unload or shutdown with
  status `minecraft:carvers`, so vanilla can continue the later stages. Buried treasure placement can be repeated
  by vanilla without replacing its chest or loot metadata in the compared cases. Swamp hut blocks and their
  saved height can also be resumed; their witch and cat are left for vanilla or M4. Pyramid workers retain the
  incoming chest flags while saving at `carvers`: vanilla clears the alcoves when it repeats pyramid placement,
  so it must be allowed to recreate the chests with their original decoration seeds. The direct placement API
  still updates the flags as vanilla does. Jungle temple workers likewise retain all four incoming chest and
  dispenser flags: a repeated temple pass clears those positions and must recreate the same loot containers.
  Igloo pass replays also match vanilla, including its height changing when the existing roof raises the
  entrance's `WORLD_SURFACE_WG` height. Shipwreck replays retain their saved height. A beached replay skips
  the original burial draw, so its chest loot seeds can change, as in vanilla. These comparisons do not
  establish a complete FEATURES-stage round trip. Ocean ruin pass replays likewise refit their saved pieces
  to the existing terrain; blocks and loot can change when earlier placement changed that terrain.
  Ruined portal passes likewise replay against existing blocks, and generated chunks continue to save at
  `carvers`. Full feature-stage resumption, other structure blocks and lighting
  remain separate roadmap tasks; generated chunks do not yet include them.
- **Lighting:** a port of vanilla's light engine, run on the worker pool, with results applied at defined
  points ([engine-core.md](engine-core.md#2-parallel-only-where-it-cannot-be-observed)).
- **Pregeneration:** a command to generate an area ahead of time, using the same pipeline
  ([qol.md](qol.md#world-pregeneration)).

---

## Anti-xray

Built in, with the same three modes as Paper's anti-xray [5], **disabled by default** and configured per world
in `harpas.toml`:

| Mode | What it does |
|---|---|
| 1 | Hidden ores are sent as the surrounding filler block (stone, deepslate, netherrack, end stone); the cheapest |
| 2 | Hidden and filler blocks are replaced with random hidden blocks, so fake ores hide real ones |
| 3 | Like mode 2, randomized per chunk layer instead of per block, which lowers join traffic and compresses better |

- Obfuscation is applied when a chunk section is encoded, per chunk rather than per player, so it keeps the
  shared encoding of [engine-core.md](engine-core.md#6-chunks) and reaches Java and Bedrock players alike.
- Hidden blocks are revealed when they become exposed, as on Paper.
- Players with the `paper.antixray.bypass` permission see real blocks when the permission option is enabled, as
  on Paper; those players get their own encoding of affected sections.
- Its random choices never use the game's random sequences ([engine-core.md](engine-core.md#7-determinism)).

---

## Parity strategy

World generation and saved worlds are checked against Mojang's server like every other subsystem:
[parity.md](parity.md) describes how, and how far each one has come.

---

## Open questions

1. Whether to write LZ4 instead of zlib, pending the benchmark.
2. Converters for worlds from older versions: if and when.

---

## Validation

1. **Round trip:** a world saved by vanilla, opened and saved by Harpas, then reopened by vanilla, with no
   differences.
2. **Generation parity:** for a set of seeds, every generated block and biome matches vanilla.
3. **Save stalls:** tick time during autosave, with many changed chunks, compared with a tick without saving.
4. **Generation throughput:** chunks per second as worker threads are added.
5. **Compression:** save size and time with zlib vs. LZ4.

---

## References

1. minecraft.wiki, "Region file format" (32×32 chunks, 4 KiB sectors, header tables, compression types including
   LZ4 since 24w04a and custom since 24w05a, `.mcc` files, `region`/`entities`/`poi` folders). https://minecraft.wiki/w/Region_file_format
2. minecraft.wiki, "Tick" (autosave every 6,000 ticks). https://minecraft.wiki/w/Tick
3. minecraft.wiki, "Custom world generation" (world generation data in `data/minecraft/worldgen` since 22w42a). https://minecraft.wiki/w/Custom_world_generation
4. Mojang version manifest, with each version's server download and hash. https://piston-meta.mojang.com/mc/game/version_manifest_v2.json
5. PaperMC documentation, "Anti-Xray" (engine modes 1, 2 and 3; bypass permission). https://docs.papermc.io/paper/anti-xray/
6. minecraft.wiki, "Java Edition 26.1.1" and "Java Edition 26.1.2" (hotfixes for chat reporting, UI tooltips and
   spectators attacking players). https://minecraft.wiki/w/Java_Edition_26.1.1,
   https://minecraft.wiki/w/Java_Edition_26.1.2
7. minecraft.wiki, "Java Edition level format" (26.1 world folder layout, `level.dat`, `level.dat_new` and
   `level.dat_old`). https://minecraft.wiki/w/Java_Edition_level_format
8. minecraft.wiki, "Data Generators" (server data generator and the registry report).
   https://minecraft.wiki/w/Minecraft_Wiki:Projects/wiki.vg_merge/Data_Generators
