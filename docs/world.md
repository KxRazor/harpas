# World

Scope: how Harpas stores worlds, where it gets vanilla's game data (world generation, recipes, loot tables,
tags, structures), how it generates and saves chunks, and how it reaches and checks parity with vanilla. The
tick loop and in-memory storage are covered in [engine-core.md](engine-core.md).

Status: design. The [Mojang EULA](#mojang-eula) acceptance and [vanilla game data](#vanilla-game-data)
provisioning are implemented; both happen on every start, since every start needs the vanilla game data. Facts about vanilla cite their source in
[References](#references); everything else is a design decision of this project.

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

Harpas reads and writes vanilla's region format [1]:

- Region files `r.x.z.mca`, each holding 32×32 chunks, built from 4 KiB sectors, with a location table and a
  timestamp table as the first two sectors.
- Three folders use region files: `region/` (chunks), `entities/` and `poi/`.
- Each chunk's data starts with its length and a compression byte. Harpas writes **zlib**, as vanilla does.
  Other types vanilla supports, including LZ4 since 24w04a, are read [1]. Whether to write LZ4 is decided by a
  benchmark, and only if it wins clearly.
- Chunks larger than 255 sectors are stored in external `c.x.z.mcc` files, as vanilla does [1].
- The rest of the world folder (`level.dat`, player data, statistics, advancements, saved data such as maps,
  raids and scoreboards) uses vanilla's files and formats.

### Supported world versions

- Harpas opens worlds of the server version ([versions.md](versions.md#normal-mode-rust-engine)), currently
  26.1.
- **Older world:** the server does not start, and the error explains how to upgrade it: open it once with
  vanilla of the supported version, which upgrades worlds on load. Converters for older versions may come
  later.
- **Newer world:** the server does not start, to avoid damaging data it does not understand.

### Saving

- Saves happen when vanilla saves: the periodic autosave every 6,000 ticks [2], when chunks unload, and on
  shutdown.
- At a save point the logic thread only takes a **snapshot** of the changed sections and entities; worker
  threads serialize, compress and write them. The tick does not wait for disk.
- Writes are atomic per region file, so a crash never leaves a half-written region.

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
- It **extracts only the data files** into one zip, `.harpas/vanilla-data/<version>.zip`: the `data/`
  directory and `version.json` of the game jar, which the server jar bundles under `META-INF/versions/` since
  1.18. The jar is never run and is deleted after extraction. The zip is written under a temporary name and
  renamed into place, so an interrupted start leaves nothing behind and keeps the data already installed.

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
- **Deterministic:** the same seed and data produce the same chunks, using the vanilla-exact random generators
  of [engine-core.md](engine-core.md#7-determinism).
- **Parallel:** chunks are generated on the worker pool, stage by stage. Each stage of a chunk depends only on
  the seed, the data and earlier stages of nearby chunks, so parallel generation gives the same result.
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

Vanilla is the specification. Every behavior is ported one to one and checked against vanilla itself.

- **Reference server in CI only.** Mojang's server runs in continuous integration as the reference for
  differential tests. It is never committed, never part of a release, and never part of the Harpas binary.
- **Order of work**, from the easiest to compare to the hardest:
  1. world generation: same seed, compared block by block, biome by biome;
  2. block behavior and redstone: scripted circuits compared tick by tick;
  3. entities and mobs: movement, collisions, AI, spawning;
  4. items, inventories, crafting and loot.
- **Coverage report:** each subsystem has a parity status (not started, partial, matching) published with
  every release, and known differences are documented.

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
