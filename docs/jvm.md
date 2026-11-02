# Embedded JVM

Scope: how Harpas runs Bukkit/Paper plugins written in Java inside the Rust server, and how the bridge keeps
overhead low without giving up functionality, compatibility or performance.

Status: design, with the M1 bridge prototype measured in [Validation](#validation). The production bridge and
shim are not implemented. Remaining *(measure)* markers need plugin workloads before the design is frozen.
Claims about the JDK cite their source in
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
| Generational Shenandoah (`-XX:+UseShenandoahGC -XX:ShenandoahGCMode=generational`) | Product in JDK 25; candidate for low-pause setups, tested in the M1 synthetic benchmark below *(measure with real plugins)* | [12] |
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

A benchmark in `crates/harpas-bench/examples/jvm-bridge.rs` measures the bridge before the shim is written.
Its [run guide](../crates/harpas-bench/examples/jvm-bridge/README.md) documents the fixture, ABI, checks and JVM
configurations. The broader validation plan is:

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

The M1 synthetic results below answer the crossing-cost and synchronous-event questions. Real plugin
compatibility and allocation workloads, the Temurin/GraalVM comparison with real plugins, automatic heap/GC
sizing, reentrant events and provisioning remain to be validated; they cannot be decided by this small fixture.

### M1 bridge results — 1 November 2026

Measured on Linux x86_64, AMD Ryzen 5 5600H, Rust 1.99.0, embedded Eclipse Temurin **25.0.5+8-LTS**.
The host used `-Xrs -Xms64m -Xmx256m --enable-native-access=ALL-UNNAMED`. Runs were sequential, without CPU
pinning. Each case had two seconds of unpaced warmup; each event-dispatch path then ran 200 measured ticks at
20 TPS, with 200 simulated players and three listeners. These are synthetic listeners, not Bukkit plugins:
they cancel and modify moves, read a shared `Player.getLocation()`, and perform occasional immediate
`teleport()` downcalls. Every run checks the behavior of both paths before measuring.

The default G1 configuration was repeated in three fresh processes. The following are the median and range
of the three per-process medians (seven timed batches within each process), without subtracting loop cost:

| Operation | Median ns/op | Range ns/op |
|---|---:|---:|
| Java loop baseline | 0.73 | 0.73–0.73 |
| Shared-memory `x` read | 1.82 | 1.73–1.87 |
| Shared-memory `getLocation()` (three doubles) | 5.36 | 4.94–6.25 |
| Java → Rust `x` read, JNI | 5.20 | 5.17–5.87 |
| Java → Rust `x` read, plain FFM | 8.35 | 8.12–9.83 |
| Java → Rust `x` read, critical FFM | 7.68 | 7.39–9.37 |
| Rust → Java ping, cached JNI including exception check | 87.64 | 82.64–101.40 |
| Rust → Java ping, FFM upcall | 21.43 | 20.36–29.57 |

Full synchronous ticks include Rust's event-slot writes, one crossing per event, all listeners, and reading
and applying the event result. Times exclude the sleep between ticks. The figures below are medians of the
three per-process means/p99s, rather than a pooled p99; the p99 uses the nearest-rank method.

| Dispatch path | Mean µs/tick (range) | p99 µs/tick (range) | Deadline overruns |
|---|---:|---:|---:|
| Cached JNI | 45.09 (43.65–45.73) | 92.50 (77.46–96.44) | 0 |
| FFM upcall | 23.18 (22.03–23.38) | 55.42 (41.86–59.34) | 0 |

**Bridge decision:** retain synchronous, individual event dispatch on the attached logic thread and shared
entity views. In this fixture, the FFM route spends about 0.046% of a 50 ms tick on 200 events; there is no
measured need to defer events or change their semantics. Use FFM upcalls as the leading dispatcher candidate,
with JNI for VM creation and bootstrap. Benchmark production downcall signatures before choosing their
binding: JNI beats plain and critical FFM for this scalar read, and `critical` gives only a small gain here.
These results do not justify marking a callback-capable write as critical.

### Heap, GC and AOT configuration probes

The same workload and checks also passed with Serial, ZGC, generational Shenandoah, compact headers, and
compact headers plus an AOT cache trained by a full run. Each alternative below is one fresh process, so
these figures are exploratory, not a ranking. Default G1 shows medians across its three processes, except
the pause column, which shows the largest pause across all three. No measured path missed a tick deadline.

| Configuration | FFM mean / p99 µs/tick | Process RSS MiB | Largest logged GC pause ms | VM creation ms |
|---|---:|---:|---:|---:|
| Default G1 | 23.18 / 55.42 | 161.7 | 1.985 | 24.9 |
| Serial | 24.65 / 54.85 | 79.4 | 1.689 | 25.7 |
| ZGC | 27.31 / 85.99 | 297.5 | 0.160 | 69.7 |
| Generational Shenandoah | 22.56 / 43.57 | 312.4 | 0.396 | 30.2 |
| G1 + compact headers | 22.26 / 42.27 | 159.7 | 1.513 | 43.5 |
| G1 + compact headers + AOT | 21.97 / 45.07 | 129.8 | 1.104 | 29.3 |

RSS is the entire Rust/JVM process at the end of the run, not the configured maximum Java heap. GC pauses
come from `-Xlog:gc*:file=...:uptimemillis` across the whole run, including the unpaced allocation-heavy
warmups. VM creation times only `JavaVM::new`, excluding fixture compilation, Java class initialization,
native linking and plugin loading; these values are not first-start times. The AOT cache was 14,336,000
bytes. Training and cache assembly run separately and are not included in the AOT production row.

Serial used the least RSS in this small workload; the concurrent collectors traded more RSS for shorter GC
pauses. Compact headers alone did not produce a clear RSS reduction relative to the default's 159.9–173.0
MiB range. AOT reuse worked with changed native addresses in a new process, but this run does not establish
faster plugin startup or time to steady state. Keep the production GC, heap, compact-header compatibility
and AOT policy subject to real plugin measurements.

### M1 Temurin vs. GraalVM Community Edition — 1 November 2026

The same benchmark ran on Eclipse Temurin **25.0.5+8-LTS** (C2) and GraalVM Community Edition **25.4.4.1.1**
(OpenJDK 25.0.4.1.1, Graal JIT through JVMCI as libgraal), both with default G1, on the host above (Linux
7.2 x86_64, Rust 1.99.0). The two JDKs differ in patch level, because the latest GraalVM CE build is based on
25.0.4. Each JVM ran in three fresh processes, alternating Temurin and GraalVM, with
`--cold-ticks 600 --ticks 200`, the default two-second warmups, `-XX:-UseDynamicNumberOfCompilerThreads` and
GC logging. Desktop applications were still running (1-minute load average 2–8 during the runs, and 22 at
the start of the first Temurin run). Every run passed the behavioral checks, and no run missed a tick
deadline. Figures are medians of the three processes, with their range.

| Measure | Temurin (C2) | GraalVM CE (Graal) |
|---|---:|---:|
| Shared-memory `x` read, ns/op | 1.86 (1.82–1.95) | 1.89 (1.89–1.95) |
| Shared-memory `getLocation()`, ns/op | 4.97 (4.87–7.58) | 2.30 (2.30–2.31) |
| Java → Rust `x` read, JNI, ns/op | 5.48 (5.17–6.65) | 5.34 (5.22–5.46) |
| Java → Rust `x` read, plain FFM, ns/op | 8.40 (8.10–8.76) | 5.44 (5.38–5.47) |
| Java → Rust `x` read, critical FFM, ns/op | 7.82 (7.61–8.08) | 3.96 (3.94–4.20) |
| Rust → Java ping, cached JNI, ns/call | 84.11 (82.27–99.65) | 85.97 (84.01–92.00) |
| Rust → Java ping, FFM upcall, ns/call | 20.69 (20.46–21.25) | 22.10 (21.76–24.01) |
| Warm ticks, cached JNI, mean / p99 µs | 35.73 / 57.60 | 34.47 / 58.05 |
| Warm ticks, FFM upcall, mean / p99 µs | 19.61 / 35.81 | 14.32 / 27.84 |
| Young GC pauses over the run | 269 (221–294) | 91 (86–94) |
| Largest logged GC pause, ms | 1.56 (1.46–2.36) | 1.66 (1.58–2.24) |
| Compilation time (MXBean), ms | 216 (211–252) | 719 (709–734) |
| CPU of the compiler threads, ms | 232 (227–270) | 739 (732–758) |
| Process CPU over the run, s | 21.01 | 21.53 |
| RSS at the end / peak RSS, MiB | 164.5 / 164.5 | 126.8 / 152.9 |
| VM creation, ms | 23.1 (20.8–49.9) | 44.5 (42.9–45.3) |

Cold FFM ticks run straight after VM creation and the self-test, before any other warmup. The table shows the
median, across processes, of each second's mean and maximum:

| Second after the self-test | Temurin mean / max µs | GraalVM mean / max µs |
|---|---:|---:|
| 1 | 444 / 1,273 | 472 / 1,449 |
| 2 | 65 / 250 | 263 / 1,127 |
| 3 | 17 / 37 | 88 / 206 |
| 4 | 15 / 28 | 24 / 107 |
| 5–30 (range of means) | 14–22 | 12–20 |

**Reading:** after warmup, Graal runs this fixture's code faster where it can inline across the FFM boundary
or remove allocations. Critical FFM reads take half the time, `getLocation()` no longer allocates its
`Location` record, and the full FFM tick is about 27% faster. The young collection count drops to about a
third, which suggests escape analysis removes allocations of this synthetic workload. Calls where the JIT
does not change the boundary (JNI, upcall entry) cost the same. In return, Graal reaches steady state about
two seconds later (40 ticks at 20 TPS), spends about three times as much CPU compiling, and doubles VM
creation time. Neither JVM missed a deadline, even when cold; the slowest cold tick (1.4 ms) is 3% of the
budget.

**JVM decision:** keep **Temurin as the default**. The steady-state gain is real but measured on an
allocation-light synthetic listener of about 15 µs per tick, which is 0.03% of a tick on either JVM; it
cannot outweigh the open compatibility question, slower warmup, extra compiler CPU and GraalVM's
monthly-release cadence. GraalVM CE remains the benchmark candidate. The decision is revisited with real
plugins in the M5 compatibility suite, which measures per-tick plugin time, warmup and behavioral
differences as described in [validation](#validation) step 5. Because both JDKs share HotSpot's runtime and
GC, the bridge design does not depend on the outcome.

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
