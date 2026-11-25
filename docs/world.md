# World

Scope: how Harpas stores worlds, where it gets vanilla's game data (world generation, recipes, loot tables,
tags, structures), and how it generates and saves chunks. Parity with vanilla is covered in
[parity.md](parity.md). The tick loop and in-memory storage are covered in [engine-core.md](engine-core.md).

Status: in progress. The [Mojang EULA](#mojang-eula) acceptance and [vanilla game data](#vanilla-game-data)
provisioning are implemented; both happen on every start, since every start needs the vanilla game data.
Region files, chunk sections, `level.dat` and player data are read and written. Existing overworld chunks are
loaded and sent within each player's view, with block states, biomes, heightmaps, saved light and block entity
data checked against vanilla. Generated chunks receive sky and block light from the light engine when they are
sent, and that light is compared with vanilla's own saved chunks. Changed chunks can be saved asynchronously;
feature definitions are loaded from the datapack, and ores, simple vegetation and the first trees are placed and
compared with vanilla, but generation through the complete feature stage, live block changes and saving other
world and player data during play remain in progress. The server persists player inventories, game modes, health and
dropped items and saved adult pigs. See [Player inventories](#player-inventories) and [Entity regions](#entity-regions).
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
- Map images live in `data/minecraft/maps/<id>.dat`, with `last_id.dat` recording the last allocated ID.
  A worker saves the treasure map before the chest can give out an item with that ID.
- The rest of the world folder (statistics, advancements, saved data such as raids and scoreboards)
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
the logic thread receives results in its dimension phase. Connections send bounded batches off the logic thread,
sharing encoded and compressed Java columns through the server's [encoding cache](engine-core.md#6-chunks).
A missing chunk is represented by the temporary void until generation exists; read errors
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
- A save writes only the chunks that changed, as vanilla does, without reading or rewriting the rest of
  their region. Each chunk goes into sectors that no chunk uses, never over the chunk it replaces, and the
  region's header then points to it; the sectors of the replaced chunk are free for a later save, so a file
  is at most one copy of the saved chunks larger than vanilla would leave it. Saving one chunk of a full
  12 MiB region takes 7 ms on a disk, where rewriting the region took 160 ms.
- Every save of a region ends with several syncs, so the chunks of a region are written together. A changed
  chunk that leaves every view waits up to five seconds for the others of its region, and a region has one
  save queued or in progress at a time: the changes made meanwhile share the next one. Autosave and shutdown
  do not wait.
- The chunks of a region and the `.mcc` files of its large chunks are saved as one transaction, so a crash
  never leaves some chunks of a save without the others, or a chunk newer than the region that points to it.
  The chunks are written into free sectors and synced, which changes nothing a reader sees; the region's new
  header and every new `.mcc` file are written whole and synced under staged names (`r.x.z.mca.header.tmp`,
  `<name>.tmp`); then a journal, `r.x.z.mca.journal`, listing the header to write, the files to install and
  the `.mcc` files to remove, is synced and renamed into place, which commits the save; then the header is
  written over the region's and synced, the staged files are renamed over the current ones, stale `.mcc`
  files removed and the journal removed. The folder is synced between these steps, on Unix, so they reach the
  disk in this order. Tools that save a whole region (`harpas_anvil::Region`) stage the region file instead
  of its header, through the same journal.
- On start, before any chunk is read, the server completes the committed saves it finds and removes the files
  of saves interrupted before their commit, with a warning for each. Opening a region completes its own
  committed save, and reading one chunk of a region with a committed save still pending is an error instead
  of a mix of old and new data. Vanilla ignores the journal, so a world should be started once with Harpas
  after a crash before it is opened with vanilla. Tests interrupt a save before each change it makes to the
  disk and check that the region reads entirely as before or entirely as after.
- A chunk whose region entry cannot be read is reported when it is read, without hiding the region's other
  chunks. A save of other chunks leaves the entry as it is, to be repaired. The exception is an entry that
  points past the end of the file, where the next chunk written would be read as that one: the save is
  refused until the entry's own chunk is saved. Saving a whole region with an unreadable entry is refused
  unless that entry is explicitly replaced or removed.
- A world may be a download, so its files are read within limits that vanilla does not have: a chunk,
  `level.dat` or a player's data may inflate to at most 256 MiB and take at most 1 GiB of memory as NBT, and
  a `.mcc` file may hold at most 256 MiB. A file over a limit is reported as unreadable.

### Player inventories

The engine loads `players/data/<uuid>.dat` on a dedicated worker before sending Play Login or the player's
first inventory, and before accepting inventory changes, item use, breaking or pickups. A missing document
starts a new inventory. Unreadable documents and unsupported items cause the server to refuse the connection
and preserve the saved data. The existing `_old` recovery applies to damaged or missing compressed files.

The main inventory and hotbar use the `Inventory` list and its byte `Slot` numbers, 0 to 35. Armor and the
offhand use the `equipment` compound [9], and `SelectedItemSlot` keeps the selected hotbar slot. Unrelated
player fields, including position and experience, unmodeled slots and equipment, and unknown fields on
supported item records are retained. The current and previous modes use `playerGameType` and
`previousPlayerGameType`. Play Login, the player list and the starting abilities use the resolved mode.
The first health packet uses the saved `Health`, `foodLevel` and `foodSaturationLevel`. The food tick also
resumes with `foodExhaustionLevel` and `foodTickTimer`. Missing values use zero for exhaustion and the timer.
Hunger and Poison resume from their remaining durations in `active_effects`. Other saved effects are retained in the
player document.

Autosave every 6,000 ticks, departure and shutdown take immutable player snapshots on the logic thread.
The worker encodes NBT, compresses and writes them through the Anvil backup writer. Its job queue is bounded
at 64; pending saves coalesce by UUID, and there is one outstanding write per UUID. A rapid reconnect uses
the newest pending snapshot, including when its older save is still being written. New snapshots remain
pending until their own write completes. Before the final shutdown snapshot, live menus are closed so their
cursor and ingredients return to persistent inventory slots, dropping only overflow. A save error stops the
server and is reported; shutdown drains the
player saves even when the chunk flush fails. A saturated player-data queue refuses additional loads rather
than blocking the logic thread.

Saved NBT and network bytes have different component codecs. `harpas-engine::item_data` converts 27 values:
custom data, maximum stack size, maximum damage, damage, unbreakable, custom name, minimum attack charge,
item name, item model, lore, rarity, repair cost, glint override, intangible projectile, glider, tooltip style,
dyed color, map color, map ID, potion contents without custom effects, potion duration scale, bucket entity
data, ominous bottle amplifier, note block sound, enchantability, enchantments and stored enchantments. Item
component and potion IDs come from the owner's generated built-in registries. Built-in Java 26.1 IDs also
cover the components needed to save worn pickaxes. Enchantment IDs use the synchronized registry sent to
clients. Removed components are preserved as removed.

The remaining component values are explicit errors. An existing inventory containing one cannot be loaded;
a Creative stack that cannot be saved is rejected before it changes the inventory. This prevents stripping
components or overwriting the original document while their codecs are still unimplemented. Run
`harpas vanilla-data generate-registry` with Java 25 to enable the other implemented component codecs.

The cursor, crafting result and crafting input grids are transient menu slots and are not written as
persistent slots. Ordinary departure drops the cursor and input items, as the menu removal rules do; shutdown
first closes live menus as described above.

`player_inventory_parity` creates inventory/equipment NBT with vanilla's own codecs, compares the network
bytes of the supported components, has Harpas write the document and reopens its inventory and equipment in
vanilla. Lifecycle tests cover reconnects, restart with an online player, backup recovery, refused loads,
non-blocking I/O, newer snapshots racing older saves, queue saturation and save errors.

### Entity regions

The Overworld uses the `entities/` region files for item entities and adult pigs. Workers read entity data
with the chunk and save immutable snapshots on autosave, unload and shutdown. An item snapshot includes the
stack and supported components, UUID, position, velocity, ground state, health, age and pickup delay.
A loaded item receives a new runtime network ID. New items use UUIDs generated from a fresh seed when the world
opens. Removing or moving an item also updates its former chunk, so it stays gone when that chunk reloads.

Items and pigs in chunks outside the simulation set pause. A drop made before its chunk arrives waits for
the saved entities to load before it can be saved. The logic thread keeps the chunk in memory while its saves are
outstanding. Terrain and entity revisions share the worker queue but are saved only when their respective
data changes. Entity regions use the same journal recovery as terrain regions. A move across region files
can span two transactions if the process is interrupted.

An adult pig retains its UUID, position, motion, ground state, fall distance, rotation, health, variant and
other saved fields. Its movement updates both affected chunks when it crosses a boundary. Baby pigs and
other mobs stay inactive and keep their original NBT, as do items with unported components.
`item_entity_parity` reads the updated Harpas region back through vanilla's codecs.
`saved_adult_pig_matches_vanilla` compares the spawn and metadata of a pig suspended with `NoAI` before
and after vanilla reopens the region Harpas saved.

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
  registry and item component reports [8]. Harpas stores the block entity type IDs in
  `.harpas/vanilla-data/<version>-block-entity-types.json`, and the entries of every built-in registry, in the
  order of their numeric IDs, in `.harpas/vanilla-data/<version>-registries.json`: the tags sent to Java clients
  need the IDs of blocks, items, fluids and other built-in registries
  ([java-connection.md](java-connection.md#tags)). The registry file also stores `consumable` defaults and,
  when present, `food` and `use_remainder` for supported foods and drinks. It also stores the modifiers and
  durability of swords and armor. The generator removes the other reports, unpacked executable files and downloaded
  jar afterwards. Neither the jar nor the generated data is committed or redistributed.
- Java is not required to start Harpas. If it is unavailable during installation, or for installations made
  before the data was added, the server warns about missing block entity IDs, built-in registries or consumable
  defaults. Apples remain available as a fallback until the consumable defaults are generated. Older registry
  files still provide the defaults they already include. To add the sword, armor or newer consumable defaults,
  install Java 25 and run `harpas vanilla-data generate-registry`. To reuse a
  downloaded jar, run `harpas vanilla-data generate-registry --from <server.jar>`. Both commands check the EULA
  and the jar hash.

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
  - Processor lists load from `worldgen/processor_list`, retaining processor order and unported JSON.
    `harpas_worldgen::processor::RuleProcessor` compiles per-block `rule` substitutions: block, exact-state,
    tag and always-true predicates, random block and exact-state predicates, and always-true, Manhattan-distance
    and axis-distance position tests. Rules read the current template state and existing terrain state,
    use a fresh legacy random source seeded by the transformed block position and stop at the first match.
    NBT modifiers preserve data (`passthrough`), produce an empty compound (`clear`) or preserve data while
    replacing the loot table and drawing its seed (`append_loot`). `rule_processor_parity` compares all 39
    shipped rule configurations, including delegates of capped processors, and synthetic cases with vanilla's
    own processor, checking output states, positions and complete NBT. Traced copies append loot so the seed
    also exposes short-circuited random consumption. Unsupported rules do not compile as partial processors.
    `append_static` recursively merges the configured `data` compound, retaining unrelated nested keys
    and replacing lists and values of different types. JSON booleans become bytes, numbers use the narrowest
    exact NBT type, and mixed lists use vanilla's compound wrappers on disk. `process` works on a snapshot;
    `process_in_place` also retains vanilla's mutation of existing NBT. A merge into existing NBT alone
    consequently spends no capped limit; creating a tag for a block without NBT does. The compiled data is
    independent of each processed block. `static_processor_parity` compares ordered processing passes with
    vanilla, including empty and nested compounds, numeric boundaries, homogeneous and mixed lists, missing
    and existing NBT, capped state changes, clear/loot/static chains, filters and settings random consumption.
    `CappedProcessor` compiles `capped` processors with rule delegates and positive integer limit providers.
    Finalization shuffles the whole template's surviving blocks using a positional legacy source forked
    from the world seed at the template origin. Only changed states or NBT consume the sampled limit;
    unchanged results leave room for later candidates. Each finalizer sees earlier finalizers' results,
    after all per-block rules, and runs before placement clips blocks to a chunk. The limit provider's
    declared zero maximum skips finalization, including clamped sources below their clamp interval, as
    in vanilla. `capped_processor_parity` compares the four shipped capped processors, their three complete
    processor lists and synthetic chains with vanilla's `StructureTemplate.processBlockInfos`: block order,
    states, positions and full NBT, across world seeds, origins, list sizes and integer providers. Placement
    settings' randomness is checked to remain untouched. Other delegates and jigsaw block placement on
    workers remain unported.
    `ProcessorPipeline` combines these processors with `block_rot`, `protected_blocks`, `block_ignore` and
    `nop`. Rot membership reads the original template state even after earlier rules change it; its draw
    uses the shared placement-settings source when supplied, or a fresh positional legacy source otherwise.
    Protected blocks check existing terrain, while ignore checks the current template block type across
    all its states. A rejected block stops the remaining per-block processors; capped finalizers then run
    over the surviving blocks. Surviving positions and NBT are preserved. All 40 vanilla processor lists
    now compile without dropping entries. `filter_processor_parity` compares those lists, all 13 shipped
    filters in isolation and synthetic chains against `StructureTemplate.processBlockInfos`, including
    settings random consumption, float thresholds, empty lists, world limits and every registry state in
    the full sample.
    Runtime-added `gravity` processors read any of the six heightmaps through `ProcessorWorld`, retaining
    the original template-local Y even when earlier processors have already moved the block. A live level
    substitutes `WORLD_SURFACE` and `OCEAN_FLOOR` for their worldgen variants; generation regions keep the
    requested kind. Subsequent rules, filters, positional random sources and capped finalizers read the
    adjusted position. Gravity preserves state and NBT and consumes no random draws. `process_in_world`
    provides these terrain reads, while `process` remains the block-read-only entry point for lists that
    need no heightmaps. `gravity_processor_parity` compares complete processing passes and terrain-read
    traces with vanilla, including chained gravity, rotations, offsets and integer overflow, world limits,
    missing and existing NBT, and explicit versus positional settings randomness.
    Runtime-added material processors also run in this pipeline. `block_age` uses the owner's stairs,
    slabs and walls tags and the shared placement-settings random source when provided, or a fresh
    positional legacy source otherwise. Mossy stairs, slabs and walls retain their incoming properties
    when keeping that shape; conversions to other shapes use their defaults. `blackstone_replace` retains
    only facing, half and slab type, resetting wall connections, stair shape and waterlogging before placement.
    `lava_submerged_block` keeps source lava where a partial outline would replace any level of lava,
    preserving the incoming NBT. Its shape query uses outlines rather than collision shapes, including
    full outlines of plants and attachment blocks without faces. `ProcessorWorld::is_shape_full_block`
    defaults to generation shapes without active block entities; worlds with dynamic shapes can override
    the query. `material_processor_parity` compares 56 pipelines in 8,928 complete passes with vanilla,
    including every registry state as incoming blocks and lava's terrain in the full sample, mossiness
    outside the unit interval, property preservation, NBT, shared and positional random sources, ordered
    filters, rules and capped finalizers. `gravity_processor_parity` also checks material processors before
    and after movement.
    Runtime-added `jigsaw_replacement` reads the current block and NBT after earlier processors.
    Missing NBT leaves the jigsaw intact; missing or non-string `final_state` replaces it with air.
    Valid replacements clear NBT, including when the replacement is another jigsaw. Invalid command
    states and `structure_void` replacements remove the block and stop later per-block processing.
    Other incoming blocks, including existing structure voids, pass through unchanged. Names and
    properties use strict command syntax, with omitted properties keeping their defaults, quoted
    property names/values, duplicate rejection and vanilla's unread trailing input. Attached SNBT is
    validated but discarded: compounds, mixed lists, typed arrays, numeric ranges, `bool`/`uuid`
    operations and string escapes. Unicode 16 names use `unicode_names2` with Java 25's control and
    algorithmic range names. The parser follows [Brigadier's string syntax](https://github.com/Mojang/brigadier)
    and Mojang's [SNBT changes](https://www.minecraft.net/en-us/article/minecraft-java-edition-1-21-5#snbt-changes),
    checked through the owner's server rather than its implementation. `jigsaw_processor_parity`
    compares every registry state, shipped template final states, malformed command strings and
    attached SNBT, missing and existing NBT, ordered rules/filters/capped finalizers and settings random
    tails. Replacement itself reads no terrain and consumes no randomness.
    Template block states turn and reflect as vanilla's `BlockState.rotate` and `BlockState.mirror` do:
    facings, orientations, rail ends and connection faces turn, `axis` swaps on quarter turns and the
    16 sign, banner and skull steps turn by four per quarter. Vanilla's exceptions are kept: fire and
    chorus plants keep their faces, anvils keep their facing in a mirror, chests keep their side, and a
    reflected stair keeps its inner corners and keeps its outer corners unless it faces along X.
    `block_transform_parity` compares every registry state with each rotation and both mirrors.
  - Single and legacy single pool elements place their template in one chunk as vanilla's
    `SinglePoolElement.place` does (`SingleElementPlacement`). The whole template is processed first: the
    element ignores structure blocks, applies jigsaw replacement, then its processor list and, for terrain
    matching, gravity at `WORLD_SURFACE_WG` one block down. A legacy element ignores structure blocks and air
    after everything else, so blocks its processors turn into air are not placed. The surviving blocks inside
    the chunk turn with the piece. Existing water waterlogs blocks with a `waterlogged` property other than
    double slabs. Placed blocks then repeatedly take water from source water above them, or beside them
    through a side that is not full, and water puts out lit candles and campfires. Block entities keep the
    template's data, with an id added for processor-made data such as suspicious gravel loot; chests,
    barrels, shulker boxes, dispensers, droppers, hoppers, crafters and decorated pots draw a loot seed from
    the placement random, kept only with a loot table, while other block entities keep their processor
    seeds. As in vanilla, the shapes at the template's edges stay as placed: neither its blocks nor their
    neighbors update, which a terrain of fences confirms.
  - A list element places each of its elements in order at the same spot; a later block entity replaces an
    earlier one at the same position, and a block without one removes it. A feature element places its placed
    feature at the piece's position (`place_placed`), with its placement modifiers, drawing from the piece's
    placement random and writing into the neighboring chunks of the generation region, as trees and flower
    patches of villages do. `pool_element_parity` places every single, legacy single and list element of
    every pool, turned and crossing a chunk edge, over dry, flooded, deep and fence layered terrain, with and
    without waterlogging, and every feature element on grass with four seeds, comparing the blocks of the
    chunk and its neighbors, the block entities as vanilla loads and saves them, and the random tail. The
    ancient city's `sculk_patch` elements are compared too, without the block entity placeholders that only a
    generation region saves.
  - Jigsaw structures grow from a start pool by connecting templates at their jigsaw blocks, as vanilla's
    `JigsawPlacement` does: highest placement priority first, the expansion hack for villages, and the
    free space a piece may still use, which is a box with the placed pieces taken out. A location with no
    template, which one ancient city pool names, is an empty template as in vanilla. Each piece keeps its
    junctions, which terrain adaptation will need. All jigsaw structures of the
    Overworld are laid out, including trial chambers, which rename pools for each start (`PoolAliasLookup`) and
    start at a random height.
  - Jigsaw structure blocks are placed in each chunk's decoration, in the step of their structure
    (`StructureStart::place_jigsaw_in_region`): trail ruins and trial chambers in `underground_structures`,
    villages and pillager outposts in `surface_structures` and ancient cities in `underground_decoration`.
    Every start whose bounds reach the chunk takes part, wherever it starts. The pieces that reach the chunk
    place their pool elements in order with the step's random source, seeded for the structure as described
    below; the processors' reference position is the center of the first piece's columns at its lowest Y.
    Template blocks are clipped to the chunk, and terrain matching reads the chunk's heights as blocks are
    placed. Templates read terrain across their whole extent, as vanilla's do: the region's chunks, and air
    with heights at the world's bottom farther away, as in a region whose farther chunks have no terrain
    yet. Feature elements write into the whole region, as other features do, including the ancient city's
    sculk patches. `jigsaw_chunk_parity` places real starts of all five villages,
    a pillager outpost, trail ruins, trial chambers and an ancient city in the decoration regions of their
    carved chunks with terrain adaptation, and compares 384 chunks block by block, the blocks written to
    their neighbors, and their block entities and loot seeds against vanilla's `StructureStart.placeInChunk`,
    including the ancient city chunks its sculk patches reach. `feature_worker_parity` checks that the
    workers place a village chunk away from its start, and nothing of it without structures. The other
    structures are still placed after carvers.
  - The normal Overworld runs cave and canyon carvers from the surrounding 17×17 source chunks, using the
    saved seed, the biome's configured carvers, the same aquifer as the noise fill, and surface restoration.
    Nether caves, debug carvers, non-vanilla data packs and custom Overworld generators are not supported by
    this pipeline. Generation is refused near saved chunks that require blending with older terrain.
  - Feature data is loaded from `worldgen/configured_feature/<name>.json`, `worldgen/placed_feature/<name>.json`
    and each biome's `features` array, which keeps one list per generation step. `minecraft:ore` and
    `minecraft:scattered_ore` configurations are parsed with their size, discard chance, rule tests and target
    states, and the placement modifiers those features use (`count`, `in_square`, `height_range` and `biome`) are
    parsed too. Other feature and modifier types keep their JSON for later families, so the complete 26.1 pack
    loads.
  - Every chunk decorates itself once, as vanilla's `applyBiomeDecoration` does: the FEATURES stage needs the
    chunks within one chunk at `carvers` and writes only into them, so there is no wider source loop as there is
    for carvers. A chunk's final blocks therefore also depend on the passes of its eight neighbors.
    `FeatureIndex::overworld` merges the biomes' ordered lists into one order for each generation step, taking
    the biomes in the biome source's order, with a depth-first topological sort (vanilla's `FeatureSorter`). A
    chunk runs, for each step, the features listed by any biome stored in the 3x3 chunks around it, in that
    order; each is seeded with `set_decoration_seed(world_seed, chunk_x * 16, chunk_z * 16)` and
    `set_feature_seed(decoration, index, step)`, where `index` is its position in the step's global order, not
    in a biome's list. When the world generates structures, each step first places its structures that reach the
    chunk, in registry order, each with `set_feature_seed(decoration, index, step)` where `index` is the
    structure's position among the step's structures (jigsaw structures so far).
    Placement modifiers run depth first, each position through the rest of the list and into the feature before
    the next comes out, as vanilla's lazy streams do; only `count_on_every_layer` collects all its positions
    first, as vanilla's does. All 15 modifier types are ported, with the integer providers `constant`,
    `uniform`, `biased_to_bottom`, `clamped`, `clamped_normal`, `trapezoid` and `weighted_list`, and the block
    predicates `matching_blocks`, `matching_block_tag`, `matching_fluids`, `inside_world_bounds`, `all_of`,
    `any_of`, `not` and `true`. Fluids are read from block states: liquid blocks hold a source at level 0,
    bubble columns, kelp and seagrass hold water, as does any `waterlogged=true` state. `solid`,
    `replaceable` and `has_sturdy_face` read flags from `harpas_core::block_behavior`, which derives vanilla's
    legacy solid flag, motion blocking and full faces from the collision shapes of minecraft-data, with the
    blocks whose properties force the solid flag or whose support shape differs from their collision shape
    listed by hand; `block_behavior_parity` compares all 29,873 states with vanilla's. `would_survive` runs the
    survival rule of saplings, the firefly bush, the mangrove propagule, sugar cane and cactus, which read
    `supports_*` block and fluid tags; the datapack loader now reads fluid tags too. A placed feature whose
    `would_survive` names another block is skipped, which changes no other feature's seed; no 26.1 placed
    feature does.
    `placement_parity` runs every Overworld placed feature's modifiers with vanilla's own on the same carved
    terrain and random seeds and compares every position. `count_on_every_layer`, `biased_to_bottom` heights
    and `fixed_placement` are only used in the Nether and the End, so unit tests cover them until those
    dimensions are compared. Once a chunk has run carvers, vanilla updates only its four final heightmaps when
    a block changes and leaves `WORLD_SURFACE_WG` and `OCEAN_FLOOR_WG` as carvers left them, so features and
    structures that read them see the carved terrain; `GeneratedChunk::freeze_worldgen_heightmaps` does the
    same for generated chunks and resumed proto-chunks. The heightmaps classify blocks with
    `block_behavior`'s motion-blocking flag and fluids, as vanilla's do. `minecraft:ore` is placed with
    vanilla's vein geometry: spheres inside other spheres are dropped, the visited set is bounded only on the lower side,
    and with a discard chance a block is skipped when any of its six neighbors is air. Scattered ore (ancient
    debris) is parsed but waits for the Nether. Height providers `constant`, `uniform`, `trapezoid`,
    `biased_to_bottom`, `very_biased_to_bottom` and `weighted_list` are ported. `RegionFeatureWorld` routes reads and writes across the nine chunks and zooms their stored biomes
    with the world seed's `BiomeManager`. The structures of each step and the other families follow.
    Besides ores, `harpas_worldgen::configured` places `simple_block`, `random_selector`,
    `simple_random_selector`, `random_boolean_selector`, `block_column` and `disk`, with the simple, weighted,
    rotated, randomized-int, noise, noise-threshold, dual-noise and rule-based block state providers. A feature runs
    only where the region may be written at its origin, as `Feature.place` checks, before drawing anything.
    `simple_block` places a state only where its survival rule holds, and a double plant as two halves where
    the block above is air, each half waterlogged when it replaces water; `block_column` checks its positions
    one step ahead of those it places and shortens its layers from the base or the tip; `disk` replaces target
    blocks from the top of each column within its radius and marks up to two non-air blocks above each run of
    replaced blocks for postprocessing. `harpas_worldgen::tree` places `tree` with straight, fancy, forking, giant,
    mega jungle, dark oak, cherry and upwards branching trunks, blob, fancy, spruce, pine, acacia, bush, jungle, mega
    pine, dark oak, cherry and random spread foliage, and mangrove roots: the tree draws its height, foliage height and radius, gives up outside
    the world's height, shortens to the free height its feature size allows (or gives up below its minimum clipped
    height), places dirt below the trunk where its provider allows, the trunk and the foliage at each attachment,
    with leaves waterlogged in water sources and persistent leaves left alone. Its blocks join four position sets
    that iterate in the order of Java's `HashSet<BlockPos>` (`harpas_worldgen::java_hash`); `updateLeaves` then
    walks them from the trunk, one set per distance, to set the `distance` of every leaf in the tree's bounds,
    including those of other trees, and `updateShapeAtEdge` updates both blocks on each side of the tree's surface
    (`harpas_worldgen::shape_update`): grass blocks, podzol and mycelium follow the snow above them, and
    vegetation, carpets, spore blossoms and double plants break when they lose their support, vines lose the
    faces that no block beside them (or the vine above) holds and break without faces, and cocoa breaks when the
    block it faces is not in `#supports_cocoa`. The blocks of structure templates that connect to their neighbors
    update as well (`connection_shape`): fences, panes and bars, walls with their low and tall sides and post,
    fence gates in walls, stair corners, door halves, double chests, and ladders, torches, wall banners and levers
    that break without what holds them. Ticks that leaves and liquids schedule there, and postprocessing
    marks, are not kept yet. Before those updates the tree's decorators run in order, on lists of its logs,
    leaves and roots taken in `HashSet` order and sorted by height, and their blocks join the decoration set:
    `trunk_vine` and `leave_vine` attach vines beside logs and leaves (hanging down to four more blocks below
    leaves), `cocoa` grows beside the lowest three blocks of the trunk, `beehive` places a bee nest facing south
    beside a log below the lowest leaves, at the first position of a shuffled list with air in front of it, and
    `place_on_ground` scatters leaf litter on solid ground (`isSolidRender`) at or above the
    `MOTION_BLOCKING_NO_LEAVES` surface around the trunk's base (`harpas_worldgen::tree_decorator`). A bee
    nest's block entity, with two or three bees, is kept in the chunk as vanilla saves it, and the FEATURES
    workers carry block entities through proto-chunks, removing one when a later block replaces its block.
    `harpas_worldgen::fallen_tree` places `fallen_tree`: a stump at the origin with its decorators, then, two or
    three blocks away in a random horizontal direction, a log two blocks shorter than its drawn length, on the
    first position within five blocks below the stump's top that is free for a tree over a sturdy top face. The
    log is placed only if every block is free and no more than two in a row overhang; its blocks lie along the
    direction's axis, mark up to two blocks above for postprocessing and run the log decorators, such as
    `attached_to_logs`, which places mushrooms beside a shuffled copy of the logs. No light is initialized while
    a chunk decorates: vanilla's sky light engine reports 15 in the columns it has no data for, so a mushroom
    survives only on a block in `#overrides_mushroom_light_requirement`, and the trees growing next to a fallen
    log's mushrooms break them. A configured feature with any part that is not ported, such as a scheduled tick
    (eyeblossoms) or a block without a survival rule (fire), is skipped as a whole. The ported features include
    the trees of every biome, the rooted azaleas above lush caves, the moss patches of pale gardens and lush caves, and the
    seagrass, kelp and sea pickles of oceans, rivers and swamps (`harpas_worldgen::ocean_vegetation`), which grow
    on the `OCEAN_FLOOR` heightmap in water where their blocks survive. `harpas_worldgen::multiface` places vines on
    the first face, without down, they can attach to, and glow lichen on a shuffled `can_be_placed_on` neighbor of
    an air or water block, then by `chance_of_spreading` spreads it once with `MultifaceSpreader`; vanilla's search
    beyond the origin sets its position from the origin each time, so it tries only the six neighbors.
    Sculk veins grow the same way with the sculk vein's spreader, which may also take replaceable, non-fire blocks
    and water sources but not the face toward sculk, a catalyst or a moving piston. `harpas_worldgen::sculk` places
    sculk patches (`SculkPatchFeature`) with vanilla's world generation `SculkSpreader`: each round adds charge
    cursors at the origin and updates them; a cursor on sculk or a vein spreads veins from its block
    (`MultifaceSpreader.spreadAll`), a vein turns a `#sculk_replaceable_world_gen` block behind a shuffled face
    into sculk, sculk may grow a sensor or a shrieker that can summon, and both decay; a cursor that keeps charge
    moves to a shuffled sculk or vein neighbor among the 18 that share a face or an edge, preferring one with
    a face against `#sculk_replaceable`, and stops 15 blocks from the origin. The patch may then turn its
    origin into a catalyst over a full block and add shriekers around it. `sculk_feature_parity` compares
    every block and the random tails of 288 placements of vanilla's deep dark and ancient city patches and
    sculk veins in test caves.
    `harpas_worldgen::bamboo` places five to sixteen bare stalk blocks over `#supports_bamboo`, then, if at least
    three fit, adds a leafy tip at the first position above them and changes the two blocks below it to leafy
    states; the tip can replace the obstruction that stopped the stalk. By the configured probability it first
    turns surface blocks in `#beneath_bamboo_podzol_replaceable` into a podzol disk of radius one to four,
    reading the live `WORLD_SURFACE` heightmap. These tags follow 26.1's vegetation and substrate tags
    ([Mojang's release notes](https://feedback.minecraft.net/hc/en-us/articles/44551668333837-Minecraft-Java-Edition-26-1)).
    `bamboo_feature_parity` compares 243 placements with vanilla, including results, random consumption,
    obstacles, chunk borders and world limits, both bamboo blocks' survival over all 29,873 supporting states,
    and 780 shape updates: stalk age follows an older stalk above, a shoot becomes a stalk below another stalk,
    and a shoot breaks without support below. Unsupported stalks keep their state until their scheduled tick,
    which is deferred along with other ticks scheduled during generation.
    `harpas_worldgen::block_blob` places the mossy cobblestone forest rocks of old growth taigas. It searches
    downward until the block below satisfies `can_place_on`, stopping above the lowest three blocks, then
    places three overlapping rounded lumps, drifting downward and toward negative X and Z after each one.
    Every block in the volume is replaced. The configured state and support predicate follow
    [26.1's `block_blob` configuration](https://www.minecraft.net/en-us/article/minecraft-java-edition-26-1);
    a predicate that accepts air can place a floating blob. `block_blob_feature_parity` compares blocks,
    results and random consumption in 960 placements across three seeds with both legacy and Xoroshiro
    sources, including mixed supports, unsupported columns, offset predicates, chunk borders and world limits.
    `harpas_worldgen::desert_well` searches downward from one block above its origin for sand and requires at
    least one non-air block in each foundation column's two supporting layers. It places the sandstone basin,
    water cross, pillars and slab roof, then chooses one block under the water in each of the two buried layers
    for suspicious sand. Their block entities carry the `minecraft:archaeology/desert_well` loot table and
    position-based seeds, as in vanilla; brushing and loot generation follow in M4. `desert_well_feature_parity`
    compares blocks, four live heightmaps, block entity NBT, results and random consumption in 320 placements
    across five seeds with both legacy and Xoroshiro sources, including rejected supports, partial foundations,
    obstructions, chunk borders, replaced containers and world limits. A natural well crossing a chunk edge is
    also checked through `applyBiomeDecoration`, worker generation and Anvil reloads.
    `harpas_worldgen::coral` places the warm ocean's `coral_tree`, `coral_claw` and `coral_mushroom` reefs:
    branching columns, bent fingers and hollow shells with their edges omitted. Each draws its block from
    `#coral_blocks` in tag encounter order, expanding nested tags and keeping the first occurrence of each
    block. Growth replaces water or `#corals` only where water lies above it, decorating accepted blocks with
    random coral plants or fans, sea pickles and outward-facing wall fans. `coral_feature_parity` compares all
    blocks, four live heightmaps, results and random tails in 468 placements across three seeds and both random
    sources, including obstructions, existing vegetation, flowing water, chunk borders and world limits.
    Warm ocean reefs also participate in `feature_blocks_parity` and `feature_worker_parity`, including Anvil
    reloads.
    `harpas_worldgen::underwater_magma` searches downward through water blocks for a floor strictly within
    `floor_search_range`, then considers the cube of `placement_radius_around_floor` around it. Each position
    draws a float before checking its blocks. The destination must be neither air nor a water block; below
    and beside it, each neighbor must close its face towards the destination with its occlusion shape and
    permit occlusion. This admits non-solid destinations and distinguishes glass from opaque supports and
    partial shapes from full cubes. `underwater_magma_parity` compares blocks, four live heightmaps, results
    and random tails in 216 placements across three seeds with both random sources, and every block state as
    the destination or one of its five closing neighbors. The full occluding faces of all 29,873 states are
    also checked by `block_behavior_parity`. A natural deposit crossing a chunk edge participates in
    `feature_blocks_parity` and `feature_worker_parity`, including Anvil reloads.
    `harpas_worldgen::freeze_top_layer` covers the 16×16 columns from the origin with snow and freezes source
    water at their motion-blocking surface. The biome at the surface decides both, using its temperature,
    altitude and frozen-ocean temperature pockets, even when it has no precipitation. Snow needs a full upper
    collision face, with the data pack's support overrides and exclusions, or eight snow layers below it;
    grass, podzol and mycelium become snowy. This runs before the target chunk initializes light, when block
    light is zero even beside emitting blocks, and consumes no random values. `freeze_top_layer_parity`
    compares every block in mixed surface columns and their four live heightmaps in 1,170 placements and
    replays across all 65 biomes and nine heights, including world limits and chunk borders, plus snow
    survival over all 29,873 block states. Natural snowy plains and a frozen river participate in the
    decoration and worker comparisons, with Anvil reloads.
    `harpas_worldgen::pointed_dripstone` places one- or two-block stalactites and stalagmites over
    `#dripstone_replaceable_blocks` or existing dripstone blocks. When both floor and ceiling qualify, a
    boolean chooses the ceiling. The base spreads clockwise, taking up to two further steps in any of the
    six directions even through blocks it cannot replace. The origin is overwritten, preserving waterlogging,
    and the taller variant needs air or a water block ahead. `pointed_dripstone_parity` compares blocks, four
    live heightmaps, results and random tails in 1,080 placements across three seeds and both random sources,
    six configurations, chunk borders and world limits; all 29,873 block states also serve as floor, ceiling,
    origin and possible second block. A natural dripstone cave participates in the decoration and worker
    comparisons, including Anvil reloads.
    `harpas_worldgen::large_dripstone` scans for a floor and ceiling, draws a uniform radius from the configured
    provider's bounds capped by the cave height, and samples each column's bluntness and scale independently.
    Both columns share a wind offset when both qualify, before searching for embedded bases and halving radii
    that do not fit. The radial profile fills air, water and lava; after its first placed block, a column stops
    at `#dripstone_replaceable_blocks` but skips other obstacles, including existing dripstone blocks.
    `large_dripstone_parity` compares the profile, blocks, four live heightmaps, results and random tails in
    1,248 placements across three seeds and both random sources, with eight configurations, dry and flooded
    caves, search boundaries, irregular and thin supports, shrinking bases, obstacles, chunk borders and world
    limits. A natural large-dripstone cave participates in the decoration and worker comparisons, including
    Anvil reloads.
    `harpas_worldgen::dripstone_cluster` samples a cluster's height, wetness, density and two radii, then visits
    its columns in X/Z order. Each column searches independently through air and water for any obstacle,
    accepting a floor, ceiling or both. Wet pools require base stone or water fluids on their sides and below;
    a new pool lowers the effective floor by one. Layers replace the data pack's dripstone-replaceable blocks,
    and pointed columns require a dripstone base. Heights follow a clamped normal distribution biased by the
    distance from the center; paired heights are split where they overlap, sometimes joining at merged tips.
    Placed columns overwrite their path and preserve waterlogging. `dripstone_cluster_parity` compares blocks,
    four live heightmaps, results and random tails in 3,072 placements and replays across three seeds and both
    random sources, with eight configurations, missing and irregular boundaries, existing dripstone, wet and
    flooded caves, obstacles, search boundaries, chunk edges and world limits. Natural clusters with long
    pointed columns participate in the decoration and worker comparisons, including Anvil reloads.
    `harpas_worldgen::monster_room` places underground monster rooms after checking solid floors and ceilings
    and one to five two-block-high openings. It excavates in X/descending-Y/Z order, keeping existing chests
    and spawners inside the room, clearing unsupported boundary blocks and making three quarters of supported
    floor blocks mossy. Each of two chests gets up to three attempts beside exactly one solid neighbor, faces
    away from opaque walls and saves `minecraft:chests/simple_dungeon` with its loot seed. The central spawner
    saves its defaults and a skeleton, zombie or spider, with zombies twice as likely. All draws share the
    feature's decoration random source. `monster_room_parity` compares blocks, four live heightmaps, full
    block entity NBT, results and random tails in 576 placements and replays across three seeds and both random
    sources, with different opening counts, partial and non-opaque supports, liquids, existing containers and
    spawners, chunk borders and world limits. A natural room crossing a chunk edge participates in decoration
    and worker comparisons, including chest and spawner NBT against vanilla and Anvil reloads. Spawner ticking
    and loot opening follow in M4.
    `harpas_worldgen::vegetation_patch` also places `waterlogged_vegetation_patch`: after replacing the ground,
    it collects positions enclosed below and on all four sides by sturdy faces, fills them with water and grows
    vegetation in that water, in a fresh Java `HashSet`'s iteration order. Lush caves' clay patches and pools
    grow small dripleaves with two halves and big dripleaf columns. Small dripleaves need their own support tag,
    or vegetation ground while in source water, and only the halves replacing water are waterlogged. A pool also
    waterlogs the first block of a big dripleaf column. Shape updates break unsupported small dripleaves and big
    leaves, turn a leaf below another leaf into a stem and keep unsupported stems until their scheduled tick,
    which follows in M4 along with fluid ticks. `lush_vegetation_parity` compares blocks, four live heightmaps,
    results and random tails on controlled floors, including holes, partial supports, flooded and obstructed
    origins, repeated placement, chunk edges and world limits, and compares survival and shape updates against
    every block state. The natural decoration and worker comparisons require both dripleaves and waterlogged
    plants in their samples and check their Anvil reloads.
    `harpas_worldgen::geode` places hollow amethyst geodes: distribution points sum inverse distances with
    `NormalNoise` seeded from the world seed, independently of decoration, producing the filling, inner,
    calcite and basalt layers and optional cracks. Protected blocks keep their states, but their providers and
    potential crystal positions consume the same random values as vanilla. Crystals try down, up, north, south, west and east in
    that order, facing outward into air or full water blocks and preserving source-water waterlogging.
    `geode_parity` compares blocks, four live heightmaps, results and random tails in 1,920 placements and
    replays across three seeds and both random sources, with ten configurations, weighted providers, invalid
    and protected blocks, different water fillings, decorations with horizontal or hopper facing properties,
    lenient codec defaults, chunk borders and world limits.
    A natural geode crossing into its eastern neighbor participates in decoration and worker comparisons,
    including Anvil reloads. Fluid ticks requested beside cracks follow in M4.
    `harpas_worldgen::block_pile` places village piles of hay, melons, pumpkins, snow and ice in two layers,
    only in air with a sturdy top face below, or a successful coin flip above dirt paths. Hay's
    `rotated_block_provider` draws X, Y or Z from the block's default state, discarding other configured
    properties. `block_pile_parity` compares blocks, four live heightmaps, results and random tails in 3,024
    placements across three seeds and both random sources, including partial supports, liquids, occupied
    positions, repeated placement, chunk edges and world limits. A synthetic biome schedule also checks
    worker decoration across chunk edges and Anvil reloads. Natural village placement awaits jigsaw blocks.
    `harpas_worldgen::fossil` places fossils (`FossilFeature`): a random rotation and one of the configured
    bone templates with its ore overlay, centered on the origin and buried 15 to 24 blocks below the lowest
    `OCEAN_FLOOR_WG` height of its footprint, but at least 10 blocks above the bottom of the world. More empty
    corners (air, water or lava) than allowed refuse it. Both templates go through their processor lists
    (`fossil_rot`, `fossil_coal`, `fossil_diamonds`) with the feature's random source as the placement random,
    which also draws each template's palette, and are clipped to the chunk and its eight neighbors. A feature
    index compiles fossils only with the owner's templates, which the chunk workers' decoration passes it.
    `fossil_feature_parity` compares every block of the region and the random tails of 288 placements of
    vanilla's own `fossil_coal` and `fossil_diamonds` over oceans, land, caves, lava, slopes and the bottom of
    the world, with both random sources.
    `harpas_worldgen::ice` places ice spikes, icebergs at the sea level and blue ice; an iceberg's ellipse can
    narrow to nothing under the water, where its distances are NaN and, as in vanilla, place nothing.
    `harpas_worldgen::liquid` places springs, where exactly `rock_count` of the five blocks beside and below are
    valid and `hole_count` are air, and lava lakes: four to seven ellipsoids in a 16x8x16 box, lava in the lower
    half and cave air in the upper, given up if liquid touches the upper half or the lower half has a wall that is
    neither solid nor lava, then a stone barrier around the lava and around half of the air. Vanilla schedules a
    fluid tick at each spring and a block tick at each block of the lake's air; they are not saved yet, so the
    water and lava of springs do not flow until a block next to them changes. A lake of water, which freezes over
    in cold biomes, is not compiled; vanilla's data has none.
    `harpas_worldgen::root_system` climbs from a lush cave's ceiling to the first block, at most 100 above, that
    takes an azalea over `#azalea_grows_on` with three blocks of air above it (or water in the lowest), gives up
    above lava or a block that is not solid, and once the tree grows fills the column below it with rooted dirt at
    random offsets and hangs roots under sturdy faces around the origin. The azalea's `bending_trunk` steps
    sideways near its top and bends over by two or three more logs, each holding random spread foliage. Pale oaks' `pale_moss` decorator places the
    `pale_moss_patch` configured feature, a `vegetation_patch` (`harpas_worldgen::vegetation_patch`), and hangs
    pale moss below logs and leaves; `creaking_heart` turns a log surrounded by logs into a dormant creaking heart.
    Pale moss carpets go through `MossyCarpetBlock.placeAt` (`harpas_worldgen::mossy_carpet`), whose topper keeps
    each side by a coin flip of the decoration region's own random source, seeded like `WorldGenRegion`'s from the
    world seed and the chunk being decorated. A block with a block entity that a feature places keeps vanilla's
    `DUMMY` placeholder in the proto-chunk until its block entity is created. A tree with roots starts its trunk `trunk_offset_y` above its
    origin; `mangrove_root_placer` needs the column below the trunk to take roots, then simulates roots on each side
    spreading out and down, failing the tree if one grows longer than its limit, and places them all, muddy roots in
    mud and, by chance, moss on the air above. `harpas_worldgen::huge_mushroom` places
    huge brown and red mushrooms: a height of four to six blocks, doubled one time in twelve, room of air or
    leaves for the stem and, for a brown one, its cap, then the cap and the stem over air or
    `#replaceable_by_mushrooms`. `alter_ground` replaces the ground in rounded squares around a giant
    trunk's base, searching each column from two blocks above to three below.
    `feature_blocks_parity` compares every block they place in 42 decorated chunks, with vanilla running
    the same features and no-ops at the others' indices, and `feature_worker_parity` compares final chunks
    from the workers the same way.
    `ore_feature_parity` compares the feature with vanilla's own `OreFeature` on the same Xoroshiro decoration
    source, including the random state after the call.
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
    other structure types are skipped. Woodland mansions are made as well. Ocean monuments check the full 3D biome volume within 29 blocks of a point
    9 blocks into the start chunk at sea level, against `required_ocean_monument_surrounding`. The biome at the
    generation point on the ocean floor is checked separately against the structure's allowed biomes. Their one
    saved building piece is laid out; its internal rooms are reconstructed as described below. Vanilla
    places shipwrecks wider or taller than 32 blocks by another rule, which none of its templates reaches, so Harpas
    refuses them. The `structure_start_parity`
    harness compares base heights at 1,764 columns, the starts, pieces and references of 4 seeds in 512×512 chunks,
    and the strongholds at all 128 ring positions of each seed, with vanilla's own `createStructures` and
    `createReferences`. Woodland mansions are rare, so it also compares every chunk where one may start in
    2048×2048 chunks, 56 mansions in all (1024×1024 chunks on pull requests). It also compares the full saved NBT of up to eight starts of each structure per seed,
    including every piece's placement state, and compares reading those records with vanilla's own loader.
    Jigsaw pieces reconstruct their template bounds on reload, as vanilla does, dropping any extra height reserved
    by the expansion hack. Ring positions and the biome checks of starts search biomes starting without a last
    result, whereas vanilla's depends on its worker threads, so a tie may resolve differently in a vanilla world.
  - Woodland mansions start on the lowest of four surface heights around a point 7 blocks into the chunk, at Y 60
    or above, turned at random. Their layout is three 11×11 grids of 8-block cells: corridors grow from the
    entrance with random turns, the cells around them become rooms, enclosed gaps are filled, and rooms are
    grouped in a shuffled order into four, two or one cells, each with a door on the first of its corners that
    borders a corridor, or none. A two-cell room with a door on the second floor may hold the stairs to a third
    floor, whose corridors grow from the landing. The layout becomes template pieces of type `wmp`: the entrance,
    outer walls traced clockwise around each floor, roofs with their fronts and corners, small walls where the
    third floor covers the second, corridor floors and carpets, inner walls and doors, and one room template per
    room, drawn from its floor's collection and turned and reflected for its door; rooms without a door are
    secret rooms. Pieces save their template's name within `woodland_mansion/`, their rotation and their mirror
    (`Mi`), which may be left-right as well as front-back. `mansion_layout_parity` compares the three grids of
    4,100 layouts cell by cell and the following random draw with vanilla's; `structure_start_parity` compares
    the pieces and saved records.
  - Chunk workers place woodland mansions in their chunk's decoration, in `surface_structures`
    (`StructureStart::place_mansion`), writing into the decoration region. Each piece that reaches the chunk
    places its template inside it, reflected then turned, without structure blocks; blocks that can hold water
    take the source water they replace and then the source water beside or above them, but not the template's
    own liquids. As the templates have no known shape, both sides of every face at the edge of what a piece
    placed update against each other, which can reshape blocks in the neighboring chunks, and then every placed
    block updates against all its neighbors. Data markers then place chests facing the turned marker direction
    with the mansion's loot table, drawing their loot seeds after those of the template's own chests, and turn
    the markers of the evokers, vindicators and allays inside the chunk into air; the mobs follow with
    gameplay in M4. Once every piece is placed, cobblestone fills each column below the lowest floor down to
    the first block that is neither air nor liquid. `mansion_parity` places four natural mansions of two seeds
    chunk by chunk on their carved terrain, 120 chunks (30 on pull requests), and compares every block, the
    blocks reshaped beside each chunk, the chests and banners and the decoration random with vanilla's own
    `StructureStart.placeInChunk`.
  - `MonumentLayout` builds ocean monuments' 46 grid cells and three special connectors, closes passages
    while retaining access to the entrance, reserves the eight-cell treasure core and fits the remaining
    cells into rooms in vanilla's order. It records every internal piece's bounds and the simple-room and
    wing designs, including wings and the penthouse outside the grid. `from_start` reconstructs this layout
    from the world seed and the single saved building piece, retaining its origin and orientation; internal
    pieces are not added to Anvil's `Children`. `monument_layout_parity` compares 3,237 layouts with vanilla:
    graphs, piece order, all eleven piece types, four orientations, negative and world-limit coordinates,
    designs, the following random draw and reconstruction from saved records (including a missing orientation).
    This layout comparison does not establish block parity.
  - `StructureStart::place_monument_exterior_in_chunk` places the building's outer wings, entrance arches,
    facade, roof and three tiers of walls, using the saved building's origin and orientation. Clearing
    preserves water and ice, empties space above sea level and extends five blocks outside the saved bounds.
    The 23 foundation pads extend through replaceable blocks until they meet support, leaving the two
    bottom layers untouched. Section intersection checks retain vanilla's asymmetric limits. Placement
    consumes no random draws and leaves saved records unchanged. `monument_exterior_parity` isolates the
    exterior by removing vanilla's internal child rooms, then compares every block and all six final-block
    heightmaps in 13,160 chunk placements and replays after reloading the building record. It covers all four
    orientations, every horizontal offset within a chunk, ocean, solid, air, mixed and uneven terrain,
    water, ice, plants and waterlogged blocks, negative coordinates, world limits and different sea levels.
    Pull requests use fewer offsets.
  - `MonumentLayout::place_rooms_in_chunk` follows the exterior with all eleven internal piece types:
    entry, treasure core with eight gold blocks, double rooms, three simple designs, sponge rooms, both
    wing designs and penthouse. Floors retain vertical passages; ceilings replace only source water when
    no cell exists above. Horizontal passages preserve water and ice as the exterior does. Child bounds
    gate placement and random consumption; an intersecting sponge room draws for all its columns before
    clipping writes to the chunk. Simple rooms draw their pillar boolean before checking vertical passages.
    `monument_interior_parity` compares 122,200 isolated-child and complete-monument chunk placements
    and replays, every block, all six final-block heightmaps and the decoration stream's next draw. It covers
    all piece types and designs, four orientations, negative and world-limit coordinates, chunk edges, mixed terrain, sea levels and
    vertical clipping. Second passes reconstruct rooms from saved building records without changing their
    NBT. No internal children are persisted. Scheduled fluid ticks and the complete FEATURES schedule
    remain pending; elder guardian spawning follows in M4 and is suppressed in the probe.
  - Chunk workers place ocean monuments in their chunk's decoration, in `surface_structures`
    (`StructureStart::place_monument`), with the exterior first and then the internal rooms.
    `StructureStart::place_monument_in_chunk` does the same for direct placements. The decoration stream is
    the one vanilla's `applyBiomeDecoration` seeds for `minecraft:monument` (index 6, step 4), so sponge rooms
    and simple-room pillars choose the same columns. Vanilla invokes the building only in chunks that intersect
    its saved bounds; the five-block exterior clearing is clipped to those chunks, so chunks reached only by
    the halo keep their terrain. A chunk's `References` likewise stays limited to the saved bounds. Only the
    single building record is saved, no internal children are persisted, and saved records are unchanged.
    The worker stream test in `monument_interior_parity` compares complete passes with vanilla; the
    engine's `monument_parity` checks that worker chunks saved at `carvers` hold the records but none of the
    monument's blocks in either request order, and compares chunks decorated without features, and their
    Anvil reloads, with the direct API. Elder guardians
    and scheduled fluid ticks follow in M4.
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
  - Chunk workers place buried treasure in its chunk's decoration, first in `underground_structures`
    (`StructureStart::place_buried_treasure`): search down from `OCEAN_FLOOR_WG` for its foundation, fill exposed
    neighbors, orient the chest and save its loot table and decoration-seeded loot seed without rolling any
    items. The piece moves to the chest, and the chunk saves its start with that move, as vanilla's does. The
    treasure's one-block shell fits in its source chunk. Mineshafts follow it in the same step, as in vanilla. `buried_treasure_parity` compares every block, heightmaps, chest NBT and saved starts in
    38 synthetic cases and four actual chunks per seed in two seeds (one seed on pull requests); the actual
    chunks come from workers that decorate without features, and are saved and reloaded from Anvil. Vanilla
    also reloads its own saved records and places the treasure again, which changes nothing.
  - Chunk workers place swamp huts in their chunk's decoration, in `surface_structures`
    (`StructureStart::place_swamp_hut`), so the ground height includes the features of the earlier steps, as
    vanilla's does: average `MOTION_BLOCKING_NO_LEAVES` ground height saved as `HPos`, oriented walls and roof corners, support columns through replaceable blocks and fence positions
    marked for postprocessing. Unwritten interior blocks and the saved `Witch`/`Cat` flags stay unchanged;
    spawning those entities belongs to M4. New huts fit inside their source chunk. A saved, height-adjusted
    piece crossing a boundary also places its intersection in neighboring chunks without copying its start.
    `swamp_hut_parity` compares every block, all six heightmaps, saved starts and postprocessing in 92 synthetic
    cases covering all orientations, terrain, supports and four-chunk pieces. It also compares two actual
    worker chunks per seed in two seeds (one seed on pull requests), which come from workers that decorate
    without features, checks Anvil reloads and a saved neighbor placed by decoration,
    and compares repeated placement after vanilla reloads the records. Below zero, vanilla's negative `HPos`
    sentinel intentionally causes another ground adjustment on each pass; these cases compare both passes.
    The vanilla probe suppresses entity spawning while observing blocks and restores the original flags.
  - Chunk workers place desert pyramids in their chunk's decoration, in `surface_structures`
    (`StructureStart::place_desert_pyramid`): the lowest `MOTION_BLOCKING_NO_LEAVES` height over the
    entire footprint, the random burial offset, foundation columns, rotated sandstone blocks and decorations,
    four chests with decoration-seeded loot, the pressure plate/TNT trap and the archaeology cellar. The cellar
    uses the decoration region's own random stream for its stairs and collapsed roof; suspicious sand selection is
    sorted and shuffled with a separate positional legacy stream and stores loot seeds from block positions.
    Chests read their neighbors from the decoration region. For new pieces, the decoration's start source fixes
    the shared height once per session as if vanilla processed the source chunk first, independent of worker
    order, from saved neighbor terrain where available and otherwise from terrain generated again through
    carvers with the same structure adaptation; already saved heights take precedence. Vanilla reads the
    heights when it places the pyramid, so lakes and other features of the earlier steps can lower them there. The probe uses that same schedule
    for worker comparisons. A vanilla world that processes another intersecting chunk first may choose a different
    burial offset. Cached starts remain immutable, and neighbor starts are not copied into the target's records.
    `desert_pyramid_parity` compares every block, all six heightmaps, chest/brushable NBT and saved starts in 160
    synthetic chunks across two seeds, including all rotations, slopes, water, negative coordinates, boundary
    chests, saved flags and the bottom of the world, then compares replay after loading Harpas's records in vanilla.
    It also compares eight natural worker chunks across two seeds, decorated without features, and their Anvil
    reloads, and checks that a target next to a saved, decorated source with altered blocks is saved at `carvers`
    without pyramid blocks and decorated reading those blocks. Pull requests use one seed.
  - Chunk workers place jungle temples in their chunk's decoration, in `surface_structures`
    (`StructureStart::place_jungle_temple`), with average `MOTION_BLOCKING_NO_LEAVES` ground height,
    saved `HPos`, randomized cobblestone/mossy cobblestone, rotated stairs and mechanisms, tripwire traps,
    vines and the piston/lever puzzle. Two chests and two dispensers save their loot tables and decoration-seeded
    loot without rolling items. New temples fit in their source chunk; saved pieces can place intersections
    across boundaries, reading the decoration region when orienting chests. Mechanism ticking belongs to M4.
    `jungle_temple_parity` compares every block, all six heightmaps, block entity NBT, saved starts and
    postprocessing in 208 synthetic chunks across two seeds, then compares their replays after vanilla loads
    Harpas records. Cases cover every rotation and container flag, slopes, water, negative coordinates,
    four-chunk pieces, negative `HPos` resumption and both vertical world limits. Two natural worker chunks
    across two seeds, decorated without features, their Anvil reloads and a saved neighbor with altered terrain
    placed by decoration are also checked. Pull requests use one seed.
  - Chunk workers place igloos in their chunk's decoration, in `surface_structures`
    (`StructureStart::place_igloo`), using the owner's template palettes, blocks and block entity NBT.
    Each piece samples the top's rotated entrance at `WORLD_SURFACE_WG`, temporarily shifts its template
    position, and saves the adjusted bounds while retaining its original template position. Reloading rebuilds
    those original bounds, as vanilla does. The top seals its ladder hole with snow only over non-air,
    non-ladder terrain. Furnace, bed, sign and brewing stand data are retained; the chest marker saves its
    loot table with the second decoration-seeded draw, without rolling items. Template placement updates
    snowy ground before the top's separate snow seal. The entrance height and template edge connections read the
    decoration region; `WORLD_SURFACE_WG` no longer changes after carvers, so the height does not depend on the
    order in which chunks decorate. Edge connections do: a chunk that decorates after its neighbor placed its part
    of the igloo connects to those blocks instead of the terrain, as in vanilla. The same edge updates reshape
    the neighbor's side: iron bars that a neighbor already placed beside the chunk connect to the new template
    block, or stop connecting, and decoration writes them into the neighbor as vanilla's generation region does,
    so the result no longer depends on which chunk decorates first. Workers clip the template's own writes to
    their chunk, including the snow seal that vanilla may write outside the target bounds. General block shape
    updates remain for later stages.
    `igloo_parity` compares every block, all six heightmaps, saved records and block entity NBT in 126 synthetic
    chunks across two seeds and their replays after vanilla reloads Harpas records. Cases cover all rotations,
    igloos with and without basements, both shaft-depth extremes, slopes, water, negative coordinates, chunk
    boundaries and both vertical world limits. Four natural igloos across two seeds also run through workers
    that decorate without features, Anvil reloads and repeated placement; each worker chunk must match vanilla
    given its neighbors either before or after their own decoration, including the neighboring blocks that
    its edge updates reshape. A saved, decorated neighbor with an
    altered entrance column checks that its terrain controls the height. Pull requests use one seed. Entity spawning remains for M4; other structure types
    remain unplaced.
  - Chunk workers place ocean and beached shipwrecks in their chunk's decoration, in `surface_structures`
    (`StructureStart::place_prepared_shipwreck`), using the owner's 20 templates and their
    palettes. Height sampling uses the unrotated footprint at the template origin, even when the hull turns
    around its pivot. Ocean pieces use the average `OCEAN_FLOOR_WG` height; beached pieces use the lowest
    `WORLD_SURFACE_WG` height, minus half the template height and a bounded burial draw. The adjusted origin
    and `height_adjusted` flag are saved, so a reloaded piece does not choose its height again. A separate
    bounded cache shares adjusted starts for up to 4,096 source chunks. The decoration's start source chooses the
    height with
    the source chunk's random stream before placing any intersection, retaining the burial draw in that
    source chunk's loot stream. Neighboring workers reuse the chosen height and skip that draw, as vanilla
    does after its first piece pass. This fixes a source-first schedule; a vanilla world that first places
    another intersecting chunk can choose a different burial offset and loot seeds.
    Placement retains water and fills liquid containers from source water above or beside them, recomputes
    stair, fence and door shapes (including existing blocks beside the template), and assigns chest loot tables without rolling
    items. Outside the target chunk, heights and blocks come from the terrain before decoration, which the start
    source reads from saved chunks or generates again through carvers: the template can reach beyond the
    decoration region, and its blocks then do not depend on the order in which chunks decorate. Writes are
    clipped to the target chunk. General block updates remain for later stages.
    `shipwreck_parity` compares every block, all six heightmaps, chest NBT, saved records and postprocessing
    in 656 synthetic chunks across two seeds, plus their replays after vanilla reloads Harpas records.
    Cases cover every template and rotation, multiple palettes, slopes, water, negative coordinates, chunk
    boundaries and both vertical world limits. Five natural shipwrecks across two seeds also run through
    workers that decorate without features, reversed worker request order, Anvil reloads and repeated
    placement, including a chunk shared with buried treasure. Two saved
    neighbor cases check altered ocean-floor columns and an already adjusted origin. Pull requests use one
    seed; the treasure overlap is included in the full sample.
  - Chunk workers place cold and warm ocean ruins in their chunk's decoration, in `surface_structures`
    (`StructureStart::place_ocean_ruin`), using the owner's 48 templates. Each intersecting
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
    to gameplay. Natural cold and warm ruins also run through workers that decorate without features, reversed
    request order, Anvil reloads and repeated placement. Saved-neighbor cases verify fitting against saved
    terrain and piece records, and that the target is saved at `carvers` without ruin blocks. Like shipwrecks,
    ruins read the terrain before decoration outside the target chunk, from saved blocks or generated again
    through carvers.
  - Chunk workers place Overworld ruined portals in their chunk's decoration, in `surface_structures`, using
    the owner's 13 templates. Rotations and
    front/back mirrors preserve each template's pivot. The saved `Properties` and `VerticalPlacement`
    rebuild the positional processors: gold removal, cold/ocean lava replacement, magma substitutions,
    block ageing and mossiness, protected blocks, lava submersion and optional blackstone replacements.
    Waterlogging and template edge shapes are resolved before jigsaw final states and terrain decoration.
    The chunk containing the piece center supplies its decoration stream, including chest loot seeds,
    netherrack spread, capped drip columns, vines and persistent jungle leaves. A center outside the
    world's vertical bounds skips the pass, as vanilla does. Replays also update existing vines whose
    supporting blocks were cleared.
    A portal's owning pass writes the entire piece and surrounding
    terrain, including positions outside the saved piece bounds. The decoration's start source prepares this
    complete sparse result against carved or saved terrain and shares up to 256 source chunks through a
    bounded cache; each chunk's decoration applies only its own writes, in the portal's step. The normal structure references retain vanilla's piece
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
  - Chunk workers place normal and badlands mineshafts in their chunk's decoration, in
    `underground_structures` after buried treasure (`place_mineshafts`), retaining the saved piece layout, entrances,
    orientation, depth and reference count. Rooms carve their upper dome; crossings include upper floors
    and supported posts; stairs excavate their rotated slope. Corridors place oak or dark oak supports,
    floating floors, hanging fences/chains, rails, torches and cobwebs. Pieces check the liquid shell clipped
    to the target chunk and the zoomed `mineshaft_blocking` biome at its clipped center, read from the
    biomes stored in the decoration region, as vanilla's `WorldGenRegion` does. Floor support and solid-render checks differ for slabs, stairs, leaves and glass.
    Each structure type shares one decoration-seeded stream across all its starts, in a new chunk's native
    reference-set iteration order. Spider corridors write cave-spider
    spawner NBT and save their `hps` flag as vanilla does. Chest minecart entities and loot remain with M4; their placement
    draws and rail blocks are already preserved. The light engine and spawner ticking are separate tasks.
    `mineshaft_parity` compares every block, all six heightmaps, postprocessing, saved records and spawner
    data in 558 synthetic chunks and their replays, including both types, all rotations, multiple references
    and reference-set growth, partial floors, liquids, blocked biomes, chunk edges and vertical limits.
    Natural worker chunks across three seeds, decorated without features, include a chunk shared with buried
    treasure, reversed requests and Anvil reloads. A saved-neighbor case checks piece records and a changed
    biome across a chunk edge, read from a saved, decorated neighbor.
  - Chunk workers place strongholds in their chunk's decoration, in `surface_structures`
    (`StructureStart::place_stronghold`), using the existing saved piece layout, in piece order. Corridors,
    prisons, three- and five-way crossings, both stair types, filler corridors and short/tall libraries retain
    their doors, child openings and room variants. Masonry draws only on box boundaries; boxes which skip air
    also skip draws outside the target chunk, while portal-room masonry and library cobweb draws include
    clipped positions. All pieces of a start share the stronghold decoration stream (index 19, step 4 in 26.1).
    Chests retain loot table names and seeds without rolling items. Chest facing excludes transparent and
    partial blocks; chests on a chunk edge read their neighbors from the terrain before decoration, which the
    start source resolves only for the positions they need, preferring saved terrain. Portal rooms place lava, stairs, bars and all twelve independently drawn eyes,
    including an active portal when every frame has an eye, and save silverfish spawner data. Spawner ticking
    follows in M4. The `Chest` and `Mob` flags are saved as vanilla saves them.
    `stronghold_parity` compares blocks, six final-block heightmaps, block entities, postprocessing and saved
    records in 5,472 placements and replays across two seeds: every piece type and rotation, all door types,
    crossing/library variants, air, liquids, caves, chunk boundaries and vertical limits, including individual
    pass replays after loading the saved records. Eighteen natural worker chunks across two seeds cover
    libraries, portal rooms, chests and crossings, decorated without features, reversed requests and Anvil
    reloads. A saved-neighbor test changes a chest's neighboring block to glass and checks that the target is
    saved at `carvers` without stronghold blocks, then decorated as vanilla places it.
- **Deterministic:** the same seed and data produce the same chunks, using the vanilla-exact random generators
  of [engine-core.md](engine-core.md#7-determinism).
- **FEATURES on the workers:** new chunks run the FEATURES stage on the chunk workers before players receive
  them, as vanilla does: every feature family and structure of the Overworld is placed, and structures are left
  out only when `generate-structures` is off. There is no setting to skip the stage, as there is none in
  vanilla; a world preset or data pack changes what it places. A chunk saved after FEATURES never runs the stage
  again, in Harpas or in vanilla.
  - A chunk decorates once its eight neighbors have completed carvers, and it is **final** once its eight
    neighbors have decorated too: from then on no decoration writes into it. Until then it is a proto-chunk in
    a store shared by the workers. Each decoration takes its 3x3 chunks out of the store, runs on one worker
    and puts them back, so two decorations never write into the same chunk at the same time, and no chunk is
    generated twice. A request for a chunk decorates the 3x3 chunks around it, which generates the 5x5 chunks
    around it through carvers, and the chunk leaves the store when it is final.
  - The logic thread receives only final chunks, so a decoration never writes into a chunk the logic thread
    owns. It saves every chunk it receives, even one nobody wants any more by the time it arrives, because
    its neighbors' blocks already depend on it.
  - When the store holds more than 2,048 proto-chunks, the least recently used are saved to their region files,
    with status `minecraft:features` once they have decorated and `minecraft:carvers` before, and read back when
    a decoration needs them. Shutdown waits for the workers' requests, saves the logic thread's chunks and then
    every proto-chunk. A final chunk leaves the store at `features` without light, which the logic thread
    computes before sending it and saves with it at `light`.
  - A saved chunk at `carvers` or `features`, written by Harpas or by vanilla, is resumed: its missing neighbors
    are generated and the chunks around it decorate, so a world generated before this setting existed gets its
    ores where players go. Earlier vanilla statuses have received no decoration writes and are generated again.
  - The edge of a world vanilla generated also holds chunks at `minecraft:initialize_light`. They have decorated,
    but vanilla's step to them needs nothing of the neighbors, so neighbors that have not decorated still write
    into them: they are resumed like chunks at `features`, and saved at `features` once they have, so that vanilla
    initializes their light again from the blocks decoration left. Vanilla's step to `minecraft:light` needs every
    neighbor at `initialize_light`, so a chunk saved at `light`, `spawn` or `full` is final and used as saved.
  - Decorations near each other run in request order, as in vanilla: a vein ore that reaches a block another
    vein replaced already can use its randomness differently. A chunk is compared with vanilla decorating its
    3x3 chunks west to east, and north to south within each column, as the workers do for a single request.
  - Positions that left the store in this session are remembered for the rest of it, a few bytes each, so a
    final chunk the logic thread has not saved yet is never generated again. A crash loses the proto-chunks in
    memory; chunks generated again next to decorated ones that were saved can then differ at the border.
- **Parallel:** missing chunks are generated through carvers on the bounded worker pool; a proto-chunk saved
  before it decorates keeps status `minecraft:carvers`, so either Harpas or vanilla can continue the later
  stages. Every structure is placed in
  decoration, so a chunk saved at `carvers` has none of their blocks yet; a hut's witch and cat are left for vanilla or M4. Pyramid and
  temple container flags are saved as vanilla saves them, since a decorated chunk is never placed again.
  Igloo pass replays also match vanilla, including its height changing when the existing roof raises the
  entrance's `WORLD_SURFACE_WG` height. Shipwreck replays retain their saved height. A beached replay skips
  the original burial draw, so its chest loot seeds can change, as in vanilla. These comparisons do not
  establish a complete FEATURES-stage round trip. Ocean ruin pass replays likewise refit their saved pieces
  to the existing terrain; blocks and loot can change when earlier placement changed that terrain.
  Ruined portal passes likewise replay against existing blocks, and generated chunks continue to save at
  `carvers`. Ocean monuments are placed in decoration, on every chunk intersecting the saved bounds, clipping
  the five-block clearing to each chunk and keeping only the single saved building record. Full feature-stage
  resumption and lighting remain separate roadmap tasks.
- **Lighting:** a port of vanilla's light engine, with results applied at defined points
  ([engine-core.md](engine-core.md#2-parallel-only-where-it-cannot-be-observed)).
  - `harpas_worldgen::light` computes sky and block light for a chunk from its blocks and one chunk around it,
    using the block registry's per-state emission and dampening. Direct sky light falls without fading;
    dimmer light loses a level per block, and water, leaves and partial blocks dampen it. Slabs, stairs, snow
    layers and the other blocks that use their shape for light occlusion close the faces their shape covers:
    light does not pass between two blocks whose facing faces cover the whole face together, and direct sky
    light stops at such a face, as vanilla's `shapeOccludes` and sky light sources do. A generated chunk is
    lit from its eight loaded neighbours, so a generating world keeps one extra chunk loaded around each
    player's view. The carver chunk on disk keeps no light, leaving vanilla free to run features and its own
    light later.
  - Light workers, as many as the chunk workers, compute it once the chunk and its neighbours are loaded,
    and the logic thread sends the chunk when its light is ready; every viewer gets the same result, and a
    connection only packs it. The light is kept in memory while the chunk is loaded, with uniform layers as
    one value, and computed again when the chunk or a neighbour changes.
  - `light_parity` starts the reference server, generates a 7×7 area and compares every saved sky and block
    light layer of three chunks with Harpas's engine on the same blocks. Vanilla omits a sky layer when it is
    all fifteen and a block layer when it is all zero; the harness accepts a missing layer only when Harpas's
    layer is uniform.
  - Chunks that ran FEATURES on the workers are lit the same way. Once a chunk at `features` handed out by the
    decorating source has its light, it is saved again with it, at vanilla's `light` status with `isLightOn`,
    so vanilla keeps the light instead of lighting the chunk again and runs `spawn` for it. Its neighbors have
    all decorated, so its blocks and theirs are final, as vanilla's `light` step requires. Like vanilla, the save
    leaves out sky layers that are all fifteen and keeps every block layer, so the chunk is sent with the same
    light after it is read back. Shutdown waits for the light of every such chunk whose neighbors are loaded.
    Chunks at `carvers`, and chunks at `features` in a world that does not decorate, whose neighbors may still
    decorate, are saved without light.
  - Light updates for live block changes (M4), which now light the whole chunk again, remain to be done. A
    chunk saved at `light` is read as saved; until then its light is not computed again after a change.
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
   differences. `round_trip_parity` has vanilla generate the spawn area of a world and place blocks whose block
   entities hold data, then has the server load every chunk it saved, at every status, and write each one whole
   again; every chunk holds exactly what vanilla saved, compared as NBT, and vanilla reopens the world without
   chunk errors. That harness leaves `level.dat`, player data, entities and points of interest untouched.
   Inventory/equipment serialization and item components are compared separately by `player_inventory_parity`;
   the engine now saves player inventories while preserving the rest of their documents.
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

9. Mojang, "Minecraft Java Edition 1.21.5" (player armor and offhand stored in the `equipment` field;
   legacy inventory armor/offhand slot numbers remain readable). https://www.minecraft.net/en-us/article/minecraft-java-edition-1-21-5
