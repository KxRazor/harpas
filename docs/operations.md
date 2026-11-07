# Configuration and operations

Scope: where Harpas' settings live, and how a server is operated: console, logs, remote administration,
metrics, profiling, shutdown, hosting, and telemetry.

Status: partly implemented. `server.properties` with every 26.1 key, `harpas.toml` checking and its `eula` and `generation.features` settings, `HARPAS_*`
overrides, the console (`help`, `list`, `stop`), `logs/latest.log` with its archives, and stopping on SIGTERM and SIGINT
are built; the rest is design. Facts about vanilla cite their source in
[References](#references); everything else is a design decision of this project.

---

## Goals

| Goal | Meaning |
|---|---|
| **Drop-in migration** | A server folder from vanilla or Paper starts on Harpas without editing configuration |
| **Works with existing tools** | Hosting panels, RCON clients, server lists and management tools built for vanilla work unchanged |
| **Minimal configuration** | Defaults are right; settings are needed only for features that are inherently the owner's choice |
| **Private by default** | Harpas sends no data anywhere unless the owner enables it |

---

## Configuration files

Every setting lives in exactly one place.

| File | Contents |
|---|---|
| **`server.properties`** | Vanilla settings, with vanilla's keys, defaults and meaning: port, MOTD, `max-players`, `online-mode`, `view-distance`, `simulation-distance`, `network-compression-threshold`, `level-name`, `level-seed`, RCON, Query, the management server, and the rest [1] |
| **`harpas.toml`** | Harpas-only settings: `eula`, `generation.features` (new chunks run the FEATURES stage, which places only ores so far; off by default, [world.md](world.md#world-generation)), `[mods]`, `[proxy]`, `[network]`, Bedrock, per-plugin capability restrictions, metrics. It never repeats a key that belongs in `server.properties` |
| **`HARPAS_*` environment variables** | Override any setting from either file, for containers and hosting panels, with one naming convention (for example `HARPAS_EULA`, `HARPAS_RUNTIME_DIR`) |
| **`ops.json`, `whitelist.json`, `banned-players.json`, `banned-ips.json`, `usercache.json`** | Vanilla's files and formats |

Why `server.properties` stays:

- Server folders from vanilla and Paper work without conversion.
- Hosting panel templates commonly write the assigned port and address into `server.properties`
  *(verify for the main panels)*.
- Vanilla's management protocol reads and changes these same settings (see below), so keeping them in the same
  file keeps behavior identical.
- Plugins, tutorials and community documentation refer to it.

Paper's own configuration files (`bukkit.yml`, `spigot.yml`, `paper-*.yml`) configure Paper's engine and are not
read. If they are present, Harpas reports once which of their settings have an equivalent and where, and which
are intentionally not supported because they change vanilla behavior
([engine-core.md](engine-core.md#non-goals)). Paper's anti-xray settings map to Harpas' anti-xray
([world.md](world.md#anti-xray)).

---

## Server folder

Harpas uses the directory it is started from as the server folder, like vanilla and Paper. Its complete layout:

```
my-server/
├── server.properties         vanilla settings
├── harpas.toml               Harpas settings
├── eula.txt                  optional; a vanilla acceptance is honored (world.md)
├── ops.json, whitelist.json, banned-players.json, banned-ips.json, usercache.json
├── world/ (and other worlds) Anvil worlds (world.md)
├── plugins/                  Bukkit/Paper jars, Java native plugin jars, .wasm plugins
├── mods/                     Fabric or NeoForge mods; modded mode only (mod-support.md)
├── logs/                     latest.log and rotated logs
├── crash-reports/
└── .harpas/                  provisioned and generated files; safe to exclude from backups
    ├── runtime/<version>/    JVM, downloaded on demand (jvm.md)
    ├── bukkit-shim/          Bukkit shim, provisioned with the runtime (jvm.md)
    ├── aot-cache/            this server's JVM AOT cache (jvm.md)
    ├── vanilla-data/<version>.zip  data extracted from Mojang's server jar (world.md)
    ├── modded/               Mojang's server and the loader, modded mode only (mod-support.md)
    ├── translation-cache/    translated mod content, modded mode only (mod-support.md)
    ├── pregen/               saved pregeneration progress (qol.md)
    └── profiles/             profiler reports (qol.md)
```

Everything under `.harpas/` is recreated automatically if deleted, except saved pregeneration progress.

---

## Console and logs

- Interactive console with command history, completion and colors; it runs vanilla commands and plugin
  commands.
- Logs in `logs/latest.log`, rotated daily into compressed files, as vanilla does.
- Crash reports in `crash-reports/`.

---

## Remote administration

All three of vanilla's remote interfaces are supported, with vanilla's settings in `server.properties` [1]:

| Interface | What it is | Notes |
|---|---|---|
| **RCON** | Remote console over TCP (`enable-rcon`, `rcon.port`, default 25575, `rcon.password`) | RCON traffic is not encrypted [1]; Harpas warns when RCON listens on anything but localhost |
| **Query** | Server information over UDP (`enable-query`, `query.port`) | Used by server lists |
| **Server Management Protocol** | JSON-RPC 2.0 over WebSocket, TLS by default, authenticated with a 40-character secret, disabled by default (`management-server-enabled`) [2] | Methods for players, allowlist, bans, IP bans, operators, server status, saving and stopping, server settings and game rules, plus pushed notifications; available since 1.21.9 and extensible with custom namespaces [2] |

Harpas implements the management protocol completely and adds a `harpas:` namespace with Harpas-specific data:
loaded plugins, tick timings, metrics, profiler control, and players by edition and protocol version.

---

## Metrics, profiling and pregeneration

Built in and available by default; see [qol.md](qol.md): Prometheus metrics (enabled on `127.0.0.1`, with names
compatible with existing Paper dashboards), health and lag-spike reports, a sampling profiler across the engine,
Java and WASM plugins, timings, and world pregeneration.

---

## Shutdown and health

- On SIGTERM, SIGINT or SIGHUP, as on the console's `stop`, Harpas closes new connections, disconnects every
  player with vanilla's "Server closed", stops the logic thread, gives connections five seconds to close and
  exits with status 0. Saving every world joins this sequence with worlds. The embedded JVM leaves these signals to
  Harpas ([jvm.md](jvm.md#signals)).
- A health endpoint reports whether the server is running and accepting players, for containers and hosting
  panels.

---

## Hosting

- An official container image, with the server folder as a volume.
- Templates for common hosting panels.
- Everything Harpas provisions lives in the server folder's `.harpas/` directory
  ([Server folder](#server-folder)), so panels that persist only the server folder keep it.

---

## Telemetry

Harpas sends no telemetry. Plugins that include their own statistics libraries manage them themselves, as on
Paper.

---

## Open questions

1. Exact list of Paper settings that have an equivalent, for the one-time migration notice.

---

## Validation

1. **Migration:** server folders from vanilla and from Paper start on Harpas with their worlds, settings,
   operators and bans, without edits.
2. **Remote interfaces:** existing RCON clients, Query clients and a client of vanilla's management protocol
   work against Harpas.
3. **Shutdown:** stopping the server with SIGTERM during heavy activity saves every world with no loss.
4. **Panels:** the container image and panel templates run on the main hosting panels.

---

## References

1. minecraft.wiki, "server.properties" (keys and defaults, RCON and Query settings, management server settings). https://minecraft.wiki/w/Server.properties
2. minecraft.wiki, "Minecraft Server Management Protocol" (JSON-RPC over WebSocket, TLS, secret, namespaces, notifications, added in 1.21.9). https://minecraft.wiki/w/Minecraft_Server_Management_Protocol
