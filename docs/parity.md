# Parity with vanilla

Scope: how Harpas checks that it behaves like vanilla, what those checks prove and what they do not, how they run
in CI and locally, and how far each subsystem has come. The behavior itself is covered in the document of each
subsystem, such as [world.md](world.md) and [engine-core.md](engine-core.md).

Status: in progress. Harnesses exist for random generators, noise, world generation, structures, block entity
updates and light. The comparisons with Mojang's server run in CI: the full sample first passed on 2026-11-04,
in about 22 minutes without a build cache, and the reduced sample in about 6. The check `parity against vanilla` is required on `main`. Facts about GitHub Actions and Mojang's files cite their source in
[References](#references); everything else is a design decision of this project.

---

## Goals

| Goal | Meaning |
|---|---|
| **Vanilla is the specification** | Every behavior is ported one to one and checked against vanilla itself, not against our reading of it |
| **Failures before merging** | A difference from vanilla stops the pull request that causes it, while the change is still in the author's head |
| **Honest coverage** | Passing means what it says: what was compared is written down, and so is what was not |
| **No Mojang files in the repository** | The reference server is used for tests only |

---

## What is compared

The reference is Mojang's **server**. No harness runs the vanilla client, so a harness says that Harpas and the
vanilla server agree, not that a client sees no difference. Checks with real clients are manual so far, for example
the signed chat validation that remains open in [java-connection.md](java-connection.md). Bedrock has no vanilla
server to compare with here; its gameplay parity is described in [bedrock-interop.md](bedrock-interop.md#validation).

There are four levels, from the cheapest to the closest to what a player sees:

| Level | Reference | Harnesses | Runs in |
|---|---|---|---|
| **Java semantics** | Java programs written for the test, using only Java SE classes and no Mojang files | `java_parity` in `harpas-core` and `harpas-worldgen` | CI, every pull request, every platform |
| **Vanilla's classes** | Java probes compiled against the server jar, which call vanilla's own code | `vanilla_parity`, `worldgen_random_parity`, `chunk_parity`, `beardifier_parity`, `structure_placement_parity`, `structure_start_parity`, `template_parity`, `monument_layout_parity`, `monument_exterior_parity`, `monument_interior_parity`, `ore_feature_parity`, `tree_feature_parity`, `bamboo_feature_parity`, `block_blob_feature_parity`, `desert_well_feature_parity`, `coral_feature_parity`, `underwater_magma_parity`, `large_dripstone_parity`, `dripstone_cluster_parity`, `lush_vegetation_parity`, `feature_stage_parity`, `placement_parity`, `block_behavior_parity`, `feature_blocks_parity`; `generated_chunk_parity`, `feature_worker_parity`, `buried_treasure_parity`, `swamp_hut_parity`, `desert_pyramid_parity`, `jungle_temple_parity`, `igloo_parity`, `shipwreck_parity`, `ocean_ruin_parity`, `mineshaft_parity`, `stronghold_parity` in `harpas-engine` | Parity workflow |
| **Vanilla's saved world** | The server itself, generating and lighting a world whose region files the test reads | `light_parity` in `harpas-engine` | Parity workflow |
| **Vanilla on the network** | The server itself, started on loopback and spoken to with the protocol codecs | `block_entity_parity` | Parity workflow |

The first level shows that Rust computes what the same algorithm computes in Java, including `int` overflow and
`StrictMath`. It does not show that the algorithm is the one vanilla uses, which is what the other two levels are
for.

A test can also need vanilla's files without comparing anything. These check Harpas alone, so they say that it can
use vanilla's data, not that the results match vanilla's, and they add nothing to the [coverage](#coverage):

| Test | Checks |
|---|---|
| `datapack_parity` in `harpas-worldgen` | The datapack loads and goes through every stage of generation |
| `world_loading` in `harpas-server` | A real server starts a new world, keeps its seed when reopened, and vanilla opens the world |
| `monument_parity` in `harpas-engine` | Workers place the monument exactly like the direct API, including chunks reached only by the exterior clearing, reversed request order and a region round trip |

They live with the comparisons, and are named like them, so that the Parity workflow runs them.

### How a probe works

1. `Reference::from_env` (`crates/harpas-worldgen/tests/reference/mod.rs`) reads `HARPAS_VANILLA_SERVER_DIR`,
   requires `eula=true` in its `eula.txt`, and unpacks the server jar and its libraries into a temporary folder.
2. The test compiles its probe (`*Probe.java`, next to the test) against them with `javac`, and runs it with `java`.
3. The probe calls vanilla's classes, such as `ChunkGeneratorStructureState`, and writes the results to a file.
   Probes that place configured features load them from the jar's data pack, as a server does, rather than use
   the registries vanilla builds in code (`VanillaRegistries`): the shipped JSON was generated through each
   codec, and a codec can write back another field, as `CherryFoliagePlacer` writes its
   `wide_bottom_layer_hole_chance` as `corner_hole_chance`.
4. The test computes the same results with Harpas and compares them.

---

## Coverage

Each subsystem has a status: not started, partial or matching. Nothing is matching yet. That status needs a
comparison that covers the subsystem's behavior and runs in CI. The figures are those of the full sample.

| Subsystem | Status | Compared | Not compared yet |
|---|---|---|---|
| Random generators, Java math, noise | Partial | Java semantics (`java_parity`); `WorldgenRandom` with legacy and Xoroshiro sources, decoration seeds, bounded draws, floating point and Gaussian draws, including reseeding (`worldgen_random_parity`) | Other vanilla classes directly, beyond what world generation exercises |
| Terrain, aquifers, ore veins, surface, carvers | Partial | Intermediate results, and every block, biome, carving mask, fluid update and heightmap of 1,621 chunks in 5 seeds with all 54 Overworld biomes; additional chunks in 2 seeds around all four structure terrain-adaptation modes | Other seeds; other structure block placement; Nether and End |
| Structures | Partial | Placements, starts, pieces and references of the structure types made so far, including ocean monuments' saved building piece; 3,237 monument internal layouts with room connections, piece order, all eleven piece types, rotated bounds, designs, random consumption and reconstruction from saved records; monument exterior blocks and all six final-block heightmaps in 13,160 chunk placements and replays with outer wings, entrance, facade, roof, stepped walls, clearing and foundations, four orientations, all sixteen chunk offsets, mixed terrain, world limits, sea levels and saved-record pass replays (internal child rooms excluded); monument internal blocks, six heightmaps and decoration random tails in 122,200 isolated-room and complete-monument chunk placements and replays, all eleven piece types, all designs and orientations, chunk edges, mixed terrain, world limits, sea levels, vertical clipping and saved-record replays (elder spawning suppressed), plus the worker stream (index 6, step 4) over 3,200 complete passes and replays; saved NBT and vanilla reloads of up to eight starts per structure and seed; all 1,202 templates and 188 pools; 4,160,772 terrain-adaptation density samples; worker terrain and saved structure records around all four adapting modes; buried treasure blocks, heightmaps, chest NBT and saved starts in 38 synthetic cases and 8 actual chunks across 2 seeds, Anvil reloads and repeated placement after vanilla reloads the treasure records; swamp hut blocks, all six heightmaps, saved starts and fence postprocessing in 92 synthetic cases and 4 worker chunks across 2 seeds, four-chunk pieces, a saved neighbor, Anvil reloads and repeated placement after vanilla reloads the records (entity spawning suppressed, original flags preserved); desert pyramid blocks, all six heightmaps, chest/brushable NBT and saved starts in 160 synthetic chunks across 2 seeds and their replays after vanilla reloads Harpas records, plus 8 natural worker chunks across 2 seeds, Anvil reloads, repeated pyramid placement and a saved neighbor with modified terrain (source-first height schedule; worker chest flags deferred while at carvers); jungle temple blocks, all six heightmaps, chest/dispenser NBT, saved starts and postprocessing in 208 synthetic chunks across 2 seeds and their replays after vanilla reloads Harpas records, plus 2 natural worker chunks, worker-pass replays, Anvil reloads and a saved neighbor (worker container flags deferred while at carvers); igloo blocks, all six heightmaps, block entity NBT and saved records in 126 synthetic chunks across 2 seeds and their replays after vanilla reloads Harpas records, four natural igloos with and without basements, worker-pass replays, Anvil reloads and a saved neighbor with an altered entrance height (repeating the igloo pass at carvers can change its height, as in vanilla; entity spawning deferred to M4); shipwreck blocks, all six heightmaps, chest NBT, saved records and postprocessing in 656 synthetic chunks across 2 seeds and their replays, all 20 templates, all rotations, waterlogging and shape updates, five natural shipwrecks across 2 seeds, reversed worker requests with a shared source-first height, Anvil reloads, worker-pass replays, two saved-neighbor cases (altered terrain and a retained adjusted origin) and a chunk shared with buried treasure; cold and warm ocean ruin blocks, all six heightmaps, chest and brushable NBT, saved records and postprocessing for all 48 templates and rotations, slopes, coasts, ice, waterlogged terrain and world limits, plus fitting/metadata over kelp (neighboring plant-to-tip conversions deferred), including vanilla placement replays, natural worker chunks, reversed requests, saved neighbors and Anvil reloads (drowned spawning suppressed); Overworld ruined portal blocks, all six heightmaps, chest NBT, saved records and postprocessing for all 13 templates, rotations and mirrors, positional processors, water/lava, spread and drip columns, vines/leaves, slopes, burial and world limits, representative template replays, natural workers across 2 seeds, reversed requests, altered saved neighbors and Anvil reloads; normal and mesa mineshaft blocks, all six heightmaps, spawner NBT, saved records and postprocessing in 558 synthetic chunks and replays, including world limits, partial floors, liquid shells, blocked biomes and shared decoration streams in native reference-set order, plus natural workers across three seeds, a chunk shared with buried treasure, reversed requests, saved neighbor biomes and Anvil reloads; stronghold blocks, six final-block heightmaps, chest/spawner NBT, records and postprocessing in 5,472 placements and replays for all piece types, door types, orientations and room variants, including air, liquids, caves, chunk edges, world limits and saved-record pass replays, plus 18 natural worker chunks across two seeds, reversed requests, a saved glass neighbor and Anvil reloads | Other structure types; other structure block placement; full feature-stage resumption |
| Block entity updates | Partial | All 49 types and representative update data, on the network | Live changes |
| Features | Partial | The vein ore feature's blocks and random state, compared with vanilla's own `OreFeature` on the same Xoroshiro decoration source for buried and surface veins, sizes 9 and 15 and discard chances 0 and 0.5 (`ore_feature_parity`); the global feature order of all 11 generation steps (`feature_stage_parity`); every block that the placed features Harpas places write into the 3x3 chunks around 42 chunks in 3 seeds, ores, simple vegetation, disks, block columns, the trees of groves, rivers, swamps, meadows and mangrove swamps, the rooted azaleas above lush caves, bamboo stalks with podzol patches in bamboo jungles, mossy cobblestone forest rocks in old growth taigas, desert wells, warm ocean coral reefs with plants, fans and sea pickles, underwater magma deposits, surface snow and ice, the seagrass, kelp and sea pickles of oceans, rivers and swamps, vines and glow lichen, including lichen spread to more faces and waterlogged lichen, the ice spikes, packed ice icebergs with snow and blue ice of frozen biomes, and the water and lava springs and the underground and surface lava lakes, compared with vanilla's own `applyBiomeDecoration` on the same carved terrain with every other placed feature replaced by a no-op at its index (`feature_blocks_parity`); the blocks, random state and block entities of 198 trees, fallen trees and huge mushrooms placed one after another in each of 9 carved chunks planted with grass, tall grass, poppies and snowy grass, cycling through oak, fancy oak, birch, spruce and pine trees, trees with bee nests, trunk and leaf vines, cocoa and leaf litter, the five fallen trees with stump vines and log mushrooms, acacias, jungle bushes, mega jungle trees, mega pines and spruces with podzol, dark oaks, cherries, huge brown and red mushrooms, mangroves with roots and propagules, pale oaks with pale moss patches, carpets, hanging moss and creaking hearts, and azaleas with bending trunks, each placed at least once, compared with vanilla's own features (`tree_feature_parity`); bamboo blocks, results and random consumption in 243 cases across 3 seeds, with uneven ground, podzol probabilities 0, 0.2 and 1, every obstructed height, unsupported and occupied origins, chunk borders and world limits, plus bamboo and bamboo shoot survival over all 29,873 supporting block states and 780 shape updates with all stalk states and directions, shoots, older neighbors and missing supports (`bamboo_feature_parity`); forest rock blocks, results and random consumption in 960 placements across 3 seeds with both legacy and Xoroshiro sources, mixed supports, configurable states, offset and always-true/false support predicates, chunk borders and world limits (`block_blob_feature_parity`); every position the placement modifiers of each Overworld placed feature yield around 18 chunks in 3 seeds, compared with vanilla's own modifiers on the same carved terrain and seeds, for all of them, with 12 more chunks of the last seed in deserts, swamps, forests, cherry groves and other biomes where survival rules decide (`placement_parity`); the solid, replaceable, liquid, motion-blocking, solid render and block entity flags and full support and occluding faces of all 29,873 block states (`block_behavior_parity`); blocks, four live heightmaps, block entity NBT, results and random tails of 320 desert well placements across 5 seeds with legacy and Xoroshiro sources, including partial foundations, chunk edges and world limits (`desert_well_feature_parity`); blocks, four live heightmaps, results and random tails of 468 coral tree, claw and mushroom placements across 3 seeds with both legacy and Xoroshiro sources, obstructions, existing vegetation, flowing water, chunk borders and world limits (`coral_feature_parity`); blocks, four live heightmaps, results and random tails of 216 underwater magma placements across 3 seeds with both random sources, floor-search boundaries, mixed and partial supports, flowing water, existing deposits, chunk edges and world limits, plus all 29,873 block states as the destination and each of its five closing neighbors (`underwater_magma_parity`); surface snow and ice, four live heightmaps, results and random tails in 1,170 placements and replays across all 65 biomes and nine heights, with mixed supports, plants, water, emitting blocks, chunk borders and world limits, plus snow survival over all 29,873 block states (`freeze_top_layer_parity`); small stalactites and stalagmites, base patches, four live heightmaps, results and random tails in 1,080 placements across three seeds and both random sources, with six configurations, wet and blocked origins, mixed supports, chunk borders and world limits, plus all 29,873 block states as floor, ceiling, origin and possible second block (`pointed_dripstone_parity`); large dripstone profiles, blocks, four live heightmaps, results and random tails in 1,248 placements across three seeds and both random sources, with eight configurations, dry and flooded caves, search boundaries, irregular supports, shrinking bases, shared wind, obstacles, chunk borders and world limits (`large_dripstone_parity`); dripstone cluster pools, layered bases, paired columns, merged tips and waterlogging, four live heightmaps, results and random tails in 3,072 placements and replays across three seeds and both random sources, with eight configurations, dry and flooded caves, missing and irregular boundaries, existing dripstone, search boundaries, obstacles, chunk edges and world limits (`dripstone_cluster_parity`); monster room blocks, four live heightmaps, full chest and spawner NBT, results and random tails in 576 placements and replays across three seeds and both random sources, with opening counts, partial and non-opaque supports, liquids, existing containers and spawners, chunk edges and world limits (`monster_room_parity`); clay patches and pools with small and big dripleaves, four live heightmaps, results and random tails across three seeds and both random sources, including irregular and flooded floors, partial supports, ceilings, repeated placement, chunk edges and world limits, plus dripleaf survival and shape updates against all 29,873 states (`lush_vegetation_parity`); final chunks from the chunk workers, 13 chunks in 2 seeds, including a bamboo jungle, an old growth pine taiga, a desert well crossing a chunk edge, a warm ocean reef, an underwater magma deposit crossing a chunk edge, snowy plains, a frozen river, two dripstone caves with long cluster columns and a lush cave with clay pools, both dripleaves and a monster room crossing a chunk edge, compared with vanilla decorating their 3x3 chunks in the workers' order on 5x5 carved chunks, then reloaded unchanged from Anvil, including the well's archaeology metadata and monster room chest and spawner NBT, the latter also compared against vanilla (`feature_worker_parity`) | The other feature types; ticks scheduled by shape updates, springs and lakes; scattered ore (ancient debris); modifiers used only in the Nether and the End; postprocessing marks of decorated chunks; the structures of each step; other decoration orders between neighboring chunks, including mushrooms in a neighbor that has already initialized its light |
| Light | Partial | Sky and block light of three generated chunks in one seed, every saved layer and all 4,096 values of each, compared with vanilla's own saved chunks after generating a 7×7 area (`light_parity`); synthetic unit cases for sky, water dimming, block light, chunk borders and nibble order | Light after block changes and queued updates; other seeds, world limits and dimensions; saving light at the `light` stage, which waits for features |
| Chat signing | Partial | What the server answers to 23 sequences of chat sessions and messages from one online player, with `enforce-secure-profile` on and off: the disconnection, the refusal sent as a system message, the message relayed signed or unsigned, and the session published or withdrawn. They cover valid messages, messages out of order, unsigned, wrongly signed and after a broken chain, keys that are not certified, expired or repeated, a second session with the same key and with a new one, bad acknowledgements and offsets, and messages ten minutes old or ahead. The vanilla server trusts a key made for the test, served by a stand-in for Mojang's services (`chat_parity`) | What other players receive; commands; acknowledgements of messages actually received; the chat spam limit; a real client |
| Saved worlds | Not started | | The round trip from vanilla through Harpas back to vanilla |
| Block behavior and redstone | Not started | | Scripted circuits, tick by tick |
| Entities and mobs | Not started | | Movement, collisions, AI, spawning |
| Items, inventories, crafting, loot | Not started | | |

The order of work follows the difficulty of comparing: world generation first, then blocks and redstone, then
entities, then items. [world.md](world.md#world-generation) has the details of each world generation harness.

This table is the base of the parity report published with every release ([releases.md](releases.md#release-process)).

### Known differences

- **Biome ties.** A climate point can be equally close to several parameter points, and vanilla then keeps the last
  result of its worker thread. Harpas starts each search without one, and the harnesses make vanilla do the same, so
  a tie may resolve differently in a real vanilla world ([world.md](world.md#world-generation)).
- **Pyramid height scheduling.** Worker comparisons process the source chunk first when choosing the shared
  pyramid height. Harpas fixes that order for deterministic burial offsets; a vanilla world that starts with a
  different intersecting chunk can choose another offset. Saved heights are retained. Comparisons of pyramid-pass
  replay do not cover the complete feature stage.
- **Shipwreck height scheduling.** Workers choose and share each shipwreck's height with a source-first
  schedule, retaining saved adjusted origins. Comparisons use the same schedule and verify reversed worker
  request order. A vanilla world that first places another intersecting chunk can choose a different beached
  burial offset and loot seeds. Pass replays do not cover the complete FEATURES stage.
- **Mineshaft scheduling.** New chunks share each structure type's decoration stream in vanilla's reference-set
  order. Workers preserve incoming spawner flags while saved at `carvers`; direct placement updates them as
  vanilla does. Chest minecart entities are constructed and discarded by the block-placement probe, retaining
  their real rail writes and loot-seed draws. Entity persistence and ticking follow in M4. Replays cover each
  mineshaft pass, not the full FEATURES schedule or its resumption after a vanilla proto-chunk reload.
- **Stronghold scheduling.** Each start's intersecting pieces share its decoration stream. Workers retain
  incoming `Chest` and `Mob` flags while saved at `carvers`; direct placement updates them, and replays can
  replace masonry, containers and portal eyes as in vanilla. The probe compares vanilla's live feature-stage
  heightmaps, with final-block WG equivalents, rather than re-priming already populated maps. Natural worker
  and saved-neighbor comparisons do not establish a full FEATURES-stage round trip or spawner ticking.
- **Ocean ruin scheduling.** Pieces fit their heights on each pass, reading pre-placement neighboring
  terrain and earlier layers placed within the target chunk. Workers prefer saved neighbors. Comparisons
  use the same bounded terrain view; they do not cover a complete FEATURES stage or drowned spawning.
  Neighboring kelp plants can become randomized tips in vanilla template edge updates; Harpas defers those
  plant shape updates to gameplay. The explicit kelp cases compare fitted records, heightmaps, entities and
  every other block, allowing only those plant-to-tip conversions.
- **Ruined portal scheduling.** Workers share a complete portal pass seeded by the chunk containing its
  piece center, including writes outside the saved piece bounds. Comparisons read the same carved or
  saved neighboring terrain and check request order and individual pass replays. They do not cover the
  complete FEATURES schedule or resumption; Nether placement remains with that dimension.
- **Feature stage coverage.** Ores, simple vegetation, mushrooms, disks, block columns, trees with straight, fancy,
  forking, giant, mega jungle, dark oak, cherry, upwards branching and bending trunks, mangrove roots and their vines, cocoa,
  bee nests, leaf litter, podzol, propagules, pale moss and creaking hearts, fallen trees, huge mushrooms and
  vegetation patches, lush cave clay pools with small and big dripleaves, the root systems of rooted azaleas, bamboo with podzol patches, forest rocks, desert wells with archaeology metadata, warm ocean coral reefs, underwater magma deposits, surface snow and ice, small stalactites and stalagmites with spreading base patches, large dripstone columns with embedded bases and wind offsets, monster rooms with loot chests and saved spawner species, seagrass, kelp, sea pickles, vines, glow lichen, ice spikes, icebergs, blue ice, lava lakes and springs are placed; the other families are not, nor the ticks that lakes and springs schedule or spawner ticking and loot opening. Mushrooms survive as if no column were lit, as in the chunk being
  decorated; a neighbor that vanilla has already lit can read other light. Bee nests, desert wells' suspicious
  sand and monster rooms' chests and spawners carry their block entity data; other blocks with block entities that features place carry vanilla's
  `DUMMY` placeholder, as a proto-chunk saves it. The scheduler skips feature
  types it does not port, so the workers run the stage only with `generation.features = true`; otherwise
  generated chunks stop at `carvers` and vanilla finishes the stage. `feature_blocks_parity` decorates one chunk at a time on terrain without structures.
  `feature_worker_parity` covers the nine passes that write into a final chunk, but in one order, without
  structures, and without the blocks of unported families that other features would read or write over.
  Postprocessing marks and saved heightmaps of decorated chunks are not compared block by block.
- **Heightmaps of gameplay blocks.** Predicates are compared for terrain and carver blocks; the classification of
  other blocks is provisional until block updates exist.

---

## Running the comparisons

### In CI

The Parity workflow (`.github/workflows/parity.yml`) runs:

- on every pull request that changes `crates/`, `ci/`, `Cargo.toml`, `Cargo.lock` or the workflow itself;
- on every push to `main`, whatever changed;
- by hand, with another Minecraft version as input.

Pull requests compare a **reduced sample**, one seed and smaller areas, set by `HARPAS_PARITY_SAMPLE=reduced`, for a
quick answer. Every other run compares the **full sample**, so a difference that only the full sample shows is found on
`main` after merging.

The harnesses run with [cargo-nextest](https://nexte.st) in four shards at once, each on its own runner: every
shard builds all the harnesses and runs a quarter of the tests (`--partition slice`). The `parity` profile in
`.config/nextest.toml` keeps running after a failure, so one run reports every difference. A test that compares
for minutes is better split, as `structure_start_parity` is into one test per seed, so the shards can share it out.
The check `parity against vanilla` passes when every shard passes, or when the pull request changes nothing that
is compared.

When the full sample fails on `main`, the workflow opens the issue "Parity against vanilla fails on main", comments
on it on each later failure and closes it on the next push that passes.

It does not run on a schedule. A schedule suits checks whose result changes without any change in the repository,
like the advisories check. These compare pinned code with a pinned version, so a run at night would only repeat
the result of the merge, after the change has landed.

### Locally

The tests that compare with the reference server, or need vanilla's files, are marked `#[ignore]`: `cargo test` and
`cargo nextest run` report them as ignored or skipped instead of `ok`, so a passing run does not claim them. Run on
purpose without `HARPAS_VANILLA_SERVER_DIR`, they fail. The tests that only need vanilla's files also accept the
pack of a local test server, `test-environment/.harpas/vanilla-data/26.1.zip`, which is git-ignored and so never in
CI.

To run them, download the server to a folder outside the repository. Setting `MOJANG_EULA_ACCEPTED=true` means that
you accept the [Minecraft EULA](https://aka.ms/MinecraftEULA):

```sh
MOJANG_EULA_ACCEPTED=true ci/fetch-vanilla-server.sh 26.1 ~/harpas-vanilla
HARPAS_VANILLA_SERVER_DIR=~/harpas-vanilla cargo test --release --workspace --locked --test '*_parity' --test world_loading -- --include-ignored
```

`cargo nextest run --profile parity`, with `--run-ignored all` in place of `-- --include-ignored`, runs the test
binaries at once, as CI does.

This compares the full sample. Use `-p <crate> --test <harness>` in place of `--workspace` and the patterns to run one
harness, and set `HARPAS_PARITY_SAMPLE=reduced` for the sample of pull requests. Java 25 must be on `PATH`.

### Mojang EULA

Mojang's server is subject to the Minecraft EULA. The server jar is never committed, cached, uploaded as an artifact
or shipped in a release ([world.md](world.md#mojang-eula) covers how server owners accept it).

Maintainers accept it for CI by setting the repository variable `MOJANG_EULA_ACCEPTED` to `true`. Without the
variable the job **fails**: a skipped job reports success [1], so skipping would make a comparison that never ran
look like one that passed.

The exception is pull requests from forks, which do not appear to receive repository variables [2]. They skip, and
are compared on the push to `main` after merging. The alternative, `pull_request_target`, runs with the base
repository's permissions next to untrusted code, which is not worth it for a check that runs again after the merge.

---

### A vanilla server without Mojang's services

A vanilla server in online mode asks Mojang which keys certify the players' chat keys, and whether a player who
joins has logged in. `chat_parity` starts the server with the system properties `minecraft.api.auth.host`,
`minecraft.api.account.host`, `minecraft.api.session.host`, `minecraft.api.services.host` and
`minecraft.api.profiles.host` set to an HTTP server of the test, which answers `/publickeys` with a key made for
the test and `hasJoined` with the profile of the test's player. The server logs the hosts it uses as its
`Environment`. No request leaves the machine and no Mojang account is involved, and the key is trusted only by
that server process.

---

## Adding a harness

1. Put it in the `tests/` folder of the crate it checks, in a file named `<name>_parity.rs`. **The workflow only runs
   test files with that name**, and fails if another file reads `HARPAS_VANILLA_SERVER_DIR` or calls
   `Reference::from_env`, `Reference::required` or `VanillaData`. Get the server with `Reference::required()` and
   mark the test `#[ignore = "needs Mojang's server: docs/parity.md#locally"]`, so that it is reported as skipped
   where there is no server and fails if it is run without one. If it takes more than a moment, make it compare
   less with the reduced sample. The same goes for a test that only needs vanilla's files, which
   `VanillaData::required()` in `crates/harpas-worldgen/tests/reference/mod.rs` finds: a unit test in `src/` is
   outside what the workflow selects, so it would never run.
2. Make the vanilla side repeatable. When vanilla depends on something like its thread history, the probe fixes that
   state and the document says so, as with [biome ties](#known-differences).
3. **Prove that it can fail.** Break the Harpas side on purpose and check that the harness notices, and say so in the
   pull request. A comparison of bounding boxes once passed with the wrong spawner template in place, and only
   comparing the elements of the pieces caught it.
4. Update the [coverage](#coverage) table, including what the harness does not compare.

---

## Trade-offs

- **Reduced sample on pull requests.** The full comparison is too slow to wait for on every pull request, so they
  compare less and `main` compares everything. Some differences are therefore found just after merging, on a known
  commit, rather than before it. Documentation changes skip the comparison.
- **Sampling, not proof.** Harnesses compare chosen seeds and areas. Passing does not establish parity for every seed.
- **Release mode.** The workflow runs in release mode for speed, so integer overflow does not panic there. The
  debug `cargo test` in CI covers that, but only for tests that do not need the reference server.

---

## Open questions

1. Whether to run the comparisons in debug mode as well, for the overflow checks.
2. The format of the published parity report, and whether a tool generates it.
3. How to compare what a vanilla client sees.

---

## Validation

1. **First run:** the workflow passes on a hosted runner, with every harness comparing rather than skipping. Done on
   2026-11-04.
2. **Required check:** `parity against vanilla` is required on `main`. Done on 2026-11-04.
3. **Failure:** a deliberate difference in a pull request fails the check.

---

## References

1. GitHub Docs, "Using conditions to control job execution" (a job skipped by a condition reports success).
   https://docs.github.com/en/actions/using-jobs/using-conditions-to-control-job-execution
2. GitHub Community, "Pass Configuration Variables to workflows triggered by a pull request from a fork". The official
   documentation does not settle this, so the workflow works either way.
   https://github.com/orgs/community/discussions/44322
3. Minecraft End User License Agreement. https://aka.ms/MinecraftEULA
4. Mojang version manifest, with each version's server download and hash.
   https://piston-meta.mojang.com/mc/game/version_manifest_v2.json
