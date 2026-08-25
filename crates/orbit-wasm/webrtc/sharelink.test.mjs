// End-to-end test of the automated share-link flow: guestShare + the host
// primitives over the real signaling server, with a mocked RTCPeerConnection
// whose data channels auto-link (no browser, no real network).
//
//   node sharelink.test.mjs
import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { HostSession, guestShare, signalingCreateRoom, signalingPutOffer, signalingGetAnswer, setWasmInput } from "./webrtc.js";
import { fileURLToPath } from "node:url";

const PORT = 8801;
const ROOT = fileURLToPath(new URL(".", import.meta.url));
const BASE = `http://127.0.0.1:${PORT}`;

// --- Mock WebRTC ----------------------------------------------------------
let chH = null; // host's channel
let chG = null; // guest's channel (paired)

class FakeChannel {
  constructor() {
    this.readyState = "open";
    this.onopen = null;
    this.onclose = null;
    this.onmessage = null;
    this.bufferedAmount = 0;
    this.bufferedAmountLowThreshold = 0;
    this.peer = null;
  }
  send(data) {
    if (this.peer?.onmessage) this.peer.onmessage({ data });
  }
  addEventListener() {}
  removeEventListener() {}
  close() {}
}

class FakePeerConnection {
  constructor() {
    this.ondatachannel = null;
    this.iceGatheringState = "new";
    this.localDescription = null;
    this.signalingState = "stable";
  }
  createDataChannel() {
    chH = new FakeChannel();
    return chH;
  }
  createOffer() {
    return Promise.resolve({ type: "offer" });
  }
  createAnswer() {
    return Promise.resolve({ type: "answer" });
  }
  setLocalDescription(desc) {
    this.iceGatheringState = "complete";
    this.localDescription = { sdp: "fake-sdp-" + Math.random() };
    this.signalingState = desc.type === "offer" ? "have-local-offer" : "stable";
    return Promise.resolve();
  }
  setRemoteDescription(desc) {
    this.signalingState = desc.type === "offer" ? "have-remote-offer" : "stable";
    if (desc.type === "offer" && chH) {
      chG = new FakeChannel();
      chH.peer = chG;
      chG.peer = chH;
      this.ondatachannel({ channel: chG });
    }
    return Promise.resolve();
  }
  addIceCandidate() {
    return Promise.resolve();
  }
  addEventListener() {}
  removeEventListener() {}
  close() {}
}

globalThis.RTCPeerConnection = FakePeerConnection;
const originalFetch = globalThis.fetch;
globalThis.fetch = (path, options) => {
  const url = path.startsWith("http") ? path : BASE + path;
  return originalFetch(url, options);
};

setWasmInput(readFileSync(new URL("../ts/pkg/orbit_wasm_bg.wasm", import.meta.url)));

const server = spawn(process.execPath, ["serve.mjs"], {
  env: { ...process.env, PORT: String(PORT) },
  cwd: ROOT,
  stdio: ["ignore", "ignore", "pipe"],
});
let serverErr = "";
server.stderr.on("data", (d) => (serverErr += d.toString()));

async function waitUp() {
  for (let i = 0; i < 50; i++) {
    try { await globalThis.fetch("/"); return; } catch { await new Promise((r) => setTimeout(r, 100)); }
  }
  throw new Error("server did not start: " + serverErr);
}

const payload = new Uint8Array(128 * 1024);
for (let i = 0; i < payload.length; i++) payload[i] = (i * 29) % 251;

async function pollAnswer(roomId, intervalMs = 100, timeoutMs = 15000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const a = await signalingGetAnswer(roomId);
    if (a) return a;
    await new Promise((r) => setTimeout(r, intervalMs));
  }
  throw new Error("timed out waiting for guest answer");
}

try {
  await waitUp();

  const logs = [];
  const line = (m) => logs.push(m);
  let result = null;

  const roomId = await signalingCreateRoom();

  // Guest (receiver): joins the shared room, publishes its answer on arrival.
  const guestPromise = guestShare(roomId, {
    loss: 0.1,
    onLog: (m) => line("G " + m),
    onResult: (r) => (result = r),
  });

  // Host (sender): create offer, publish, wait for the guest's answer, send.
  const host = new HostSession({ reliable: true, filename: "live.mp4", onLog: (m) => line("H " + m) });
  const offer = await host.createOffer();
  await signalingPutOffer(roomId, offer);
  const answer = await pollAnswer(roomId);
  await host.acceptAnswer(answer, payload, 4096, 0.1);

  await guestPromise;

  const ok =
    result && result.ok && result.payload.length === payload.length &&
    result.payload.every((b, i) => b === payload[i]);
  const nameOk = result?.meta?.filename === "live.mp4";
  if (!nameOk) {
    console.error(`FAILED: filename lost, got '${result?.meta?.filename}' expected 'live.mp4'`);
    process.exit(1);
  }

  console.log(`guest fed ${result?.fed} · overhead x${(result?.fed / result?.meta.k).toFixed(3)} · dropped ${result?.dropped} · ok=${!!ok}`);
  if (!ok) {
    console.error("FAILED");
    for (const m of logs.slice(-20)) console.error("  " + m);
    process.exit(1);
  }
  console.log("ok — automated share-link flow works (offer -> room -> answer -> transfer)");
} finally {
  server.kill();
}