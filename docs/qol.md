# Built-in quality-of-life tools

Scope: tools that server owners usually add as plugins, built into Harpas and available by default: Prometheus
metrics, a profiler with health and lag-spike reports, and world pregeneration. Configuration files, remote
administration and the rest of day-to-day operation are covered in [operations.md](operations.md).

Status: design. Nothing here is implemented. Facts cite their source in [References](#references);
everything else is a design decision of this project.

---

## Why built in

- **Nothing to install.** These are among the first plugins most owners add; having them built in fits
  "works by default".
- **They see what plugins cannot.** Harpas knows each tick phase, the cost of the JVM bridge, players by
  edition and protocol version, Bedrock traffic, and the work running in parallel off the logic thread.
- **Profiling plugins may not work fully on Harpas.** Popular profilers for Paper rely on internals of HotSpot
  and of Paper that behave differently, or do not exist, on Harpas.

Metrics and profiling answer different questions:

| | Metrics | Profiler |
|---|---|---|
| Question | How is the server doing over time? | Where exactly is the time or memory going? |
| Data | Aggregated numbers sampled continuously (TPS, tick time, players, memory) | Call stacks sampled during a window, tick breakdowns, heap contents |
| Typical use | Dashboards and alerts | Investigating lag, a slow plugin or a memory leak |

Both are needed; the metrics do not replace the profiler.

---

## Prometheus metrics

An HTTP endpoint in the Prometheus/OpenMetrics format, **enabled by default and bound to `127.0.0.1`**, so it is
reachable only from the machine itself. Exposing it on other addresses, or disabling it, is a setting in
`harpas.toml`.

### Names compatible with existing dashboards

Where an equivalent exists, metrics use the names of the most widely used Prometheus exporter plugin for Paper
[1], so existing Grafana dashboards work unchanged:

| Metric | Meaning |
|---|---|
| `mc_tps` | Server tick rate |
| `mc_tick_duration_median`, `mc_tick_duration_average`, `mc_tick_duration_min`, `mc_tick_duration_max` | Tick duration over recent ticks, in nanoseconds |
| `mc_players_online_total` | Online players per world |
| `mc_players_total` | Unique players |
| `mc_loaded_chunks_total` | Loaded chunks per world |
| `mc_entities_total` | Loaded entities per world |
| `mc_world_size` | World size in bytes |
| `mc_jvm_memory`, `mc_jvm_threads` | JVM memory and threads, when the JVM is running |

### Harpas metrics

Under the `harpas_` prefix:

- tick time per phase (mean and 99th percentile) and time on the logic thread vs. parallel work;
- players by edition and protocol version;
- network traffic per edition, and shared-encoding cache hits;
- Bedrock blob cache hits and misses;
- JVM bridge: events dispatched vs. skipped by interest masks, Rust → Java calls per tick;
- time per plugin (Java and WASM);
- garbage collection pauses, when the JVM is running.

### Existing exporter plugins

Exporter plugins written for Paper keep working through the Bukkit shim: Harpas reports real values to Paper's
API for TPS and tick times, so an owner who already runs an exporter and a dashboard changes nothing.

---

## Profiler

Available through commands and through the `harpas:` namespace of the management protocol
([operations.md](operations.md#remote-administration)).

### Health report

`harpas health`: TPS; tick time (mean, 95th percentile, maximum); process and system CPU; memory of the engine
by subsystem; JVM heap and garbage collection; players by edition; loaded chunks and entities per world.

### Tick monitoring and lag spikes

- Every tick is timed per phase; a tick over a threshold (by default, the 50 ms budget) is recorded as a
  **spike**, with its breakdown by phase, plugin, event listener, entity type and chunk.
- `harpas spikes` lists recent spikes and their breakdowns.

### Sampling profiler

`harpas profiler start` / `stop` samples call stacks for a time window across the three runtimes Harpas runs:

| Runtime | How |
|---|---|
| Rust engine | Stack sampling of the logic thread and worker threads, annotated with the current tick phase |
| Java plugins | JDK Flight Recorder, built into OpenJDK, with low-overhead method sampling [2][3] |
| WASM plugins | Wasmtime's guest profiler, which samples guest stacks while WebAssembly runs; samples are not taken during host calls [4] |

The three are merged into **one report on one timeline**. Reports use the Firefox Profiler's processed profile
format, which Wasmtime's guest profiler already produces [4], and are written to the server folder. They can be
opened locally; nothing is uploaded anywhere by default.

### Memory

- Engine memory by subsystem (chunks, entities, encoding caches, Bedrock translation, plugins).
- JVM heap summary by class, and heap dumps on request, using the JDK's own tooling.
- Garbage collection activity over time.

### Timings

`harpas timings`: time per tick phase, per plugin and per event listener over a window, as a plain report. It is
the quick view; the sampling profiler is the detailed one.

---

## World pregeneration

`harpas pregen` generates chunks ahead of time with the same pipeline as normal generation
([world.md](world.md#world-generation)):

- **Area:** shape (square or circle), center (coordinates or spawn) and radius, or the world border as the area.
- **Several tasks at once**, for different worlds or areas.
- **Pause and continue**, with progress saved to the server folder so it survives restarts.
- **Progress:** chunks done, percentage, rate and estimated time, in the console and in game.
- **Trim:** delete chunks outside a selection.
- **Does not lag players:** generation runs on worker threads, and its rate adapts so that the tick stays
  within budget while players are online.

---

## Open questions

1. Spike threshold and how many spikes are kept.
2. Whether profile reports should also be offered in other common formats.

---

## Validation

1. **Dashboards:** a Grafana dashboard built for the Paper exporter plugin shows correct data from Harpas'
   endpoint without changes.
2. **Exporter plugins:** the Paper exporter plugin running on Harpas reports the same TPS and tick times as the
   built-in endpoint.
3. **Profiler:** a deliberately slow Java plugin, WASM plugin and engine phase each show up correctly in one
   merged report.
4. **Overhead:** tick time with metrics on and with the profiler running, compared with both off.
5. **Pregeneration:** a large area pregenerates while players are online, with the tick within budget, and
   resumes after a restart.

---

## References

1. sladkoff/minecraft-prometheus-exporter (MIT), README, list of exported metrics. https://github.com/sladkoff/minecraft-prometheus-exporter
2. JEP 328: Flight Recorder (open-sourced in JDK 11). https://openjdk.org/jeps/328
3. Red Hat, "How to use JDK Flight Recorder (JFR)?" (starting recordings with `jcmd`, method sampling). https://access.redhat.com/solutions/662203
4. Wasmtime API documentation, `GuestProfiler` (Firefox processed profile format, sampling limits). https://docs.wasmtime.dev/api/wasmtime/struct.GuestProfiler.html
