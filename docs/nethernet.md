# NetherNet transport

Scope: `harpas-nethernet`, the transport Bedrock clients use to connect: HTTP signaling, then WebRTC data
channels. It turns those channels into streams of game batches for [bedrock-interop.md](bedrock-interop.md), and it
is the first code that handles Bedrock traffic from the internet.

Status: prototype. Signaling, the server identity, segments and the WebRTC side are implemented in
`harpas-nethernet` and a Bedrock 26.60 client reached its Login packet ([Prototype](#prototype)); LAN discovery,
HTTPS and the robustness limits beyond the ones listed there are design. Every number or behavior marked
*(measure)* or *(verify)* must be confirmed before the design is frozen. Protocol facts cite their source in [References](#references); everything else is a design decision
of this project.

---

## Why NetherNet

Since Bedrock 26.60 (27 October 2026), clients join servers only over NetherNet; 26.40 started the move and
26.50 made NetherNet the default of Mojang's dedicated server [1][2]. GeyserMC made NetherNet its only transport
for joins [3], and gophertunnel accepts the NetherNet server address that 26.60 clients send [4]. Every supported
Bedrock client ([versions.md](versions.md#bedrock-window)) joins over NetherNet *(verify for 26.40)*, so it is the
only transport Harpas implements for Bedrock.

---

## Goals

In priority order. When two goals conflict, the higher one wins.

| # | Goal | Meaning |
|---|---|---|
| 1 | **Compatibility** | Every Bedrock client (desktop, mobile, console) connects and stays connected on real networks |
| 2 | **Robustness** | Malformed or hostile traffic cannot crash the server, exhaust its memory or amplify attacks |
| 3 | **Performance** | Low CPU per connection and low latency, with thousands of connections on one server |

### Non-goals

- Transports no supported client uses.
- Signaling through Xbox Live (the WebSocket service friends' worlds and Realms use) *(verify whether servers
  ever need it)*.
- WebRTC media (audio and video): NetherNet only uses data channels.

---

## Protocol scope

A join has two steps: signaling over HTTP, then a WebRTC connection over UDP [5][6].

### Signaling

A client joining `host:port` probes these base URLs and keeps the first that answers with a 2xx [5]:

| Port | URLs, in order |
|---|---|
| 19132 or none | `https://host:19132` and `https://host` together, then `http://host:19132`, then `http://host` |
| Any other | `https://host:port`, then `http://host:port` |

If nothing answers within about six seconds, the join fails. A port that silently drops packets delays the
plain-HTTP attempts until then, so every probed port must answer or refuse quickly.

| Request | Answer |
|---|---|
| `GET /v1/join` | The server list entry, as JSON: `name`, `protocol`, `version`, `level`, `players`, `maxPlayers`, `gameType` [7] |
| `POST /v1/join/{networkID}` with an SDP offer | The SDP answer (`application/sdp`); `networkID` is the client's 64-bit ID in decimal |

- The offer carries all ICE candidates: no trickle ICE, so one request completes the negotiation [6].
- Over plain HTTP, the client asks the player once whether to trust the server, and remembers the answer.
  HTTPS with a certificate the client trusts avoids the prompt [2][5].

### Identity

The SDP answer carries an `a=identity` attribute [8]:

- a JWT signed with ES384 by the server's own P-384 key, whose `cpk` claim is that public key;
- a detached ES384 signature over the answer's DTLS fingerprints, which binds the key to the WebRTC connection.

Clients keep the server's key on first use, so the key must persist across restarts *(verify how the client
reacts to a changed key)*. Client offers carry the same kind of attribute, with a token from Minecraft's
authorization service; Harpas authenticates players with the login packet ([identity.md](identity.md)) and does
not need it.

### WebRTC connection

- **ICE, DTLS and SCTP** over UDP: the server answers with its host candidates and acts as the DTLS client
  when the offer leaves the role open [6].
- **Two data channels**, opened by the client [9]:

| Label | Delivery | Use |
|---|---|---|
| `ReliableDataChannel` | Reliable, ordered | Game batches |
| `UnreliableDataChannel` | Unreliable (`maxRetransmits` 0) | Not used by the game so far *(verify)* |

- **Segments.** Each data channel message starts with one byte, the number of segments still to come, then
  the data. A batch is complete when that byte is 0. Senders split batches over about 10,000 bytes; a batch can
  have at most 255 more segments [9].
- **Game batches** are the Bedrock batches of [protocol-codecs.md](protocol-codecs.md) with two differences
  from the old transport: there is no `0xfe` header, and Minecraft's own packet encryption is not used, because
  DTLS already encrypts [10].

### LAN discovery

Clients find servers on the local network with encrypted broadcasts on UDP port 7551 [11]. This is a later
step; joining by address does not need it.

---

## Robustness

| Threat | Defense |
|---|---|
| Floods of signaling requests | Per-address rate limits; small limits on request size (an SDP offer is a few kilobytes) and on time to send it |
| Half-open WebRTC connections | A cap on pending negotiations and a timeout for ICE, DTLS and SCTP to complete |
| Memory exhaustion through segments | A limit on the bytes of one batch, checked as segments arrive, and on segments per batch |
| Malformed input | SDP, segment framing and batches are fuzzed; a decoding error closes the connection, never panics |
| Amplification | ICE connectivity checks answer only candidates from an accepted offer |

---

## Performance

- **One UDP socket, many connections.** WebRTC connections are demultiplexed by address in user space; the
  WebRTC stack does no I/O itself, so receive and send stay batched (`recvmmsg`/`sendmmsg` on Linux)
  *(measure in M6)*.
- **Integration with shared batches.** A batch compressed once for many players
  ([bedrock-interop.md](bedrock-interop.md#5-encode-and-compress-once-per-audience)) is only encrypted per
  connection, now by DTLS *(measure in M6)*.
- **Congestion control** is SCTP's own; its defaults are tuned against real clients on lossy and high-latency
  links *(measure in M6)*.

---

## Testing

- **Fuzzing** of the signaling requests, SDP handling and segment framing.
- **Interoperability**: real Bedrock clients on desktop, mobile and console; go-nethernet as a second
  implementation in automated tests.
- **Network conditions**: simulated loss, reordering, latency and jitter.
- **Load**: thousands of simulated connections, measuring CPU, latency and memory.

---

## Prototype

M1 prototype, in `crates/harpas-nethernet`:

- **Signaling:** an HTTP/1.1 subset parsed without I/O, `GET /v1/join` with the status JSON and
  `POST /v1/join/{networkID}` with the offer; heads up to 8 KiB and bodies up to 64 KiB.
- **Identity:** a P-384 key kept across restarts, a self-signed ES384 token with the key as a JWK in `cpk`, and
  the signed fingerprints, added to str0m's answer as `a=identity`.
- **WebRTC:** str0m 0.24 with its pure-Rust crypto, ICE-lite with one host candidate on one UDP socket, no I/O
  in the server: it takes datagrams and a clock and returns datagrams and events.
- **Segments:** batches split at 10,000 bytes; incoming batches limited to 4 MiB.
- **Tests:** a str0m client with the two Bedrock data channels joins in memory and exchanges segmented batches
  both ways.

Result on 1 November 2026, with a Bedrock 26.60 client on Android 16 over Wi-Fi, against
`examples/bedrock_login.rs`: the client read the status, sent a 2,536-byte offer, accepted the answer with its
identity after the trust prompt, opened the reliable channel, and sent Request Network Settings (protocol 2225)
and a Login of 588,282 bytes, about 59 segments. It then read the compressed Play Status the server sent and
closed the channel.

## Open questions

1. WebRTC stack: str0m worked for the prototype [12]; whether to keep it depends on its CPU and memory per
   connection *(measure in M6, with the load test under [Testing](#testing))*. The M1 prototype answered
   feasibility, not cost.
2. HTTPS for signaling: how operators get a certificate the client trusts, and whether plain HTTP with the
   trust prompt is an acceptable default.
3. Whether servers need Xbox Live signaling or LAN discovery for anything players expect.

---

## References

1. ConnectHosting, "NetherNet Error in Minecraft Bedrock" (26.40, 26.50 and 26.60 timeline). https://connecthosting.net/blog/minecraft-nethernet-error-bedrock
2. Winternode, "How to Set Up Geyser for Java and Bedrock Cross-Play" (26.60 release date). https://winternode.com/help/games/minecraft-java/plugins/geyser
3. GeyserMC/Geyser commits of 26 and 31 October 2026 ("change bedrock.transport config default to nethernet", #6843). https://github.com/GeyserMC/Geyser/commits
4. Sandertv/gophertunnel commits #541 ("Support for 1.26.60") and #544 ("Accept the NetherNet ServerAddress sent by 1.26.60 clients"). https://github.com/Sandertv/gophertunnel/commits
5. df-mc/go-nethernet `endpoint/doc.go` (probe order, six-second limit, trust prompt). https://github.com/df-mc/go-nethernet/blob/master/endpoint/doc.go
6. df-mc/go-nethernet `endpoint/handler.go` (`/v1/join` routes, no trickle ICE, answer content type) and `listener.go` (DTLS role of the answer). https://github.com/df-mc/go-nethernet/blob/master/endpoint/handler.go
7. df-mc/go-nethernet `endpoint/status.go` (status JSON fields). https://github.com/df-mc/go-nethernet/blob/master/endpoint/status.go
8. df-mc/go-nethernet `identity.go` (ES384 token with `cpk`, fingerprint assertion). https://github.com/df-mc/go-nethernet/blob/master/identity.go
9. df-mc/go-nethernet `message.go` and `conn.go` (data channel labels and parameters, segment byte, 10,000-byte split, 255 segments). https://github.com/df-mc/go-nethernet/blob/master/message.go
10. Sandertv/gophertunnel `minecraft/protocol/packet/transport.go` (`BatchHeaderer`, `EncryptionDisabler`). https://github.com/Sandertv/gophertunnel/blob/master/minecraft/protocol/packet/transport.go
11. df-mc/go-nethernet `discovery/listener.go` (LAN discovery on UDP port 7551). https://github.com/df-mc/go-nethernet/blob/master/discovery/listener.go
12. algesten/str0m (Sans-IO WebRTC). https://github.com/algesten/str0m
