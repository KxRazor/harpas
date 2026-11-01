# Protocol codecs

Scope: how Harpas produces the code that encodes and decodes Java Edition packets (`harpas-protocol-java`) and
Bedrock packets (`harpas-protocol-bedrock`): the protocol schema they are generated from, the generator, the
hand-written exceptions, testing, and the constraints on the sources. Bedrock transport is covered in
[raknet.md](raknet.md); translation and gameplay in [bedrock-interop.md](bedrock-interop.md).

Status: design. Nothing here is implemented. Facts about sources cite them in [References](#references);
everything else is a design decision of this project.

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

---

## Codegen

- A generator turns each schema into Rust codecs at build time, one set per supported protocol version.
  Differences between versions are visible as differences in the schema.
- Generated encoders write into reusable buffers and keep no per-player state, so one encoded packet can be
  shared by every player who receives it.
- Generated decoders validate lengths and bounds before allocating, since they parse untrusted network input.

### Hand-written exceptions

Packets and types whose structure the schema cannot express cleanly are written by hand.

**Bedrock:**

- chunk and sub-chunk payloads;
- network NBT;
- item stack serialization;
- entity metadata;
- packets with flag-dependent fields, such as `PlayerAuthInputPacket` and `ItemStackRequest`.

**Java:**

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
- **Encryption has a real CPU cost.** CFB8 runs one AES block operation per byte rather than per 16 bytes;
  hardware AES helps, but the cost grows with traffic *(measure)*.
- **The zlib implementation is our choice.** The protocol requires zlib-format data, not a specific library, so
  a faster implementation can be used *(measure)*.

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

## Open questions

1. Schema format: adopt ProtoDef as-is, or define our own and convert from it?

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
