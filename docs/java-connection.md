# Java connections

Scope: how a Java Edition client connects to Harpas, from the first byte to play: connection states,
authentication, encryption and compression, the configuration state, chat signing, and protection against
abusive connections. Packet codecs are covered in [protocol-codecs.md](protocol-codecs.md); the Bedrock
counterpart is [nethernet.md](nethernet.md).

Status: in progress. Every state is implemented in `harpas-java`: Handshaking, Status and the server list
(legacy ping included), Login in online and offline mode with encryption and compression, Configuration with
registry data, and Play with players in a void world. Java player chat and signature verification are
implemented, as is the protection against abuse; real-client validation of signed chat remains open.
Facts about the protocol cite their source in [References](#references); everything else is a design decision
of this project.

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

### Implementation

- **No I/O in the state machine.** `harpas_java::Connection` takes the bytes read from the socket and produces
  the bytes to send, like the NetherNet transport ([nethernet.md](nethernet.md)). The `harpas` binary drives
  it with one thread per connection, which the connection limits below keep bounded; a network runtime that
  replaces those threads is chosen when the number of connections calls for it (open question 2). Connections queue player events for the logic thread's packet phase
  and send its updates to clients; the resulting online count is used in server-list responses
  ([engine-core.md](engine-core.md#1-vanilla-tick-order-on-one-logic-thread)).
- **Framing:** a frame's VarInt length is at most 3 bytes and 2,097,151, as in vanilla; the length is checked
  before waiting for the frame, so a connection never buffers more than one frame.
- **Handshake:** a Login or Transfer handshake with another protocol version gets vanilla's Login Disconnect,
  `multiplayer.disconnect.outdated_client` below protocol 754 (Java 1.16.5) and
  `multiplayer.disconnect.incompatible` otherwise, both with the server's version. A Transfer is refused with
  `multiplayer.disconnect.transfers_disabled` unless `accepts-transfers=true`. Status answers any version, and
  the response's protocol number tells the client whether it can join.
- **Status:** the response carries the version, `max-players`, the online count, the `motd`, the
  `server-icon.png` (only a 64×64 PNG, as in vanilla) and `enforcesSecureChat`. It is answered once; the
  connection closes after the Pong, as in vanilla.
- **Legacy ping:** a connection whose first byte is `0xFE` gets the pre-1.7 answer vanilla gives: the beta
  format for `0xFE` alone, the 1.4 format with protocol 127 otherwise.
- **Timeout:** a connection that sends nothing for 30 seconds is closed, as in vanilla.
- **Login:** names with spaces, control characters or non-ASCII characters are refused, as in vanilla. In
  online mode, see [Authentication](#authentication) and [Encryption and compression](#encryption-and-compression).
  In offline mode the player's UUID is vanilla's offline UUID, a version 3 UUID of `OfflinePlayer:<name>`, and
  the connection is not encrypted, as in vanilla. Once authenticated, a player is refused with
  `multiplayer.disconnect.server_full` when `max-players` players are in Play, before Login Success and
  compression, as in vanilla; players still in Configuration are not counted, as in vanilla. The whitelist,
  bans and operators who bypass the limit come with commands (M4).
- **Configuration:** see [Configuration state](#configuration-state).
- **Play:** players join the void world at (0.5, 65, 0.5), with abilities for the `gamemode` of
  `server.properties`. The server maintains a shared player list and sends profiles, entities and accepted
  movement to other players in range. Empty overworld chunks follow the player's position and effective view
  distance. Java 26.1's player entity type is 155 in the project's pinned minecraft-data version [5]. Play
  packets the schema does not have yet are ignored instead of closing the connection. The implemented
  movement checks and their limits are described in [engine-core.md](engine-core.md#1-vanilla-tick-order-on-one-logic-thread).
- **Keep Alive:** every 15 seconds in Configuration and Play; a client that does not answer within 15 seconds,
  or answers with another ID, is disconnected, as in vanilla.
- **Tested with a client:** on 2026-11-01, a vanilla Java 26.1 client listed the server with its MOTD colors,
  icon, player counts and latency; it then joined the void world in creative mode, flew around, and saw itself
  in the player list, with no error in the client's log. On 2026-11-01, in online mode and through a playit.gg
  tunnel, another player's premium Java 26.1 client on Windows joined: the session server verified the
  player, the connection was encrypted and compressed, and the player list and the third-person view showed
  the player's skin. On 2026-11-02, two Java 26.1 clients in offline creative mode appeared together in the
  player list, saw each other's entities and synchronized movement, and the disconnect check was successful.
  Two offline clients also exchanged chat messages in both directions and saw their own messages.

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

Implementation:

- The key pair is generated when the server starts and shared by every connection; the verify token is 4 random
  bytes per login.
- The connection itself does no I/O: once it has the shared secret, it hands the caller an `AuthRequest` with
  the name and the login hash (SHA-1 of the empty server ID, the secret and the public key, printed as a signed
  hexadecimal number as Java's `BigInteger` does), and the caller answers with the session server's result. The
  `harpas` binary makes the request on the connection's thread with `ureq`, rustls and the system's root
  certificates, with a 10-second limit.
- An unknown join gets vanilla's `multiplayer.disconnect.unverified_username`; an unreachable session server
  gets `multiplayer.disconnect.authservers_down`.
- `prevent-proxy-connections`, which adds the player's address to the request, is not read yet.

**RSA decryption.** The `rsa` crate's PKCS#1 v1.5 decryption leaks timing (RUSTSEC-2023-0071, the Marvin
attack), and no fixed release exists. Harpas uses only its raw RSA operation, with random blinding, and removes
the padding itself in constant time. A bad padding is not an error: it gives pseudo-random bytes derived from
the ciphertext (implicit rejection, as TLS stacks do), so a bad padding and a wrong verify token fail in the
same way. The advisory is ignored in `deny.toml` with this reason, until `rsa` ships a fix.

---

## Encryption and compression

- **Encryption:** AES/CFB8 with the shared secret as key and IV, as a continuous stream for the whole
  connection [2] ([protocol-codecs.md](protocol-codecs.md#java-wire-format)).
- **Compression:** zlib per packet above `network-compression-threshold` from `server.properties`, vanilla's
  default being 256 bytes [3].
- Both follow vanilla's settings; Harpas adds no settings of its own here.

Implementation:

- **Order:** encryption starts as soon as the Encryption Response is accepted, in both directions; any bytes
  the client sent after it are decrypted in place. Set Compression is sent, uncompressed, just before Login
  Success, and compression applies to every frame after it in both directions.
- **Compression limits:** a compressed packet must declare at least the threshold and at most 8 MiB, as in
  vanilla, and must inflate to exactly the declared length; output never grows past it, so a decompression
  bomb is stopped at the declared size. A negative threshold turns compression off.
- **Libraries:** zlib-rs at level 6 through `flate2`, and the RustCrypto `aes` and `cfb8` crates, as measured
  in [protocol-codecs.md](protocol-codecs.md#shared-encoding-results).

---

## Configuration state

Before play, the server sends the client what it needs for the server version: registry data, feature flags,
tags and resource packs. For a client of another protocol version, these go through version translation
([versions.md](versions.md#how-translation-works)). The configuration state can be re-entered later, as
vanilla does, for example to change resource packs.

How it works now:

1. The server sends its brand, the `minecraft:vanilla` feature flag and the data pack it knows,
   `minecraft:core` at the server version, as vanilla does.
2. The client answers with the packs it has. A 26.1 client has the core pack built in, so the server sends the
   28 synchronized registries **by name only**, in vanilla's order, and the client takes their data from its
   own copy. The names come from minecraft-data ([versions.md](versions.md)); a client without the core pack is
   disconnected with a message, since sending the data itself needs vanilla's data files (M3).
3. Tags: the registry data and the client's item components refer to tags, and the client refuses the
   registries, or fails when it builds its items, if one is missing. Until Harpas has vanilla's data
   ([world.md](world.md#vanilla-game-data)), the server sends the tags the client needs **empty**: 122 of them,
   found from the errors a 26.1 client reports and completed with the vanilla tags its item components use.
   Empty tags change nothing in a void world; with real worlds, the real tags replace them.
4. Finish Configuration; the client acknowledges it and the connection moves to Play.

---

## Chat signing

Since 1.19, Java clients sign each chat message with a key Mojang issues for the account, and other clients
verify the signature; this is what makes messages reportable to Mojang. Bedrock players have no Mojang key,
so their messages cannot be signed. Vanilla can distribute signing keys lazily [6].

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

**Java implementation:**

- A connection checks the account's public-key certificate against `playerCertificateKeys` fetched from
  `https://api.minecraftservices.com/publickeys` over HTTPS at startup. If the keys cannot be fetched or
  parsed, the server trusts no signing keys; it logs the failure and a restart retries the fetch. Test
  authorities are generated independently and are never configured as production trust roots.
- The connection verifies the sender, session, message index, timestamp, salt, text and previously seen
  signatures [7]. It checks key expiration, rejects invalid signatures and replayed session IDs, and tracks
  each recipient's 20-message acknowledgement window and pending messages. A bad acknowledgement or
  checksum closes that connection. A bad signature breaks the signing chain until the player starts a new
  session; a missing signature or expired key refuses the message. The receiving client determines whether
  an old message is trustworthy.
- The logic thread distributes verified session information before signed messages, including to players
  who join later. It sends each original message to all players, including the sender, subject to their chat
  visibility setting. Signatures are sent in full rather than through the optional signature cache.
- With `online-mode=true` and `enforce-secure-profile=true`, a message without a verified session and
  signature is rejected. Players can join before sending a session. In offline mode, messages are relayed
  unsigned and `enforcesSecureChat` is false. With secure profiles disabled, an online player without a
  session can send unsigned messages; an established session cannot silently downgrade to unsigned chat.
- Commands and Bedrock chat are not implemented yet. Signed commands are discarded but their acknowledgement
  and signing indices are tracked so they do not break later player messages.

Automated tests use generated player and authority keys, including an encrypted online connection with a
simulated session server; they are not a substitute for two real premium clients. A real online exchange,
including verification/reportability and "Only Show Secure Chat", remains to be checked when available.

---

## Keepalive and timeouts

Keepalive and the connection timeouts follow vanilla's behavior, so clients and proxies see the same
disconnections they see on vanilla.

---

## Protection against abuse

Active by default, with no configuration, like the Bedrock side ([nethernet.md](nethernet.md#robustness)):

| Threat | Defense |
|---|---|
| Connection floods | Limits on connections that are not in Play yet, total and per address, and on new connections and login attempts per address |
| Oversized packets | A maximum packet size per state; larger packets close the connection |
| Decompression bombs | A limit on the uncompressed size a compressed packet may declare, checked before inflating |
| Slow or idle connections | Timeouts in Handshaking, Status, Login and Configuration |
| Malformed packets | Decoders validate lengths before allocating and are fuzzed ([protocol-codecs.md](protocol-codecs.md#testing)); a decoding error closes the connection, never panics |

Limits are chosen so that no vanilla client behavior can hit them *(measure against real clients)*:

| Limit | Value | Notes |
|---|---|---|
| Connections not in Play | 256 in total, 8 per address | A connection stops counting when it reaches Play, so players behind one address are limited only while they connect. An IPv6 address counts as its /64 prefix in every per-address limit |
| New connections | 20 per address per 10 seconds | A server list refresh and a join take two |
| Login attempts | 5 per address per 10 seconds | Refused with Bukkit's "Connection throttled! Please wait before reconnecting." |
| Packet size | Handshaking 1 KiB, Status 256 bytes, Login 32 KiB, Configuration 64 KiB, Play vanilla's 2 MiB frames and 8 MiB uncompressed | Checked on the declared length, before the bytes are buffered or inflated |
| Time to log in | 30 seconds from the connection to Configuration | Vanilla's login timeout; Login gets `multiplayer.disconnect.slow_login`. Configuration and Play rely on Keep Alive |

Loopback connections, such as a proxy on the same machine, skip the per-address limits. Connections over a
limit are closed as soon as they are accepted, before a thread is started for them.

---

## Open questions

1. Default values of the per-address limits: the values above are a first choice, to be measured against real
   clients and networks behind one address.
2. Network runtime: an async runtime or an event loop over the I/O-free connection, when thread-per-connection
   stops scaling.

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
5. PrismarineJS/minecraft-data, Java 26.1 entity types at the project's pinned revision.
   https://github.com/PrismarineJS/minecraft-data/blob/bd8b5c6048e5af9694374b83f25f5b4aade619cc/data/pc/26.1/entities.json
6. Minecraft Java Edition snapshot 22w43a, changes to chat-session key distribution.
   https://feedback.minecraft.net/hc/en-us/articles/10330243077261-Minecraft-Java-Edition-Snapshot-22w43a
7. Minecraft Packet ID Reference, Java 26.1 chat signing and acknowledgements.
   https://github.com/xKumorio/Minecraft-Packet-ID-Reference/blob/main/versions/26.1.md
