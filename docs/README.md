# Harpas documentation

One document per integration. Each one states its scope, goals, design, trade-offs, open questions and how
it will be validated, with sources for every external claim. The build order is in [ROADMAP.md](../ROADMAP.md).

| Document | Scope | Status |
|---|---|---|
| [engine-core.md](engine-core.md) | Engine core: vanilla tick order on one logic thread, unobservable parallel work, vanilla-shaped storage | In progress |
| [parity.md](parity.md) | Parity with vanilla: what is compared and how, coverage per subsystem, running the comparisons | In progress |
| [versions.md](versions.md) | Version support: 26.x focus for every combination, server vs. client versions, translation | Design |
| [world.md](world.md) | World: Anvil save format, vanilla data provisioning, generation | In progress |
| [java-connection.md](java-connection.md) | Java connections: states, authentication, encryption, chat signing, abuse protection | In progress |
| [identity.md](identity.md) | Player identity: UUIDs and names for both editions, Floodgate compatibility | Design |
| [commands.md](commands.md) | Commands and permissions: one Brigadier tree, Bukkit-style nodes, Bedrock command list | Design |
| [jvm.md](jvm.md) | Embedded JVM for Bukkit/Paper plugins: bridge design, threading, JVM tuning | Design, measured in M1 |
| [bedrock-interop.md](bedrock-interop.md) | Java ↔ Bedrock interoperability: shared encoding, blob cache, authentication, skins, hard problems | Design |
| [nethernet.md](nethernet.md) | NetherNet transport for Bedrock: HTTP signaling, WebRTC data channels, robustness, performance | Prototype |
| [protocol-codecs.md](protocol-codecs.md) | Java and Bedrock packet codecs generated from our own schema; currently supported client versions | In progress |
| [networks.md](networks.md) | Server networks: Velocity/BungeeCord forwarding, Bedrock transfers, Harpas networks without a proxy | Design |
| [plugin-api.md](plugin-api.md) | Native plugin API: WIT definition, WASM and JVM hosts, languages, sandbox | Design |
| [mod-support.md](mod-support.md) | Fabric/NeoForge modded mode (26.x), plugins with mods, mod content for Java and Bedrock players without the modpack | Design |
| [operations.md](operations.md) | Configuration files, console and logs, RCON, Query, management protocol, metrics, timings, hosting | Partly implemented |
| [qol.md](qol.md) | Built-in tools: Prometheus metrics, health and lag-spike reports, profiler, timings, world pregeneration | Design |
| [releases.md](releases.md) | Versioning (SemVer), release channels, release process, support policy | Decided |
| [licensing.md](licensing.md) | Licenses of Harpas' code, the GPL-3.0 Bukkit shim, how contributions are licensed | Decided |
| content.md | Custom content for the Rust engine, for vanilla Java and Bedrock clients | Paused |
