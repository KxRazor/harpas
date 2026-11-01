# Version support

Scope: which Minecraft versions Harpas supports, for the server and for clients, in normal and modded mode,
and how newer and older clients are handled.

Status: design. Nothing here is implemented. Facts cite their source in [References](#references);
everything else is a design decision of this project.

---

## Current focus: 26.x

Harpas targets the **26.x** line only, in every combination:

| Combination | Versions |
|---|---|
| Bukkit/Paper plugins only | 26.x |
| Native plugins (WASM, Java) only | 26.x |
| Mods only (modded mode) | 26.x |
| Bukkit/Paper plugins + mods | 26.x |
| Native plugins + mods | 26.x |

Older versions (1.21.x, 1.20.x and earlier) are out of scope for now, for every combination. They will be
considered later, together.

---

## Version names

Since 2026, Mojang numbers both editions by year: the first two digits are the year, and the rest is the
release within that year. Java releases go 26.1, 26.2, 26.3; Bedrock releases advance in steps of ten (26.10,
26.30, 26.50, …) with hotfixes in between [4][5]. The same number can therefore mean different things in each
edition, so these documents always name the edition.

Some sources still label Bedrock releases in the older form (minecraft-data stores 26.60 as `1.26.60`); Harpas
uses Mojang's current names everywhere it shows a version.

---

## Server version and client versions

Two things are separated:

- **Server version:** the game rules and world data the server runs. There is exactly one.
- **Client versions:** which clients can connect. There can be several, by translating packets between each
  client's protocol and the server's.

### Normal mode (Rust engine)

| | Version | How |
|---|---|---|
| **Server version** | **26.1** | The newest Java version with complete protocol and registry data in minecraft-data [1]. It moves up when minecraft-data adds a newer version |
| **Java clients 26.1** | Native | Codecs generated for protocol 775 ([protocol-codecs.md](protocol-codecs.md)) |
| **Java clients 26.2 and 26.3** | Translated: newer client, older server | Java 26.3 (protocol 777) is already out [2], and the launcher keeps players on the latest release, so most players need this |
| **Bedrock clients** | The current release and the two before it, counted by protocol number | One codec per protocol version; see [Bedrock window](#bedrock-window) |

Translating newer Java clients needs their protocol schema, which minecraft-data does not have yet for 26.2
and 26.3. Harpas writes those schemas from public protocol documentation [2] and **contributes them to
minecraft-data**, so minecraft-data stays the single source.

### Bedrock window

Bedrock clients update through each platform's store, when the player accepts the update, so players are not all
on the same release at the same time *(measure the spread from real connection data)*. Supporting only the
current release would turn away everyone who has not updated yet.

- **Window:** the current Bedrock release and the **two before it**, counted by **protocol number**. Minor
  releases that keep the same protocol number do not use a slot.
- **New releases first:** players who update first cannot join until their release is supported, so each new
  release is added as soon as possible, ideally within days, from minecraft-data's schema, which is current for
  Bedrock [1]; if it lags, Harpas contributes the update.
- **Rolling:** when a new release is added, the oldest one leaves the window.
- **No configuration:** the window is set by each Harpas release, not by server owners.
- **Clear messages:** a client below the window is told to update Minecraft; a client above it is told the server
  does not support its version yet.

Why it stays cheap:

- Codecs are generated from each version's schema ([protocol-codecs.md](protocol-codecs.md)).
- Hashed block network IDs keep block IDs stable across releases
  ([bedrock-interop.md](bedrock-interop.md#4-stable-block-network-ids)), removing one of the largest translation
  tables between releases.
- Shared encoding is per protocol version, so three releases cost at most three encodings of each packet.

### Modded mode

The server version is the one the mods require ([mod-support.md](mod-support.md#choosing-the-mod-runtime)),
limited to **26.x**. Java players use a client of that same version with the Harpas client mod; there is no
Java version translation in modded mode. Bedrock players join from their current Bedrock version, translated by
Harpas.

---

## How translation works

- **Per-version codecs**, generated from each version's schema.
- **Translation tables** between a client version and the server version: block states, items, entity types,
  sounds, particles, and the packets whose shape changed.
- **Newer client, older server:** the client knows everything the server has; content the server lacks is
  simply never sent.
- **Older client, newer server** (later): content the client does not know is replaced by the closest content it
  does know.
- **Shared encoding per protocol version.** A packet is encoded and compressed once **per protocol version**
  among its receivers, not once per player: 100 players viewing a chunk from three client versions cost three
  encodings. This is why translation lives in the core rather than in a plugin.

---

## Later: older versions

When older versions are taken on, for all combinations at once:

- **Older Java clients** (1.21.x, 1.20.x…) through translation from the server version, with substitutes for
  content they lack.
- **Java 1.8** is a separate case: its protocol is very different, and the combat change between 1.8 and 1.9 is
  the most common source of complaints on multi-version servers [3].
- **Mods for older versions:** each supported version line needs its own companion mod build (including its
  implementation of the core API), its own Harpas client mod build, and its own Java-to-Bedrock tables.

---

## Open questions

1. Policy for moving the server version up: immediately when minecraft-data adds a version, or after a
   stabilization period.

---

## Validation

1. A 26.1, a 26.2 and a 26.3 Java client play together on the same server; the parity suite passes for each.
2. Bedrock clients from every release in the window join and play; clients just below and just above the
   window get the right message.
3. Shared encoding: encodings per tick grow with the number of distinct protocol versions, not with players.

---

## References

1. PrismarineJS/minecraft-data, `data/pc/26.1/version.json` and `data/dataPaths.json`. https://github.com/PrismarineJS/minecraft-data/blob/master/data/pc/26.1/version.json
2. minecraft.wiki, "Java Edition protocol/Packets" (documents protocol 777 for Java 26.3). https://minecraft.wiki/w/Java_Edition_protocol/Packets
3. MineGuard, "ViaVersion + ViaBackwards: Multi-Version Support on One Minecraft Server" (combat change between 1.8 and 1.9 as the main complaint). https://mineguard.pro/en/blog/viaversion-viabackwards-cross-version-setup
4. Windows Central, "Minecraft is changing how it handles update version numbers" (December 2025 announcement). https://www.windowscentral.com/gaming/minecraft/minecraft-changing-update-version-numbers-december-2025
5. Neowin, "Mojang is simplifying the Minecraft version numbering system on Bedrock, Java, and snapshots". https://www.neowin.net/news/mojang-is-simplifying-the-minecraft-version-numbering-system-on-bedrock-java-and-snapshots/
