// Orbit-Transfer · WebRTC data-channel bridge.
//
// Browser-to-browser transfer over a WebRTC data channel, using the
// fountain codec compiled to WASM (orbit-transfer-wasm). Signaling is
// manual: each side prints its SDP (offer/answer), you paste it into the
// other tab, then the transfer streams rateless symbols end-to-end.
//
// Data-channel framing (single binary channel, first byte = type):
//   0x00  META   JSON { filename, payloadLength, symbolSize, k,
//                      checksumHex, budget }
//   0x01  SYMBOL esi (u32 LE) + symbol bytes
//   0x02  DONE   (receiver -> sender, transfer complete)
//
// Loss tolerance is demonstrated with an optional simulated drop on the
// receiver side; the channel itself can be toggled between reliable and
// partially-reliable (unreliable SCTP) for real-loss experiments.

import init, {
  Fountain,
  Decoder,
} from "../ts/pkg/orbit_wasm.js";

export const BRIDGE_VERSION = "0.7.1";

export const FRAME_META = 0x00;
export const FRAME_SYMBOL = 0x01;
export const FRAME_DONE = 0x02;

let wasmInput = "../ts/pkg/orbit_wasm_bg.wasm";

/**
 * Overrides the WASM module input (defaults to a path fetched relative to
 * the page; tests pass raw bytes).
 */
export function setWasmInput(input) {
  wasmInput = input;
}

async function loadWasm() {
  await init({ module_or_path: wasmInput });
}

/** Waits for ICE gathering to finish so the SDP carries every candidate.
 *  Resolves true when gathering completed, false on timeout. */
function iceComplete(pc, timeoutMs = 10000) {
  return new Promise((resolve) => {
    if (pc.iceGatheringState === "complete") return resolve(true);
    function on() {
      if (pc.iceGatheringState === "complete") {
        clearTimeout(t);
        pc.removeEventListener("icegatheringstatechange", on);
        resolve(true);
      }
    }
    const t = setTimeout(() => {
      pc.removeEventListener("icegatheringstatechange", on);
      resolve(false);
    }, timeoutMs);
    pc.addEventListener("icegatheringstatechange", on);
  });
}

/** Counts the a=candidate lines in an SDP (0 => no candidates, ICE can't run). */
function candidateCount(sdp) {
  return (sdp.match(/^a=candidate:/gm) ?? []).length;
}

/** Returns { addr, port } of the first host candidate in an SDP. */
function firstHostCandidate(sdp) {
  const m = sdp.match(/^a=candidate:(\S+) (\d+) udp (\d+) (\S+) (\d+) typ (\S+)/m);
  return m ? { addr: m[4], port: parseInt(m[5], 10) } : null;
}

/**
 * Same-machine fallback: Chrome masks host candidates with mDNS names
 * (xxx.local) which some network interfaces cannot resolve, leaving ICE
 * with no candidate pair (iceConnectionState stuck at "new"). The mDNS
 * *name* is masked but the *port* is not, so we add a loopback candidate
 * pointing at the peer's port — two tabs on the same host then connect
 * over 127.0.0.1. Harmless if normal candidates work.
 */
async function addLoopbackCandidate(pc, sdp, onLog) {
  const c = firstHostCandidate(sdp);
  if (!c) return;
  onLog("loopback candidate: 127.0.0.1:" + c.port + " (peer showed " + c.addr + ")");
  try {
    await pc.addIceCandidate({
      candidate: `candidate:orbit 1 udp 2122262783 127.0.0.1 ${c.port} typ host`,
      sdpMid: "0",
    });
  } catch (e) {
    onLog("loopback candidate rejected: " + e.message);
  }
}

/** Logs connection-level state transitions to diagnose where a link fails. */
function wireDiagnostics(pc, onLog) {
  pc.addEventListener("connectionstatechange", () =>
    onLog("connectionState: " + pc.connectionState));
  pc.addEventListener("iceconnectionstatechange", () =>
    onLog("iceConnectionState: " + pc.iceConnectionState));
  pc.addEventListener("icecandidateerror", (e) =>
    onLog("ICE candidate error: " + (e.errorText ?? e.errorCode)));
}

/** Waits for the data channel to reach "open" (SCTP handshake done). */
function waitOpen(channel, pc, timeoutMs = 20000) {
  return new Promise((resolve, reject) => {
    if (channel.readyState === "open") return resolve();
    function on() {
      clearTimeout(t);
      channel.removeEventListener("open", on);
      resolve();
    }
    const t = setTimeout(() => {
      channel.removeEventListener("open", on);
      reject(new Error(
        "data channel did not open (timeout) — ice=" + pc.iceConnectionState +
        ", conn=" + pc.connectionState));
    }, timeoutMs);
    channel.addEventListener("open", on);
  });
}

// ---------------------------------------------------------------------------
// Automated signaling (shareable-link flow).
//
// The static server (`serve.mjs`) exposes small HTTP endpoints that store one
// SDP offer and one answer per room. The host creates a room, publishes its
// offer and hands out `?room=<id>`; the guest opens that link, fetches the
// offer, publishes its answer; the host polls for it and sends. No other
// coordination needed.
// ---------------------------------------------------------------------------

async function api(path, options) {
  const res = await fetch("/api/rooms" + path, options);
  if (!res.ok) return null;
  return res;
}

export async function signalingCreateRoom() {
  const res = await api("", { method: "POST" });
  if (!res) throw new Error("signaling: create room failed");
  const { roomId } = await res.json();
  return roomId;
}

export async function signalingPutOffer(roomId, sdp) {
  const res = await api(`/${roomId}/offer`, { method: "PUT", body: sdp });
  if (!res) throw new Error("signaling: publish offer failed");
}

export async function signalingGetOffer(roomId) {
  const res = await api(`/${roomId}/offer`);
  return res ? res.text() : null;
}

export async function signalingPutAnswer(roomId, sdp) {
  const res = await api(`/${roomId}/answer`, { method: "PUT", body: sdp });
  if (!res) throw new Error("signaling: publish answer failed");
}

export async function signalingGetAnswer(roomId) {
  const res = await api(`/${roomId}/answer`);
  return res ? res.text() : null;
}

export async function signalingDeleteRoom(roomId) {
  await api(`/${roomId}`, { method: "DELETE" });
}

async function poll(fn, intervalMs, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const v = await fn();
    if (v) return v;
    await new Promise((r) => setTimeout(r, intervalMs));
  }
  throw new Error("signaling: timed out waiting for the peer");
}

/**
 * Host-side automated flow: create a room, publish the offer, return a
 * shareable `?room=<id>` URL. Call `waitForAnswer()` once the guest has
 * joined to accept the answer and start sending `payload`.
 */
export async function hostShare(payload, symbolSize, opts = {}) {
  const host = new HostSession({
    reliable: opts.reliable ?? true,
    maxRetransmits: opts.maxRetransmits,
    loopback: opts.loopback ?? true,
    filename: opts.filename ?? "transfer.bin",
    onLog: opts.onLog ?? (() => {}),
  });
  const sdp = await host.createOffer();
  const roomId = await signalingCreateRoom();
  await signalingPutOffer(roomId, sdp);
  const url = new URL(window.location.href);
  url.searchParams.set("room", roomId);
  const waitForAnswer = async () => {
    const answer = await poll(
      () => signalingGetAnswer(roomId),
      opts.pollIntervalMs ?? 500,
      opts.pollTimeoutMs ?? 120_000,
    );
    await host.acceptAnswer(answer, payload, symbolSize, opts.loss ?? 0);
    return host;
  };
  return { host, roomId, url: url.href, waitForAnswer };
}

/**
 * Guest-side automated flow: join a room, fetch the host's offer, publish
 * the answer, then decode whatever symbols arrive on the data channel.
 */
export async function guestShare(roomId, opts = {}) {
  const guest = new GuestSession({
    loss: opts.loss ?? 0,
    loopback: opts.loopback ?? true,
    onLog: opts.onLog ?? (() => {}),
    onResult: opts.onResult ?? (() => {}),
  });
  const offer = await poll(
    () => signalingGetOffer(roomId),
    opts.pollIntervalMs ?? 500,
    opts.pollTimeoutMs ?? 60_000,
  );
  const answer = await guest.acceptOffer(offer);
  await signalingPutAnswer(roomId, answer);
  return guest;
}

export class HostSession {
  constructor(opts = {}) {
    this.opts = {
      reliable: opts.reliable ?? true,
      maxRetransmits: opts.maxRetransmits,
      loopback: opts.loopback ?? true,
      onLog: opts.onLog ?? (() => {}),
    };
    this.pc = null;
    this.channel = null;
    this.encoder = null;
    this.meta = null;
    this.budget = 0;
    this.sent = 0;
    this.done = false;
  }

  /** Creates the offer SDP (paste this into the guest tab). */
  async createOffer() {
    await loadWasm();
    const pc = new RTCPeerConnection({ iceServers: [] });
    const channel = pc.createDataChannel("orbit", {
      ordered: this.opts.reliable,
      maxRetransmits: this.opts.reliable ? undefined : this.opts.maxRetransmits,
    });
    channel.binaryType = "arraybuffer";
    channel.onopen = () => this.opts.onLog("data channel open");
    channel.onclose = () => this.opts.onLog("data channel closed");
    channel.onmessage = (ev) => this.#handleControl(ev.data);
    this.pc = pc;
    this.channel = channel;
    wireDiagnostics(pc, this.opts.onLog);

    const offer = await pc.createOffer();
    await pc.setLocalDescription(offer);
    if (!(await iceComplete(pc))) {
      this.opts.onLog("WARNING: ICE gathering not complete — the SDP may miss candidates");
    }
    const c = firstHostCandidate(pc.localDescription.sdp);
    this.opts.onLog("offer SDP: " + candidateCount(pc.localDescription.sdp) + " candidate(s) · " + (c ? c.addr + ":" + c.port : "none"));
    return pc.localDescription.sdp;
  }

  /** Accepts the guest's answer SDP and starts sending `payload`. */
  async acceptAnswer(sdp, payload, symbolSize, loss) {
    if (this.pc.signalingState !== "have-local-offer") {
      throw new Error(
        "No offer in progress (state '" + this.pc.signalingState + "'). " +
        "If the page was reloaded, the old offer/answer pair is dead: click " +
        "'Create offer' again and redo the copy/paste exchange.",
      );
    }
    await pcSetRemote(this.pc, "answer", sdp);
    if (this.opts.loopback) {
      await addLoopbackCandidate(this.pc, sdp, this.opts.onLog);
    }
    await waitOpen(this.channel, this.pc);
    this.opts.onLog("connection up — sending");
    await this.#sendPayload(payload, symbolSize, loss);
  }

  #handleControl(data) {
    const view = new Uint8Array(data);
    if (view.length >= 1 && view[0] === FRAME_DONE) {
      this.done = true;
      this.opts.onLog("receiver finished; sender stopping");
    }
  }

  async #sendPayload(payload, symbolSize, loss) {
    const encoder = new Fountain(payload, symbolSize);
    this.encoder = encoder;
    const k = encoder.k();
    // Rateless: keep emitting until the receiver's DONE arrives, whatever the
    // loss rate. The cap only guards against a stuck channel.
    const hardCap = Math.max(5000, encoder.l() * 10);
    this.meta = {
      filename: this.opts.filename ?? "transfer.bin",
      payloadLength: encoder.payloadLen(),
      symbolSize: encoder.symbolSize(),
      k,
      checksumHex: encoder.checksumHex(),
      budget: hardCap,
    };
    const enc = new TextEncoder();
    const meta = new Uint8Array([FRAME_META, ...enc.encode(JSON.stringify(this.meta))]);
    await sendChunked(this.channel, meta);
    this.opts.onLog(`sending ${this.meta.payloadLength} B (K=${k}, cap ${hardCap}, loss ${Math.round(loss * 100)}%)`);

    const start = performance.now();
    for (let esi = 0; esi < hardCap && !this.done; esi++) {
      const sym = encoder.encodeSymbol(esi);
      const frame = new Uint8Array(5 + sym.length);
      frame[0] = FRAME_SYMBOL;
      new DataView(frame.buffer, 1, 4).setUint32(0, esi, true);
      frame.set(sym, 5);
      await sendChunked(this.channel, frame);
      this.sent += 1;
      if (this.sent % 500 === 0) {
        const mbps = (this.meta.payloadLength / 1048576) / ((performance.now() - start) / 1000);
        this.opts.onLog(`sent ${this.sent} symbols (${mbps.toFixed(2)} MiB/s)`);
      }
    }
    const doneMsg = this.done
      ? "sender done (receiver finished)"
      : "sender done — cap hit, no DONE (receiver may be short of symbols)";
    this.opts.onLog(`${doneMsg}: ${this.sent}/${hardCap} in ${((performance.now() - start) / 1000).toFixed(2)}s`);
  }

  async close() {
    this.channel?.close();
    this.pc?.close();
  }
}

async function pcSetRemote(pc, type, sdp) {
  await pc.setRemoteDescription({ type, sdp });
}

/** Waits for the channel buffer to drain below a watermark. */
function sendChunked(channel, data) {
  const HWM = 4 * 1024 * 1024;
  if (channel.bufferedAmount + data.byteLength > HWM) {
    return new Promise((resolve) => {
      const onDrain = () => {
        if (channel.bufferedAmount + data.byteLength <= HWM) {
          channel.removeEventListener("bufferedamountlow", onDrain);
          resolve();
        }
      };
      channel.addEventListener("bufferedamountlow", onDrain);
      channel.bufferedAmountLowThreshold = HWM / 2;
    }).then(() => {
      channel.send(data);
    });
  }
  channel.send(data);
  return Promise.resolve();
}

export class GuestSession {
  constructor(opts = {}) {
    this.opts = {
      loss: opts.loss ?? 0,
      loopback: opts.loopback ?? true,
      onLog: opts.onLog ?? (() => {}),
      onResult: opts.onResult ?? (() => {}),
    };
    this.pc = null;
    this.channel = null;
    this.decoder = null;
    this.meta = null;
    this.received = 0;
    this.dropped = 0;
    this.fed = 0;
    this.pending = [];
  }

  /** Sets the offer SDP from the host and creates the answer. */
  async acceptOffer(sdp) {
    await loadWasm();
    const pc = new RTCPeerConnection({ iceServers: [] });
    this.pc = pc;
    wireDiagnostics(pc, this.opts.onLog);
    pc.ondatachannel = (ev) => {
      this.channel = ev.channel;
      this.channel.binaryType = "arraybuffer";
      this.channel.onmessage = (e) => this.#handle(e.data);
      this.channel.onopen = () => this.opts.onLog("data channel open");
      this.channel.onclose = () => this.opts.onLog("data channel closed");
    };
    await pcSetRemote(pc, "offer", sdp);
    const answer = await pc.createAnswer();
    await pc.setLocalDescription(answer);
    if (!(await iceComplete(pc))) {
      this.opts.onLog("WARNING: ICE gathering not complete — the answer may miss candidates");
    }
    const c = firstHostCandidate(pc.localDescription.sdp);
    this.opts.onLog("answer SDP: " + candidateCount(pc.localDescription.sdp) + " candidate(s) · " + (c ? c.addr + ":" + c.port : "none"));
    if (this.pc.signalingState !== "stable") {
      throw new Error("Unexpected signaling state '" + this.pc.signalingState + "' after creating the answer.");
    }
    if (this.opts.loopback) {
      await addLoopbackCandidate(pc, sdp, this.opts.onLog);
    }
    return pc.localDescription.sdp;
  }

  #handle(data) {
    const view = new Uint8Array(data);
    if (view.length === 0) return;
    switch (view[0]) {
      case FRAME_META: {
        this.meta = JSON.parse(new TextDecoder().decode(view.subarray(1)));
        this.opts.onLog(`meta: ${this.meta.payloadLength} B, K=${this.meta.k}, symbolSize ${this.meta.symbolSize}`);
        this.decoder = new Decoder(
          this.meta.payloadLength,
          this.meta.symbolSize,
          this.meta.k,
          this.meta.checksumHex,
        );
        this.#flush();
        break;
      }
      case FRAME_SYMBOL: {
        const esi = new DataView(view.buffer, 1, 4).getUint32(0, true);
        const data = view.slice(5);
        this.received += 1;
        if (Math.random() < this.opts.loss) {
          this.dropped += 1;
          break;
        }
        this.fed += 1;
        this.#feed(esi, data);
        break;
      }
      case FRAME_DONE:
        this.opts.onLog("sender finished");
        break;
      default:
        this.opts.onLog(`unknown frame type ${view[0]}`);
    }
  }

  #flush() {
    for (const [esi, data] of this.pending) this.#feed(esi, data);
    this.pending = [];
  }

  #feed(esi, data) {
    if (!this.decoder) {
      this.pending.push([esi, data]);
      return;
    }
    try {
      const complete = this.decoder.addSymbol(esi, data);
      if (this.fed % 200 === 0) {
        this.opts.onLog(`fed ${this.fed} (distinct ${this.decoder.received()}) need ~${this.meta.k}`);
      }
      if (complete) {
        this.opts.onLog(`decoded at ${this.fed} symbols fed (distinct ${this.decoder.received()}, overhead x${(this.fed / this.meta.k).toFixed(3)})`);
        const payload = this.decoder.reconstruct();
        const ok = payload.length === this.meta.payloadLength;
        this.opts.onResult({ payload, ok, meta: this.meta, received: this.received, dropped: this.dropped, fed: this.fed });
        const done = new Uint8Array([FRAME_DONE]);
        this.channel?.send(done);
      }
    } catch (e) {
      this.opts.onLog("decoder error: " + e.message);
    }
  }
}