# Embedded JVM

Scope: how Harpas runs Bukkit/Paper plugins written in Java inside the Rust server, and how the bridge keeps
overhead low without giving up functionality, compatibility or performance.

Status: design. Nothing here is implemented. Every number marked *(measure)* must be confirmed by the bridge
benchmark in [Validation](#validation) before the design is frozen. Claims about the JDK cite their source in
[References](#references); everything else is a design decision of this project.

---

## Goals

In priority order. When two goals conflict, the higher one wins.

| # | Goal | Meaning |
|---|---|---|
| 1 | **Compatibility** | Unmodified plugin jars load from `plugins/` and behave as they do on Paper. Target tiers below |
| 2 | **Functionality** | Plugins see the Bukkit/Paper API they expect: events, schedulers, commands, permissions, persistent data, per-plugin classloaders |
| 3 | **Performance** | Plugin code runs at full JIT speed; the bridge adds as little as possible on top; no JVM cost when no Java plugin is installed |

### Principles

- **Paper is the reference behavior.** When the Bukkit API leaves something unspecified, Harpas does what
  Paper does, because that is what plugins are tested against.
- **Zero configuration.** Server owners never enable or disable an optimization to make a plugin work.
- **Optimizations must be unobservable.** An optimization is only applied where no plugin can tell the
  difference: same results, same event order, same exceptions. An optimization that could change behavior is
  either redesigned until it cannot, or not used.

### Compatibility tiers

| Tier | Plugins | Approach |
|---|---|---|
| T1 | Pure Bukkit/Paper API (permissions, economy, chat, utilities) | Full support: the shim implements the API |
| T2 | Packet-level (ProtocolLib, PacketEvents) | To be decided: these libraries hook Paper's network pipeline and its internal packet classes, so supporting them means emulating those internals *(verify the exact surface they rely on)* |
| T3 | NMS / CraftBukkit internals via reflection | Not supported in general; hand-written shims for selected popular plugins only |

### Floodgate API

The shim also implements the **Floodgate API** (MIT) [23], which many Bukkit plugins use to tell whether a
player is on Bedrock and to show Bedrock forms. Those plugins work unchanged on Harpas, with player identities
compatible with Floodgate's ([identity.md](identity.md)).

### Non-goals

- Running Fabric or NeoForge mods on the Rust engine. They patch Mojang's code, so they run in modded mode on
  Mojang's server instead ([mod-support.md](mod-support.md)). Forge is not supported.

---

## Architecture

```
┌──────────────────────── harpas process ────────────────────────┐
│                                                                │
│  Rust engine: logic thread (vanilla tick order)                │
│     │  event with listeners          ▲  API calls              │
│     │  (synchronous call)            │  (downcalls, applied    │
│     ▼                                │   immediately)          │
│  ┌──────────────── shared off-heap memory ───────────────┐     │
│  │ event slot (arguments, result) │ entity/player views   │     │
│  └────────────────────────────────────────────────────────┘     │
│     ▲                                │                         │
│  ┌──┴────────────────────────────────▼──────────────────┐      │
│  │ JVM (HotSpot, JDK 25), same logic thread             │      │
│  │  harpas-bukkit shim  ─►  plugin classloaders  ─► jars │      │
│  └───────────────────────────────────────────────────────┘      │
└────────────────────────────────────────────────────────────────┘
```

Components:

- **`harpas-jvm`** (Rust): creates the JVM, attaches the engine's logic thread to it, owns the shared memory,
  and dispatches events to Java.
- **`java/harpas-bukkit`** (Java): implements the Bukkit/Paper interfaces on top of the shared memory and
  downcalls into Rust. It is an adapter over the same core API that WASM plugins use; it never reaches engine
  internals directly.
- **Java bindings of the native API**: Java plugins written against Harpas' own API run on the same JVM and
  bridge, with bindings generated from the WIT definition ([plugin-api.md](plugin-api.md)).

**In modded mode** the shim is unchanged, but the core API it uses is implemented by the companion mod inside
the modded server's JVM ([mod-support.md](mod-support.md#plugins-and-mods-together)). Events and API calls then
go from Java to Java, and Bukkit's main thread is the modded server's main thread rather than the Rust logic
thread.

### Principle: synchronous events, paid only when someone listens

Bukkit events are synchronous. A `BlockBreakEvent` fires **before** the block is broken, and if a listener
cancels it the block is not broken; listeners also read the world inside the handler and expect to see it as
it is at that instant. Events therefore cannot be collected and delivered once per tick.

So the engine calls into Java **at the point of the tick where the event happens**, on its logic thread,
which is attached to the JVM once at startup:

1. The engine reaches an action that fires an event, for example breaking a block.
2. If no plugin listens to that event type (the [interest mask](#2-event-interest-mask)), nothing crosses to
   Java and the action continues.
3. Otherwise the engine writes the event's arguments into a reusable off-heap **event slot** and calls the
   shim's dispatcher. The dispatcher runs the listeners in Bukkit's priority order and writes the result
   (cancelled, modified fields) back into the slot.
4. The engine reads the result and continues, or stops if the event was cancelled.

While listeners run, the engine is paused on the same thread, so the world cannot change underneath them.
Writes from plugins are applied immediately through downcalls, and an action that fires further events
(a plugin teleporting a player fires `PlayerTeleportEvent`) dispatches them re-entrantly, as Paper does.

Calls from native code into Java go through an upcall stub or JNI, and an exception escaping an FFM upcall
terminates the JVM [2]; the dispatcher catches every exception at the boundary. FFM downcalls (Java → native)
are the cheap, well-optimized direction, with overhead targeted to be "comparable to, if not better than, JNI"
[1]. The cost of the Rust → Java call per event is measured in the [bridge benchmark](#validation).

---

## Optimizations

Each technique states why it is unobservable to plugins. All of them are automatic; none has a setting.

### 1. No JVM unless needed

The JVM starts only if `plugins/` contains at least one Bukkit/Paper plugin. Without one, the runtime is not
even downloaded (see [Runtime provisioning](#runtime-provisioning)). A server with only WASM plugins pays
nothing. *Unobservable: there is no plugin to observe it.*

### 2. Event interest mask

At registration time the shim builds a bitmask of event types with at least one listener and publishes it
to Rust. Rust skips writing events nobody listens to. High-frequency events such as `PlayerMoveEvent` cost
nothing on servers whose plugins do not listen to them.
*Unobservable: an event with no listeners has no effect in Bukkit. The shim updates the mask whenever
listeners are registered or unregistered at runtime (`HandlerList` changes), so a listener added later
receives events from the next tick on, as it would on Paper.*

### 3. Reads from shared memory, not calls

Hot read-only state (position, rotation, health, game mode, world, ping…) is published by Rust into
off-heap views, laid out as fixed-size records per entity slot. Shim methods such as `getLocation()` and
`getHealth()` read them through `MemorySegment`/`VarHandle` with no native call.
*Unobservable: the engine is paused on the same thread while listeners and tasks run, and plugin writes are
applied immediately, so the views always show the current state.*

### 4. Writes applied immediately, network output batched

Plugin writes (`setBlock`, `teleport`, `setHealth`…) are downcalls that the engine applies at once, so a later
read, by the same plugin or another, sees them, as on Paper. Only what is inherently asynchronous is batched:
packets produced by plugin calls (`sendMessage`, titles, sounds) join the connection's outgoing queue and are
sent with the rest of the tick's network output.

### 5. Cheap downcalls where a call is unavoidable

Queries that cannot be precomputed (e.g. block lookups in arbitrary chunks, ray traces) use FFM downcall
handles stored in `static final` fields and invoked with `invokeExact`. Short, non-blocking functions that
never call back into Java may be linked with `Linker.Option.critical(...)`, a hint the JVM can use for
optimizations valid only for such functions; misuse can crash the JVM [3][4].
*Unobservable: these are internal shim functions, invisible to plugins. `critical` is only applied to
functions proven short and non-reentrant.*

### 6. Bulk APIs for heavy plugins

World-editing style workloads (thousands of block reads/writes) get bulk entry points that pass a whole
region through one shared segment instead of one call per block. The Bukkit API stays the same; the shim
batches internally only where the result is identical to doing the calls one by one.

### 7. Fast listener dispatch

Paper replaces reflective listener invocation with generated executors: ASM-generated classes, and method
handles for private or static handlers [5]. The shim does the same (generated classes or
`LambdaMetafactory`), so dispatch is a direct call that the JIT can inline. *Unobservable: listeners are
called in the same order with the same arguments; only the invocation mechanism changes.*

### 8. Stable object identity

The same engine entity always maps to the same Java object (`Player`, `Entity`, `World`), looked up by
entity ID. Plugins rely on `==`, `equals` and objects used as map keys.

### Rejected: event object pooling

Reusing event objects between dispatches would reduce allocations, but a plugin that keeps a reference to an
event after its handler returns would see the event change. That is observable, so pooling is not used.
Short-lived event objects are cheap for generational collectors. If measurements show allocation matters,
the alternative to evaluate is letting the JIT's escape analysis remove the allocations, which is
unobservable by definition.

---

## Threading

Bukkit's API is not thread-safe and assumes one main thread. Paper enforces this with `AsyncCatcher`, which
throws `IllegalStateException` when certain operations run off the primary thread [6]. Folia instead runs
regions in parallel, requires plugins to opt in with `folia-supported: true`, and replaces
`BukkitScheduler` with `RegionScheduler`, `EntityScheduler`, `GlobalRegionScheduler` and `AsyncScheduler` [7].

Harpas runs the game in vanilla tick order on one logic thread ([engine-core.md](engine-core.md)), and that
thread is Bukkit's main thread:

- `Bukkit.isPrimaryThread()` is true on the logic thread.
- Events are dispatched synchronously, at the point of the tick where they happen (see
  [Architecture](#architecture)).
- `BukkitScheduler` tasks run on the logic thread at the same point of the tick as on Paper.
- Async tasks (`runTaskAsynchronously`) run on a thread pool. Calls into the API from those threads behave
  exactly as on Paper: where Paper throws (`AsyncCatcher`), Harpas throws the same exception; where Paper
  tolerates the call, Harpas tolerates it too. Harpas is never stricter than Paper.
- Plugins that declare `folia-supported: true` load as well. Folia's schedulers are implemented and run their
  tasks on the logic thread at the corresponding point of the tick; they work, without regions running in
  parallel.
- Timings are recorded per plugin (see below). A slow plugin is reported, never disabled automatically.

Plugin code runs serially on the logic thread, as on Paper. The engine work around it (networking, chunk
loading and generation, lighting, packet encoding, Bedrock translation) runs in parallel.

---

## JVM configuration

Target: **HotSpot, JDK 25** (current LTS). `harpas-jvm` chooses all flags itself; server owners do not need
to set any. Each flag has a reason, and only flags that do not change Java semantics are used.

### Choice of JVM

OpenJDK distributions from different vendors, Temurin among them, compile the same HotSpot code base and are
TCK-verified; they differ in support, lifecycle and platform coverage rather than in the JVM itself [16]. The
real alternatives are different JIT compilers or different JVMs.

Requirement: the runtime's license must allow commercial use, including paid hosting, without conditions.

| Candidate | What differs | License | Status |
|---|---|---|---|
| **Eclipse Temurin** (HotSpot, C2 JIT) | Reference build | GPLv2 with Classpath Exception [17] | **Default** |
| **GraalVM Community Edition** (HotSpot runtime, Graal JIT) | Different JIT compiler | GPLv2 with Classpath Exception | **Benchmark candidate** |
| GraalVM builds under the GraalVM Free Terms and Conditions | Graal JIT | Redistribution only when no fee is charged [18] | Excluded: ambiguous for paid hosts |
| Commercial JVMs with their own JIT and GC | Different JIT and GC | Production use requires a paid license | Excluded |

Notes on GraalVM Community Edition:

- GraalVM has been detached from the Java SE release train; GraalVM for JDK 24 was the last release licensed
  and supported as part of Java SE, and Community Edition remains available [19][20].
- Since the 25.1 line, releases are monthly innovation releases on OpenJDK 25 with quarterly security updates,
  not a classic LTS. Recent releases changed the Graal JIT's default inlining and enabled loop vectorization
  [21].
- macOS on Intel is no longer supported after 25.0.1 [21].

GraalVM Community Edition replaces Temurin as the default only if the [JVM comparison](#validation) shows a
clear gain with no compatibility difference, and its maintenance cadence is judged acceptable. Because the
runtime is downloaded on demand and pinned per Harpas version, switching later only changes which build is
pinned.

### Flags

| Setting | Why | Source |
|---|---|---|
| `-XX:+UseCompactObjectHeaders` | Product feature in JDK 25, off by default. Shrinks object headers to 64 bits; the JEP reports 22% less heap and 8% less CPU in one SPECjbb2015 setting *(measure for plugin workloads)*. It does not change Java semantics; only code that inspects object layout through `Unsafe` could notice. Enabled unless the plugin compatibility suite finds a plugin it breaks | [8] |
| `-XX:AOTCacheOutput` (training run) / `-XX:AOTCache` (production) | AOT cache of loaded and linked classes (JEP 483) plus method profiles (JEP 515), created in one step with JEP 514. Faster startup and warmup. Must be regenerated when the JDK or the plugin set changes | [9][10] |
| GC: G1 or Serial for small heaps; ZGC for large ones | The plugin heap is small because the world lives in Rust. In JDK 24/25 the AOT cache did not support ZGC; JEP 516 (JDK 26) removes that limit | [9][11] |
| Generational Shenandoah (`-XX:+UseShenandoahGC -XX:ShenandoahGCMode=generational`) | Product in JDK 25; candidate for low-pause setups *(measure)* | [12] |
| `--enable-native-access=<shim module>` | FFM downcalls and upcall stubs are restricted methods; enabling native access only for the shim keeps plugins from using them unintentionally | [2][1] |
| `-Xmx` sized automatically | The heap holds plugin data only. The default must be generous enough for heavy plugins (map renderers, logging plugins); an out-of-memory error is a compatibility failure, so the heap errs on the large side | design |

The AOT cache is regenerated automatically by `harpas-jvm` when it detects a change in the JDK or in
`plugins/`: the first start after a change runs in training mode.

---

## Runtime provisioning

Bukkit/Paper plugins work by default: server owners do not install Java. Harpas downloads and verifies the
runtime itself, and only when it is needed.

### Server folder layout

Like Paper, Harpas uses the directory it is started from as the server folder: configuration, worlds and
plugins live relative to the current directory. Everything Harpas provisions for the JVM lives inside that
folder:

```
my-server/                   ← the directory harpas is started from
├── harpas.toml
├── world/
├── plugins/                 ← Bukkit/Paper jars and .wasm components
└── .harpas/
    ├── runtime/<version>/   ← JVM, downloaded on demand
    ├── bukkit-shim/         ← the Java shim, provisioned with the runtime
    └── aot-cache/           ← this server's AOT cache
```

Why the server folder, not next to the binary or in a per-user cache:

- **Containers and hosting panels** persist the server folder; a per-user cache outside it can be lost on
  every restart and force a new download each time.
- **The binary's directory can be read-only** (system-wide installs) and can be shared by several servers.
- **The AOT cache is per server anyway**, since it depends on the JDK and on the plugin set.
- **Self-contained and easy to clean up:** moving the folder moves everything; updating Harpas removes the
  previous runtime without coordinating with other servers.

`.harpas/runtime/` can be excluded from backups; it is recreated automatically.

The complete server folder layout is in [operations.md](operations.md#server-folder).

### Detection

At startup Harpas scans `plugins/` for jars that contain `plugin.yml` or `paper-plugin.yml` (Bukkit/Paper
plugins) or a Harpas plugin manifest (Java plugins written against the native API, see
[plugin-api.md](plugin-api.md#4-java-plugins-use-the-same-api-on-the-embedded-jvm)). Looking inside the jar
avoids downloading a JVM because of an unrelated `.jar` file. If none is found, nothing is downloaded and no
JVM is started.

If the first Bukkit/Paper plugin is added while the server is running, the runtime is downloaded in the
background and the plugin is loaded once it is ready.

### Download and verification

- Each Harpas release pins one exact JDK build per platform. The pinned build changes only with a Harpas
  release, after passing the compatibility suite.
- The runtime is downloaded from the Adoptium API (`api.adoptium.net/v3/binary/...`) [22].
- The download is verified before use: SHA-256 (the `.sha256.txt` file next to the binary) and Adoptium's GPG
  signature (`.sig`) [22]. Harpas also stores the expected SHA-256 of the pinned build in its own binary, so a
  compromised mirror cannot substitute a different build.
- **Atomic install:** the archive is downloaded and extracted into a temporary directory inside `.harpas/`,
  verified, and only then renamed to `runtime/<version>/`. An interrupted download never leaves a broken
  runtime.
- `HTTPS_PROXY` is honored.
- The JVM is loaded from `libjvm` inside `.harpas/runtime/<version>/` (see [Embedding](#embedding)).

### Without internet access

- `harpas runtime install --from <archive>` installs a runtime archive obtained separately; it goes through
  the same verification.
- Official container images ship with the runtime already installed.

### Optional shared location

`HARPAS_RUNTIME_DIR` points several servers on the same machine at one shared runtime directory. It is optional
and meant for operators who run many servers; nobody needs it for a normal setup. A shared directory uses a
lock file so that servers starting at the same time do not download twice.

### If provisioning fails

Starting without the Java plugins could mean starting without the permissions or protection plugins the
server relies on. So the server **does not start**: it prints a clear error that names the plugins needing
Java and explains how to fix it (network, proxy, or `harpas runtime install --from`).

---

## Embedding

### Creating the JVM

Rust loads `libjvm` from the provisioned runtime (see [Runtime provisioning](#runtime-provisioning)) and
creates the JVM once per process through the JNI invocation API (`JNI_CreateJavaVM`). In the Rust `jni`
crate this requires the `invocation` feature; the thread that creates the VM is not attached automatically
and must attach explicitly [13]. The VM is created once and never destroyed while threads are attached, since
`JavaVM::destroy()` with attached threads is undefined behavior in that crate [13].

### Threads

- The engine's logic thread is attached to the JVM once at startup and stays attached; it is the only engine
  thread that dispatches events and runs plugin tasks.
- Other Rust threads (networking, chunk workers) do not call into Java. If an upcall does run on a thread
  unknown to the VM, HotSpot attaches it as a daemon thread on entry and detaches it when the thread exits
  [14]. That path works but is not used in the hot loop.
- Async plugin tasks run on Java threads created by the shim.

### Signals

HotSpot uses SIGSEGV and other signals internally (implicit null checks, safepoint polling) [15]. The Rust
host must therefore:

- Install its own handlers (if any) before creating the JVM, or preload/link `libjsig` so later handlers are
  chained behind the JVM's [15].
- Handle SIGINT/SIGTERM/SIGHUP/SIGQUIT itself for clean shutdown; these cannot be chained, and `-Xrs` makes
  the JVM leave them alone [15].
- Never treat a SIGSEGV inside JVM code as a crash of the Rust process.

---

## Failure isolation

- Exceptions thrown by a plugin are caught by the shim at the dispatch boundary, logged, and attributed to
  the plugin, as Bukkit does. No exception reaches an upcall stub, because an exception there terminates
  the JVM [2].
- A plugin that takes too long is reported in timings and in the console. It is not disabled automatically:
  on Paper a slow plugin slows the server but keeps working, and Harpas behaves the same.
- A JVM crash takes the process down (same address space). Mitigation is the restricted use of `critical`
  and of raw memory access, both confined to the shim.

---

## Observability

- Per-plugin and per-listener timings (time spent, events handled, downcalls made).
- Bridge counters: events dispatched vs. skipped by the interest mask, Rust → Java calls and downcalls per
  tick.
- GC and heap metrics from the JVM, exposed alongside engine metrics.

---

## Open questions

1. Reimplement the Bukkit/Paper interfaces or load the published API jar? Either way the shim is GPL-3.0
   ([licensing.md](licensing.md#gpl-30-for-the-bukkit-shim)).
2. Which Paper-specific APIs (Adventure, Brigadier commands, `paper-plugin.yml` loaders) are in T1?
3. Memory layout of the entity views: fixed slots per entity vs. per-component arrays.
4. Which T3 plugins, if any, justify hand-written shims?
5. Composition of the plugin compatibility suite: which real plugins, and which behaviors of each are
   checked against Paper.
6. Obligations of redistributing the runtime, if Harpas ever ships it instead of downloading it on demand.
7. Scope of T2: which parts of Paper's network pipeline and packet classes to emulate for packet-level
   libraries.

---

## Validation

A benchmark in `bench/jvm-bridge` decides the open performance questions before the shim is written:

1. Rust embeds JDK 25; a minimal shim exposes `Player`, `getLocation()`, `teleport()` and `PlayerMoveEvent`.
2. 200 simulated players at 20 TPS with several listeners.
3. Measure: FFM downcall cost (plain and `critical`), JNI downcall cost for comparison, upcall cost,
   shared-memory read cost, cost of a synchronous event dispatch (Rust → Java call, listeners, result read
   back), time spent in plugins per tick, RSS, GC pauses.
4. Configurations: default HotSpot; HotSpot with compact object headers and AOT cache; G1 vs. Serial vs.
   ZGC vs. generational Shenandoah.
5. **JVM comparison:** Temurin (C2 JIT) vs. GraalVM Community Edition (Graal JIT), both on JDK 25, with the
   same set of real plugins. Measure time spent in plugins per tick (mean and 99th percentile), warmup time, RSS
   (the Graal compiler also uses memory), CPU spent compiling, and the compatibility suite, where any
   behavioral difference disqualifies a candidate.
6. **Provisioning:** download size and time per platform, and first-start time including verification.

Results are recorded in this document and replace the *(measure)* markers.

---

## References

1. JEP 454: Foreign Function & Memory API (delivered in JDK 22). https://openjdk.org/jeps/454
2. `java.lang.foreign.Linker`, JDK 25 API documentation (upcall exceptions, restricted methods, arenas).
   https://docs.oracle.com/en/java/javase/25/docs/api/java.base/java/lang/foreign/Linker.html
3. `Linker.Option`, JDK 22 API documentation (`critical(boolean allowHeapAccess)`).
   https://docs.oracle.com/en/java/javase/22/docs/api/java.base/java/lang/foreign/Linker.Option.html
4. JDK-8318645: Add Panama feature to pass heap segments to native code. https://bugs.openjdk.org/browse/JDK-8318645
5. Paper patch "Use ASM for event executors" (third-party mirror; check the official PaperMC repository for
   the current state). https://ayakael.net/mirrors/papermc/commit/386693ee50f3096c207c03adef4dd6df7d126b76
6. Paper patch "Improve and expand AsyncCatcher" (third-party mirror).
   https://ayakael.net/mirrors/papermc/blame/commit/1dcda2491eeac70738fdff1983831d78f99f8f27/patches/server/Improve-and-expand-AsyncCatcher.patch
7. PaperMC/Folia README. https://github.com/PaperMC/Folia
8. JEP 519: Compact Object Headers (JDK 25). https://openjdk.org/jeps/519
9. Inside.java, "Run Into the New Year with Java's Ahead-of-Time Cache Optimizations".
   https://inside.java/2026/01/09/run-aot-cache/
10. SoftwareMill, "What's new in Project Leyden: JEP 514 and JEP 515 explained".
    https://softwaremill.com/whats-new-in-project-leyden-jep-514-and-jep-515-explained/
11. JEP 516: Ahead-of-Time Object Caching with Any GC (JDK 26). https://openjdk.org/jeps/516
12. JEP 521: Generational Shenandoah (JDK 25). https://openjdk.org/jeps/521
13. `jni` crate 0.22 migration guide and `JavaVM` docs. https://docs.rs/crate/jni/latest/source/docs/0.22-MIGRATION.md
14. OpenJDK source, `src/hotspot/share/prims/upcallLinker.cpp` (`maybe_attach_and_get_thread`).
    https://github.com/openjdk/jdk/blob/master/src/hotspot/share/prims/upcallLinker.cpp
15. Java SE 24 Troubleshooting Guide, "Handle Signals and Exceptions".
    https://docs.oracle.com/en/java/javase/24/troubleshoot/handle-signals-and-exceptions.html
16. foojay, "Which Java Runtime Should You Use in Production? Comparing OpenJDK Distributions". https://foojay.io/today/which-java-runtime-should-you-use-in-production-comparing-openjdk-distributions/
17. Eclipse Temurin NOTICE (SPDX `GPL-2.0 WITH Classpath-exception-2.0`) and Temurin developer list on bundling. https://www.eclipse.org/lists/temurin-dev/msg00005.html
18. GraalVM FAQ (GraalVM Free Terms and Conditions: free use, redistribution only when no fee is charged, jlink output covered). https://www.graalvm.org/faq/
19. ADTmag, "Oracle Shifts GraalVM Focus Away from Java" (2025-09-30). https://adtmag.com/articles/2025/09/30/oracle-shifts-graalvm-focus-away-from-java.aspx
20. InfoWorld, "GraalVM 25 arrives, backed by JDK 25". https://www.infoworld.com/article/4061937/graalvm-25-arrives-backed-by-jdk-25.html
21. GraalVM 25.3 release notes. https://www.graalvm.org/release-notes/25.3/
22. Adoptium, "Using the Adoptium API in CI/CD Pipelines" (binary endpoint, `.sha256.txt`, GPG signatures). https://adoptium.net/installation/ci-scripts/
23. GeyserMC/Floodgate (MIT), API module. https://github.com/GeyserMC/Floodgate/tree/master/api
