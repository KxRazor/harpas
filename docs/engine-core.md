# Engine core

Scope: how the Rust engine runs the game in normal mode: the tick loop and its order, which work runs in
parallel, how threads are laid out, how the world and entities are stored, and how plugins are called from
the tick. Modded mode uses Mojang's server instead and is covered in [mod-support.md](mod-support.md).

Status: in progress. The logic thread, tick skeleton, players, chunk sections, loading and sending existing
overworld chunks, asynchronous chunk saving, and vanilla's deterministic random generators, Java math helpers
and gradient/simplex noise samplers are implemented. World generation, live block changes, saving other world
and player data, and the remaining entity logic are still in progress. Facts about vanilla cite their source in
[References](#references); everything else is a design decision of this project.

---

## Goals

In priority order. When two goals conflict, the higher one wins.

| # | Goal | Meaning |
|---|---|---|
| 1 | **Vanilla parity** | Game logic runs in the same order and with the same results as vanilla: redstone, farms and contraptions behave the same |
| 2 | **Plugin compatibility** | Bukkit/Paper plugins see the same synchronous events and the same main-thread semantics as on Paper |
| 3 | **Performance** | Everything that cannot be observed runs in parallel and off the logic thread |

### Non-goals

- **Optimizations that change vanilla behavior.** Paper and Spigot offer optional optimizations that alter
  game behavior (for example, entity activation ranges that skip ticking distant entities). Harpas does not
  apply them: they would break parity. Owners migrating from Paper are told so by the one-time migration notice
  ([operations.md](operations.md#configuration-files)).

---

## Decisions

### 1. Vanilla tick order on one logic thread

Game logic runs on **one logic thread**, in vanilla's order. Vanilla targets 20 ticks per second (one tick
every 50 ms) and, when it falls more than two seconds behind, resets its next-tick target instead of running
extra ticks [1]. Each server tick runs [1]:

1. Functions tagged to run every tick.
   Plugin scheduled tasks run at the point of the tick where Paper runs its scheduler *(verify against Paper)*.
2. Each dimension, in a fixed order (Overworld, the Nether, the End, then custom dimensions):
   - world border, weather, daylight cycle, sleeping, scheduled commands;
   - scheduled block ticks, then scheduled fluid ticks;
   - raids, chunk load levels;
   - every chunk, in random order: mob spawning, ice and snow, random ticks;
   - sending block changes, points of interest, unloading chunks;
   - block events, the dragon fight;
   - every non-passenger entity in order, with its passengers;
   - block entities, game events.
3. Incoming and outgoing packets for each player, then player info.
4. Autosave every 6,000 ticks.

This order is observable (redstone timing, farms, entity interactions), so Harpas keeps it exactly.

**Implemented skeleton:** the server starts one logic thread with a 50 ms tick target. It catches up after
short delays and resets the target after a delay over two seconds. The tick has phases for functions,
dimensions, connections and autosave, in that order; autosave is scheduled every 6,000 ticks. The dimension
phase applies completed chunk reads and saves, and schedules unloading; the autosave phase queues immutable
chunk snapshots without waiting for disk. Connection and player work is implemented. The other phases remain
placeholders.

Connection threads queue entry, departure, movement and verified chat for the connection phase, and translate the resulting
messages into edition-specific packets. The logic thread owns each player's profile and position and publishes
the online count. Player entities have distinct runtime IDs; a duplicate UUID disconnects the old session.
All players appear in the player list, while entity tracking is per viewer, capped by both its view distance
and 128 blocks. Spectator entities are hidden from non-spectators. A departure removes the entity and list entry
even after an I/O or protocol error. Authentication and Keep Alive remain on the connection threads. A player
whose connection has 60,000 messages waiting leaves in the next connection phase
([java-connection.md](java-connection.md#protection-against-abuse)).

Movement currently uses the void-world checks even in loaded chunks: it rejects non-finite positions or angles,
clamps coordinates to vanilla's limits [3], and corrects a displacement over 10 blocks from the beginning of a tick. A pending teleport must be confirmed
before more movement is accepted. In Survival and Adventure, a player hovering in the void for more than 80
ticks is disconnected unless `allow-flight` is enabled; Creative and Spectator may fly [4]. These are the
current void-world checks, not a complete implementation of vanilla's movement and collision rules: world
collision, vehicle and elytra movement, velocity effects and their flight exemptions need the later world and
gameplay work. The logic thread publishes chat sessions before messages and gives late joiners the current
sessions; verification and each recipient's acknowledgement window stay on the connection thread
([java-connection.md](java-connection.md#chat-signing)).

### 2. Parallel only where it cannot be observed

Running regions of the world in parallel, as Folia does, scales further but changes behavior: its README
expects nearly every plugin to need changes and lists broken areas such as the scoreboard API, portals and
respawning, world loading, and `Entity#teleport` [2]. That conflicts with goals 1 and 2, so Harpas does not tick
regions in parallel.

Work that cannot change the result of the tick runs in parallel, off the logic thread:

| Parallel work | Why it is unobservable |
|---|---|
| Networking: Java TCP and Bedrock NetherNet I/O | Packets are handled at the tick's packet step, in order |
| Packet encoding, compression and encryption | Encodes state the tick has already produced |
| Bedrock translation | Reads state after it is produced, like encoding |
| Chunk loading, saving and generation | Generation is deterministic from the seed; chunks join the world at the point vanilla would load them |
| Lighting | Results are applied at defined points, as vanilla's light engine applies queued updates *(saved light verified by `light_parity`; update timing still to verify)* |
| Entity tracking (which players see which entities) | Computes what to send, not what happens |
| Precomputation on a snapshot (for example, path searches) | Only if the result is identical to computing it on the logic thread at that point *(verify per case)* |

### 3. Thread layout

| Thread(s) | Work |
|---|---|
| **Logic thread** | The tick in vanilla order; Bukkit's main thread; synchronous plugin events and scheduled tasks |
| **Network runtime** | Java and Bedrock connections, framing, encryption |
| **Worker pool** | Chunk generation, loading and saving, lighting, encoding and compression, Bedrock translation, entity tracking |
| **Plugin async threads** | Bukkit async tasks (Java) and asynchronous I/O for WASM plugins |

The logic thread is attached to the JVM at startup when Java plugins are present ([jvm.md](jvm.md#threads)).

The current chunk loader uses a bounded queue and two workers for every three hardware threads, at
most 16: 8 on a machine with 12. Generating a view of 441 chunks on such a machine (6 cores) went from 71
chunks a second with 4 workers to 81 with 8 and 83 with 12; from 10 workers on, the logic thread waited up to
5 ms for a processor. With the features stage, whose workers share the chunks around the one each decorates,
the same view generates at 15 chunks a second with 1 worker, 45 with 4, 57 with 8 and 62 with 12: a worker that finds
a chunk in another worker's hands generates or decorates the others it needs meanwhile, and waits only when
it has nothing else to do. Limited to 4 hardware threads, 2
workers generated as fast as 4. The logic thread requests each player's
view, interleaving viewers and prioritizing nearby chunks, and applies completed reads in the dimension phase.
A player's view is computed again only when the player enters another chunk or changes its view distance, and
the chunks the world keeps loaded only when some view changed or a player joined or left.
Shared requests are deduplicated; results no longer needed are discarded, except that discovering an
incompatible chunk still stops the server. Java connections encode chunks in batches of at most eight, wait for
the client's acknowledgement and honor its requested chunk rate; the Java configuration registry supplies the biome ID order. Missing
chunks remain void until generation exists, and block entity updates are still to come.

### 4. Plugins are called from inside the tick

Plugin events are dispatched **synchronously, at the point of the tick where they happen**, on the logic
thread: a cancellable event is decided before the action, and plugin writes are applied immediately. Events
with no listener are skipped by interest masks. Details: [jvm.md](jvm.md#principle-synchronous-events-paid-only-when-someone-listens)
for Java plugins and [plugin-api.md](plugin-api.md#threading) for WASM plugins.

### 5. Storage shaped like vanilla, not a general-purpose ECS

Vanilla ticks each entity completely, one after another, in the order of the world's entity list. A
general-purpose ECS was considered and set aside: it does not guarantee iteration order, its default storage
moves an entity's data when components are added or removed (shared views need stable addresses), and turning
vanilla's class hierarchy of entity types into components would be a redesign that puts parity at risk.

Harpas uses its own storage:

- **Entities mirror vanilla's classes.** Each entity type is a Rust type with the same fields and behavior as
  its vanilla counterpart, ported one to one so that parity tests can compare them directly.
- **One ordered entity list per world**, in vanilla's order, drives the entity tick.
- **Stable slots.** Entities live in generational slots and never move; a slot index plus a generation is the
  entity's handle for the engine, plugins and the bridges.
- **Hot data in per-field arrays.** Position, velocity, bounding box, health, chunk section and change flags are
  stored in arrays indexed by slot. These arrays **are** the shared views read by the JVM bridge and WASM host,
  so there is no copy.
- **Parallel passes over the hot arrays** for unobservable work: entity tracking, packet encoding, Bedrock
  translation.

### 6. Chunks

- Sections of 16×16×16 blocks stored as **paletted containers**, the same structure vanilla uses
  (`harpas_core::chunk`). A section holds 4,096 block states and 64 biomes, one per 4×4×4 blocks, both indexed
  Y, then Z, then X [5]:
  - **In memory as in vanilla:** one value with no data, a palette with packed indices, or global IDs. Block
    state palettes use 4 bits per index from the second value, then 5 to 8; biome palettes use 1 to 3. Past
    that, the container stores global IDs with as many bits as the registry needs. Indices are packed into
    longs, lowest bits first, never across two longs. The palette grows as values are set and never shrinks.
  - **Java chunk packets carry this form as it is**, so encoding a section copies its containers [6].
  - **Anvil files use a compacted form** [5]: a palette of the values in use, in order of first use, always kept
    even past 8 bits; at least 4 bits per block state index; no data when there is one value. Reading checks
    the data length and every index, as vanilla does.
- A **version per section** that changes on every change. Versions come from one process-wide counter, so a
  section that is unloaded and loaded again never takes a version a cache already holds. The shared encoding
  of Java and Bedrock packets caches encoded sections by version
  ([protocol-codecs.md](protocol-codecs.md#java-wire-format),
  [bedrock-interop.md](bedrock-interop.md#1-shared-chunk-encoding)).
- Block states are global IDs from the **block state registry** (`harpas_core::block`). Blocks are defined by
  the game's code, so the registry is fixed for a version: built from minecraft-data's `blocks.json`, it names
  each state by its block and property values, in vanilla's order. Biomes are global IDs from the biome
  registry the server loads, since data packs can add biomes.
- The counts vanilla keeps per section (non-air blocks, which Java chunk packets carry, and ticking blocks
  and fluids) arrive with the block behavior that defines which blocks tick.
- Block entities in a per-chunk map keyed by position.

### 7. Determinism

- **Random number generators identical to vanilla's** (`harpas_core::random`), bit for bit, and consumed in
  the same order:
  - Java's 48-bit linear congruential generator [7] (`LegacyRandom`) and Blackman and Vigna's
    `xoroshiro128++` 1.0 [8] (`XoroshiroRandom`), seeded through silver/golden ratio expansion, Stafford's
    `Mix13` avalanche mixer and UTF-8 MD5 digests [9], with Lemire's bounded 32-bit interval draw on
    `XoroshiroRandom` and Marsaglia's polar Gaussian on both;
  - positional factories (`at(x, y, z)`, `from_hash_of`, `from_seed`), persisted named sequences
    (`RandomSequence`), and `WorldgenRandom` seeding for chunk decorations, placed features, carvers,
    regional structures and slime chunks [9].
    `WorldgenRandom` samples bits as `BitRandomSource` does, even with a Xoroshiro source: in particular its
    `nextLong` combines two high-32-bit draws. Its methods and decoration seeds are compared directly with
    vanilla's wrapper by `worldgen_random_parity`.
- **Java's math semantics** (`harpas_core::math`) where results differ from Rust's defaults: wrapping
  `MIN / -1` division, `Math.floorDiv` and `Math.floorMod` with negative divisors [7], masked shift counts
  and logical `>>>`, `Math.round` ties toward `+∞` without `x + 0.5` double-rounding [7], a cross-platform
  1-ULP natural logarithm, Java-ordered interpolation and `Mth` floor/ceil/angle helpers, vanilla's
  65,536-entry `f32` sine table, `position_seed` and UTF-16 `String.hashCode`.
- **Gradient and simplex noise samplers identical to vanilla's** (`harpas_core::noise`): Ken Perlin's 2002
  3D improved gradient noise [10] (`ImprovedNoise`, with `(double) 1.0e-7f` vertical step quantization),
  multi-octave `PerlinNoise` (preserving zero-amplitude entries in the `2^(N-1) / (2^N - 1)` normalization
  factor, `2^25` coordinate wrapping, modern `"octave_<k>"` positional seeding and legacy sequential
  `consume_count(262)` seeding), paired `NormalNoise` (`337 / 331` frequency ratio and `1 / 6` target
  deviation), three-sampler `BlendedNoise` (`684.412` coordinate scale and smeared limit/main octaves), and
  2D/3D `SimplexNoise` and `PerlinSimplexNoise` [11].
- Parallel work never consumes the logic's random sequences.

---

## Open questions

1. **Dimensions in parallel.** Vanilla ticks dimensions one after another. Ticking them in parallel helps only
   if interactions between dimensions (portals, scoreboards, global state, commands) can be deferred to a
   point where the difference cannot be observed.
2. Which precomputations on snapshots are provably identical to computing them on the logic thread.

---

## Validation

1. **Parity harnesses:** scripted scenarios (redstone circuits, farms, mob behavior, world generation) run on
   vanilla and on Harpas, comparing results tick by tick.
2. **Tick budget:** time per tick (mean and 99th percentile) with 50, 200 and 500 players, split by phase and by
   logic thread vs. parallel work.
3. **Plugin overhead:** time spent in synchronous event dispatch per tick with a set of real plugins.
4. **Scaling of parallel work:** throughput of chunk generation, encoding and Bedrock translation as worker
   threads are added.

---

## References

1. minecraft.wiki, "Tick" (Java Edition tick order, 20 TPS target, two-second reset, random ticks). https://minecraft.wiki/w/Tick
2. PaperMC/Folia README (regionized multithreading, plugin compatibility, broken APIs). https://github.com/PaperMC/Folia
3. Minecraft Packet ID Reference, Java 26.1 (movement packets and coordinate limits).
   https://github.com/xKumorio/Minecraft-Packet-ID-Reference/blob/main/versions/26.1.md
4. Minecraft Docs, "Input to movement" (speed and flight checks; current Java behavior).
   https://minecraftdocs.dev/systems/player/input-to-movement
5. minecraft.wiki, "Chunk format" (sections, block state and biome palettes, bits per index in Anvil files).
   https://minecraft.wiki/w/Chunk_format
6. minecraft.wiki, "Java Edition protocol/Chunk format" (paletted containers on the wire, bits per entry,
   index order). https://minecraft.wiki/w/Java_Edition_protocol/Chunk_format
7. Java SE 25 API Specification, `java.util.Random` and `java.lang.Math` (48-bit LCG, `nextLong` sign
   extension, Marsaglia polar Gaussian, `floorDiv`/`floorMod`, `round`).
   https://docs.oracle.com/en/java/javase/25/docs/api/java.base/java/util/Random.html
8. David Blackman and Sebastiano Vigna, "Scrambled Linear Pseudorandom Number Generators" (`xoroshiro128++`
   1.0, 2019). https://prng.di.unimi.it/xoroshiro128plusplus.c
9. minecraft.wiki, "Random" and "Seed (level generation)" (`XoroshiroRandomSource`, Stafford `Mix13`, MD5
   positional seeding, decoration, feature, carver, structure and slime-chunk seeds).
   https://minecraft.wiki/w/Random, https://minecraft.wiki/w/Seed_(level_generation)
10. Ken Perlin, "Improving Noise", *ACM Transactions on Graphics (SIGGRAPH 2002)*, 21(3):681–682, and
    minecraft.wiki, "Noise" (`ImprovedNoise`, `PerlinNoise`, `NormalNoise` and `BlendedNoise`).
    https://mrl.cs.nyu.edu/~perlin/paper445.pdf, https://minecraft.wiki/w/Noise
11. Stefan Gustavson, "Simplex noise demystified", Linköping University (2005), for 2D and 3D simplex
    skew/unskew constants, corner falloff and 12-edge gradient table.
    https://weber.itn.liu.se/~stegu/simplexnoise/simplexnoise.pdf
