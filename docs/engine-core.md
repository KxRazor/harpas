# Engine core

Scope: how the Rust engine runs the game in normal mode: the tick loop and its order, which work runs in
parallel, how threads are laid out, how the world and entities are stored, and how plugins are called from
the tick. Modded mode uses Mojang's server instead and is covered in [mod-support.md](mod-support.md).

Status: in progress. The logic thread, tick skeleton and players in the void world are implemented; world
storage and the remaining entity logic are still design. Facts about vanilla cite their source in
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
dimensions, connections and autosave, in that order; autosave is scheduled every 6,000 ticks. Only the
connection and player work is implemented; the other phases remain placeholders.

Connection threads queue entry, departure and movement for the connection phase, and translate the resulting
messages into edition-specific packets. The logic thread owns each player's profile and position and publishes
the online count. Player entities have distinct runtime IDs; a duplicate UUID disconnects the old session.
All players appear in the player list, while entity tracking is per viewer, capped by both its view distance
and 128 blocks. Spectator entities are hidden from non-spectators. A departure removes the entity and list entry
even after an I/O or protocol error. Authentication and Keep Alive remain on the connection threads.

For the void world, movement rejects non-finite positions or angles, clamps coordinates to vanilla's limits [3],
and corrects a displacement over 10 blocks from the beginning of a tick. A pending teleport must be confirmed
before more movement is accepted. In Survival and Adventure, a player hovering in the void for more than 80
ticks is disconnected unless `allow-flight` is enabled; Creative and Spectator may fly [4]. These are the
current void-world checks, not a complete implementation of vanilla's movement and collision rules: world
collision, vehicle and elytra movement, velocity effects and their flight exemptions need the later world and
gameplay work. Signed chat is also pending.

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
| Lighting | Results are applied at defined points, as vanilla's light engine applies queued updates *(verify against vanilla's light update timing)* |
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

- Sections of 16×16×16 blocks stored as **paletted containers**, the same structure vanilla uses.
- A **version counter per section**, incremented on every change. The shared encoding of Java and Bedrock
  packets caches encoded sections by version ([protocol-codecs.md](protocol-codecs.md#java-wire-format),
  [bedrock-interop.md](bedrock-interop.md#1-shared-chunk-encoding)).
- Block entities in a per-chunk map keyed by position.

### 7. Determinism

- **Random number generators identical to vanilla's**, bit for bit, and consumed in the same order. World
  generation, mob spawning, loot and many behaviors depend on them.
- **Java's math semantics** where results differ from Rust's defaults (floating-point and integer edge cases),
  so computed values match vanilla's.
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
