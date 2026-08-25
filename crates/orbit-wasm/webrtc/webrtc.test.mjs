// Integration test for the WebRTC bridge: drives the full host -> guest
// transfer over a mocked, ordered data channel (no browser, no network).
//
//   node webrtc.test.mjs
import { readFileSync } from "node:fs";
import { HostSession, GuestSession, setWasmInput } from "./webrtc.js";

const wasmBytes = readFileSync(new URL("../ts/pkg/orbit_wasm_bg.wasm", import.meta.url));
setWasmInput(wasmBytes);

class FakeChannel {
  constructor(peer) {
    this.peer = peer;
    this.readyState = "open";
    this.onopen = null;
    this.onclose = null;
    this.onmessage = null;
    this.bufferedAmount = 0;
    this.bufferedAmountLowThreshold = 0;
  }
  send(data) {
    if (this.peer.onmessage) this.peer.onmessage({ data });
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
    return new FakeChannel(null);
  }
  createOffer() {
    return Promise.resolve({ type: "offer" });
  }
  createAnswer() {
    return Promise.resolve({ type: "answer" });
  }
  setLocalDescription(desc) {
    this.iceGatheringState = "complete";
    this.localDescription = { sdp: "fake-sdp" };
    // JSEP: an offer moves to have-local-offer; an answer (replying to a
    // remote offer) settles the negotiation back to stable.
    this.signalingState = desc.type === "offer" ? "have-local-offer" : "stable";
    return Promise.resolve();
  }
  setRemoteDescription(desc) {
    this.signalingState = desc.type === "offer" ? "have-remote-offer" : "stable";
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

const log = [];
const payload = new Uint8Array(256 * 1024);
for (let i = 0; i < payload.length; i++) payload[i] = (i * 13) % 251;

const loss = 0.1;
const host = new HostSession({ reliable: true, onLog: (m) => log.push("H " + m) });
const offerSdp = await host.createOffer();

const guest = new GuestSession({
  loss,
  onLog: (m) => log.push("G " + m),
  onResult: (r) => (result = r),
});
let result = null;
const answerSdp = await guest.acceptOffer(offerSdp);

// Wire the mock channel both ways: host's channel and guest's channel share
// a synchronous pipe (ordered, reliable).
const guestCh = new FakeChannel(host.channel);
host.channel.peer = guestCh;
guest.pc.ondatachannel({ channel: guestCh });

await host.acceptAnswer(answerSdp, payload, 4096, loss);

const ok =
  result &&
  result.ok &&
  result.payload.length === payload.length &&
  result.payload.every((b, i) => b === payload[i]);

console.log(
  `host sent ${host.sent}/${host.meta.budget} · guest fed ${result?.fed} (overhead x${(result?.fed / result?.meta.k).toFixed(3)}) · dropped ${result?.dropped} · ok=${!!ok}`,
);

if (!ok) {
  console.error("FAILED");
  for (const m of log.slice(-20)) console.error("  " + m);
  process.exit(1);
}
console.log("ok");