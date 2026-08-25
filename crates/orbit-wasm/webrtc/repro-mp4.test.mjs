// Repro with the user's actual MP4 file: 2,977,664 B, 4096 B symbols,
// 10% simulated loss, sequential esi order.
import { readFileSync } from "node:fs";
import init, { Fountain, Decoder } from "../ts/pkg/orbit_wasm.js";

await init({ module_or_path: readFileSync(new URL("../ts/pkg/orbit_wasm_bg.wasm", import.meta.url)) });

const payload = readFileSync("C:\\Users\\Danie\\Downloads\\VID-20240927-WA0015.mp4");
console.log("payload:", payload.length, "bytes");

const encoder = new Fountain(payload, 4096);
const k = encoder.k();
const l = encoder.l();
const decoder = new Decoder(payload.length, 4096, k, encoder.checksumHex());

let received = 0;
let dropped = 0;
let fed = 0;
let complete = false;
let esi = 0;
while (!complete && esi < l * 10) {
  if (Math.random() < 0.1) {
    dropped += 1;
  } else {
    received += 1;
    fed += 1;
    complete = decoder.addSymbol(esi, encoder.encodeSymbol(esi));
  }
  esi += 1;
}

console.log(`K=${k} l=${l} · received=${received} dropped=${dropped} fed=${fed} overhead=x${(fed / k).toFixed(3)} complete=${complete}`);
if (complete) {
  const out = decoder.reconstruct();
  console.log(`reconstruct ok: ${out.length === payload.length && out.every((b, i) => b === payload[i])}`);
} else {
  console.log("NOT COMPLETE at cap");
}