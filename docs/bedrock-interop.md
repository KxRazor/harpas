# Bedrock interoperability

Scope: how Harpas lets Bedrock Edition clients play on the same server, in the same worlds, as Java Edition
clients, at a fraction of the cost of running a separate translation proxy.

Status: design. Nothing here is implemented. Every number or behavior marked *(measure)* or *(verify)* must
be confirmed before the design is frozen. Claims about the Bedrock protocol, the Java client or other
projects cite their source in [References](#references); everything else is a design decision of this
project.

---

## Currently supported clients

| Edition | Version | Protocol |
|---|---|---|
| **Java** | 26.1 | 775 |
| **Bedrock** | 26.60 | 2225 |

Supported versions follow the protocol data in minecraft-data; see
[protocol-codecs.md](protocol-codecs.md#currently-supported-clients) and the version policy in
[versions.md](versions.md).

---

## Goals

In priority order. When two goals conflict, the higher one wins.

| # | Goal | Meaning |
|---|---|---|
| 1 | **Compatibility** | Unmodified Bedrock clients join and play; Java and Bedrock players see and affect the same world |
| 2 | **Functionality** | Gameplay works the same for both editions wherever Bedrock can represent it |
| 3 | **Performance** | Bedrock support costs a small fraction of a separate proxy, and nothing when it is disabled |

### Principles

- **The internal model is the Java one.** The engine simulates Java Edition rules. Bedrock is a client
  protocol translated at the edge, not a second simulation.
- **Translate once, share with everyone.** Anything identical for several Bedrock players is encoded once.
- **Disable means zero cost.** Bedrock support is a feature switch, not an optimization switch. When it is off,
  nothing related to Bedrock is loaded or kept in memory.

### Non-goals

- Bedrock-only gameplay rules. Where the editions differ, the Java rule applies.
- Bedrock add-ons that change client behavior with scripts. Custom content for mods is covered in
  [mod-support.md](mod-support.md); custom content for the Rust engine is not designed yet (`content.md`).

---

## Why an integrated design

The common way to let Bedrock clients join a Java server is a translation proxy that connects to the server
as one Java client per Bedrock player. Building the translation into the server avoids the costs that come
with that shape:

| Proxy cost | Integrated design |
|---|---|
| Packets are encoded as Java packets, decoded again, translated and encoded as Bedrock packets | Bedrock packets are built directly from engine state |
| Each Bedrock player has its own copy of the chunks, entities and inventories it can see | Sessions read the engine's state; only truly per-player state is kept per session |
| The same chunk seen by N Bedrock players is translated N times | Each chunk section is encoded once and shared |
| Identity and skins need extra components on the Java server | Authentication and skin handling are part of the server |

The [Validation](#validation) section compares memory and CPU against a proxy setup with measurements.

---

## Architecture

```
┌───────────────────────────── harpas process ─────────────────────────────┐
│                                                                          │
│   Java clients ── TCP ──► harpas-protocol-java ─┐                        │
│                                                 ├──► engine (Java model) │
│   Bedrock clients ─ UDP ─► harpas-raknet ─► harpas-protocol-bedrock      │
│                                        │        │                        │
│                                        ▼        │                        │
│                                 harpas-bedrock ─┘                        │
│                     (translation, shared encode cache, sessions)         │
└──────────────────────────────────────────────────────────────────────────┘
```

- **`harpas-raknet`**: the RakNet transport, described in [raknet.md](raknet.md).
- **`harpas-protocol-bedrock`**: batching, compression, encryption, packet codecs. No game logic.
- **`harpas-bedrock`**: translation between the engine and Bedrock packets; per-session state that is truly
  per player (inventory views, movement validation, form state); the shared encode cache.

A Bedrock session reads the engine's state directly. There is no intermediate Java client, no Java packet
encoding for Bedrock players, and no copy of the world per session.

---

## Efficiency techniques

### 1. Shared chunk encoding

Each chunk section is encoded to the Bedrock format once per version of that section (a counter incremented
whenever the section changes). All Bedrock players reuse the encoded bytes. Cost scales with the number of
distinct sections in view, not with the number of players viewing them.

### 2. Sub-chunk requests

Since 1.18.10, the server can send "skeleton" `LevelChunkPacket`s (sub-chunk count `0xffffffff`), and the
client requests individual 16×16×16 sections with `SubChunkRequestPacket`; the server answers with
`SubChunkPacket`, which carries terrain, block entity NBT and heightmaps [1][2]. Harpas uses this mode so
clients only fetch the sections they need, served from the shared encode cache.

### 3. Client blob cache

Bedrock clients can cache chunk data as content-addressed blobs, each identified by the XXHash64 of its
serialized bytes. The client reports hits and misses (`ClientCacheBlobStatusPacket`) and the server sends
only the missing blobs (`ClientCacheMissResponsePacket`). The cache persists across sessions [3][4], so a
returning player does not download unchanged terrain again.

Constraints:

- Blobs must not contain runtime IDs that change between sessions [4]. See technique 4.
- The server must keep the blobs referenced in a transaction available until the client answers; vanilla
  allows between 1 and 8 concurrent transactions depending on connection quality [4]. Blob hashes are
  computed once, alongside the shared encoding.

### 4. Stable block network IDs

Bedrock lets the server tell the client, in `StartGame`, that block network IDs are hashes rather than indices
into the expected block palette [5][16]. The hash is computed from the block state's canonical NBT *(verify the
exact algorithm; FNV-1a is the expected one)*. Harpas uses hashed IDs from the start, which makes encoded
sections stable across sessions and versions, as the blob cache requires, and covers custom blocks without a
version-specific palette. The trade-off is that every block in a sub-chunk palette takes a few more bytes [16]:
on a synthetic overworld column, 1.3% more before compression and 2.7% more after it (2,928 bytes instead of
2,852; [protocol-codecs.md](protocol-codecs.md#shared-encoding-results)). Hash collisions are checked when the
mapping table is built.

### 5. Encode and compress once per audience

Packets that are identical for every Bedrock viewer (entity movement, block updates, particles, sounds) are
encoded once. Compression can be shared too, because of how a batch is built on the wire [6][7][8]:

1. Packets are length-prefixed and concatenated into a batch.
2. Once compression is enabled, a byte selects the algorithm (`0` zlib, `1` snappy, `0xff` none); batches
   below the negotiated threshold are sent uncompressed with `0xff`.
3. Encryption, when enabled, comes after compression: an 8-byte checksum (the first 8 bytes of SHA-256 over
   the per-connection send counter, the compressed data and the key) is appended, then the result is
   encrypted with AES-256 in CTR mode using a per-connection key.

Compression depends only on the batch contents, the algorithm and the level. The checksum and encryption
depend on the connection. So Harpas:

- uses the same compression algorithm, level and threshold for every Bedrock connection;
- sends shared packets in their own batch, compressed once;
- does only the checksum and AES-CTR pass per connection, which is much cheaper than compressing.

Trade-off: a shared batch cannot be merged with that player's own packets, so there are more batches per
tick. Measured on one chunk column, sharing wins from the second viewer: 104 µs per viewer for two viewers
instead of 203 µs, and 3.8 µs with 200 viewers, where the per-connection checksum and AES-CTR pass is all that is
left ([protocol-codecs.md](protocol-codecs.md#shared-encoding-results)). The cost of the extra batches is the
checksum, encryption and RakNet framing of a small batch *(measure)*.

### 6. Native authentication

The server verifies the Xbox Live identity chain sent by the client and derives the player's UUID from the
XUID, compatibly with Floodgate; names get a prefix to avoid collisions with Java players
([identity.md](identity.md)). No extra plugin is needed. *(verify the exact chain format against the protocol docs for the target
version)*

### 7. Zero cost when disabled

Bedrock settings live in `harpas.toml`:

```toml
[bedrock]
enabled = true   # default
port = 19132     # UDP, Bedrock's default
```

With `enabled = false`:

- The UDP listener is not opened.
- Mapping tables (blocks, items, entities, biomes) are not loaded.
- The shared encode cache does not exist.
- Entities keep no Bedrock-specific metadata.

These are the only Bedrock settings a server owner needs. Turning the feature off does not trade
compatibility for speed.

---

## Hard problems

These are about correctness, not efficiency, and need their own design work.

### Movement

In server-authoritative mode the client sends `PlayerAuthInputPacket`; the server simulates the movement and
compares it with the client's predicted position, sending a correction if they diverge.
`CorrectPlayerMovePrediction` is only sent when `StartGame` selects the authoritative mode with rewind [9][10].
Bedrock and Java movement physics are not identical, so Harpas must decide whether to simulate Bedrock
physics for Bedrock players or validate with tolerances against the Java simulation. A known Bedrock bug
enables server-authoritative block breaking whenever server-side movement is selected, regardless of the
`StartGame` flag (MCPE-122609, resolution "Incomplete") [11].

### Inventories

Bedrock uses server-authoritative inventory transactions (`ItemStackRequest`), and container screens open
only when the server sends `OpenContainerPacket` [9][10]. The server must assign item stack network IDs.
Mapping these transactions onto Java's container click model is the most laborious part of the translation.

### Skins

Bedrock players send their skin image to the server. Showing it to Java players is constrained by the Java
client. Verified in the 26.3 client and authlib 10.0.77 bytecode [12]:

- authlib discards a textures property whose URL is not on the texture domain advertised by Mojang's
  services discovery;
- for every player other than the local one, the client uses the skin only if the textures property is
  signed with Mojang's key; otherwise it shows the default skin.

So a Bedrock skin reaches Java players only as a Mojang-signed texture hosted by Mojang. Such textures can
only be produced by applying the skin to a real Mojang account, which is what skin signing services do.

Design:

- Java players see Bedrock skins through a signing service, with results cached by skin hash so each
  distinct skin is signed once and reused across players and restarts.
- While a skin is being signed, or if the service is unavailable, Java players see the default skin, as they
  would for any unsigned profile.
- Bedrock players see every skin directly: Bedrock skins as sent, and Java skins downloaded from Mojang's
  texture server and forwarded.

### Protocol versions

The Bedrock protocol changes with every release [13], and Bedrock clients update automatically. Harpas
supports a window of releases and adds new ones quickly; the policy is in
[versions.md](versions.md#bedrock-window).

### Chat

Bedrock players cannot sign chat messages, so their messages reach Java players as system messages, always
visible; Java players keep signed chat among themselves ([java-connection.md](java-connection.md#chat-signing)).

### Feature gaps

Some Java features have no Bedrock equivalent and vice versa. Each gap is handled by the closest
representation, a resource-pack-based substitute, or documented as unsupported. The list is built from
testing, not assumed.

---

## Protocol codecs

Bedrock packet codecs are generated from our own protocol schema. The schema, the generator, the
hand-written exceptions, testing and the licensing constraints are described in
[protocol-codecs.md](protocol-codecs.md).

---

## Dependencies and sources of truth

| Need | Option | Notes |
|---|---|---|
| Protocol reference | Mojang/bedrock-protocol-docs [13] | Official, for server partners. Read only; its files never enter the repository or the build (see [protocol-codecs.md](protocol-codecs.md)). Microsoft states the protocol is not a supported API and has no backward-compatibility guarantee [14] |
| Block, item and entity mappings | GeyserMC/mappings [15] | MIT licensed (copyright GeyserMC), actively maintained. Contains block, item, biome, particle, sound and collision data. Reuse requires keeping the copyright and license notice in `THIRD_PARTY_NOTICES.md` |
| RakNet transport | Our own, `harpas-raknet` | See [raknet.md](raknet.md) |
| Bedrock protocol codecs | Generated from our own schema | See [protocol-codecs.md](protocol-codecs.md) |
| Skin signing | External signing service | Required for Java viewers (see [Skins](#skins)) |

---

## Open questions

1. Bedrock movement: simulate Bedrock physics, or validate against Java physics with tolerances?
2. Skin signing: which service, its rate limits, and what is sent to it (the skin image only, never player
   identifiers)?

---

## Validation

1. **Memory and CPU against a proxy setup.** Same world, same view distance, 1/10/50 Bedrock bots: a Java
   server with a translation proxy vs. Harpas. Measure RSS, CPU per tick, and bytes sent per player.
2. **Shared encoding and compression.** Encode and compression cost, and cache hit rate, as the number of
   Bedrock viewers of the same area grows; batches per tick with and without shared batches.
3. **Blob cache.** Bandwidth for a returning player with and without the blob cache.
4. **Disabled mode.** RSS and startup time with `bedrock.enabled = false` must match a build without
   Bedrock support.
5. **Gameplay parity.** Scripted scenarios (movement, combat, inventories, containers, block placing and
   breaking) run with a Java and a Bedrock client side by side.

Results are recorded in this document and replace the *(measure)* markers.

---

## References

1. Mojang/bedrock-protocol-docs, `LevelChunkPacket` and `SubChunkRequestPacket` schemas (sub-chunk request mode). https://github.com/Mojang/bedrock-protocol-docs/tree/main/json
2. Mojang/bedrock-protocol-docs, `LevelChunkPacket.json`. https://github.com/Mojang/bedrock-protocol-docs/blob/main/json/LevelChunkPacket.json
3. Mojang/bedrock-protocol-docs, `ClientCacheMissResponsePacket.json` and `ClientCacheBlobStatusPacket.json`. https://github.com/Mojang/bedrock-protocol-docs/tree/main/json
4. Tomcc, notes on the Bedrock blob cache. https://gist.github.com/Tomcc/4be79d3eafcd158c5059abd4ab2e8d35
5. Mojang/bedrock-protocol-docs, `StartGamePacketPayload.json` (field "BlockNetworkIds Are Hashes"). https://github.com/Mojang/bedrock-protocol-docs/blob/main/json/StartGamePacketPayload.json
6. gophertunnel, `minecraft/protocol/packet/encoder.go` (batch layout, compression byte, threshold). https://github.com/Sandertv/gophertunnel/blob/master/minecraft/protocol/packet/encoder.go
7. gophertunnel, `minecraft/protocol/packet/encryption.go` (checksum and AES-CTR encryption). https://github.com/Sandertv/gophertunnel/blob/master/minecraft/protocol/packet/encryption.go
8. gophertunnel, `minecraft/protocol/packet/compression.go` (algorithm IDs). https://github.com/Sandertv/gophertunnel/blob/master/minecraft/protocol/packet/compression.go
9. Mojang/bedrock-protocol-docs, `PlayerAuthInputPacket.json`. https://github.com/Mojang/bedrock-protocol-docs/blob/main/json/PlayerAuthInputPacket.json
10. Mojang/bedrock-protocol-docs, `additional_docs/` (server-authoritative movement and inventory guides). https://github.com/Mojang/bedrock-protocol-docs/tree/main/additional_docs
11. Mojang bug tracker, MCPE-122609. https://bugs-legacy.mojang.com/browse/MCPE-122609
12. Inspection of Minecraft Java Edition 26.3 client classes (`net.minecraft.client.resources.SkinManager`,
    `net.minecraft.client.multiplayer.PlayerInfo`) and `com.mojang:authlib:10.0.77`
    (`MinecraftServicesSessionService.unpackTextures`, `MinecraftServicesDiscoveryService.isAllowedTextureDomain`),
    downloaded from Mojang's official launcher endpoints and read with `javap`. Not redistributed.
13. Mojang/bedrock-protocol-docs. https://github.com/Mojang/bedrock-protocol-docs
14. Microsoft Learn, Minecraft Creator documentation, "More info sources". https://learn.microsoft.com/en-us/minecraft/creator/documents/moreinfosources
15. GeyserMC/mappings (MIT). https://github.com/GeyserMC/mappings
16. gophertunnel, `minecraft/protocol/packet/start_game.go` (`UseBlockNetworkIDHashes`: block name hash instead of palette index; extra bytes per palette entry). https://github.com/Sandertv/gophertunnel/blob/master/minecraft/protocol/packet/start_game.go
