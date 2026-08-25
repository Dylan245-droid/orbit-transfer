// Quick Node smoke test for the wasm-bindgen web build.
import { readFileSync } from "node:fs";
import init, { Fountain, Decoder } from "./pkg/orbit_wasm.js";

const wasmBytes = readFileSync(new URL("./pkg/orbit_wasm_bg.wasm", import.meta.url));
await init({ module_or_path: wasmBytes });

const payload = new Uint8Array(1024 * 1024);
for (let i = 0; i < payload.length; i++) payload[i] = (i * 7) % 251;

const enc = new Fountain(payload, 4096);
const k = enc.k();
const dec = new Decoder(enc.payloadLen(), enc.symbolSize(), k, enc.checksumHex());

let fed = 0;
let esi = 0;
const budget = enc.l() + 50;
while (esi < budget) {
  const sym = enc.encodeSymbol(esi);
  fed += 1;
  if (dec.addSymbol(esi, sym)) break;
  esi += 1;
}
const rec = dec.reconstruct();
const ok =
  rec.length === payload.length &&
  rec.every((b, i) => b === payload[i]);

console.log(
  `k=${k} l=${enc.l()} fed=${fed} overhead=${(fed / k).toFixed(3)} ok=${ok}`,
);
if (!ok) process.exit(1);