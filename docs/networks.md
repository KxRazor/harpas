# Server networks

Scope: how Harpas servers work in multi-server networks: behind existing Java proxies (Velocity and
BungeeCord), with Bedrock players, and as a network of Harpas servers without a proxy.

Status: design. Nothing here is implemented. Facts about protocols and proxies cite their source in
[References](#references); everything else is a design decision of this project.

---

## Goals

| Goal | Meaning |
|---|---|
| **Work with existing proxies** | A Harpas server is a drop-in backend behind Velocity or BungeeCord |
| **Bedrock players too** | Bedrock players can move between servers of a network |
| **Networks without a proxy** | Harpas servers can form a network for both editions using the clients' own transfer support |
| **Secure by default** | A backend accepts players only from the proxy or network it trusts |

---

## Behind a Java proxy

In a proxied network the proxy authenticates the player and connects them to a backend. The backend must
learn the player's real identity (UUID, name, IP address, skin) from the proxy; this is called forwarding.
Harpas supports the three modes Velocity documents [1]:

| Mode | How it works | Security |
|---|---|---|
| **Velocity modern** | Binary player data signed with HMAC-SHA256 and a shared secret, sent on the `velocity:player_info` login plugin channel; forwarding format versions 1 to 4 [2]. Minecraft 1.13 or later [1] | Strongest: the proxy cannot be impersonated without the secret |
| **BungeeGuard** | Legacy BungeeCord forwarding plus a secret token in the player's profile properties (`bungeeguard-token`) [2] | Better than legacy, weaker than modern |
| **BungeeCord legacy** | Player data in the handshake, unsigned | Described by Velocity as "fundamentally insecure" [1]: anyone who can reach the backend can impersonate any player |

Configuration:

```toml
[proxy]
mode = "velocity"        # "velocity", "bungeecord", or "none" (default)
secret = "..."           # required for "velocity"; for "bungeecord" it enables BungeeGuard
```

- **BungeeCord with a secret is BungeeGuard; without one it is legacy.** Legacy mode prints a prominent
  warning on every start, explaining that the backend must not be reachable except from the proxy.
- In proxy mode, **direct connections without valid forwarding are refused.**
- Online-mode authentication is done by the proxy; the backend trusts the forwarded identity only after
  verifying it.
- **Plugin messages pass through.** Plugins that talk to the proxy over the `BungeeCord` channel (for example,
  to send a player to another server) work as on Paper.
- **Modded mode:** the companion mod implements the same forwarding modes, so a Harpas modded server works
  behind Velocity or BungeeCord without extra mods ([mod-support.md](mod-support.md)).

---

## Bedrock players in a network

Velocity and BungeeCord speak only the Java protocol, so Bedrock players cannot go through them.

- Bedrock players connect **directly** to a Harpas server.
- To move a Bedrock player to another server, Harpas sends Bedrock's **TransferPacket**, which carries the
  target's address and port [3]. The client reconnects to the other Harpas server, which verifies the player's
  Xbox identity again.
- **Plugin compatibility:** when a plugin asks to move a Bedrock player over the `BungeeCord` channel (a
  `Connect` to a named server), Harpas translates it into a TransferPacket to that server's Bedrock address,
  taken from the network table (below).
- **Limitation:** a Java proxy does not see Bedrock players, so its player counts and proxy commands do not
  include them.

---

## Harpas network without a proxy

Since 1.20.5, Java clients support **transfers** (the server tells the client to connect to another address)
and **cookies** (small payloads the client stores and presents to the next server). Vanilla's
`accepts-transfers=true` accepts transfers from anywhere [4]; Harpas only accepts transfers it can verify.

### Network table

Each server lists the network's members in `harpas.toml`:

```toml
[network]
name = "my-network"
self = "lobby"

[[network.server]]
name = "lobby"
java = "lobby.example.net:25565"
bedrock = "lobby.example.net:19132"
public_key = "..."

[[network.server]]
name = "survival"
java = "survival.example.net:25565"
bedrock = "survival.example.net:19132"
public_key = "..."
```

### Java transfers with signed cookies

- **Each server has its own Ed25519 key pair.** Its private key stays on that server; its public key is listed
  in the network table of every member.
- Before a transfer, the sending server stores a **signed transfer cookie** on the client, containing: format
  version, network name, issuing server, target server, player UUID and name, timestamp, and a random nonce.
- The receiving server accepts a transferred connection only if the cookie's signature verifies against a
  member's public key, the target is itself, the timestamp is within a short window, and the nonce has not been
  used before. Otherwise the connection is refused.
- Signatures are asymmetric, so a server can only issue transfers in its own name: compromising one server's
  configuration does not let an attacker forge transfers from the others.

### Bedrock transfers

Bedrock has no cookies. Bedrock transfers use the TransferPacket, and the receiving server relies on the Xbox
identity verification every Bedrock login already goes through.

### Plugin API

`transfer(player, server)` works for both editions, in the native API ([plugin-api.md](plugin-api.md)) and in
the Bukkit shim, and is what the `BungeeCord`-channel translation uses for Bedrock players.

### Not included

Sharing inventories or other player data between servers. As in vanilla, each server keeps its own player
data; networks that need sharing do it with plugins.

---

## Harpas as a proxy for both editions (later)

A proxy that speaks Java and Bedrock would show players of both editions in one place, but it is a separate
product of roughly the size of the whole Bedrock interoperability:

| Part | Size |
|---|---|
| Java and Bedrock login and authentication at the proxy | Medium |
| Forwarding to backends | Medium |
| Switching servers without disconnecting (clearing entities, scoreboards, boss bars, tab list, dimension), separately for each edition | Large |
| Bedrock at the proxy (translating there, or passing Bedrock through to backends) | Large |
| Compression and encryption on both sides of the proxy | Medium |
| Proxy plugin ecosystem (compatibility with existing proxy plugin APIs, or a new one) | Very large |
| Several protocol versions at once | Large |

It is out of scope until the server is mature and there is demand for it.

---

## Open questions

1. Whether to build Harpas as a proxy for both editions, and when.
2. Network membership: static table in each server's `harpas.toml` (current design) or a shared source.
3. Length of the transfer cookie's time window.

---

## Validation

1. **Forwarding:** Harpas behind Velocity (modern) and BungeeCord (with and without BungeeGuard); direct
   connections without valid forwarding are refused.
2. **Plugin messages:** a server-switching plugin moves Java players through the proxy and Bedrock players
   through TransferPacket.
3. **Proxyless network:** Java and Bedrock players move between Harpas servers; forged, expired, replayed or
   misdirected cookies are rejected.

---

## References

1. PaperMC, Velocity documentation, "Player information forwarding". https://docs.papermc.io/velocity/player-information-forwarding/
2. PaperMC/Velocity, `PlayerDataForwarding.java` (channel `velocity:player_info`, HmacSHA256, forwarding versions 1 to 4, `bungeeguard-token`). https://github.com/PaperMC/Velocity/blob/dev/3.0.0/proxy/src/main/java/com/velocitypowered/proxy/connection/PlayerDataForwarding.java
3. Mojang/bedrock-protocol-docs, `TransferPacketPayload.json` (server address, server port, reload world). https://github.com/Mojang/bedrock-protocol-docs/blob/main/json/TransferPacketPayload.json
4. GameServerKings, "server.properties reference" (`accepts-transfers`, transfers since 1.20.5, cookies). https://www.gameserverkings.com/knowledge-base/minecraft/server-properties-reference.md
