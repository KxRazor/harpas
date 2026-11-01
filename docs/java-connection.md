# Java connections

Scope: how a Java Edition client connects to Harpas, from the first byte to play: connection states,
authentication, encryption and compression, the configuration state, chat signing, and protection against
abusive connections. Packet codecs are covered in [protocol-codecs.md](protocol-codecs.md); the Bedrock
counterpart is [raknet.md](raknet.md).

Status: design. Nothing here is implemented. Facts about the protocol cite their source in
[References](#references); everything else is a design decision of this project.

---

## Goals

| Goal | Meaning |
|---|---|
| **Vanilla behavior** | Authentication, encryption, compression and settings behave as on a vanilla server |
| **Everyone sees everyone** | Java and Bedrock players always see each other's chat |
| **Robust by default** | Abusive or malformed connections cannot exhaust the server, with no configuration |

---

## Connection states

Every connection starts in Handshaking and moves through the states vanilla defines [1]:

| State | What happens |
|---|---|
| **Handshaking** | The client states its protocol version and whether it wants status or login. Harpas picks the codec set for that protocol version ([versions.md](versions.md)) or refuses with a clear message |
| **Status** | Server list ping: MOTD, player counts, version, icon |
| **Login** | Authentication, encryption and compression (below) |
| **Configuration** | Registry data and the other data the client needs before playing (below) |
| **Play** | The game |

---

## Authentication

With `online-mode=true` in `server.properties`, as in vanilla [2]:

1. The server sends an Encryption Request with its public key (a 1024-bit RSA key pair generated at startup)
   and a verify token.
2. The client registers the join with Mojang's session server and answers with the shared secret and the verify
   token, encrypted with the server's public key.
3. The server checks the verify token and asks Mojang's session server whether the player joined:
   `https://sessionserver.mojang.com/session/minecraft/hasJoined`, with the username and the login hash. The
   answer carries the player's UUID, name and signed properties, including the skin.

- Requests to the session server run off the logic thread, so a slow session server never stalls the tick.
- In proxy mode, authentication is done by the proxy and the identity arrives through forwarding
  ([networks.md](networks.md#behind-a-java-proxy)).
- Identity rules for both editions, including Bedrock players, are in [identity.md](identity.md).

---

## Encryption and compression

- **Encryption:** AES/CFB8 with the shared secret as key and IV, as a continuous stream for the whole
  connection [2] ([protocol-codecs.md](protocol-codecs.md#java-wire-format)).
- **Compression:** zlib per packet above `network-compression-threshold` from `server.properties`, vanilla's
  default being 256 bytes [3].
- Both follow vanilla's settings; Harpas adds no settings of its own here.

---

## Configuration state

Before play, the server sends the client what it needs for the server version: registry data, feature flags,
tags and resource packs. For a client of another protocol version, these go through version translation
([versions.md](versions.md#how-translation-works)). The configuration state can be re-entered later, as
vanilla does, for example to change resource packs.

---

## Chat signing

Since 1.19, Java clients sign each chat message with a key Mojang issues for the account, and other clients
verify the signature; this is what makes messages reportable to Mojang. With `enforce-secure-profile=true`
(vanilla's default), a server disconnects clients that have no signing key [4]. Bedrock players have no Mojang
key, so their messages cannot be signed.

Harpas uses a **mixed** model:

| Message | Sent to Java clients as |
|---|---|
| From a Java player | Signed player chat, exactly as in vanilla: verifiable and reportable |
| From a Bedrock player | A system message formatted as `<name> message` |

- Java players keep everything vanilla gives them, including reporting.
- Bedrock players' messages are always visible, also to Java players who enable "Only Show Secure Chat", which
  hides unsigned player chat.
- `enforce-secure-profile` keeps its vanilla meaning for Java clients.
- Plugins that change chat see both kinds of messages through the same chat event.

---

## Keepalive and timeouts

Keepalive and the connection timeouts follow vanilla's behavior, so clients and proxies see the same
disconnections they see on vanilla.

---

## Protection against abuse

Active by default, with no configuration, like the Bedrock side ([raknet.md](raknet.md#robustness)):

| Threat | Defense |
|---|---|
| Connection floods | Per-address limits on new connections and on login attempts |
| Oversized packets | A maximum packet size per state; larger packets close the connection |
| Decompression bombs | A limit on the uncompressed size a compressed packet may declare, checked before inflating |
| Slow or idle connections | Timeouts in Handshaking, Status, Login and Configuration |
| Malformed packets | Decoders validate lengths before allocating and are fuzzed ([protocol-codecs.md](protocol-codecs.md#testing)); a decoding error closes the connection, never panics |

Limits are chosen so that no vanilla client behavior can hit them *(measure against real clients)*.

---

## Open questions

1. Default values of the per-address limits.

---

## Validation

1. **Authentication:** online-mode logins succeed with real accounts and fail with forged ones; a slow session
   server does not affect the tick.
2. **Chat:** with Java and Bedrock players together, every player sees every message, including Java players
   with "Only Show Secure Chat"; Java messages remain reportable.
3. **Abuse:** connection floods, oversized packets and decompression bombs are rejected without affecting
   players already online.

---

## References

1. minecraft.wiki, "Java Edition protocol/Packets" (connection states). https://minecraft.wiki/w/Java_Edition_protocol/Packets
2. minecraft.wiki, "Java Edition protocol/Encryption" (encryption request, session server join and hasJoined,
   AES/CFB8). https://minecraft.wiki/w/Java_Edition_protocol/Encryption
3. minecraft.wiki, "server.properties" (`network-compression-threshold`, `online-mode`). https://minecraft.wiki/w/Server.properties
4. e-im/FreedomChat README (behavior of `enforce-secure-profile` with clients that have no signing key). https://github.com/e-im/FreedomChat
