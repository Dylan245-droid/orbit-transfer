# Orbit-Transfer · WebRTC demo

Browser-to-browser transfer over a **WebRTC data channel**, using the fountain
codec compiled to WASM. The sender drags &amp; drops a file and gets a
**shareable link**; the receiver opens it and the SDP offer/answer is exchanged
through a tiny in-memory signaling room on the static server. The sender then
streams rateless symbols end-to-end and the receiver reconstructs the file from
any K + ε of them — no central data server.

## Run

```sh
# build the WASM package first (once)
cd ../ts
npm install
npm run build        # wasm-pack + tsc (or see ts/README.md)

# serve the demo + signaling
node serve.mjs       # -> http://127.0.0.1:8787/webrtc/index.html
```

Open the URL in **two tabs** of the same browser (or two machines on the same
LAN):

1. **Sender tab**: pick a file → *Drag &amp; drop → create share link* →
   copy the generated `?room=<id>` link.
2. **Receiver tab**: open the link (or paste it into a new tab) → adjust the
   simulated-loss slider → *Join as receiver*.
3. The transfer streams symbols; the receiver logs the decode overhead and
   verifies the payload (XXH3 + byte compare).

A **manual signaling** mode (copy/paste SDP by hand, no server room) is also
available under "Manual signaling (advanced)" — useful if you cannot reach the
signaling endpoints.

## Signaling API

The static server also stores one SDP offer and one answer per room
(in-memory, 30-minute TTL):

| Endpoint | Role |
| --- | --- |
| `POST /api/rooms` | create a room → `{ roomId }` |
| `PUT /api/rooms/:id/offer` | host publishes its SDP offer |
| `GET /api/rooms/:id/offer` | guest fetches the offer |
| `PUT /api/rooms/:id/answer` | guest publishes its SDP answer |
| `GET /api/rooms/:id/answer` | host fetches the answer |
| `DELETE /api/rooms/:id` | clean up a room |

## What it demonstrates

- **Any file type**: the codec is type-agnostic — the payload is raw bytes and
  is reconstructed exactly (verified byte-for-byte plus an XXH3 checksum).
  The only practical limit is memory: the file is loaded whole into the tab
  (file → `ArrayBuffer` → WASM encoder), so very large files (≳ 1 GB) may
  strain the browser. All file types are accepted (mp4, pdf, zip, images,
  executables, …).
- **Loss tolerance**: the receiver can drop a configurable percentage of
  incoming symbols (simulated loss) and still decode, because the codec is
  rateless.
- **Unreliable SCTP**: toggle the channel to *unreliable* to exercise real
  packet loss on the data channel (SCTP over UDP with `maxRetransmits: 1`);
  the codec absorbs it the same way.
- **Bandwidth throttling**: the sender paces on `bufferedAmount`, so it does
  not buffer unboundedly into the channel.

## Files

| File | Purpose |
| --- | --- |
| `serve.mjs` | zero-dependency static server + signaling rooms |
| `webrtc.js` | bridge module: `HostSession`, `GuestSession`, `hostShare`/`guestShare`, framing, pacing |
| `index.html` | share-link demo (auto signaling) + manual SDP fallback |
| `webrtc.test.mjs` | Node integration test over a mocked data channel |
| `signaling.test.mjs` | Node test of the signaling room endpoints |
| `sharelink.test.mjs` | Node end-to-end test of the automated share-link flow |

## Notes

- ICE uses host candidates only (`iceServers: []`), which covers
  same-machine and same-LAN use. Cross-network signaling would need STUN/TURN
  and a signaling channel.
- The data-channel framing is `[type:u8]` + payload:
  `0x00` META (JSON), `0x01` SYMBOL (`esi:u32LE` + bytes), `0x02` DONE.