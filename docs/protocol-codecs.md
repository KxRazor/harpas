# Protocol codecs

Scope: how Harpas produces the code that encodes and decodes Java Edition packets (`harpas-protocol-java`) and
Bedrock packets (`harpas-protocol-bedrock`): the protocol schema they are generated from, the generator, the
hand-written exceptions, testing, and the constraints on the sources. Bedrock transport is covered in
[nethernet.md](nethernet.md); translation and gameplay in [bedrock-interop.md](bedrock-interop.md).

Status: prototype. The Java 26.1 Handshaking, Status and Login codecs are generated from the schema
([Prototype results](#prototype-results)); Configuration, Play and Bedrock are design. Facts about sources cite
them in [References](#references); everything else is a design decision of this project.

---

## Currently supported clients

| Edition | Version | Protocol | Source |
|---|---|---|---|
| **Java** | 26.1 | 775 | minecraft-data [3][9] |
| **Bedrock** | 26.60 | 2225 | minecraft-data [3][10] |

The full version policy, including translated 26.2 and 26.3 Java clients, is in [versions.md](versions.md).
Supported versions follow the protocol data available in minecraft-data. When it adds a new release, the schema
is updated and the codecs are regenerated. For Java this means Harpas can trail the latest game release: Java
26.3 (protocol 777) is out [11], while minecraft-data's complete Java data currently stops at 26.1.

---

## Goals

In priority order. When two goals conflict, the higher one wins.

| # | Goal | Meaning |
|---|---|---|
| 1 | **Correctness** | Every codec matches what real clients send and expect, byte for byte |
| 2 | **Clean sources** | Nothing in the repository or the build depends on Mojang's files or code |
| 3 | **Maintainability** | A new release means updating a schema, not reviewing hundreds of hand-written codecs |
| 4 | **Performance** | Encoders support encoding and compressing a packet once for every player who receives it |

### Why generate

The Bedrock protocol has about 240 packets [1] and changes with every release [2]; the Java protocol changes
with every release too. Writing and maintaining every codec by hand is slow, and each release would mean
reviewing them all again. Harpas generates its codecs from a schema instead.

---

## Our own schema

- Each protocol crate keeps a **machine-readable protocol schema in the repository**, in a format of our own and
  under the project's license. It describes packets, types, enums and how fields are serialized, per protocol
  version.
- The schemas are built from permissively licensed community sources:
  - **PrismarineJS/minecraft-data** (MIT [3]) for both editions. For Bedrock, `data/bedrock/<version>/proto.yml`,
    a ProtoDef description created from community protocol documentation [4]; for Java, the per-version
    protocol data under `data/pc/` [9].
  - **gophertunnel** (MIT) [5] and **CloudburstMC/Protocol** (Apache-2.0) [6]: Bedrock reference
    implementations used to cross-check fields.
- Attribution for every source goes in [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md).

### Format

The schema is TOML, in `crates/harpas-protocol-<edition>/schema/<version>/`: a `version.toml` with the version
name and protocol number, and one file per connection state (`handshaking.toml`, `status.toml`, …). A state file
has:

- `[[clientbound]]` and `[[serverbound]]` packets, each with its ID, Rust name, minecraft-data name, doc and
  fields;
- `[structs.<Name>]` and `[enums.<Name>]` used by those packets;
- `[[hand_written]]` entries for minecraft-data packets that are not generated, each with the reason.

```toml
[[clientbound]]
id = 0x02
name = "LoginSuccess"
mcdata = "success"
doc = "Ends login with the player's profile; the client answers with Login Acknowledged."
fields = [
    { name = "uuid", type = "uuid" },
    { name = "username", type = "string(16)" },
    { name = "properties", type = "array(16, ProfileProperty)" },
]
```

Field types are short expressions: fixed-size types (`bool`, `u8` to `i64`, `f32`, `f64`, `varint`, `varlong`,
`uuid`), `string(max)`, `bytes(max)`, `rest(max)`, `option(T)`, `array(max, T)`, and the names of structs and
enums. Limits are vanilla's, taken from the protocol documentation [7]: a string limit counts UTF-16 code units,
as vanilla does, and a string without one gets vanilla's default of 32767. Byte arrays and arrays without a
limit are bounded by the packet's own length.

Names are Harpas' own: Rust names follow the packet names of the community protocol documentation [7], and each
packet keeps its minecraft-data name only to be compared with it.

---

## Codegen

- A generator (`harpas-protocol-codegen`) turns each schema into Rust codecs at build time, from the protocol
  crate's `build.rs`, one module per supported protocol version (`v26_1`), with one module per state and per
  direction inside it. Each packet is a struct with its `ID`; each direction has a `Packet` enum that encodes
  and decodes any of its packets. Differences between versions are visible as differences in the schema.
- The generated code calls a small hand-written runtime (`codec.rs`): one read and one write function per wire
  type, the `Encode` and `Decode` traits and `DecodeError`.
- `cargo run -p harpas-protocol-codegen -- check <schema-version-dir> <protocol.json>` compares a schema with
  minecraft-data: the packets in each state and direction, their IDs, and the sequence of wire types in each
  packet. Every minecraft-data packet must be generated or listed as hand-written. A test runs the same check
  against a committed subset of minecraft-data, so the schema cannot drift from its source unnoticed.
- Generated encoders write into reusable buffers and keep no per-player state, so one encoded packet can be
  shared by every player who receives it.
- Generated decoders validate lengths and bounds before allocating, since they parse untrusted network input.
  Every length is checked against its limit and against the bytes left, and every array element counts as at
  least one byte, so no packet makes a decoder allocate, or loop, more than the packet's own size. A decoded
  packet must use all of its bytes.

### Hand-written exceptions

Packets and types whose structure the schema cannot express cleanly are written by hand.

**Bedrock:**

- chunk and sub-chunk payloads;
- network NBT;
- item stack serialization;
- entity metadata;
- packets with flag-dependent fields, such as `PlayerAuthInputPacket` and `ItemStackRequest`.

**Java:**

- the legacy server list ping, sent by pre-1.7 clients before any framing;
- chunk data and light data;
- item stacks and their data components;
- text components, sent as NBT;
- entity metadata;
- registry data sent during the Configuration state;
- the command tree.

Hand-written codecs go through the same tests as generated ones.

---

## Java wire format

The parts of the Java protocol that shape the codec and connection design [7][8]:

- **States:** Handshaking, Status, Login, Configuration and Play.
- **Framing:** each packet is a length (VarInt), a packet ID (VarInt) and its data.
- **Compression, per packet:** once the server sends Set Compression, every packet carries a data length
  field. Packets at or above the threshold are compressed with zlib; smaller ones are sent uncompressed with a
  data length of 0.
- **Encryption:** AES/CFB8, with the shared secret as both key and IV. The cipher is a continuous stream across
  the whole connection and covers everything, including the length fields.

Consequences:

- **Compression can be shared.** Each packet is compressed on its own and encryption comes afterwards, so a
  packet sent to many players (chunk data, entity movement) is encoded and compressed **once**; only encryption
  is done per connection. Unlike Bedrock batches, there is no need to separate shared packets from each player's
  own packets.
- **Encryption has a real CPU cost.** CFB8 runs one AES block operation per byte rather than per 16 bytes,
  and each operation waits for the previous one, so hardware AES cannot run them in parallel. Measured: 42 MB/s
  with the RustCrypto `cfb8` crate and 70 MB/s with AES-NI instructions written directly, about 65 µs per
  compressed chunk column ([Shared encoding results](#shared-encoding-results)). Once compression is shared,
  encryption is most of the cost of each extra online-mode viewer; a hand-written AES-NI CFB8 is the optimization
  to try first.
- **The zlib implementation is our choice.** The protocol requires zlib-format data, not a specific library.
  Harpas uses **zlib-rs** (pure Rust, Zlib license), at level 6 like vanilla's default: it gave the smallest
  output and was 2.8 times faster than miniz_oxide, the default backend of `flate2`, at the same level.
  libdeflate was faster on Bedrock data but needs a C build; it remains an option if compression shows up in
  profiles.

---

## Testing

- **Round trips:** every codec decodes and re-encodes captured packets byte for byte.
- **Cross-checks (Bedrock):** the same packets are encoded by gophertunnel or CloudburstMC/Protocol and
  compared.
- **Fuzzing:** every decoder, generated or hand-written.
- **Real clients:** captures from current Java and Bedrock clients on each supported protocol version.

---

## What we do not do with Mojang's files

Mojang publishes JSON Schemas for the whole Bedrock protocol (about 1,000 files, 239 of them packets) [1]. Their
license states "(c) Mojang AB. All rights reserved" and that downloading them means agreeing to the Minecraft
EULA [12]. To keep the project free of those terms:

- Mojang's schema files are **never** committed, downloaded by the build, or used as input to the generator.
- Mojang's documentation is only **read** by developers to understand the protocol, which Microsoft describes
  as its purpose [13].
- Mojang's game code is not read to write codecs, and Mojang's jars, code and assets never enter the
  repository or the Harpas binary.

---

## Risks

- minecraft-data has no `LICENSE` file; the README states MIT and that the license "might change" if a source
  requires it [3]. Accepted: it is widely used under these terms, and the project tracks any change.
- Community data can lag a new release, by days for Bedrock and longer for Java. Accepted: supported versions
  follow minecraft-data, and Harpas can help by contributing updates to it.

---

## Prototype results

### Codec generation

M1 prototype: the Java 26.1 Handshaking, Status and Login codecs, generated from the schema, measured on
2026-11-01 against minecraft-data commit `bd8b5c6`.

| Measure | Result |
|---|---|
| Packets generated | 17 (1 Handshaking, 4 Status, 12 Login); 1 hand-written (legacy ping) |
| Schema size | 153 lines of TOML, against 449 lines of minecraft-data JSON for the same states |
| Generator | About 1,350 lines without tests (schema loader, Rust emitter, minecraft-data check); runtime about 290 lines |
| Generated code | 966 lines for the three states |
| Build cost | 4.1 s for a clean build of `harpas-protocol-java` with the generator and its dependencies (`serde`, `toml`, `serde_json`), 12 threads; regeneration after a schema change is under 0.1 s |
| Correctness | Byte-for-byte round trips against packets written by hand from the protocol documentation; the schema matches minecraft-data with no differences; a vanilla Java 26.1 client lists the example server with its MOTD, player count and latency, and joining it shows the Login Disconnect message with the player's name |
| Coverage of the rest of Java 26.1 | 137 of the 240 Configuration and Play packets use only types the format already has; 195 once it gains enums from mappers, bit fields, switches, registry holders and `lpVec3`; the remaining 45 need hand-written types (NBT, entity metadata) that the schema refers to by name |

### Decision: our own format, checked against ProtoDef

The schema uses its own format, and minecraft-data's ProtoDef stays the source it is checked against, not the
format it is written in. This closes the open question on the schema format. Reasons:

- **Limits.** ProtoDef carries no vanilla limits (username 16, cookie 5120 bytes, server address 255), and the
  decoders need them before allocating. They would have to live in a second file anyway.
- **Names and docs.** ProtoDef names come from minecraft-data (`success`, `encryption_begin`, used in both
  directions), not from the protocol documentation, and carry no docs. Rust types need stable names of their
  own, and the schema is where they are reviewed.
- **Semantics.** Several ProtoDef types are defined only by node-minecraft-protocol's implementation
  (`entityMetadataLoop`, `topBitSetTerminatedArray`, `registryEntryHolder`), and switches compare against
  relative paths such as `../flags`. Our format names the hand-written types instead of describing them.
- **Same maintenance.** The `check` command reduces both formats to wire types, so a new minecraft-data release
  still shows exactly which packets changed; updating the schema means editing those packets.

The cost is that our schema is maintained by hand rather than copied: for a new release, `check` lists the
packets to update. Whether a converter that drafts new packets from ProtoDef is worth writing is decided with
the Play state in M2.

### Shared encoding results

M1 prototype (`cargo run --release -p harpas-bench --example shared-encoding`), measured on 2026-11-01 on an AMD
Ryzen 5 5600H, single thread. The input is a synthetic overworld column: 24 sections with palettes of 7 to 9
blocks below the surface and single-value air above, full sky light above the ground. It is encoded from palette
indices, as the engine stores sections.

| Step | Java | Bedrock |
|---|---|---|
| Uncompressed column | 54,502 bytes | 17,821 bytes (hashed IDs) |
| Encode from palette indices | 24 µs | 26 µs |
| Compress, zlib-rs level 6 | 172 µs, 2,639 bytes | 173 µs, 2,928 bytes |
| Encrypt per connection | 65 µs (AES-128-CFB8) | 2.8 µs (SHA-256 checksum and AES-256-CTR) |

Compression by library and level, same column (µs, bytes):

| Library | Level 1 | Level 4 | Level 6 |
|---|---|---|---|
| miniz_oxide, Java | 42, 3,794 | 225, 2,818 | 489, 2,702 |
| zlib-rs, Java | 29, 4,361 | 100, 2,864 | 172, 2,639 |
| libdeflate, Java | 58, 2,930 | 140, 2,868 | 186, 2,753 |
| miniz_oxide, Bedrock | 35, 3,925 | 147, 3,100 | 411, 2,982 |
| zlib-rs, Bedrock | 27, 4,483 | 102, 3,126 | 173, 2,928 |
| libdeflate, Bedrock | 29, 3,245 | 70, 3,151 | 114, 3,027 |

CPU per viewer to send one column to every viewer, encoding and compressing for each viewer or once for all:

| Viewers | Java, each | Java, shared | Java, shared, unencrypted | Bedrock, each | Bedrock, shared |
|---|---|---|---|---|---|
| 1 | 266 µs | 264 µs | 199 µs | 205 µs | 205 µs |
| 2 | 264 µs | 164 µs | 101 µs | 203 µs | 104 µs |
| 10 | 264 µs | 84 µs | 20 µs | 205 µs | 23 µs |
| 50 | 268 µs | 68 µs | 4.1 µs | 204 µs | 6.9 µs |
| 200 | 266 µs | 64 µs | 1.0 µs | 204 µs | 3.8 µs |

Conclusions:

- **Sharing pays from the second viewer.** Two viewers already halve the cost; with 200 viewers, a Bedrock
  viewer costs 54 times less, and an unencrypted Java viewer (offline mode, or behind a proxy) 266 times less.
- **Compression, not encoding, is the cost to share.** Encoding from palette indices is 24 µs; compression is 7
  times that. The shared encoding cache must hold compressed bytes, not just encoded ones.
- **Online-mode Java viewers are bounded by CFB8.** With sharing, 64 of the 65 µs left per viewer are
  encryption. It runs on the connection's network thread, not on the logic thread.

## Open questions

1. Decoders return owned values (`String`, `Vec<u8>`). Borrowed decoding (`&str`, `&[u8]`) avoids copies for
   Play packets *(measure in M2)*.

---

## References

1. Mojang/bedrock-protocol-docs, `json/` directory (packet, type and enum schemas). https://github.com/Mojang/bedrock-protocol-docs/tree/main/json
2. Mojang/bedrock-protocol-docs README ("The protocol can change between releases"). https://github.com/Mojang/bedrock-protocol-docs
3. PrismarineJS/minecraft-data README, "License" section. https://github.com/PrismarineJS/minecraft-data#license
4. PrismarineJS/minecraft-data, `data/bedrock/latest/proto.yml`. https://github.com/PrismarineJS/minecraft-data/blob/master/data/bedrock/latest/proto.yml
5. Sandertv/gophertunnel (MIT). https://github.com/Sandertv/gophertunnel
6. CloudburstMC/Protocol (Apache-2.0). https://github.com/CloudburstMC/Protocol
7. minecraft.wiki, "Java Edition protocol/Packets" (states, framing, compression). https://minecraft.wiki/w/Java_Edition_protocol/Packets
8. minecraft.wiki, "Java Edition protocol/Encryption" (AES/CFB8, key and IV, continuous stream). https://minecraft.wiki/w/Java_Edition_protocol/Encryption
9. PrismarineJS/minecraft-data, `data/pc/26.1/version.json` (Java 26.1, protocol 775) and `data/dataPaths.json`. https://github.com/PrismarineJS/minecraft-data/blob/master/data/pc/26.1/version.json
10. PrismarineJS/minecraft-data, `data/bedrock/1.26.60/version.json` (Bedrock 26.60, which minecraft-data labels `1.26.60`; protocol 2225). https://github.com/PrismarineJS/minecraft-data/blob/master/data/bedrock/1.26.60/version.json
11. minecraft.wiki, "Java Edition protocol/Packets" (documents protocol 777 for Java 26.3). https://minecraft.wiki/w/Java_Edition_protocol/Packets
12. Mojang/bedrock-protocol-docs, `LICENSE`. https://github.com/Mojang/bedrock-protocol-docs/blob/main/LICENSE
13. Microsoft Learn, Minecraft Creator documentation, "More info sources". https://learn.microsoft.com/en-us/minecraft/creator/documents/moreinfosources
