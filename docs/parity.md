# Parity with vanilla

Scope: how Harpas checks that it behaves like vanilla, what those checks prove and what they do not, how they run
in CI and locally, and how far each subsystem has come. The behavior itself is covered in the document of each
subsystem, such as [world.md](world.md) and [engine-core.md](engine-core.md).

Status: in progress. Harnesses exist for random generators, noise, world generation, structures and block entity
updates. The comparisons with Mojang's server run in CI: the full sample first passed on 2026-11-04, in about 22
minutes without a build cache, and the reduced sample in about 6. The check `parity against vanilla` is required on `main`. Facts about GitHub Actions and Mojang's files cite their source in
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

There are three levels, from the cheapest to the closest to what a player sees:

| Level | Reference | Harnesses | Runs in |
|---|---|---|---|
| **Java semantics** | Java programs written for the test, using only Java SE classes and no Mojang files | `java_parity` in `harpas-core` and `harpas-worldgen` | CI, every pull request, every platform |
| **Vanilla's classes** | Java probes compiled against the server jar, which call vanilla's own code | `vanilla_parity`, `worldgen_random_parity`, `chunk_parity`, `beardifier_parity`, `structure_placement_parity`, `structure_start_parity`, `template_parity`, `monument_layout_parity`, `monument_exterior_parity`, `monument_interior_parity`; `generated_chunk_parity`, `buried_treasure_parity`, `swamp_hut_parity`, `desert_pyramid_parity`, `jungle_temple_parity`, `igloo_parity`, `shipwreck_parity`, `ocean_ruin_parity`, `mineshaft_parity`, `stronghold_parity` in `harpas-engine` | Parity workflow |
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
| Features | Not started | | |
| Light | Not started | | |
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

It does not run on a schedule. A schedule suits checks whose result changes without any change in the repository,
like the advisories check. These compare pinned code with a pinned version, so a run at night would only repeat
the result of the merge, after the change has landed.

### Locally

`cargo test` skips the comparisons with the reference server when `HARPAS_VANILLA_SERVER_DIR` is not set, and those
tests still report `ok`. A passing `cargo test` says nothing about them. The tests that only need vanilla's files also
accept the pack of a local test server, `test-environment/.harpas/vanilla-data/26.1.zip`, which is git-ignored and so
never in CI.

To run them, download the server to a folder outside the repository. Setting `MOJANG_EULA_ACCEPTED=true` means that
you accept the [Minecraft EULA](https://aka.ms/MinecraftEULA):

```sh
MOJANG_EULA_ACCEPTED=true ci/fetch-vanilla-server.sh 26.1 ~/harpas-vanilla
HARPAS_VANILLA_SERVER_DIR=~/harpas-vanilla cargo test --release --workspace --locked --test '*_parity' --test world_loading
```

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

## Adding a harness

1. Put it in the `tests/` folder of the crate it checks, in a file named `<name>_parity.rs`. **The workflow only runs
   test files with that name**, and fails if another file reads `HARPAS_VANILLA_SERVER_DIR` or calls
   `Reference::from_env`. Read one of them, and without it print why the test is skipped and return. If it takes more
   than a moment, make it compare less with the reduced sample. The same goes for a test that only needs vanilla's
   files: a unit test in `src/` is outside what the workflow selects, so it would skip there and still pass.
   `VanillaData` in `crates/harpas-worldgen/tests/reference/mod.rs` finds the files.
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

1. Whether the full comparison on `main` needs splitting into a matrix. `structure_start_parity` takes about half
   of it, around 11 minutes on a hosted runner.
2. Whether to run the comparisons in debug mode as well, for the overflow checks.
3. The format of the published parity report, and whether a tool generates it.
4. How to compare what a vanilla client sees.

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
