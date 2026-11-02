# JVM bridge prototype (M1)

Run from the repository root with a **JDK 25** (`java`, `javac` and `jar`). Set `JAVA_HOME` to use a specific
distribution for both compilation and the embedded `libjvm`; otherwise the tools are discovered on `PATH`.
No JDK is needed to build the Rust workspace. `jni` is a benchmark-only dependency.

```sh
cargo run --release -p harpas-bench --example jvm-bridge
```

The host compiles the synthetic Java fixture into `target/jvm-bridge/bridge.jar`, embeds the JVM, and attaches
one thread for the whole run. The JAR is reused while its source is unchanged, including between JVM vendors
and AOT training/production. Delete `target/jvm-bridge/bridge.jar` to force recompilation. Run instances
sequentially: they share these build artifacts.

The default run takes about 45 seconds. `--ticks N` changes the measured ticks per dispatch path (default
200), and `--warmup-ms N` changes the unpaced warmup per case (default 2000). `--self-test` runs only the
correctness checks, as CI does on Linux, Windows and macOS:

```sh
cargo run -p harpas-bench --example jvm-bridge -- --self-test
```

Every run checks native and shared reads, JNI and FFM teleports, both event dispatch paths, event cancellation
and mutation, visibility of immediate writes, thread identity and containment of listener exceptions. An
unexpected exception fails the run. The host releases its JNI references, detaches, and destroys the JVM at
the end so GC logs and AOT training are flushed.

## What is measured

- Java loops: baseline, shared `x` reads, the same Rust `x` read via JNI and plain/critical FFM, and
  `getLocation()` reading three coordinates. Each case warms up separately and reports the median of seven
  100,000-operation batches. A volatile checksum consumes the result. Values include loop/slot-selection
  cost; the baseline is reported without subtraction. The static final method handles use `invokeExact`.
- Rust → Java: a cached JNI `(long) -> long` call including `jni`'s exception check, and an FFM upcall to the
  same method, on the already attached thread. After warmup, seven batches follow a 60 ms calibration;
  the result is the median per-call time.
- Synchronous ticks: each path warms up unpaced, then runs 200 player movement events per tick at 20 TPS,
  with three synthetic listeners in registration order. They cancel every eighth player's move, modify its
  destination, read the shared location, and teleport every sixteenth player (IDs 1, 17, …). Each event
  crosses separately; Rust reads its result and applies an uncancelled move before the next event. Mean
  and nearest-rank p99 include event-slot writes, calls, listeners and result application, excluding sleep.
  JNI is measured before FFM. Deadline overruns count ticks finishing after the scheduled next tick.
- Whole-run JVM collection counts/time, heap usage and compiler time from MXBeans. Linux also reports
  current and peak process RSS from `/proc/self/status`; use an external tool on other platforms. GC
  collection time is **not** a pause percentile. Enable unified logging below for actual pauses.

The views use native-endian C layouts: 200 records of three doubles (24 bytes each), followed by a 32-byte
event slot (`int player`, `int cancelled`, three doubles). Rust owns process-lifetime storage in `UnsafeCell`;
neither side keeps Rust references into it across calls. Only the attached logic thread accesses it. The
read leaf linked as `critical(false)` neither blocks, allocates nor calls Java. Teleports use plain FFM
because a production teleport can fire another Java event.

This is a synthetic API, licensed MIT OR Apache-2.0, without Bukkit/Paper dependencies. It is larger than the
usual contribution size because the native host, Java fixture and behavioral checks must agree on one ABI
to make the measurements reviewable together. It does not exercise real plugin compatibility, reentrant
events, background access, production world work or a warmed-up server's heap. Two seconds of warmup is a
controlled input, not a measurement of time to JIT steady state. Repeat in fresh processes on an idle host
and increase the duration when evaluating production choices.

## JVM configurations

Everything after the second `--` is passed unchanged as JVM options. The host defaults to `-Xrs -Xms64m
-Xmx256m --enable-native-access=ALL-UNNAMED`; appended options can override heap sizing. This fixture uses the
unnamed module, whereas the production design restricts native access to the shim module.

```sh
cargo run --release -p harpas-bench --example jvm-bridge -- --ticks 200 -- \
  '-Xlog:gc*:file=target/jvm-bridge/gc.log:uptimemillis'
```

Run the same command in separate processes with each of:

| Configuration | JVM options |
|---|---|
| Default HotSpot | none (G1 on the measured host) |
| Serial | `-XX:+UseSerialGC` |
| ZGC | `-XX:+UseZGC` |
| Generational Shenandoah | `-XX:+UseShenandoahGC -XX:ShenandoahGCMode=generational` |
| Compact headers | `-XX:+UseCompactObjectHeaders` |

For compact headers plus AOT, train with the full workload, then reuse the identical JDK, fixture JAR and
heap/header settings. AOT needs a JAR rather than a directory of class files:

```sh
cargo run --release -p harpas-bench --example jvm-bridge -- --ticks 200 -- \
  -XX:+UseCompactObjectHeaders -XX:AOTCacheOutput=target/jvm-bridge/bridge.aot
cargo run --release -p harpas-bench --example jvm-bridge -- --ticks 200 -- \
  -XX:+UseCompactObjectHeaders -XX:AOTCache=target/jvm-bridge/bridge.aot -Xlog:aot=info
```

Use fresh cache/log filenames for each JDK or fixture revision. Record the embedded runtime version printed
by the benchmark, CPU/OS, Rust version, flags, warmup, ticks, and results. The measured results and their
architectural implications are in [`docs/jvm.md`](../../../../docs/jvm.md#validation).
