# Player identity

Scope: how players of both editions are identified (UUIDs and names), how name collisions between editions are
avoided, and what plugins see. Authentication itself is covered in [java-connection.md](java-connection.md) and
[bedrock-interop.md](bedrock-interop.md#6-native-authentication).

Status: design. Nothing here is implemented. Facts cite their source in [References](#references);
everything else is a design decision of this project.

---

## Goals

| Goal | Meaning |
|---|---|
| **Stable identity** | A player keeps the same UUID across sessions, so plugin data (permissions, economy, claims) follows them |
| **No collisions** | A Java player and a Bedrock player never share a UUID or a name |
| **Migration keeps data** | Servers that used the common Bedrock bridge keep their Bedrock players' data when moving to Harpas |

---

## Java players

- **Online mode:** the UUID and name come from Mojang's session server ([java-connection.md](java-connection.md#authentication)).
- **Proxy mode:** the UUID and name come from forwarding ([networks.md](networks.md#behind-a-java-proxy)).
- **Offline mode:** vanilla's offline UUIDs, derived from the name, as vanilla does.

---

## Bedrock players

### UUID

The UUID is derived from the player's XUID, verified through Xbox Live at login, in the same way as Floodgate:
the 128-bit UUID whose most significant 64 bits are zero and whose least significant 64 bits are the XUID [1].

- Stable: the XUID belongs to the Xbox account and does not change.
- Never collides with Java UUIDs issued by Mojang.
- **Compatible with servers that used Geyser with Floodgate**, so permissions, inventories and plugin data of
  their Bedrock players carry over.

### Name

Bedrock gamertags can contain spaces and can match a Java player's name. As Floodgate does by default [2]:

- A **prefix** is added to Bedrock names, `.` by default (`.Steve`). A prefix without letters or digits cannot
  produce a valid Java name, so it cannot collide.
- **Spaces are replaced** with `_`.
- Both are set in `harpas.toml`:

```toml
[bedrock]
username_prefix = "."
replace_spaces = true
```

- Java names are limited to 16 characters; a prefixed Bedrock name that would exceed it is shortened
  *(verify how Floodgate shortens, to stay compatible)*.

---

## What plugins see

- **Bukkit plugins:** a normal `Player` with the UUID and name above. The Floodgate API is also available, so
  plugins that ask whether a player is on Bedrock, or show them forms, work unchanged
  ([jvm.md](jvm.md#floodgate-api)).
- **Native plugins:** the edition, protocol version and, for Bedrock, the XUID of every player
  ([plugin-api.md](plugin-api.md#edition-aware-api)).

---

## Later: linking accounts

Linking a person's Java and Bedrock accounts so they share one identity (inventory, permissions) is out of scope
for the first version, and will be designed once basic identity works.

---

## Open questions

1. Account linking: design and when.

---

## Validation

1. A Bedrock player gets the same UUID across sessions, and the same UUID as on a server running Floodgate.
2. A Java and a Bedrock player with the same name are both online, with different names and UUIDs.
3. A world and plugin data migrated from a server with Geyser and Floodgate keep their Bedrock players' data.

---

## References

1. GeyserMC/Floodgate (MIT), `core/src/main/java/org/geysermc/floodgate/util/Utils.java` (`getJavaUuid`: `new UUID(0, xuid)`). https://github.com/GeyserMC/Floodgate/blob/master/core/src/main/java/org/geysermc/floodgate/util/Utils.java
2. GeyserMC/Floodgate, `core/src/main/resources/config.yml` (`username-prefix: "."`, `replace-spaces: true`). https://github.com/GeyserMC/Floodgate/blob/master/core/src/main/resources/config.yml
