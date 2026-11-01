# RakNet transport

Scope: `harpas-raknet`, the UDP transport that Bedrock clients use to connect. It turns datagrams into
reliable, ordered streams of game batches for [bedrock-interop.md](bedrock-interop.md), and it is the first code that
handles traffic from the internet.

Status: design. Nothing here is implemented. Every number or behavior marked *(measure)* or *(verify)* must
be confirmed before the design is frozen. Protocol facts cite their source in [References](#references);
everything else is a design decision of this project.

---

## Goals

In priority order. When two goals conflict, the higher one wins.

| # | Goal | Meaning |
|---|---|---|
| 1 | **Compatibility** | Every Bedrock client (desktop, mobile, console) connects and stays connected on real networks |
| 2 | **Robustness** | Malformed or hostile traffic cannot crash the server, exhaust its memory or amplify attacks |
| 3 | **Performance** | Low CPU per datagram and low latency, with thousands of connections on one socket |

### Why our own implementation

- RakNet is small compared with the Bedrock game protocol. The reference used here, go-raknet, is about
  2,300 lines of Go without tests or comments [1].
- It is the most exposed and most performance-sensitive layer of Bedrock support. Owning it lets us batch
  system calls, reuse buffers, plug directly into the shared batches of
  [bedrock-interop.md](bedrock-interop.md#5-encode-and-compress-once-per-audience), and fuzz every parser.
- No existing Rust implementation was found mature enough to depend on for this layer.

### Non-goals

- RakNet features Bedrock does not use (e.g. RakNet's own plugin system or its NAT punch-through).
- Other Bedrock transports. A second, WebRTC-based transport called NetherNet exists, with a community
  specification [6] *(verify scope and whether servers need it)*.

---

## Protocol scope

Bedrock uses RakNet protocol version **11** [2]. The parts Harpas implements:

### Offline (unconnected) phase

Messages are identified by an ID byte and, in unconnected messages, a fixed 16-byte "magic" sequence [3]:

| ID | Message | Purpose |
|---|---|---|
| `0x01`, `0x02` | Unconnected ping | Server list query |
| `0x1c` | Unconnected pong | Server list answer; for Bedrock it carries a `MCPE;…` status string (name, protocol, version, player counts, server ID) [4] |
| `0x05` / `0x06` | Open connection request / reply 1 | Protocol version check and MTU discovery |
| `0x07` / `0x08` | Open connection request / reply 2 | MTU agreement, cookie check, connection creation |
| `0x19` | Incompatible protocol version | Rejection |

### Connected phase

| ID | Message | Purpose |
|---|---|---|
| `0x09` / `0x10` | Connection request / accepted | Session establishment |
| `0x13` | New incoming connection | Client confirms the session |
| `0x00` / `0x03` | Connected ping / pong | Liveness and RTT measurement |
| `0x15` | Disconnect notification | Clean close |
| `0xfe` | Game batch | Bedrock payload, handed to `harpas-protocol-bedrock` |

### Datagrams

- A datagram header flags it as data (`0x80`), ACK (`0x40`) or NACK (`0x20`) [2].
- Data datagrams carry a sequence number; the receiver acknowledges ranges with ACKs and requests missing
  sequence numbers with NACKs.
- Each datagram carries one or more encapsulated messages with a reliability mode [2]:

| Reliability | Arrives | Order | Duplicates |
|---|---|---|---|
| Unreliable | Maybe | Any | Possible |
| Unreliable sequenced | Maybe | Newest wins | Possible |
| Reliable | Always | Any | Never |
| Reliable ordered | Always | In order (per channel) | Never |
| Reliable sequenced | Maybe | Newest wins | Never |

Bedrock game batches travel as reliable ordered messages, so that path is optimized first.

### MTU and fragmentation

- The MTU is negotiated during the offline phase. go-raknet bounds it between 400 and 1,492 bytes and
  subtracts 28 bytes for IP and UDP headers [2].
- Messages larger than the effective MTU are split into fragments with a split ID, count and index, and
  reassembled by the receiver [2].

---

## Robustness

| Threat | Defense |
|---|---|
| IP spoofing and amplification during the handshake | Cookies: reply 1 carries a cookie derived from a secret salt and the client address; request 2 must echo it. The salt rotates and the previous salt is still accepted, as in go-raknet [5] |
| Memory exhaustion through fragments | Limits on fragments per message and on concurrent reassemblies per connection. go-raknet uses 512 and 16 [2]; Harpas starts from these values *(measure against real clients)* |
| Floods of half-open connections | Per-address rate limits and a cap on pending handshakes |
| Malformed packets | Every parser is fuzzed; a decoding error closes or ignores the offending input and never panics |
| Repeat offenders | Temporary per-address blocking after protocol errors. go-raknet blocks for 10 s by default [5] |

go-raknet lets operators disable cookies because some hosting providers' own DDoS protection interferes with
them [5]. Harpas keeps cookies on; whether that interference still exists and how to handle it without a
setting is an open question.

---

## Performance

- **One socket, many connections.** Connections are demultiplexed by address in user space.
- **Batched system calls.** Receive and send many datagrams per system call (`recvmmsg`/`sendmmsg` on Linux),
  and use UDP segmentation offload where the kernel supports it *(measure)*.
- **Several receive threads.** One socket per thread with `SO_REUSEPORT`, or a single socket drained by
  several workers; chosen by measurement *(measure)*.
- **Buffer reuse.** Datagram and reassembly buffers come from pools; the steady state allocates nothing per
  datagram.
- **ACK coalescing.** ACKs are sent in ranges on a short tick rather than per datagram. go-raknet ticks every
  100 ms [2]; Harpas tunes the interval for latency *(measure)*.
- **Integration with shared batches.** A batch that was compressed once for many players
  ([bedrock-interop.md](bedrock-interop.md#5-encode-and-compress-once-per-audience)) is encrypted per connection and then
  fragmented by this layer without being copied again.

### Congestion control

- go-raknet bounds the send window at 2,048 datagrams [2].
- Harpas starts with a window-based scheme driven by ACK/NACK feedback and RTT, then tunes it against real
  clients on lossy and high-latency links *(measure)*. This is the part most likely to need iteration: too
  aggressive saturates links, too conservative adds lag.

---

## Testing

- **Fuzzing** of every decoder: offline messages, datagram headers, encapsulation, fragments, ACK/NACK
  ranges.
- **Interoperability**: real Bedrock clients on desktop, mobile and console; go-raknet as a second client
  implementation in automated tests.
- **Network conditions**: simulated loss, reordering, duplication, latency and jitter, checking that
  reliable ordered delivery holds and that throughput recovers.
- **Load**: thousands of simulated connections on one server, measuring CPU per datagram, latency and memory.

---

## Open questions

1. Cookies and hosting providers' DDoS protection: is the interference still real, and can it be detected
   automatically?
2. Congestion control algorithm and its defaults.
3. Threading model for receive and send.
4. Scope of NetherNet and whether servers need it.

---

## References

1. Sandertv/go-raknet (MIT): source size measured by counting non-empty, non-comment lines of the non-test
   `.go` files. https://github.com/Sandertv/go-raknet
2. go-raknet `conn.go` and `packet.go` (protocol version, MTU bounds, header flags, reliabilities, split
   limits, window size, tick interval). https://github.com/Sandertv/go-raknet/blob/master/conn.go
3. go-raknet `internal/message/id.go` (message IDs and unconnected magic). https://github.com/Sandertv/go-raknet/blob/master/internal/message/id.go
4. gophertunnel `minecraft/listener.go` (Bedrock `MCPE;…` pong data). https://github.com/Sandertv/gophertunnel/blob/master/minecraft/listener.go
5. go-raknet `listener.go` and `handler.go` (cookies, salt rotation, `DisableCookies`, block duration). https://github.com/Sandertv/go-raknet/blob/master/listener.go
6. df-mc/nethernet-spec, a specification for the NetherNet protocol. https://github.com/df-mc/nethernet-spec
