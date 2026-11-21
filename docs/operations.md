# Configuration and operations

Scope: where Harpas' settings live, and how a server is operated: console, logs, remote administration,
metrics, profiling, shutdown, hosting, and telemetry.

Status: partly implemented. `server.properties` with every 26.1 key, `harpas.toml` checking and its `eula` setting, `HARPAS_*`
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
| **`harpas.toml`** | Harpas-only settings: `eula`, `[mods]`, `[proxy]`, `[network]`, Bedrock, per-plugin capability restrictions, metrics. It never repeats a key that belongs in `server.properties` |
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

### Whitelist, bans and operators

The four lists are loaded when the server starts, in vanilla's formats, and written back as vanilla writes them:
`banned-players.json`, `banned-ips.json` and `ops.json` always, `whitelist.json` only when it does not exist. The
entries come out in the order of vanilla's `HashMap`, with dates in the system's time zone and Gson's escaping,
so the files are the same as vanilla's. Unlike vanilla, a file that cannot be read is left as it is rather than
overwritten with what could be read of it.

A player who has authenticated is checked in the order of vanilla's `PlayerList.canPlayerLogin`, with its
messages:

1. A player ban: `multiplayer.disconnect.banned.reason` with the reason, or the default reason, and the end of
   the ban if it has one.
2. The whitelist, when `white-list=true`: `multiplayer.disconnect.not_whitelisted`. Operators skip it.
3. An IP ban: `multiplayer.disconnect.banned_ip.reason`. The key is the address as vanilla cuts it from
   `/address:port`, so an IPv6 address leaves only `[` and its first group, as in vanilla.
4. `max-players`: operators with `bypassesPlayerLimit` skip it.

A ban whose end has passed still matches at first, as in vanilla: the login that finds it fails without a message,
and the expired bans of that list are dropped until the server restarts, so the player's next login goes on. The
commands that change the lists come with the command tree ([commands.md](commands.md)); until then the files are
edited while the server is stopped.

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
