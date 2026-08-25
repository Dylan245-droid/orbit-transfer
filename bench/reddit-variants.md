# Reddit variants — r/selfhosted & r/opensource

## r/selfhosted (angle: "replaces your sync/proxy stack, no cloud")

**Title:** Orbit-Transfer: self-hosted P2P/relay file sync with fountain codes
(78 MB Docker relay, no cloud, loss-proof)

**Body:**

I built a file transfer system you can self-host entirely. It splits a file
into K symbols, adds LDPC + HDPC precode checks, and the sender streams an
unbounded sequence of encoded symbols — the receiver rebuilds the file from any
K+ε of them. No retransmission protocol, no "lost chunk" requests, works over
lossy links and multiple paths at once.

Why it's self-hoster friendly:
- **One tiny relay**: `docker run -p 9000:9000 dylanondo/orbit-relay` (~78 MB,
  single binary, WebSocket, routes sealed symbols by session id).
- **N relays = N× throughput** — run 2-3 on your old boxes/ARM devices and the
  transfer uses all of them (round-robin, dead relays skipped automatically).
- **No central server for same-LAN**: browser-to-browser WebRTC, two tabs and
  it's done.
- **Encryption**: optional passphrase seals every symbol (ChaCha20-Poly1305);
  the relay only routes ciphertext.

Live demo (no install): https://orbit-transfer-theta.vercel.app/webrtc/

Bench (32 MiB, 2048 kbps cap per path):
http 2.12 MiB/s · orbit-1 2.63 · orbit-2 4.41 · orbit-3 6.02 (≈2.84×).

Repo: https://github.com/Dylan245-droid/orbit-transfer
Docker: https://hub.docker.com/r/dylanondo/orbit-relay
npm (browser codec): https://www.npmjs.com/package/orbit-transfer-wasm

Happy to answer questions — especially about running it on low-end ARM boxes.

## r/opensource (angle: full stack open, paper included)

**Title:** Orbit-Transfer — open-source P2P/relay file transfer with rateless
fountain codes (MIT OR Apache-2.0)

**Body:**

Open-sourced a file transfer system based on rateless fountain codes (the same
family of codes used in 3GPP broadcast and RaptorQ):

- **Codec**: LT + LDPC + RaptorQ-style HDPC precoding, peeling decoder,
  SIMD-vectorizable XOR kernel, XXH3 integrity. Rust, compiled to WASM.
- **Transport**: WebSocket edge relays + optional direct P2P over TCP **or
  QUIC**, with a multi-path scheduler that aggregates N relays ~linearly.
- **Browsers**: the codec runs in WASM over WebRTC data channels — a shareable
  `?room=` link transfers a file P2P, loss slider to 40% and it still decodes.
- **Paper**: full IEEEtran paper in the repo (English + French) with the
  design, the aggregation study, and a real rate-limiter fix (CPU-spinning
  token buckets collapse under concurrency; parked-sleep + self-correcting
  credit keeps N relays uniform).

Bench (reproducible, `node bench/run-bench.mjs`):
3 throttled relays ≈ 2.84× a single throttled HTTP path.

License: MIT OR Apache-2.0.

Repo: https://github.com/Dylan245-droid/orbit-transfer
Live demo: https://orbit-transfer-theta.vercel.app/webrtc/
npm: https://www.npmjs.com/package/orbit-transfer-wasm
Docker: https://hub.docker.com/r/dylanondo/orbit-relay