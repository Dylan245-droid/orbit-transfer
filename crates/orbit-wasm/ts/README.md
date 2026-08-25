# orbit-transfer-wasm

TypeScript bindings for the WASM build of **orbit-fountain** — rateless
erasure codes with LDPC precoding, compiled from Rust.

Encode and decode files in the browser with built-in loss tolerance:
the sender emits as many distinct symbols as the link demands, and the
receiver reconstructs the payload from any K + ε of them.

## Usage

```ts
import { loadOrbit, OrbitEncoder, OrbitDecoder, simulateTransfer } from "orbit-transfer-wasm";

await loadOrbit();

// 1. In-memory end-to-end transfer with 10% packet loss
const payload = new Uint8Array(await file.arrayBuffer());
const result = simulateTransfer(payload, { symbolSize: 4096, loss: 0.1 });
console.log(`decoded ${result.decoded.length} bytes with overhead x${result.overhead.toFixed(3)}`);

// 2. Manual encode / decode over your own channel
const encoder = new OrbitEncoder(payload, 4096);
const decoder = new OrbitDecoder(
  encoder.payloadLength, encoder.symbolSize, encoder.k, encoder.checksumHex,
);

// Sender side: emit symbols (rateless — any esi is valid)
const symbol = encoder.encodeSymbol(0);
sendToReceiver(symbol, 0);

// Receiver side: feed whatever arrives (duplicates are harmless)
decoder.addSymbol(0, symbol);
if (decoder.isComplete) {
  const recovered = decoder.reconstruct();
}
```

## Building

Requires `wasm-pack` and the `wasm32-unknown-unknown` target:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-pack
npm install
npm run build        # wasm-pack + tsc
```

The wasm-pack step outputs `pkg/` next to `src/`; `tsc` emits `dist/`
with the declarations.

## API

| Symbol | Description |
| --- | --- |
| `loadOrbit(module_or_path?)` | idempotent WASM module init |
| `OrbitEncoder(payload, symbolSize)` | `k`, `l`, `payloadLength`, `symbolSize`, `checksumHex`, `encodeSymbol(esi)` |
| `OrbitDecoder(payloadLength, symbolSize, k, checksumHex)` | `addSymbol(esi, data)`, `received`, `isComplete`, `reconstruct()` |
| `simulateTransfer(payload, opts)` | end-to-end demo helper with simulated loss |

## License

MIT OR Apache-2.0 (same as the Rust workspace).