# Mod support

Scope: how Harpas runs Fabric and NeoForge mods ("modded mode"), and how mod content reaches Java and Bedrock
players **without them installing the modpack**: Bedrock players need nothing, and Java players need only the
Harpas client mod.

Status: design. Nothing here is implemented. Coverage estimates are expectations based on what each client can
display; they must be confirmed by testing real mods ([Validation](#validation)). External facts cite their
source in [References](#references).

---

## Background: why mods need Mojang's server

A Bukkit plugin talks to an API, which Harpas can implement on top of its Rust engine ([jvm.md](jvm.md)). A
Fabric or NeoForge mod does not: it patches Minecraft's own server code with Mixins, injecting into the
methods that generate chunks, move entities or tick blocks. Those methods must exist, in Java, exactly as
Mojang wrote them.

So in modded mode **the world lives in the JVM**, simulated by the real Minecraft server with Fabric or
NeoForge. Harpas does not replace that engine; it provisions it, tunes it, and handles everything around it:
Bedrock players, the content translation, and the Harpas client mod for Java players.

---

## Goals

| Goal | Meaning |
|---|---|
| **No modpack for players** | Players never download the server's modpack |
| **Bedrock: nothing to install** | Bedrock players receive everything automatically when they join |
| **Java: one universal client mod** | Java players install the Harpas client mod once; it works on every Harpas modded server |
| **Plugins and mods together** | Bukkit/Paper plugins and native plugins (WASM, Java) run alongside mods on the same server |
| **Minimal configuration** | One required setting, the mod runtime; Harpas provisions everything else |
| **Honesty about gaps** | What cannot be shown is documented, not hidden |

### Non-goals

- Running mod code on the Rust engine.
- Importing mods into the Rust engine. Only a mod's data (textures, models, recipes) could be imported; its
  behavior is Java code written against Mojang's internals. Importing would sacrifice compatibility, so every
  mod runs on its real loader.
- Sending code to clients. The server sends data only; executing server-provided code on players' machines is
  not acceptable.
- Vanilla Java clients in modded mode. Java players need the Harpas client mod (see
  [Java players](#java-players-the-harpas-client-mod)).
- Mods for versions older than 26.x, for now ([versions.md](versions.md)).

---

## Modded mode

### Detection

Mods go in the server folder's `mods/` directory, as on Fabric and NeoForge servers; plugins stay in
`plugins/`. At startup Harpas scans `mods/` for jars containing `fabric.mod.json` (Fabric) or
`META-INF/neoforge.mods.toml` (NeoForge). If any are found, the server runs in modded mode.

### Choosing the mod runtime

The mod runtime is chosen **explicitly** in `harpas.toml`, and it is required whenever mods are present:

```toml
[mods]
runtime = "fabric"   # or "neoforge"
```

- **No runtime set but mods present:** the server does not start. The error names the runtime the detected
  mods belong to, so the fix is one line.
- **Mods for the other loader present:** the server does not start and lists every mod that does not belong to
  the chosen runtime. Loading both loaders' mods is never attempted, including through bridges that load
  Fabric mods on NeoForge.
- **Minecraft version and loader version:** both loaders' metadata declare dependency ranges on Minecraft and
  on the loader. Harpas picks the newest Minecraft version and loader version that satisfy every mod. If no
  version satisfies all of them, the server does not start and names the conflicting mods.
- **26.x only:** if the mods require a Minecraft version older than 26.x, the server does not start and names
  the mods; older versions are out of scope for now ([versions.md](versions.md)).
- **Optional pins:** `harpas.toml` can also pin the Minecraft version or the loader version, for owners who
  need a specific combination.

### Provisioning

Everything is downloaded on demand into `.harpas/`, with the same verification and atomic install as the
JVM runtime ([jvm.md](jvm.md#runtime-provisioning)):

| Component | Source | License |
|---|---|---|
| JVM runtime | Pinned build, as in [jvm.md](jvm.md#choice-of-jvm) | GPLv2 with Classpath Exception |
| Minecraft server | Mojang's official download endpoints, verified against the hash in Mojang's version manifest [1]. Downloaded on the server owner's machine and never redistributed by Harpas, after the owner accepts Mojang's EULA once ([world.md](world.md#mojang-eula)) | Mojang EULA |
| Fabric Loader | Fabric's metadata service, which already lists loaders for 26.3 [2] | Apache-2.0 [3] |
| NeoForge | NeoForge's Maven repository | LGPL-2.1 [4] |
| Harpas companion mod | Bundled with Harpas, one build per loader | Project license |

### Architecture

```
Java clients + Harpas client mod ──TCP──► Minecraft server + Fabric/NeoForge + mods
                                               (inside Harpas' embedded JVM)
                                                         ▲
                                                         │ shared memory / FFM bridge
Bedrock clients ──UDP──► Harpas (Rust) ◄──────── Harpas companion mod
                   RakNet, codecs, Bedrock translation, pack generation
```

The modded server runs **inside Harpas' embedded JVM**, in the same process. The companion mod uses the same
shared-memory bridge as the Bukkit layer to give the Bedrock layer the world state without serialization.
The alternative, a child process with IPC, isolates crashes better; the trade-off is listed in
[Open questions](#open-questions).

### Companion mod

The companion mod runs inside the modded server. It reads each mod's registries and assets, feeds the Bedrock
layer, serves content definitions to the Harpas client mod, and keeps the loader from requiring the modpack on
the client (registry sync and mod network channels; see below). It is built for **both loaders from one code
base**: a shared core with all the logic, and a thin adapter per loader that only bootstraps it.

- It also **implements Harpas' core API** on top of Mojang's server, so plugins run in modded mode (see
  [Plugins and mods together](#plugins-and-mods-together)).
- One build per supported Minecraft version line (26.x), since mods, loaders and Mojang's internals change
  between versions.
- Bundled with Harpas and installed into the modded server automatically; owners do not add it themselves.

### Plugins and mods together

Plugins never talk to the engine directly: the Bukkit shim, WASM plugins and Java native plugins all use
**Harpas' core API** ([plugin-api.md](plugin-api.md)). That API has two implementations:

```
   Bukkit/Paper plugins (shim)    WASM plugins    Java native plugins
                \                      |                 /
                 └────────── Harpas core API ───────────┘
                         /                        \
         Normal mode: Rust engine          Modded mode: companion mod,
                                           hooked into Mojang's server
```

- **Same plugins, both modes.** A plugin does not know whether it runs on the Rust engine or on a modded
  server.
- **Events come from the modded server's own code.** The companion mod hooks the server where actions happen
  (breaking a block, a player joining) and dispatches events synchronously, on the server thread, with the same
  semantics as in normal mode.
- **Bukkit plugins run in the same JVM** as the modded server, so events and API calls go from Java to Java
  without crossing into Rust. WASM plugins are reached with Java-to-Rust calls, the inexpensive direction.
- **Mod content in the API.** Mod blocks, items and entities are exposed by namespaced identifier
  (`modid:name`), since they do not exist in Bukkit's classic `Material` enumeration.
- **Limits:** plugins that use Mojang's internals (NMS) do not work, as in normal mode. A plugin can assume
  vanilla behavior that a mod changes; such conflicts are documented, not hidden.
- **Cost:** the core API has to be implemented in the companion mod for every supported version line, which is
  one reason modded mode is limited to 26.x for now.

### What runs where

| In the JVM | In Rust |
|---|---|
| World simulation: chunk loading, generation and saving, entities, block ticks | Bedrock networking: RakNet, codecs, compression, encryption |
| All mod logic | Translating chunks, entities, inventories and movement for Bedrock, with shared encoding and the blob cache ([bedrock-interop.md](bedrock-interop.md)) |
| Java client networking | Xbox authentication and skin signing |
| The companion mod | Converting mod assets into Bedrock packs and Java client definitions |
| | Provisioning, supervision, console, metrics |

### What Harpas' JVM adds

- **Provisioned runtime:** owners install nothing.
- **Compact object headers:** modded servers hold millions of Java objects, where smaller headers matter most
  *(measure with real modpacks)*.
- **AOT cache, with a limited reach:** JEP 483's AOT cache only covers classes loaded by the JDK's built-in
  class loaders, not classes from user-defined loaders [5]. Fabric and NeoForge load Minecraft and mod classes
  through their own class loaders *(verify for each loader)*, so the cache mostly helps the JDK and loader
  bootstrap, not the mod classes that dominate startup. Harpas still generates the cache, but does not promise
  a large startup gain in modded mode *(measure)*.
- **Automatic heap and GC choice** sized to the modpack.

### Other optimizations in the modded flow

| Optimization | Gain | Compatibility |
|---|---|---|
| **Bedrock translation in Rust, in-process** (shared memory, shared encoding, blob cache) | The largest: no proxy, no per-player Java client session | No effect on mods |
| **Translation cache** ([below](#translation-cache)) | Startup: mod assets are converted once | No effect on mods |
| **Provisioning cache** | Mojang's server, the loader and its libraries are downloaded and installed once in `.harpas/` and reused | No effect on mods |
| **Parallel startup** | Mod assets (read from the jars) are converted while the server boots; only registry-dependent steps wait for the server | No effect on mods |
| **Compact object headers, GC and heap sizing** | Memory and pause times *(measure)* | No change to Java semantics |
| **AOT cache** | Small in modded mode, see above *(measure)* | No change to Java semantics |

Server-side performance mods are popular on modded servers, but Harpas does **not** add them automatically:
they change game internals, their availability differs per Minecraft version and loader, and any conflict would
break a modpack that works on a plain loader. Owners can add them like any other mod.

---

## Bringing mod content to every client

The companion mod reads each mod's registries (blocks, states, items, entities) from the running server, and
its assets (block states, models, textures, sounds, language files) from its jar. **One analysis produces one
set of content definitions**, in Bedrock's data formats, used by both clients:

| Client | What it receives |
|---|---|
| **Bedrock** | Native custom blocks, items and entities, sent with the resource packs when the player joins |
| **Java with the Harpas client mod** | The same definitions, registered as real content by the client mod, plus a resource pack |

### Bedrock: native content

Textures (including animated flipbooks) and models are converted to Bedrock geometry and materials, and each
block state becomes a Bedrock permutation with its geometry, materials and rotation. Harpas does this in its own
Rust pipeline. Bedrock limits: 16 values per state, 65,536 permutations per block, and a recommended maximum
of 65,536 custom permutations per world [6]. The hashed block network IDs of
[bedrock-interop.md](bedrock-interop.md#4-stable-block-network-ids) cover custom blocks too.

Mods whose entity models use GeckoLib keep them in Bedrock-style geometry and animation files [7], which
convert well. Models written in Java code do not, unless a [community adapter](#community-adapters) describes
them.

### Java players: the Harpas client mod

In modded mode, Java players need the **Harpas client mod** and nothing else: no modpack.

- **One mod for every server.** It is generic: it does not contain any server's content, it interprets the
  definitions the server sends. Players install it once, for Fabric or NeoForge on the client.
- **Required.** When a Java client without the Harpas client mod (or with an incompatible version) connects,
  the server refuses the connection with a clear message and a link to download it. A handshake at login
  detects the mod and its version.
- **No modpack.** The companion mod keeps the loader's registry synchronization from expecting the mods on the
  client; Fabric, for example, disconnects clients that lack registry entries the server has [8]. The client
  learns about mod content only through Harpas' definitions *(verify for NeoForge's registry synchronization)*.
- **Real content.** Blocks, items and entities are registered for real on the client from the server's
  definitions, so there are no borrowed vanilla block states and no display-entity stand-ins in modded mode.

### Translation cache

A mod's appearance (textures, models, sounds, its list of blocks and items) is data, and it does not change
while the mod list stays the same. The translation is therefore done **once** and stored in
`.harpas/translation-cache/`:

- the Bedrock resource packs and custom block, item and entity definitions;
- the content definitions and resource pack for the Harpas client mod.

The cache is keyed by the exact set of mod files (their hashes) and the Minecraft and loader versions. When any
of them changes, the affected entries are regenerated; otherwise startup reuses the cache. Only the appearance
is cached: mod behavior always runs on the loader.

---

## The Harpas client mod

### Bedrock formats as the common language

Bedrock already has mature data formats for geometry, animations, Molang expressions, particles and UI. The
client mod teaches Java to read the same formats, so **one definition serves both clients**, and Bedrock needs
nothing extra. GeckoLib (MIT) already renders Bedrock-style models and keyframe animations on Java for Fabric
and NeoForge [7], and the client mod builds on it.

### What it provides

| Need | How (data only) |
|---|---|
| New blocks, items and entities | Registered from server-sent definitions |
| Animated entities and machines (rotating shafts and gears) | Bedrock-style geometry and animations bound to values sent by the server |
| Machine GUIs | GUIs described as data: slots, progress bars bound to values, buttons that send events, tabs |
| HUDs and overlays | On-screen bars and text bound to server values |
| Custom particles | Particles defined as data, in Bedrock's format |
| Mod keybindings | The server declares keybindings; the client registers them and reports presses |
| Mining and placement | Client-side mining prediction from the real break times, and no flicker on place and break |
| Custom fluids | Data-defined fluids (texture, color, basic visual behavior); the hardest item on the list |

### Community adapters

What a mod does with client code (special renderers, hand-written screens) cannot be converted automatically.
With the client mod's formats, anyone can write a **data adapter** for a mod ("this shaft is this model, it
rotates with this value, its GUI is this"). Adapters are published as data packages in a community registry
and applied automatically when Harpas detects the mod, so popular mods can look close to complete without work
from their authors.

### Security rule

- **First phase:** data plus a limited Molang-style expression language, with no file, network or general
  code access.
- **Later, if data falls short:** a strictly sandboxed WASM runtime on the client with access only to
  rendering and UI APIs. This greatly increases the attack surface and is deferred until there is a clear
  need.

### Cost

A separate product to maintain: one build per loader, updated for every Minecraft release. Because it is
required for Java players in modded mode, it ships together with modded mode.

---

## Expected coverage

Without the modpack. Java columns assume the Harpas client mod.

### Shown well

| Content | Java (Harpas client mod) | Bedrock |
|---|---|---|
| Blocks of any shape, with any number of states | Real blocks | Real custom blocks, within 65,536 permutations and 16 values per state |
| Items (tools, food, materials, armor) | Real items | Real custom items |
| Game logic (recipes, machines, growth, mob AI, world generation) | Complete: it runs on the server | Complete |
| Sounds and language files | Resource pack | Resource pack |
| Multiblock structures | Shown | Shown |
| Mod commands | Work | Translated |
| Entities with GeckoLib models | Model and animations | Model and animations |
| Particles | Data-defined particles | Data-defined particles |

### Shown with limits

| Content | Java (Harpas client mod) | Bedrock |
|---|---|---|
| Entities with models written in Java code | Only with a community adapter | Only with a community adapter |
| Machine GUIs | Data-defined GUIs: slots, bars, buttons, tabs | Forms or container screens; less flexible *(verify)* |
| HUDs and overlays | Data-defined bars and text | Limited *(verify)* |
| Mod keybindings | Declared by the server | No equivalent; alternatives (items, commands) are needed |
| Custom fluids | Basic data-defined fluids | Not supported; shown as water or lava |
| Custom biomes | Good (synchronized as data) | Limited *(verify)* |
| Custom dimensions | Work | Bedrock has three dimensions; custom ones must be shown as one of them *(verify)* |

### Not shown

| Content | Why |
|---|---|
| Special rendering without an adapter (rotating belts, animated pipes, visual effects) | Client code; a static version at best |
| Shaders and screen effects | Client code |
| Client-only mods (minimaps, recipe viewers, client optimizations) | Not applicable: they do not run on the server |

### By kind of mod

| Kind of mod | Experience without the modpack |
|---|---|
| Content (ores, food, tools, furniture, decoration) | Very good on both clients |
| Mobs | Very good with GeckoLib models; needs adapters otherwise |
| Technology (machines, automation) | Logic works fully; visuals depend on data GUIs and adapters |
| Magic and effects | Partial: depends on particles, HUDs and adapters |
| Client-only | Not applicable |

---

## Existing approaches

Other projects solve parts of this problem. Harpas combines their techniques in one server:

| Approach | What it does | Limitation that Harpas addresses |
|---|---|---|
| A Bedrock translation companion for modded servers | Reads mod registries and assets inside the modded server and turns them into Bedrock packs and custom blocks, working with a proxy that connects as one Java client per Bedrock player | The proxy's cost: Harpas translates in-process, with shared encoding and the blob cache |
| A server-side content library | Mods written against it display custom content to vanilla Java clients with borrowed block states, item models and display entities; an optional client mod shows the real content and fixes mining and flicker | Mod authors must adopt it; Harpas works with regular mods |
| Packet-level automatic conversion | Converts regular mods for vanilla Java clients by transforming outgoing packets, leaving the server genuinely modded | Best-effort visuals; Harpas gives Java players real content through its client mod |

What Harpas takes from them: reading registries and assets from inside the modded server; converting models,
textures and block states into Bedrock permutations; detecting a client mod with a handshake; and leaving the
server genuinely modded while changing only what clients receive.

---

## Phasing

1. After the core engine, Bedrock interoperability, the Bukkit layer and the native API, since modded mode
   reuses their JVM provisioning, RakNet, codecs and content pipeline.
2. Modded mode together with the Harpas client mod (Java players need it) and the companion mod's
   implementation of the core API (plugins need it).
3. Community adapters and the adapter registry.

---

## Open questions

1. Modded server inside the embedded JVM (shared memory, one process; the current preference) or as a child
   process (crash isolation, IPC)?
2. Support policy within 26.x: how long each version line keeps companion mod and client mod builds.
3. How to handle mods' custom network channels to clients that do not have the mods.
4. Format of data-defined GUIs, and governance of the community adapter registry.
5. Distribution channel for the Harpas client mod.

---

## Validation

1. **Coverage suite:** a fixed list of popular mods per category (content, mobs, technology, magic), checked
   with a Java client running only the Harpas client mod and with a Bedrock client; the coverage tables above
   are updated from the results.
2. **No modpack:** a Java client with only the Harpas client mod joins every server in the suite without
   registry or channel errors, on both loaders.
3. **Bedrock efficiency:** memory, CPU and bandwidth with Bedrock players on a modded server, compared with a
   proxy-based setup on the same modpack.
4. **Startup:** time to a joinable server for a large modpack, with and without the translation cache and the
   AOT cache.
5. **Memory:** heap and RSS of a large modpack with and without compact object headers.

---

## References

1. Mojang version manifest (`piston-meta.mojang.com/mc/game/version_manifest_v2.json`), which lists each
   version's server download and its hash. https://piston-meta.mojang.com/mc/game/version_manifest_v2.json
2. Fabric metadata service, loader versions for 26.3. https://meta.fabricmc.net/v2/versions/loader/26.3
3. FabricMC/fabric-loader (Apache-2.0). https://github.com/FabricMC/fabric-loader
4. neoforged/NeoForge (LGPL-2.1). https://github.com/neoforged/NeoForge
5. JEP 483: Ahead-of-Time Class Loading & Linking (classes from user-defined class loaders are out of scope). https://openjdk.org/jeps/483
6. Bedrock Wiki, "Block Permutations". https://wiki.bedrock.dev/blocks/block-permutations
7. bernie-g/geckolib (MIT). https://github.com/bernie-g/geckolib
8. FabricMC/fabric issue #2168 (registry sync disconnects clients missing entries). https://github.com/FabricMC/fabric/issues/2168
