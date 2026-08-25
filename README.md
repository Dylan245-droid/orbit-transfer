# Orbit-Transfer

[![npm](https://img.shields.io/npm/v/orbit-transfer-wasm)](https://www.npmjs.com/package/orbit-transfer-wasm)
[![Docker](https://img.shields.io/docker/v/dylanondo/orbit-relay?label=docker)](https://hub.docker.com/r/dylanondo/orbit-relay)
[![GitHub](https://img.shields.io/github/v/release/Dylan245-droid/orbit-transfer)](https://github.com/Dylan245-droid/orbit-transfer)

Hybrid P2P + edge-relay file transfer with **rateless fountain codes** (LT + LDPC + HDPC).

Orbit-Transfer splits a file into K source blocks, adds S LDPC and H HDPC
precode checks, and emits an *unbounded* stream of encoded symbols. The receiver
needs only K + ε *any* symbols to rebuild the file — no retransmission requests,
no fixed overhead, no per-packet bookkeeping. The sender keeps emitting until
the receiver says `READY`, so any loss rate is absorbed automatically.

```
Sender ──rateless symbols──▶ Relay(s) ──rateless symbols──▶ Receiver
        ──direct P2P symbols─────────────────────────────▶      ▲
        ◀────── READY ◀──────────────────────────────────────────┘
```

## Workspace layout

| Crate | Role |
|-------|------|
| `orbit-fountain` | LT fountain encoder/decoder: Robust Soliton distribution, LDPC + HDPC precoding (Raptor/RaptorQ-style), systematic phase, peeling (inactivation) decoding, SIMD-vectorizable XOR, XXH3 integrity |
| `orbit-protocol` | Compact binary wire protocol (Hello / Meta / Symbol / Ready / Done / Direct) |
| `orbit-transport` | Relay client (send/recv halves) + rateless sender/receiver sessions + multi-path scheduler + P2P direct channel (TCP or QUIC) |
| `orbit-crypto` | AEAD per-symbol encryption: Argon2id key derivation + ChaCha20-Poly1305 |
| `orbit-relay` | Edge relay: room-based routing, pre-receiver buffering, async backpressure, byte/symbol counters |
| `orbit-cli` | `orbit send`, `orbit receive`, `orbit relay` |
| `orbit-wasm` | wasm-bindgen bindings + browser demo (`demo/index.html`) + TypeScript package (`ts/`) |

## Install & deploy

**Edge relay** (one binary, WebSocket, routes sealed symbols by session id):

```sh
docker run --rm -p 9000:9000 dylanondo/orbit-relay
```

**Browser codec** (fountain encode/decode in WASM + TypeScript):

```sh
npm install orbit-transfer-wasm
```

```ts
import { loadOrbit, OrbitEncoder, OrbitDecoder } from "orbit-transfer-wasm";
await loadOrbit();
const encoder = new OrbitEncoder(payload, 4096);      // file bytes -> symbols
const symbol = encoder.encodeSymbol(esi);             // rateless: any esi
// send symbol to the peer over any channel (WebRTC, WebSocket, …)
const decoder = new OrbitDecoder(payload.length, 4096, encoder.k, encoder.checksumHex);
decoder.addSymbol(esi, symbol);
if (decoder.isComplete) decoder.reconstruct();        // exact payload back
```

## Quick start

```bash
# Terminal 1: run one or more relays
cargo run -p orbit-cli -- relay --addr 0.0.0.0:9000
cargo run -p orbit-cli -- relay --addr 0.0.0.0:9001

# Terminal 2: send a file over two relays (prints the session id)
cargo run -p orbit-cli -- send ./bigfile.bin --relay ws://127.0.0.1:9000 --relay ws://127.0.0.1:9001

# Terminal 3: receive it
cargo run -p orbit-cli -- receive ./copy.bin --relay ws://127.0.0.1:9000 --relay ws://127.0.0.1:9001 --session <id>
```

With encryption (symbols are sealed end-to-end; the relay only routes):

```bash
cargo run -p orbit-cli -- send ./bigfile.bin --relay ws://127.0.0.1:9000 --secret "passphrase"
cargo run -p orbit-cli -- receive ./copy.bin --relay ws://127.0.0.1:9000 --session <id> --secret "passphrase"
```

The receiver advertises a P2P listen address via the relay by default; when
the sender can reach it, symbols are split 50/50 between the direct TCP link
and the relays, with automatic fallback if the direct link fails:

```bash
cargo run -p orbit-cli -- receive ./copy.bin --relay ws://127.0.0.1:9000 --session <id> --listen 0.0.0.0:9001
```

The direct P2P path can also run over **QUIC** instead of TCP (0-RTT setup,
built-in congestion control, connection migration):

```bash
cargo run -p orbit-cli -- receive ./copy.bin --relay ws://127.0.0.1:9000 --session <id> --quic
```

The receiver advertises a `quic://` address; the sender picks QUIC
automatically. Symbol payloads remain sealed by the optional AEAD layer, and
the self-signed QUIC certificate is trusted implicitly because the session is
already authenticated.

## Multi-relay aggregation

`--relay` is repeatable: both sides connect to *every* relay, and the
multi-path scheduler round-robins symbols across all live links. This
aggregates the available uplinks — useful when a single relay (or direct
P2P path) is the bottleneck:

```bash
# 3 relays → 3x the relay bandwidth
cargo run -p orbit-cli -- send ./bigfile.bin \
  --relay ws://relay-a:9000 --relay ws://relay-b:9000 --relay ws://relay-c:9000
```

- A relay that dies mid-transfer is disabled and its load is transparently
  redistributed over the remaining links; no symbols are lost (the receiver
  never asks twice — it just needs K + ε *any* symbols).
- Ratelessness makes aggregation trivial: symbols from different relays can
  arrive in any order and still decode.
- Per-connection reader tasks merge into one ordered feed on the receiver;
  after `READY` the receiver closes all sockets and drains each relay link to
  EOF, so relays flush cleanly instead of hitting a TCP reset.

Exit stats show the split, e.g. `paths: 2456 direct / 2544 relay`.

## Why fountain codes?

- **No retransmission protocol**: the receiver never asks for missing chunks;
  it just waits for any K + ε symbols.
- **Rateless by construction**: overhead adapts to the actual loss rate.
- **Order independent**: symbols can arrive shuffled across multiple paths
  (P2P + relay + more) and still decode.
- **LDPC + HDPC precoding** (Raptor/RaptorQ-style): S low-density checks let
  the decode graph chain through the payload, and H dense high-density checks
  (RFC 6330 binomial count) act as a safety net so decoding finishes near
  K + ε even at small K — the systematic phase alone recovers everything with
  zero overhead.
- **Single pass integrity**: XXH3 checksum over the whole payload verified at
  reconstruction; optional AEAD seals every symbol.
- **Privacy**: with `--secret`, the relay sees only ciphertext — nonce derived
  from (session id, esi) keeps each symbol independently authenticatable.

## Performance

```bash
cargo run -p orbit-fountain --release --example bench
```

Example output (64 MiB payload, 4 KiB symbols):

| Payload | Symbol size | Loss | Encode | Decode | Symbols used |
|---------|-------------|------|--------|--------|--------------|
| 64 MiB | 4 KiB | 0% | ~3 Gb/s | ~0.2 s | 16384 / 24586 (x1.000) |
| 64 MiB | 4 KiB | 5% | ~3 Gb/s | ~0.2 s | ~1.1x L symbols |

## Real-World Applications

Orbit-Transfer's combination of rateless codes + multi-path aggregation +
browser WASM makes it useful in scenarios where traditional TCP or MPTCP
struggle:

| Scenario | Why Orbit wins |
|---|---|
| **Satellite internet** (geostationary, ~600 ms RTT, bursty loss) | No per-packet retransmission round-trip: the receiver simply waits for K+ε symbols, absorbing burst loss without stalling. |
| **Mobile hybrid networks** (WiFi + 5G, roaming) | Aggregates both uplinks and migrates seamlessly when one path drops — no reconnect, no restart. |
| **CDN edge distribution** (many rate-limited relays) | N throttled relays deliver ~N× throughput with a simple round-robin scheduler; no transport-layer congestion coordination. |
| **Web-based P2P file sharing** (browser-to-browser) | 10 GB files exchanged directly between two browser tabs via WebRTC data channels, with relays as fallback — no central server cost. |
| **Backup over unreliable links** (remote sites, intermittent connectivity) | Resumable, loss-resilient, and checkpoint-free: any interruption just means fewer symbols received, not a full restart. |
| **Satellite / maritime / aerospace** (intermittent, high-loss links) | Rateless encoding adapts to whatever loss rate the link produces; no feedback channel required. |
| **Software supply chain** (OS images, ML models, 100+ GB) | Aggregates multi-path bandwidth + BLAKE3-style integrity (via XXH3 checksum) + optional AEAD encryption. |
| **Live streaming with FEC** | Same codec works as a real-time erasure code for low-overhead broadcast over lossy channels. |

## Protocol

Binary frames over WebSocket (relay) or raw TCP (direct P2P):
`[type: u8][len: u32 LE][payload]`.

| Type | Payload |
|------|---------|
| `Hello` | session id (u64), role (u8: sender=1, receiver=2) |
| `Meta` | session id, filename, size, symbol size, K, checksum (XXH3) |
| `Symbol` | session id, esi (u32), symbol data (optionally sealed) |
| `Ready` | session id |
| `Done` | session id |
| `Direct` | session id, addr (string) — receiver advertises its P2P listener |

## Browser WebRTC

`crates/orbit-wasm/webrtc` is a browser-to-browser transfer over a WebRTC data
channel (fountain codec in WASM). Two tabs exchange SDP via a tiny in-memory
signaling room on the static server, then symbols stream end-to-end; a
simulated-loss slider and an unreliable-SCTP toggle show the rateless code
absorbing packet loss.

```sh
cd crates/orbit-wasm/ts && npm install && npm run build
cd ../webrtc && node serve.mjs     # http://127.0.0.1:8787/webrtc/index.html
```

The sender drags &amp; drops a file and gets a **shareable `?room=<id>` link**;
the receiver opens it and the transfer runs P2P. Manual copy/paste SDP is still
available as an advanced fallback.

```sh
node webrtc.test.mjs               # integration test over a mocked channel
node signaling.test.mjs            # signaling room endpoints
node sharelink.test.mjs            # automated share-link flow end-to-end
```

## Roadmap

- [x] P2P direct path alongside the relay — multi-path scheduler splits
      symbols, automatic fallback to the relay when the direct link fails
- [x] Multi-relay aggregation — N relays with round-robin load, dead-relay
      fallback, drain-based graceful shutdown
- [x] Systematic Raptor-style precoding (LDPC) for smaller overhead at low K
- [x] WASM bindings for browser-to-browser transfers (`orbit-wasm` + demo)
- [x] Encryption (AEAD over symbol payloads)
- [x] TypeScript package for the WASM bindings (`orbit-wasm/ts`)
- [x] RaptorQ-style HDPC checks on top of LDPC (dense high-density parity,
      RFC 6330 binomial count) for near-optimal overhead at small K
- [x] WebRTC data channels for browser-to-browser transfers
      (`orbit-wasm/webrtc`: two-tab demo over a data channel, manual signaling)
- [x] Automated WebRTC signaling — the demo now generates a shareable
      `?room=<id>` link and exchanges SDP over the server's in-memory room
      (manual copy/paste kept as an advanced fallback)
- [x] QUIC transport as an alternative to TCP for the direct path
      (`--quic` on the receiver; `quic://` address advertised via the relay)

## License

MIT OR Apache-2.0