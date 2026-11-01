# Commands and permissions

Scope: how commands from vanilla and from every kind of plugin are registered, executed and shown to Java and
Bedrock players, and how permissions decide who may run them.

Status: design. Nothing here is implemented. Facts cite their source in [References](#references);
everything else is a design decision of this project.

---

## Goals

| Goal | Meaning |
|---|---|
| **One command system** | Vanilla, Bukkit and native plugins share one command tree, with no special cases per plugin type |
| **Familiar permissions** | Existing permission plugins manage every plugin, including native ones |
| **Both editions** | Java and Bedrock players get the command list and suggestions |

---

## One Brigadier tree

All commands live in **one command tree**, in the style of vanilla's Brigadier dispatcher:

- **Vanilla commands**, ported with their argument types, selectors and suggestions.
- **Bukkit plugins**, through the classic command API and through Paper's Brigadier-based command API.
- **Native plugins** (WASM and Java), through the native API ([plugin-api.md](plugin-api.md)).
- **Namespaced fallbacks**, as on Bukkit and Paper: every plugin command is also reachable as
  `<plugin>:<command>`, so two plugins with the same command name stay usable.

The tree is sent to Java clients with suggestions, filtered by the player's permissions, as on Paper. Commands
from the console, RCON, command blocks and functions run through the same tree.

---

## Bedrock players

Bedrock has its own command list format (`AvailableCommandsPacket`) [1]. Harpas translates the tree into it:

- Commands and their arguments are listed with suggestions where Bedrock has an equivalent argument type.
- Arguments with no Bedrock equivalent are accepted as free text and parsed by the tree on the server.
- The list is filtered by the player's permissions, as for Java.

---

## Permissions

Permissions are **string nodes in Bukkit's style** (for example `essentials.home`), shared by every kind of
plugin:

- **Bukkit plugins** use Bukkit's permission API as on Paper.
- **Native plugins** declare their permission nodes and defaults in their metadata, and check them through the
  native API.
- **Vanilla operator levels** (0 to 4, from `ops.json`) map onto nodes for vanilla commands, so operators
  behave as in vanilla.
- **Defaults** (everyone, operators only, nobody) come from the declaring plugin, as on Bukkit.
- **Permission plugins** for Bukkit therefore manage permissions for WASM and Java native plugins as well.

---

## Open questions

1. Exact mapping between vanilla operator levels and permission nodes for vanilla commands.

---

## Validation

1. A Bukkit plugin, a WASM plugin and a Java native plugin register commands that appear together in Java's
   suggestions and in Bedrock's command list.
2. A Bukkit permission plugin grants and denies commands of all three plugin types.
3. Two plugins with the same command name are both reachable through their namespaced form.

---

## References

1. Mojang/bedrock-protocol-docs, `AvailableCommandsPacket.json`. https://github.com/Mojang/bedrock-protocol-docs/blob/main/json/AvailableCommandsPacket.json
