# Changelog

All notable changes to Harpas are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Harpas adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) as described in [docs/releases.md](docs/releases.md).
Entries since the last move are still in [changelog.d/](changelog.d/README.md), one file each, until a release moves
them here.

## [Unreleased]

## [0.2.0+26.1] - 2026-11-16

Worlds. The server opens vanilla 26.1 worlds, where players explore them, and saves them back so that vanilla reads
every chunk as it saved it. A new world generates the same world as vanilla for the same seed: terrain, biomes,
caves, every Overworld feature and structure, and light, generated and saved on worker threads. Breaking and
placing blocks, mobs, items and the Nether and the End follow in 0.3.0; Bedrock players are not supported yet.

### Added

- Woodland mansions in new worlds: their templates with both mirrors, waterlogging and shape updates, chests with
  loot, mob markers and cobblestone foundations, placed in decoration and compared chunk by chunk with vanilla
  (`harpas-worldgen`). Evokers, vindicators and allays follow with gameplay.

- Woodland mansion starts: the layout of corridors, rooms and the third floor, and its template pieces with
  their rotations and mirrors, compared with vanilla's, and the left-right mirror of templates and block states
  (`harpas-worldgen`).

- Saved light: a final chunk is saved with the light the chunk workers computed for it,
  at vanilla's `light` status with `isLightOn`, so vanilla keeps that light instead of lighting the chunk again and
  spawns the chunk's first mobs when it opens the world (`harpas-engine`).

- Sculk patches generate in the deep dark and in ancient cities, spreading sculk, sculk veins, sensors,
  shriekers and catalysts as vanilla's world generation does, and sculk veins grow on deep dark cave walls,
  when chunks run the FEATURES stage (`harpas-worldgen`).

- Fossils generate below the ocean floor in deserts, swamps and mangrove swamps, with their coal and diamond ore
  overlays, when chunks run the FEATURES stage (`harpas-worldgen`, `harpas-engine`).

- Villages, pillager outposts, trail ruins, trial chambers and ancient cities now place their blocks in
  generated chunks (`harpas-engine`, `harpas-worldgen`). Compared with vanilla 26.1 chunk by chunk for real
  starts of each. Their decorations that reach into neighboring chunks, such as village trees, and the
  ancient city's sculk patches follow separately.

- Jigsaw list pool elements, and feature pool elements such as the trees, flowers and hay piles of villages,
  place as in vanilla (`harpas-worldgen`). Compared with vanilla 26.1 for every list element and every
  feature element except the ancient city's sculk patches, which follow with that feature.

- Jigsaw template pool elements place their blocks in a chunk as in vanilla: the processors each element adds,
  turned block states, waterlogging and water filling, and block entities with loot seeds (`harpas-worldgen`).
  Compared with vanilla 26.1 for every single and legacy single element of every pool. Running them on chunk
  workers follows separately.

- Structure template block states turn and reflect as in vanilla: facings, orientations, rail shapes,
  connection faces, axes and sign, banner and skull rotations, with vanilla's stair, chest, anvil, fire and
  chorus plant exceptions (`harpas-worldgen`). Compared with vanilla 26.1 for every block state. Placing
  jigsaw templates on chunk workers follows separately.

- Jigsaw template replacement: strict final block states, structure-void filtering and attached SNBT
  validation, preserving vanilla’s processor order, missing-data behavior and random consumption
  (`harpas-worldgen`). Compared with vanilla 26.1, including every registry state and the final states
  in the server owner’s structure templates. Placing jigsaw templates on chunk workers follows separately.

- Static block entity data in structure rules (`append_static`), with recursive NBT merges, vanilla JSON
  numeric and list types, and existing-data mutation in capped processors (`harpas-worldgen`). Template
  placement for jigsaw structures on chunk workers remains pending.

- Template material processors: `block_age`, `blackstone_replace` and `lava_submerged_block` now run in
  the ordered processor pipeline, preserving vanilla's block properties, outline-shape checks, NBT and
  placement-settings random consumption. Complete processing passes are compared with vanilla, including
  every block state and interactions with gravity, rules, filters and capped finalizers. Jigsaw replacement
  and template placement on chunk workers follow separately (`harpas-worldgen`).

- Template gravity processors (`gravity`), with all six heightmaps, original template-local heights,
  live-level heightmap substitutions and ordered rules, filters and capped finalizers. Complete processing
  passes, terrain reads, preserved NBT and random consumption are compared with vanilla's own pipeline
  (`gravity_processor_parity`). Jigsaw template placement on chunk workers follows separately
  (`harpas-worldgen`).

- Structure template block filters: deterioration (`block_rot`), protected terrain (`protected_blocks`),
  ignored block types (`block_ignore`) and passthrough (`nop`), combined with rules and capped finalizers in
  an ordered pipeline. All 40 vanilla processor lists compile and are compared with vanilla's own pipeline,
  including block filtering, NBT and placement randomness (`harpas-worldgen`). Jigsaw template placement on
  chunk workers follows separately.

- Structure template processors: `capped` rule delegates with whole-template limits, world-seeded candidate
  shuffles, integer limit providers and ordered finalization, compared with vanilla's processor pipeline,
  including archaeology loot NBT and chains of limits (`harpas-worldgen`). Jigsaw block placement on chunk
  workers follows separately.

- Template rule processors: load ordered processor lists and apply per-block substitutions with block,
  state, tag, probability and distance predicates, positional randomness and preserved, cleared or appended
  loot NBT, compared with vanilla's own processors (`harpas-worldgen`). Jigsaw block placement follows.

- Village block pile features: hay with random axes, melons, pumpkins, snow and weighted ice, with vanilla's
  ground support and dirt-path checks (`harpas-worldgen`). Natural village placement follows with jigsaw
  structure blocks.

- Java chunk encoding and compression shared between viewers through a cache bounded to 4,096 entries and
  64 MiB of frame buffers. Changes to chunk contents or light invalidate the cached packet, unloaded chunk data
  is released, and each connection encrypts its own stream (`harpas-java`, `harpas-server`).

- Amethyst geodes with layered shells, cracks, budding amethyst and directional crystals, compared with
  vanilla's feature and chunk decoration, including worker generation across chunk edges and Anvil reloads
  (`harpas-worldgen`, `harpas-engine`). Fluid ticks beside cracks follow in M4.

- Lush cave clay patches and pools with small and big dripleaves, waterlogging, support rules and shape
  updates, compared with vanilla's features and chunk decoration, including worker generation and Anvil
  reloads (`harpas-worldgen`, `harpas-engine`). Fluid and block ticks follow in M4.

- Underground monster rooms with cobblestone walls, mossy floors, oriented loot chests and saved spawner
  species, compared with vanilla's feature and chunk decoration, including worker generation and Anvil reloads
  (`harpas-worldgen`, `harpas-engine`). Spawner ticking and opening chest loot follow in M4.

- Dripstone cave clusters with wet pools, layered bases, stalactites and stalagmites, merged tips and
  waterlogged columns, generated on chunk workers and saved in Anvil
  (`harpas-worldgen`, `harpas-engine`).

- Large dripstone columns (`large_dripstone`), with cave scanning, embedded bases that shrink to fit, radial
  profiles, shared wind offsets and obstacle handling. Compared with vanilla 26.1, including natural chunk
  generation on workers and Anvil reloads. 153 of the 164 Overworld placed features now run (`harpas-worldgen`).

- Small stalactites and stalagmites (`pointed_dripstone`), with spreading dripstone bases, random floor or
  ceiling selection, one- or two-block tips and waterlogging. Compared with vanilla 26.1, including natural
  chunk generation and Anvil reloads. 148 of the 164 Overworld placed features now run (`harpas-worldgen`).

- Surface snow and ice during chunk decoration (`freeze_top_layer`), with biome temperatures, altitude,
  frozen ocean temperature pockets, snow support and snowy grass, podzol and mycelium
  (`harpas-worldgen`), including worker generation and Anvil saves.

- Underwater magma deposits, with floor searches, directional occlusion and vanilla placement randomness,
  included in worker generation and Anvil saves (`harpas-worldgen`).

- Warm ocean coral reefs: branching trees, claws and hollow mushrooms, with coral plants, fans and sea
  pickles, generated on chunk workers and saved through Anvil (`harpas-worldgen`, `harpas-engine`).

- Desert wells in generated deserts: sandstone foundations, water, pillars and slab roofs, with two buried
  suspicious sand blocks and their archaeology loot metadata, compared with vanilla 26.1, including worker
  generation and Anvil reloads (`harpas-worldgen`, `harpas-engine`). Brushing
  follows with gameplay.

- Forest rocks: mossy cobblestone boulders in old growth taigas (`block_blob`), with 26.1's configurable block
  state and support predicate, on the FEATURES chunk workers (`harpas-worldgen`, `harpas-engine`).

- Bamboo stalks and their podzol patches in jungles, including ground support and shape updates for bamboo
  and bamboo shoots, compared with vanilla 26.1 (`harpas-worldgen`).

- Lakes and springs: `lake` with the underground and surface lava lakes, their cave air and stone barrier, and
  `spring_feature` with the water and lava springs in cave and mountain walls, without the ticks vanilla schedules for
  them. 144 of the 164 Overworld placed features now run (`harpas-worldgen`).

- Release binaries carry a build provenance attestation: `gh attestation verify <archive> --repo KxRazor/harpas`
  proves that the archive was built by the project's Release workflow.

- Releases carry `harpas` binaries for macOS on Intel and on Apple Silicon, next to those for Linux and Windows.

- Frozen biomes: the `spike` feature of ice spike plains, `iceberg` with packed and blue ice icebergs centered at the
  sea level, smoothed, snowed on and cut through, and `blue_ice`, which grows under packed ice in frozen oceans.
  139 of the 164 Overworld placed features now run (`harpas-worldgen`).

- Vines and glow lichen: the `vines` feature of jungles, swamps and caves, and `multiface_growth` with glow lichen,
  which attaches to stone, deepslate and the other `can_be_placed_on` blocks in caves, waterlogged in water, and
  spreads through `MultifaceSpreader` to its own block, the next one or around the corner. 135 of the 164
  Overworld placed features now run (`harpas-worldgen`).

- Ocean plants: the `seagrass`, `kelp` and `sea_pickle` features on the ocean floor of oceans, rivers and swamps,
  with the survival rules of seagrass, tall seagrass, kelp and sea pickles. 132 of the 164 Overworld placed
  features now run (`harpas-worldgen`).

- Azaleas: the `bending_trunk` trunk placer and the `root_system` feature, which climbs from a lush cave's
  ceiling to the first spot with room for an azalea tree, places rooted dirt in the column below it and hangs roots
  around the cave ceiling. Every trunk, foliage and root placer of vanilla's trees is now ported, and rooted
  azaleas grow above lush caves, 121 of the 164 Overworld placed features (`harpas-worldgen`).

- Pale gardens: the `pale_moss` and `creaking_heart` decorators, `vegetation_patch`, and pale moss carpets placed
  through `MossyCarpetBlock.placeAt` with the decoration region's own random source (`WorldGenRegion.getRandom`),
  which `FeatureWorld::start_region` now seeds as vanilla does. Blocks with a block entity that features place
  keep vanilla's `DUMMY` placeholder in proto-chunks, and `BlockBehavior` gains `hasBlockEntity`, compared for
  every block state. Pale garden trees and moss patches and the moss patches of lush caves now grow, 120 of the 164
  Overworld placed features. The feature probes' regions now hold the
  configured feature registry, a chunk generator and the region's random source, which decorators read
  (`harpas-core`, `harpas-worldgen`).

- Mangroves: the `upwards_branching` trunk placer, which grows through `can_grow_through` and places no block
  below, the `random_spread` foliage placer, `mangrove_root_placer` with its simulated roots, muddy roots in mud,
  waterlogging and moss on top, and the `attached_to_leaves` decorator, which hangs propagules. Mangrove swamps
  now grow their trees, 116 of the 164 Overworld placed features. `tree_feature_parity` adds both mangroves, a
  mangrove swamp, a jungle and a dark forest, and shifts each chunk's pattern of positions so that every feature
  finds room; `feature_blocks_parity` adds a mangrove swamp where mangroves grow (`harpas-worldgen`).

- Dark oak and cherry trees and huge mushrooms: the `dark_oak` and `cherry` trunk and foliage placers, with the
  cherry's branches and hanging leaves, and `huge_brown_mushroom` and `huge_red_mushroom`. Dark forests, cherry
  groves and mushroom fields now grow, 115 of the 164 Overworld placed features. The feature probes now load
  configured features from the jar's data pack, as a server does, instead of the registries vanilla builds in
  code: they differ where a codec writes back another field, as the cherry's `corner_hole_chance` does.
  `tree_feature_parity` adds dark oaks, cherries and huge mushrooms (`harpas-worldgen`).

- Jungle, savanna and giant trees: the `forking`, `giant` and `mega_jungle` trunk placers, the `acacia`, `bush`,
  `jungle` and `mega_pine` foliage placers and the `alter_ground` decorator, which spreads podzol around giant
  spruces and pines. Jungles, sparse jungles, savannas, windswept savannas, both old growth taigas and bamboo
  jungles now grow their trees, 112 of the 164 Overworld placed features. `tree_feature_parity` adds acacias,
  jungle bushes, mega jungle trees, mega pines and mega spruces, and checks that vanilla places each of its 22
  features (`harpas-worldgen`).

- Fallen trees: `minecraft:fallen_tree`, a stump with its decorators and, two or three blocks away in a random
  direction, a log lying on the first free block over sturdy ground within five blocks below, placed only when
  no more than two of its blocks in a row overhang, with mushrooms from the new `attached_to_logs` decorator.
  Mushrooms survive only on blocks in `#overrides_mushroom_light_requirement`, since no light is initialized
  while chunks decorate, so trees break them and the mushroom patches of eight placed features now run.
  Decorators move to `harpas_worldgen::tree_decorator`. The trees of plains, forests, birch and flower forests,
  taigas, snowy biomes, badlands and windswept hills now grow, 105 of the 164 Overworld placed features.
  `tree_feature_parity` adds the five fallen trees, 102 features per chunk, and the reference region's light
  reads as unlit (`harpas-worldgen`).

- Tree decorators: `trunk_vine`, `leave_vine`, `cocoa`, `beehive` and `place_on_ground`, run in order on the
  tree's logs, leaves and roots in vanilla's `HashSet` order sorted by height. Bee nests keep their two or three
  bees in the saved block entity, which the FEATURES workers now carry through proto-chunks and remove when a
  later block replaces theirs. Vines lose the faces nothing holds and cocoa breaks without its log in the shape
  updates along a tree, and `BlockBehavior` gains `isSolidRender` and full collision faces. Swamp and meadow
  trees now grow, 87 of the 164 Overworld placed features; most other biomes' trees wait for fallen trees.
  `tree_feature_parity` adds trees with bee nests, vines, cocoa and leaf litter and compares their block
  entities, `feature_blocks_parity` adds a meadow with a bee nest, and `block_behavior_parity` compares
  `isSolidRender` for all 29,873 block states (`harpas-core`, `harpas-worldgen`, `harpas-engine`).

- Trees: `minecraft:tree` with straight and fancy trunks, blob, fancy, spruce and pine foliage and two- and
  three-layer feature sizes, for trees without decorators or roots. Trees shorten to the free height around
  them, set the `distance` of every leaf in their bounds walking vanilla's `HashSet` order, and update the
  blocks along their surface, breaking double plants and vegetation that lose their support and clearing snowy
  grass. Groves and the trees of oceans and rivers now grow, 85 of the 164 Overworld placed features.
  `tree_feature_parity` places 48 oak, fancy oak, birch, spruce and pine trees one after another in each of 6
  chunks and compares every block and each tree's random state with vanilla's own features, and
  `feature_blocks_parity` adds a grove and a river (`harpas-worldgen`).

- Simple features: `simple_block`, `random_selector`, `simple_random_selector`, `random_boolean_selector`,
  `block_column` and `disk`, with the simple, weighted, randomized-int, noise, noise-threshold, dual-noise and
  rule-based block state providers and the survival rules of the flowers, grasses, bushes, dry vegetation,
  azaleas, lily pads, carpets, leaf litter and spore blossoms they place. 83 of the 164 Overworld placed
  features now run, including every flower and grass patch, sugar cane, cactus, pumpkins, melons and disks of
  sand, clay and gravel. `feature_blocks_parity` compares every block they place in 24 chunks with vanilla's
  own `applyBiomeDecoration`, and `feature_worker_parity` compares final chunks with all of them; the center
  support of every block state face joins `block_behavior_parity` (`harpas-core`, `harpas-worldgen`).

- Block shapes and behavior: the collision shape of every 26.1 block state, from minecraft-data, and vanilla's
  solid, replaceable, liquid and motion-blocking flags and full faces derived from them in `harpas-core`.
  `block_behavior_parity` compares all 29,873 states with vanilla's. The `solid`, `replaceable`,
  `has_sturdy_face` and `would_survive` block predicates now apply, with the survival rules of saplings, the
  firefly bush, the mangrove propagule, sugar cane and cactus, so every 26.1 placed feature's placement runs;
  `placement_parity` compares them all, in more biomes. The datapack loader reads fluid tags (`harpas-core`,
  `harpas-worldgen`).

- Placement modifiers: all 15 vanilla types, from `heightmap`, `random_offset`, `block_predicate_filter` and
  `surface_water_depth_filter` to the noise-driven counts, `environment_scan`, `count_on_every_layer` and
  `fixed_placement`, with the integer providers, height providers and block predicates they use, run depth
  first as vanilla's streams are. Placed features whose predicates need block shapes or survival rules
  (`solid`, `replaceable`, `would_survive`, `has_sturdy_face`) are still skipped. `placement_parity` compares
  every position of 142 Overworld placed features with vanilla's own modifiers on the same carved terrain
  (`harpas-worldgen`).

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

- Shape updates at the edges of trees and structure templates also reshape fences, panes and bars, walls, fence
  gates, stairs, doors and chests, and break ladders, torches, wall banners and levers without their support, as
  vanilla's do (`harpas-worldgen`).

- Faster startup: the Overworld's generator and its structure placement share one structure state, whose
  stronghold ring positions are computed once instead of up to three times, so preparing world generation takes
  about 6 seconds instead of 16 (`harpas-engine`, `harpas-server`).

- New chunks always run the FEATURES stage, as in vanilla: ores, vegetation, trees, lakes, springs and every
  structure of the Overworld are placed before players receive them, and `generation.features` in `harpas.toml`
  is gone. `generate-structures=false` still leaves structures out (`harpas-server`).

- Mineshafts and strongholds are placed in the FEATURES stage, in their decoration steps as in vanilla, instead of
  right after carvers. Mineshafts now follow buried treasure, as in vanilla, and read the biomes stored in the
  chunks being decorated; mineshaft and stronghold spawner and chest flags are saved as vanilla saves them
  (`harpas-engine`, `harpas-worldgen`).

- Shipwrecks, ocean ruins and ruined portals are placed in the FEATURES stage, in their decoration step as in
  vanilla, instead of right after carvers. Outside the chunk they read the terrain from before decoration, so their
  blocks do not depend on the order in which chunks decorate (`harpas-engine`, `harpas-worldgen`).

- Desert pyramids, jungle temples and igloos are placed in the FEATURES stage, in their decoration step as in
  vanilla, instead of right after carvers. They read neighboring blocks from the chunks being decorated, and
  pyramid and temple chests and dispensers save their placed flags as vanilla does (`harpas-engine`,
  `harpas-worldgen`).

- Ocean monuments are placed in the FEATURES stage, in their decoration step as in vanilla, instead of right
  after carvers (`harpas-engine`, `harpas-worldgen`).

- Swamp huts are placed in the FEATURES stage, in their decoration step as in vanilla, instead of right after
  carvers, so their ground height includes the features placed before them (`harpas-engine`, `harpas-worldgen`).

- Buried treasure is placed in the FEATURES stage, in its decoration step as in vanilla, instead of right after
  carvers (`harpas-engine`, `harpas-worldgen`).

- Jigsaw structures (villages, pillager outposts, trail ruins, trial chambers and ancient cities) are placed in
  the FEATURES stage, step by step before each step's features as in vanilla, and in every chunk their pieces
  reach rather than only where they start; village trees, flowers and hay now reach neighboring chunks
  (`harpas-engine`, `harpas-worldgen`).

- Chunk workers no longer wait for each other while there are other chunks to
  generate around them: a view of 441 chunks generates at 57 chunks a second with 8 workers instead of 35,
  and at 45 with 4 instead of 22 (`harpas-engine`).

- Saving writes only the chunks that changed into free sectors of their region, as vanilla does, instead of
  reading and rewriting the whole region file: 7 ms instead of 160 ms for one chunk of a full region on a
  disk. A save is still one transaction with the region's `.mcc` files (`harpas-anvil`, `harpas-engine`).
- A region with a chunk entry that cannot be read no longer stops the server when another of its chunks is
  saved: the entry is left as it is. Only an entry that points past the end of the file still does
  (`harpas-anvil`).

- Chunks are read and generated by two thirds of the machine's hardware threads, at most 16, instead of at
  most 4: 8 workers on a machine with 12 threads, where generation through carvers is 15% faster, and 67% faster
  with the FEATURES stage (`harpas-engine`).

- A tick no longer computes every player's view again: views and the chunks kept loaded are computed when a
  player enters another chunk, changes its view distance, joins or leaves. With 100 players at view distance
  32 standing still, the chunk part of a tick takes 0.2 ms instead of 10 ms. Player profiles, skins included,
  are no longer copied each time a player moves (`harpas-engine`).

- A connection sends what the server has for its player as soon as it is ready, instead of up to 50 ms later
  when the player's client was sending nothing. Each connection now reads its socket on a second thread
  (`harpas-server`, `harpas-engine`).

- Release binaries are built with link-time optimization across every crate: world generation is about 4%
  faster and the binary 17% smaller.

- `rsa` 0.10.0 replaces its release candidate, which a build without `Cargo.lock` no longer resolved to code that
  compiles, and the exception for RUSTSEC-2023-0071, fixed in that release, is gone (`harpas-java`).

- The light of generated chunks is computed on worker threads as soon as a chunk and its neighbors are loaded,
  instead of on the connection of the first player it is sent to, and a chunk is sent once its light is ready
  (`harpas-engine`, `harpas-server`).

- A changed chunk that leaves every view is saved up to five seconds later, together with the other chunks of
  its region that left meanwhile, and a region has one save at a time. Exploring new terrain used to rewrite
  the region file once for each row of chunks left behind (`harpas-engine`).

- The light of a generated chunk is computed once and shared by the connections that send it, until the chunk or
  one of its neighbors changes. Every connection used to compute it again for every chunk it sent, which took far
  longer than the rest of the chunk packet (`harpas-server`).

- Generated chunks stop updating `WORLD_SURFACE_WG` and `OCEAN_FLOOR_WG` once they have run carvers, as vanilla
  does, and heightmaps classify blocks by their motion-blocking flag and fluid instead of a provisional list of
  names (`harpas-worldgen`, `harpas-engine`).

- A generated chunk that arrives after nobody wants it any more is saved instead of discarded
  (`harpas-engine`).

### Fixed

- Iron bars at the edge of an igloo's basement could stay connected to terrain that the igloo replaced, when
  the chunk beside them decorated first. Placing an igloo now reshapes the iron bars beside its chunk, as
  vanilla's template edge updates do (`harpas-worldgen`).

- Java clients receive vanilla's tags in the Configuration state, resolved from the vanilla data at startup:
  661 tags of 15 registries, compared with a vanilla 26.1 server. They were sent empty, so a player under water
  saw no blue fog, and blocks, items, fluids and entities were in none of their tags. The tags of blocks, items,
  fluids and other built-in registries need the registry IDs that the installation generates with Java; for an
  installation made without it, run `harpas vanilla-data generate-registry` (`harpas-java`, `harpas-server`).

- Light under slabs, stairs, snow layers and the other blocks that use their shape for light occlusion: the faces
  their shape covers now stop light, so the ground under a lower slab or stair roof is lit from the side, as in
  vanilla, instead of by direct sky light (`harpas-worldgen`, `harpas-core`).

- Chunks past the edge of a world vanilla generated failed to load, because the chunks
  vanilla left at `initialize_light` could not take their neighbors' decorations. The chunk workers now resume them
  like chunks at `features`, and use chunks from `light` on as saved (`harpas-engine`).

- Proto-chunks saved after decoration keep the blocks that their features and structures marked for
  postprocessing, after the ones marked at carvers (`harpas-engine`).

- Chat that the server does not accept gets vanilla's answers, in the client's language, where it got texts in
  English: a red system message for a message out of order, unsigned in a signing session, wrongly signed,
  after a broken signing chain or signed with an expired key, and vanilla's disconnection reasons for a chat
  key that Mojang did not certify and for a bad acknowledgement (`harpas-java`).
- A chat key that had expired when it arrived no longer disconnects a player who has no session, who is left
  without one, as in vanilla; a player who was signing is disconnected. A player who sends again the key it
  already signs with keeps its signing chain, where it was disconnected for a repeated session
  (`harpas-java`, `harpas-engine`).
- With `enforce-secure-profile=false`, a player without a chat session who sends a signed message has it
  relayed unsigned, as in vanilla, where it was refused; such a player's messages are no longer held to the
  order of their timestamps (`harpas-java`).

- Chat accepts the characters U+0080 to U+009F, as vanilla does; a message with one closed the connection. A
  message with a character vanilla does not allow now disconnects with vanilla's "Illegal characters in chat", and
  a client with chat disabled in its settings gets vanilla's "Chat disabled in client options", both in the
  client's language (`harpas-java`).

- The flight check looks at the blocks around the player, as vanilla does. A survival player who swam, climbed a
  ladder or vine, or stayed in the air next to a block for four seconds was disconnected for flying
  (`harpas-engine`).

- A chunk, `level.dat` or a player's data that inflates to more than 256 MiB, or takes more than 1 GiB of
  memory once read, is refused as unreadable instead of filling the memory, as a crafted file in a downloaded
  world could (`harpas-anvil`, `harpas-core`).

- A panic is written to `logs/latest.log` as an error, with its thread and place, instead of only to the
  terminal. When the logic thread panics, the world is saved before the server stops, and the server reports
  why it stopped (`harpas-engine`, `harpas-server`).

- With an empty `server-ip`, the server also listens for IPv4 on systems whose IPv6 listener takes only IPv6
  connections, as on Windows, where IPv4 clients could not connect (`harpas-server`).

- Players join with their chunk center at the world's spawn instead of chunk 0, 0. With a spawn far from the
  origin, the client discarded the chunks around the player as outside its view (`harpas-java`).

- With `generation.features = true`, decorated chunks saved at `minecraft:features` are sent with light computed
  from their blocks instead of their missing saved light, which left terrain and trees dark in daylight
  (`harpas-server`).

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

### Security

- The connections of an IPv6 address that are not playing yet also count for its /48 and its /32, at most 32
  and 64. One /48, which a single customer of a hosting company often has, could fill all 256 places with one
  connection from each of its /64 prefixes and keep every other player out. A full server now also takes a
  place from the block that holds the most (`harpas-server`).

- A few addresses can no longer keep every other player from connecting by filling the 256 places for
  connections that are not playing yet, which 32 IPv4 addresses could. When the places are full, a new
  connection takes the oldest one of the address that holds the most (`harpas-server`).

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
